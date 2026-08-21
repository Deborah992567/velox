//! Latency histogram with configurable bucket boundaries.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// A histogram for tracking value distributions (e.g., latencies).
#[derive(Debug)]
pub struct Histogram {
    name: String,
    buckets: Vec<Bucket>,
    total_count: AtomicU64,
    total_sum: AtomicU64,
}

#[derive(Debug)]
struct Bucket {
    le: f64,
    count: AtomicU64,
}

impl Histogram {
    /// Create a histogram with the given upper-bound bucket boundaries.
    pub fn new(name: impl Into<String>, boundaries: &[f64]) -> Self {
        let mut buckets = Vec::with_capacity(boundaries.len() + 1);
        for &le in boundaries {
            buckets.push(Bucket {
                le,
                count: AtomicU64::new(0),
            });
        }
        buckets.push(Bucket {
            le: f64::INFINITY,
            count: AtomicU64::new(0),
        });
        Self {
            name: name.into(),
            buckets,
            total_count: AtomicU64::new(0),
            total_sum: AtomicU64::new(0),
        }
    }

    /// Record a value.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn record(&self, value: f64) {
        for bucket in &self.buckets {
            if value <= bucket.le {
                bucket.count.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.total_count.fetch_add(1, Ordering::Relaxed);
        self.total_sum.fetch_add(value as u64, Ordering::Relaxed);
    }

    pub fn count(&self) -> u64 {
        self.total_count.load(Ordering::Relaxed)
    }

    pub fn sum(&self) -> u64 {
        self.total_sum.load(Ordering::Relaxed)
    }

    pub fn bucket_count(&self, le: f64) -> u64 {
        for bucket in &self.buckets {
            if bucket.le >= le {
                return bucket.count.load(Ordering::Relaxed);
            }
        }
        0
    }

    pub fn inf_count(&self) -> u64 {
        self.buckets
            .last()
            .map_or(0, |b| b.count.load(Ordering::Relaxed))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn bucket_count_count(&self) -> usize {
        self.buckets.len()
    }
}

/// A histogram snapshot (for export without locks).
#[derive(Debug, Clone)]
pub struct HistogramSnapshot {
    pub name: String,
    pub count: u64,
    pub sum: u64,
    pub buckets: Vec<(f64, u64)>,
}

impl Histogram {
    pub fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            name: self.name.clone(),
            count: self.count(),
            sum: self.sum(),
            buckets: self
                .buckets
                .iter()
                .map(|b| (b.le, b.count.load(Ordering::Relaxed)))
                .collect(),
        }
    }
}

/// Thread-safe snapshot store.
#[derive(Debug, Default)]
pub struct SnapshotStore {
    snapshots: Mutex<Vec<HistogramSnapshot>>,
}

impl SnapshotStore {
    #[allow(clippy::missing_const_for_fn)]
    pub fn new() -> Self {
        Self {
            snapshots: Mutex::new(Vec::new()),
        }
    }

    pub fn push(&self, snap: HistogramSnapshot) {
        if let Ok(mut vec) = self.snapshots.lock() {
            vec.push(snap);
        }
    }

    pub fn drain(&self) -> Vec<HistogramSnapshot> {
        self.snapshots
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.snapshots.lock().map_or(0, |v| v.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_basic_recording() {
        let h = Histogram::new("latency", &[0.1, 0.5, 1.0, 5.0]);
        h.record(0.05);
        h.record(0.3);
        h.record(2.0);
        assert_eq!(h.count(), 3);
    }

    #[test]
    fn histogram_bucket_counts() {
        let h = Histogram::new("test", &[10.0, 20.0, 30.0]);
        h.record(5.0);
        h.record(15.0);
        h.record(25.0);
        assert_eq!(h.bucket_count(10.0), 1);
        assert_eq!(h.bucket_count(20.0), 2);
        assert_eq!(h.bucket_count(30.0), 3);
        assert_eq!(h.inf_count(), 3);
    }

    #[test]
    fn histogram_sum() {
        let h = Histogram::new("test", &[100.0]);
        h.record(10.0);
        h.record(20.0);
        assert_eq!(h.sum(), 30);
    }

    #[test]
    fn histogram_empty() {
        let h = Histogram::new("empty", &[1.0, 2.0]);
        assert_eq!(h.count(), 0);
        assert_eq!(h.sum(), 0);
        assert_eq!(h.inf_count(), 0);
    }

    #[test]
    fn histogram_snapshot() {
        let h = Histogram::new("snap", &[10.0, 20.0]);
        h.record(5.0);
        let snap = h.snapshot();
        assert_eq!(snap.name, "snap");
        assert_eq!(snap.count, 1);
        assert_eq!(snap.buckets.len(), 3);
    }

    #[test]
    fn snapshot_store_drain() {
        let store = SnapshotStore::new();
        assert!(store.is_empty());
        store.push(HistogramSnapshot {
            name: "x".to_string(),
            count: 1,
            sum: 1,
            buckets: vec![],
        });
        assert_eq!(store.len(), 1);
        let drained = store.drain();
        assert_eq!(drained.len(), 1);
        assert!(store.is_empty());
    }

    #[test]
    fn name_exposed() {
        let h = Histogram::new("my.hist", &[]);
        assert_eq!(h.name(), "my.hist");
        assert_eq!(h.bucket_count_count(), 1);
    }
}
