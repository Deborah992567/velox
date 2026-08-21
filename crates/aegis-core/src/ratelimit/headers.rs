//! Rate limit response headers (RFC 6585 / draft-ietf-httpapi-ratelimit-headers).
//!
//! Generates `RateLimit`, `RateLimit-Policy`, and `Retry-After` headers.

use std::fmt::Write;

/// Rate limit state for a single key.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitState {
    /// Maximum allowed requests in the window.
    pub limit: u32,
    /// Remaining requests in the current window.
    pub remaining: u32,
    /// Seconds until the window resets.
    pub reset_secs: u32,
}

impl RateLimitState {
    pub const fn new(limit: u32, remaining: u32, reset_secs: u32) -> Self {
        Self {
            limit,
            remaining,
            reset_secs,
        }
    }

    /// Render the `RateLimit` header value (RFC draft format).
    /// Example: `limit=100, remaining=95, reset=60`
    pub fn header_value(&self) -> String {
        format!(
            "limit={}, remaining={}, reset={}",
            self.limit, self.remaining, self.reset_secs
        )
    }

    /// Render the `RateLimit-Policy` header value.
    /// Example: `100;w=60`
    pub fn policy_header(&self) -> String {
        format!("{};w={}", self.limit, self.reset_secs)
    }

    /// Render the `Retry-After` header value in seconds.
    #[must_use]
    pub const fn retry_after_secs(&self) -> u32 {
        self.reset_secs
    }

    /// Whether the client has exhausted their quota.
    #[must_use]
    pub const fn is_exhausted(&self) -> bool {
        self.remaining == 0
    }
}

/// Generate standard rate limit response headers.
pub fn rate_limit_headers(state: &RateLimitState) -> Vec<(&'static str, String)> {
    vec![
        ("ratelimit-limit", state.limit.to_string()),
        ("ratelimit-remaining", state.remaining.to_string()),
        ("ratelimit-reset", state.reset_secs.to_string()),
        (
            "ratelimit-policy",
            format!("{};w={}", state.limit, state.reset_secs),
        ),
    ]
}

/// Generate a 429 Too Many Requests response headers.
pub fn too_many_request_headers(state: &RateLimitState) -> Vec<(&'static str, String)> {
    let mut headers = rate_limit_headers(state);
    headers.push(("retry-after", state.reset_secs.to_string()));
    headers
}

/// Format rate limit headers for logging.
pub fn format_rate_limit_log(state: &RateLimitState) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "limit={}/{} reset={}s",
        state.remaining, state.limit, state.reset_secs
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_state_header() {
        let state = RateLimitState::new(100, 95, 60);
        assert_eq!(state.header_value(), "limit=100, remaining=95, reset=60");
    }

    #[test]
    fn policy_header() {
        let state = RateLimitState::new(50, 48, 30);
        assert_eq!(state.policy_header(), "50;w=30");
    }

    #[test]
    fn retry_after() {
        let state = RateLimitState::new(100, 0, 120);
        assert_eq!(state.retry_after_secs(), 120);
    }

    #[test]
    fn exhausted() {
        let state = RateLimitState::new(10, 0, 60);
        assert!(state.is_exhausted());
        let ok = RateLimitState::new(10, 1, 60);
        assert!(!ok.is_exhausted());
    }

    #[test]
    fn rate_limit_headers_count() {
        let state = RateLimitState::new(100, 90, 30);
        let headers = rate_limit_headers(&state);
        assert_eq!(headers.len(), 4);
    }

    #[test]
    fn too_many_request_includes_retry_after() {
        let state = RateLimitState::new(10, 0, 30);
        let headers = too_many_request_headers(&state);
        assert_eq!(headers.len(), 5);
        assert!(headers.iter().any(|(k, _)| *k == "retry-after"));
    }

    #[test]
    fn format_log() {
        let state = RateLimitState::new(100, 50, 60);
        let log = format_rate_limit_log(&state);
        assert_eq!(log, "limit=50/100 reset=60s");
    }
}
