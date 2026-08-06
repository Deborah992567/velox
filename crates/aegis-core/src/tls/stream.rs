//! A TLS connection wrapper over an underlying transport.

use std::io::{self, Read, Write};
use std::sync::Arc;

use rustls::{ProtocolVersion, ServerConfig, ServerConnection};

use crate::core::error::{Context, ErrorKind, Result};

/// A terminated TLS connection: a rustls [`ServerConnection`] layered over a
/// transport (`TCP`, Unix socket, …).
///
/// Constructing a [`TlsStream`] starts the handshake; [`TlsStream::handshake`]
/// drives it to completion (blocking). After that the stream implements
/// [`Read`]/[`Write`] for plaintext, and the negotiated ALPN protocol and TLS
/// version are queryable.
#[derive(Debug)]
pub struct TlsStream<S> {
    conn: ServerConnection,
    transport: S,
}

impl<S> TlsStream<S> {
    /// Start a TLS handshake on an existing transport.
    pub fn new(config: Arc<ServerConfig>, transport: S) -> Result<Self> {
        let conn =
            ServerConnection::new(config).context(ErrorKind::Tls, "starting TLS handshake")?;
        Ok(Self { conn, transport })
    }

    /// The ALPN protocol negotiated with the client, if any.
    #[must_use]
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.conn.alpn_protocol()
    }

    /// The TLS protocol version negotiated with the client, if the handshake
    /// has completed.
    #[must_use]
    pub fn negotiated_version(&self) -> Option<ProtocolVersion> {
        self.conn.protocol_version()
    }

    /// Whether the handshake is still in progress.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn is_handshaking(&self) -> bool {
        self.conn.is_handshaking()
    }

    /// The underlying transport.
    #[must_use]
    pub const fn get_ref(&self) -> &S {
        &self.transport
    }

    /// A mutable borrow of the underlying transport.
    #[must_use]
    pub const fn get_mut(&mut self) -> &mut S {
        &mut self.transport
    }

    /// Consume the stream, returning the underlying transport.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.transport
    }
}

impl<S: Read + Write> TlsStream<S> {
    /// Drive the TLS handshake to completion over the transport.
    pub fn handshake(&mut self) -> Result<()> {
        while self.conn.is_handshaking() {
            self.conn
                .complete_io(&mut self.transport)
                .context(ErrorKind::Tls, "TLS handshake failed")?;
        }
        Ok(())
    }
}

impl<S: Read + Write> Read for TlsStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        rustls::Stream::new(&mut self.conn, &mut self.transport).read(buf)
    }
}

impl<S: Read + Write> Write for TlsStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        rustls::Stream::new(&mut self.conn, &mut self.transport).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        rustls::Stream::new(&mut self.conn, &mut self.transport).flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::Arc;

    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, ClientConnection, RootCertStore};

    use crate::tls::config::{TlsConfig, TlsServerOptions};
    use crate::tls::keypair::KeyPair;
    use crate::tls::resolver::SniResolver;
    use crate::tls::stream::TlsStream;

    /// Generate a self-signed certificate for a SAN and return its key pair
    /// plus the certificate (for client trust roots).
    fn make_cert(san: &str) -> (KeyPair, rustls::pki_types::CertificateDer<'static>) {
        let params = rcgen::CertificateParams::new(vec![san.to_string()]).expect("params");
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = params.self_signed(&key).expect("cert");
        let cert_pem = cert.pem();
        let cert_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .next()
            .expect("one cert")
            .expect("parse cert");
        let keypair = KeyPair::from_pem(cert_pem.as_bytes(), key.serialize_pem().as_bytes())
            .expect("keypair");
        (keypair, cert_der)
    }

    fn client_config(roots: &[rustls::pki_types::CertificateDer<'static>]) -> ClientConfig {
        let mut root_store = RootCertStore::empty();
        for root in roots {
            root_store.add(root.clone()).expect("add root");
        }
        let mut config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
                .expect("versions")
                .with_root_certificates(root_store)
                .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        config
    }

    fn tls_config_with(cert: KeyPair) -> TlsConfig {
        TlsConfig::build(
            SniResolver::new(cert).expect("resolver"),
            TlsServerOptions::default(),
        )
        .expect("config")
    }

    fn server_name(name: &'static str) -> ServerName<'static> {
        ServerName::try_from(name).expect("server name")
    }

    #[test]
    fn handshake_negotiates_version_alpn_and_echoes() {
        let (keypair, ca) = make_cert("example.com");
        let config = tls_config_with(keypair);
        let server_config = Arc::clone(config.server_config());

        let (mut client_sock, server_sock) = std::os::unix::net::UnixStream::pair().expect("pair");
        let mut server = TlsStream::new(server_config, server_sock).expect("server stream");
        let handle = std::thread::spawn(move || {
            server.handshake().expect("server handshake");
            assert_eq!(server.alpn_protocol(), Some(&b"http/1.1"[..]));
            assert_eq!(
                server.negotiated_version(),
                Some(rustls::ProtocolVersion::TLSv1_3)
            );
            let mut buf = [0u8; 5];
            server.read_exact(&mut buf).expect("read");
            server.write_all(&buf).expect("echo");
            server.flush().expect("flush");
        });

        let mut client =
            ClientConnection::new(Arc::new(client_config(&[ca])), server_name("example.com"))
                .expect("client");
        client.complete_io(&mut client_sock).expect("handshake");
        assert_eq!(client.alpn_protocol(), Some(&b"http/1.1"[..]));
        assert_eq!(
            client.protocol_version(),
            Some(rustls::ProtocolVersion::TLSv1_3)
        );

        client.writer().write_all(b"hello").expect("write");
        client.complete_io(&mut client_sock).expect("flush");
        let mut buf = [0u8; 5];
        rustls::Stream::new(&mut client, &mut client_sock)
            .read_exact(&mut buf)
            .expect("read plaintext");
        assert_eq!(&buf, b"hello");

        handle.join().expect("server thread");
    }

    #[test]
    fn sni_selects_per_name_certificate() {
        let (default_kp, default_ca) = make_cert("example.com");
        let (named_kp, named_ca) = make_cert("foo.example.com");
        let mut resolver = SniResolver::new(default_kp).expect("resolver");
        resolver
            .insert("foo.example.com", named_kp)
            .expect("insert");
        let config = TlsConfig::build(resolver, TlsServerOptions::default()).expect("config");
        let server_config = Arc::clone(config.server_config());

        let (mut client_sock, server_sock) = std::os::unix::net::UnixStream::pair().expect("pair");
        let handle = std::thread::spawn(move || {
            TlsStream::new(server_config, server_sock)
                .expect("stream")
                .handshake()
                .expect("handshake");
        });

        let mut client = ClientConnection::new(
            Arc::new(client_config(&[default_ca.clone(), named_ca.clone()])),
            server_name("foo.example.com"),
        )
        .expect("client");
        client.complete_io(&mut client_sock).expect("handshake");

        let presented = client
            .peer_certificates()
            .expect("peer certs")
            .first()
            .expect("a cert");
        assert_eq!(presented, &named_ca);
        handle.join().expect("server thread");
    }

    #[test]
    fn unknown_sni_falls_back_to_default_certificate() {
        let (default_kp, default_ca) = make_cert("*.example.com");
        let (other_kp, other_ca) = make_cert("foo.example.com");
        let mut resolver = SniResolver::new(default_kp).expect("resolver");
        resolver
            .insert("foo.example.com", other_kp)
            .expect("insert");
        let config = TlsConfig::build(resolver, TlsServerOptions::default()).expect("config");
        let server_config = Arc::clone(config.server_config());

        let (mut client_sock, server_sock) = std::os::unix::net::UnixStream::pair().expect("pair");
        let handle = std::thread::spawn(move || {
            TlsStream::new(server_config, server_sock)
                .expect("stream")
                .handshake()
                .expect("handshake");
        });

        let mut client = ClientConnection::new(
            Arc::new(client_config(&[default_ca.clone(), other_ca])),
            server_name("unknown.example.com"),
        )
        .expect("client");
        client.complete_io(&mut client_sock).expect("handshake");

        let presented = client
            .peer_certificates()
            .expect("peer certs")
            .first()
            .expect("a cert");
        assert_eq!(presented, &default_ca);
        handle.join().expect("server thread");
    }

    #[test]
    fn session_ticket_is_issued_and_reconnect_succeeds() {
        let (keypair, ca) = make_cert("example.com");
        let config = tls_config_with(keypair);
        let server_config = Arc::clone(config.server_config());

        let (mut client_sock_1, server_sock_1) =
            std::os::unix::net::UnixStream::pair().expect("pair");
        let (mut client_sock_2, server_sock_2) =
            std::os::unix::net::UnixStream::pair().expect("pair");
        let handle = std::thread::spawn(move || {
            for server_sock in [server_sock_1, server_sock_2] {
                let mut stream =
                    TlsStream::new(Arc::clone(&server_config), server_sock).expect("stream");
                stream.handshake().expect("handshake");
                let mut buf = [0u8; 5];
                stream.read_exact(&mut buf).expect("read");
                stream.write_all(&buf).expect("echo");
                stream.flush().expect("flush");
            }
        });

        let client_config = Arc::new(client_config(&[ca]));
        let name = server_name("example.com");

        let mut first =
            ClientConnection::new(Arc::clone(&client_config), name.clone()).expect("c1");
        first.complete_io(&mut client_sock_1).expect("c1 handshake");
        first.writer().write_all(b"hello").expect("write");
        first.complete_io(&mut client_sock_1).expect("c1 flush");
        first.complete_io(&mut client_sock_1).expect("c1 recv");
        assert!(
            first.tls13_tickets_received() >= 1,
            "server should issue a TLS 1.3 session ticket for resumption"
        );
        drop(first);

        let mut second = ClientConnection::new(Arc::clone(&client_config), name).expect("c2");
        second
            .complete_io(&mut client_sock_2)
            .expect("c2 handshake");
        assert_eq!(second.alpn_protocol(), Some(&b"http/1.1"[..]));
        second.writer().write_all(b"hello").expect("write");
        second.complete_io(&mut client_sock_2).expect("c2 flush");

        handle.join().expect("server thread");
    }

    #[test]
    fn reload_serves_replacement_certificates() {
        let (first_kp, first_ca) = make_cert("example.com");
        let mut config = tls_config_with(first_kp);

        let (mut client_sock, server_sock) = std::os::unix::net::UnixStream::pair().expect("pair");
        let server_config = Arc::clone(config.server_config());
        let handle = std::thread::spawn(move || {
            TlsStream::new(server_config, server_sock)
                .expect("stream")
                .handshake()
                .expect("handshake");
        });
        let mut before = ClientConnection::new(
            Arc::new(client_config(std::slice::from_ref(&first_ca))),
            server_name("example.com"),
        )
        .expect("client");
        before.complete_io(&mut client_sock).expect("handshake");
        let before_cert = before
            .peer_certificates()
            .expect("peer certs")
            .first()
            .expect("a cert")
            .clone();
        assert_eq!(before_cert, first_ca);
        handle.join().expect("server thread");

        let (replacement_kp, replacement_ca) = make_cert("reloaded.example.com");
        config
            .reload(SniResolver::new(replacement_kp).expect("resolver"))
            .expect("reload");

        let (mut client_sock, server_sock) = std::os::unix::net::UnixStream::pair().expect("pair");
        let server_config = Arc::clone(config.server_config());
        let handle = std::thread::spawn(move || {
            TlsStream::new(server_config, server_sock)
                .expect("stream")
                .handshake()
                .expect("handshake");
        });
        let mut after = ClientConnection::new(
            Arc::new(client_config(std::slice::from_ref(&replacement_ca))),
            server_name("reloaded.example.com"),
        )
        .expect("client");
        after.complete_io(&mut client_sock).expect("handshake");
        let after_cert = after
            .peer_certificates()
            .expect("peer certs")
            .first()
            .expect("a cert")
            .clone();
        assert_eq!(after_cert, replacement_ca);
        handle.join().expect("server thread");
    }
}
