//! HTTP request smuggling detection.
//!
//! Phase 23: Detects CL.TE and TE.CL desync attacks.

use crate::http::{HeaderName, Headers};

/// Result of a smuggling check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmugglingCheck {
    /// No issues detected.
    Clean,
    /// Conflicting Content-Length headers.
    ConflictingContentLength,
    /// Content-Length and Transfer-Encoding both present.
    ContentLengthAndTransferEncoding,
    /// Multiple Transfer-Encoding headers.
    MultipleTransferEncoding,
    /// CL value exceeds reasonable limit.
    ContentLengthTooLarge(u64),
    /// Body doesn't match declared length.
    ContentLengthMismatch { declared: u64, actual: usize },
}

impl SmugglingCheck {
    pub const fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }

    pub const fn description(&self) -> &'static str {
        match self {
            Self::Clean => "no issues",
            Self::ConflictingContentLength => "conflicting Content-Length values",
            Self::ContentLengthAndTransferEncoding => "both CL and TE present",
            Self::MultipleTransferEncoding => "multiple Transfer-Encoding headers",
            Self::ContentLengthTooLarge(_) => "Content-Length exceeds limit",
            Self::ContentLengthMismatch { .. } => "body length doesn't match Content-Length",
        }
    }
}

/// Maximum allowed Content-Length (10 MiB default).
pub const DEFAULT_MAX_CL: u64 = 10 * 1024 * 1024;

/// Check headers for smuggling indicators.
pub fn check_headers(headers: &Headers) -> SmugglingCheck {
    check_headers_with_limit(headers, DEFAULT_MAX_CL)
}

/// Check headers with a configurable max Content-Length.
pub fn check_headers_with_limit(headers: &Headers, max_cl: u64) -> SmugglingCheck {
    let has_te = headers.contains(&HeaderName::TransferEncoding);
    let has_cl = headers.contains(&HeaderName::ContentLength);

    if has_te && has_cl {
        return SmugglingCheck::ContentLengthAndTransferEncoding;
    }

    let cl_count = headers.get_all(&HeaderName::ContentLength).count();
    if cl_count > 1 {
        return SmugglingCheck::ConflictingContentLength;
    }

    let te_count = headers.get_all(&HeaderName::TransferEncoding).count();
    if te_count > 1 {
        return SmugglingCheck::MultipleTransferEncoding;
    }

    if let Some(cl_bytes) = headers.get(&HeaderName::ContentLength)
        && let Ok(s) = std::str::from_utf8(cl_bytes)
        && let Ok(val) = s.trim().parse::<u64>()
        && val > max_cl
    {
        return SmugglingCheck::ContentLengthTooLarge(val);
    }

    SmugglingCheck::Clean
}

/// Verify actual body length matches declared Content-Length.
#[allow(clippy::cast_possible_truncation)]
pub fn verify_body_length(headers: &Headers, body_len: usize) -> SmugglingCheck {
    if let Some(cl) = headers.content_length()
        && cl as usize != body_len
    {
        return SmugglingCheck::ContentLengthMismatch {
            declared: cl,
            actual: body_len,
        };
    }
    SmugglingCheck::Clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_headers(pairs: &[(&str, &str)]) -> Headers {
        let mut h = Headers::new();
        for (name, value) in pairs {
            match *name {
                "content-length" => h.push_value(HeaderName::ContentLength, *value),
                "transfer-encoding" => h.push_value(HeaderName::TransferEncoding, *value),
                _ => h.push_value(HeaderName::Custom((*name).into()), *value),
            }
        }
        h
    }

    #[test]
    fn clean_headers() {
        let h = make_headers(&[("content-length", "100")]);
        assert_eq!(check_headers(&h), SmugglingCheck::Clean);
    }

    #[test]
    fn conflicting_content_length() {
        let h = make_headers(&[("content-length", "100"), ("content-length", "200")]);
        assert_eq!(check_headers(&h), SmugglingCheck::ConflictingContentLength);
    }

    #[test]
    fn cl_and_te_present() {
        let h = make_headers(&[("content-length", "100"), ("transfer-encoding", "chunked")]);
        assert_eq!(
            check_headers(&h),
            SmugglingCheck::ContentLengthAndTransferEncoding
        );
    }

    #[test]
    fn multiple_transfer_encoding() {
        let h = make_headers(&[
            ("transfer-encoding", "chunked"),
            ("transfer-encoding", "gzip"),
        ]);
        assert_eq!(check_headers(&h), SmugglingCheck::MultipleTransferEncoding);
    }

    #[test]
    fn content_length_too_large() {
        let h = make_headers(&[("content-length", "99999999999")]);
        assert_eq!(
            check_headers(&h),
            SmugglingCheck::ContentLengthTooLarge(99_999_999_999)
        );
    }

    #[test]
    fn body_length_mismatch() {
        let h = make_headers(&[("content-length", "100")]);
        assert_eq!(
            verify_body_length(&h, 50),
            SmugglingCheck::ContentLengthMismatch {
                declared: 100,
                actual: 50,
            }
        );
    }

    #[test]
    fn body_length_match() {
        let h = make_headers(&[("content-length", "100")]);
        assert_eq!(verify_body_length(&h, 100), SmugglingCheck::Clean);
    }

    #[test]
    fn check_with_custom_limit() {
        let h = make_headers(&[("content-length", "200")]);
        assert_eq!(
            check_headers_with_limit(&h, 100),
            SmugglingCheck::ContentLengthTooLarge(200)
        );
    }

    #[test]
    fn is_clean_helpers() {
        assert!(SmugglingCheck::Clean.is_clean());
        assert!(!SmugglingCheck::ConflictingContentLength.is_clean());
    }

    #[test]
    fn description_messages() {
        assert_eq!(SmugglingCheck::Clean.description(), "no issues");
        assert_eq!(
            SmugglingCheck::ContentLengthAndTransferEncoding.description(),
            "both CL and TE present"
        );
    }
}
