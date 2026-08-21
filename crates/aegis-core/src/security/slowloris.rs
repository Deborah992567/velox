//! Slowloris / slow-attack detection and mitigation.
//!
//! Phase 24: Tracks connection timing to detect slow clients.

use std::time::{Duration, Instant};

/// Configuration for slowloris detection.
#[derive(Debug, Clone)]
pub struct SlowlorisConfig {
    /// Max time to wait for headers to complete.
    pub header_timeout: Duration,
    /// Max time to wait for body chunks.
    pub body_timeout: Duration,
    /// Max time for the initial connection to send first byte.
    pub connect_timeout: Duration,
    /// Max total request time from accept to complete.
    pub request_timeout: Duration,
}

impl Default for SlowlorisConfig {
    fn default() -> Self {
        Self {
            header_timeout: Duration::from_secs(30),
            body_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_mins(1),
        }
    }
}

/// Tracks timing for a single connection.
#[derive(Debug)]
#[allow(clippy::struct_field_names)]
pub struct ConnectionTimer {
    accepted_at: Instant,
    first_header_at: Option<Instant>,
    last_activity_at: Instant,
}

impl ConnectionTimer {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            accepted_at: now,
            first_header_at: None,
            last_activity_at: now,
        }
    }

    pub fn mark_header(&mut self) {
        if self.first_header_at.is_none() {
            self.first_header_at = Some(Instant::now());
        }
        self.last_activity_at = Instant::now();
    }

    pub fn mark_activity(&mut self) {
        self.last_activity_at = Instant::now();
    }

    pub fn elapsed_since_accept(&self) -> Duration {
        self.accepted_at.elapsed()
    }

    pub fn elapsed_since_last_activity(&self) -> Duration {
        self.last_activity_at.elapsed()
    }

    pub fn is_timed_out(&self, config: &SlowlorisConfig) -> Option<TimedOutReason> {
        let since_accept = self.elapsed_since_accept();
        if since_accept >= config.request_timeout {
            return Some(TimedOutReason::RequestTimeout);
        }

        if self.first_header_at.is_none() && since_accept >= config.connect_timeout {
            return Some(TimedOutReason::ConnectTimeout);
        }

        if self.first_header_at.is_some()
            && self.elapsed_since_last_activity() >= config.header_timeout
        {
            return Some(TimedOutReason::HeaderTimeout);
        }

        None
    }
}

impl Default for ConnectionTimer {
    fn default() -> Self {
        Self::new()
    }
}

/// Reason a connection was timed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimedOutReason {
    ConnectTimeout,
    HeaderTimeout,
    RequestTimeout,
}

impl std::fmt::Display for TimedOutReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectTimeout => write!(f, "connect timeout"),
            Self::HeaderTimeout => write!(f, "header timeout"),
            Self::RequestTimeout => write!(f, "request timeout"),
        }
    }
}

/// Connection tracker for a server.
#[derive(Debug)]
pub struct ConnectionTracker {
    active: std::collections::HashMap<u64, ConnectionTimer>,
    next_id: u64,
    config: SlowlorisConfig,
}

impl ConnectionTracker {
    pub fn new(config: SlowlorisConfig) -> Self {
        Self {
            active: std::collections::HashMap::new(),
            next_id: 1,
            config,
        }
    }

    pub fn accept(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.active.insert(id, ConnectionTimer::new());
        id
    }

    pub fn mark_header(&mut self, id: u64) {
        if let Some(timer) = self.active.get_mut(&id) {
            timer.mark_header();
        }
    }

    pub fn mark_activity(&mut self, id: u64) {
        if let Some(timer) = self.active.get_mut(&id) {
            timer.mark_activity();
        }
    }

    pub fn check_timeout(&self, id: u64) -> Option<TimedOutReason> {
        self.active
            .get(&id)
            .and_then(|t| t.is_timed_out(&self.config))
    }

    pub fn remove(&mut self, id: u64) {
        self.active.remove(&id);
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Sweep and return IDs that have timed out.
    pub fn sweep(&self) -> Vec<(u64, TimedOutReason)> {
        self.active
            .iter()
            .filter_map(|(&id, timer)| timer.is_timed_out(&self.config).map(|reason| (id, reason)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let c = SlowlorisConfig::default();
        assert_eq!(c.header_timeout, Duration::from_secs(30));
        assert_eq!(c.connect_timeout, Duration::from_secs(10));
        assert_eq!(c.request_timeout, Duration::from_mins(1));
    }

    #[test]
    fn timer_fresh_no_timeout() {
        let t = ConnectionTimer::new();
        let c = SlowlorisConfig::default();
        assert!(t.is_timed_out(&c).is_none());
    }

    #[test]
    fn timer_connect_timeout() {
        let c = SlowlorisConfig {
            connect_timeout: Duration::ZERO,
            ..Default::default()
        };
        let t = ConnectionTimer::new();
        assert_eq!(t.is_timed_out(&c), Some(TimedOutReason::ConnectTimeout));
    }

    #[test]
    fn timer_header_timeout() {
        let c = SlowlorisConfig {
            header_timeout: Duration::ZERO,
            connect_timeout: Duration::from_hours(1),
            request_timeout: Duration::from_hours(1),
            body_timeout: Duration::from_hours(1),
        };
        let mut t = ConnectionTimer::new();
        t.mark_header();
        assert_eq!(t.is_timed_out(&c), Some(TimedOutReason::HeaderTimeout));
    }

    #[test]
    fn timer_request_timeout() {
        let c = SlowlorisConfig {
            request_timeout: Duration::ZERO,
            ..Default::default()
        };
        let t = ConnectionTimer::new();
        assert_eq!(t.is_timed_out(&c), Some(TimedOutReason::RequestTimeout));
    }

    #[test]
    fn tracker_lifecycle() {
        let mut tracker = ConnectionTracker::new(SlowlorisConfig::default());
        let id = tracker.accept();
        assert_eq!(tracker.active_count(), 1);
        tracker.mark_header(id);
        tracker.mark_activity(id);
        assert!(tracker.check_timeout(id).is_none());
        tracker.remove(id);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn sweep_finds_timeouts() {
        let config = SlowlorisConfig {
            connect_timeout: Duration::ZERO,
            ..Default::default()
        };
        let mut tracker = ConnectionTracker::new(config);
        let _id1 = tracker.accept();
        let _id2 = tracker.accept();
        let timed_out = tracker.sweep();
        assert_eq!(timed_out.len(), 2);
    }

    #[test]
    fn timed_out_reason_display() {
        assert_eq!(
            TimedOutReason::ConnectTimeout.to_string(),
            "connect timeout"
        );
    }
}
