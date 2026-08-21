//! Token bucket rate limiter.

use std::time::Instant;

/// A token bucket rate limiter.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Create a new token bucket.
    ///
    /// `capacity` is the maximum number of tokens, `refill_rate` is tokens
    /// added per second.
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            capacity,
            tokens: capacity,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume one token. Returns `true` if allowed.
    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Try to consume `n` tokens. Returns `true` if all were available.
    pub fn try_acquire_many(&mut self, n: f64) -> bool {
        self.refill();
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    /// Number of tokens currently available.
    pub const fn available(&self) -> f64 {
        self.tokens
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = elapsed
            .mul_add(self.refill_rate, self.tokens)
            .min(self.capacity);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_capacity() {
        let mut bucket = TokenBucket::new(5.0, 1.0);
        for _ in 0..5 {
            assert!(bucket.try_acquire());
        }
        assert!(!bucket.try_acquire());
    }

    #[test]
    fn refill_over_time() {
        let mut bucket = TokenBucket::new(5.0, 1000.0); // very fast refill
        for _ in 0..5 {
            bucket.try_acquire();
        }
        // With 1000 tokens/sec, after a tiny sleep we should get more
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(bucket.try_acquire());
    }

    #[test]
    fn try_acquire_many() {
        let mut bucket = TokenBucket::new(10.0, 1.0);
        assert!(bucket.try_acquire_many(10.0));
        assert!(!bucket.try_acquire_many(1.0));
    }

    #[test]
    fn available_reports_correctly() {
        let mut bucket = TokenBucket::new(5.0, 1.0);
        assert!((bucket.available() - 5.0).abs() < 0.01);
        bucket.try_acquire();
        assert!((bucket.available() - 4.0).abs() < 0.01);
    }

    #[test]
    fn never_exceeds_capacity() {
        let bucket = TokenBucket::new(3.0, 1.0);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!((bucket.available() - 3.0).abs() < 0.5);
    }
}
