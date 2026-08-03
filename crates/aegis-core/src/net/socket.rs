//! Raw socket helpers: creation, options, and non-blocking mode.
//!
//! Everything here operates on file descriptors directly (`libc`); higher
//! layers wrap the results in [`super::Listener`] and [`super::Connection`].

use std::io;
use std::mem::{size_of, size_of_val};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

use super::addr::InetAddr;

/// `SO_REUSEPORT` allows several sockets to bind the same address so each
/// worker can `accept` on its own listener (see ADR 0005).
pub const SO_REUSEPORT: libc::c_int = libc::SO_REUSEPORT;

/// Narrow an OS address-family constant (`AF_INET` = 2, ...) to the platform's
/// `sa_family_t`. All real families fit; a failure here is a programming
/// error, so it panics rather than producing an invalid socket address.
pub fn sa_family(af: i32) -> libc::sa_family_t {
    libc::sa_family_t::try_from(af).expect("socket family fits sa_family_t")
}

/// Narrow a byte length to `socklen_t` (a 32-bit type on every platform we
/// target). Socket addresses are far smaller than 4 GiB, so the conversion is
/// infallible in practice.
pub fn socklen(size: usize) -> libc::socklen_t {
    libc::socklen_t::try_from(size).expect("socket address fits socklen_t")
}

/// Options applied to a listening socket before `bind`.
///
/// Boolean knobs are kept as named fields for readability rather than packed
/// into a bitmask; the count exceeds the `struct_excessive_bools` threshold by
/// design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct SocketOptions {
    /// `SO_REUSEADDR` — allow rebinding while old sockets linger.
    pub reuseaddr: bool,
    /// `SO_REUSEPORT` — share the address across worker processes.
    pub reuseport: bool,
    /// IPv6 only: when binding `[::]:port`, do not also accept IPv4.
    pub ipv6_only: bool,
    /// `SO_KEEPALIVE` on accepted connections.
    pub keepalive: bool,
    /// `TCP_NODELAY` on accepted connections.
    pub nodelay: bool,
    /// Listen backlog passed to `listen(2)`.
    pub backlog: i32,
    /// Whether the socket is created in non-blocking mode.
    pub nonblocking: bool,
}

impl SocketOptions {
    /// Default options: reuse-address on, listen backlog of 511, blocking.
    pub const fn new() -> Self {
        Self {
            reuseaddr: true,
            reuseport: false,
            ipv6_only: false,
            keepalive: false,
            nodelay: false,
            backlog: 511,
            nonblocking: false,
        }
    }

    /// Enable `SO_REUSEPORT`.
    #[must_use]
    pub const fn reuseport(mut self, on: bool) -> Self {
        self.reuseport = on;
        self
    }

    /// Restrict a wildcard IPv6 bind to IPv6 only.
    #[must_use]
    pub const fn ipv6_only(mut self, on: bool) -> Self {
        self.ipv6_only = on;
        self
    }

    /// Enable keep-alive on accepted connections.
    #[must_use]
    pub const fn keepalive(mut self, on: bool) -> Self {
        self.keepalive = on;
        self
    }

    /// Enable `TCP_NODELAY` on accepted connections.
    #[must_use]
    pub const fn nodelay(mut self, on: bool) -> Self {
        self.nodelay = on;
        self
    }

    /// Set the listen backlog.
    #[must_use]
    pub const fn backlog(mut self, backlog: i32) -> Self {
        self.backlog = backlog;
        self
    }

    /// Create the socket in non-blocking mode.
    #[must_use]
    pub const fn nonblocking(mut self, on: bool) -> Self {
        self.nonblocking = on;
        self
    }
}

/// Create a stream socket for the given address family.
pub fn create_socket(addr: &InetAddr) -> io::Result<OwnedFd> {
    let family = match addr {
        InetAddr::V4(_) => libc::AF_INET,
        InetAddr::V6(_) => libc::AF_INET6,
        InetAddr::Unix(_) => libc::AF_UNIX,
    };
    let domain = family;
    // SOCK_CLOEXEC is only available on Linux; elsewhere we set the flag on
    // the returned descriptor or accept the transient inheritance window.
    #[cfg(target_os = "linux")]
    let ty = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let ty = libc::SOCK_STREAM;
    // SAFETY: standard socket(2) syscall with valid arguments.
    let fd = unsafe { libc::socket(domain, ty, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a newly-created, owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Toggle a boolean socket option.
pub fn set_bool_option(
    fd: RawFd,
    level: libc::c_int,
    name: libc::c_int,
    on: bool,
) -> io::Result<()> {
    let value = libc::c_int::from(on);
    // SAFETY: setsockopt with a pointer to a valid c_int.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            std::ptr::from_ref(&value).cast(),
            socklen(size_of_val(&value)),
        )
    };
    if rc != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Set an integer socket option (e.g. buffer sizes).
pub fn set_int_option(
    fd: RawFd,
    level: libc::c_int,
    name: libc::c_int,
    value: libc::c_int,
) -> io::Result<()> {
    // SAFETY: setsockopt with a pointer to a valid c_int.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            std::ptr::from_ref(&value).cast(),
            socklen(size_of_val(&value)),
        )
    };
    if rc != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Enable or disable `O_NONBLOCK` on a descriptor.
pub fn set_nonblocking(fd: RawFd, on: bool) -> io::Result<()> {
    // SAFETY: fcntl F_GETFL reads the descriptor flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let new_flags = if on {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    // SAFETY: fcntl F_SETFL writes the descriptor flags.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, new_flags) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Bind a socket to an address.
pub fn bind(fd: RawFd, addr: &InetAddr) -> io::Result<()> {
    let (storage, len) = addr.to_sockaddr();
    // SAFETY: bind(2) with a valid sockaddr pointer and length.
    let rc = unsafe { libc::bind(fd, std::ptr::addr_of!(storage).cast(), len) };
    if rc != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Mark a bound socket as a passive listener.
pub fn listen(fd: RawFd, backlog: i32) -> io::Result<()> {
    // SAFETY: listen(2) on a valid socket descriptor.
    let rc = unsafe { libc::listen(fd, backlog) };
    if rc != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Read the local address bound to a socket (`getsockname`).
pub fn getsockname(fd: RawFd) -> io::Result<InetAddr> {
    // SAFETY: `storage` is zeroed and big enough for any socket address the
    // kernel may write; `len` is updated to the actual size.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = socklen(size_of::<libc::sockaddr_storage>());
    // SAFETY: getsockname writes into `storage` and updates `len`.
    let rc = unsafe {
        libc::getsockname(
            fd,
            std::ptr::addr_of_mut!(storage).cast(),
            std::ptr::addr_of_mut!(len),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    InetAddr::from_sockaddr(&storage, len)
}

/// Read the peer address of a connected socket (`getpeername`).
pub fn getpeername(fd: RawFd) -> io::Result<InetAddr> {
    // SAFETY: `storage` is zeroed and big enough for any socket address the
    // kernel may write; `len` is updated to the actual size.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = socklen(size_of::<libc::sockaddr_storage>());
    // SAFETY: getpeername writes into `storage` and updates `len`.
    let rc = unsafe {
        libc::getpeername(
            fd,
            std::ptr::addr_of_mut!(storage).cast(),
            std::ptr::addr_of_mut!(len),
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    InetAddr::from_sockaddr(&storage, len)
}

#[cfg(test)]
mod tests {
    use super::{
        SO_REUSEPORT, bind, create_socket, getsockname, listen, set_bool_option, set_nonblocking,
    };
    use crate::net::addr::InetAddr;
    use std::os::fd::{AsFd, AsRawFd};

    #[test]
    fn creates_ipv4_stream_socket() {
        let socket = create_socket(&InetAddr::parse("0").unwrap().remove(0)).unwrap();
        assert!(socket.as_fd().as_raw_fd() > 0);
    }

    #[test]
    fn creates_unix_stream_socket() {
        let socket = create_socket(&InetAddr::Unix("/tmp/aegis-x.sock".into())).unwrap();
        assert!(socket.as_fd().as_raw_fd() > 0);
    }

    #[test]
    fn nonblocking_flag_toggles() {
        let socket = create_socket(&InetAddr::parse("0").unwrap().remove(0)).unwrap();
        let fd = socket.as_fd().as_raw_fd();
        set_nonblocking(fd, true).unwrap();
        // SAFETY: fcntl F_GETFL reads the flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert_ne!(flags & libc::O_NONBLOCK, 0);
        set_nonblocking(fd, false).unwrap();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert_eq!(flags & libc::O_NONBLOCK, 0);
    }

    #[test]
    fn bool_options_apply() {
        let socket = create_socket(&InetAddr::parse("0").unwrap().remove(0)).unwrap();
        let fd = socket.as_fd().as_raw_fd();
        set_bool_option(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, true).unwrap();
        set_bool_option(fd, libc::SOL_SOCKET, SO_REUSEPORT, true).unwrap();
        set_bool_option(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, false).unwrap();
    }

    #[test]
    fn bind_listen_and_query_local_addr() {
        let addr = InetAddr::parse("127.0.0.1:0").unwrap().remove(0);
        let socket = create_socket(&addr).unwrap();
        bind(socket.as_fd().as_raw_fd(), &addr).unwrap();
        listen(socket.as_fd().as_raw_fd(), 16).unwrap();
        let local = getsockname(socket.as_fd().as_raw_fd()).unwrap();
        // Port 0 was replaced by an ephemeral port; the IP is unchanged.
        assert!(local.port().is_some_and(|port| port > 0));
        let (InetAddr::V4(expected), InetAddr::V4(actual)) = (&addr, &local) else {
            panic!("expected IPv4 endpoints");
        };
        assert_eq!(expected.ip(), actual.ip());
    }
}
