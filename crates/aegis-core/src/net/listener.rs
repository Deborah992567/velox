//! Listening sockets for incoming connections.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

use super::socket::{SocketOptions, create_socket, getsockname};
use super::{Connection, InetAddr};

/// A bound, listening stream socket.
pub struct Listener {
    fd: OwnedFd,
    options: SocketOptions,
    local: InetAddr,
}

impl Listener {
    /// Create a socket for `addr`, apply options, bind, and start listening.
    ///
    /// For wildcard addresses both the IPv4 and IPv6 wildcards must be bound
    /// separately; each call binds one endpoint.
    pub fn bind(addr: &InetAddr, options: SocketOptions) -> io::Result<Self> {
        let fd = create_socket(addr)?;
        let raw = fd.as_raw_fd();

        if options.reuseaddr {
            super::socket::set_bool_option(raw, libc::SOL_SOCKET, libc::SO_REUSEADDR, true)?;
        }
        if options.reuseport {
            super::socket::set_bool_option(
                raw,
                libc::SOL_SOCKET,
                super::socket::SO_REUSEPORT,
                true,
            )?;
        }
        if options.ipv6_only && matches!(addr, InetAddr::V6(_)) {
            super::socket::set_bool_option(raw, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, true)?;
        }
        if options.nonblocking {
            super::socket::set_nonblocking(raw, true)?;
        }

        super::socket::bind(raw, addr)?;
        super::socket::listen(raw, options.backlog)?;
        let local = getsockname(raw)?;

        Ok(Self { fd, options, local })
    }

    /// Accept one pending connection, applying connection options.
    ///
    /// The listener must be non-blocking (or a readiness check performed) for
    /// this to return `WouldBlock` rather than block.
    pub fn accept(&self) -> io::Result<Connection> {
        // SAFETY: accept(2) on a listening descriptor; ownership of the new
        // descriptor is transferred to `Connection`.
        let raw = unsafe {
            libc::accept(
                self.fd.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a freshly-accepted descriptor that we now own.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut connection = Connection::from_owned(fd);
        if self.options.nonblocking {
            super::socket::set_nonblocking(connection.as_raw_fd(), true)?;
        }
        if self.options.keepalive {
            super::socket::set_bool_option(
                connection.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_KEEPALIVE,
                true,
            )?;
        }
        if self.options.nodelay {
            super::socket::set_bool_option(
                connection.as_raw_fd(),
                libc::IPPROTO_TCP,
                libc::TCP_NODELAY,
                true,
            )?;
        }
        connection.refresh_addrs()?;
        Ok(connection)
    }

    /// The address this listener is actually bound to (ephemeral port
    /// resolved).
    pub const fn local_addr(&self) -> &InetAddr {
        &self.local
    }

    /// The options the listener was created with.
    pub const fn options(&self) -> &SocketOptions {
        &self.options
    }
}

impl AsRawFd for Listener {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl AsFd for Listener {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listener")
            .field("fd", &self.fd.as_raw_fd())
            .field("local", &self.local.display())
            .field("options", &self.options)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::{Connection, Listener};
    use crate::net::addr::InetAddr;
    use crate::net::socket::SocketOptions;

    fn spin_accept(listener: &Listener, tries: usize) -> std::io::Result<Connection> {
        let mut last = None;
        for _ in 0..tries {
            match listener.accept() {
                Ok(conn) => return Ok(conn),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("accept never became ready")))
    }

    #[test]
    fn ipv4_accept_round_trip() {
        let addr = InetAddr::parse("127.0.0.1:0").unwrap().remove(0);
        let listener = Listener::bind(&addr, SocketOptions::new().nonblocking(true)).unwrap();
        let port = listener.local_addr().port().unwrap();

        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.set_nonblocking(false).unwrap();

        let mut connection = spin_accept(&listener, 100).unwrap();
        connection.write_all(b"ping").unwrap();

        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
        let expected = format!("127.0.0.1:{}", client.local_addr().unwrap().port());
        assert_eq!(connection.peer_addr().unwrap().display(), expected);
    }

    #[test]
    fn unix_accept_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aegis.sock");
        let listener = Listener::bind(
            &InetAddr::Unix(path.clone()),
            SocketOptions::new().nonblocking(true),
        )
        .unwrap();

        let mut client = std::os::unix::net::UnixStream::connect(&path).unwrap();

        let mut connection = spin_accept(&listener, 100).unwrap();
        connection.write_all(b"unix").unwrap();

        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"unix");
    }

    #[test]
    fn reuseport_allows_second_bind() {
        let addr = InetAddr::parse("127.0.0.1:0").unwrap().remove(0);
        let first = Listener::bind(&addr, SocketOptions::new().reuseport(true)).unwrap();
        let port = first.local_addr().port().unwrap();
        let second_addr = InetAddr::parse(&format!("127.0.0.1:{port}"))
            .unwrap()
            .remove(0);
        // Same address+port, SO_REUSEPORT on both — must succeed.
        let second = Listener::bind(&second_addr, SocketOptions::new().reuseport(true)).unwrap();
        assert_eq!(second.local_addr().port(), first.local_addr().port());
    }

    #[test]
    fn non_reuseport_bind_conflicts() {
        let addr = InetAddr::parse("127.0.0.1:0").unwrap().remove(0);
        let first = Listener::bind(&addr, SocketOptions::new()).unwrap();
        let port = first.local_addr().port().unwrap();
        let second_addr = InetAddr::parse(&format!("127.0.0.1:{port}"))
            .unwrap()
            .remove(0);
        let result = Listener::bind(&second_addr, SocketOptions::new());
        assert!(result.is_err());
    }
}
