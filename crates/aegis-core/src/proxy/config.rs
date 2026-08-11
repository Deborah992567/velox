//! Upstream target and timeout configuration for the reverse proxy.
//!
//! A `proxy_pass` directive names an upstream and, optionally, a URI
//! replacement prefix. Following nginx semantics (architecture §10):
//!
//! - `proxy_pass http://host[:port];` — no URI part; the full original request
//!   URI (path plus query) is passed through untouched;
//! - `proxy_pass http://host[:port]/prefix;` — the matched location prefix is
//!   replaced with `/prefix` (trailing parts preserved);
//! - `proxy_pass unix:/path.sock;` — the upstream is a Unix domain socket.
//!
//! The `Host` header sent upstream is the authority written in the directive
//! (with the explicit port, when one was given), exactly as nginx's
//! `$proxy_host` would be.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use crate::net::InetAddr;

/// The scheme of a `proxy_pass` upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamScheme {
    /// Plain HTTP over TCP, or a Unix socket.
    Http,
    /// HTTPS over TLS. Parseable, but the exchange rejects it until an
    /// outbound rustls client lands.
    Https,
}

impl UpstreamScheme {
    /// The default port when `proxy_pass` omitted one.
    const fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// A parsed `proxy_pass` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyTarget {
    /// The scheme.
    pub scheme: UpstreamScheme,
    /// The resolved upstream address.
    pub addr: InetAddr,
    /// The URI replacement prefix, when the directive carried a URI part;
    /// `None` means the full original request URI is passed through.
    pub uri_prefix: Option<Vec<u8>>,
    /// The value used for the upstream `Host` header (`$proxy_host`).
    pub host_header: String,
}

impl ProxyTarget {
    /// An HTTP target with no URI replacement.
    pub fn http(addr: InetAddr) -> Self {
        Self {
            scheme: UpstreamScheme::Http,
            addr: addr.clone(),
            uri_prefix: None,
            host_header: host_header_for(&addr),
        }
    }

    /// Attach a URI replacement prefix (`/v1`, `/api`, ...).
    #[must_use]
    pub fn with_uri_prefix(mut self, prefix: impl Into<Vec<u8>>) -> Self {
        self.uri_prefix = Some(prefix.into());
        self
    }
}

/// Why a `proxy_pass` value was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyTargetError {
    /// The value was empty.
    Empty,
    /// No `http://`, `https://`, or `unix:` scheme prefix.
    MissingScheme,
    /// An unknown scheme (`ftp://`, ...).
    UnsupportedScheme,
    /// The authority was malformed (empty, bad IPv6, missing bracket).
    MalformedAuthority,
    /// A non-numeric or out-of-range port.
    InvalidPort,
    /// The host did not resolve to any address.
    UnknownHost,
}

/// Parse a `proxy_pass` value into a target.
pub fn parse_proxy_pass(value: &str) -> Result<ProxyTarget, ProxyTargetError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ProxyTargetError::Empty);
    }

    if let Some(rest) = value.strip_prefix("unix:") {
        let path = rest.trim_end_matches(':');
        if path.is_empty() {
            return Err(ProxyTargetError::MalformedAuthority);
        }
        return Ok(ProxyTarget {
            scheme: UpstreamScheme::Http,
            addr: InetAddr::Unix(PathBuf::from(path)),
            uri_prefix: None,
            host_header: "localhost".to_string(),
        });
    }

    let Some((scheme, rest)) = value.split_once("://") else {
        return Err(ProxyTargetError::MissingScheme);
    };
    let scheme = match scheme {
        "http" => UpstreamScheme::Http,
        "https" => UpstreamScheme::Https,
        _ => return Err(ProxyTargetError::UnsupportedScheme),
    };

    let (authority, uri_prefix) = match rest.split_once('/') {
        Some((authority, path)) => (authority, Some(format!("/{path}").into_bytes())),
        None => (rest, None),
    };
    if authority.is_empty() {
        return Err(ProxyTargetError::MalformedAuthority);
    }

    let (host, port, explicit_port) = split_authority(authority, scheme.default_port())?;
    let addr = resolve(host, port).ok_or(ProxyTargetError::UnknownHost)?;
    let host_header = if explicit_port {
        display_host(host, port)
    } else {
        display_host(host, 0)
    };
    Ok(ProxyTarget {
        scheme,
        addr,
        uri_prefix,
        host_header,
    })
}

/// Split an authority into `(host, port, port_was_explicit)`. A bracketed
/// IPv6 literal may carry a port (`[::1]:8080`) or omit it (`[::1]`).
fn split_authority(
    authority: &str,
    default_port: u16,
) -> Result<(&str, u16, bool), ProxyTargetError> {
    if let Some(rest) = authority.strip_prefix('[') {
        if let Some((host, port)) = rest.split_once("]:") {
            return Ok((host, parse_port(port)?, true));
        }
        let host = rest
            .strip_suffix(']')
            .ok_or(ProxyTargetError::MalformedAuthority)?;
        return Ok((host, default_port, false));
    }
    if let Some((host, port)) = authority.rsplit_once(':')
        && !host.contains(':')
    {
        return Ok((host, parse_port(port)?, true));
    }
    Ok((authority, default_port, false))
}

/// Parse a decimal port, rejecting empty and out-of-range values.
fn parse_port(port: &str) -> Result<u16, ProxyTargetError> {
    port.parse().map_err(|_| ProxyTargetError::InvalidPort)
}

/// Resolve a host to a single [`InetAddr`]: numeric IPs directly, otherwise
/// the first address from the system resolver.
fn resolve(host: &str, port: u16) -> Option<InetAddr> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Some(InetAddr::V4(SocketAddrV4::new(ip, port)));
    }
    if let Ok(ip) = host.parse::<Ipv6Addr>() {
        return Some(InetAddr::V6(SocketAddrV6::new(ip, port, 0, 0)));
    }
    (host, port)
        .to_socket_addrs()
        .ok()?
        .next()
        .map(|addr| match addr {
            std::net::SocketAddr::V4(v4) => InetAddr::V4(v4),
            std::net::SocketAddr::V6(v6) => InetAddr::V6(v6),
        })
}

/// Render a host for the `Host` header, bracketing IPv6 literals. A zero port
/// means "no port suffix" (the default port for the scheme).
fn display_host(host: &str, port: u16) -> String {
    let bracketed = if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    if port == 0 {
        bracketed
    } else {
        format!("{bracketed}:{port}")
    }
}

/// Derive the `Host` header from an address (for hand-built targets).
fn host_header_for(addr: &InetAddr) -> String {
    match addr {
        InetAddr::V4(v4) => format!("{}:{}", v4.ip(), v4.port()),
        InetAddr::V6(v6) => format!("[{}]:{}", v6.ip(), v6.port()),
        InetAddr::Unix(_) => "localhost".to_string(),
    }
}

/// Timeouts and retry policy for one proxied exchange.
#[derive(Debug, Clone, Copy)]
pub struct ProxyOptions {
    /// Establish the upstream connection within this deadline.
    pub connect_timeout: Duration,
    /// A single upstream read (response head or body) may block for at most
    /// this long before the exchange aborts.
    pub read_timeout: Duration,
    /// A single upstream write (request head or body) may block for at most
    /// this long.
    pub send_timeout: Duration,
    /// How many additional attempts after the first. Retries only ever apply
    /// to bodyless, idempotent requests and only before any response bytes
    /// reach the client.
    pub retries: u32,
}

impl Default for ProxyOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_mins(1),
            read_timeout: Duration::from_mins(1),
            send_timeout: Duration::from_mins(1),
            retries: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProxyTargetError, UpstreamScheme, parse_proxy_pass};
    use crate::net::InetAddr;
    use std::net::{Ipv4Addr, SocketAddrV4};

    fn ipv4(host: [u8; 4], port: u16) -> InetAddr {
        InetAddr::V4(SocketAddrV4::new(Ipv4Addr::from(host), port))
    }

    #[test]
    fn parses_http_target_without_port_or_prefix() {
        let target = parse_proxy_pass("http://127.0.0.1").unwrap();
        assert_eq!(target.scheme, UpstreamScheme::Http);
        assert_eq!(target.addr, ipv4([127, 0, 0, 1], 80));
        assert_eq!(target.uri_prefix, None);
        assert_eq!(target.host_header, "127.0.0.1");
    }

    #[test]
    fn parses_host_port_and_prefix() {
        let target = parse_proxy_pass("http://127.0.0.1:8080/api").unwrap();
        assert_eq!(target.addr, ipv4([127, 0, 0, 1], 8080));
        assert_eq!(target.uri_prefix, Some(b"/api".to_vec()));
        assert_eq!(target.host_header, "127.0.0.1:8080");
    }

    #[test]
    fn trailing_slash_is_a_prefix() {
        let target = parse_proxy_pass("http://127.0.0.1:8080/").unwrap();
        assert_eq!(target.uri_prefix, Some(b"/".to_vec()));
        assert_eq!(target.host_header, "127.0.0.1:8080");
    }

    #[test]
    fn https_defaults_to_443() {
        let target = parse_proxy_pass("https://93.184.216.34").unwrap();
        assert_eq!(target.scheme, UpstreamScheme::Https);
        assert_eq!(target.addr, ipv4([93, 184, 216, 34], 443));
        assert_eq!(target.host_header, "93.184.216.34");
    }

    #[test]
    fn unix_socket_targets_use_localhost_host() {
        let target = parse_proxy_pass("unix:/tmp/aegis-upstream.sock").unwrap();
        assert_eq!(target.scheme, UpstreamScheme::Http);
        assert_eq!(
            target.addr,
            InetAddr::Unix("/tmp/aegis-upstream.sock".into())
        );
        assert_eq!(target.host_header, "localhost");
    }

    #[test]
    fn ipv6_authority_brackets_literal() {
        let target = parse_proxy_pass("http://[::1]:8080/").unwrap();
        assert!(matches!(target.addr, InetAddr::V6(_)));
        assert_eq!(target.host_header, "[::1]:8080");
        assert_eq!(target.uri_prefix, Some(b"/".to_vec()));
    }

    #[test]
    fn hostname_resolves() {
        let target = parse_proxy_pass("http://localhost:8080").unwrap();
        assert!(matches!(target.addr, InetAddr::V4(_) | InetAddr::V6(_)));
        assert_eq!(target.host_header, "localhost:8080");
    }

    #[test]
    fn rejects_malformed_values() {
        for (value, error) in [
            ("", ProxyTargetError::Empty),
            ("backend", ProxyTargetError::MissingScheme),
            ("ftp://127.0.0.1", ProxyTargetError::UnsupportedScheme),
            ("http://", ProxyTargetError::MalformedAuthority),
            ("http://127.0.0.1:99999", ProxyTargetError::InvalidPort),
            ("http://[::1", ProxyTargetError::MalformedAuthority),
            ("http://127.0.0.1:abc", ProxyTargetError::InvalidPort),
            ("http://no-such-host.invalid", ProxyTargetError::UnknownHost),
        ] {
            assert_eq!(parse_proxy_pass(value).unwrap_err(), error, "for {value:?}");
        }
    }

    #[test]
    fn hand_built_target_derives_host_header() {
        let target = super::ProxyTarget::http(ipv4([10, 0, 0, 1], 9000));
        assert_eq!(target.host_header, "10.0.0.1:9000");
    }

    #[test]
    fn default_options_have_bounded_retries() {
        let options = super::ProxyOptions::default();
        assert!(options.connect_timeout > std::time::Duration::ZERO);
        assert_eq!(options.retries, 1);
    }
}
