//! Keepalive connection pool for upstream targets.
//!
//! Phase 9 opened a fresh upstream connection for every proxied request. Phase
//! 10 keeps those connections alive between requests: a borrowed connection is
//! returned to the pool when the exchange ended cleanly (a fully delimited
//! response consumed exactly to the message boundary), and reused by the next
//! request to the same target. Stale connections — the peer closed them, or
//! leftover pipelined bytes make them untrustworthy — are detected with a
//! non-blocking peek and dropped before reuse.
//!
//! The pool is bounded by [`PoolOptions::max_connections`] (connections in
//! use plus idle). A [`borrow`](UpstreamPool::borrow) that finds the pool
//! saturated waits on a condition variable until a connection returns or
//! [`PoolOptions::acquire_timeout`] elapses, which mirrors a proxy worker
//! queueing on a saturated upstream. `try_borrow` is the non-blocking form.
//!
//! One pool instance serves one worker; it is `Send + Sync` so a worker's
//! reactor can share it. Because a proxied exchange occupies its thread for
//! its whole duration, the wait cannot deadlock on the very thread that owns
//! the saturated connections.

use std::collections::VecDeque;
use std::io;
use std::os::fd::AsRawFd;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::net::{
    Connection, Peek, SocketTimeoutSide, connect_with_timeout, peek, set_socket_timeout,
};
use crate::proxy::config::{ProxyOptions, ProxyTarget};

/// Pool sizing and lifetime limits.
#[derive(Debug, Clone, Copy)]
pub struct PoolOptions {
    /// Total connections (in use + idle) the pool may hold for one target.
    pub max_connections: usize,
    /// How many idle connections are kept before surplus ones are closed.
    pub max_idle: usize,
    /// How long an idle connection may sit before it is closed.
    pub idle_timeout: Duration,
    /// How long a saturated [`UpstreamPool::borrow`] waits for a connection.
    pub acquire_timeout: Duration,
}

impl Default for PoolOptions {
    fn default() -> Self {
        Self {
            max_connections: 32,
            max_idle: 8,
            idle_timeout: Duration::from_mins(1),
            acquire_timeout: Duration::from_secs(5),
        }
    }
}

/// One idle connection and when it entered the pool.
struct IdleConnection {
    connection: Connection,
    since: Instant,
}

/// The connections not currently in use, plus how many are borrowed.
struct PoolInner {
    idle: VecDeque<IdleConnection>,
    in_use: usize,
}

/// A keepalive pool of upstream connections.
///
/// The pool does not care which target a connection belongs to: the caller
/// (the proxy) passes the right `target` when borrowing, and a returned
/// connection is only ever handed back to the same target. A pool per worker
/// with one target per pool is the common configuration.
pub struct UpstreamPool {
    inner: Mutex<PoolInner>,
    signal: Condvar,
    options: PoolOptions,
}

impl Default for UpstreamPool {
    fn default() -> Self {
        Self::new(PoolOptions::default())
    }
}

impl std::fmt::Debug for UpstreamPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().unwrap();
        f.debug_struct("UpstreamPool")
            .field("idle", &inner.idle.len())
            .field("in_use", &inner.in_use)
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl UpstreamPool {
    /// A pool with the given sizing and lifetime limits.
    pub const fn new(options: PoolOptions) -> Self {
        Self {
            inner: Mutex::new(PoolInner {
                idle: VecDeque::new(),
                in_use: 0,
            }),
            signal: Condvar::new(),
            options,
        }
    }

    /// The sizing and lifetime limits in effect.
    pub const fn options(&self) -> PoolOptions {
        self.options
    }

    /// Borrow a connection, connecting to the target if none is idle.
    ///
    /// Returns immediately when a healthy idle connection exists or the pool
    /// has capacity to open a new one. When the pool is saturated it waits up
    /// to [`PoolOptions::acquire_timeout`] for a connection to be returned,
    /// failing with `TimedOut` if none appears in time. The returned guard
    /// returns the connection to the pool (if it was [`PooledConnection`]'
    /// reuse flag, i.e. the exchange ended at a message boundary) or closes
    /// it, on drop.
    ///
    /// # Panics
    ///
    /// Panics if the pool's mutex is poisoned (a previous borrower panicked
    /// while holding it).
    pub fn borrow(
        &self,
        target: &ProxyTarget,
        options: &ProxyOptions,
    ) -> io::Result<PooledConnection<'_>> {
        let deadline = Instant::now() + self.options.acquire_timeout;
        loop {
            match self.try_borrow(target, options) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "upstream pool saturated",
                        ));
                    }
                    let (guard, result) = self
                        .signal
                        .wait_timeout(
                            self.inner.lock().unwrap(),
                            deadline.checked_duration_since(now).unwrap_or_default(),
                        )
                        .unwrap();
                    drop(guard);
                    if result.timed_out() {
                        // One final attempt so a release racing the timeout
                        // still wins; a saturated pool otherwise reports the
                        // timeout that was already due.
                        return self.try_borrow(target, options).map_err(|_| {
                            io::Error::new(io::ErrorKind::TimedOut, "upstream pool saturated")
                        });
                    }
                }
                other => return other,
            }
        }
    }

    /// Borrow a connection without waiting: either a healthy idle connection,
    /// or a freshly connected one when the pool has capacity. Returns
    /// `WouldBlock` when the pool is saturated and no idle connection exists.
    ///
    /// # Panics
    ///
    /// Panics if the pool's mutex is poisoned (a previous borrower panicked
    /// while holding it).
    pub fn try_borrow(
        &self,
        target: &ProxyTarget,
        options: &ProxyOptions,
    ) -> io::Result<PooledConnection<'_>> {
        let mut inner = self.inner.lock().unwrap();
        self.evict_expired(&mut inner);
        while let Some(idle) = inner.idle.pop_front() {
            let conn = idle.connection;
            match peek(conn.as_raw_fd()) {
                Ok(Peek::Empty) => {
                    inner.in_use += 1;
                    return Ok(PooledConnection {
                        connection: Some(conn),
                        pool: self,
                        reusable: false,
                    });
                }
                Ok(Peek::Data | Peek::Eof) | Err(_) => {
                    // Stale pipelined bytes, a dead peer, or an I/O error:
                    // the connection cannot be reused, so drop it and try the
                    // next idle one.
                }
            }
        }
        if inner.in_use + inner.idle.len() >= self.options.max_connections {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "upstream pool at capacity",
            ));
        }
        inner.in_use += 1;
        drop(inner);
        match connect_target_socket(target, options) {
            Ok(conn) => Ok(PooledConnection {
                connection: Some(conn),
                pool: self,
                reusable: false,
            }),
            Err(error) => {
                let mut inner = self.inner.lock().unwrap();
                inner.in_use -= 1;
                drop(inner);
                self.signal.notify_one();
                Err(error)
            }
        }
    }

    /// Drop every idle connection, returning how many were closed. Used at
    /// shutdown or on a pool reconfiguration.
    ///
    /// # Panics
    ///
    /// Panics if the pool's mutex is poisoned.
    pub fn close_idle(&self) -> usize {
        let mut inner = self.inner.lock().unwrap();
        let closed = inner.idle.len();
        inner.idle.clear();
        closed
    }

    /// How many connections are currently borrowed.
    ///
    /// # Panics
    ///
    /// Panics if the pool's mutex is poisoned.
    pub fn in_use(&self) -> usize {
        self.inner.lock().unwrap().in_use
    }

    /// How many connections are idle in the pool.
    ///
    /// # Panics
    ///
    /// Panics if the pool's mutex is poisoned.
    pub fn idle_len(&self) -> usize {
        self.inner.lock().unwrap().idle.len()
    }

    /// Total connections the pool is tracking (in use plus idle).
    ///
    /// # Panics
    ///
    /// Panics if the pool's mutex is poisoned.
    pub fn total(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.in_use + inner.idle.len()
    }

    /// Close idle connections that have sat longer than the idle timeout.
    fn evict_expired(&self, inner: &mut PoolInner) {
        let cutoff = Instant::now()
            .checked_sub(self.options.idle_timeout)
            .unwrap_or_else(Instant::now);
        inner.idle.retain(|idle| idle.since >= cutoff);
    }

    /// Return a connection to the pool, or close it.
    ///
    /// `reusable` is true only when the exchange ended at a message boundary
    /// with the body fully consumed; otherwise the connection is dropped.
    /// Keeping at most [`PoolOptions::max_idle`] idle connections bounds how
    /// many a target holds on to.
    fn release(&self, conn: Connection, reusable: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.in_use -= 1;
        if reusable && inner.idle.len() < self.options.max_idle {
            inner.idle.push_back(IdleConnection {
                connection: conn,
                since: Instant::now(),
            });
        } else {
            drop(conn);
        }
        drop(inner);
        self.signal.notify_one();
    }
}

/// A borrowed pool connection.
///
/// On drop the connection returns to the pool when [`PooledConnection::mark_reusable`]
/// was called, and is closed otherwise.
pub struct PooledConnection<'a> {
    connection: Option<Connection>,
    pool: &'a UpstreamPool,
    reusable: bool,
}

impl PooledConnection<'_> {
    /// The underlying connection.
    ///
    /// # Panics
    ///
    /// Panics if the connection was already returned to the pool (a guard is
    /// only usable once).
    #[allow(clippy::missing_const_for_fn)]
    pub fn conn_mut(&mut self) -> &mut Connection {
        self.connection.as_mut().expect("connection present")
    }

    /// Mark the connection as ending at a message boundary, so it is kept
    /// alive for the next request instead of being closed.
    pub const fn mark_reusable(&mut self) {
        self.reusable = true;
    }
}

impl Drop for PooledConnection<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.connection.take() {
            self.pool.release(conn, self.reusable);
        }
    }
}

impl io::Read for PooledConnection<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.conn_mut().read(buf)
    }
}

impl io::Write for PooledConnection<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.conn_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.conn_mut().flush()
    }
}

impl AsRawFd for PooledConnection<'_> {
    fn as_raw_fd(&self) -> i32 {
        self.connection
            .as_ref()
            .expect("connection present")
            .as_raw_fd()
    }
}

impl std::fmt::Debug for PooledConnection<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.connection {
            Some(conn) => write!(f, "PooledConnection({conn:?}, reusable={})", self.reusable),
            None => write!(f, "PooledConnection(returned)"),
        }
    }
}

/// Connect to a target and pin the exchange's read/write timeouts on the
/// socket. Shared by the pool and the direct-exchange path.
fn connect_target_socket(target: &ProxyTarget, options: &ProxyOptions) -> io::Result<Connection> {
    let conn = connect_with_timeout(&target.addr, options.connect_timeout)?;
    set_socket_timeout(
        conn.as_raw_fd(),
        SocketTimeoutSide::Read,
        Some(options.read_timeout),
    )?;
    set_socket_timeout(
        conn.as_raw_fd(),
        SocketTimeoutSide::Write,
        Some(options.send_timeout),
    )?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::{PoolOptions, UpstreamPool};
    use crate::net::{InetAddr, Listener, SocketOptions};
    use crate::proxy::config::{ProxyOptions, ProxyTarget};
    use std::io::ErrorKind;
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    fn unix_target(path: std::path::PathBuf) -> ProxyTarget {
        ProxyTarget::http(InetAddr::Unix(path))
    }

    fn listener() -> (tempfile::TempDir, Listener, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upstream.sock");
        let listener = Listener::bind(&InetAddr::Unix(path.clone()), SocketOptions::new()).unwrap();
        (dir, listener, path)
    }

    #[test]
    fn borrow_connects_and_reuses_same_fd() {
        let (_dir, listener, path) = listener();
        let pool = UpstreamPool::default();
        let options = ProxyOptions::default();
        let target = unix_target(path);

        let mut first = pool.borrow(&target, &options).unwrap();
        let first_fd = first.as_raw_fd();
        first.mark_reusable();
        drop(first);

        let mut second = pool.borrow(&target, &options).unwrap();
        assert_eq!(second.as_raw_fd(), first_fd);
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.in_use(), 1);
        second.mark_reusable();
        drop(second);
        assert_eq!(pool.in_use(), 0);
        assert_eq!(pool.idle_len(), 1);
        drop(listener);
    }

    #[test]
    fn stale_connection_with_leftover_bytes_is_replaced() {
        let (_dir, listener, path) = listener();
        let pool = UpstreamPool::default();
        let options = ProxyOptions::default();
        let target = unix_target(path);

        let mut conn = pool.borrow(&target, &options).unwrap();
        let mut server = listener.accept().unwrap();
        // The upstream pipelined an extra byte past the message boundary.
        server.write_all(b"x").unwrap();
        conn.mark_reusable();
        drop(conn);

        // The pool sees the leftover byte, discards the connection, and opens
        // a fresh one rather than reusing a stream that is not at a boundary.
        let fresh = pool.borrow(&target, &options).unwrap();
        assert_eq!(pool.total(), 1);
        // The discarded connection's peer sees EOF.
        let mut buf = [0u8; 1];
        assert_eq!(server.read(&mut buf).unwrap(), 0);
        // A second connection arrived on the listener: a fresh one was opened.
        drop(listener.accept().unwrap());
        drop(fresh);
    }

    #[test]
    fn non_reusable_return_closes_connection() {
        let (_dir, listener, path) = listener();
        let pool = UpstreamPool::default();
        let options = ProxyOptions::default();
        let target = unix_target(path);

        let conn = pool.borrow(&target, &options).unwrap();
        let mut server = listener.accept().unwrap();

        // The connection was closed on return (no mark_reusable), so the peer
        // sees EOF, and nothing was kept for reuse.
        drop(conn);
        let mut buf = [0u8; 1];
        assert_eq!(server.read(&mut buf).unwrap(), 0);
        assert_eq!(pool.idle_len(), 0);
        assert_eq!(pool.total(), 0);
        drop(listener);
    }

    #[test]
    fn try_borrow_is_blocked_at_capacity() {
        let (_dir, listener, path) = listener();
        let options = ProxyOptions::default();
        let pool = UpstreamPool::new(PoolOptions {
            max_connections: 1,
            ..PoolOptions::default()
        });
        let target = unix_target(path);

        let held = pool.borrow(&target, &options).unwrap();
        let error = pool.try_borrow(&target, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::WouldBlock);
        drop(held);
        let next = pool.try_borrow(&target, &options).unwrap();
        assert_eq!(pool.total(), 1);
        drop(next);
        drop(listener);
    }

    #[test]
    fn saturated_borrow_times_out() {
        let (_dir, listener, path) = listener();
        let options = ProxyOptions::default();
        let pool = UpstreamPool::new(PoolOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_millis(100),
            ..PoolOptions::default()
        });
        let target = unix_target(path);

        let held = pool.borrow(&target, &options).unwrap();
        let started = std::time::Instant::now();
        let error = pool.borrow(&target, &options).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(90));
        drop(held);
        drop(listener);
    }

    #[test]
    fn saturated_borrow_waits_for_release() {
        let (_dir, listener, path) = listener();
        let options = ProxyOptions::default();
        let pool = UpstreamPool::new(PoolOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            ..PoolOptions::default()
        });
        let target = unix_target(path);

        let held = pool.borrow(&target, &options).unwrap();
        let conn = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| pool.borrow(&target, &options).unwrap());
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
            waiter.join().unwrap()
        });
        assert_eq!(pool.total(), 1);
        drop(conn);
        drop(listener);
    }

    #[test]
    fn max_idle_trims_surplus() {
        let (_dir, listener, path) = listener();
        let options = ProxyOptions::default();
        let pool = UpstreamPool::new(PoolOptions {
            max_connections: 4,
            max_idle: 2,
            ..PoolOptions::default()
        });
        let target = unix_target(path);

        let mut a = pool.borrow(&target, &options).unwrap();
        let mut b = pool.borrow(&target, &options).unwrap();
        let mut c = pool.borrow(&target, &options).unwrap();
        a.mark_reusable();
        b.mark_reusable();
        c.mark_reusable();
        drop(a);
        drop(b);
        drop(c);
        assert_eq!(pool.idle_len(), 2);
        assert_eq!(pool.total(), 2);
        drop(listener);
    }

    #[test]
    fn close_idle_closes_all_idle_connections() {
        let (_dir, listener, path) = listener();
        let options = ProxyOptions::default();
        let pool = UpstreamPool::default();
        let target = unix_target(path);

        let mut a = pool.borrow(&target, &options).unwrap();
        a.mark_reusable();
        drop(a);
        assert_eq!(pool.close_idle(), 1);
        assert_eq!(pool.idle_len(), 0);
        drop(listener);
    }

    #[test]
    fn idle_expiry_evicts_when_borrowing() {
        let (_dir, listener, path) = listener();
        let options = ProxyOptions::default();
        let pool = UpstreamPool::new(PoolOptions {
            idle_timeout: Duration::from_millis(50),
            ..PoolOptions::default()
        });
        let target = unix_target(path);

        let mut conn = pool.borrow(&target, &options).unwrap();
        let mut server = listener.accept().unwrap();
        conn.mark_reusable();
        drop(conn);
        assert_eq!(pool.idle_len(), 1);
        std::thread::sleep(Duration::from_millis(120));
        let fresh = pool.borrow(&target, &options).unwrap();
        assert_eq!(pool.total(), 1);
        // The expired idle connection was closed before reuse: its peer EOFs.
        let mut buf = [0u8; 1];
        assert_eq!(server.read(&mut buf).unwrap(), 0);
        drop(fresh);
        drop(listener);
    }
}
