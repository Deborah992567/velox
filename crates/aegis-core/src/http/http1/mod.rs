//! HTTP/1.x wire protocol: parser, chunked coding, and response engine.
//!
//! Everything here is *incremental*: the parser and the chunked decoder
//! consume exactly the bytes available and report how far they got, so the
//! connection manager can feed socket reads into them without buffering
//! entire messages. Strictness is a security feature — see the architecture
//! security model (§11) — so malformed input is rejected rather than
//! repaired, and every framing decision is pinned to a single
//! `Content-Length` or to `chunked`.
//!
//! Sub-modules:
//! - [`parser`]: incremental request-head parser with smuggling defenses.
//! - [`chunked`]: incremental chunked transfer decoder.
//! - [`engine`]: injection-safe response encoder.

pub mod chunked;
pub mod engine;
pub mod parser;

use crate::http::{Header, HeaderName, is_tchar};

/// Why a single header field line was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFieldError {
    /// The line has no colon.
    MissingColon,
    /// The field name is empty.
    EmptyName,
    /// The field name contains a non-token character (this also catches
    /// obs-fold continuation lines, which begin with whitespace).
    InvalidName,
    /// The value contains a control byte other than HTAB.
    InvalidValue,
}

/// Parse one header field line (`name : value`, without the trailing CRLF).
///
/// Surrounding whitespace is stripped from the value, the name is normalized
/// to lowercase, and both name and value are strictly validated. Used by the
/// head parser and by the chunked decoder's trailer handling.
pub(crate) fn parse_header_field(line: &[u8]) -> Result<Header, HeaderFieldError> {
    let Some((name_bytes, rest)) = split_once_colon(line) else {
        return Err(HeaderFieldError::MissingColon);
    };
    if name_bytes.is_empty() {
        return Err(HeaderFieldError::EmptyName);
    }
    if !name_bytes.iter().all(|&b| is_tchar(b)) {
        return Err(HeaderFieldError::InvalidName);
    }
    let name = HeaderName::parse(name_bytes).ok_or(HeaderFieldError::InvalidName)?;
    let value = trim_ows(rest);
    validate_field_value(value)?;
    Ok(Header::new(name, value.to_vec()))
}

/// Strip leading and trailing optional whitespace (SP/HTAB) from a value.
pub(crate) fn trim_ows(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && (bytes[start] == b' ' || bytes[start] == b'\t') {
        start += 1;
    }
    while end > start && (bytes[end - 1] == b' ' || bytes[end - 1] == b'\t') {
        end -= 1;
    }
    &bytes[start..end]
}

/// Reject control bytes other than HTAB in a field value (RFC 9110
/// `field-content`).
pub(crate) fn validate_field_value(value: &[u8]) -> Result<(), HeaderFieldError> {
    if value.iter().any(|&b| (b < 0x20 && b != b'\t') || b == 0x7f) {
        return Err(HeaderFieldError::InvalidValue);
    }
    Ok(())
}

/// Split a line on its first colon.
fn split_once_colon(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let i = line.iter().position(|&b| b == b':')?;
    Some((&line[..i], &line[i + 1..]))
}

/// Decode one hex digit, `None` if `b` is not `0-9`, `a-f`, or `A-F`.
pub(crate) const fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{HeaderFieldError, parse_header_field, trim_ows, validate_field_value};
    use crate::http::HeaderName;

    #[test]
    fn parses_simple_fields() {
        let header = parse_header_field(b"Content-Type: text/plain").unwrap();
        assert_eq!(header.name, HeaderName::ContentType);
        assert_eq!(header.value, b"text/plain");
    }

    #[test]
    fn strips_surrounding_ows() {
        let header = parse_header_field(b"X-Custom:   spaced out  \t").unwrap();
        assert_eq!(header.name.as_str(), "x-custom");
        assert_eq!(header.value, b"spaced out");
    }

    #[test]
    fn rejects_malformed_fields() {
        assert_eq!(
            parse_header_field(b"no-colon"),
            Err(HeaderFieldError::MissingColon)
        );
        assert_eq!(
            parse_header_field(b": value"),
            Err(HeaderFieldError::EmptyName)
        );
        assert_eq!(
            parse_header_field(b"Bad Name: x"),
            Err(HeaderFieldError::InvalidName)
        );
        assert_eq!(
            parse_header_field(b" obs-fold: x"),
            Err(HeaderFieldError::InvalidName)
        );
        assert_eq!(
            parse_header_field(b"X-Tab: bad\x01ctrl"),
            Err(HeaderFieldError::InvalidValue)
        );
    }

    #[test]
    fn allows_tab_but_not_other_controls_in_values() {
        assert!(validate_field_value(b"a\tb").is_ok());
        assert!(validate_field_value(b"").is_ok());
        assert!(validate_field_value(b"obs-\xfftext").is_ok());
        assert!(validate_field_value(b"a\x00b").is_err());
        assert!(validate_field_value(b"a\x7fb").is_err());
    }

    #[test]
    fn trims_ows_correctly() {
        assert_eq!(trim_ows(b""), b"");
        assert_eq!(trim_ows(b"   "), b"");
        assert_eq!(trim_ows(b"\tfoo\t "), b"foo");
        assert_eq!(trim_ows(b"foo"), b"foo");
    }
}
