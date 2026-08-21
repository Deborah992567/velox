//! TLS session ticket cache for session resumption.
//!
//! Stores encrypted session tickets to speed up TLS handshakes.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A cached TLS session ticket.
#[derive(Debug, Clone)]
pub struct SessionTicket {
    pub ticket_id: Vec<u8>,
    pub session_data: Vec<u8>,
    pub created_at: Instant,
    pub ttl: Duration,
}

impl SessionTicket {
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= self.ttl
    }

    pub fn remaining(&self) -> Duration {
        self.ttl
            .checked_sub(self.created_at.elapsed())
            .unwrap_or(Duration::ZERO)
    }
}

/// Session ticket cache with TTL-based eviction.
#[derive(Debug)]
pub struct TlsSessionCache {
    tickets: HashMap<Vec<u8>, SessionTicket>,
    max_entries: usize,
    default_ttl: Duration,
}

impl TlsSessionCache {
    pub fn new(max_entries: usize, default_ttl: Duration) -> Self {
        Self {
            tickets: HashMap::new(),
            max_entries,
            default_ttl,
        }
    }

    pub fn insert(&mut self, ticket_id: Vec<u8>, session_data: Vec<u8>) {
        if self.tickets.len() >= self.max_entries {
            self.evict_expired();
        }
        if self.tickets.len() >= self.max_entries {
            self.evict_oldest();
        }
        self.tickets.insert(
            ticket_id,
            SessionTicket {
                ticket_id: Vec::new(),
                session_data,
                created_at: Instant::now(),
                ttl: self.default_ttl,
            },
        );
    }

    pub fn get(&self, ticket_id: &[u8]) -> Option<&[u8]> {
        self.tickets.get(ticket_id).and_then(|t| {
            if t.is_expired() {
                None
            } else {
                Some(t.session_data.as_slice())
            }
        })
    }

    pub fn remove(&mut self, ticket_id: &[u8]) -> bool {
        self.tickets.remove(ticket_id).is_some()
    }

    pub fn len(&self) -> usize {
        self.tickets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty()
    }

    pub fn entry_count(&self) -> usize {
        self.tickets.len()
    }

    fn evict_expired(&mut self) {
        self.tickets.retain(|_, t| !t.is_expired());
    }

    fn evict_oldest(&mut self) {
        if let Some(oldest_key) = self
            .tickets
            .iter()
            .min_by_key(|(_, t)| t.created_at)
            .map(|(k, _)| k.clone())
        {
            self.tickets.remove(&oldest_key);
        }
    }

    pub fn clear(&mut self) {
        self.tickets.clear();
    }
}

impl Default for TlsSessionCache {
    fn default() -> Self {
        Self::new(1024, Duration::from_hours(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let mut cache = TlsSessionCache::new(10, Duration::from_mins(1));
        cache.insert(vec![1, 2, 3], vec![4, 5, 6]);
        assert_eq!(cache.get(&[1, 2, 3]), Some(vec![4, 5, 6].as_slice()));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn get_nonexistent() {
        let cache = TlsSessionCache::new(10, Duration::from_mins(1));
        assert_eq!(cache.get(&[1, 2, 3]), None);
    }

    #[test]
    fn remove_ticket() {
        let mut cache = TlsSessionCache::new(10, Duration::from_mins(1));
        cache.insert(vec![1], vec![2]);
        assert!(cache.remove(&[1]));
        assert!(cache.is_empty());
    }

    #[test]
    fn evicts_when_full() {
        let mut cache = TlsSessionCache::new(2, Duration::from_mins(1));
        cache.insert(vec![1], vec![10]);
        cache.insert(vec![2], vec![20]);
        assert_eq!(cache.len(), 2);
        cache.insert(vec![3], vec![30]);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn default_config() {
        let cache = TlsSessionCache::default();
        assert_eq!(cache.max_entries, 1024);
    }

    #[test]
    fn clear_removes_all() {
        let mut cache = TlsSessionCache::new(10, Duration::from_mins(1));
        cache.insert(vec![1], vec![10]);
        cache.insert(vec![2], vec![20]);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn expired_ticket_not_returned() {
        let mut cache = TlsSessionCache::new(10, Duration::ZERO);
        cache.insert(vec![1], vec![10]);
        assert!(cache.get(&[1]).is_none());
    }

    #[test]
    fn entry_count_matches_len() {
        let mut cache = TlsSessionCache::new(10, Duration::from_mins(1));
        cache.insert(vec![1], vec![10]);
        assert_eq!(cache.entry_count(), cache.len());
    }
}
