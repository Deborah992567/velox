//! Proxy connection pool statistics and monitoring.
//!
//! Tracks per-upstream connection pool metrics for observability.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Aggregate pool statistics snapshot.
#[derive(Debug, Clone, Copy, Default)]
pub struct PoolStats {
    pub total_conns: u64,
    pub idle_conns: u64,
    pub active_conns: u64,
    pub waiters: u64,
    pub created_total: u64,
    pub reused_total: u64,
    pub timed_out_total: u64,
    pub errors_total: u64,
}

impl PoolStats {
    #[allow(clippy::cast_precision_loss)]
    pub fn utilization(&self) -> f64 {
        let total = self.total_conns;
        if total == 0 {
            0.0
        } else {
            self.active_conns as f64 / total as f64
        }
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn reuse_ratio(&self) -> f64 {
        let total = self.created_total + self.reused_total;
        if total == 0 {
            0.0
        } else {
            self.reused_total as f64 / total as f64
        }
    }
}

/// Atomic counters for live pool metrics.
#[derive(Debug)]
pub struct PoolMetrics {
    pub created: AtomicU64,
    pub reused: AtomicU64,
    pub timed_out: AtomicU64,
    pub errors: AtomicU64,
    pub active: AtomicU64,
    pub idle: AtomicU64,
    pub waiters: AtomicU64,
}

impl Default for PoolMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl PoolMetrics {
    pub const fn new() -> Self {
        Self {
            created: AtomicU64::new(0),
            reused: AtomicU64::new(0),
            timed_out: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            active: AtomicU64::new(0),
            idle: AtomicU64::new(0),
            waiters: AtomicU64::new(0),
        }
    }

    pub fn snapshot(&self) -> PoolStats {
        PoolStats {
            total_conns: self.active.load(Ordering::Relaxed) + self.idle.load(Ordering::Relaxed),
            idle_conns: self.idle.load(Ordering::Relaxed),
            active_conns: self.active.load(Ordering::Relaxed),
            waiters: self.waiters.load(Ordering::Relaxed),
            created_total: self.created.load(Ordering::Relaxed),
            reused_total: self.reused.load(Ordering::Relaxed),
            timed_out_total: self.timed_out.load(Ordering::Relaxed),
            errors_total: self.errors.load(Ordering::Relaxed),
        }
    }

    pub fn record_created(&self) {
        self.created.fetch_add(1, Ordering::Relaxed);
        self.active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_reused(&self) {
        self.reused.fetch_add(1, Ordering::Relaxed);
        self.active.fetch_add(1, Ordering::Relaxed);
        self.idle.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn record_returned(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
        self.idle.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_closed(&self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn record_timed_out(&self) {
        self.timed_out.fetch_add(1, Ordering::Relaxed);
        self.active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_waiter_start(&self) {
        self.waiters.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_waiter_done(&self) {
        self.waiters.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Per-upstream connection age tracker.
#[derive(Debug, Clone)]
pub struct ConnectionAge {
    pub created_at: Instant,
    pub last_used: Instant,
    pub request_count: u64,
}

impl Default for ConnectionAge {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionAge {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            created_at: now,
            last_used: now,
            request_count: 0,
        }
    }

    pub fn on_request(&mut self) {
        self.last_used = Instant::now();
        self.request_count += 1;
    }

    pub fn age(&self) -> Duration {
        self.created_at.elapsed()
    }

    pub fn idle_time(&self) -> Duration {
        self.last_used.elapsed()
    }

    pub fn should_recycle(&self, max_age: Duration, max_idle: Duration) -> bool {
        self.age() >= max_age || self.idle_time() >= max_idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_metrics_snapshot() {
        let m = PoolMetrics::new();
        m.record_created();
        m.record_created();
        m.record_returned();
        let s = m.snapshot();
        assert_eq!(s.created_total, 2);
        assert_eq!(s.active_conns, 1);
        assert_eq!(s.idle_conns, 1);
    }

    #[test]
    fn record_reused() {
        let m = PoolMetrics::new();
        m.record_created();
        m.record_returned();
        m.record_reused();
        let s = m.snapshot();
        assert_eq!(s.reused_total, 1);
        assert_eq!(s.active_conns, 1);
        assert_eq!(s.idle_conns, 0);
    }

    #[test]
    fn record_timed_out() {
        let m = PoolMetrics::new();
        m.record_created();
        m.record_timed_out();
        let s = m.snapshot();
        assert_eq!(s.timed_out_total, 1);
        assert_eq!(s.active_conns, 0);
    }

    #[test]
    fn pool_stats_utilization() {
        let s = PoolStats {
            active_conns: 3,
            idle_conns: 7,
            total_conns: 10,
            ..Default::default()
        };
        assert!((s.utilization() - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn pool_stats_reuse_ratio() {
        let s = PoolStats {
            created_total: 3,
            reused_total: 7,
            ..Default::default()
        };
        assert!((s.reuse_ratio() - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn pool_stats_empty() {
        let s = PoolStats::default();
        assert!(s.utilization().abs() < f64::EPSILON);
        assert!(s.reuse_ratio().abs() < f64::EPSILON);
    }

    #[test]
    fn connection_age() {
        let mut age = ConnectionAge::new();
        assert!(age.age() < Duration::from_secs(1));
        assert!(age.idle_time() < Duration::from_secs(1));
        age.on_request();
        assert_eq!(age.request_count, 1);
    }

    #[test]
    fn should_recycle() {
        let mut age = ConnectionAge::new();
        let hour = Duration::from_hours(1);
        assert!(!age.should_recycle(hour, hour));
        age.last_used = Instant::now().checked_sub(Duration::from_mins(1)).unwrap();
        assert!(age.should_recycle(hour, Duration::from_secs(30)));
    }

    #[test]
    fn waiter_tracking() {
        let m = PoolMetrics::new();
        m.record_waiter_start();
        m.record_waiter_start();
        assert_eq!(m.snapshot().waiters, 2);
        m.record_waiter_done();
        assert_eq!(m.snapshot().waiters, 1);
    }
}
