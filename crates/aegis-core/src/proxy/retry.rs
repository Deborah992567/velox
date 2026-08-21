//! Proxy retry logic with exponential backoff.
//!
//! Determines when and how to retry failed upstream requests.

use std::time::Duration;

/// Retry policy configuration.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub backoff_multiplier: f64,
    pub retry_on_status: Vec<u16>,
    pub retry_on_connect_error: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            backoff_multiplier: 2.0,
            retry_on_status: vec![502, 503, 504],
            retry_on_connect_error: true,
        }
    }
}

impl RetryPolicy {
    pub const NONE: Self = Self {
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        backoff_multiplier: 1.0,
        retry_on_status: Vec::new(),
        retry_on_connect_error: false,
    };

    /// Check if a given status code should trigger a retry.
    pub fn should_retry_status(&self, status: u16) -> bool {
        self.retry_on_status.contains(&status)
    }

    /// Check if an attempt number (0-based) is within the retry limit.
    #[must_use]
    pub const fn can_retry(&self, attempt: u32) -> bool {
        attempt < self.max_retries
    }

    /// Compute the backoff duration for a given attempt (0-based).
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn backoff_for(&self, attempt: u32) -> Duration {
        let factor = self.backoff_multiplier.powi(attempt.cast_signed());
        let ms = (self.initial_backoff.as_millis() as f64 * factor) as u64;
        Duration::from_millis(ms).min(self.max_backoff)
    }

    /// Whether the method is safe to retry.
    pub fn is_retryable_method(method: &str) -> bool {
        matches!(method, "GET" | "HEAD" | "OPTIONS" | "PUT" | "DELETE")
    }
}

/// Tracks retry state for a single request.
#[derive(Debug, Clone)]
pub struct RetryState {
    attempt: u32,
    policy: RetryPolicy,
    total_delay: Duration,
}

impl RetryState {
    pub const fn new(policy: RetryPolicy) -> Self {
        Self {
            attempt: 0,
            policy,
            total_delay: Duration::ZERO,
        }
    }

    pub const fn should_retry_connect_error(&self) -> bool {
        self.policy.can_retry(self.attempt) && self.policy.retry_on_connect_error
    }

    pub fn record_failure(&mut self) {
        self.attempt += 1;
        self.total_delay += self.policy.backoff_for(self.attempt.saturating_sub(1));
    }

    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    #[must_use]
    pub const fn total_delay(&self) -> Duration {
        self.total_delay
    }

    pub const fn is_exhausted(&self) -> bool {
        !self.policy.can_retry(self.attempt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy() {
        let p = RetryPolicy::default();
        assert_eq!(p.max_retries, 3);
        assert!(p.should_retry_status(502));
        assert!(p.should_retry_status(503));
        assert!(p.should_retry_status(504));
        assert!(!p.should_retry_status(200));
        assert!(!p.should_retry_status(404));
    }

    #[test]
    fn none_policy() {
        let p = RetryPolicy::NONE;
        assert!(!p.can_retry(0));
        assert_eq!(p.max_retries, 0);
    }

    #[test]
    fn backoff_increases() {
        let p = RetryPolicy::default();
        let b0 = p.backoff_for(0);
        let b1 = p.backoff_for(1);
        let b2 = p.backoff_for(2);
        assert!(b1 > b0);
        assert!(b2 > b1);
    }

    #[test]
    fn backoff_capped_at_max() {
        let p = RetryPolicy {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(3),
            backoff_multiplier: 10.0,
            ..Default::default()
        };
        assert_eq!(p.backoff_for(5), Duration::from_secs(3));
    }

    #[test]
    fn can_retry_within_limit() {
        let p = RetryPolicy {
            max_retries: 2,
            ..Default::default()
        };
        assert!(p.can_retry(0));
        assert!(p.can_retry(1));
        assert!(!p.can_retry(2));
    }

    #[test]
    fn retryable_methods() {
        assert!(RetryPolicy::is_retryable_method("GET"));
        assert!(RetryPolicy::is_retryable_method("HEAD"));
        assert!(RetryPolicy::is_retryable_method("PUT"));
        assert!(RetryPolicy::is_retryable_method("DELETE"));
        assert!(!RetryPolicy::is_retryable_method("POST"));
    }

    #[test]
    fn retry_state_tracks_attempts() {
        let policy = RetryPolicy {
            max_retries: 3,
            ..Default::default()
        };
        let mut state = RetryState::new(policy);
        assert_eq!(state.attempt(), 0);
        assert!(!state.is_exhausted());

        state.record_failure();
        assert_eq!(state.attempt(), 1);
        assert!(state.total_delay() > Duration::ZERO);

        state.record_failure();
        state.record_failure();
        assert!(state.is_exhausted());
    }

    #[test]
    fn retry_state_connect_error() {
        let policy = RetryPolicy {
            retry_on_connect_error: true,
            max_retries: 1,
            ..Default::default()
        };
        let mut state = RetryState::new(policy);
        assert!(state.should_retry_connect_error());
        state.record_failure();
        assert!(!state.should_retry_connect_error());
    }
}
