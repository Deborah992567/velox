//! Outbound (client) connections to upstream servers.
//!
//! Phase 2 delivered accepted [`Connection`]s; a reverse proxy also needs the
//! connecting side. [`connect`] is a plain blocking `connect(2)`; the proxy
//! layer uses [`connect_with_timeout`], which starts a non-blocking connect,
//! waits on the socket with `poll(2)` up to a deadline, and confirms the
//! result with `SO_ERROR` — so an unreachable upstream fails fast instead of
//! hanging on the kernel's multi-minute default.

use std::io;
use std::os::fd::AsRawFd;
use std::time::Duration;

use super::socket::{connect as raw_connect, create_socket, finish_connect, set_nonblocking};
use super::{Connection, InetAddr};

/// How a non-blocking connect attempt left the socket.
#[derive(Debug)]
pub enum ConnectState {
    /// The connection completed immediately.
    Connected(Connection),
    /// The connection is still being established; poll writability and then
    /// confirm with [`finish_connect`].
    InProgress(Connection),
}

/// A blocking connect to `addr`.
pub fn connect(addr: &InetAddr) -> io::Result<Connection> {
    let fd = create_socket(addr)?;
    raw_connect(fd.as_raw_fd(), addr)?;
    Ok(Connection::from_owned(fd))
}

/// Start a non-blocking connect to `addr`.
///
/// The returned connection is in non-blocking mode in both cases, so the
/// caller must switch it back to blocking mode (or keep driving it with the
/// reactor) once it is usable.
pub fn connect_nonblocking(addr: &InetAddr) -> io::Result<ConnectState> {
    let fd = create_socket(addr)?;
    set_nonblocking(fd.as_raw_fd(), true)?;
    match raw_connect(fd.as_raw_fd(), addr) {
        Ok(()) => Ok(ConnectState::Connected(Connection::from_owned(fd))),
        Err(error) if is_in_progress(&error) => {
            Ok(ConnectState::InProgress(Connection::from_owned(fd)))
        }
        Err(error) => Err(error),
    }
}

/// Wait up to `timeout` for a connect to `addr`, returning a blocking
/// connection on success.
pub fn connect_with_timeout(addr: &InetAddr, timeout: Duration) -> io::Result<Connection> {
    let connection = match connect_nonblocking(addr)? {
        ConnectState::Connected(connection) => {
            set_nonblocking(connection.as_raw_fd(), false)?;
            return Ok(connection);
        }
        ConnectState::InProgress(connection) => connection,
    };
    let mut pollfd = libc::pollfd {
        fd: connection.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: poll(2) with one valid pollfd entry.
    let rc = unsafe { libc::poll(&raw mut pollfd, 1, millis) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if rc == 0 {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "upstream connect timed out",
        ));
    }
    finish_connect(connection.as_raw_fd())?;
    set_nonblocking(connection.as_raw_fd(), false)?;
    Ok(connection)
}

/// Whether an error means "the non-blocking connect is still running":
/// `EAGAIN` (already mapped to `WouldBlock`) or `EINPROGRESS`.
fn is_in_progress(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || matches!(error.raw_os_error(), Some(code) if code == libc::EINPROGRESS)
}

#[cfg(test)]
mod tests {
    use super::{ConnectState, connect, connect_nonblocking, connect_with_timeout};
    use crate::net::Listener;
    use crate::net::addr::InetAddr;
    use crate::net::socket::SocketOptions;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    #[test]
    fn blocking_connect_to_unix_listener() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connect.sock");
        let listener = Listener::bind(&InetAddr::Unix(path.clone()), SocketOptions::new()).unwrap();

        let mut conn = connect(&InetAddr::Unix(path)).unwrap();
        let mut server = listener.accept().unwrap();
        conn.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[test]
    fn nonblocking_connect_eventually_connects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("async.sock");
        let listener = Listener::bind(&InetAddr::Unix(path.clone()), SocketOptions::new()).unwrap();

        let state = connect_nonblocking(&InetAddr::Unix(path)).unwrap();
        let mut conn = match state {
            ConnectState::Connected(conn) => conn,
            ConnectState::InProgress(conn) => {
                finish_poll(listener.as_raw_fd());
                conn
            }
        };
        let mut server = listener.accept().unwrap();
        conn.write_all(b"yo").unwrap();
        let mut buf = [0u8; 2];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"yo");
    }

    #[test]
    fn connect_with_timeout_honors_blocking_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timeout.sock");
        let listener = Listener::bind(&InetAddr::Unix(path.clone()), SocketOptions::new()).unwrap();

        let mut conn = connect_with_timeout(&InetAddr::Unix(path), Duration::from_secs(2)).unwrap();
        let mut server = listener.accept().unwrap();
        // The returned connection must be blocking again: a raw read parks.
        server.write_all(b"hi").unwrap();
        let mut buf = [0u8; 2];
        conn.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hi");
    }

    #[test]
    fn connect_to_missing_unix_socket_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.sock");
        let result = connect_with_timeout(&InetAddr::Unix(path), Duration::from_secs(1));
        assert!(result.is_err());
    }

    /// Confirm a non-blocking connect by polling the listener (which becomes
    /// readable once a connection arrives) — a deterministic stand-in for the
    /// caller polling the connecting socket for writability.
    fn finish_poll(listener_fd: i32) {
        let mut pfd = libc::pollfd {
            fd: listener_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let mut attempts = 0;
        while attempts < 100 {
            // SAFETY: poll(2) with one valid pollfd entry.
            let rc = unsafe { libc::poll(&raw mut pfd, 1, 100) };
            if rc > 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
            attempts += 1;
        }
        panic!("connect never became ready");
    }
}
