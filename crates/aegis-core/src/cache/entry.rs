//! Cache entry types.

use std::time::Instant;

/// A cached HTTP response.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// HTTP status code.
    pub status: u16,
    /// Response headers as raw bytes (name, value).
    pub headers: Vec<(Vec<u8>, Vec<u8>)>,
    /// Response body.
    pub body: Vec<u8>,
    /// When this entry was stored.
    pub stored_at: Instant,
    /// Time-to-live. `None` means the entry never expires.
    pub ttl: Option<std::time::Duration>,
    /// Size of the entry in bytes (header + body).
    pub size: usize,
}

impl CacheEntry {
    /// Create a new cache entry.
    pub fn new(
        status: u16,
        headers: Vec<(Vec<u8>, Vec<u8>)>,
        body: Vec<u8>,
        ttl: Option<std::time::Duration>,
    ) -> Self {
        let size = body.len()
            + headers
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>();
        Self {
            status,
            headers,
            body,
            stored_at: Instant::now(),
            ttl,
            size,
        }
    }

    /// Returns `true` if this entry has expired.
    pub fn is_expired(&self) -> bool {
        self.ttl.is_some_and(|ttl| self.stored_at.elapsed() > ttl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_entry_is_not_expired() {
        let entry = CacheEntry::new(
            200,
            Vec::new(),
            b"hello".to_vec(),
            Some(std::time::Duration::from_mins(1)),
        );
        assert!(!entry.is_expired());
    }

    #[test]
    fn entry_with_zero_ttl_is_immediately_expired() {
        let entry = CacheEntry::new(
            200,
            Vec::new(),
            b"hello".to_vec(),
            Some(std::time::Duration::ZERO),
        );
        // TTL of zero should be expired on the next check
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(entry.is_expired());
    }

    #[test]
    fn size_accounts_for_body_and_headers() {
        let headers = vec![
            (b"Content-Type".to_vec(), b"text/plain".to_vec()),
            (b"Cache-Control".to_vec(), b"max-age=60".to_vec()),
        ];
        let entry = CacheEntry::new(200, headers, b"hello".to_vec(), None);
        assert_eq!(entry.size, 50);
    }

    #[test]
    fn stored_at_is_recent() {
        let before = Instant::now();
        let entry = CacheEntry::new(200, Vec::new(), b"".to_vec(), None);
        let after = Instant::now();
        assert!(entry.stored_at >= before);
        assert!(entry.stored_at <= after);
    }
}
