//! An accepted TCP or Unix stream connection.

use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};

use super::addr::InetAddr;
use super::socket;

/// A connected stream: either TCP (IPv4/IPv6) or a Unix domain socket.
///
/// Phase 2 provides the descriptor wrapper, address queries, and byte
/// `Read`/`Write`. The reactor-based non-blocking I/O and buffer management
/// arrive in Phases 3–4.
pub struct Connection {
    fd: OwnedFd,
    local: InetAddr,
    peer: InetAddr,
}

impl Connection {
    /// Wrap an already-connected descriptor.
    pub fn from_owned(fd: OwnedFd) -> Self {
        // Best-effort address queries; failures degrade to their worst-case
        // representations rather than failing construction.
        let local = socket::getsockname(fd.as_raw_fd())
            .unwrap_or_else(|_| InetAddr::Unix(std::path::PathBuf::from("unknown")));
        let peer = socket::getpeername(fd.as_raw_fd())
            .unwrap_or_else(|_| InetAddr::Unix(std::path::PathBuf::from("unknown")));
        Self { fd, local, peer }
    }

    /// Re-query the local and peer addresses (used after accept).
    pub fn refresh_addrs(&mut self) -> io::Result<()> {
        self.local = socket::getsockname(self.fd.as_raw_fd())?;
        self.peer = socket::getpeername(self.fd.as_raw_fd())?;
        Ok(())
    }

    /// The peer address (the remote end).
    pub fn peer_addr(&self) -> io::Result<InetAddr> {
        Ok(self.peer.clone())
    }

    /// The local address (our end of the connection).
    pub fn local_addr(&self) -> io::Result<InetAddr> {
        Ok(self.local.clone())
    }

    /// Enable or disable `TCP_NODELAY` (no-op for Unix sockets).
    pub fn set_nodelay(&self, on: bool) -> io::Result<()> {
        if self.is_tcp() {
            socket::set_bool_option(self.as_raw_fd(), libc::IPPROTO_TCP, libc::TCP_NODELAY, on)?;
        }
        Ok(())
    }

    /// Enable or disable `SO_KEEPALIVE`.
    pub fn set_keepalive(&self, on: bool) -> io::Result<()> {
        socket::set_bool_option(self.as_raw_fd(), libc::SOL_SOCKET, libc::SO_KEEPALIVE, on)
    }

    /// Enable non-blocking mode on this connection.
    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        socket::set_nonblocking(self.as_raw_fd(), on)
    }

    /// Whether this is a TCP connection (as opposed to a Unix socket).
    pub const fn is_tcp(&self) -> bool {
        matches!(self.local, InetAddr::V4(_) | InetAddr::V6(_))
    }
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: read(2) into the caller-provided buffer.
        let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(usize::try_from(n).expect("non-negative read count"))
        }
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: write(2) from the caller-provided buffer.
        let n = unsafe { libc::write(self.fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(usize::try_from(n).expect("non-negative write count"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsRawFd for Connection {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl AsFd for Connection {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("fd", &self.fd.as_raw_fd())
            .field("local", &self.local.display())
            .field("peer", &self.peer.display())
            .finish()
    }
}
