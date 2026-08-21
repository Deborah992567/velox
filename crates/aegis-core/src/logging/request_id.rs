//! Request-ID correlation for structured logging.
//!
//! Each inbound request gets a unique ID that flows through proxy chains,
//! appears in access logs, and can be used to correlate worker logs.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(1);

/// A unique request identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(u64);

impl RequestId {
    /// Generate the next request ID.
    pub fn next() -> Self {
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// The raw numeric value.
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Format as a zero-padded 16-character hex string suitable for HTTP headers.
    pub fn as_hex(self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        let hex = b"0123456789abcdef";
        let v = self.0;
        for i in 0..16 {
            buf[15 - i] = hex[((v >> (i * 4)) & 0xF) as usize];
        }
        buf
    }

    /// Format as a hyphenated `req-<hex>` string (e.g. `req-0000000000000001`).
    pub const fn display(self) -> RequestIdDisplay {
        RequestIdDisplay(self)
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "req-{:016x}", self.0)
    }
}

/// Formatter for `RequestId`.
#[derive(Debug)]
pub struct RequestIdDisplay(RequestId);

impl fmt::Display for RequestIdDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "req-{:016x}", self.0.0)
    }
}

// Thread-local current request ID for log correlation.
thread_local! {
    static CURRENT: std::cell::Cell<Option<RequestId>> = const { std::cell::Cell::new(None) };
}

/// Set the current request ID for this thread.
pub fn set_current(id: RequestId) {
    CURRENT.with(|c| c.set(Some(id)));
}

/// Get the current request ID for this thread, if any.
pub fn current() -> Option<RequestId> {
    CURRENT.with(std::cell::Cell::get)
}

/// Clear the current request ID for this thread.
pub fn clear_current() {
    CURRENT.with(|c| c.set(None));
}

/// RAII guard that sets the request ID on creation and clears it on drop.
#[derive(Debug)]
pub struct RequestIdGuard {
    previous: Option<RequestId>,
}

impl RequestIdGuard {
    /// Set the given request ID as current and return a guard that will
    /// restore the previous ID when dropped.
    pub fn new(id: RequestId) -> Self {
        let previous = current();
        set_current(id);
        Self { previous }
    }
}

impl Drop for RequestIdGuard {
    fn drop(&mut self) {
        match self.previous {
            Some(id) => set_current(id),
            None => clear_current(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_ids() {
        let a = RequestId::next();
        let b = RequestId::next();
        assert_ne!(a, b);
    }

    #[test]
    fn hex_format() {
        let id = RequestId(42);
        let hex = id.as_hex();
        assert_eq!(&hex, b"000000000000002a");
    }

    #[test]
    fn display_format() {
        let id = RequestId(256);
        assert_eq!(id.to_string(), "req-0000000000000100");
    }

    #[test]
    fn thread_local_current() {
        clear_current();
        assert!(current().is_none());
        let id = RequestId::next();
        set_current(id);
        assert_eq!(current(), Some(id));
        clear_current();
        assert!(current().is_none());
    }

    #[test]
    fn guard_restores_previous() {
        let outer = RequestId(100);
        let inner = RequestId(200);
        set_current(outer);
        {
            let _guard = RequestIdGuard::new(inner);
            assert_eq!(current(), Some(inner));
        }
        assert_eq!(current(), Some(outer));
        clear_current();
    }

    #[test]
    fn as_u64() {
        assert_eq!(RequestId(12345).as_u64(), 12345);
    }
}
