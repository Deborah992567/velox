//! Strict percent-decoding and query-string parsing.
//!
//! Request-targets and query strings may carry percent-encoded octets
//! (`%HH`). Per the architecture security model (§11) decoding must be
//! *strict*: a literal `%` not followed by two hex digits is a protocol
//! error, not something to pass through, because lenient handling is what
//! lets smuggling and traversal tricks slip through normalizers.
//!
//! The raw target stays in [`crate::http::Request`]; handlers decode paths
//! and query strings with the helpers here.

use crate::http::Request;

/// Why percent-decoding failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// A literal `%` was not followed by exactly two hex digits.
    InvalidEscape,
}

/// Decode a percent-encoded byte string in place, into `out`.
///
/// Returns [`DecodeError::InvalidEscape`] on a malformed escape. Encoded
/// bytes are emitted verbatim — decoding does *not* resolve `.`/`..` segments
/// or normalize anything else; that is the path normalizer's job.
pub fn decode(input: &[u8], out: &mut Vec<u8>) -> Result<(), DecodeError> {
    let mut i = 0;
    while i < input.len() {
        match input[i] {
            b'%' => {
                if i + 2 >= input.len() {
                    return Err(DecodeError::InvalidEscape);
                }
                let hi = hex_value(input[i + 1]).ok_or(DecodeError::InvalidEscape)?;
                let lo = hex_value(input[i + 2]).ok_or(DecodeError::InvalidEscape)?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    Ok(())
}

/// Decode a percent-encoded string into a new buffer.
pub fn decode_to_vec(input: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(input.len());
    decode(input, &mut out)?;
    Ok(out)
}

/// A decoded query-string key/value pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryParam {
    /// The decoded parameter name.
    pub key: Vec<u8>,
    /// The decoded parameter value, `None` when the raw string had no `=`.
    pub value: Option<Vec<u8>>,
}

/// Split a percent-decoded query string into parameters on `&`, separating
/// each on the first `=`.
///
/// The query must already have been percent-decoded with [`decode`]; this
/// only splits. `+` is *not* converted to space — form-encoding is a
/// `application/x-www-form-urlencoded` concern, not a URI concern — and any
/// byte is permitted in a decoded query value.
pub fn parse_query(query: &[u8]) -> Vec<QueryParam> {
    query
        .split(|&b| b == b'&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.iter().position(|&b| b == b'=').map_or_else(
                || QueryParam {
                    key: part.to_vec(),
                    value: None,
                },
                |i| QueryParam {
                    key: part[..i].to_vec(),
                    value: Some(part[i + 1..].to_vec()),
                },
            )
        })
        .collect()
}

/// Parse and percent-decode the query of a parsed [`Request`].
///
/// Returns an empty list when the request has no query or the query is
/// entirely empty segments.
pub fn parse_request_query(request: &Request) -> Result<Vec<QueryParam>, DecodeError> {
    let Some(query) = request.query() else {
        return Ok(Vec::new());
    };
    let decoded = decode_to_vec(query)?;
    Ok(parse_query(&decoded))
}

const fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeError, QueryParam, decode_to_vec, parse_query, parse_request_query};
    use crate::http::{BodyFraming, Headers, Method, Request, Version};

    #[test]
    fn decodes_plain_and_encoded() {
        assert_eq!(decode_to_vec(b"hello").unwrap(), b"hello");
        assert_eq!(decode_to_vec(b"a%20b%2Fc").unwrap(), b"a b/c");
        assert_eq!(decode_to_vec(b"%2f%2e%2e").unwrap(), b"/..");
        assert_eq!(decode_to_vec(b"100%25").unwrap(), b"100%");
        assert_eq!(decode_to_vec(b"%FF%00").unwrap(), b"\xff\x00");
    }

    #[test]
    fn rejects_malformed_escapes() {
        assert_eq!(decode_to_vec(b"a%"), Err(DecodeError::InvalidEscape));
        assert_eq!(decode_to_vec(b"a%2"), Err(DecodeError::InvalidEscape));
        assert_eq!(decode_to_vec(b"%GG"), Err(DecodeError::InvalidEscape));
        assert_eq!(decode_to_vec(b"%20%"), Err(DecodeError::InvalidEscape));
    }

    #[test]
    fn splits_query_pairs() {
        assert_eq!(
            parse_query(b"a=1&b=2"),
            vec![
                QueryParam {
                    key: b"a".to_vec(),
                    value: Some(b"1".to_vec())
                },
                QueryParam {
                    key: b"b".to_vec(),
                    value: Some(b"2".to_vec())
                },
            ]
        );
        assert_eq!(
            parse_query(b"flag"),
            vec![QueryParam {
                key: b"flag".to_vec(),
                value: None
            },]
        );
        assert_eq!(
            parse_query(b"a=b=c"),
            vec![QueryParam {
                key: b"a".to_vec(),
                value: Some(b"b=c".to_vec())
            },]
        );
        assert_eq!(
            parse_query(b"&&a=1&&"),
            vec![QueryParam {
                key: b"a".to_vec(),
                value: Some(b"1".to_vec())
            },]
        );
        assert_eq!(parse_query(b""), Vec::<QueryParam>::new());
    }

    #[test]
    fn request_query_is_decoded_and_split() {
        let request = Request::new(
            Method::Get,
            b"/search?q=hello%20world&page=2".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::None,
        );
        let params = parse_request_query(&request).unwrap();
        assert_eq!(
            params,
            vec![
                QueryParam {
                    key: b"q".to_vec(),
                    value: Some(b"hello world".to_vec())
                },
                QueryParam {
                    key: b"page".to_vec(),
                    value: Some(b"2".to_vec())
                },
            ]
        );

        let no_query = Request::new(
            Method::Get,
            b"/".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::None,
        );
        assert_eq!(
            parse_request_query(&no_query).unwrap(),
            Vec::<QueryParam>::new()
        );
    }
}
