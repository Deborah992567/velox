//! Upstream groups: load balancing across a set of servers with health
//! tracking.
//!
//! Phase 9 proxied to a single `proxy_pass` target and Phase 10 pooled
//! connections per target. Phase 11 groups several servers behind one
//! `upstream {}` block and balances requests across the healthy ones:
//!
//! - [`BalancePolicy::RoundRobin`] cycles through the servers;
//! - [`BalancePolicy::WeightedRoundRobin`] smooths the cycle by `weight`
//!   (nginx-style smooth weighted round robin);
//! - [`BalancePolicy::LeastConnections`] picks the server with the fewest
//!   in-flight requests.
//!
//! Each server carries a `weight`, a `backup` flag (backups are only used when
//! every primary is down), and passive-health thresholds (`max_fails` /
//! `fail_timeout`). A server is marked down after `max_fails` consecutive
//! failures and is revived after `fail_timeout` has elapsed — or immediately
//! by a successful active probe.
//!
//! [`proxy_exchange_lb`] runs the Phase 9 exchange against peers selected by
//! the group, retrying bodyless idempotent requests across peers while feeding
//! success/failure back into the passive-health state. Active checks probe
//! each server from a background thread (TCP connect, or an HTTP request with
//! a `2xx`/`3xx` expectation).

use std::io;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::http::{BodyFraming, Request};
use crate::net::{Connection, SocketTimeoutSide, connect_with_timeout, set_socket_timeout};
use crate::proxy::config::{ProxyOptions, ProxyTarget};
use crate::proxy::exchange::{
    BodyRelay, ExchangeError, ProxyOutcome, prepare_response, relay_body, relay_request,
};
use crate::proxy::pool::{PoolOptions, PooledConnection, UpstreamPool};

/// How requests are distributed across the healthy servers of a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalancePolicy {
    /// Plain round robin over healthy servers.
    RoundRobin,
    /// Smooth weighted round robin (nginx's upstream selection).
    WeightedRoundRobin,
    /// Pick the server with the fewest in-flight requests.
    LeastConnections,
}

/// One server in an upstream group.
#[derive(Debug, Clone)]
pub struct UpstreamServer {
    /// The resolved `proxy_pass` target (address, scheme, URI prefix, Host).
    pub target: ProxyTarget,
    /// Relative load weight for [`BalancePolicy::WeightedRoundRobin`].
    pub weight: u32,
    /// A backup is only selected when every non-backup server is down.
    pub backup: bool,
    /// Consecutive failures after which the server is marked down.
    pub max_fails: u32,
    /// How long a down server stays down before it is retried.
    pub fail_timeout: Duration,
}

impl UpstreamServer {
    /// A server with default weight 1 and passive-health thresholds.
    pub const fn new(target: ProxyTarget) -> Self {
        Self {
            target,
            weight: 1,
            backup: false,
            max_fails: 1,
            fail_timeout: Duration::from_secs(10),
        }
    }

    /// Set the relative weight.
    #[must_use]
    pub const fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }

    /// Mark the server as a backup.
    #[must_use]
    pub const fn as_backup(mut self) -> Self {
        self.backup = true;
        self
    }

    /// Set the passive-health thresholds.
    #[must_use]
    pub const fn with_health(mut self, max_fails: u32, fail_timeout: Duration) -> Self {
        self.max_fails = max_fails;
        self.fail_timeout = fail_timeout;
        self
    }
}

/// A static upstream group configuration.
#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    /// The servers, in declaration order.
    pub servers: Vec<UpstreamServer>,
    /// How healthy servers are selected.
    pub policy: BalancePolicy,
}

impl UpstreamConfig {
    /// A round-robin group over the given servers.
    pub const fn round_robin(servers: Vec<UpstreamServer>) -> Self {
        Self {
            servers,
            policy: BalancePolicy::RoundRobin,
        }
    }

    /// Set the balancing policy.
    #[must_use]
    pub const fn with_policy(mut self, policy: BalancePolicy) -> Self {
        self.policy = policy;
        self
    }
}

/// Why a peer could not be selected or connected.
#[derive(Debug)]
pub enum PeerError {
    /// No server is currently healthy (all down, or only backups exist and
    /// they are down too).
    NoHealthyUpstream,
    /// Connecting to the selected peer failed.
    Connect(io::Error),
}

/// A connection borrowed from the pool of one peer, selected by the balancer.
///
/// Reports the exchange's outcome back to the group's passive-health state:
/// [`SelectedPeer::finish_success`] marks the peer healthy and resets its
/// failure counter; [`SelectedPeer::fail`] records a failure (possibly taking
/// the peer down). A guard dropped without either simply releases the
/// connection and the in-flight slot without changing health.
#[derive(Debug)]
pub struct SelectedPeer<'a> {
    group: &'a UpstreamGroup,
    index: usize,
    conn: Option<PooledConnection<'a>>,
    reported: bool,
}

impl SelectedPeer<'_> {
    /// The selected server's index in the group.
    pub const fn index(&self) -> usize {
        self.index
    }

    /// The selected server's proxy target.
    pub fn target(&self) -> &ProxyTarget {
        &self.group.servers[self.index].target
    }

    /// The underlying connection.
    ///
    /// # Panics
    ///
    /// Panics if the connection was already returned (the peer is spent).
    pub fn conn_mut(&mut self) -> &mut Connection {
        self.conn
            .as_mut()
            .expect("peer connection present")
            .conn_mut()
    }

    /// Report a successful exchange: the peer answered, so its failure streak
    /// resets and it stays healthy. `keepalive` returns the connection to the
    /// peer's pool when the response ended at a message boundary.
    pub fn finish_success(mut self, keepalive: bool) {
        if keepalive && let Some(conn) = &mut self.conn {
            conn.mark_reusable();
        }
        self.group.succeed(self.index);
        self.reported = true;
    }

    /// Report a failed exchange, marking the peer down once its consecutive
    /// failures reach `max_fails`. Consumes the connection (closing it).
    pub fn fail(&mut self) {
        self.group.fail(self.index);
        self.reported = true;
    }

    /// Mark the peer healthy and reset its state (used by active probes).
    pub fn probe_healthy(&mut self) {
        self.group.mark_healthy(self.index);
        self.reported = true;
    }
}

impl Drop for SelectedPeer<'_> {
    fn drop(&mut self) {
        if !self.reported {
            self.group.decrement_in_flight(self.index);
        }
    }
}

/// Mutable per-server runtime state.
#[derive(Debug)]
struct PeerState {
    healthy: bool,
    failures: u32,
    down_since: Option<Instant>,
    in_flight: usize,
    /// Smooth weighted round robin accumulator (signed: it dips below zero
    /// after a selection subtracts the group's total weight).
    current_weight: i64,
}

/// The balancer's mutable state, guarded so the group can be shared.
#[derive(Debug)]
struct GroupInner {
    state: Vec<PeerState>,
    cursor: usize,
}

/// A group of servers sharing one balancing policy and per-server pools.
pub struct UpstreamGroup {
    servers: Vec<UpstreamServer>,
    policy: BalancePolicy,
    inner: Mutex<GroupInner>,
    pools: Vec<UpstreamPool>,
}

impl std::fmt::Debug for UpstreamGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().unwrap();
        f.debug_struct("UpstreamGroup")
            .field("servers", &self.servers.len())
            .field("state", &inner.state)
            .finish_non_exhaustive()
    }
}

impl UpstreamGroup {
    /// Build a group from its static configuration, giving each server its own
    /// connection pool sized by `pool_options`.
    pub fn new(config: &UpstreamConfig, pool_options: PoolOptions) -> Self {
        let pools = config
            .servers
            .iter()
            .map(|_| UpstreamPool::new(pool_options))
            .collect();
        let state = config
            .servers
            .iter()
            .map(|_| PeerState {
                healthy: true,
                failures: 0,
                down_since: None,
                in_flight: 0,
                current_weight: 0,
            })
            .collect();
        Self {
            servers: config.servers.clone(),
            policy: config.policy,
            inner: Mutex::new(GroupInner { state, cursor: 0 }),
            pools,
        }
    }

    /// A group with default pool limits.
    pub fn from_config(config: &UpstreamConfig) -> Self {
        Self::new(config, PoolOptions::default())
    }

    /// The number of configured servers.
    pub const fn len(&self) -> usize {
        self.servers.len()
    }

    /// Whether the group has no servers at all.
    pub const fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// The balancing policy.
    pub const fn policy(&self) -> BalancePolicy {
        self.policy
    }

    /// Whether a server is currently considered healthy.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds or the state mutex is poisoned.
    pub fn is_healthy(&self, index: usize) -> bool {
        self.inner.lock().unwrap().state[index].healthy
    }

    /// The consecutive-failure count of a server.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds or the state mutex is poisoned.
    pub fn failures(&self, index: usize) -> u32 {
        self.inner.lock().unwrap().state[index].failures
    }

    /// The number of in-flight requests a server is currently handling.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds or the state mutex is poisoned.
    pub fn in_flight(&self, index: usize) -> usize {
        self.inner.lock().unwrap().state[index].in_flight
    }

    /// The number of currently healthy servers.
    ///
    /// # Panics
    ///
    /// Panics if the state mutex is poisoned.
    pub fn healthy_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .state
            .iter()
            .filter(|state| state.healthy)
            .count()
    }

    /// Select the next server per the balancing policy and borrow a pooled
    /// connection to it.
    ///
    /// Down servers whose `fail_timeout` has elapsed are revived before
    /// selection; backups are only considered when every primary is down.
    /// Returns [`PeerError::NoHealthyUpstream`] when nothing can be selected.
    ///
    /// # Panics
    ///
    /// Panics if the state mutex is poisoned.
    pub fn next(&self, options: &ProxyOptions) -> Result<SelectedPeer<'_>, PeerError> {
        let index = {
            let mut inner = self.inner.lock().unwrap();
            self.revive_expired(&mut inner);
            let index = self
                .select(&mut inner)
                .ok_or(PeerError::NoHealthyUpstream)?;
            inner.state[index].in_flight += 1;
            index
        };
        let borrowed = self.pools[index].borrow(&self.servers[index].target, options);
        let conn = match borrowed {
            Ok(conn) => {
                return Ok(SelectedPeer {
                    group: self,
                    index,
                    conn: Some(conn),
                    reported: false,
                });
            }
            Err(error) => error,
        };
        {
            let mut inner = self.inner.lock().unwrap();
            inner.state[index].in_flight -= 1;
            // A connect failure is peer-health evidence; a pool-acquire
            // timeout is our own backpressure and is not penalized.
            if conn.kind() != io::ErrorKind::TimedOut {
                inner.state[index].failures += 1;
                if inner.state[index].failures >= self.servers[index].max_fails {
                    inner.state[index].healthy = false;
                    inner.state[index].down_since = Some(Instant::now());
                }
            }
        }
        Err(PeerError::Connect(conn))
    }

    /// Try every server directly: this is the hook the active health checker
    /// uses, bypassing the pools so probes never consume pooled connections.
    ///
    /// # Panics
    ///
    /// Panics if the state mutex is poisoned.
    pub fn probe_all(&self, config: &HealthCheckConfig) {
        for index in 0..self.servers.len() {
            let ok = probe_server(&self.servers[index].target, config);
            let mut inner = self.inner.lock().unwrap();
            if ok {
                inner.state[index].failures = 0;
                inner.state[index].healthy = true;
                inner.state[index].down_since = None;
                inner.state[index].current_weight = 0;
            } else {
                inner.state[index].failures += 1;
                if inner.state[index].failures >= self.servers[index].max_fails {
                    inner.state[index].healthy = false;
                    inner.state[index].down_since = Some(Instant::now());
                }
            }
        }
    }

    /// Revive a healthy flag on a server (from a successful active probe).
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds or the state mutex is poisoned.
    pub fn mark_healthy(&self, index: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.state[index].failures = 0;
        inner.state[index].healthy = true;
        inner.state[index].down_since = None;
        inner.state[index].current_weight = 0;
    }

    /// Record a successful exchange: reset failures and mark healthy.
    fn succeed(&self, index: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.state[index].in_flight = inner.state[index].in_flight.saturating_sub(1);
        inner.state[index].failures = 0;
        inner.state[index].healthy = true;
        inner.state[index].down_since = None;
        inner.state[index].current_weight = 0;
    }

    /// Record a failed exchange: bump the streak, going down at `max_fails`.
    fn fail(&self, index: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.state[index].in_flight = inner.state[index].in_flight.saturating_sub(1);
        inner.state[index].failures += 1;
        if inner.state[index].failures >= self.servers[index].max_fails {
            inner.state[index].healthy = false;
            inner.state[index].down_since = Some(Instant::now());
        }
    }

    /// Release an in-flight slot without changing health (unreported drop).
    fn decrement_in_flight(&self, index: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.state[index].in_flight = inner.state[index].in_flight.saturating_sub(1);
    }

    /// Bring back any down server whose fail timeout has elapsed, so it gets a
    /// fresh chance on the next selection (nginx's `fail_timeout` retry).
    fn revive_expired(&self, inner: &mut GroupInner) {
        let now = Instant::now();
        for (index, state) in inner.state.iter_mut().enumerate() {
            if !state.healthy
                && let Some(since) = state.down_since
                && now.saturating_duration_since(since) >= self.servers[index].fail_timeout
            {
                state.healthy = true;
                state.failures = 0;
                state.down_since = None;
                state.current_weight = 0;
            }
        }
    }

    /// Pick the next peer per the balancing policy, considering healthy
    /// primaries first and backups only when no primary is healthy.
    fn select(&self, inner: &mut GroupInner) -> Option<usize> {
        let healthy: Vec<usize> = inner
            .state
            .iter()
            .enumerate()
            .filter(|(_, state)| state.healthy)
            .map(|(index, _)| index)
            .collect();
        if healthy.is_empty() {
            return None;
        }
        let primaries: Vec<usize> = healthy
            .iter()
            .copied()
            .filter(|&index| !self.servers[index].backup)
            .collect();
        let candidates = if primaries.is_empty() {
            // Every primary is down; fall back to healthy backups.
            healthy
        } else {
            primaries
        };
        match self.policy {
            BalancePolicy::RoundRobin | BalancePolicy::LeastConnections => {
                let chosen = if matches!(self.policy, BalancePolicy::RoundRobin) {
                    *candidates
                        .iter()
                        .find(|&&index| index >= inner.cursor)
                        .or_else(|| candidates.first())
                        .expect("candidates is non-empty")
                } else {
                    candidates
                        .iter()
                        .copied()
                        .min_by_key(|&index| inner.state[index].in_flight)
                        .expect("candidates is non-empty")
                };
                inner.cursor = (chosen + 1) % inner.state.len().max(1);
                Some(chosen)
            }
            BalancePolicy::WeightedRoundRobin => {
                let total: i64 = candidates
                    .iter()
                    .map(|&index| i64::from(self.servers[index].weight))
                    .sum();
                for &index in &candidates {
                    inner.state[index].current_weight += i64::from(self.servers[index].weight);
                }
                let chosen = candidates
                    .iter()
                    .copied()
                    .max_by_key(|&index| inner.state[index].current_weight)
                    .expect("candidates is non-empty");
                inner.state[chosen].current_weight -= total;
                Some(chosen)
            }
        }
    }
}

/// The kind of active probe to run against each server.
#[derive(Debug, Clone)]
pub enum ProbeKind {
    /// Just establish a TCP (or Unix socket) connection.
    Tcp,
    /// Send an HTTP GET and require a `2xx`/`3xx` status.
    Http { path: Vec<u8> },
}

/// Timing for the active health checker.
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// How often every server is probed.
    pub interval: Duration,
    /// Per-probe connect/read budget.
    pub timeout: Duration,
    /// What a successful probe must prove.
    pub kind: ProbeKind,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(5),
            timeout: Duration::from_secs(2),
            kind: ProbeKind::Tcp,
        }
    }
}

/// Probe one server, returning whether it is healthy.
fn probe_server(target: &ProxyTarget, config: &HealthCheckConfig) -> bool {
    match &config.kind {
        ProbeKind::Tcp => connect_with_timeout(&target.addr, config.timeout).is_ok(),
        ProbeKind::Http { path } => http_probe(target, path, config.timeout).is_ok(),
    }
}

/// A TCP-probe with an HTTP request/status check.
fn http_probe(target: &ProxyTarget, path: &[u8], timeout: Duration) -> io::Result<()> {
    let mut conn = connect_with_timeout(&target.addr, timeout)?;
    set_socket_timeout(conn.as_raw_fd(), SocketTimeoutSide::Read, Some(timeout))?;
    let host = target.host_header.as_bytes();
    let mut head = Vec::with_capacity(64 + path.len() + host.len());
    head.extend_from_slice(b"GET ");
    head.extend_from_slice(path);
    head.extend_from_slice(b" HTTP/1.1\r\nhost: ");
    head.extend_from_slice(host);
    head.extend_from_slice(b"\r\nconnection: close\r\n\r\n");
    conn.write_all(&head)?;
    let mut buf = [0u8; 1024];
    let mut read = Vec::new();
    while !read.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = conn.read(&mut buf)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "probe: connection closed",
            ));
        }
        read.extend_from_slice(&buf[..n]);
        if read.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "probe: response head too large",
            ));
        }
    }
    let line = read
        .splitn(2, |&b| b == b'\n')
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "probe: empty status line"))?;
    let line = std::str::from_utf8(line).unwrap_or("");
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "probe: bad status line"))?;
    if (200..400).contains(&status) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "probe: unexpected status {status}"
        )))
    }
}

/// A background thread that actively probes every server on an interval.
#[derive(Debug)]
pub struct HealthChecker {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HealthChecker {
    /// Spawn the probe loop over `group`.
    pub fn spawn(group: Arc<UpstreamGroup>, config: HealthCheckConfig) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                group.probe_all(&config);
                let deadline = Instant::now() + config.interval;
                while Instant::now() < deadline {
                    if thread_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    /// Stop the probe loop and wait for it to finish.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HealthChecker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Proxy one request through a balanced upstream group.
///
/// Each attempt selects the next peer via [`UpstreamGroup::next`], runs the
/// Phase 9 exchange against it, and feeds the outcome back to the group's
/// passive health. Connect/relay/prepare failures on bodyless idempotent
/// requests retry against other peers (capped at [`ProxyOptions::retries`]
/// extra attempts); once any byte has reached the client the exchange is not
/// retried. A peer that fails enough times is marked down and skipped by
/// subsequent selections.
#[allow(clippy::too_many_arguments)]
pub fn proxy_exchange_lb<C: Read + Write>(
    client: &mut C,
    request: &Request,
    matched_prefix: Option<&str>,
    group: &UpstreamGroup,
    options: &ProxyOptions,
    client_ip: &str,
    proto: &str,
) -> Result<ProxyOutcome, ExchangeError> {
    let retryable = request.framing == BodyFraming::None && request.method.is_idempotent();
    let mut attempts = options.retries.saturating_add(1);
    loop {
        let mut peer = match group.next(options) {
            Ok(peer) => peer,
            Err(PeerError::NoHealthyUpstream) => return Err(ExchangeError::NoHealthyUpstream),
            Err(PeerError::Connect(error)) => {
                if retryable && attempts > 1 {
                    attempts -= 1;
                    continue;
                }
                return Err(ExchangeError::Connect(error));
            }
        };
        attempts -= 1;
        let target = peer.target().clone();
        if let Err(error) = relay_request(
            client,
            peer.conn_mut(),
            request,
            matched_prefix,
            &target,
            client_ip,
            proto,
        ) {
            if is_upstream_failure(&error) {
                peer.fail();
            }
            if retryable && attempts > 0 && !matches!(error, ExchangeError::Relayed(_)) {
                drop(peer);
                continue;
            }
            return Err(error);
        }
        let prepared = match prepare_response(client, peer.conn_mut(), request) {
            Ok(prepared) => prepared,
            Err(error) => {
                if is_upstream_failure(&error) {
                    peer.fail();
                }
                if retryable && attempts > 0 && !matches!(error, ExchangeError::Relayed(_)) {
                    drop(peer);
                    continue;
                }
                return Err(error);
            }
        };
        if prepared.relay == BodyRelay::WsRelay {
            crate::proxy::exchange::clear_ws_timeouts(client, peer.conn_mut());
            let result = crate::proxy::websocket::ws_relay(client, peer.conn_mut());
            match result {
                Ok(()) => {
                    peer.finish_success(false);
                    return Ok(ProxyOutcome::Complete);
                }
                Err(e) => {
                    peer.fail();
                    return if e.kind() == io::ErrorKind::UnexpectedEof {
                        Err(ExchangeError::UpstreamEof)
                    } else {
                        Err(ExchangeError::Upstream(e))
                    };
                }
            }
        }
        let outcome = match relay_body(client, peer.conn_mut(), prepared) {
            Ok(outcome) => outcome,
            Err(error) => {
                if is_upstream_failure(&error) {
                    peer.fail();
                }
                return Err(error);
            }
        };
        peer.finish_success(outcome == ProxyOutcome::Complete);
        return Ok(outcome);
    }
}

/// Whether an exchange error is evidence the peer itself is unhealthy, as
/// opposed to a client-side problem (which must never penalize the upstream).
const fn is_upstream_failure(error: &ExchangeError) -> bool {
    matches!(
        error,
        ExchangeError::Connect(_)
            | ExchangeError::Upstream(_)
            | ExchangeError::UpstreamEof
            | ExchangeError::UpstreamHead(_)
            | ExchangeError::UpstreamBody(_)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        BalancePolicy, HealthCheckConfig, ProbeKind, ProxyOptions, UpstreamConfig, UpstreamGroup,
        UpstreamServer, proxy_exchange_lb,
    };
    use crate::http::{BodyFraming, Headers, Method, Request, Version};
    use crate::net::{InetAddr, Listener, SocketOptions};
    use crate::proxy::config::ProxyTarget;
    use crate::proxy::exchange::ProxyOutcome;
    use std::io::{Read, Write};
    use std::time::Duration;

    fn target(path: &std::path::Path) -> ProxyTarget {
        ProxyTarget::http(InetAddr::Unix(path.to_path_buf()))
    }

    /// Bind a unix listener and return it plus the path.
    fn listener(dir: &tempfile::TempDir, name: &str) -> (Listener, std::path::PathBuf) {
        let path = dir.path().join(name);
        let listener = Listener::bind(&InetAddr::Unix(path.clone()), SocketOptions::new()).unwrap();
        (listener, path)
    }

    /// Collect the sequence of peer indices a policy selects over `count`
    /// borrows, using real listeners so connections succeed.
    #[allow(clippy::collection_is_never_read)] // listeners only keep sockets alive
    fn selection_sequence(
        servers: Vec<UpstreamServer>,
        policy: BalancePolicy,
        count: usize,
    ) -> Vec<usize> {
        let dir = tempfile::tempdir().unwrap();
        let mut listeners = Vec::new();
        let servers: Vec<UpstreamServer> = servers
            .into_iter()
            .enumerate()
            .map(|(index, server)| {
                let (listener, path) = listener(&dir, &format!("s{index}.sock"));
                listeners.push(listener);
                UpstreamServer {
                    target: target(&path),
                    ..server
                }
            })
            .collect();
        let config = UpstreamConfig { servers, policy };
        let group = UpstreamGroup::from_config(&config);
        let options = ProxyOptions::default();
        let mut sequence = Vec::new();
        for _ in 0..count {
            let peer = group.next(&options).unwrap();
            sequence.push(peer.index());
            drop(peer);
        }
        sequence
    }

    #[test]
    fn round_robin_cycles_over_healthy_servers() {
        let a = UpstreamServer::new(target(std::path::Path::new("/tmp/a")));
        let b = UpstreamServer::new(target(std::path::Path::new("/tmp/b")));
        let c = UpstreamServer::new(target(std::path::Path::new("/tmp/c")));
        let sequence = selection_sequence(vec![a, b, c], BalancePolicy::RoundRobin, 6);
        assert_eq!(sequence, vec![0, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn weighted_round_robin_smooths_by_weight() {
        let a = UpstreamServer::new(target(std::path::Path::new("/tmp/a"))).with_weight(1);
        let b = UpstreamServer::new(target(std::path::Path::new("/tmp/b"))).with_weight(2);
        let c = UpstreamServer::new(target(std::path::Path::new("/tmp/c"))).with_weight(3);
        let sequence = selection_sequence(vec![a, b, c], BalancePolicy::WeightedRoundRobin, 6);
        // Smooth weighted round robin: c three times, b twice, a once, spread
        // out rather than clumped.
        assert_eq!(sequence, vec![2, 1, 2, 0, 1, 2]);
    }

    #[test]
    fn least_connections_picks_least_busy() {
        let dir = tempfile::tempdir().unwrap();
        let (listener_a, path_a) = listener(&dir, "a.sock");
        let (listener_b, path_b) = listener(&dir, "b.sock");
        let config = UpstreamConfig {
            servers: vec![
                UpstreamServer::new(target(&path_a)),
                UpstreamServer::new(target(&path_b)),
            ],
            policy: BalancePolicy::LeastConnections,
        };
        let group = UpstreamGroup::from_config(&config);
        let options = ProxyOptions::default();

        // Each selection goes to the least busy server: first A, then B.
        let hold_a = group.next(&options).unwrap();
        let hold_b = group.next(&options).unwrap();
        assert_eq!((hold_a.index(), hold_b.index()), (0, 1));

        // B finishes, leaving it idle. The next selection must pick B over the
        // busy A — a round-robin policy would have returned A here.
        drop(hold_b);
        let idle = group.next(&options).unwrap();
        assert_eq!(idle.index(), 1);
        drop(idle);
        drop(hold_a);
        drop(listener_a);
        drop(listener_b);
    }

    #[test]
    fn backups_only_used_when_all_primaries_down() {
        let dir = tempfile::tempdir().unwrap();
        let (listener_a, path_a) = listener(&dir, "a.sock");
        let (listener_b, path_b) = listener(&dir, "b.sock");
        let (listener_c, path_c) = listener(&dir, "c.sock");
        let config = UpstreamConfig {
            servers: vec![
                UpstreamServer::new(target(&path_a)),
                UpstreamServer::new(target(&path_b)),
                UpstreamServer::new(target(&path_c)).as_backup(),
            ],
            policy: BalancePolicy::RoundRobin,
        };
        let group = UpstreamGroup::from_config(&config);
        let options = ProxyOptions::default();

        // While primaries are healthy the backup is never picked.
        assert!(group.next(&options).unwrap().index() < 2);
        assert!(group.next(&options).unwrap().index() < 2);

        // Fail both primaries past their threshold.
        let mut a = group.next(&options).unwrap();
        let _ = &mut a;
        a.fail();
        let mut b = group.next(&options).unwrap();
        let _ = &mut b;
        b.fail();

        // The next selection is the backup.
        let backup = group.next(&options).unwrap();
        assert_eq!(backup.index(), 2);
        drop(backup);
        drop(listener_a);
        drop(listener_b);
        drop(listener_c);
    }

    #[test]
    fn passive_failures_mark_down_and_fail_timeout_revives() {
        let dir = tempfile::tempdir().unwrap();
        let (listener_a, path_a) = listener(&dir, "a.sock");
        let (listener_b, path_b) = listener(&dir, "b.sock");
        let config = UpstreamConfig {
            servers: vec![
                UpstreamServer::new(target(&path_a)).with_health(2, Duration::from_millis(80)),
                UpstreamServer::new(target(&path_b)),
            ],
            policy: BalancePolicy::RoundRobin,
        };
        let group = UpstreamGroup::from_config(&config);
        let options = ProxyOptions::default();

        // One failure is below the threshold of 2.
        let mut a = group.next(&options).unwrap();
        assert_eq!(a.index(), 0);
        a.fail();
        assert!(group.is_healthy(0));
        assert_eq!(group.failures(0), 1);

        // A full round-robin turn later, the second failure takes server A
        // down.
        let b = group.next(&options).unwrap();
        assert_eq!(b.index(), 1);
        drop(b);
        let mut a = group.next(&options).unwrap();
        assert_eq!(a.index(), 0);
        a.fail();
        assert!(!group.is_healthy(0));

        // Selections now skip the down server.
        let peer = group.next(&options).unwrap();
        assert_eq!(peer.index(), 1);
        drop(peer);

        // After fail_timeout the server is revived.
        std::thread::sleep(Duration::from_millis(120));
        let peer = group.next(&options).unwrap();
        assert_eq!(peer.index(), 0);
        drop(peer);
        drop(listener_a);
        drop(listener_b);
    }

    #[test]
    fn successful_exchange_resets_failure_streak() {
        let dir = tempfile::tempdir().unwrap();
        let (listener_a, path_a) = listener(&dir, "a.sock");
        let (listener_b, path_b) = listener(&dir, "b.sock");
        let config = UpstreamConfig {
            servers: vec![
                UpstreamServer::new(target(&path_a)).with_health(2, Duration::from_secs(10)),
                UpstreamServer::new(target(&path_b)),
            ],
            policy: BalancePolicy::RoundRobin,
        };
        let group = UpstreamGroup::from_config(&config);
        let options = ProxyOptions::default();

        let mut a = group.next(&options).unwrap();
        a.fail();
        assert_eq!(group.failures(0), 1);

        // A success resets the streak before it can reach the threshold.
        let peer = group.next(&options).unwrap();
        assert_eq!(peer.index(), 1);
        peer.finish_success(true);
        drop(listener_a);
        drop(listener_b);
    }

    #[test]
    fn active_tcp_probe_marks_peer_health() {
        let dir = tempfile::tempdir().unwrap();
        let (listener_up, path_up) = listener(&dir, "up.sock");
        let (listener_down, path_down) = listener(&dir, "down.sock");
        let config = UpstreamConfig {
            servers: vec![
                UpstreamServer::new(target(&path_up)).with_health(2, Duration::from_secs(10)),
                UpstreamServer::new(target(&path_down)).with_health(2, Duration::from_secs(10)),
            ],
            policy: BalancePolicy::RoundRobin,
        };
        let group = UpstreamGroup::from_config(&config);
        let check = HealthCheckConfig {
            kind: ProbeKind::Tcp,
            ..HealthCheckConfig::default()
        };

        // Both peers probe healthy (both listeners accept connections).
        group.probe_all(&check);
        assert!(group.is_healthy(0));
        assert!(group.is_healthy(1));

        // Shut down peer B's listener: the probe now fails and, after
        // max_fails (2) consecutive failures, marks it down.
        drop(listener_down);
        group.probe_all(&check);
        assert!(group.is_healthy(1));
        group.probe_all(&check);
        assert!(!group.is_healthy(1));
        drop(listener_up);
    }

    #[test]
    fn active_http_probe_requires_2xx_or_3xx() {
        let dir = tempfile::tempdir().unwrap();
        let (listener_ok, path_ok) = listener(&dir, "ok.sock");
        let (listener_err, path_err) = listener(&dir, "err.sock");
        // Serve a 200 on one, a 500 on the other.
        let server_ok = std::thread::spawn(move || {
            let mut conn = listener_ok.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = conn.read(&mut buf);
            conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let server_err = std::thread::spawn(move || {
            let mut conn = listener_err.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = conn.read(&mut buf);
            conn.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });

        let config = UpstreamConfig {
            servers: vec![
                UpstreamServer::new(target(&path_ok)).with_health(2, Duration::from_secs(10)),
                UpstreamServer::new(target(&path_err)).with_health(2, Duration::from_secs(10)),
            ],
            policy: BalancePolicy::RoundRobin,
        };
        let group = UpstreamGroup::from_config(&config);
        let check = HealthCheckConfig {
            kind: ProbeKind::Http {
                path: b"/health".to_vec(),
            },
            ..HealthCheckConfig::default()
        };
        group.probe_all(&check);
        assert!(group.is_healthy(0));
        assert!(group.is_healthy(1));
        // A second probe of the 500 peer takes it down (max_fails = 2).
        group.probe_all(&check);
        assert!(!group.is_healthy(1));
        server_ok.join().unwrap();
        server_err.join().unwrap();
    }

    #[test]
    fn end_to_end_balancing_over_two_healthy_servers() {
        let dir = tempfile::tempdir().unwrap();
        let (listener_a, path_a) = listener(&dir, "a.sock");
        let (listener_b, path_b) = listener(&dir, "b.sock");
        let server_a = std::thread::spawn(move || {
            let mut conn = listener_a.accept().unwrap();
            let mut tmp = [0u8; 1024];
            let mut buf = Vec::new();
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nfrom-a")
                .unwrap();
        });
        let server_b = std::thread::spawn(move || {
            let mut conn = listener_b.accept().unwrap();
            let mut tmp = [0u8; 1024];
            let mut buf = Vec::new();
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nfrom-b")
                .unwrap();
        });

        let config = UpstreamConfig {
            servers: vec![
                UpstreamServer::new(target(&path_a)),
                UpstreamServer::new(target(&path_b)),
            ],
            policy: BalancePolicy::RoundRobin,
        };
        let group = UpstreamGroup::from_config(&config);
        let options = ProxyOptions::default();
        let request = Request::new(
            Method::Get,
            b"/".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::None,
        );

        for (i, expected) in ["from-a", "from-b"].iter().enumerate() {
            let mut client = TestIo::new(b"");
            let outcome = proxy_exchange_lb(
                &mut client,
                &request,
                None,
                &group,
                &options,
                "10.0.0.1",
                "http",
            )
            .unwrap();
            assert_eq!(outcome, ProxyOutcome::Complete);
            let body = std::str::from_utf8(&client.output).unwrap();
            assert!(body.ends_with(*expected), "request {i}: {body}");
        }
        server_a.join().unwrap();
        server_b.join().unwrap();
    }

    /// An in-memory peer: serves `input` on reads and records `output`.
    #[derive(Debug)]
    struct TestIo {
        input: Vec<u8>,
        pos: usize,
        output: Vec<u8>,
    }

    impl TestIo {
        fn new(input: &[u8]) -> Self {
            Self {
                input: input.to_vec(),
                pos: 0,
                output: Vec::new(),
            }
        }
    }

    impl Read for TestIo {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = (self.input.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    impl Write for TestIo {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
