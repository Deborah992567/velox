//! Bidirectional WebSocket relay for reverse-proxy upgrade connections.
//!
//! After the proxy negotiates the `101 Switching Protocols` upgrade with the
//! upstream, this module takes over: it relays every byte between the client
//! and the upstream verbatim — no frame-level decoding or re-encoding — so
//! the two endpoints speak RFC 6455 directly through the proxy.
//!
//! The caller is responsible for disabling socket timeouts on the upstream
//! (and optionally the client) before entering the relay. The relay reads
//! into a fixed buffer and copies in whichever direction has data. The loop
//! terminates when either side closes its write half (EOF) or an I/O error
//! surfaces.

use std::io::{self, Read, Write};

/// Size of one read in each direction during the bidirectional relay.
const WS_BUFFER: usize = 16 * 1024;

/// Relay WebSocket frames bidirectionally between `client` and `upstream`.
///
/// Both sockets must already have completed the HTTP upgrade handshake — the
/// caller has already relayed the `101` response to the client. This function
/// reads from each side and writes to the other until one side signals EOF or
/// an I/O error occurs.
///
/// The caller should clear socket timeouts before entering the relay so that
/// idle WebSocket connections do not time out.
pub fn ws_relay<C, U>(client: &mut C, upstream: &mut U) -> io::Result<()>
where
    C: Read + Write,
    U: Read + Write,
{
    let mut buf = [0u8; WS_BUFFER];
    loop {
        let n = match client.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        };
        if upstream.write_all(&buf[..n]).is_err() {
            return Ok(());
        }

        let n = match upstream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        };
        if client.write_all(&buf[..n]).is_err() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    #[test]
    fn relays_bytes_in_both_directions() {
        let (mut a, mut b) = UnixStream::pair().unwrap();

        // a -> b: "hello"
        a.write_all(b"hello").unwrap();
        let mut buf = [0u8; 5];
        b.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");

        // b -> a: "world"
        b.write_all(b"world").unwrap();
        a.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn relay_ends_on_eof() {
        let (mut client, mut upstream) = UnixStream::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let n = upstream.read(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"ping");
            upstream.write_all(b"pong").unwrap();
        });

        client.write_all(b"ping").unwrap();
        let mut buf = [0u8; 64];
        let n = client.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"pong");
        drop(client);
        server_thread.join().unwrap();
    }

    #[test]
    fn relay_relay_ends_when_upstream_closes() {
        let (mut client, mut upstream) = UnixStream::pair().unwrap();
        upstream.write_all(b"data").unwrap();

        let mut buf = [0u8; 64];
        let n = client.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"data");
        drop(upstream);
        let n = client.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }
}
