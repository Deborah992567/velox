//! Incremental HTTP/1.x response-head parser.
//!
//! The mirror of [`super::parser`] for the other direction: a proxy reads the
//! upstream's status line and header fields with the same incremental,
//! limit-enforcing FSM that the server uses for request heads. The response
//! grammar is a strict subset of the request grammar — no request-target, no
//! mandatory `Host` — so this parser only rejects what a response can get
//! wrong: a malformed status line, an out-of-range status code, or framing
//! that a proxy cannot safely relay (`Content-Length` and `Transfer-Encoding`
//! together, or a transfer coding other than plain `chunked`).
//!
//! Framing follows RFC 9112 §6.3: `Transfer-Encoding` overrides
//! `Content-Length`, a lone digits-only `Content-Length` wins otherwise, and
//! neither means the body runs until the connection closes (only valid for
//! `HTTP/1.0`-style upstreams, which a per-request proxy connection handles
//! naturally).

use super::{HeaderFieldError, parse_header_field};
use crate::http::http1::engine::status_allows_body;
use crate::http::limits::RequestLimits;
use crate::http::{BodyFraming, HeaderName, Headers, Method, StatusCode, Version};

/// Progress of a [`ResponseParser::feed`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedResult {
    /// The head is not complete yet; feed more bytes.
    Incomplete,
    /// The head parsed cleanly. As with the request parser, trailing bytes
    /// (the start of the body, or a pipelined next head) are retrieved with
    /// [`ResponseParser::drain_pending`].
    Complete(ResponseHead),
    /// The head is malformed or over a limit; the upstream connection must be
    /// rejected with a `502`.
    Error(ResponseParseError),
}

/// Why a response head was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseParseError {
    /// A line was terminated by a bare LF rather than CRLF.
    BareLf,
    /// The status line was empty (the head started with CRLF).
    EmptyStatusLine,
    /// The status line did not split into `version SP status [reason]` with a
    /// valid version and a 3-digit numeric status.
    InvalidStatusLine,
    /// The version token was not exactly `HTTP/1.0` or `HTTP/1.1`.
    InvalidVersion,
    /// The status code was not three digits in the range 100–599.
    InvalidStatus,
    /// `Content-Length` and `Transfer-Encoding` were both present, or the
    /// `Transfer-Encoding` named a coding other than a single `chunked`.
    UnsupportedFraming,
    /// A header field name was malformed.
    InvalidHeaderName,
    /// A header field value contained a control byte other than HTAB.
    InvalidHeaderValue,
    /// The accumulated head exceeded `RequestLimits::max_head_size`.
    HeadTooLarge,
    /// More than `RequestLimits::max_headers` header fields.
    TooManyHeaders,
    /// A header field line exceeded `RequestLimits::max_header_value`.
    HeaderValueTooLong,
}

/// A parsed HTTP/1.x response head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHead {
    /// The protocol version.
    pub version: Version,
    /// The status code with its reason phrase.
    pub status: StatusCode,
    /// The header fields, in order.
    pub headers: Headers,
    /// How the response body is delimited on the wire.
    pub framing: BodyFraming,
}

impl ResponseHead {
    /// Whether this response carries a body: the request method is not `HEAD`
    /// and the status code permits one.
    pub const fn has_body(&self, method: &Method) -> bool {
        !matches!(method, Method::Head) && status_allows_body(self.status)
    }
}

/// Which kind of line the parser is currently accumulating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Reading the status line.
    StatusLine,
    /// Reading header field lines (terminated by the blank line).
    HeaderFields,
}

/// An incremental HTTP/1.x response-head parser.
#[derive(Debug)]
pub struct ResponseParser {
    limits: RequestLimits,
    buf: Vec<u8>,
    stage: Stage,
    version: Version,
    status: StatusCode,
    headers: Headers,
    head_bytes: usize,
}

impl Default for ResponseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponseParser {
    /// A parser with the default [`RequestLimits`].
    pub fn new() -> Self {
        Self::new_with_limits(RequestLimits::default())
    }

    /// A parser with explicit [`RequestLimits`].
    pub const fn new_with_limits(limits: RequestLimits) -> Self {
        Self {
            limits,
            buf: Vec::new(),
            stage: Stage::StatusLine,
            version: Version::Http11,
            status: StatusCode::new(200),
            headers: Headers::new(),
            head_bytes: 0,
        }
    }

    /// Feed more bytes and advance the parse.
    ///
    /// On [`FeedResult::Complete`] the head has been parsed; trailing bytes
    /// can be taken with [`drain_pending`] and fed into the body handler or,
    /// after a relayed `1xx` interim head, back into a reset parser. On
    /// [`FeedResult::Error`] the parse is poisoned and the upstream connection
    /// must be discarded.
    ///
    /// [`drain_pending`]: ResponseParser::drain_pending
    pub fn feed(&mut self, bytes: &[u8]) -> FeedResult {
        self.buf.extend_from_slice(bytes);
        loop {
            let line = match self.next_line() {
                Ok(Some(line)) => line,
                Ok(None) => {
                    return match self.check_in_progress() {
                        Ok(()) => FeedResult::Incomplete,
                        Err(error) => FeedResult::Error(error),
                    };
                }
                Err(error) => return FeedResult::Error(error),
            };
            self.head_bytes += line.len() + 2;
            if self.head_bytes > self.limits.max_head_size {
                return FeedResult::Error(ResponseParseError::HeadTooLarge);
            }
            match self.stage {
                Stage::StatusLine => {
                    if let Err(error) = self.parse_status_line(&line) {
                        return FeedResult::Error(error);
                    }
                }
                Stage::HeaderFields => {
                    if line.is_empty() {
                        return match self.finish_head() {
                            Ok(head) => FeedResult::Complete(head),
                            Err(error) => FeedResult::Error(error),
                        };
                    }
                    let header = match parse_header_field(&line) {
                        Ok(header) => header,
                        Err(error) => return FeedResult::Error(map_header_error(error)),
                    };
                    self.headers.push(header);
                    if self.headers.len() > self.limits.max_headers {
                        return FeedResult::Error(ResponseParseError::TooManyHeaders);
                    }
                }
            }
        }
    }

    /// Bytes that arrived after the completed head (the start of the body).
    /// Returns the empty slice when the buffer is drained.
    pub fn drain_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// Drop the buffer and re-arm the parser for the next head on the same
    /// connection (used after relaying a `1xx` interim response).
    pub fn reset(&mut self) {
        self.buf.clear();
        self.stage = Stage::StatusLine;
        self.version = Version::Http11;
        self.status = StatusCode::new(200);
        self.headers = Headers::new();
        self.head_bytes = 0;
    }

    /// Pull one complete line (without its CRLF) from the buffer.
    fn next_line(&mut self) -> Result<Option<Vec<u8>>, ResponseParseError> {
        let Some(lf) = self.buf.iter().position(|&b| b == b'\n') else {
            return Ok(None);
        };
        if lf == 0 || self.buf[lf - 1] != b'\r' {
            return Err(ResponseParseError::BareLf);
        }
        let line = self.buf[..lf - 1].to_vec();
        self.buf.drain(..=lf);
        Ok(Some(line))
    }
    /// Apply limits to the in-progress line and the running head total.
    const fn check_in_progress(&self) -> Result<(), ResponseParseError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        match self.stage {
            Stage::StatusLine if self.buf.len() > self.limits.max_request_line => {
                return Err(ResponseParseError::InvalidStatusLine);
            }
            Stage::HeaderFields if self.buf.len() > self.limits.max_header_value => {
                return Err(ResponseParseError::HeaderValueTooLong);
            }
            _ => {}
        }
        if self.head_bytes + self.buf.len() > self.limits.max_head_size {
            return Err(ResponseParseError::HeadTooLarge);
        }
        Ok(())
    }

    /// Parse `version SP status [reason]`, where `status` is exactly three
    /// digits in the range 100–599. The reason phrase is optional (RFC 9112
    /// §4): it is consumed but not validated.
    fn parse_status_line(&mut self, line: &[u8]) -> Result<(), ResponseParseError> {
        if line.is_empty() {
            return Err(ResponseParseError::EmptyStatusLine);
        }
        let mut parts = line.splitn(3, |&b| b == b' ');
        let version = parts.next().unwrap_or_default();
        let status = parts.next().unwrap_or_default();
        if version.is_empty() || status.is_empty() {
            return Err(ResponseParseError::InvalidStatusLine);
        }
        let version = Version::parse(version).ok_or(ResponseParseError::InvalidVersion)?;
        if status.len() != 3 || !status.iter().all(u8::is_ascii_digit) {
            return Err(ResponseParseError::InvalidStatus);
        }
        let code = status
            .iter()
            .fold(0u16, |acc, b| acc * 10 + u16::from(b - b'0'));
        if !(100..=599).contains(&code) {
            return Err(ResponseParseError::InvalidStatus);
        }
        self.version = version;
        self.status = StatusCode::new(code);
        self.stage = Stage::HeaderFields;
        Ok(())
    }

    /// Derive the response body framing.
    fn finish_head(&mut self) -> Result<ResponseHead, ResponseParseError> {
        let framing = self.framing()?;
        Ok(ResponseHead {
            version: self.version,
            status: self.status,
            headers: std::mem::take(&mut self.headers),
            framing,
        })
    }

    /// Decide how the response body is framed on the wire.
    ///
    /// `Transfer-Encoding` overrides `Content-Length` (RFC 9112 §6.3). A
    /// transfer coding other than a single `chunked` is rejected because the
    /// proxy cannot re-emit it safely.
    fn framing(&self) -> Result<BodyFraming, ResponseParseError> {
        let transfer_encodings: Vec<&[u8]> = self
            .headers
            .get_all(&HeaderName::TransferEncoding)
            .collect();
        if !transfer_encodings.is_empty() {
            if transfer_encodings.len() != 1
                || !transfer_encodings[0].eq_ignore_ascii_case(b"chunked")
            {
                return Err(ResponseParseError::UnsupportedFraming);
            }
            return Ok(BodyFraming::Chunked);
        }
        if self.headers.contains(&HeaderName::ContentLength) {
            let length = self
                .headers
                .content_length()
                .ok_or(ResponseParseError::UnsupportedFraming)?;
            return Ok(BodyFraming::Length(length));
        }
        Ok(BodyFraming::None)
    }
}

const fn map_header_error(error: HeaderFieldError) -> ResponseParseError {
    match error {
        HeaderFieldError::MissingColon
        | HeaderFieldError::EmptyName
        | HeaderFieldError::InvalidName => ResponseParseError::InvalidHeaderName,
        HeaderFieldError::InvalidValue => ResponseParseError::InvalidHeaderValue,
    }
}

#[cfg(test)]
mod tests {
    use super::{FeedResult, ResponseParseError, ResponseParser};
    use crate::http::limits::RequestLimits;
    use crate::http::{BodyFraming, Method, StatusCode, Version};

    fn parse_one(bytes: &[u8]) -> Result<super::ResponseHead, ResponseParseError> {
        let mut parser = ResponseParser::new();
        match parser.feed(bytes) {
            FeedResult::Complete(head) => Ok(head),
            FeedResult::Incomplete => panic!("feed returned Incomplete for a complete head"),
            FeedResult::Error(error) => Err(error),
        }
    }

    #[test]
    fn parses_a_simple_200() {
        let head =
            parse_one(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\n")
                .unwrap();
        assert_eq!(head.version, Version::Http11);
        assert_eq!(head.status, StatusCode::OK);
        assert_eq!(head.framing, BodyFraming::Length(5));
        assert_eq!(
            head.headers.get_str(&crate::http::HeaderName::ContentType),
            Some("text/plain")
        );
        assert!(head.has_body(&Method::Get));
    }

    #[test]
    fn parses_status_without_reason_phrase() {
        let head = parse_one(b"HTTP/1.1 204\r\n\r\n").unwrap();
        assert_eq!(head.status, StatusCode::NO_CONTENT);
        assert!(!head.has_body(&Method::Get));
    }

    #[test]
    fn parses_http10() {
        let head = parse_one(b"HTTP/1.0 200 OK\r\n\r\n").unwrap();
        assert_eq!(head.version, Version::Http10);
        assert_eq!(head.framing, BodyFraming::None);
    }

    #[test]
    fn head_has_no_body() {
        let head = parse_one(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n").unwrap();
        assert!(head.has_body(&Method::Get));
        assert!(!head.has_body(&Method::Head));
    }

    #[test]
    fn chunked_framing_wins_over_content_length() {
        let head = parse_one(
            b"HTTP/1.1 200 OK\r\nContent-Length: 999\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .unwrap();
        assert_eq!(head.framing, BodyFraming::Chunked);
    }

    #[test]
    fn rejects_unsupported_transfer_encoding() {
        for te in [b"gzip".as_slice(), b"gzip, chunked", b"chunked, chunked"] {
            let mut head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: ".to_vec();
            head.extend_from_slice(te);
            head.extend_from_slice(b"\r\n\r\n");
            assert_eq!(
                parse_one(&head).unwrap_err(),
                ResponseParseError::UnsupportedFraming,
                "te={te:?}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_content_length() {
        assert_eq!(
            parse_one(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\n")
                .unwrap_err(),
            ResponseParseError::UnsupportedFraming
        );
    }

    #[test]
    fn rejects_malformed_status_lines() {
        let mut head = b"HTTP/1.1\r\n".to_vec();
        head.extend_from_slice(b"\r\n");
        assert_eq!(
            parse_one(&head).unwrap_err(),
            ResponseParseError::InvalidStatusLine,
            "line={head:?}"
        );
    }

    #[test]
    fn rejects_bad_versions() {
        for line in [
            b"HTTP/2.0 200 OK\r\n".as_slice(),
            b"HTTP/1.9 200 OK\r\n".as_slice(),
        ] {
            let mut head = line.to_vec();
            head.extend_from_slice(b"\r\n");
            assert_eq!(
                parse_one(&head).unwrap_err(),
                ResponseParseError::InvalidVersion,
                "line={line:?}"
            );
        }
    }

    #[test]
    fn rejects_bad_status_codes() {
        for line in [
            b"HTTP/1.1 20 OK\r\n".as_slice(),
            b"HTTP/1.1 2000 OK\r\n".as_slice(),
            b"HTTP/1.1 99 OK\r\n".as_slice(),
            b"HTTP/1.1 600 OK\r\n".as_slice(),
            b"HTTP/1.1 xyz OK\r\n".as_slice(),
        ] {
            let mut head = line.to_vec();
            head.extend_from_slice(b"\r\n");
            assert_eq!(
                parse_one(&head).unwrap_err(),
                ResponseParseError::InvalidStatus,
                "line={line:?}"
            );
        }
    }

    #[test]
    fn rejects_bare_lf_and_empty_status_line() {
        assert_eq!(
            parse_one(b"HTTP/1.1 200 OK\n\r\n").unwrap_err(),
            ResponseParseError::BareLf
        );
        assert_eq!(
            parse_one(b"\r\nHTTP/1.1 200 OK\r\n\r\n").unwrap_err(),
            ResponseParseError::EmptyStatusLine
        );
    }

    #[test]
    fn rejects_invalid_header_lines() {
        assert_eq!(
            parse_one(b"HTTP/1.1 200 OK\r\nBad Name: x\r\n\r\n").unwrap_err(),
            ResponseParseError::InvalidHeaderName
        );
        assert_eq!(
            parse_one(b"HTTP/1.1 200 OK\r\nX-Bad: a\x00b\r\n\r\n").unwrap_err(),
            ResponseParseError::InvalidHeaderValue
        );
    }

    #[test]
    fn feeds_incrementally_and_buffers_pending() {
        let mut parser = ResponseParser::new();
        let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nxyz";
        let mut head = None;
        for &byte in wire {
            match parser.feed(&[byte]) {
                FeedResult::Incomplete => {}
                FeedResult::Complete(parsed) => head = Some(parsed),
                FeedResult::Error(error) => panic!("unexpected error {error:?}"),
            }
        }
        let head = head.expect("head never completed");
        assert_eq!(head.framing, BodyFraming::Length(3));
        assert_eq!(parser.drain_pending(), b"xyz");
    }

    #[test]
    fn resets_for_interim_head() {
        let mut parser = ResponseParser::new();
        match parser.feed(b"HTTP/1.1 103 Early Hints\r\nLink: </style.css>; rel=preload\r\n\r\n") {
            FeedResult::Complete(head) => {
                assert_eq!(head.status.code(), 103);
                assert!(!head.has_body(&Method::Get));
            }
            other => panic!("{other:?}"),
        }
        parser.reset();
        match parser.feed(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n") {
            FeedResult::Complete(head) => assert_eq!(head.status, StatusCode::OK),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn enforces_head_size_limit() {
        let mut parser = ResponseParser::new_with_limits(RequestLimits::small());
        let mut head = b"HTTP/1.1 200 OK\r\n".to_vec();
        while head.len() < 400 {
            head.extend_from_slice(b"X-Filler: ");
            head.extend(std::iter::repeat_n(b'z', 40));
            head.extend_from_slice(b"\r\n");
        }
        head.extend_from_slice(b"\r\n");
        assert_eq!(
            parser.feed(&head),
            FeedResult::Error(ResponseParseError::HeadTooLarge)
        );
    }
}
