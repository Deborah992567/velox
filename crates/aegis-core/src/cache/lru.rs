//! LRU (Least Recently Used) eviction for the cache.
//!
//! Provides an LRU tracking structure that records access order for cache
//! entries keyed by `usize` IDs.

use std::collections::HashMap;

/// LRU eviction tracker. O(1) access and eviction.
#[derive(Debug)]
pub struct LruTracker {
    order: Vec<usize>,
    positions: HashMap<usize, usize>,
}

impl LruTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self {
            order: Vec::new(),
            positions: HashMap::new(),
        }
    }

    /// Number of tracked entries.
    pub const fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether the tracker is empty.
    pub const fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Mark an entry as most recently used.
    pub fn touch(&mut self, id: usize) {
        if let Some(&pos) = self.positions.get(&id) {
            self.order.remove(pos);
            for p in &mut self.positions.values_mut() {
                if *p > pos {
                    *p -= 1;
                }
            }
        }
        self.positions.insert(id, self.order.len());
        self.order.push(id);
    }

    /// Evict the least recently used entry. Returns its ID.
    pub fn evict(&mut self) -> Option<usize> {
        let id = self.order.first().copied()?;
        self.order.remove(0);
        self.positions.remove(&id);
        for p in self.positions.values_mut() {
            *p -= 1;
        }
        Some(id)
    }

    /// Remove a specific entry.
    pub fn remove(&mut self, id: usize) -> bool {
        if let Some(pos) = self.positions.remove(&id) {
            self.order.remove(pos);
            for p in self.positions.values_mut() {
                if *p > pos {
                    *p -= 1;
                }
            }
            true
        } else {
            false
        }
    }

    /// Check if an entry is tracked.
    pub fn contains(&self, id: usize) -> bool {
        self.positions.contains_key(&id)
    }
}

impl Default for LruTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_and_evict() {
        let mut lru = LruTracker::new();
        lru.touch(1);
        lru.touch(2);
        lru.touch(3);
        assert_eq!(lru.len(), 3);
        assert_eq!(lru.evict(), Some(1));
        assert_eq!(lru.len(), 2);
    }

    #[test]
    fn touch_moves_to_end() {
        let mut lru = LruTracker::new();
        lru.touch(1);
        lru.touch(2);
        lru.touch(3);
        lru.touch(1); // move 1 to most recent
        assert_eq!(lru.evict(), Some(2));
        assert_eq!(lru.evict(), Some(3));
        assert_eq!(lru.evict(), Some(1));
    }

    #[test]
    fn remove_entry() {
        let mut lru = LruTracker::new();
        lru.touch(1);
        lru.touch(2);
        assert!(lru.remove(1));
        assert!(!lru.contains(1));
        assert!(lru.contains(2));
        assert_eq!(lru.len(), 1);
    }

    #[test]
    fn evict_empty_returns_none() {
        let mut lru = LruTracker::new();
        assert_eq!(lru.evict(), None);
    }

    #[test]
    fn default_is_empty() {
        let lru = LruTracker::default();
        assert!(lru.is_empty());
        assert_eq!(lru.len(), 0);
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let mut lru = LruTracker::new();
        lru.touch(1);
        assert!(!lru.remove(99));
    }

    #[test]
    fn touch_existing_does_not_duplicate() {
        let mut lru = LruTracker::new();
        lru.touch(1);
        lru.touch(1);
        lru.touch(1);
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.evict(), Some(1));
    }
}
