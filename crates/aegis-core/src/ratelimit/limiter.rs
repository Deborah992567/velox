//! Per-key rate limiter with automatic cleanup.

use std::collections::HashMap;

use super::token_bucket::TokenBucket;

/// Per-key rate limiter.
#[derive(Debug)]
pub struct RateLimiter {
    buckets: HashMap<String, TokenBucket>,
    capacity: f64,
    refill_rate: f64,
}

impl RateLimiter {
    /// Create a new rate limiter. Each key gets `capacity` tokens refilling
    /// at `refill_rate` tokens/second.
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            capacity,
            refill_rate,
        }
    }

    /// Try to acquire a token for the given key.
    pub fn try_acquire(&mut self, key: &str) -> bool {
        self.buckets
            .entry(key.to_string())
            .or_insert_with(|| TokenBucket::new(self.capacity, self.refill_rate))
            .try_acquire()
    }

    /// Number of tracked keys.
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    /// Whether no keys are tracked.
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// Remove all tracked keys.
    pub fn clear(&mut self) {
        self.buckets.clear();
    }

    /// Remove a specific key.
    pub fn remove(&mut self, key: &str) -> bool {
        self.buckets.remove(key).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limits_per_key() {
        let mut limiter = RateLimiter::new(2.0, 1.0);
        assert!(limiter.try_acquire("a"));
        assert!(limiter.try_acquire("a"));
        assert!(!limiter.try_acquire("a"));
        // Different key has its own bucket
        assert!(limiter.try_acquire("b"));
    }

    #[test]
    fn tracks_keys() {
        let mut limiter = RateLimiter::new(5.0, 1.0);
        limiter.try_acquire("x");
        limiter.try_acquire("y");
        assert_eq!(limiter.len(), 2);
        assert!(!limiter.is_empty());
    }

    #[test]
    fn clear_removes_all() {
        let mut limiter = RateLimiter::new(5.0, 1.0);
        limiter.try_acquire("a");
        limiter.clear();
        assert!(limiter.is_empty());
    }

    #[test]
    fn remove_specific_key() {
        let mut limiter = RateLimiter::new(1.0, 1.0);
        limiter.try_acquire("a");
        assert!(limiter.remove("a"));
        assert!(!limiter.remove("a"));
        assert!(limiter.is_empty());
    }
}
