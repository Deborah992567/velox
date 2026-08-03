//! HTTP/1.x shared core.
//!
//! Type-level building blocks shared by the parser ([`crate::http::http1`]),
//! the response engine, and later the HTTP/2 and HTTP/3 stacks: methods,
//! versions, status codes, header names/values, and the parsed request.
//!
//! Everything here is pure data with no I/O. The incremental wire protocol is
//! implemented in [`crate::http::http1`]; this module only defines the
//! in-memory model.

use std::fmt;

pub mod limits;

/// An HTTP request method (RFC 9110 §9.1).
///
/// The nine standard methods are modelled as fixed variants; any other legal
/// method token (e.g. `PROPFIND`, `BREW`) is preserved verbatim in
/// [`Method::Extension`] so routing can match it without losing the token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`
    Get,
    /// `HEAD`
    Head,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `PATCH`
    Patch,
    /// `DELETE`
    Delete,
    /// `OPTIONS`
    Options,
    /// `CONNECT`
    Connect,
    /// `TRACE`
    Trace,
    /// Any other RFC 9110 method token, stored exactly as received.
    Extension(Box<str>),
}

impl Method {
    /// Parse a method token into the standard set, or `None` for extensions.
    ///
    /// The caller must have already validated the token (see
    /// [`crate::http::http1::parser`]); this only classifies it.
    pub fn parse(token: &[u8]) -> Option<Self> {
        match token {
            b"GET" => Some(Self::Get),
            b"HEAD" => Some(Self::Head),
            b"POST" => Some(Self::Post),
            b"PUT" => Some(Self::Put),
            b"PATCH" => Some(Self::Patch),
            b"DELETE" => Some(Self::Delete),
            b"OPTIONS" => Some(Self::Options),
            b"CONNECT" => Some(Self::Connect),
            b"TRACE" => Some(Self::Trace),
            _ => None,
        }
    }

    /// Wrap a validated extension token.
    pub const fn extension(token: Box<str>) -> Self {
        Self::Extension(token)
    }

    /// The method as bytes, in canonical (uppercase) form for standard
    /// methods and verbatim for extensions.
    pub fn as_bytes(&self) -> &[u8] {
        self.as_str().as_bytes()
    }

    /// The method as a string, in canonical (uppercase) form for standard
    /// methods and verbatim for extensions.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Connect => "CONNECT",
            Self::Trace => "TRACE",
            Self::Extension(token) => token,
        }
    }

    /// Whether this is one of the nine standard methods.
    pub const fn is_standard(&self) -> bool {
        !matches!(self, Self::Extension(_))
    }

    /// Whether a response to this method carries a message body by default.
    ///
    /// `HEAD` responses never have a body; the request methods with defined
    /// request bodies (`POST`/`PUT`/`PATCH`) still require an explicit
    /// `Content-Length` or `Transfer-Encoding` to carry one.
    pub fn response_has_no_body(self) -> bool {
        matches!(self, Self::Head)
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The HTTP protocol version on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
    /// `HTTP/1.0`
    Http10,
    /// `HTTP/1.1`
    Http11,
}

impl Version {
    /// The wire form, e.g. `HTTP/1.1`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
        }
    }

    /// Parse an exact version token (`HTTP/1.0` or `HTTP/1.1`).
    pub fn parse(token: &[u8]) -> Option<Self> {
        match token {
            b"HTTP/1.0" => Some(Self::Http10),
            b"HTTP/1.1" => Some(Self::Http11),
            _ => None,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An HTTP status code with its canonical reason phrase.
///
/// A transparent `u16` wrapper so arbitrary upstream codes round-trip while
/// the well-known codes get typed constants and category helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusCode {
    code: u16,
}

impl StatusCode {
    /// `100 Continue`
    pub const CONTINUE: Self = Self::new(100);
    /// `101 Switching Protocols`
    pub const SWITCHING_PROTOCOLS: Self = Self::new(101);
    /// `102 Processing`
    pub const PROCESSING: Self = Self::new(102);
    /// `200 OK`
    pub const OK: Self = Self::new(200);
    /// `201 Created`
    pub const CREATED: Self = Self::new(201);
    /// `202 Accepted`
    pub const ACCEPTED: Self = Self::new(202);
    /// `203 Non-Authoritative Information`
    pub const NON_AUTHORITATIVE_INFORMATION: Self = Self::new(203);
    /// `204 No Content`
    pub const NO_CONTENT: Self = Self::new(204);
    /// `205 Reset Content`
    pub const RESET_CONTENT: Self = Self::new(205);
    /// `206 Partial Content`
    pub const PARTIAL_CONTENT: Self = Self::new(206);
    /// `300 Multiple Choices`
    pub const MULTIPLE_CHOICES: Self = Self::new(300);
    /// `301 Moved Permanently`
    pub const MOVED_PERMANENTLY: Self = Self::new(301);
    /// `302 Found`
    pub const FOUND: Self = Self::new(302);
    /// `303 See Other`
    pub const SEE_OTHER: Self = Self::new(303);
    /// `304 Not Modified`
    pub const NOT_MODIFIED: Self = Self::new(304);
    /// `307 Temporary Redirect`
    pub const TEMPORARY_REDIRECT: Self = Self::new(307);
    /// `308 Permanent Redirect`
    pub const PERMANENT_REDIRECT: Self = Self::new(308);
    /// `400 Bad Request`
    pub const BAD_REQUEST: Self = Self::new(400);
    /// `401 Unauthorized`
    pub const UNAUTHORIZED: Self = Self::new(401);
    /// `402 Payment Required`
    pub const PAYMENT_REQUIRED: Self = Self::new(402);
    /// `403 Forbidden`
    pub const FORBIDDEN: Self = Self::new(403);
    /// `404 Not Found`
    pub const NOT_FOUND: Self = Self::new(404);
    /// `405 Method Not Allowed`
    pub const METHOD_NOT_ALLOWED: Self = Self::new(405);
    /// `406 Not Acceptable`
    pub const NOT_ACCEPTABLE: Self = Self::new(406);
    /// `407 Proxy Authentication Required`
    pub const PROXY_AUTHENTICATION_REQUIRED: Self = Self::new(407);
    /// `408 Request Timeout`
    pub const REQUEST_TIMEOUT: Self = Self::new(408);
    /// `409 Conflict`
    pub const CONFLICT: Self = Self::new(409);
    /// `410 Gone`
    pub const GONE: Self = Self::new(410);
    /// `411 Length Required`
    pub const LENGTH_REQUIRED: Self = Self::new(411);
    /// `412 Precondition Failed`
    pub const PRECONDITION_FAILED: Self = Self::new(412);
    /// `413 Content Too Large`
    pub const CONTENT_TOO_LARGE: Self = Self::new(413);
    /// `414 URI Too Long`
    pub const URI_TOO_LONG: Self = Self::new(414);
    /// `415 Unsupported Media Type`
    pub const UNSUPPORTED_MEDIA_TYPE: Self = Self::new(415);
    /// `416 Range Not Satisfiable`
    pub const RANGE_NOT_SATISFIABLE: Self = Self::new(416);
    /// `417 Expectation Failed`
    pub const EXPECTATION_FAILED: Self = Self::new(417);
    /// `421 Misdirected Request`
    pub const MISDIRECTED_REQUEST: Self = Self::new(421);
    /// `422 Unprocessable Content`
    pub const UNPROCESSABLE_CONTENT: Self = Self::new(422);
    /// `425 Too Early`
    pub const TOO_EARLY: Self = Self::new(425);
    /// `426 Upgrade Required`
    pub const UPGRADE_REQUIRED: Self = Self::new(426);
    /// `428 Precondition Required`
    pub const PRECONDITION_REQUIRED: Self = Self::new(428);
    /// `429 Too Many Requests`
    pub const TOO_MANY_REQUESTS: Self = Self::new(429);
    /// `431 Request Header Fields Too Large`
    pub const REQUEST_HEADER_FIELDS_TOO_LARGE: Self = Self::new(431);
    /// `500 Internal Server Error`
    pub const INTERNAL_SERVER_ERROR: Self = Self::new(500);
    /// `501 Not Implemented`
    pub const NOT_IMPLEMENTED: Self = Self::new(501);
    /// `502 Bad Gateway`
    pub const BAD_GATEWAY: Self = Self::new(502);
    /// `503 Service Unavailable`
    pub const SERVICE_UNAVAILABLE: Self = Self::new(503);
    /// `504 Gateway Timeout`
    pub const GATEWAY_TIMEOUT: Self = Self::new(504);
    /// `505 HTTP Version Not Supported`
    pub const HTTP_VERSION_NOT_SUPPORTED: Self = Self::new(505);
    /// `507 Insufficient Storage`
    pub const INSUFFICIENT_STORAGE: Self = Self::new(507);
    /// `511 Network Authentication Required`
    pub const NETWORK_AUTHENTICATION_REQUIRED: Self = Self::new(511);

    /// Wrap an arbitrary numeric code.
    pub const fn new(code: u16) -> Self {
        Self { code }
    }

    /// The numeric code.
    pub const fn code(self) -> u16 {
        self.code
    }

    /// Whether the code is in the 2xx range.
    pub const fn is_success(self) -> bool {
        self.code >= 200 && self.code < 300
    }

    /// Whether the code is in the 3xx range.
    pub const fn is_redirection(self) -> bool {
        self.code >= 300 && self.code < 400
    }

    /// Whether the code is in the 4xx range.
    pub const fn is_client_error(self) -> bool {
        self.code >= 400 && self.code < 500
    }

    /// Whether the code is in the 5xx range.
    pub const fn is_server_error(self) -> bool {
        self.code >= 500 && self.code < 600
    }

    /// The canonical reason phrase, or `Unknown` for unregistered codes.
    pub const fn reason_phrase(self) -> &'static str {
        match self.code {
            100 => "Continue",
            101 => "Switching Protocols",
            102 => "Processing",
            200 => "OK",
            201 => "Created",
            202 => "Accepted",
            203 => "Non-Authoritative Information",
            204 => "No Content",
            205 => "Reset Content",
            206 => "Partial Content",
            300 => "Multiple Choices",
            301 => "Moved Permanently",
            302 => "Found",
            303 => "See Other",
            304 => "Not Modified",
            307 => "Temporary Redirect",
            308 => "Permanent Redirect",
            400 => "Bad Request",
            401 => "Unauthorized",
            402 => "Payment Required",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            406 => "Not Acceptable",
            407 => "Proxy Authentication Required",
            408 => "Request Timeout",
            409 => "Conflict",
            410 => "Gone",
            411 => "Length Required",
            412 => "Precondition Failed",
            413 => "Content Too Large",
            414 => "URI Too Long",
            415 => "Unsupported Media Type",
            416 => "Range Not Satisfiable",
            417 => "Expectation Failed",
            421 => "Misdirected Request",
            422 => "Unprocessable Content",
            425 => "Too Early",
            426 => "Upgrade Required",
            428 => "Precondition Required",
            429 => "Too Many Requests",
            431 => "Request Header Fields Too Large",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Timeout",
            505 => "HTTP Version Not Supported",
            507 => "Insufficient Storage",
            511 => "Network Authentication Required",
            _ => "Unknown",
        }
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.code, self.reason_phrase())
    }
}

/// A header field name, normalized to lowercase at parse time.
///
/// Well-known names are fixed variants for O(1) lookup; anything else is
/// preserved in [`HeaderName::Custom`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HeaderName {
    /// `host`
    Host,
    /// `connection`
    Connection,
    /// `content-length`
    ContentLength,
    /// `transfer-encoding`
    TransferEncoding,
    /// `expect`
    Expect,
    /// `content-type`
    ContentType,
    /// `content-encoding`
    ContentEncoding,
    /// `accept`
    Accept,
    /// `accept-encoding`
    AcceptEncoding,
    /// `accept-language`
    AcceptLanguage,
    /// `user-agent`
    UserAgent,
    /// `referer`
    Referer,
    /// `authorization`
    Authorization,
    /// `proxy-authorization`
    ProxyAuthorization,
    /// `cookie`
    Cookie,
    /// `set-cookie`
    SetCookie,
    /// `location`
    Location,
    /// `server`
    Server,
    /// `date`
    Date,
    /// `cache-control`
    CacheControl,
    /// `etag`
    Etag,
    /// `if-match`
    IfMatch,
    /// `if-none-match`
    IfNoneMatch,
    /// `if-modified-since`
    IfModifiedSince,
    /// `if-unmodified-since`
    IfUnmodifiedSince,
    /// `if-range`
    IfRange,
    /// `range`
    Range,
    /// `accept-ranges`
    AcceptRanges,
    /// `content-range`
    ContentRange,
    /// `upgrade`
    Upgrade,
    /// `origin`
    Origin,
    /// `via`
    Via,
    /// `trailer`
    Trailer,
    /// `www-authenticate`
    WwwAuthenticate,
    /// `proxy-authenticate`
    ProxyAuthenticate,
    /// `strict-transport-security`
    StrictTransportSecurity,
    /// `content-security-policy`
    ContentSecurityPolicy,
    /// `pragma`
    Pragma,
    /// `expires`
    Expires,
    /// `keep-alive`
    KeepAlive,
    /// `x-forwarded-for`
    XForwardedFor,
    /// `x-forwarded-proto`
    XForwardedProto,
    /// `x-real-ip`
    XRealIp,
    /// Any other valid field name, lowercased.
    Custom(Box<str>),
}

impl HeaderName {
    /// Parse and normalize a field name, or `None` if it is not a legal
    /// RFC 9110 field-name (empty or containing non-token characters).
    pub fn parse(name: &[u8]) -> Option<Self> {
        if name.is_empty() || !name.iter().all(|&b| is_tchar(b)) {
            return None;
        }
        let mut lower = Vec::with_capacity(name.len());
        lower.extend(name.iter().map(u8::to_ascii_lowercase));
        let s = String::from_utf8(lower).ok()?;
        Some(match s.as_str() {
            "host" => Self::Host,
            "connection" => Self::Connection,
            "content-length" => Self::ContentLength,
            "transfer-encoding" => Self::TransferEncoding,
            "expect" => Self::Expect,
            "content-type" => Self::ContentType,
            "content-encoding" => Self::ContentEncoding,
            "accept" => Self::Accept,
            "accept-encoding" => Self::AcceptEncoding,
            "accept-language" => Self::AcceptLanguage,
            "user-agent" => Self::UserAgent,
            "referer" => Self::Referer,
            "authorization" => Self::Authorization,
            "proxy-authorization" => Self::ProxyAuthorization,
            "cookie" => Self::Cookie,
            "set-cookie" => Self::SetCookie,
            "location" => Self::Location,
            "server" => Self::Server,
            "date" => Self::Date,
            "cache-control" => Self::CacheControl,
            "etag" => Self::Etag,
            "if-match" => Self::IfMatch,
            "if-none-match" => Self::IfNoneMatch,
            "if-modified-since" => Self::IfModifiedSince,
            "if-unmodified-since" => Self::IfUnmodifiedSince,
            "if-range" => Self::IfRange,
            "range" => Self::Range,
            "accept-ranges" => Self::AcceptRanges,
            "content-range" => Self::ContentRange,
            "upgrade" => Self::Upgrade,
            "origin" => Self::Origin,
            "via" => Self::Via,
            "trailer" => Self::Trailer,
            "www-authenticate" => Self::WwwAuthenticate,
            "proxy-authenticate" => Self::ProxyAuthenticate,
            "strict-transport-security" => Self::StrictTransportSecurity,
            "content-security-policy" => Self::ContentSecurityPolicy,
            "pragma" => Self::Pragma,
            "expires" => Self::Expires,
            "keep-alive" => Self::KeepAlive,
            "x-forwarded-for" => Self::XForwardedFor,
            "x-forwarded-proto" => Self::XForwardedProto,
            "x-real-ip" => Self::XRealIp,
            _ => Self::Custom(s.into_boxed_str()),
        })
    }

    /// The wire form, always lowercase.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Host => "host",
            Self::Connection => "connection",
            Self::ContentLength => "content-length",
            Self::TransferEncoding => "transfer-encoding",
            Self::Expect => "expect",
            Self::ContentType => "content-type",
            Self::ContentEncoding => "content-encoding",
            Self::Accept => "accept",
            Self::AcceptEncoding => "accept-encoding",
            Self::AcceptLanguage => "accept-language",
            Self::UserAgent => "user-agent",
            Self::Referer => "referer",
            Self::Authorization => "authorization",
            Self::ProxyAuthorization => "proxy-authorization",
            Self::Cookie => "cookie",
            Self::SetCookie => "set-cookie",
            Self::Location => "location",
            Self::Server => "server",
            Self::Date => "date",
            Self::CacheControl => "cache-control",
            Self::Etag => "etag",
            Self::IfMatch => "if-match",
            Self::IfNoneMatch => "if-none-match",
            Self::IfModifiedSince => "if-modified-since",
            Self::IfUnmodifiedSince => "if-unmodified-since",
            Self::IfRange => "if-range",
            Self::Range => "range",
            Self::AcceptRanges => "accept-ranges",
            Self::ContentRange => "content-range",
            Self::Upgrade => "upgrade",
            Self::Origin => "origin",
            Self::Via => "via",
            Self::Trailer => "trailer",
            Self::WwwAuthenticate => "www-authenticate",
            Self::ProxyAuthenticate => "proxy-authenticate",
            Self::StrictTransportSecurity => "strict-transport-security",
            Self::ContentSecurityPolicy => "content-security-policy",
            Self::Pragma => "pragma",
            Self::Expires => "expires",
            Self::KeepAlive => "keep-alive",
            Self::XForwardedFor => "x-forwarded-for",
            Self::XForwardedProto => "x-forwarded-proto",
            Self::XRealIp => "x-real-ip",
            Self::Custom(name) => name,
        }
    }
}

impl fmt::Display for HeaderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One header field: normalized name plus raw value bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Normalized (lowercase) name.
    pub name: HeaderName,
    /// Raw value bytes, with surrounding whitespace stripped.
    pub value: Vec<u8>,
}

impl Header {
    /// Build a header from a name and value bytes.
    pub const fn new(name: HeaderName, value: Vec<u8>) -> Self {
        Self { name, value }
    }

    /// The value interpreted as UTF-8, if it is valid UTF-8.
    pub fn value_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.value).ok()
    }
}

/// An ordered, duplicate-preserving set of header fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<Header>,
}

impl Headers {
    /// An empty header set.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// The number of header fields.
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no header fields are present.
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Append a header field.
    pub fn push(&mut self, header: Header) {
        self.entries.push(header);
    }

    /// Append a header field from a name and value bytes.
    pub fn push_value(&mut self, name: HeaderName, value: impl AsRef<[u8]>) {
        self.entries
            .push(Header::new(name, value.as_ref().to_vec()));
    }

    /// Iterate over all header fields in order.
    pub fn iter(&self) -> impl Iterator<Item = &Header> {
        self.entries.iter()
    }

    /// The first value for `name`, if present.
    pub fn get(&self, name: &HeaderName) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|h| &h.name == name)
            .map(|h| h.value.as_slice())
    }

    /// The first value for `name` as UTF-8, if present and valid.
    pub fn get_str(&self, name: &HeaderName) -> Option<&str> {
        self.get(name).and_then(|v| std::str::from_utf8(v).ok())
    }

    /// Every value for `name`, in order.
    pub fn get_all<'a>(&'a self, name: &'a HeaderName) -> impl Iterator<Item = &'a [u8]> + 'a {
        self.entries
            .iter()
            .filter(move |h| &h.name == name)
            .map(|h| h.value.as_slice())
    }

    /// Whether at least one field with `name` is present.
    pub fn contains(&self, name: &HeaderName) -> bool {
        self.get(name).is_some()
    }

    /// The declared body length: exactly one digits-only `Content-Length`, or
    /// `None` (including when the field is absent, malformed, or duplicated).
    pub fn content_length(&self) -> Option<u64> {
        let mut seen: Option<u64> = None;
        for value in self.get_all(&HeaderName::ContentLength) {
            let len = parse_decimal_u64(value)?;
            if seen.is_some() {
                return None;
            }
            seen = Some(len);
        }
        seen
    }

    /// Whether `Transfer-Encoding` names `chunked` (case-insensitively).
    pub fn transfer_encoding_chunked(&self) -> bool {
        self.get_str(&HeaderName::TransferEncoding)
            .is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
    }

    /// Whether the client expects `100 Continue` before a body.
    pub fn expect_continue(&self) -> bool {
        self.get_str(&HeaderName::Expect)
            .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
    }
}

impl IntoIterator for Headers {
    type Item = Header;
    type IntoIter = std::vec::IntoIter<Header>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

/// How the body of a message is delimited on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFraming {
    /// No body: neither `Content-Length` nor `Transfer-Encoding` present.
    None,
    /// Exactly `u64` bytes follow (`Content-Length`).
    Length(u64),
    /// Chunked transfer encoding.
    Chunked,
}

/// A parsed HTTP/1.x request head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The request method.
    pub method: Method,
    /// The raw request-target (origin-form, absolute-form, authority-form, or
    /// asterisk-form) exactly as received.
    pub target: Vec<u8>,
    /// The protocol version.
    pub version: Version,
    /// The header fields, in order.
    pub headers: Headers,
    /// How the request body is framed.
    pub framing: BodyFraming,
}

impl Request {
    /// Assemble a request.
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(
        method: Method,
        target: Vec<u8>,
        version: Version,
        headers: Headers,
        framing: BodyFraming,
    ) -> Self {
        Self {
            method,
            target,
            version,
            headers,
            framing,
        }
    }

    /// The path component of the request-target (up to the first `?`).
    pub fn path(&self) -> &[u8] {
        self.target
            .iter()
            .position(|&b| b == b'?')
            .map_or_else(|| &self.target[..], |i| &self.target[..i])
    }

    /// The query component of the request-target (after the first `?`).
    pub fn query(&self) -> Option<&[u8]> {
        self.target
            .iter()
            .position(|&b| b == b'?')
            .map(|i| &self.target[i + 1..])
    }

    /// The `Host` header value, if present.
    pub fn host(&self) -> Option<&[u8]> {
        self.headers.get(&HeaderName::Host)
    }

    /// Whether the client asked for `100 Continue` before sending a body.
    pub fn expects_continue(&self) -> bool {
        self.headers.expect_continue()
    }
}

/// Whether a byte is a valid RFC 9110 token character (`tchar`).
pub(crate) const fn is_tchar(b: u8) -> bool {
    matches!(
        b,
        b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'!'
            | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
    )
}

/// Parse a digits-only decimal string into a `u64`.
///
/// Returns `None` for empty input, non-digit bytes, leading `+`/`-`, and
/// overflow — used for strict `Content-Length` parsing where any deviation is
/// a request-smuggling vector.
pub(crate) fn parse_decimal_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut n: u64 = 0;
    for &b in bytes {
        n = n.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::{
        BodyFraming, Header, HeaderName, Headers, Method, Request, StatusCode, Version, is_tchar,
        parse_decimal_u64,
    };

    #[test]
    fn method_parse_roundtrips_standard_set() {
        for (token, method) in [
            (b"GET".as_slice(), Method::Get),
            (b"HEAD".as_slice(), Method::Head),
            (b"POST".as_slice(), Method::Post),
            (b"PUT".as_slice(), Method::Put),
            (b"PATCH".as_slice(), Method::Patch),
            (b"DELETE".as_slice(), Method::Delete),
            (b"OPTIONS".as_slice(), Method::Options),
            (b"CONNECT".as_slice(), Method::Connect),
            (b"TRACE".as_slice(), Method::Trace),
        ] {
            assert_eq!(Method::parse(token).as_ref(), Some(&method));
            assert_eq!(method.as_bytes(), token);
            assert!(method.is_standard());
        }
    }

    #[test]
    fn method_extension_preserves_token() {
        let ext = Method::parse(b"PROPFIND");
        assert_eq!(ext, None);
        let method = Method::extension("PROPFIND".into());
        assert_eq!(method.as_str(), "PROPFIND");
        assert!(!method.is_standard());
        assert_eq!(method.as_bytes(), b"PROPFIND");
    }

    #[test]
    fn version_parse_is_exact() {
        assert_eq!(Version::parse(b"HTTP/1.1"), Some(Version::Http11));
        assert_eq!(Version::parse(b"HTTP/1.0"), Some(Version::Http10));
        assert_eq!(Version::parse(b"HTTP/2.0"), None);
        assert_eq!(Version::parse(b"HTTP/1.1 "), None);
        assert_eq!(Version::as_str(Version::Http11), "HTTP/1.1");
    }

    #[test]
    fn status_codes_have_reason_phrases() {
        assert_eq!(StatusCode::OK.reason_phrase(), "OK");
        assert_eq!(StatusCode::NOT_FOUND.reason_phrase(), "Not Found");
        assert_eq!(
            StatusCode::CONTENT_TOO_LARGE.reason_phrase(),
            "Content Too Large"
        );
        assert_eq!(StatusCode::new(599).reason_phrase(), "Unknown");
        assert_eq!(StatusCode::CONTINUE.code(), 100);
        assert!(StatusCode::OK.is_success());
        assert!(StatusCode::NOT_FOUND.is_client_error());
        assert!(StatusCode::BAD_GATEWAY.is_server_error());
        assert!(StatusCode::MOVED_PERMANENTLY.is_redirection());
    }

    #[test]
    fn header_name_parse_normalizes_case() {
        assert_eq!(HeaderName::parse(b"Host"), Some(HeaderName::Host));
        assert_eq!(
            HeaderName::parse(b"CONTENT-LENGTH"),
            Some(HeaderName::ContentLength)
        );
        assert_eq!(
            HeaderName::parse(b"X-Custom-Thing"),
            Some(HeaderName::Custom("x-custom-thing".into()))
        );
        assert_eq!(
            HeaderName::parse(b"X-Custom-Thing").unwrap().as_str(),
            "x-custom-thing"
        );
        assert_eq!(HeaderName::parse(b""), None);
        assert_eq!(HeaderName::parse(b"Bad Name"), None);
        assert_eq!(HeaderName::parse(b"bad\x7fname"), None);
    }

    #[test]
    fn header_value_str_roundtrips_utf8() {
        let header = Header::new(
            HeaderName::ContentType,
            b"text/plain; charset=utf-8".to_vec(),
        );
        assert_eq!(header.value_str(), Some("text/plain; charset=utf-8"));
        let binary = Header::new(HeaderName::Custom("x-blob".into()), vec![0xff, 0x00]);
        assert_eq!(binary.value_str(), None);
    }

    #[test]
    fn headers_get_getall_and_duplicates() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Cookie, "a=1");
        headers.push_value(HeaderName::Cookie, "b=2");
        headers.push_value(HeaderName::Host, "example.com");
        assert!(!headers.is_empty());
        assert_eq!(headers.len(), 3);
        assert_eq!(headers.get(&HeaderName::Host), Some(&b"example.com"[..]));
        assert_eq!(headers.get(&HeaderName::Cookie), Some(&b"a=1"[..]));
        assert_eq!(
            headers.get_all(&HeaderName::Cookie).collect::<Vec<_>>(),
            vec![&b"a=1"[..], &b"b=2"[..]]
        );
        assert!(headers.contains(&HeaderName::Host));
        assert!(!headers.contains(&HeaderName::Server));
    }

    #[test]
    fn content_length_requires_single_digits_only_value() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::ContentLength, "5");
        assert_eq!(headers.content_length(), Some(5));

        let mut dup = Headers::new();
        dup.push_value(HeaderName::ContentLength, "5");
        dup.push_value(HeaderName::ContentLength, "5");
        assert_eq!(dup.content_length(), None);

        for bad in [
            b"".as_slice(),
            b"+5",
            b"5 ",
            b" 5",
            b"5,5",
            b"x5",
            b"18446744073709551616",
        ] {
            let mut h = Headers::new();
            h.push_value(HeaderName::ContentLength, bad);
            assert_eq!(h.content_length(), None, "must reject {bad:?}");
        }

        let mut huge = Headers::new();
        huge.push_value(HeaderName::ContentLength, "18446744073709551615");
        assert_eq!(huge.content_length(), Some(u64::MAX));
    }

    #[test]
    fn framing_helpers_detect_chunked_and_continue() {
        let mut h = Headers::new();
        h.push_value(HeaderName::TransferEncoding, "Chunked");
        assert!(h.transfer_encoding_chunked());
        let mut e = Headers::new();
        e.push_value(HeaderName::Expect, "100-continue");
        assert!(e.expect_continue());
        let mut n = Headers::new();
        n.push_value(HeaderName::Expect, "something-else");
        assert!(!n.expect_continue());
        assert!(!n.transfer_encoding_chunked());
    }

    #[test]
    fn request_splits_path_and_query() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        let request = Request::new(
            Method::Get,
            b"/a/b/c?x=1&y=%20".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        );
        assert_eq!(request.path(), b"/a/b/c");
        assert_eq!(request.query(), Some(&b"x=1&y=%20"[..]));
        assert_eq!(request.host(), Some(&b"example.com"[..]));
        assert!(!request.expects_continue());

        let no_query = Request::new(
            Method::Post,
            b"/".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::Length(0),
        );
        assert_eq!(no_query.path(), b"/");
        assert_eq!(no_query.query(), None);
    }

    #[test]
    fn tchar_table_is_exact() {
        assert!(is_tchar(b'a') && is_tchar(b'Z') && is_tchar(b'0'));
        assert!(is_tchar(b'!') && is_tchar(b'~') && is_tchar(b'^'));
        assert!(!is_tchar(b' ') && !is_tchar(b'\t') && !is_tchar(b'\r'));
        assert!(!is_tchar(b'\x7f') && !is_tchar(0xff) && !is_tchar(b'('));
    }

    #[test]
    fn decimal_parser_is_strict() {
        assert_eq!(parse_decimal_u64(b"0"), Some(0));
        assert_eq!(parse_decimal_u64(b"007"), Some(7));
        assert_eq!(
            parse_decimal_u64(b"12345678901234567890"),
            Some(12_345_678_901_234_567_890)
        );
        assert_eq!(parse_decimal_u64(b"18446744073709551616"), None);
        assert_eq!(parse_decimal_u64(b"12a"), None);
        assert_eq!(parse_decimal_u64(b"-1"), None);
        assert_eq!(parse_decimal_u64(b""), None);
    }
}
