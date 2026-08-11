//! Raw socket helpers: creation, options, and non-blocking mode.
//!
//! Everything here operates on file descriptors directly (`libc`); higher
//! layers wrap the results in [`super::Listener`] and [`super::Connection`].

use std::io;
use std::mem::{size_of, size_of_val};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

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

/// Blocking `connect(2)`.
///
/// On a socket that has been placed in non-blocking mode this returns
/// immediately with `io::ErrorKind::WouldBlock`/`InProgress` instead of
/// blocking until the connection completes; callers then poll writability and
/// call [`finish_connect`].
pub fn connect(fd: RawFd, addr: &InetAddr) -> io::Result<()> {
    let (storage, len) = addr.to_sockaddr();
    // SAFETY: connect(2) with a valid sockaddr pointer and length.
    let rc = unsafe { libc::connect(fd, std::ptr::addr_of!(storage).cast(), len) };
    if rc != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Confirm a non-blocking `connect` that reported writability, reading the
/// socket error (`SO_ERROR`) the kernel stashed on the socket.
pub fn finish_connect(fd: RawFd) -> io::Result<()> {
    let mut error: libc::c_int = 0;
    let mut len = socklen(size_of_val(&error));
    // SAFETY: getsockopt with a pointer to a valid c_int and matching length.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            std::ptr::from_mut(&mut error).cast(),
            &raw mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    if error != 0 {
        Err(io::Error::from_raw_os_error(error))
    } else {
        Ok(())
    }
}

/// Which direction a socket timeout applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketTimeoutSide {
    /// `SO_RCVTIMEO` — limits a blocking `read`.
    Read,
    /// `SO_SNDTIMEO` — limits a blocking `write`.
    Write,
}

/// Set (`Some`) or clear (`None`) the receive or send timeout on a socket.
///
/// After expiry a blocking call on the socket fails with
/// `io::ErrorKind::WouldBlock`, which higher layers map onto their own
/// `TimedOut` semantics.
pub fn set_socket_timeout(
    fd: RawFd,
    side: SocketTimeoutSide,
    timeout: Option<Duration>,
) -> io::Result<()> {
    let (level, name) = match side {
        SocketTimeoutSide::Read => (libc::SOL_SOCKET, libc::SO_RCVTIMEO),
        SocketTimeoutSide::Write => (libc::SOL_SOCKET, libc::SO_SNDTIMEO),
    };
    let timeval = timeout.map_or(
        libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        |duration| libc::timeval {
            tv_sec: libc::time_t::try_from(duration.as_secs()).unwrap_or(libc::time_t::MAX),
            tv_usec: libc::suseconds_t::try_from(duration.subsec_micros())
                .unwrap_or(libc::suseconds_t::MAX),
        },
    );
    // SAFETY: setsockopt with a pointer to a valid timeval.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            std::ptr::from_ref(&timeval).cast(),
            socklen(size_of_val(&timeval)),
        )
    };
    if rc != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// What a non-blocking peek found on a connected socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peek {
    /// No data and no EOF pending; the connection is quiescent and reusable.
    Empty,
    /// The peer has already written bytes that nobody consumed — stale
    /// pipelined data; the connection is not reusable.
    Data,
    /// The peer closed (EOF); the connection is dead.
    Eof,
}

/// Inspect a connected socket without blocking and without consuming bytes.
///
/// A `poll(2)` with zero timeout reports readiness, and a `MSG_PEEK` read
/// distinguishes "nothing pending" from "bytes pending" from "peer closed".
/// Used by the upstream pool to weed out idle connections the peer has closed
/// (or that carry leftover bytes) before handing one to the next request.
pub fn peek(fd: RawFd) -> io::Result<Peek> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll(2) with one valid pollfd entry.
    let rc = unsafe { libc::poll(&raw mut pfd, 1, 0) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if rc == 0 {
        return Ok(Peek::Empty);
    }
    let mut byte = 0u8;
    // SAFETY: recv(2) into one valid byte; MSG_PEEK does not consume.
    let n = unsafe {
        libc::recv(
            fd,
            std::ptr::from_mut(&mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    match n.cmp(&0) {
        std::cmp::Ordering::Less => {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                Ok(Peek::Empty)
            } else {
                Err(error)
            }
        }
        std::cmp::Ordering::Equal => Ok(Peek::Eof),
        std::cmp::Ordering::Greater => Ok(Peek::Data),
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
        Peek, SO_REUSEPORT, bind, create_socket, getsockname, listen, peek, set_bool_option,
        set_nonblocking,
    };
    use crate::net::addr::InetAddr;
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, AsRawFd};
    use std::os::unix::net::UnixStream;

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
    fn peek_sees_empty_data_and_eof() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        assert_eq!(peek(a.as_raw_fd()).unwrap(), Peek::Empty);
        b.write_all(b"x").unwrap();
        assert_eq!(peek(a.as_raw_fd()).unwrap(), Peek::Data);
        drop(b);
        // The byte is still there (peeked, not consumed); EOF only surfaces
        // once the buffer is drained.
        assert_eq!(peek(a.as_raw_fd()).unwrap(), Peek::Data);
        let mut buf = [0u8; 1];
        a.read_exact(&mut buf).unwrap();
        assert_eq!(peek(a.as_raw_fd()).unwrap(), Peek::Eof);
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
