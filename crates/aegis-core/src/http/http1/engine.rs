//! Injection-safe HTTP/1.x response encoder.
//!
//! The encoder turns a [`Response`] head into wire bytes. Injection safety is
//! the point: a header name or value that could terminate the head is a
//! response-splitting vector (architecture §11), so every field is validated
//! before it is written and a control byte is an error, never a passthrough.
//! The body framing header is derived from the caller's [`BodyFraming`]
//! choice and suppressed for statuses that cannot carry a body (1xx, 204,
//! 304). Chunked bodies are assembled with the [`encode_chunk`] /
//! [`encode_last_chunk`] helpers so the connection manager can stream frames
//! without ever holding a whole body.

use super::validate_field_value;
use crate::http::{BodyFraming, Headers, Request, Response, is_tchar};

/// Why a response head could not be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// A header field name is not a legal RFC 9110 field-name.
    InvalidName,
    /// A header field value contains a control byte other than HTAB — which
    /// includes CR/LF, i.e. an attempted response split.
    InvalidValue,
}

/// Encode `response`'s head — status line, header fields, framing header, and
/// the terminating blank line — into `out`.
pub fn encode_head(
    response: &Response,
    framing: BodyFraming,
    out: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    out.extend_from_slice(
        format!(
            "{} {} {}\r\n",
            response.version.as_str(),
            response.status.code(),
            response.status.reason_phrase()
        )
        .as_bytes(),
    );
    encode_headers(out, &response.headers)?;
    let framing = if status_allows_body(response.status) {
        framing
    } else {
        BodyFraming::None
    };
    match framing {
        BodyFraming::None => {}
        BodyFraming::Length(len) => {
            out.extend_from_slice(format!("content-length: {len}\r\n").as_bytes());
        }
        BodyFraming::Chunked => out.extend_from_slice(b"transfer-encoding: chunked\r\n"),
    }
    out.extend_from_slice(b"\r\n");
    Ok(())
}

/// Encode one chunked frame: `hex-size CRLF data CRLF`.
pub fn encode_chunk(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(format!("{:x}\r\n", data.len()).as_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
}

/// Encode a request head — request line, header fields, framing header, and
/// the terminating blank line — into `out`.
///
/// The framing header (`Content-Length`/`Transfer-Encoding`) is derived from
/// [`Request::framing`]; callers must therefore strip those two fields from
/// `request.headers` before encoding (as the proxy's rewrite step does) or
/// the body would be declared twice.
pub fn encode_request_head(request: &Request, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    out.extend_from_slice(request.method.as_bytes());
    out.extend_from_slice(b" ");
    out.extend_from_slice(&request.target);
    out.extend_from_slice(b" ");
    out.extend_from_slice(request.version.as_str().as_bytes());
    out.extend_from_slice(b"\r\n");
    encode_headers(out, &request.headers)?;
    match request.framing {
        BodyFraming::None => {}
        BodyFraming::Length(len) => {
            out.extend_from_slice(format!("content-length: {len}\r\n").as_bytes());
        }
        BodyFraming::Chunked => out.extend_from_slice(b"transfer-encoding: chunked\r\n"),
    }
    out.extend_from_slice(b"\r\n");
    Ok(())
}

/// Encode the terminating `0` chunk plus an optional trailer block.
pub fn encode_last_chunk(out: &mut Vec<u8>, trailers: &Headers) -> Result<(), EncodeError> {
    out.extend_from_slice(b"0\r\n");
    encode_headers(out, trailers)?;
    out.extend_from_slice(b"\r\n");
    Ok(())
}

fn encode_headers(out: &mut Vec<u8>, headers: &Headers) -> Result<(), EncodeError> {
    for header in headers.iter() {
        let name = header.name.as_str().as_bytes();
        if !name.iter().all(|&b| is_tchar(b)) {
            return Err(EncodeError::InvalidName);
        }
        validate_field_value(&header.value).map_err(|_| EncodeError::InvalidValue)?;
        out.extend_from_slice(name);
        out.extend_from_slice(b": ");
        out.extend_from_slice(&header.value);
        out.extend_from_slice(b"\r\n");
    }
    Ok(())
}

/// Whether a status code may carry a message body: not 1xx, 204, or 304.
pub(crate) const fn status_allows_body(status: crate::http::StatusCode) -> bool {
    !(status.code() < 200 || status.code() == 204 || status.code() == 304)
}

#[cfg(test)]
mod tests {
    use super::{EncodeError, encode_chunk, encode_head, encode_last_chunk, encode_request_head};
    use crate::http::{
        BodyFraming, Header, HeaderName, Headers, Method, Request, Response, StatusCode, Version,
    };

    #[test]
    fn encodes_a_simple_response() {
        let mut response = Response::new(Version::Http11, StatusCode::OK);
        response.header(HeaderName::ContentType, "text/plain");
        let mut out = Vec::new();
        encode_head(&response, BodyFraming::None, &mut out).unwrap();
        assert_eq!(out, b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\n\r\n");
    }

    #[test]
    fn adds_content_length_header() {
        let response = Response::new(Version::Http11, StatusCode::OK);
        let mut out = Vec::new();
        encode_head(&response, BodyFraming::Length(42), &mut out).unwrap();
        assert_eq!(out, b"HTTP/1.1 200 OK\r\ncontent-length: 42\r\n\r\n");
    }

    #[test]
    fn adds_chunked_header() {
        let response = Response::new(Version::Http11, StatusCode::OK);
        let mut out = Vec::new();
        encode_head(&response, BodyFraming::Chunked, &mut out).unwrap();
        assert_eq!(
            out,
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n"
        );
    }

    #[test]
    fn http10_status_line() {
        let response = Response::new(Version::Http10, StatusCode::NOT_FOUND);
        let mut out = Vec::new();
        encode_head(&response, BodyFraming::None, &mut out).unwrap();
        assert_eq!(out, b"HTTP/1.0 404 Not Found\r\n\r\n");
    }

    #[test]
    fn suppresses_framing_for_bodyless_statuses() {
        for status in [
            StatusCode::CONTINUE,
            StatusCode::NO_CONTENT,
            StatusCode::NOT_MODIFIED,
        ] {
            let response = Response::new(Version::Http11, status);
            let mut out = Vec::new();
            encode_head(&response, BodyFraming::Length(10), &mut out).unwrap();
            let head = String::from_utf8_lossy(&out);
            assert!(!head.contains("content-length"), "{status}");
            assert!(!head.contains("transfer-encoding"), "{status}");
        }
    }

    #[test]
    fn rejects_response_splitting() {
        let mut response = Response::new(Version::Http11, StatusCode::OK);
        response.headers.push_value(
            HeaderName::Custom("x-evil".into()),
            "a\r\nContent-Length: 0",
        );
        let mut out = Vec::new();
        assert_eq!(
            encode_head(&response, BodyFraming::None, &mut out),
            Err(EncodeError::InvalidValue)
        );
    }

    #[test]
    fn rejects_bad_header_names() {
        let mut response = Response::new(Version::Http11, StatusCode::OK);
        response.headers.push(Header::new(
            HeaderName::Custom("bad name".into()),
            b"x".to_vec(),
        ));
        let mut out = Vec::new();
        assert_eq!(
            encode_head(&response, BodyFraming::None, &mut out),
            Err(EncodeError::InvalidName)
        );
    }

    #[test]
    fn encodes_chunk_frames() {
        let mut out = Vec::new();
        encode_chunk(&mut out, b"hello");
        assert_eq!(out, b"5\r\nhello\r\n");
    }

    #[test]
    fn encodes_last_chunk_with_trailers() {
        let mut trailers = Headers::new();
        trailers.push_value(HeaderName::Etag, "abc");
        let mut out = Vec::new();
        encode_last_chunk(&mut out, &trailers).unwrap();
        assert_eq!(out, b"0\r\netag: abc\r\n\r\n");
    }

    #[test]
    fn encodes_a_simple_request() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        headers.push_value(HeaderName::ContentType, "text/plain");
        let request = Request::new(
            Method::Get,
            b"/index.html".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        );
        let mut out = Vec::new();
        encode_request_head(&request, &mut out).unwrap();
        assert_eq!(
            out,
            b"GET /index.html HTTP/1.1\r\nhost: example.com\r\ncontent-type: text/plain\r\n\r\n"
        );
    }

    #[test]
    fn encodes_request_framing_header() {
        let mut length = Request::new(
            Method::Post,
            b"/up".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::Length(12),
        );
        let mut out = Vec::new();
        encode_request_head(&length, &mut out).unwrap();
        assert!(out.ends_with(b"\r\ncontent-length: 12\r\n\r\n"));

        length.framing = BodyFraming::Chunked;
        let mut out = Vec::new();
        encode_request_head(&length, &mut out).unwrap();
        assert!(out.ends_with(b"\r\ntransfer-encoding: chunked\r\n\r\n"));
    }

    #[test]
    fn rejects_splitting_via_request_headers() {
        let mut headers = Headers::new();
        headers.push_value(
            HeaderName::Custom("x-evil".into()),
            "a\r\nContent-Length: 0",
        );
        let request = Request::new(
            Method::Get,
            b"/".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        );
        let mut out = Vec::new();
        assert_eq!(
            encode_request_head(&request, &mut out),
            Err(EncodeError::InvalidValue)
        );
    }
}
