//! Request rewriting for the reverse proxy: URI replacement and header
//! normalization on the way upstream.
//!
//! `proxy_pass http://host[:port]/prefix;` inside `location /old/` rewrites a
//! request to `/old/a` into `/prefix/a` — the directive's URI part replaces
//! the matched location prefix (nginx semantics, architecture §10). With no
//! URI part the request target passes through untouched.
//!
//! Header rewriting strips the hop-by-hop and framing fields a proxy must not
//! relay verbatim (RFC 9110 §7.6.1 plus the framing pair that the encoder
//! re-derives), pins `Host` to the upstream authority (`$proxy_host`), and
//! appends `X-Forwarded-For`, `X-Real-IP`, and `X-Forwarded-Proto`.

use crate::http::{HeaderName, Headers, Request};
use crate::proxy::config::ProxyTarget;

/// Why a request target could not be rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteError {
    /// A URI replacement was configured but the matched location has no
    /// replaceable prefix (a regex or named-location match).
    NoPrefix,
    /// The request path does not start with the matched location prefix.
    PrefixMismatch,
}

/// Compute the upstream request-target.
///
/// Without a `proxy_pass` URI part the original target is returned verbatim.
/// With one, the matched location prefix is replaced by the directive's
/// prefix, preserving the path suffix and any query string. Non-origin-form
/// targets (absolute-form, asterisk-form) are passed through unchanged.
pub fn rewrite_target(
    request: &Request,
    location_prefix: Option<&str>,
    target: &ProxyTarget,
) -> Result<Vec<u8>, RewriteError> {
    let Some(directive_prefix) = target.uri_prefix.as_deref() else {
        return Ok(request.target.clone());
    };
    if !request.target.starts_with(b"/") {
        return Ok(request.target.clone());
    }
    let Some(matched) = location_prefix else {
        return Err(RewriteError::NoPrefix);
    };
    let path = request.path();
    let suffix = path
        .strip_prefix(matched.as_bytes())
        .ok_or(RewriteError::PrefixMismatch)?;
    let query = request.query();
    let query_len = query.map_or(0, <[u8]>::len);
    let mut out = Vec::with_capacity(directive_prefix.len() + suffix.len() + 1 + query_len);
    out.extend_from_slice(directive_prefix);
    out.extend_from_slice(suffix);
    if let Some(query) = query {
        out.push(b'?');
        out.extend_from_slice(query);
    }
    Ok(out)
}

/// Rewrite the request headers for the upstream: `Host` pinned to the target
/// authority, hop-by-hop and framing fields stripped, and the forwarded-*
/// headers appended.
pub fn rewrite_request_headers(
    request: &Request,
    target: &ProxyTarget,
    client_ip: &str,
    proto: &str,
) -> Headers {
    let mut out = Headers::new();
    out.push_value(HeaderName::Host, target.host_header.clone());
    for header in request.headers.iter() {
        if is_hop_by_hop(&header.name)
            || matches!(
                header.name,
                HeaderName::Host
                    | HeaderName::ContentLength
                    | HeaderName::Expect
                    | HeaderName::XForwardedFor
                    | HeaderName::XRealIp
                    | HeaderName::XForwardedProto
            )
        {
            continue;
        }
        out.push(header.clone());
    }
    out.push_value(HeaderName::XForwardedFor, forwarded_for(request, client_ip));
    out.push_value(HeaderName::XRealIp, client_ip);
    out.push_value(HeaderName::XForwardedProto, proto);
    out
}

/// Rewrite headers for a WebSocket upgrade request going upstream.
///
/// Unlike [`rewrite_request_headers`] this preserves the `Connection: Upgrade`,
/// `Upgrade: websocket`, and `Sec-WebSocket-*` headers the upstream needs to
/// accept the handshake (RFC 6455 §8.1.3 requires the proxy to forward the
/// client's `Sec-WebSocket-Key` unchanged).
pub fn rewrite_ws_request_headers(
    request: &Request,
    target: &ProxyTarget,
    client_ip: &str,
    proto: &str,
) -> Headers {
    let mut out = Headers::new();
    out.push_value(HeaderName::Host, target.host_header.clone());
    for header in request.headers.iter() {
        match header.name {
            HeaderName::Host
            | HeaderName::ContentLength
            | HeaderName::Expect
            | HeaderName::XForwardedFor
            | HeaderName::XRealIp
            | HeaderName::XForwardedProto => continue,
            _ => {}
        }
        if is_hop_by_hop(&header.name) && !is_ws_passthrough(&header.name) {
            continue;
        }
        out.push(header.clone());
    }
    out.push_value(HeaderName::XForwardedFor, forwarded_for(request, client_ip));
    out.push_value(HeaderName::XRealIp, client_ip);
    out.push_value(HeaderName::XForwardedProto, proto);
    out
}

/// Whether a hop-by-hop header must be preserved for WebSocket upgrades.
const fn is_ws_passthrough(name: &HeaderName) -> bool {
    matches!(
        name,
        HeaderName::Connection | HeaderName::Upgrade | HeaderName::Trailer
    )
}

/// A copy of `headers` with every hop-by-hop field removed (RFC 9110 §7.6.1):
/// the standard connection-control fields plus any field named by a
/// `Connection` token.
///
/// Also drops `Content-Length`/`Transfer-Encoding`, which the encoder
/// re-derives from the body framing when it emits a fresh head.
pub(crate) fn strip_hop_by_hop(headers: &Headers) -> Headers {
    let mut named: Vec<String> = Vec::new();
    for value in headers.get_all(&HeaderName::Connection) {
        let Ok(value) = std::str::from_utf8(value) else {
            continue;
        };
        for token in value.split(',') {
            let token = token.trim();
            if !token.is_empty() {
                named.push(token.to_ascii_lowercase());
            }
        }
    }
    let mut out = Headers::new();
    for header in headers.iter() {
        if is_hop_by_hop(&header.name)
            || matches!(header.name, HeaderName::ContentLength)
            || named.iter().any(|t| header.name.as_str() == t.as_str())
        {
            continue;
        }
        out.push(header.clone());
    }
    out
}

/// Whether a field name is hop-by-hop regardless of `Connection` tokens.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(name, HeaderName::Custom(custom) if custom.as_ref() == "proxy-connection" || custom.as_ref() == "te")
        || matches!(
            name,
            HeaderName::Connection
                | HeaderName::KeepAlive
                | HeaderName::TransferEncoding
                | HeaderName::Trailer
                | HeaderName::Upgrade
                | HeaderName::ProxyAuthorization
                | HeaderName::ProxyAuthenticate
        )
}

/// Append the client address to any existing `X-Forwarded-For` value.
fn forwarded_for(request: &Request, client_ip: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for value in request.headers.get_all(&HeaderName::XForwardedFor) {
        let Ok(value) = std::str::from_utf8(value) else {
            continue;
        };
        if !first {
            out.push_str(", ");
        }
        out.push_str(value);
        first = false;
    }
    if !first {
        out.push_str(", ");
    }
    out.push_str(client_ip);
    out
}

#[cfg(test)]
mod tests {
    use super::{
        RewriteError, rewrite_request_headers, rewrite_target, rewrite_ws_request_headers,
        strip_hop_by_hop,
    };
    use crate::http::{BodyFraming, HeaderName, Headers, Method, Request, Version};
    use crate::net::InetAddr;
    use crate::proxy::config::{ProxyTarget, parse_proxy_pass};

    fn request(target: &[u8], headers: Headers) -> Request {
        Request::new(
            Method::Get,
            target.to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        )
    }

    #[test]
    fn passes_target_through_without_uri_part() {
        let target = parse_proxy_pass("http://127.0.0.1:8080").unwrap();
        let request = request(b"/api/foo?x=1", Headers::new());
        assert_eq!(
            rewrite_target(&request, Some("/api"), &target).unwrap(),
            b"/api/foo?x=1"
        );
    }

    #[test]
    fn replaces_the_location_prefix() {
        let target =
            ProxyTarget::http(InetAddr::Unix("/tmp/up.sock".into())).with_uri_prefix(b"/v1");
        let request = request(b"/api/foo?x=1", Headers::new());
        assert_eq!(
            rewrite_target(&request, Some("/api"), &target).unwrap(),
            b"/v1/foo?x=1"
        );
    }

    #[test]
    fn exact_match_replaces_the_whole_path() {
        let target =
            ProxyTarget::http(InetAddr::Unix("/tmp/up.sock".into())).with_uri_prefix(b"/app");
        let request = request(b"/", Headers::new());
        assert_eq!(
            rewrite_target(&request, Some("/"), &target).unwrap(),
            b"/app"
        );
    }

    #[test]
    fn prefix_without_trailing_slash_preserves_suffix() {
        let target =
            ProxyTarget::http(InetAddr::Unix("/tmp/up.sock".into())).with_uri_prefix(b"/v");
        let request = request(b"/api/foo", Headers::new());
        assert_eq!(
            rewrite_target(&request, Some("/api"), &target).unwrap(),
            b"/v/foo"
        );
    }

    #[test]
    fn non_origin_targets_pass_through() {
        let target =
            ProxyTarget::http(InetAddr::Unix("/tmp/up.sock".into())).with_uri_prefix(b"/v1");
        let asterisk = request(b"*", Headers::new());
        assert_eq!(rewrite_target(&asterisk, Some("/"), &target).unwrap(), b"*");
        let absolute = request(b"http://example.com/a", Headers::new());
        assert_eq!(
            rewrite_target(&absolute, Some("/"), &target).unwrap(),
            b"http://example.com/a"
        );
    }

    #[test]
    fn requires_a_prefix_for_replacement() {
        let target =
            ProxyTarget::http(InetAddr::Unix("/tmp/up.sock".into())).with_uri_prefix(b"/v1");
        let request = request(b"/api/foo", Headers::new());
        assert_eq!(
            rewrite_target(&request, None, &target),
            Err(RewriteError::NoPrefix)
        );
    }

    #[test]
    fn mismatched_prefix_is_an_error() {
        let target =
            ProxyTarget::http(InetAddr::Unix("/tmp/up.sock".into())).with_uri_prefix(b"/v1");
        let request = request(b"/other/foo", Headers::new());
        assert_eq!(
            rewrite_target(&request, Some("/api"), &target),
            Err(RewriteError::PrefixMismatch)
        );
    }

    #[test]
    fn rewrites_headers_for_the_upstream() {
        let target = ProxyTarget::http(InetAddr::Unix("/tmp/up.sock".into()));
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        headers.push_value(HeaderName::Connection, "keep-alive");
        headers.push_value(HeaderName::ContentLength, "0");
        headers.push_value(HeaderName::UserAgent, "curl");
        headers.push_value(HeaderName::XForwardedFor, "203.0.113.7");
        let request = request(b"/", headers);
        let rewritten = rewrite_request_headers(&request, &target, "10.0.0.1", "http");
        assert_eq!(rewritten.get_str(&HeaderName::Host), Some("localhost"));
        assert_eq!(
            rewritten.get_str(&HeaderName::XForwardedFor),
            Some("203.0.113.7, 10.0.0.1")
        );
        assert_eq!(rewritten.get_str(&HeaderName::XRealIp), Some("10.0.0.1"));
        assert_eq!(
            rewritten.get_str(&HeaderName::XForwardedProto),
            Some("http")
        );
        assert!(!rewritten.contains(&HeaderName::Connection));
        assert!(!rewritten.contains(&HeaderName::ContentLength));
        assert_eq!(rewritten.get_str(&HeaderName::UserAgent), Some("curl"));
    }

    #[test]
    fn connection_tokens_mark_other_fields_hop_by_hop() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Connection, "X-Obsolete");
        headers.push_value(HeaderName::Custom("x-obsolete".into()), "v");
        headers.push_value(HeaderName::Etag, "abc");
        let stripped = strip_hop_by_hop(&headers);
        assert!(!stripped.contains(&HeaderName::Connection));
        assert!(!stripped.contains(&HeaderName::Custom("x-obsolete".into())));
        assert_eq!(stripped.get_str(&HeaderName::Etag), Some("abc"));
    }

    #[test]
    fn ws_request_headers_preserve_upgrade_and_key() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        headers.push_value(HeaderName::Connection, "Upgrade");
        headers.push_value(HeaderName::Upgrade, "websocket");
        headers.push_value(HeaderName::Custom("sec-websocket-version".into()), "13");
        headers.push_value(
            HeaderName::Custom("sec-websocket-key".into()),
            "dGhlIHNhbXBsZSBub25jZQ==",
        );
        headers.push_value(HeaderName::UserAgent, "test");
        let request = request(b"/chat", headers);
        let target = ProxyTarget::http(InetAddr::Unix("/tmp/up.sock".into()));
        let rewritten = rewrite_ws_request_headers(&request, &target, "10.0.0.1", "http");
        assert_eq!(rewritten.get_str(&HeaderName::Upgrade), Some("websocket"));
        assert_eq!(rewritten.get_str(&HeaderName::Connection), Some("Upgrade"));
        assert_eq!(
            rewritten.get_str(&HeaderName::Custom("sec-websocket-key".into())),
            Some("dGhlIHNhbXBsZSBub25jZQ==")
        );
        assert_eq!(
            rewritten.get_str(&HeaderName::Custom("sec-websocket-version".into())),
            Some("13")
        );
        assert_eq!(rewritten.get_str(&HeaderName::UserAgent), Some("test"));
        assert_eq!(
            rewritten.get_str(&HeaderName::XForwardedFor),
            Some("10.0.0.1")
        );
    }
}
