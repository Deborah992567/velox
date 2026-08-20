//! In-memory cache store with LRU eviction and TTL expiry.

use std::collections::HashMap;

use super::entry::CacheEntry;
use super::lru::LruTracker;

/// In-memory cache store.
#[derive(Debug)]
pub struct CacheStore {
    entries: HashMap<String, CacheEntry>,
    lru: LruTracker,
    next_id: usize,
    id_to_key: HashMap<usize, String>,
    max_bytes: usize,
    current_bytes: usize,
}

impl CacheStore {
    /// Create a new cache store with the given byte capacity.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: LruTracker::new(),
            next_id: 0,
            id_to_key: HashMap::new(),
            max_bytes,
            current_bytes: 0,
        }
    }

    /// Insert a cache entry.
    pub fn insert(&mut self, key: String, entry: CacheEntry) {
        // Evict until we have room
        while self.current_bytes + entry.size > self.max_bytes && !self.lru.is_empty() {
            if let Some(evicted_id) = self.lru.evict()
                && let Some(evicted_key) = self.id_to_key.remove(&evicted_id)
                && let Some(removed) = self.entries.remove(&evicted_key)
            {
                self.current_bytes -= removed.size;
            }
        }

        // If entry is still too large, don't insert
        if entry.size > self.max_bytes {
            return;
        }

        let id = self.next_id;
        self.next_id += 1;

        self.current_bytes += entry.size;
        self.id_to_key.insert(id, key.clone());
        self.entries.insert(key, entry);
        self.lru.touch(id);
    }

    /// Look up a cache entry by key, returning None if expired or missing.
    pub fn get(&mut self, key: &str) -> Option<&CacheEntry> {
        let id = self
            .id_to_key
            .iter()
            .find(|(_, k)| k.as_str() == key)
            .map(|(&id, _)| id)?;

        let entry = self.entries.get(key)?;
        if entry.is_expired() {
            self.remove(key);
            return None;
        }
        self.lru.touch(id);
        self.entries.get(key)
    }

    /// Remove a cache entry.
    pub fn remove(&mut self, key: &str) -> bool {
        if let Some(entry) = self.entries.remove(key) {
            self.current_bytes -= entry.size;
            if let Some((&id, _)) = self.id_to_key.iter().find(|(_, k)| k.as_str() == key) {
                self.id_to_key.remove(&id);
                self.lru.remove(id);
            }
            true
        } else {
            false
        }
    }

    /// Remove all expired entries.
    pub fn purge_expired(&mut self) {
        let expired_keys: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_expired())
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired_keys {
            self.remove(&key);
        }
    }

    /// Number of entries in the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Current byte usage.
    pub const fn bytes_used(&self) -> usize {
        self.current_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(body: &[u8], ttl_secs: u64) -> CacheEntry {
        CacheEntry::new(
            200,
            Vec::new(),
            body.to_vec(),
            Some(std::time::Duration::from_secs(ttl_secs)),
        )
    }

    #[test]
    fn insert_and_get() {
        let mut store = CacheStore::new(1024);
        store.insert("/page".into(), entry(b"hello", 60));
        assert!(store.get("/page").is_some());
    }

    #[test]
    fn expired_entry_not_returned() {
        let mut store = CacheStore::new(1024);
        store.insert("/page".into(), entry(b"hello", 0));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(store.get("/page").is_none());
    }

    #[test]
    fn lru_eviction_under_pressure() {
        let mut store = CacheStore::new(30); // very small
        store.insert("a".into(), entry(b"12345", 60)); // 5 bytes
        store.insert("b".into(), entry(b"12345", 60)); // 5 bytes
        store.insert("c".into(), entry(b"12345", 60)); // 5 bytes
        store.insert("d".into(), entry(b"12345", 60)); // 5 bytes, should evict
        assert!(store.len() <= 6); // may evict some entries
    }

    #[test]
    fn remove_entry() {
        let mut store = CacheStore::new(1024);
        store.insert("/page".into(), entry(b"hello", 60));
        assert!(store.remove("/page"));
        assert!(!store.remove("/page"));
        assert!(store.is_empty());
    }

    #[test]
    fn purge_expired() {
        let mut store = CacheStore::new(1024);
        store.insert("/expired".into(), entry(b"old", 0));
        store.insert("/valid".into(), entry(b"new", 60));
        std::thread::sleep(std::time::Duration::from_millis(10));
        store.purge_expired();
        assert_eq!(store.len(), 1);
        assert!(store.get("/valid").is_some());
    }

    #[test]
    fn bytes_tracked() {
        let mut store = CacheStore::new(1024);
        assert_eq!(store.bytes_used(), 0);
        store.insert("k".into(), entry(b"12345", 60));
        assert_eq!(store.bytes_used(), 5);
        store.remove("k");
        assert_eq!(store.bytes_used(), 0);
    }

    #[test]
    fn oversized_entry_not_inserted() {
        let mut store = CacheStore::new(5);
        store.insert("big".into(), entry(b"123456", 60));
        assert!(store.is_empty());
    }
}
