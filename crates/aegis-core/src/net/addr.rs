//! Socket addresses: IPv4, IPv6, and Unix domain sockets.
//!
//! Aegis binds three kinds of endpoints:
//!
//! - `0.0.0.0:8080`, `127.0.0.1:80` — IPv4
//! - `[::1]:8080`, `[::]:80` — IPv6 (bracketed)
//! - `/var/run/aegis.sock`, `unix:/path` — Unix domain sockets
//!
//! A bare port (`8080`) expands to both the IPv4 and IPv6 wildcard
//! addresses so a single `listen 8080;` directive serves on both stacks.

use std::ffi::CStr;
use std::io;
use std::mem::size_of;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::path::PathBuf;

use super::socket::{sa_family, socklen};
use crate::core::error::Error;

/// A socket endpoint the server can bind or connect to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InetAddr {
    /// IPv4 socket address.
    V4(SocketAddrV4),
    /// IPv6 socket address (flow info and scope id kept zero unless set).
    V6(SocketAddrV6),
    /// Unix domain socket path.
    Unix(PathBuf),
}

impl InetAddr {
    /// Parse one `listen` argument into the addresses to bind.
    ///
    /// A bare port expands to the IPv4 **and** IPv6 wildcard addresses; a
    /// bracketed IPv6 address (`[::1]:80`) yields a single IPv6 endpoint; a
    /// path (starting with `/`) is treated as a Unix domain socket.
    pub fn parse(text: &str) -> crate::core::Result<Vec<Self>> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::parse("empty listen address"));
        }

        if text.starts_with('/') || text.starts_with("unix:") {
            let path = text.strip_prefix("unix:").unwrap_or(text);
            let path = PathBuf::from(path);
            if path.as_os_str().is_empty() {
                return Err(Error::parse("empty unix socket path"));
            }
            return Ok(vec![Self::Unix(path)]);
        }

        if text.starts_with('[') {
            let rest = text.strip_prefix('[').unwrap_or(text);
            let (ip, port) = rest
                .split_once("]:")
                .ok_or_else(|| Error::parse(format!("invalid listen address \"{text}\"")))?;
            let ip: Ipv6Addr = ip.parse().map_err(|_| {
                Error::parse(format!("invalid IPv6 address \"{ip}\" in \"{text}\""))
            })?;
            let port = parse_port(port, text)?;
            return Ok(vec![Self::V6(SocketAddrV6::new(ip, port, 0, 0))]);
        }

        if let Some((host, port)) = text.rsplit_once(':') {
            let port = parse_port(port, text)?;
            if let Ok(ip) = host.parse::<Ipv4Addr>() {
                return Ok(vec![Self::V4(SocketAddrV4::new(ip, port))]);
            }
            if let Ok(ip) = host.parse::<Ipv6Addr>() {
                return Ok(vec![Self::V6(SocketAddrV6::new(ip, port, 0, 0))]);
            }
            return Err(Error::parse(format!(
                "unsupported host \"{host}\" in \"{text}\" (numeric IP addresses only)"
            )));
        }

        if let Ok(port) = parse_port(text, text) {
            return Ok(vec![
                Self::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)),
                Self::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0)),
            ]);
        }

        Err(Error::parse(format!("invalid listen address \"{text}\"")))
    }

    /// The textual form for logs: `0.0.0.0:8080`, `[::1]:8080`,
    /// `unix:/var/run/aegis.sock`.
    pub fn display(&self) -> String {
        match self {
            Self::V4(addr) => addr.to_string(),
            Self::V6(addr) => format!("[{ip}]:{port}", ip = addr.ip(), port = addr.port()),
            Self::Unix(path) => format!("unix:{}", path.display()),
        }
    }

    /// Whether this address binds any network interface (`0.0.0.0` or `::`).
    pub const fn is_wildcard(&self) -> bool {
        match self {
            Self::V4(addr) => addr.ip().is_unspecified(),
            Self::V6(addr) => addr.ip().is_unspecified(),
            Self::Unix(_) => false,
        }
    }

    /// Whether this is a Unix domain socket endpoint.
    pub const fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }

    /// The port for TCP addresses, `None` for Unix sockets.
    pub const fn port(&self) -> Option<u16> {
        match self {
            Self::V4(addr) => Some(addr.port()),
            Self::V6(addr) => Some(addr.port()),
            Self::Unix(_) => None,
        }
    }

    /// Render into a raw `sockaddr_storage` plus its length.
    pub(crate) fn to_sockaddr(&self) -> (libc::sockaddr_storage, libc::socklen_t) {
        // SAFETY: `storage` is zeroed and immediately aliased as the concrete
        // sockaddr type matching `self`; `self` is valid for `'static`.
        let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        match self {
            Self::V4(addr) => {
                // SAFETY: `storage` is at least as large as `sockaddr_in` and
                // we only write within the written portion.
                let sin =
                    unsafe { &mut *std::ptr::from_mut(&mut storage).cast::<libc::sockaddr_in>() };
                sin.sin_family = sa_family(libc::AF_INET);
                sin.sin_port = addr.port().to_be();
                sin.sin_addr.s_addr = u32::from_ne_bytes(addr.ip().octets());
                (storage, socklen(size_of::<libc::sockaddr_in>()))
            }
            Self::V6(addr) => {
                // SAFETY: `storage` is at least as large as `sockaddr_in6` and
                // we only write within the written portion.
                let sin6 =
                    unsafe { &mut *std::ptr::from_mut(&mut storage).cast::<libc::sockaddr_in6>() };
                sin6.sin6_family = sa_family(libc::AF_INET6);
                sin6.sin6_port = addr.port().to_be();
                sin6.sin6_flowinfo = addr.flowinfo();
                sin6.sin6_scope_id = addr.scope_id();
                sin6.sin6_addr.s6_addr = addr.ip().octets();
                (storage, socklen(size_of::<libc::sockaddr_in6>()))
            }
            Self::Unix(path) => {
                let bytes = path.as_os_str().as_encoded_bytes();
                // SAFETY: `storage` is at least as large as `sockaddr_un` and
                // we only write within the written portion.
                let sun =
                    unsafe { &mut *std::ptr::from_mut(&mut storage).cast::<libc::sockaddr_un>() };
                sun.sun_family = sa_family(libc::AF_UNIX);
                let path_slot: &mut [libc::c_char] = &mut sun.sun_path;
                assert!(
                    bytes.len() < path_slot.len(),
                    "unix socket path too long for sockaddr_un"
                );
                // SAFETY: the assertion above guarantees `bytes` fits inside
                // the `sun_path` byte buffer; raw bytes are copied verbatim.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        path_slot.as_mut_ptr().cast::<u8>(),
                        bytes.len(),
                    );
                }
                let len = size_of::<libc::sockaddr_un>() - (path_slot.len() - bytes.len() - 1);
                (storage, socklen(len))
            }
        }
    }

    /// Rebuild an endpoint from a raw `sockaddr` (used by `getsockname` and
    /// `getpeername`).
    pub(crate) fn from_sockaddr(
        storage: &libc::sockaddr_storage,
        len: libc::socklen_t,
    ) -> io::Result<Self> {
        // SAFETY: the caller supplies a pointer to a `sockaddr` written by the
        // kernel with family and payload matching `len`.
        let raw = std::ptr::addr_of!(*storage).cast::<libc::sockaddr>();
        let family = i32::from(unsafe { (*raw).sa_family });
        match family {
            libc::AF_INET => {
                // SAFETY: family AF_INET implies a `sockaddr_in` payload.
                if len < socklen(size_of::<libc::sockaddr_in>()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "short IPv4 socket address",
                    ));
                }
                let sin = unsafe { &*raw.cast::<libc::sockaddr_in>() };
                let ip = Ipv4Addr::from(sin.sin_addr.s_addr.to_ne_bytes());
                Ok(Self::V4(SocketAddrV4::new(ip, u16::from_be(sin.sin_port))))
            }
            libc::AF_INET6 => {
                // SAFETY: family AF_INET6 implies a `sockaddr_in6` payload.
                if len < socklen(size_of::<libc::sockaddr_in6>()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "short IPv6 socket address",
                    ));
                }
                let sin6 = unsafe { &*raw.cast::<libc::sockaddr_in6>() };
                let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                Ok(Self::V6(SocketAddrV6::new(
                    ip,
                    u16::from_be(sin6.sin6_port),
                    sin6.sin6_flowinfo,
                    sin6.sin6_scope_id,
                )))
            }
            libc::AF_UNIX => {
                // SAFETY: family AF_UNIX implies a `sockaddr_un` payload with a
                // NUL-terminated path.
                let sun = unsafe { &*raw.cast::<libc::sockaddr_un>() };
                let path = unsafe { CStr::from_ptr(sun.sun_path.as_ptr()) };
                Ok(Self::Unix(PathBuf::from(path.to_str().unwrap_or_default())))
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported socket family {other}"),
            )),
        }
    }
}

fn parse_port(port: &str, whole: &str) -> crate::core::Result<u16> {
    port.parse::<u16>()
        .map_err(|_| Error::parse(format!("invalid port \"{port}\" in \"{whole}\"")))
}

#[cfg(test)]
mod tests {
    use super::InetAddr;

    #[test]
    fn bare_port_expands_to_dual_stack() {
        let addrs = InetAddr::parse("8080").unwrap();
        assert_eq!(addrs.len(), 2);
        assert!(addrs[0].is_wildcard());
        assert!(addrs[1].is_wildcard());
        assert_eq!(addrs[0].port(), Some(8080));
        assert_eq!(addrs[1].port(), Some(8080));
    }

    #[test]
    fn ipv4_host_and_port() {
        let addrs = InetAddr::parse("127.0.0.1:8080").unwrap();
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].display(), "127.0.0.1:8080");
        assert!(!addrs[0].is_wildcard());
    }

    #[test]
    fn ipv6_bracketed() {
        let addrs = InetAddr::parse("[::1]:8080").unwrap();
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].display(), "[::1]:8080");
    }

    #[test]
    fn ipv6_wildcard() {
        let addrs = InetAddr::parse("[::]:80").unwrap();
        assert_eq!(addrs.len(), 1);
        assert!(addrs[0].is_wildcard());
    }

    #[test]
    fn unix_paths() {
        for input in ["/var/run/aegis.sock", "unix:/tmp/aegis.sock"] {
            let addrs = InetAddr::parse(input).unwrap();
            assert_eq!(addrs.len(), 1);
            assert!(addrs[0].is_unix());
            assert!(addrs[0].display().starts_with("unix:/"));
        }
    }

    #[test]
    fn rejects_invalid_inputs() {
        for bad in [
            "",
            "not-an-address",
            "127.0.0.1",
            "localhost:8080",
            "[::1]",
            "0:99999",
            "[nope]:80",
        ] {
            assert!(InetAddr::parse(bad).is_err(), "expected error for {bad:?}");
        }
    }

    #[test]
    fn port_zero_is_allowed() {
        let addrs = InetAddr::parse("0").unwrap();
        assert_eq!(addrs[0].port(), Some(0));
    }

    #[test]
    fn sockaddr_round_trip_v4() {
        let addr = InetAddr::parse("127.0.0.1:8080").unwrap().remove(0);
        let (storage, len) = addr.to_sockaddr();
        let back = InetAddr::from_sockaddr(&storage, len).unwrap();
        assert_eq!(back, addr);
    }

    #[test]
    fn sockaddr_round_trip_v6() {
        let addr = InetAddr::parse("[::1]:8080").unwrap().remove(0);
        let (storage, len) = addr.to_sockaddr();
        let back = InetAddr::from_sockaddr(&storage, len).unwrap();
        assert_eq!(back, addr);
    }

    #[test]
    fn sockaddr_round_trip_unix() {
        let addr = InetAddr::Unix("/tmp/aegis.sock".into());
        let (storage, len) = addr.to_sockaddr();
        let back = InetAddr::from_sockaddr(&storage, len).unwrap();
        assert_eq!(back, addr);
    }
}
