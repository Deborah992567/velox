//! Hard limits on HTTP/1.x request heads.
//!
//! The limits below come from `docs/architecture.md` §7 and §11: a request
//! line of at most 8 KiB, a header block of at most 64 KiB, and at most 100
//! header fields. Enforcing them *during* incremental parsing — before the
//! terminating blank line has arrived — is what makes the parser resilient to
//! slowloris-style floods and unbounded-header attacks.
//!
//! Values are advisory (tunable per site later); the parser treats any
//! over-limit condition as a hard `400 Bad Request` (or `414`/`431` for the
//! targeted sub-limits).

/// Limits applied while parsing one request head.
#[derive(Debug, Clone, Copy)]
pub struct RequestLimits {
    /// Maximum length of the request line including its trailing CRLF.
    pub max_request_line: usize,
    /// Maximum total size of the request head (request line + headers +
    /// blank terminator), in bytes.
    pub max_head_size: usize,
    /// Maximum number of header fields.
    pub max_headers: usize,
    /// Maximum length of a single header value, in bytes.
    pub max_header_value: usize,
    /// Maximum length of the request-target, in bytes.
    pub max_target_len: usize,
    /// Maximum length of a method token, in bytes.
    pub max_method_len: usize,
}

impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            max_request_line: 8 * 1024,
            max_head_size: 64 * 1024,
            max_headers: 100,
            max_header_value: 8 * 1024,
            max_target_len: 8 * 1024,
            max_method_len: 64,
        }
    }
}

impl RequestLimits {
    /// A deliberately tight limit set for tests.
    pub const fn small() -> Self {
        Self {
            max_request_line: 64,
            max_head_size: 256,
            max_headers: 8,
            max_header_value: 64,
            max_target_len: 64,
            max_method_len: 16,
        }
    }
}

/// Why a request head exceeded a [`RequestLimits`] bound.
///
/// The parser maps these to status codes: line too long → 414, header
/// block/fields too large → 431, body too large → 413, everything else → 400.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitViolation {
    /// The request line exceeded `max_request_line`.
    RequestLineTooLong,
    /// The accumulated head exceeded `max_head_size`.
    HeadTooLarge,
    /// More than `max_headers` header fields.
    TooManyHeaders,
    /// A single header value exceeded `max_header_value`.
    HeaderValueTooLong,
    /// The request-target exceeded `max_target_len`.
    TargetTooLong,
    /// The method token exceeded `max_method_len`.
    MethodTooLong,
    /// The request body exceeded the configured maximum.
    BodyTooLarge,
}

#[cfg(test)]
mod tests {
    use super::{LimitViolation, RequestLimits};

    #[test]
    fn defaults_match_architecture_document() {
        let limits = RequestLimits::default();
        assert_eq!(limits.max_request_line, 8 * 1024);
        assert_eq!(limits.max_head_size, 64 * 1024);
        assert_eq!(limits.max_headers, 100);
        assert_eq!(limits.max_target_len, 8 * 1024);
    }

    #[test]
    fn small_limits_are_useful_for_tests() {
        let limits = RequestLimits::small();
        assert!(limits.max_head_size < RequestLimits::default().max_head_size);
        assert_eq!(limits.max_headers, 8);
        assert_eq!(limits.max_method_len, 16);
    }

    #[test]
    fn violations_are_stable_values() {
        let cases = [
            LimitViolation::RequestLineTooLong,
            LimitViolation::HeadTooLarge,
            LimitViolation::TooManyHeaders,
            LimitViolation::HeaderValueTooLong,
            LimitViolation::TargetTooLong,
            LimitViolation::MethodTooLong,
            LimitViolation::BodyTooLarge,
        ];
        let mut distinct = std::collections::HashSet::new();
        for violation in cases {
            assert!(distinct.insert(violation));
        }
        assert_eq!(distinct.len(), cases.len());
    }
}
