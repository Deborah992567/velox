//! Persistent internal connection pool stats endpoint.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Server-wide connection statistics.
#[derive(Debug)]
pub struct ConnStats {
    pub total_accepted: AtomicU64,
    pub total_closed: AtomicU64,
    pub total_errors: AtomicU64,
    pub currently_open: AtomicU64,
    pub peak_open: AtomicU64,
    pub graceful_shutdown: AtomicBool,
}

impl Default for ConnStats {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnStats {
    pub const fn new() -> Self {
        Self {
            total_accepted: AtomicU64::new(0),
            total_closed: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
            currently_open: AtomicU64::new(0),
            peak_open: AtomicU64::new(0),
            graceful_shutdown: AtomicBool::new(false),
        }
    }

    pub fn record_accept(&self) {
        self.total_accepted.fetch_add(1, Ordering::Relaxed);
        let open = self.currently_open.fetch_add(1, Ordering::Relaxed) + 1;
        self.peak_open.fetch_max(open, Ordering::Relaxed);
    }

    pub fn record_close(&self) {
        self.total_closed.fetch_add(1, Ordering::Relaxed);
        self.currently_open.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn record_error(&self) {
        self.total_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_graceful_shutdown(&self, val: bool) {
        self.graceful_shutdown.store(val, Ordering::Relaxed);
    }

    pub fn is_graceful_shutdown(&self) -> bool {
        self.graceful_shutdown.load(Ordering::Relaxed)
    }

    pub fn currently_open(&self) -> u64 {
        self.currently_open.load(Ordering::Relaxed)
    }

    pub fn peak_open(&self) -> u64 {
        self.peak_open.load(Ordering::Relaxed)
    }

    pub fn total_accepted(&self) -> u64 {
        self.total_accepted.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_and_close() {
        let s = ConnStats::new();
        s.record_accept();
        s.record_accept();
        assert_eq!(s.currently_open(), 2);
        assert_eq!(s.total_accepted(), 2);
        assert_eq!(s.peak_open(), 2);
        s.record_close();
        assert_eq!(s.currently_open(), 1);
    }

    #[test]
    fn peak_tracking() {
        let s = ConnStats::new();
        s.record_accept();
        s.record_accept();
        s.record_accept();
        s.record_close();
        s.record_close();
        assert_eq!(s.peak_open(), 3);
        assert_eq!(s.currently_open(), 1);
    }

    #[test]
    fn error_counting() {
        let s = ConnStats::new();
        s.record_error();
        s.record_error();
        assert_eq!(s.total_errors.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn graceful_shutdown() {
        let s = ConnStats::new();
        assert!(!s.is_graceful_shutdown());
        s.set_graceful_shutdown(true);
        assert!(s.is_graceful_shutdown());
    }

    #[test]
    fn default_stats_zeroed() {
        let s = ConnStats::new();
        assert_eq!(s.currently_open(), 0);
        assert_eq!(s.total_accepted(), 0);
        assert_eq!(s.peak_open(), 0);
    }
}
