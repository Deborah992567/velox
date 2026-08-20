//! Gateway protocols: `FastCGI`, SCGI, and uWSGI.
//!
//! Phase 13 adds protocol adapters that translate HTTP requests into
//! gateway-specific wire formats and relay the response back. Each adapter
//! implements the [`ProtocolAdapter`] trait, which abstracts the
//! connect -> send request -> read response flow so the proxy exchange can
//! target application servers speaking any of these protocols.
//!
//! The adapters own their connection management and can be used standalone
//! or plugged into the proxy layer via [`super::exchange`].

pub mod fastcgi;
pub mod scgi;
pub mod uwsgi;

use std::io::{self, Read, Write};

/// The role assigned to a `FastCGI` responder (also used conceptually for
/// SCGI/uWSGI which are always "responder" role).
pub const ROLE_RESPONDER: u16 = 1;

/// Outcome of a gateway exchange: the HTTP status code to send back to the
/// client and the raw response body (stdout from `FastCGI`, raw body from
/// SCGI/uWSGI).
#[derive(Debug)]
pub struct GatewayResponse {
    /// The HTTP status code the gateway produced (parsed from the response
    /// headers, or 200 by default).
    pub status: u16,
    /// The response body bytes (already de-chunked / de-framed).
    pub body: Vec<u8>,
}

/// A protocol adapter that speaks a gateway wire format to an application
/// server.
///
/// Implementations are responsible for:
/// 1. Encoding the HTTP request into the gateway protocol's request format.
/// 2. Sending it over the provided writer.
/// 3. Reading the gateway protocol's response and decoding it into an HTTP
///    response body + status code.
pub trait ProtocolAdapter {
    /// Encode and send the HTTP request to the upstream gateway, then read
    /// the full response back.
    ///
    /// `request_head` is the raw HTTP request line + headers (without the
    /// body). `request_body` is the complete request body (may be empty).
    /// `upstream` is the connected socket to the gateway process.
    fn exchange(
        &self,
        upstream: &mut dyn ReadWritePair,
        request_head: &[u8],
        request_body: &[u8],
    ) -> io::Result<GatewayResponse>;
}

/// A pair of reader + writer references, used to abstract over connections
/// that may or may not be the same object.
pub trait ReadWritePair: Read + Write {}

impl<T: Read + Write> ReadWritePair for T {}

/// Parse HTTP response headers from a raw byte buffer (the gateway's stdout)
/// into a status code and the remaining body bytes.
///
/// Expects the standard `HTTP/1.x STATUS Reason\r\nHeaders...\r\n\r\n`
/// format. If no valid head is found, defaults to 200 with the full buffer
/// as body.
fn parse_http_response(raw: &[u8]) -> GatewayResponse {
    let Some(head_end) = find_head_end(raw) else {
        return GatewayResponse {
            status: 200,
            body: raw.to_vec(),
        };
    };
    let head = &raw[..head_end];
    let body = &raw[head_end + 4..]; // skip \r\n\r\n

    let status = extract_status_code(head);

    GatewayResponse {
        status,
        body: body.to_vec(),
    }
}

/// Find the `\r\n\r\n` that terminates the HTTP response head.
fn find_head_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Extract the numeric status code from the first line of an HTTP response
/// head (`HTTP/1.1 200 OK`).
fn extract_status_code(head: &[u8]) -> u16 {
    let first_line_end = head
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(head.len());
    let first_line = &head[..first_line_end];
    // Skip "HTTP/1.x "
    let Some(code_start) = first_line.iter().position(|&b| b == b' ').map(|p| p + 1) else {
        return 200;
    };
    let code_bytes = &first_line[code_start..];
    let code_end = code_bytes.iter().position(|&b| b == b' ' || b == b'\r');
    let code_end = code_end.unwrap_or(code_bytes.len());
    let code_str = std::str::from_utf8(&code_bytes[..code_end]).unwrap_or("200");
    code_str.parse().unwrap_or(200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_response_200() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        let resp = parse_http_response(raw);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"hello");
    }

    #[test]
    fn parse_http_response_404() {
        let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        let resp = parse_http_response(raw);
        assert_eq!(resp.status, 404);
        assert!(resp.body.is_empty());
    }

    #[test]
    fn parse_http_response_no_head() {
        let raw = b"just raw body bytes";
        let resp = parse_http_response(raw);
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"just raw body bytes");
    }

    #[test]
    fn extract_status_code_from_standard_line() {
        let head = b"HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain\r\n";
        assert_eq!(extract_status_code(head), 502);
    }

    #[test]
    fn extract_status_code_minimal() {
        let head = b"HTTP/1.1 204 No Content\r\n";
        assert_eq!(extract_status_code(head), 204);
    }

    #[test]
    fn find_head_end_present() {
        let data = b"HTTP/1.1 200 OK\r\n\r\nbody";
        assert_eq!(find_head_end(data), Some(15));
    }

    #[test]
    fn find_head_end_absent() {
        let data = b"no headers here";
        assert_eq!(find_head_end(data), None);
    }
}
