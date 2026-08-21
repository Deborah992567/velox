//! Request body size limiter.
//!
//! Enforces maximum body sizes per-route or per-server to prevent abuse.

use std::fmt;

/// Maximum body size configuration.
#[derive(Debug, Clone, Copy)]
pub struct BodyLimit {
    max_bytes: u64,
}

impl BodyLimit {
    pub const fn new(max_bytes: u64) -> Self {
        Self { max_bytes }
    }

    pub const ZERO: Self = Self { max_bytes: 0 };
    pub const ONE_KB: Self = Self { max_bytes: 1024 };
    pub const ONE_MB: Self = Self {
        max_bytes: 1024 * 1024,
    };
    pub const TEN_MB: Self = Self {
        max_bytes: 10 * 1024 * 1024,
    };
    pub const HUNDRED_MB: Self = Self {
        max_bytes: 100 * 1024 * 1024,
    };
    pub const ONE_GB: Self = Self {
        max_bytes: 1024 * 1024 * 1024,
    };

    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Check if `current + additional` exceeds the limit.
    pub const fn check(&self, current: u64, additional: u64) -> Result<(), BodyLimitExceeded> {
        let total = current.saturating_add(additional);
        if total > self.max_bytes {
            Err(BodyLimitExceeded {
                limit: self.max_bytes,
                attempted: total,
            })
        } else {
            Ok(())
        }
    }

    pub const fn is_exceeded(&self, current: u64) -> bool {
        current > self.max_bytes
    }

    pub const fn is_exceeded_incl(&self, current: u64, additional: u64) -> bool {
        current.saturating_add(additional) > self.max_bytes
    }

    /// Remaining bytes allowed.
    pub const fn remaining(&self, current: u64) -> u64 {
        self.max_bytes.saturating_sub(current)
    }

    /// How much of the limit has been consumed (0.0 .. 1.0).
    #[allow(clippy::cast_precision_loss)]
    pub fn utilization(&self, current: u64) -> f64 {
        if self.max_bytes == 0 {
            1.0
        } else {
            (current as f64) / (self.max_bytes as f64)
        }
    }
}

impl Default for BodyLimit {
    fn default() -> Self {
        Self::TEN_MB
    }
}

impl fmt::Display for BodyLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.max_bytes;
        if bytes.is_multiple_of(1024 * 1024 * 1024) {
            write!(f, "{}GB", bytes / (1024 * 1024 * 1024))
        } else if bytes.is_multiple_of(1024 * 1024) {
            write!(f, "{}MB", bytes / (1024 * 1024))
        } else if bytes.is_multiple_of(1024) {
            write!(f, "{}KB", bytes / 1024)
        } else {
            write!(f, "{bytes}B")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyLimitExceeded {
    pub limit: u64,
    pub attempted: u64,
}

impl fmt::Display for BodyLimitExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "body size limit of {} bytes exceeded (attempted: {})",
            self.limit, self.attempted
        )
    }
}

impl std::error::Error for BodyLimitExceeded {}

/// Tracks body consumption for a single request.
#[derive(Debug, Clone)]
pub struct BodyTracker {
    limit: BodyLimit,
    received: u64,
    complete: bool,
}

impl BodyTracker {
    pub const fn new(limit: BodyLimit) -> Self {
        Self {
            limit,
            received: 0,
            complete: false,
        }
    }

    /// Record `n` bytes received. Returns `Err` if limit exceeded.
    pub fn record(&mut self, n: u64) -> Result<(), BodyLimitExceeded> {
        self.limit.check(self.received, n)?;
        self.received = self.received.saturating_add(n);
        Ok(())
    }

    pub const fn received(&self) -> u64 {
        self.received
    }

    pub const fn remaining(&self) -> u64 {
        self.limit.remaining(self.received)
    }

    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    pub const fn mark_complete(&mut self) {
        self.complete = true;
    }

    pub const fn is_exceeded(&self) -> bool {
        self.limit.is_exceeded(self.received)
    }

    pub fn utilization(&self) -> f64 {
        self.limit.utilization(self.received)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_within_limit() {
        let limit = BodyLimit::new(100);
        assert!(limit.check(50, 49).is_ok());
    }

    #[test]
    fn check_at_limit() {
        let limit = BodyLimit::new(100);
        assert!(limit.check(50, 50).is_ok());
    }

    #[test]
    fn check_exceeds_limit() {
        let limit = BodyLimit::new(100);
        let err = limit.check(50, 51).unwrap_err();
        assert_eq!(err.limit, 100);
        assert_eq!(err.attempted, 101);
    }

    #[test]
    fn constants_correct() {
        assert_eq!(BodyLimit::ONE_KB.max_bytes(), 1024);
        assert_eq!(BodyLimit::ONE_MB.max_bytes(), 1024 * 1024);
        assert_eq!(BodyLimit::TEN_MB.max_bytes(), 10 * 1024 * 1024);
    }

    #[test]
    fn default_is_10mb() {
        assert_eq!(BodyLimit::default().max_bytes(), 10 * 1024 * 1024);
    }

    #[test]
    fn display_formats_human_readable() {
        assert_eq!(BodyLimit::ONE_KB.to_string(), "1KB");
        assert_eq!(BodyLimit::ONE_MB.to_string(), "1MB");
        assert_eq!(BodyLimit::ONE_GB.to_string(), "1GB");
        assert_eq!(BodyLimit::new(512).to_string(), "512B");
    }

    #[test]
    fn remaining_and_utilization() {
        let limit = BodyLimit::new(100);
        assert_eq!(limit.remaining(30), 70);
        assert_eq!(limit.remaining(100), 0);
        assert!((limit.utilization(50) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn zero_limit_always_exceeds() {
        let limit = BodyLimit::ZERO;
        assert!(limit.check(0, 1).is_err());
        assert!(limit.is_exceeded(1));
        assert!((limit.utilization(0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn tracker_records_and_detects() {
        let mut tracker = BodyTracker::new(BodyLimit::new(10));
        assert!(!tracker.is_exceeded());
        assert_eq!(tracker.received(), 0);

        tracker.record(5).unwrap();
        assert_eq!(tracker.received(), 5);
        assert_eq!(tracker.remaining(), 5);

        tracker.record(5).unwrap();
        assert_eq!(tracker.received(), 10);
    }

    #[test]
    fn tracker_over_limit_errors() {
        let mut tracker = BodyTracker::new(BodyLimit::new(5));
        tracker.record(3).unwrap();
        assert!(tracker.record(3).is_err());
        assert_eq!(tracker.received(), 3);
    }

    #[test]
    fn tracker_complete_flag() {
        let mut tracker = BodyTracker::new(BodyLimit::ONE_MB);
        assert!(!tracker.is_complete());
        tracker.mark_complete();
        assert!(tracker.is_complete());
    }

    #[test]
    fn utilization_zero_limit() {
        let mut tracker = BodyTracker::new(BodyLimit::ZERO);
        assert!(tracker.utilization() >= 0.99);
        tracker.record(0).unwrap();
        assert!(tracker.utilization() >= 0.99);
    }
}
