//! Atomic counters for metrics.

use std::sync::atomic::{AtomicU64, Ordering};

/// A monotonically increasing counter.
#[derive(Debug)]
pub struct Counter {
    name: String,
    value: AtomicU64,
}

impl Counter {
    /// Create a new counter.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: AtomicU64::new(0),
        }
    }

    /// Increment by 1.
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment by `n`.
    pub fn add(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// Current value.
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    /// The metric name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Reset to zero.
    pub fn reset(&self) {
        self.value.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_starts_at_zero() {
        let c = Counter::new("requests");
        assert_eq!(c.get(), 0);
    }

    #[test]
    fn inc_and_add() {
        let c = Counter::new("bytes");
        c.inc();
        c.inc();
        assert_eq!(c.get(), 2);
        c.add(10);
        assert_eq!(c.get(), 12);
    }

    #[test]
    fn reset() {
        let c = Counter::new("x");
        c.add(42);
        assert_eq!(c.get(), 42);
        c.reset();
        assert_eq!(c.get(), 0);
    }

    #[test]
    fn name_is_exposed() {
        let c = Counter::new("my.metric");
        assert_eq!(c.name(), "my.metric");
    }
}
