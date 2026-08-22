//! TLS termination with rustls.
//!
//! Phase 8 of the roadmap: terminate TLS on accepted connections with secure
//! defaults, per-`server_name` certificate selection via SNI, session
//! resumption, and rebuildable configuration for certificate reload.
//!
//! The module is split into four pieces:
//!
//! - [`keypair`] — loading certificate chains and private keys from PEM.
//! - [`resolver`] — an SNI-driven certificate resolver (rustls's
//!   [`ResolvesServerCert`](rustls::server::ResolvesServerCert) trait).
//! - [`config`] — building a rustls [`ServerConfig`](rustls::ServerConfig)
//!   with secure defaults, ALPN, protocol versions, and session resumption,
//!   plus certificate reload.
//! - [`stream`] — the [`TlsStream`] handshake/read/write wrapper over an
//!   underlying transport.
//!
//! The cryptography comes from rustls (TLS 1.2 + 1.3) backed by the `ring`
//! provider per ADR 0003; no cryptographic primitives are implemented here.

pub mod config;
pub mod keypair;
pub mod resolver;
pub mod session_cache;
pub mod stream;

pub use config::{TlsConfig, TlsServerOptions, TlsVersion};
pub use keypair::KeyPair;
pub use resolver::SniResolver;
pub use stream::TlsStream;
