//! SNI-driven certificate selection.

use std::collections::HashMap;
use std::sync::Arc;

use rustls::server::ClientHello;
use rustls::server::ResolvesServerCert;
use rustls::sign::CertifiedKey;

use crate::core::error::{Context, ErrorKind, Result};

use super::keypair::KeyPair;

/// A certificate resolver that picks a [`CertifiedKey`] from the SNI value.
///
/// Host names are matched case-insensitively (the SNI value and the stored
/// keys are normalized to lowercase). Requests without SNI, or with an SNI
/// that does not match any configured name, fall back to the default
/// certificate — the nginx behaviour for a `listen ... ssl` block with a
/// `default_server` certificate.
///
/// The resolver is immutable once built; certificate reload rebuilds a fresh
/// resolver (and its [`ServerConfig`](rustls::ServerConfig)) rather than
/// mutating this one.
#[derive(Debug)]
pub struct SniResolver {
    by_name: HashMap<String, Arc<CertifiedKey>>,
    default: Arc<CertifiedKey>,
}

impl SniResolver {
    /// Build a resolver with `default` as the fallback certificate.
    pub fn new(default: KeyPair) -> Result<Self> {
        let default = Arc::new(
            default
                .certified_key()
                .context(ErrorKind::Tls, "building default certificate")?,
        );
        Ok(Self {
            by_name: HashMap::new(),
            default,
        })
    }

    /// Add a certificate for a specific host name (SNI).
    pub fn insert(&mut self, name: impl Into<String>, keypair: KeyPair) -> Result<()> {
        let key = Arc::new(
            keypair
                .certified_key()
                .context(ErrorKind::Tls, "building certificate")?,
        );
        self.by_name.insert(name.into().to_ascii_lowercase(), key);
        Ok(())
    }

    /// Replace the fallback certificate.
    pub fn set_default(&mut self, keypair: KeyPair) -> Result<()> {
        self.default = Arc::new(
            keypair
                .certified_key()
                .context(ErrorKind::Tls, "building default certificate")?,
        );
        Ok(())
    }

    /// The configured SNI host names, sorted for determinism.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.by_name.keys().cloned().collect();
        names.sort();
        names
    }

    /// Whether a host name has a dedicated certificate.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(&name.to_ascii_lowercase())
    }

    /// The number of per-name certificates (excluding the default).
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Whether no per-name certificates are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

impl ResolvesServerCert for SniResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        client_hello.server_name().map_or_else(
            || Some(Arc::clone(&self.default)),
            |name| {
                self.by_name
                    .get(&name.to_ascii_lowercase())
                    .cloned()
                    .or(Some(Arc::clone(&self.default)))
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::SniResolver;
    use crate::tls::keypair::KeyPair;

    fn self_signed_pem(san: &str) -> (Vec<u8>, Vec<u8>) {
        let mut params = rcgen::CertificateParams::new(vec![san.to_string()]).expect("params");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = params.self_signed(&key).expect("cert");
        (cert.pem().into_bytes(), key.serialize_pem().into_bytes())
    }

    fn keypair(san: &str) -> KeyPair {
        let (cert, key) = self_signed_pem(san);
        KeyPair::from_pem(&cert, &key).expect("keypair")
    }

    #[test]
    fn insert_and_lookup_normalize_case() {
        let mut resolver = SniResolver::new(keypair("example.com")).expect("new");
        resolver
            .insert("API.Example.com", keypair("api.example.com"))
            .expect("insert");
        assert!(resolver.contains("api.example.com"));
        assert!(resolver.contains("API.EXAMPLE.COM"));
        assert_eq!(resolver.len(), 1);
        assert!(!resolver.is_empty());
        assert_eq!(resolver.names(), vec!["api.example.com"]);
    }

    #[test]
    fn empty_resolver_has_no_names() {
        let resolver = SniResolver::new(keypair("example.com")).expect("new");
        assert!(resolver.is_empty());
        assert!(resolver.names().is_empty());
    }

    #[test]
    fn default_is_exposed() {
        let mut resolver = SniResolver::new(keypair("example.com")).expect("new");
        resolver
            .set_default(keypair("fallback.example.com"))
            .expect("set default");
        assert!(resolver.is_empty());
    }
}
