//! Building rustls server configurations with secure defaults.

use std::sync::Arc;

use rustls::server::ServerSessionMemoryCache;
use rustls::{ServerConfig, SupportedProtocolVersion};

use crate::core::error::{Context, Error, ErrorKind, Result};

use super::resolver::SniResolver;

/// A TLS protocol version that may be negotiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsVersion {
    /// TLS 1.2
    Tls12,
    /// TLS 1.3
    Tls13,
}

impl TlsVersion {
    /// The rustls protocol version descriptor.
    #[must_use]
    pub const fn protocol(self) -> &'static SupportedProtocolVersion {
        match self {
            Self::Tls12 => &rustls::version::TLS12,
            Self::Tls13 => &rustls::version::TLS13,
        }
    }
}

/// Tunables for building a [`ServerConfig`].
#[derive(Debug, Clone)]
pub struct TlsServerOptions {
    /// The protocol versions to offer, in preference order.
    pub versions: Vec<TlsVersion>,
    /// The ALPN protocol names to advertise, in preference order.
    pub alpn: Vec<Vec<u8>>,
    /// The maximum size of the TLS 1.2 session cache.
    pub session_cache_size: usize,
    /// Whether session resumption (TLS 1.2 tickets + 1.3 tickets) is enabled.
    pub resumption: bool,
}

impl Default for TlsServerOptions {
    fn default() -> Self {
        Self {
            versions: vec![TlsVersion::Tls13, TlsVersion::Tls12],
            alpn: vec![b"http/1.1".to_vec()],
            session_cache_size: 256,
            resumption: true,
        }
    }
}

/// An immutable, rebuildable TLS server configuration.
///
/// Owns a rustls [`ServerConfig`] (itself immutable and cheaply cloneable via
/// [`Arc`]). Certificate reload rebuilds a fresh resolver and configuration
/// with the same [`TlsServerOptions`]; the old configuration is kept alive by
/// any in-flight connections until they drain.
#[derive(Debug)]
pub struct TlsConfig {
    options: TlsServerOptions,
    resolver: Arc<SniResolver>,
    server_config: Arc<ServerConfig>,
}

impl TlsConfig {
    /// Build a server configuration for the given SNI resolver and options.
    pub fn build(resolver: SniResolver, options: TlsServerOptions) -> Result<Self> {
        if options.versions.is_empty() {
            return Err(Error::new(
                ErrorKind::Tls,
                "at least one TLS version is required",
            ));
        }
        let resolver = Arc::new(resolver);
        let server_config = build_server_config(Arc::clone(&resolver), &options)?;
        Ok(Self {
            options,
            resolver,
            server_config: Arc::new(server_config),
        })
    }

    /// Rebuild the configuration with a new certificate resolver (certificate
    /// reload). The options are preserved; connections already using the old
    /// configuration continue with it.
    pub fn reload(&mut self, resolver: SniResolver) -> Result<()> {
        let resolver = Arc::new(resolver);
        self.server_config = Arc::new(build_server_config(Arc::clone(&resolver), &self.options)?);
        self.resolver = resolver;
        Ok(())
    }

    /// The immutable rustls configuration.
    #[must_use]
    pub const fn server_config(&self) -> &Arc<ServerConfig> {
        &self.server_config
    }

    /// The certificate resolver in use.
    #[must_use]
    pub const fn resolver(&self) -> &Arc<SniResolver> {
        &self.resolver
    }

    /// The options the configuration was built from.
    #[must_use]
    pub const fn options(&self) -> &TlsServerOptions {
        &self.options
    }
}

fn build_server_config(
    resolver: Arc<SniResolver>,
    options: &TlsServerOptions,
) -> Result<ServerConfig> {
    let versions: Vec<&'static SupportedProtocolVersion> =
        options.versions.iter().map(|v| v.protocol()).collect();
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&versions)
        .context(ErrorKind::Tls, "configuring TLS protocol versions")?
        .with_no_client_auth()
        .with_cert_resolver(resolver);

    config.alpn_protocols.clone_from(&options.alpn);
    if options.resumption {
        config.ticketer = rustls::crypto::ring::Ticketer::new()
            .context(ErrorKind::Tls, "creating session ticket key")?;
        config.session_storage = ServerSessionMemoryCache::new(options.session_cache_size);
    } else {
        config.ticketer = Arc::new(NoTickets);
        config.session_storage = Arc::new(NoSessionStorage);
    }
    Ok(config)
}

/// A ticketer that produces no tickets; used when resumption is disabled.
#[derive(Debug)]
struct NoTickets;

impl rustls::server::ProducesTickets for NoTickets {
    fn enabled(&self) -> bool {
        false
    }

    fn lifetime(&self) -> u32 {
        0
    }

    fn encrypt(&self, _plain: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn decrypt(&self, _cipher: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

/// A session storage that stores nothing; used when resumption is disabled.
#[derive(Debug)]
struct NoSessionStorage;

impl rustls::server::StoresServerSessions for NoSessionStorage {
    fn put(&self, _key: Vec<u8>, _value: Vec<u8>) -> bool {
        false
    }

    fn get(&self, _key: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn take(&self, _key: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn can_cache(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{TlsConfig, TlsServerOptions, TlsVersion};
    use crate::tls::keypair::KeyPair;
    use crate::tls::resolver::SniResolver;

    fn keypair(san: &str) -> KeyPair {
        let mut params = rcgen::CertificateParams::new(vec![san.to_string()]).expect("params");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = params.self_signed(&key).expect("cert");
        let cert_pem = cert.pem();
        KeyPair::from_pem(cert_pem.as_bytes(), key.serialize_pem().as_bytes()).expect("keypair")
    }

    fn resolver(san: &str) -> SniResolver {
        SniResolver::new(keypair(san)).expect("resolver")
    }

    #[test]
    fn builds_config_with_secure_defaults() {
        let config =
            TlsConfig::build(resolver("example.com"), TlsServerOptions::default()).expect("build");
        let server = config.server_config();
        assert_eq!(server.alpn_protocols, vec![b"http/1.1".to_vec()]);
        assert!(config.options().resumption);
    }

    #[test]
    fn resumption_can_be_disabled() {
        let options = TlsServerOptions {
            resumption: false,
            ..TlsServerOptions::default()
        };
        let config = TlsConfig::build(resolver("example.com"), options).expect("build");
        assert!(!config.options().resumption);
    }

    #[test]
    fn version_list_can_be_restricted() {
        let options = TlsServerOptions {
            versions: vec![TlsVersion::Tls13],
            ..TlsServerOptions::default()
        };
        let config = TlsConfig::build(resolver("example.com"), options).expect("build");
        assert_eq!(config.options().versions, [TlsVersion::Tls13]);
    }

    #[test]
    fn empty_version_list_is_rejected() {
        let options = TlsServerOptions {
            versions: Vec::new(),
            ..TlsServerOptions::default()
        };
        let error = TlsConfig::build(resolver("example.com"), options).expect_err("empty versions");
        assert_eq!(error.kind(), crate::core::error::ErrorKind::Tls);
    }

    #[test]
    fn reload_rebuilds_with_new_resolver() {
        let mut config =
            TlsConfig::build(resolver("example.com"), TlsServerOptions::default()).expect("build");
        assert!(config.resolver().is_empty());

        let mut updated = resolver("example.com");
        updated
            .insert("api.example.com", keypair("api.example.com"))
            .expect("insert");
        config.reload(updated).expect("reload");
        assert!(config.resolver().contains("api.example.com"));
        assert_eq!(config.options().alpn, vec![b"http/1.1".to_vec()]);
    }
}
