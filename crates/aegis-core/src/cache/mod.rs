//! In-memory response cache with TTL expiry and LRU eviction.
//!
//! Phase 15 adds a cache layer for proxy and static file responses.

pub mod entry;
pub mod lru;
pub mod store;
