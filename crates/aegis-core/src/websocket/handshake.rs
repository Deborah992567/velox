//! The RFC 6455 opening handshake (§4).
//!
//! A client request and the matching server response: the client sends a GET
//! with `Connection: Upgrade`, `Upgrade: websocket`, `Sec-WebSocket-Version:
//! 13`, and a `Sec-WebSocket-Key` (a base64-encoded 16-byte nonce); the server
//! answers `101 Switching Protocols` with an accept value computed as
//! base64(SHA-1(key + GUID)) (§1.3). Subprotocol and extension negotiation are
//! option-fields on top of that.
//!
//! [`is_websocket_upgrade`] classifies a parsed request head,
//! [`upgrade_response`] builds the server's `101`, [`client_request`] builds
//! the request for a client side (used by the Phase 12 proxy, which must
//! forward the client's key untouched, RFC 6455 §8.1.3), and
//! [`accept_key`]/[`validate_key`] expose the accept computation and the key's
//! 16-byte check.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha1::{Digest, Sha1};

use crate::http::{
    BodyFraming, HeaderName, Headers, Method, Request, Response, StatusCode, Version,
};

/// The GUID appended to the client key before the SHA-1 accept computation
/// (§1.3).
pub const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The only version of the handshake this phase speaks (§4.1.6).
pub const WEBSOCKET_VERSION: &str = "13";

/// Compute the `Sec-WebSocket-Accept` value for a client key:
/// base64(SHA-1(key + GUID)) (§4.2.2).
pub fn accept_key(key: &[u8]) -> String {
    let mut hash = Sha1::new();
    hash.update(key);
    hash.update(WEBSOCKET_GUID.as_bytes());
    BASE64.encode(hash.finalize())
}

/// Whether `key` is a legal `Sec-WebSocket-Key`: base64 that decodes to
/// exactly 16 bytes (§4.1).
pub fn validate_key(key: &[u8]) -> bool {
    BASE64.decode(key).is_ok_and(|decoded| decoded.len() == 16)
}

/// Whether a comma-separated header value contains `token`,
/// case-insensitively, with surrounding whitespace tolerated.
pub fn contains_token(value: &[u8], token: &str) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    value
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case(token))
}

/// Whether `request` is a WebSocket opening handshake: `GET`, HTTP/1.1,
/// `Connection: upgrade`, `Upgrade: websocket`, `Sec-WebSocket-Version: 13`,
/// and a valid `Sec-WebSocket-Key` (§4.1).
pub fn is_websocket_upgrade(request: &Request) -> bool {
    request.method == Method::Get
        && request.version == Version::Http11
        && request
            .headers
            .get(&HeaderName::Connection)
            .is_some_and(|value| contains_token(value, "upgrade"))
        && request
            .headers
            .get(&HeaderName::Upgrade)
            .is_some_and(|value| contains_token(value, "websocket"))
        && request
            .headers
            .get_str(&HeaderName::Custom("sec-websocket-version".into()))
            .is_some_and(|value| contains_token(value.as_bytes(), WEBSOCKET_VERSION))
        && request
            .headers
            .get(&HeaderName::Custom("sec-websocket-key".into()))
            .is_some_and(validate_key)
}

/// Build the server's `101 Switching Protocols` response for `key`.
///
/// `subprotocol` is the negotiated `Sec-WebSocket-Protocol` value to echo
/// back, if one was agreed. Returns `None` when the key is not a valid
/// 16-byte nonce.
pub fn upgrade_response(key: &[u8], subprotocol: Option<&str>) -> Option<Response> {
    if !validate_key(key) {
        return None;
    }
    let mut response = Response::new(Version::Http11, StatusCode::SWITCHING_PROTOCOLS);
    response.header(HeaderName::Upgrade, "websocket");
    response.header(HeaderName::Connection, "Upgrade");
    response.header(
        HeaderName::Custom("sec-websocket-accept".into()),
        accept_key(key),
    );
    if let Some(subprotocol) = subprotocol {
        response.header(
            HeaderName::Custom("sec-websocket-protocol".into()),
            subprotocol,
        );
    }
    Some(response)
}

/// Build a client-side opening handshake request with the given key.
///
/// `host` is the `Host` value, `target` the request-target (path), `key` the
/// 16-byte base64 nonce, and `subprotocols` the offered `Sec-WebSocket-Protocol`
/// values. The proxy forwards the client's key rather than generating its own
/// (RFC 6455 §8.1.3).
pub fn client_request(host: &str, target: &[u8], key: &str, subprotocols: &[&str]) -> Request {
    let mut headers = Headers::new();
    headers.push_value(HeaderName::Host, host);
    headers.push_value(HeaderName::Connection, "Upgrade");
    headers.push_value(HeaderName::Upgrade, "websocket");
    headers.push_value(
        HeaderName::Custom("sec-websocket-version".into()),
        WEBSOCKET_VERSION,
    );
    headers.push_value(HeaderName::Custom("sec-websocket-key".into()), key);
    if !subprotocols.is_empty() {
        headers.push_value(
            HeaderName::Custom("sec-websocket-protocol".into()),
            subprotocols.join(", "),
        );
    }
    Request::new(
        Method::Get,
        target.to_vec(),
        Version::Http11,
        headers,
        BodyFraming::None,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        WEBSOCKET_GUID, accept_key, client_request, contains_token, is_websocket_upgrade,
        upgrade_response, validate_key,
    };
    use crate::http::{BodyFraming, Headers, Method, Request, Version};

    #[test]
    fn accept_key_matches_the_rfc_vector() {
        // RFC 6455 §1.3 worked example.
        assert_eq!(
            accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn guid_matches_rfc_6455() {
        assert_eq!(WEBSOCKET_GUID, "258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    }

    #[test]
    fn validates_keys() {
        assert!(validate_key(b"dGhlIHNhbXBsZSBub25jZQ=="));
        assert!(!validate_key(b"not base64!"));
        // Base64 of 15 bytes decodes to 15 bytes.
        assert!(!validate_key(b"c2hvcnQgbm9uY2U="));
        // Base64 of 17 bytes decodes to 17 bytes.
        assert!(!validate_key(b"bG9uZ2VyIG5vbmNlIWhlcmU="));
    }

    #[test]
    fn matches_tokens_case_insensitively() {
        assert!(contains_token(b"keep-alive, Upgrade", "upgrade"));
        assert!(contains_token(b"upgrade", "Upgrade"));
        assert!(!contains_token(b"keep-alive", "upgrade"));
        assert!(!contains_token(b"upgraded", "upgrade"));
    }

    fn upgrade_request() -> Request {
        let mut headers = Headers::new();
        headers.push_value(crate::http::HeaderName::Host, "example.com");
        headers.push_value(crate::http::HeaderName::Connection, "keep-alive, Upgrade");
        headers.push_value(crate::http::HeaderName::Upgrade, "WebSocket");
        headers.push_value(
            crate::http::HeaderName::Custom("sec-websocket-version".into()),
            "13",
        );
        headers.push_value(
            crate::http::HeaderName::Custom("sec-websocket-key".into()),
            "dGhlIHNhbXBsZSBub25jZQ==",
        );
        Request::new(
            Method::Get,
            b"/chat".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        )
    }

    #[test]
    fn classifies_upgrade_requests() {
        assert!(is_websocket_upgrade(&upgrade_request()));
        let mut request = upgrade_request();
        request.method = Method::Post;
        assert!(!is_websocket_upgrade(&request));
        let mut request = upgrade_request();
        request.version = Version::Http10;
        assert!(!is_websocket_upgrade(&request));
    }

    #[test]
    fn rejects_upgrades_with_bad_keys_or_versions() {
        let mut request = upgrade_request();
        request.headers = {
            let mut headers = Headers::new();
            headers.push_value(crate::http::HeaderName::Host, "example.com");
            headers.push_value(crate::http::HeaderName::Connection, "upgrade");
            headers.push_value(crate::http::HeaderName::Upgrade, "websocket");
            headers.push_value(
                crate::http::HeaderName::Custom("sec-websocket-version".into()),
                "13",
            );
            headers.push_value(
                crate::http::HeaderName::Custom("sec-websocket-key".into()),
                "not-a-key",
            );
            headers
        };
        assert!(!is_websocket_upgrade(&request));

        let mut request = upgrade_request();
        request.headers = {
            let mut headers = Headers::new();
            headers.push_value(crate::http::HeaderName::Host, "example.com");
            headers.push_value(crate::http::HeaderName::Connection, "upgrade");
            headers.push_value(crate::http::HeaderName::Upgrade, "websocket");
            headers.push_value(
                crate::http::HeaderName::Custom("sec-websocket-version".into()),
                "12",
            );
            headers.push_value(
                crate::http::HeaderName::Custom("sec-websocket-key".into()),
                "dGhlIHNhbXBsZSBub25jZQ==",
            );
            headers
        };
        assert!(!is_websocket_upgrade(&request));
    }

    #[test]
    fn response_carries_101_and_accept() {
        let response = upgrade_response(b"dGhlIHNhbXBsZSBub25jZQ==", None).unwrap();
        assert_eq!(
            response.status,
            crate::http::StatusCode::SWITCHING_PROTOCOLS
        );
        assert_eq!(
            response.headers.get_str(&crate::http::HeaderName::Custom(
                "sec-websocket-accept".into()
            )),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
        assert!(upgrade_response(b"invalid", None).is_none());
    }

    #[test]
    fn response_echoes_subprotocol() {
        let response = upgrade_response(b"dGhlIHNhbXBsZSBub25jZQ==", Some("chat")).unwrap();
        assert_eq!(
            response.headers.get_str(&crate::http::HeaderName::Custom(
                "sec-websocket-protocol".into()
            )),
            Some("chat")
        );
    }

    #[test]
    fn client_request_is_a_valid_upgrade() {
        let request = client_request(
            "example.com",
            b"/chat",
            "dGhlIHNhbXBsZSBub25jZQ==",
            &["chat"],
        );
        assert!(is_websocket_upgrade(&request));
        assert_eq!(request.target, b"/chat");
    }
}
