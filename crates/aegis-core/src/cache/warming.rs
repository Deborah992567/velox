//! Cache warming hints for proactive content prefetching.
//!
//! Analyzes access patterns to predict which content should be preloaded.

use std::collections::HashMap;
use std::time::Instant;

/// A warming hint for a single cache entry.
#[derive(Debug, Clone)]
pub struct WarmingHint {
    pub path: String,
    pub priority: u8,
    pub estimated_size: u64,
    pub last_accessed: Instant,
    pub access_count: u32,
}

impl WarmingHint {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            priority: 0,
            estimated_size: 0,
            last_accessed: Instant::now(),
            access_count: 0,
        }
    }

    #[must_use]
    pub const fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    #[must_use]
    pub const fn with_size(mut self, size: u64) -> Self {
        self.estimated_size = size;
        self
    }

    /// Score for sorting: higher = warm first.
    #[allow(clippy::cast_precision_loss)]
    pub fn score(&self) -> f64 {
        let recency = 1.0 / (1.0 + self.last_accessed.elapsed().as_secs() as f64);
        let frequency = f64::from(self.access_count);
        let priority_bonus = f64::from(self.priority) * 10.0;
        recency + frequency + priority_bonus
    }
}

/// Tracks access patterns and generates warming hints.
#[derive(Debug)]
pub struct WarmingHintTracker {
    hints: HashMap<String, WarmingHint>,
    max_hints: usize,
    min_access_count: u32,
}

impl WarmingHintTracker {
    pub fn new(max_hints: usize, min_access_count: u32) -> Self {
        Self {
            hints: HashMap::new(),
            max_hints,
            min_access_count,
        }
    }

    pub fn record_access(&mut self, path: &str, size: u64) {
        let hint = self
            .hints
            .entry(path.into())
            .or_insert_with(|| WarmingHint::new(path));
        hint.access_count += 1;
        hint.last_accessed = Instant::now();
        hint.estimated_size = size;
    }

    pub fn get_hints(&self) -> Vec<&WarmingHint> {
        let mut hints: Vec<&WarmingHint> = self
            .hints
            .values()
            .filter(|h| h.access_count >= self.min_access_count)
            .collect();
        hints.sort_by(|a, b| {
            b.score()
                .partial_cmp(&a.score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hints.truncate(self.max_hints);
        hints
    }

    pub fn hint_count(&self) -> usize {
        self.hints.len()
    }

    pub fn clear(&mut self) {
        self.hints.clear();
    }

    pub fn total_estimated_size(&self) -> u64 {
        self.hints.values().map(|h| h.estimated_size).sum()
    }
}

impl Default for WarmingHintTracker {
    fn default() -> Self {
        Self::new(100, 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn record_and_retrieve() {
        let mut tracker = WarmingHintTracker::new(10, 1);
        tracker.record_access("/index.html", 4096);
        tracker.record_access("/style.css", 2048);
        let hints = tracker.get_hints();
        assert_eq!(hints.len(), 2);
    }

    #[test]
    fn min_access_count_filters() {
        let mut tracker = WarmingHintTracker::new(10, 3);
        tracker.record_access("/a", 100);
        tracker.record_access("/a", 100);
        assert_eq!(tracker.get_hints().len(), 0);
        tracker.record_access("/a", 100);
        assert_eq!(tracker.get_hints().len(), 1);
    }

    #[test]
    fn max_hints_limits_output() {
        let mut tracker = WarmingHintTracker::new(3, 1);
        for i in 0..10 {
            tracker.record_access(&format!("/path/{i}"), 100);
        }
        assert_eq!(tracker.get_hints().len(), 3);
    }

    #[test]
    fn score_prefers_recent() {
        let mut h1 = WarmingHint::new("/a");
        h1.access_count = 1;
        let mut h2 = WarmingHint::new("/b");
        h2.access_count = 1;
        h2.last_accessed = Instant::now();
        h1.last_accessed = Instant::now()
            .checked_sub(Duration::from_hours(1))
            .expect("one hour ago is representable");
        assert!(h2.score() > h1.score());
    }

    #[test]
    fn total_estimated_size() {
        let mut tracker = WarmingHintTracker::new(10, 1);
        tracker.record_access("/a", 1000);
        tracker.record_access("/b", 2000);
        assert_eq!(tracker.total_estimated_size(), 3000);
    }

    #[test]
    fn clear_resets() {
        let mut tracker = WarmingHintTracker::new(10, 1);
        tracker.record_access("/a", 100);
        tracker.clear();
        assert_eq!(tracker.hint_count(), 0);
    }

    #[test]
    fn default_config() {
        let tracker = WarmingHintTracker::default();
        assert_eq!(tracker.max_hints, 100);
        assert_eq!(tracker.min_access_count, 2);
    }

    #[test]
    fn builder_chain() {
        let h = WarmingHint::new("/test").with_priority(5).with_size(1024);
        assert_eq!(h.priority, 5);
        assert_eq!(h.estimated_size, 1024);
    }
}
