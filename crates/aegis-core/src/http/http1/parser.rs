//! Incremental HTTP/1.x request-head parser.
//!
//! The parser is a consumer of exactly what it is fed: it scans its internal
//! buffer for the CRLF that terminates each line, keeps partial lines buffered
//! until their terminator arrives, and applies the [`RequestLimits`] *during*
//! scanning so a slowloris-style client can never grow an unbounded head
//! (architecture §11). Malformed input is rejected outright — strictness is a
//! security property, not a convenience — and framing decisions
//! (`Content-Length` vs. `Transfer-Encoding`) come from a single canonical
//! source so the request-smuggling defense is structural rather than
//! heuristic.
//!
//! A completed head resets the parser for the next message on the same
//! connection; bytes that arrived after the head (a body or a pipelined
//! request) stay buffered and are retrieved with
//! [`RequestParser::drain_pending`].

use super::{HeaderFieldError, parse_header_field};
use crate::http::limits::RequestLimits;
use crate::http::{
    BodyFraming, HeaderName, Headers, Method, Request, StatusCode, Version, is_tchar,
};

/// Progress of a [`RequestParser::feed`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedResult {
    /// The head is not complete yet; feed more bytes.
    Incomplete,
    /// The head parsed cleanly. The parser has already been reset for the
    /// next message; any bytes that followed the head (body or pipelined
    /// request) remain buffered and can be retrieved with
    /// [`RequestParser::drain_pending`].
    Complete(Request),
    /// The head is malformed or over a limit; the connection must be
    /// rejected. [`ParseError::status`] gives the response code.
    Error(ParseError),
}

/// Why a request head was rejected.
///
/// The error maps onto the HTTP response that should be sent before the
/// connection is closed (see [`ParseError::status`]). Parser errors are
/// distinct from [`crate::http::limits::LimitViolation`] because the parser
/// needs the precise protocol reason, not just which bound was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// A line was terminated by a bare LF rather than CRLF.
    BareLf,
    /// The request line was empty (the message started with CRLF).
    EmptyRequestLine,
    /// The request line did not split into exactly `method SP target SP
    /// version` with non-empty parts.
    InvalidRequestLine,
    /// The method token contained a non-tchar byte.
    InvalidMethod,
    /// The method token exceeded `RequestLimits::max_method_len`.
    MethodTooLong,
    /// The request-target was not a legal origin/absolute/authority/
    /// asterisk-form for the method.
    InvalidTarget,
    /// The request-target exceeded `RequestLimits::max_target_len`.
    TargetTooLong,
    /// A header field name was malformed (including obs-fold continuation
    /// lines, which begin with whitespace).
    InvalidHeaderName,
    /// A header field value contained a control byte other than HTAB.
    InvalidHeaderValue,
    /// HTTP/1.1 request without a `Host` field.
    MissingHost,
    /// More than one `Host` field.
    DuplicateHost,
    /// A `Content-Length` field was not a single digits-only decimal value.
    MalformedContentLength,
    /// `Content-Length` and `Transfer-Encoding` were both present — the
    /// canonical request-smuggling signature.
    ContentLengthAndTransferEncoding,
    /// A `Transfer-Encoding` other than a single `chunked` was used on a
    /// request.
    TransferEncodingNotChunked,
    /// The request line exceeded `RequestLimits::max_request_line`.
    RequestLineTooLong,
    /// The accumulated head exceeded `RequestLimits::max_head_size`.
    HeadTooLarge,
    /// More than `RequestLimits::max_headers` header fields.
    TooManyHeaders,
    /// A header field line exceeded `RequestLimits::max_header_value`.
    HeaderValueTooLong,
    /// The version token was not exactly `HTTP/1.0` or `HTTP/1.1`.
    InvalidVersion,
}

impl ParseError {
    /// The HTTP response code that should accompany this rejection.
    pub const fn status(self) -> StatusCode {
        match self {
            Self::InvalidVersion => StatusCode::HTTP_VERSION_NOT_SUPPORTED,
            Self::RequestLineTooLong | Self::TargetTooLong => StatusCode::URI_TOO_LONG,
            Self::HeadTooLarge | Self::TooManyHeaders | Self::HeaderValueTooLong => {
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
            }
            _ => StatusCode::BAD_REQUEST,
        }
    }
}

/// Which kind of line the parser is currently accumulating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Reading the request line.
    RequestLine,
    /// Reading header field lines (terminated by the blank line).
    HeaderFields,
}

/// An incremental HTTP/1.x request-head parser.
///
/// `feed` consumes whatever bytes the connection provides and reports the
/// first terminal outcome: the head is still incomplete, the head completed,
/// or the head was rejected. The parser owns its buffer, so a caller hands it
/// bytes once and never thinks about them again until it receives a terminal
/// result.
#[derive(Debug)]
pub struct RequestParser {
    limits: RequestLimits,
    buf: Vec<u8>,
    stage: Stage,
    method: Method,
    target: Vec<u8>,
    version: Version,
    headers: Headers,
    head_bytes: usize,
}

impl Default for RequestParser {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestParser {
    /// A parser with the default [`RequestLimits`].
    pub fn new() -> Self {
        Self::new_with_limits(RequestLimits::default())
    }

    /// A parser with explicit [`RequestLimits`].
    pub const fn new_with_limits(limits: RequestLimits) -> Self {
        Self {
            limits,
            buf: Vec::new(),
            stage: Stage::RequestLine,
            method: Method::Get,
            target: Vec::new(),
            version: Version::Http11,
            headers: Headers::new(),
            head_bytes: 0,
        }
    }

    /// Feed more bytes and advance the parse.
    ///
    /// On [`FeedResult::Complete`] the parser has already been reset for the
    /// next message; trailing bytes can be taken with [`drain_pending`]
    /// (for a body or a pipelined request) and fed back in. On
    /// [`FeedResult::Error`] the parse is poisoned and the connection must be
    /// closed after sending the mapped status.
    ///
    /// [`drain_pending`]: RequestParser::drain_pending
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
                return FeedResult::Error(ParseError::HeadTooLarge);
            }
            match self.stage {
                Stage::RequestLine => {
                    if let Err(error) = self.parse_request_line(&line) {
                        return FeedResult::Error(error);
                    }
                }
                Stage::HeaderFields => {
                    if line.is_empty() {
                        return match self.finish_head() {
                            Ok(request) => FeedResult::Complete(request),
                            Err(error) => FeedResult::Error(error),
                        };
                    }
                    let header = match parse_header_field(&line) {
                        Ok(header) => header,
                        Err(error) => return FeedResult::Error(map_header_error(error)),
                    };
                    self.headers.push(header);
                    if self.headers.len() > self.limits.max_headers {
                        return FeedResult::Error(ParseError::TooManyHeaders);
                    }
                }
            }
        }
    }

    /// Bytes that arrived after the completed head (body or pipelined
    /// request). Returns the empty slice when the buffer is drained.
    pub fn drain_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// Drop the buffer and re-arm the parser for a fresh message.
    ///
    /// This is only needed when abandoning an in-flight head (e.g. a protocol
    /// upgrade); a completed head resets automatically.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.stage = Stage::RequestLine;
        self.method = Method::Get;
        self.target.clear();
        self.version = Version::Http11;
        self.headers = Headers::new();
        self.head_bytes = 0;
    }

    /// Pull one complete line (without its CRLF) from the buffer.
    fn next_line(&mut self) -> Result<Option<Vec<u8>>, ParseError> {
        let Some(lf) = self.buf.iter().position(|&b| b == b'\n') else {
            return Ok(None);
        };
        if lf == 0 || self.buf[lf - 1] != b'\r' {
            return Err(ParseError::BareLf);
        }
        let line = self.buf[..lf - 1].to_vec();
        self.buf.drain(..=lf);
        Ok(Some(line))
    }

    /// Apply limits to the in-progress line and the running head total.
    const fn check_in_progress(&self) -> Result<(), ParseError> {
        if self.buf.is_empty() {
            return Ok(());
        }
        match self.stage {
            Stage::RequestLine if self.buf.len() > self.limits.max_request_line => {
                return Err(ParseError::RequestLineTooLong);
            }
            Stage::HeaderFields if self.buf.len() > self.limits.max_header_value => {
                return Err(ParseError::HeaderValueTooLong);
            }
            _ => {}
        }
        if self.head_bytes + self.buf.len() > self.limits.max_head_size {
            return Err(ParseError::HeadTooLarge);
        }
        Ok(())
    }

    /// Parse `method SP target SP version`, validating against the limits and
    /// the RFC 9112 request-target forms.
    fn parse_request_line(&mut self, line: &[u8]) -> Result<(), ParseError> {
        if line.is_empty() {
            return Err(ParseError::EmptyRequestLine);
        }
        let mut parts = line.split(|&b| b == b' ');
        let method = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        let version = parts.next().unwrap_or_default();
        if method.is_empty() || target.is_empty() || version.is_empty() || parts.next().is_some() {
            return Err(ParseError::InvalidRequestLine);
        }
        if method.len() > self.limits.max_method_len {
            return Err(ParseError::MethodTooLong);
        }
        if !method.iter().all(|&b| is_tchar(b)) {
            return Err(ParseError::InvalidMethod);
        }
        if target.len() > self.limits.max_target_len {
            return Err(ParseError::TargetTooLong);
        }
        let method = Method::parse(method).unwrap_or_else(|| {
            Method::extension(
                std::str::from_utf8(method)
                    .expect("a tchar-only token is always ASCII")
                    .into(),
            )
        });
        Self::validate_target(&method, target)?;
        let version = Version::parse(version).ok_or(ParseError::InvalidVersion)?;
        self.method = method;
        self.target = target.to_vec();
        self.version = version;
        self.stage = Stage::HeaderFields;
        Ok(())
    }

    /// Validate the request-target against the RFC 9112 §3.2 forms allowed for
    /// the method: origin-form (`/path`), asterisk-form (`*` for `OPTIONS`
    /// only), authority-form (`CONNECT` only), and absolute-form
    /// (`scheme://host`).
    fn validate_target(method: &Method, target: &[u8]) -> Result<(), ParseError> {
        if target.iter().any(|&b| b < 0x20 || b == 0x7f) {
            return Err(ParseError::InvalidTarget);
        }
        if method == &Method::Connect {
            if target.contains(&b'/') || target.contains(&b'?') {
                return Err(ParseError::InvalidTarget);
            }
            return Ok(());
        }
        if target == b"*" {
            return if matches!(method, Method::Options) {
                Ok(())
            } else {
                Err(ParseError::InvalidTarget)
            };
        }
        if target.starts_with(b"/") {
            return Ok(());
        }
        let scheme_break = target.iter().position(|&b| b == b':');
        if scheme_break
            .is_some_and(|i| i > 0 && target.len() > i + 2 && target[i + 1..].starts_with(b"//"))
        {
            return Ok(());
        }
        Err(ParseError::InvalidTarget)
    }

    /// Derive the body framing and assemble the request, applying the
    /// smuggling defenses: no CL+TE co-presence, single digits-only
    /// `Content-Length`, single exactly-`chunked` `Transfer-Encoding`, and a
    /// mandatory single `Host` on HTTP/1.1.
    fn finish_head(&mut self) -> Result<Request, ParseError> {
        if self.version == Version::Http11 {
            match self.headers.get_all(&HeaderName::Host).count() {
                0 => return Err(ParseError::MissingHost),
                1 => {}
                _ => return Err(ParseError::DuplicateHost),
            }
        }
        let framing = self.framing()?;
        let request = Request::new(
            self.method.clone(),
            std::mem::take(&mut self.target),
            self.version,
            std::mem::take(&mut self.headers),
            framing,
        );
        self.method = Method::Get;
        self.stage = Stage::RequestLine;
        Ok(request)
    }

    /// Decide how the request body is framed on the wire.
    fn framing(&self) -> Result<BodyFraming, ParseError> {
        let has_content_length = self.headers.contains(&HeaderName::ContentLength);
        let transfer_encodings: Vec<&[u8]> = self
            .headers
            .get_all(&HeaderName::TransferEncoding)
            .collect();
        if has_content_length && !transfer_encodings.is_empty() {
            return Err(ParseError::ContentLengthAndTransferEncoding);
        }
        if !transfer_encodings.is_empty() {
            if transfer_encodings.len() != 1
                || !transfer_encodings[0].eq_ignore_ascii_case(b"chunked")
            {
                return Err(ParseError::TransferEncodingNotChunked);
            }
            return Ok(BodyFraming::Chunked);
        }
        if has_content_length {
            let length = self
                .headers
                .content_length()
                .ok_or(ParseError::MalformedContentLength)?;
            return Ok(BodyFraming::Length(length));
        }
        Ok(BodyFraming::None)
    }
}

const fn map_header_error(error: HeaderFieldError) -> ParseError {
    match error {
        HeaderFieldError::MissingColon
        | HeaderFieldError::EmptyName
        | HeaderFieldError::InvalidName => ParseError::InvalidHeaderName,
        HeaderFieldError::InvalidValue => ParseError::InvalidHeaderValue,
    }
}

#[cfg(test)]
mod tests {
    use super::{FeedResult, ParseError, RequestParser};
    use crate::http::limits::RequestLimits;
    use crate::http::{BodyFraming, Method, Request, StatusCode, Version};

    fn parse_one(bytes: &[u8]) -> Result<Request, ParseError> {
        let mut parser = RequestParser::new();
        match parser.feed(bytes) {
            FeedResult::Complete(request) => Ok(request),
            FeedResult::Incomplete => panic!("feed returned Incomplete for a complete head"),
            FeedResult::Error(error) => Err(error),
        }
    }

    #[test]
    fn parses_a_simple_get() {
        let request = parse_one(b"GET /index.html HTTP/1.1\r\nHost: example.com\r\n\r\n").unwrap();
        assert_eq!(request.method, Method::Get);
        assert_eq!(request.target, b"/index.html");
        assert_eq!(request.version, Version::Http11);
        assert_eq!(request.framing, BodyFraming::None);
        assert_eq!(request.host(), Some(&b"example.com"[..]));
    }

    #[test]
    fn parses_content_length_body() {
        let request =
            parse_one(b"POST /submit HTTP/1.1\r\nHost: x\r\nContent-Length: 12\r\n\r\n").unwrap();
        assert_eq!(request.method, Method::Post);
        assert_eq!(request.framing, BodyFraming::Length(12));
    }

    #[test]
    fn parses_chunked_body() {
        let request =
            parse_one(b"POST /upload HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n")
                .unwrap();
        assert_eq!(request.framing, BodyFraming::Chunked);
    }

    #[test]
    fn records_expect_continue() {
        let request = parse_one(
            b"POST /up HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nExpect: 100-continue\r\n\r\n",
        )
        .unwrap();
        assert!(request.expects_continue());
        let plain =
            parse_one(b"POST /up HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\n").unwrap();
        assert!(!plain.expects_continue());
    }

    #[test]
    fn http10_does_not_require_host() {
        let request = parse_one(b"GET / HTTP/1.0\r\n\r\n").unwrap();
        assert_eq!(request.version, Version::Http10);
        assert_eq!(request.framing, BodyFraming::None);
    }

    #[test]
    fn http11_requires_a_single_host() {
        assert_eq!(
            parse_one(b"GET / HTTP/1.1\r\n\r\n").unwrap_err(),
            ParseError::MissingHost
        );
        assert_eq!(
            parse_one(b"GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n").unwrap_err(),
            ParseError::DuplicateHost
        );
    }

    #[test]
    fn rejects_cl_te_co_presence() {
        assert_eq!(
            parse_one(
                b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n"
            )
            .unwrap_err(),
            ParseError::ContentLengthAndTransferEncoding
        );
    }

    #[test]
    fn rejects_non_chunked_transfer_encoding() {
        for te in [
            b"gzip".as_slice(),
            b"gzip, chunked",
            b"chunked, chunked",
            b"identity",
        ] {
            let mut head = b"POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: ".to_vec();
            head.extend_from_slice(te);
            head.extend_from_slice(b"\r\n\r\n");
            assert_eq!(
                parse_one(&head).unwrap_err(),
                ParseError::TransferEncodingNotChunked,
                "te={te:?}"
            );
        }
    }

    #[test]
    fn rejects_malformed_content_length() {
        for cl in [
            b"5,5".as_slice(),
            b"x".as_slice(),
            b"+5".as_slice(),
            b"18446744073709551616".as_slice(),
        ] {
            let mut head = b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: ".to_vec();
            head.extend_from_slice(cl);
            head.extend_from_slice(b"\r\n\r\n");
            assert_eq!(
                parse_one(&head).unwrap_err(),
                ParseError::MalformedContentLength,
                "cl={cl:?}"
            );
        }
        assert_eq!(
            parse_one(
                b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\n"
            )
            .unwrap_err(),
            ParseError::MalformedContentLength
        );
    }

    #[test]
    fn rejects_obs_fold_and_bare_lf() {
        assert_eq!(
            parse_one(b"GET / HTTP/1.1\r\nHost: x\r\n Foo: bar\r\n\r\n").unwrap_err(),
            ParseError::InvalidHeaderName
        );
        assert_eq!(
            parse_one(b"GET / HTTP/1.1\nHost: x\r\n\r\n").unwrap_err(),
            ParseError::BareLf
        );
    }

    #[test]
    fn rejects_control_bytes_in_header_value() {
        assert_eq!(
            parse_one(b"GET / HTTP/1.1\r\nHost: x\r\nX-Bad: a\x00b\r\n\r\n").unwrap_err(),
            ParseError::InvalidHeaderValue
        );
    }

    #[test]
    fn rejects_unknown_http_versions() {
        assert_eq!(
            parse_one(b"GET / HTTP/2.0\r\nHost: x\r\n\r\n").unwrap_err(),
            ParseError::InvalidVersion
        );
        assert_eq!(
            parse_one(b"GET / HTTP/1.9\r\nHost: x\r\n\r\n").unwrap_err(),
            ParseError::InvalidVersion
        );
    }

    #[test]
    fn rejects_malformed_request_lines() {
        for line in [
            b"GET / HTTP/1.1 extra\r\n".as_slice(),
            b"GET /\r\n".as_slice(),
            b"GET /  HTTP/1.1\r\n".as_slice(),
            b"  GET / HTTP/1.1\r\n".as_slice(),
            b"GET / HTTP/1.1 \r\n".as_slice(),
        ] {
            let mut head = line.to_vec();
            head.extend_from_slice(b"Host: x\r\n\r\n");
            assert_eq!(
                parse_one(&head).unwrap_err(),
                ParseError::InvalidRequestLine,
                "line={line:?}"
            );
        }
    }

    #[test]
    fn rejects_an_empty_request_line() {
        assert_eq!(
            parse_one(b"\r\nGET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap_err(),
            ParseError::EmptyRequestLine
        );
    }

    #[test]
    fn asterisk_form_is_options_only() {
        let request = parse_one(b"OPTIONS * HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(request.method, Method::Options);
        assert_eq!(request.target, b"*");
        assert_eq!(
            parse_one(b"GET * HTTP/1.1\r\nHost: x\r\n\r\n").unwrap_err(),
            ParseError::InvalidTarget
        );
    }

    #[test]
    fn connect_uses_authority_form() {
        let request =
            parse_one(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
                .unwrap();
        assert_eq!(request.method, Method::Connect);
        assert_eq!(request.target, b"example.com:443");
        assert_eq!(
            parse_one(b"CONNECT example.com/path HTTP/1.1\r\nHost: x\r\n\r\n").unwrap_err(),
            ParseError::InvalidTarget
        );
    }

    #[test]
    fn absolute_form_is_accepted_and_extension_methods_kept() {
        let request =
            parse_one(b"GET http://example.com/path HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(request.target, b"http://example.com/path");
        let propfind = parse_one(b"PROPFIND /dav HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(propfind.method, Method::extension("PROPFIND".into()));
    }

    #[test]
    fn feeds_incrementally_and_buffers_pending() {
        let mut parser = RequestParser::new();
        let head = b"POST /x HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\n\r\nbody";
        let mut request = None;
        for &byte in head {
            match parser.feed(&[byte]) {
                FeedResult::Incomplete => {}
                FeedResult::Complete(parsed) => request = Some(parsed),
                FeedResult::Error(error) => panic!("unexpected error {error:?}"),
            }
        }
        let request = request.expect("head never completed");
        assert_eq!(request.framing, BodyFraming::Length(3));
        assert_eq!(parser.drain_pending(), b"body");
    }

    #[test]
    fn handles_pipelined_requests() {
        let mut parser = RequestParser::new();
        let two = b"GET /a HTTP/1.1\r\nHost: x\r\n\r\nGET /b HTTP/1.1\r\nHost: y\r\n\r\n";
        let first = match parser.feed(two) {
            FeedResult::Complete(request) => request,
            other => panic!("{other:?}"),
        };
        assert_eq!(first.target, b"/a");
        let pending = parser.drain_pending();
        let second = match parser.feed(&pending) {
            FeedResult::Complete(request) => request,
            other => panic!("{other:?}"),
        };
        assert_eq!(second.target, b"/b");
        assert_eq!(second.host(), Some(&b"y"[..]));
    }

    #[test]
    fn enforces_max_headers() {
        let mut parser = RequestParser::new_with_limits(RequestLimits::small());
        let mut head = b"GET / HTTP/1.1\r\nHost: x\r\n".to_vec();
        for i in 0..8 {
            head.extend_from_slice(format!("X-H{i}: v\r\n").as_bytes());
        }
        head.extend_from_slice(b"\r\n");
        assert_eq!(
            parser.feed(&head),
            FeedResult::Error(ParseError::TooManyHeaders)
        );
    }

    #[test]
    fn enforces_request_line_limit_incrementally() {
        let mut parser = RequestParser::new_with_limits(RequestLimits::small());
        let mut partial = b"GET /".to_vec();
        partial.extend(std::iter::repeat_n(b'a', 80));
        assert_eq!(
            parser.feed(&partial),
            FeedResult::Error(ParseError::RequestLineTooLong)
        );
    }

    #[test]
    fn enforces_header_value_limit_incrementally() {
        let mut parser = RequestParser::new_with_limits(RequestLimits::small());
        assert_eq!(
            parser.feed(b"GET / HTTP/1.1\r\nHost: x\r\nX-V: "),
            FeedResult::Incomplete
        );
        let long_value = b"b".repeat(70);
        assert_eq!(
            parser.feed(&long_value),
            FeedResult::Error(ParseError::HeaderValueTooLong)
        );
    }

    #[test]
    fn enforces_head_size_limit() {
        let mut parser = RequestParser::new_with_limits(RequestLimits::small());
        let mut head = b"GET / HTTP/1.1\r\nHost: x\r\n".to_vec();
        while head.len() < 400 {
            head.extend_from_slice(b"X-Filler: ");
            head.extend(std::iter::repeat_n(b'z', 40));
            head.extend_from_slice(b"\r\n");
        }
        head.extend_from_slice(b"\r\n");
        assert_eq!(
            parser.feed(&head),
            FeedResult::Error(ParseError::HeadTooLarge)
        );
    }

    #[test]
    fn enforces_target_limit() {
        let mut parser = RequestParser::new_with_limits(RequestLimits::small());
        let mut head = b"GET /".to_vec();
        head.extend(std::iter::repeat_n(b'a', 70));
        head.extend_from_slice(b" HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(
            parser.feed(&head),
            FeedResult::Error(ParseError::TargetTooLong)
        );
    }

    #[test]
    fn errors_map_to_status_codes() {
        assert_eq!(
            ParseError::InvalidVersion.status(),
            StatusCode::HTTP_VERSION_NOT_SUPPORTED
        );
        assert_eq!(ParseError::TargetTooLong.status(), StatusCode::URI_TOO_LONG);
        assert_eq!(
            ParseError::HeadTooLarge.status(),
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
        );
        assert_eq!(ParseError::MissingHost.status(), StatusCode::BAD_REQUEST);
    }
}
