//! Certificate chains and private keys for TLS.

use std::io::BufReader;
use std::path::Path;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::{CertifiedKey, SigningKey};

use crate::core::error::{Context, Error, ErrorKind, Result};

/// A certificate chain plus its matching private key.
///
/// PEM loading is strict: the chain must contain at least one certificate and
/// the key file must yield a usable key. The key must be supported by the
/// `ring`-based signing backend (RSA, ECDSA, or Ed25519 in PKCS#8/PKCS#1 form).
///
/// A mismatched key is not detected here — rustls surfaces the mismatch as a
/// handshake failure, matching how both nginx and rustls behave.
#[derive(Debug)]
pub struct KeyPair {
    cert_chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl KeyPair {
    /// Load a key pair from in-memory PEM: a certificate chain and a private
    /// key.
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Self> {
        let cert_chain = read_certs(cert_pem)?;
        let key = read_key(key_pem)?;
        let pair = Self { cert_chain, key };
        pair.validate()?;
        Ok(pair)
    }

    /// Load a key pair from PEM files on disk.
    pub fn from_pem_files(cert_path: &Path, key_path: &Path) -> Result<Self> {
        let cert_pem = std::fs::read(cert_path)
            .context(ErrorKind::Io, format!("reading {}", cert_path.display()))?;
        let key_pem = std::fs::read(key_path)
            .context(ErrorKind::Io, format!("reading {}", key_path.display()))?;
        Self::from_pem(&cert_pem, &key_pem)
    }

    /// The certificate chain, leaf first.
    #[must_use]
    pub fn cert_chain(&self) -> &[CertificateDer<'static>] {
        &self.cert_chain
    }

    /// Validate that this key pair is loadable: a non-empty chain and a key
    /// supported by the signing backend.
    pub fn validate(&self) -> Result<()> {
        if self.cert_chain.is_empty() {
            return Err(Error::new(
                ErrorKind::Tls,
                "certificate chain is empty: no certificates found",
            ));
        }
        if !self.cert_chain.iter().all(|cert| !cert.as_ref().is_empty()) {
            return Err(Error::new(
                ErrorKind::Tls,
                "certificate chain contains an empty certificate",
            ));
        }
        signing_key(&self.key).context(
            ErrorKind::Tls,
            "private key is not supported by the TLS backend",
        )?;
        Ok(())
    }

    /// Build a rustls [`CertifiedKey`] from this key pair.
    pub(crate) fn certified_key(&self) -> Result<CertifiedKey> {
        Ok(CertifiedKey::new(
            self.cert_chain.clone(),
            signing_key(&self.key).context(ErrorKind::Tls, "building TLS signing key")?,
        ))
    }
}

fn read_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    let certs = rustls_pemfile::certs(&mut BufReader::new(pem))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context(ErrorKind::Tls, "parsing certificate chain")?;
    if certs.is_empty() {
        return Err(Error::new(
            ErrorKind::Tls,
            "no certificates found in PEM input",
        ));
    }
    Ok(certs)
}

fn read_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>> {
    rustls_pemfile::private_key(&mut BufReader::new(pem))
        .context(ErrorKind::Tls, "parsing private key")?
        .context(ErrorKind::Tls, "no private key found in PEM input")
}

fn signing_key(
    key: &PrivateKeyDer<'_>,
) -> std::result::Result<std::sync::Arc<dyn SigningKey>, rustls::Error> {
    rustls::crypto::ring::sign::any_supported_type(key)
}

#[cfg(test)]
mod tests {
    use super::KeyPair;
    use crate::core::error::ErrorKind;
    use std::path::PathBuf;

    /// A self-signed certificate + key for a name, as PEM bytes.
    fn self_signed_pem(san: &str) -> (Vec<u8>, Vec<u8>) {
        let mut params = rcgen::CertificateParams::new(vec![san.to_string()]).expect("params");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = params.self_signed(&key).expect("cert");
        (cert.pem().into_bytes(), key.serialize_pem().into_bytes())
    }

    fn temp_pems(san: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert_pem, key_pem) = self_signed_pem(san);
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, &cert_pem).expect("write cert");
        std::fs::write(&key_path, &key_pem).expect("write key");
        (dir, cert_path, key_path)
    }

    #[test]
    fn loads_chain_and_key_from_pem() {
        let (_dir, cert_path, key_path) = temp_pems("example.com");
        let pair = KeyPair::from_pem_files(&cert_path, &key_path).expect("load");
        assert_eq!(pair.cert_chain().len(), 1);
        assert!(!pair.cert_chain()[0].as_ref().is_empty());
        pair.validate().expect("valid");
    }

    #[test]
    fn accepts_ecdsa_keys() {
        let mut params =
            rcgen::CertificateParams::new(vec!["ecdsa.example.com".to_string()]).expect("params");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("key");
        let cert = params.self_signed(&key).expect("cert");
        let pair =
            KeyPair::from_pem(cert.pem().as_bytes(), key.serialize_pem().as_bytes()).expect("load");
        pair.validate().expect("valid");
    }

    #[test]
    fn rejects_empty_certificate_input() {
        let (_dir, _cert_path, key_path) = temp_pems("example.com");
        let key_pem = std::fs::read(&key_path).expect("read key");
        let error = KeyPair::from_pem(b"", &key_pem).expect_err("empty certs");
        assert_eq!(error.kind(), ErrorKind::Tls);
    }

    #[test]
    fn rejects_missing_or_garbage_key() {
        let (_dir, cert_path, _key_path) = temp_pems("example.com");
        let cert_pem = std::fs::read(&cert_path).expect("read cert");
        let error = KeyPair::from_pem(&cert_pem, b"").expect_err("no key");
        assert_eq!(error.kind(), ErrorKind::Tls);

        let error = KeyPair::from_pem(&cert_pem, b"not a key").expect_err("garbage key");
        assert_eq!(error.kind(), ErrorKind::Tls);
    }

    #[test]
    fn rejects_missing_files() {
        let error = KeyPair::from_pem_files(
            PathBuf::from("/nope/cert.pem").as_path(),
            PathBuf::from("/nope/key.pem").as_path(),
        );
        assert_eq!(error.unwrap_err().kind(), ErrorKind::Io);
    }
}
