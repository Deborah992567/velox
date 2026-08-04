//! Static file handler: turns a [`Request`] into a response.
//!
//! This is the orchestration layer that binds the Phase 6 pieces together.
//! Given a document root and a parsed request it decides what to serve:
//!
//! - method gate: only `GET`/`HEAD` are handled, anything else is `405`;
//! - the request-target is resolved through [`super::resolver`], which rejects
//!   traversal and malformed input with `403`/`400`;
//! - directories are redirected to a trailing-slash URL, served via an index
//!   file, or rendered as a listing ([`super::listing`]) when enabled;
//! - files are validated against `If-None-Match`/`If-Modified-Since`
//!   ([`super::validators`]) for `304`, and served as `200`, or as `206` with
//!   a single satisfiable byte range ([`super::range`]), with a
//!   [`StaticBody::File`] body ready for zero-copy `sendfile` transmission.
//!
//! The function is pure with respect to decision-making: it performs the file
//! system reads needed to classify the resource but returns the response head
//! and body description to the caller (the connection layer), which performs
//! the actual socket I/O.

use std::fs::File;
use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use crate::http::{BodyFraming, HeaderName, Method, Request, Response, StatusCode};

use super::date::format_http_date;
use super::listing::{list_directory, render_listing};
use super::mime::mime_type_for_path;
use super::range::{ByteRange, RangeResult, parse_range};
use super::resolver::{ResolveError, resolve};
use super::validators::{
    etag_for, if_modified_since_matches, if_none_match_matches, last_modified_for,
};

/// Configuration for the static file handler.
#[derive(Debug, Clone)]
pub struct StaticFileOptions {
    /// The document root; resolved paths are always confined to it.
    pub root: PathBuf,
    /// Index files tried in order for directory requests.
    pub index_files: Vec<PathBuf>,
    /// Whether to render an HTML listing when no index file matches.
    pub listing: bool,
}

impl Default for StaticFileOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            index_files: vec![PathBuf::from("index.html")],
            listing: false,
        }
    }
}

/// The body a static response carries, or a description of it.
#[derive(Debug)]
pub enum StaticBody {
    /// No body: only the head is transmitted (e.g. `HEAD`, `304`, `416`).
    None,
    /// Fully buffered bytes (error pages, directory listings).
    Bytes(Vec<u8>),
    /// A byte range of an already-open file, ready for zero-copy transfer.
    File { file: File, offset: u64, len: u64 },
}

impl StaticBody {
    /// The exact byte length of this body, if it has one.
    pub const fn content_length(&self) -> Option<u64> {
        match self {
            Self::None => None,
            Self::Bytes(bytes) => Some(bytes.len() as u64),
            Self::File { len, .. } => Some(*len),
        }
    }
}

/// The outcome of [`handle`]: a response head plus its body.
#[derive(Debug)]
pub struct StaticFileResponse {
    /// The response head, ready for [`http1::engine::encode_head`].
    pub response: Response,
    /// How the head frames the body.
    pub framing: BodyFraming,
    /// The body, if any.
    pub body: StaticBody,
}

/// Handle a request against the configured document root.
pub fn handle(request: &Request, options: &StaticFileOptions) -> StaticFileResponse {
    let mut response = base_response(request, StatusCode::OK);

    if !matches!(request.method, Method::Get | Method::Head) {
        return method_not_allowed(request, &mut response);
    }

    let resolved = match resolve(request.path(), &options.root) {
        Ok(path) => path,
        Err(ResolveError::EscapesRoot) => {
            return error_page(request, &mut response, StatusCode::FORBIDDEN);
        }
        Err(
            ResolveError::InvalidPercentEncoding
            | ResolveError::NotUtf8
            | ResolveError::InvalidCharacter,
        ) => {
            return error_page(request, &mut response, StatusCode::BAD_REQUEST);
        }
    };

    let metadata = match std::fs::metadata(&resolved) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return error_page(request, &mut response, StatusCode::NOT_FOUND);
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return error_page(request, &mut response, StatusCode::FORBIDDEN);
        }
        Err(_) => {
            return error_page(request, &mut response, StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    if metadata.is_dir() {
        return directory(request, options, &mut response, &resolved);
    }
    if !metadata.is_file() {
        return error_page(request, &mut response, StatusCode::NOT_FOUND);
    }

    let file = match File::open(&resolved) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return error_page(request, &mut response, StatusCode::FORBIDDEN);
        }
        Err(_) => {
            return error_page(request, &mut response, StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    file_response(request, &mut response, file, metadata, &resolved)
}

/// A base response carrying a `Date` header for every reply.
fn base_response(request: &Request, status: StatusCode) -> Response {
    let mut response = Response::new(request.version, status);
    response.header(
        HeaderName::Date,
        format_http_date(OffsetDateTime::now_utc()),
    );
    response
}

/// Serve a request that mapped to a directory.
fn directory(
    request: &Request,
    options: &StaticFileOptions,
    response: &mut Response,
    resolved: &Path,
) -> StaticFileResponse {
    if !request.path().ends_with(b"/") {
        return redirect_to_slash(request, response);
    }

    for index in &options.index_files {
        let candidate = resolved.join(index);
        match std::fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => {
                if let Ok(file) = File::open(&candidate) {
                    return file_response(request, response, file, metadata, &candidate);
                }
            }
            _ => {}
        }
    }

    if !options.listing {
        return error_page(request, response, StatusCode::FORBIDDEN);
    }

    let Ok(entries) = list_directory(resolved) else {
        return error_page(request, response, StatusCode::INTERNAL_SERVER_ERROR);
    };
    let url = std::str::from_utf8(request.path()).unwrap_or("/");
    let html = render_listing(url, &entries);
    response.header(HeaderName::ContentType, "text/html; charset=utf-8");
    let len = html.len() as u64;
    StaticFileResponse {
        framing: BodyFraming::Length(len),
        body: StaticBody::Bytes(html.into_bytes()),
        response: response.clone(),
    }
}

/// Redirect a directory request that lacks a trailing slash.
fn redirect_to_slash(request: &Request, response: &mut Response) -> StaticFileResponse {
    response.status = StatusCode::MOVED_PERMANENTLY;
    let mut location = Vec::with_capacity(request.path().len() + 1);
    location.extend_from_slice(request.path());
    location.push(b'/');
    if let Some(query) = request.query() {
        location.push(b'?');
        location.extend_from_slice(query);
    }
    response.header(HeaderName::Location, location);
    StaticFileResponse {
        framing: BodyFraming::None,
        body: StaticBody::None,
        response: response.clone(),
    }
}

/// Serve a regular file, honoring conditional requests and byte ranges.
fn file_response(
    request: &Request,
    response: &mut Response,
    file: File,
    metadata: std::fs::Metadata,
    path: &Path,
) -> StaticFileResponse {
    let etag = etag_for(&metadata);
    let last_modified = last_modified_for(&metadata);
    let length = metadata.len();

    response.header(HeaderName::Etag, &etag);
    response.header(HeaderName::AcceptRanges, "bytes");
    if let Some(last_modified) = last_modified {
        response.header(HeaderName::LastModified, format_http_date(last_modified));
    }

    if let Some(value) = request.headers.get(&HeaderName::IfNoneMatch) {
        if if_none_match_matches(value, &etag) {
            return not_modified(request, response);
        }
    } else if last_modified.is_some_and(|lm| {
        request
            .headers
            .get(&HeaderName::IfModifiedSince)
            .is_some_and(|v| if_modified_since_matches(v, lm))
    }) {
        return not_modified(request, response);
    }

    let head_requested = request.method == Method::Head;
    if !head_requested && let Some(value) = request.headers.get(&HeaderName::Range) {
        match parse_range(value, length) {
            RangeResult::Single(range) => {
                return partial_content(request, response, file, range, length, path);
            }
            RangeResult::Unsatisfiable => {
                response.header(HeaderName::ContentRange, format!("bytes */{length}"));
                return error_page(request, response, StatusCode::RANGE_NOT_SATISFIABLE);
            }
            RangeResult::Ignore | RangeResult::Multiple(_) => {}
        }
    }

    response.header(HeaderName::ContentType, mime_type_for_path(path));
    let framing = BodyFraming::Length(length);
    let body = if head_requested {
        StaticBody::None
    } else {
        StaticBody::File {
            file,
            offset: 0,
            len: length,
        }
    };
    StaticFileResponse {
        framing,
        body,
        response: response.clone(),
    }
}

/// A `304 Not Modified` reply carrying the validators that matched.
fn not_modified(_request: &Request, response: &mut Response) -> StaticFileResponse {
    response.status = StatusCode::NOT_MODIFIED;
    StaticFileResponse {
        framing: BodyFraming::None,
        body: StaticBody::None,
        response: response.clone(),
    }
}

/// A `206 Partial Content` reply for a single satisfiable range.
fn partial_content(
    _request: &Request,
    response: &mut Response,
    file: File,
    range: ByteRange,
    length: u64,
    path: &Path,
) -> StaticFileResponse {
    response.status = StatusCode::PARTIAL_CONTENT;
    response.header(HeaderName::ContentType, mime_type_for_path(path));
    response.header(
        HeaderName::ContentRange,
        format!("bytes {}-{}/{}", range.start, range.end, length),
    );
    StaticFileResponse {
        framing: BodyFraming::Length(range.len()),
        body: StaticBody::File {
            file,
            offset: range.start,
            len: range.len(),
        },
        response: response.clone(),
    }
}

/// A `405` for unsupported methods.
fn method_not_allowed(request: &Request, response: &mut Response) -> StaticFileResponse {
    response.status = StatusCode::METHOD_NOT_ALLOWED;
    response.header(HeaderName::Allow, "GET, HEAD");
    error_page(request, response, StatusCode::METHOD_NOT_ALLOWED)
}

/// A small HTML error page body.
fn error_page(
    _request: &Request,
    response: &mut Response,
    status: StatusCode,
) -> StaticFileResponse {
    response.status = status;
    response.header(HeaderName::ContentType, "text/html; charset=utf-8");
    let body = format!(
        "<!DOCTYPE html>\n<html><head><title>{} {}</title></head>\n<body><h1>{} {}</h1>\n<hr><p>velox</p></body></html>\n",
        status.code(),
        status.reason_phrase(),
        status.code(),
        status.reason_phrase(),
    );
    let len = body.len() as u64;
    StaticFileResponse {
        framing: BodyFraming::Length(len),
        body: StaticBody::Bytes(body.into_bytes()),
        response: response.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{StaticBody, StaticFileOptions, handle};
    use crate::http::http1::engine::encode_head;
    use crate::http::{BodyFraming, HeaderName, Headers, Method, Request, StatusCode, Version};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn request(method: Method, target: &[u8]) -> Request {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        Request::new(
            method,
            target.to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        )
    }

    fn setup() -> (PathBuf, StaticFileOptions) {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("aegis-handler-{}-{unique}", std::process::id()));
        let _ = std::fs::create_dir_all(root.join("sub"));
        std::fs::write(root.join("hello.txt"), b"hello world").expect("write");
        std::fs::write(root.join("sub").join("index.html"), b"<h1>sub index</h1>").expect("write");
        let options = StaticFileOptions {
            root: root.clone(),
            index_files: vec![PathBuf::from("index.html")],
            listing: false,
        };
        (root, options)
    }

    fn encode_head_text(response: &crate::http::Response, framing: BodyFraming) -> String {
        let mut out = Vec::new();
        encode_head(response, framing, &mut out).expect("encode");
        String::from_utf8(out).expect("utf8")
    }

    fn body_bytes(body: &StaticBody) -> Vec<u8> {
        match body {
            StaticBody::None => Vec::new(),
            StaticBody::Bytes(bytes) => bytes.clone(),
            StaticBody::File { file, offset, len } => {
                use std::io::{Read, Seek, SeekFrom};
                let mut bytes = vec![0u8; usize::try_from(*len).expect("len fits")];
                let mut file = file;
                file.seek(SeekFrom::Start(*offset)).expect("seek");
                file.read_exact(&mut bytes).expect("read");
                bytes
            }
        }
    }

    #[test]
    fn serves_a_file() {
        let (root, options) = setup();
        let result = handle(&request(Method::Get, b"/hello.txt"), &options);
        assert_eq!(result.response.status, StatusCode::OK);
        let head = encode_head_text(&result.response, result.framing);
        assert!(
            head.contains("content-type: text/plain; charset=utf-8"),
            "{head}"
        );
        assert!(head.contains("accept-ranges: bytes"), "{head}");
        assert!(head.contains("etag:"), "{head}");
        assert!(head.contains("content-length: 11"), "{head}");
        assert_eq!(body_bytes(&result.body), b"hello world");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_file_is_404() {
        let (root, options) = setup();
        let result = handle(&request(Method::Get, b"/nope.txt"), &options);
        assert_eq!(result.response.status, StatusCode::NOT_FOUND);
        assert!(!body_bytes(&result.body).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn traversal_is_forbidden() {
        let (root, options) = setup();
        let result = handle(&request(Method::Get, b"/../secret"), &options);
        assert_eq!(result.response.status, StatusCode::FORBIDDEN);
        let result = handle(&request(Method::Get, b"/%2e%2e/etc/passwd"), &options);
        assert_eq!(result.response.status, StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_targets_are_bad_requests() {
        let (root, options) = setup();
        let result = handle(&request(Method::Get, b"/a%ZZb"), &options);
        assert_eq!(result.response.status, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn conditional_requests_return_304() {
        let (root, options) = setup();
        let first = handle(&request(Method::Get, b"/hello.txt"), &options);
        let etag = first
            .response
            .headers
            .get_str(&HeaderName::Etag)
            .expect("etag")
            .to_string();

        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        headers.push_value(HeaderName::IfNoneMatch, &etag);
        let conditional = Request::new(
            Method::Get,
            b"/hello.txt".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        );
        let result = handle(&conditional, &options);
        assert_eq!(result.response.status, StatusCode::NOT_MODIFIED);
        assert!(matches!(result.body, StaticBody::None));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn single_byte_range_returns_206() {
        let (root, options) = setup();
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        headers.push_value(HeaderName::Range, "bytes=0-4");
        let ranged = Request::new(
            Method::Get,
            b"/hello.txt".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        );
        let result = handle(&ranged, &options);
        assert_eq!(result.response.status, StatusCode::PARTIAL_CONTENT);
        let head = encode_head_text(&result.response, result.framing);
        assert!(head.contains("content-range: bytes 0-4/11"), "{head}");
        assert!(head.contains("content-length: 5"), "{head}");
        assert_eq!(body_bytes(&result.body), b"hello");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsatisfiable_range_returns_416() {
        let (root, options) = setup();
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        headers.push_value(HeaderName::Range, "bytes=100-");
        let ranged = Request::new(
            Method::Get,
            b"/hello.txt".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        );
        let result = handle(&ranged, &options);
        assert_eq!(result.response.status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert!(
            result
                .response
                .headers
                .get_str(&HeaderName::ContentRange)
                .is_some_and(|v| v == "bytes */11")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn head_has_no_body() {
        let (root, options) = setup();
        let result = handle(&request(Method::Head, b"/hello.txt"), &options);
        assert_eq!(result.response.status, StatusCode::OK);
        assert!(matches!(result.body, StaticBody::None));
        assert_eq!(result.framing, BodyFraming::Length(11));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsupported_methods_are_405() {
        let (root, options) = setup();
        let result = handle(&request(Method::Post, b"/hello.txt"), &options);
        assert_eq!(result.response.status, StatusCode::METHOD_NOT_ALLOWED);
        assert!(
            result
                .response
                .headers
                .get_str(&HeaderName::Allow)
                .is_some_and(|v| v == "GET, HEAD")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn directory_redirects_to_trailing_slash() {
        let (root, options) = setup();
        let result = handle(&request(Method::Get, b"/sub"), &options);
        assert_eq!(result.response.status, StatusCode::MOVED_PERMANENTLY);
        assert!(
            result
                .response
                .headers
                .get_str(&HeaderName::Location)
                .is_some_and(|v| v == "/sub/")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn directory_serves_index_file() {
        let (root, options) = setup();
        let result = handle(&request(Method::Get, b"/sub/"), &options);
        assert_eq!(result.response.status, StatusCode::OK);
        assert_eq!(body_bytes(&result.body), b"<h1>sub index</h1>");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn directory_without_index_is_forbidden_unless_listing() {
        let (root, options) = setup();
        let result = handle(&request(Method::Get, b"/sub/"), &options);
        assert_eq!(result.response.status, StatusCode::OK); // index exists

        let mut no_index = StaticFileOptions {
            root: root.clone(),
            index_files: vec![PathBuf::from("index.html")],
            listing: false,
        };
        let _ = std::fs::remove_file(root.join("sub").join("index.html"));
        let result = handle(&request(Method::Get, b"/sub/"), &no_index);
        assert_eq!(result.response.status, StatusCode::FORBIDDEN);

        no_index.listing = true;
        let result = handle(&request(Method::Get, b"/sub/"), &no_index);
        assert_eq!(result.response.status, StatusCode::OK);
        let body = String::from_utf8(body_bytes(&result.body)).expect("utf8");
        assert!(body.contains("Index of /sub/"), "{body}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn error_page_uses_request_version() {
        let (root, options) = setup();
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        let http10 = Request::new(
            Method::Get,
            b"/nope.txt".to_vec(),
            Version::Http10,
            headers,
            BodyFraming::None,
        );
        let result = handle(&http10, &options);
        let head = encode_head_text(&result.response, result.framing);
        assert!(head.starts_with("HTTP/1.0 404"), "{head}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn date_header_is_present() {
        let (root, options) = setup();
        let result = handle(&request(Method::Get, b"/hello.txt"), &options);
        assert!(result.response.headers.contains(&HeaderName::Date));
        let _ = std::fs::remove_dir_all(&root);
    }
}
