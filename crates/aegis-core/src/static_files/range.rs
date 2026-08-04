//! Byte-range parsing for `Range` requests (RFC 9110 §14).
//!
//! The parser accepts `int-range` (`0-499`, `500-`) and `suffix-range`
//! (`-500`) specifications in the `bytes` range unit and resolves them against
//! the resource length into inclusive byte ranges. A header that uses an
//! unsupported unit or that is syntactically invalid is ignored, in which case
//! the server serves the full representation; a well-formed range-set that
//! cannot be satisfied maps to `416 Range Not Satisfiable` with a
//! `Content-Range: bytes */len` header.

/// An inclusive byte range within a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// First byte (0-based, inclusive).
    pub start: u64,
    /// Last byte (0-based, inclusive).
    pub end: u64,
}

impl ByteRange {
    /// The number of bytes covered by this range.
    pub const fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    /// Whether the range covers no bytes.
    pub const fn is_empty(&self) -> bool {
        self.end < self.start
    }
}

/// The outcome of parsing a `Range` header against a resource length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeResult {
    /// No usable range: the header is absent, uses an unsupported unit, or is
    /// syntactically invalid. Serve the full representation.
    Ignore,
    /// No satisfiable range: respond `416` with `Content-Range: bytes */len`.
    Unsatisfiable,
    /// Exactly one satisfiable range.
    Single(ByteRange),
    /// More than one satisfiable range.
    Multiple(Vec<ByteRange>),
}

/// Parse a `Range` header value against a resource of `len` bytes.
pub fn parse_range(value: &[u8], len: u64) -> RangeResult {
    let Some(eq) = value.iter().position(|&b| b == b'=') else {
        return RangeResult::Ignore;
    };
    let unit = trim_ows(&value[..eq]);
    if !unit.eq_ignore_ascii_case(b"bytes") {
        return RangeResult::Ignore;
    }
    let range_set = &value[eq + 1..];
    if trim_ows(range_set).is_empty() {
        return RangeResult::Ignore;
    }

    let mut ranges = Vec::new();
    for spec in split_commas(range_set) {
        match parse_range_spec(spec, len) {
            RangeSpec::Satisfiable(range) => ranges.push(range),
            RangeSpec::Unsatisfiable => {}
            RangeSpec::Malformed => return RangeResult::Ignore,
        }
    }
    if ranges.is_empty() {
        return RangeResult::Unsatisfiable;
    }
    if ranges.len() == 1 {
        RangeResult::Single(ranges[0])
    } else {
        RangeResult::Multiple(ranges)
    }
}

enum RangeSpec {
    Satisfiable(ByteRange),
    Unsatisfiable,
    Malformed,
}

fn parse_range_spec(spec: &[u8], len: u64) -> RangeSpec {
    if len == 0 {
        return RangeSpec::Unsatisfiable;
    }
    if let Some(suffix) = spec.strip_prefix(b"-") {
        let Some(length) = parse_decimal(suffix) else {
            return RangeSpec::Malformed;
        };
        if length == 0 {
            return RangeSpec::Unsatisfiable;
        }
        let start = len.saturating_sub(length);
        return RangeSpec::Satisfiable(ByteRange {
            start,
            end: len - 1,
        });
    }
    let Some(dash) = spec.iter().position(|&b| b == b'-') else {
        return RangeSpec::Malformed;
    };
    let Some(first) = parse_decimal(&spec[..dash]) else {
        return RangeSpec::Malformed;
    };
    if first >= len {
        return RangeSpec::Unsatisfiable;
    }
    let end = if dash + 1 == spec.len() {
        len - 1
    } else {
        let Some(last) = parse_decimal(&spec[dash + 1..]) else {
            return RangeSpec::Malformed;
        };
        if last < first {
            return RangeSpec::Malformed;
        }
        last.min(len - 1)
    };
    RangeSpec::Satisfiable(ByteRange { start: first, end })
}

/// Parse an ASCII decimal into a `u64`, rejecting empty or overflowing input.
fn parse_decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    bytes.iter().try_fold(0u64, |acc, &b| {
        acc.checked_mul(10)?.checked_add(u64::from(b - b'0'))
    })
}

/// Split a range-set on commas and trim each element.
fn split_commas(range_set: &[u8]) -> Vec<&[u8]> {
    range_set
        .split(|&b| b == b',')
        .map(trim_ows)
        .filter(|element| !element.is_empty())
        .collect()
}

/// Trim optional whitespace (spaces and tabs) from both ends of a value.
fn trim_ows(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, RangeResult, parse_range};

    #[test]
    fn int_ranges_resolve() {
        assert_eq!(
            parse_range(b"bytes=0-499", 1000),
            RangeResult::Single(ByteRange { start: 0, end: 499 })
        );
        assert_eq!(
            parse_range(b"bytes=500-", 1000),
            RangeResult::Single(ByteRange {
                start: 500,
                end: 999
            })
        );
        assert_eq!(
            parse_range(b"bytes=0-0", 1000),
            RangeResult::Single(ByteRange { start: 0, end: 0 })
        );
    }

    #[test]
    fn open_and_suffix_ranges_resolve() {
        assert_eq!(
            parse_range(b"bytes=-500", 1000),
            RangeResult::Single(ByteRange {
                start: 500,
                end: 999
            })
        );
        assert_eq!(
            parse_range(b"bytes=-500", 300),
            RangeResult::Single(ByteRange { start: 0, end: 299 })
        );
        assert_eq!(
            parse_range(b"bytes=0-99999", 100),
            RangeResult::Single(ByteRange { start: 0, end: 99 })
        );
    }

    #[test]
    fn multiple_ranges_are_preserved() {
        assert_eq!(
            parse_range(b"bytes=0-5,10-15", 1000),
            RangeResult::Multiple(vec![
                ByteRange { start: 0, end: 5 },
                ByteRange { start: 10, end: 15 },
            ])
        );
        assert_eq!(
            parse_range(b"bytes=1-2,999-", 100),
            RangeResult::Single(ByteRange { start: 1, end: 2 })
        );
    }

    #[test]
    fn unsatisfiable_ranges() {
        assert_eq!(parse_range(b"bytes=5-", 3), RangeResult::Unsatisfiable);
        assert_eq!(parse_range(b"bytes=-0", 100), RangeResult::Unsatisfiable);
        assert_eq!(parse_range(b"bytes=10-20", 5), RangeResult::Unsatisfiable);
        assert_eq!(parse_range(b"bytes=100-", 100), RangeResult::Unsatisfiable);
    }

    #[test]
    fn unsupported_and_malformed_headers_are_ignored() {
        assert_eq!(parse_range(b"items=0-5", 1000), RangeResult::Ignore);
        assert_eq!(parse_range(b"bytes", 1000), RangeResult::Ignore);
        assert_eq!(parse_range(b"bytes=", 1000), RangeResult::Ignore);
        assert_eq!(parse_range(b"bytes=10-5", 1000), RangeResult::Ignore);
        assert_eq!(parse_range(b"bytes=abc", 1000), RangeResult::Ignore);
        assert_eq!(parse_range(b"bytes=1,", 1000), RangeResult::Ignore);
        assert_eq!(parse_range(b"", 1000), RangeResult::Ignore);
        assert_eq!(
            parse_range(b"bytes=99999999999999999999999-", 1000),
            RangeResult::Ignore
        );
    }

    #[test]
    fn whitespace_is_tolerated() {
        assert_eq!(
            parse_range(b"  bytes = 0-499 , 600- ", 1000),
            RangeResult::Multiple(vec![
                ByteRange { start: 0, end: 499 },
                ByteRange {
                    start: 600,
                    end: 999
                },
            ])
        );
        assert_eq!(
            parse_range(b"BYTES=0-499", 1000),
            RangeResult::Single(ByteRange { start: 0, end: 499 })
        );
    }

    #[test]
    fn empty_resource_is_never_satisfiable() {
        assert_eq!(parse_range(b"bytes=0-0", 0), RangeResult::Unsatisfiable);
        assert_eq!(parse_range(b"bytes=-1", 0), RangeResult::Unsatisfiable);
    }

    #[test]
    fn range_length_counts_bytes() {
        let range = ByteRange { start: 5, end: 9 };
        assert_eq!(range.len(), 5);
        assert_eq!(ByteRange { start: 0, end: 0 }.len(), 1);
        assert!(!range.is_empty());
        assert!(ByteRange { start: 2, end: 1 }.is_empty());
    }
}
