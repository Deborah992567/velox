//! Atomic gauges for metrics that go up and down.

use std::sync::atomic::{AtomicI64, Ordering};

/// A metric that can increase or decrease.
#[derive(Debug)]
pub struct Gauge {
    name: String,
    value: AtomicI64,
}

impl Gauge {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: AtomicI64::new(0),
        }
    }

    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec(&self) {
        self.value.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn add(&self, n: i64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    pub fn sub(&self, n: i64) {
        self.value.fetch_sub(n, Ordering::Relaxed);
    }

    pub fn set(&self, n: i64) {
        self.value.store(n, Ordering::Relaxed);
    }

    pub fn get(&self) -> i64 {
        self.value.load(Ordering::Relaxed)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn reset(&self) {
        self.value.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_starts_at_zero() {
        let g = Gauge::new("connections");
        assert_eq!(g.get(), 0);
    }

    #[test]
    fn inc_dec_add_sub() {
        let g = Gauge::new("x");
        g.inc();
        g.inc();
        assert_eq!(g.get(), 2);
        g.dec();
        assert_eq!(g.get(), 1);
        g.add(10);
        assert_eq!(g.get(), 11);
        g.sub(5);
        assert_eq!(g.get(), 6);
    }

    #[test]
    fn set_and_get() {
        let g = Gauge::new("y");
        g.set(42);
        assert_eq!(g.get(), 42);
    }

    #[test]
    fn reset() {
        let g = Gauge::new("z");
        g.set(99);
        g.reset();
        assert_eq!(g.get(), 0);
    }
}
