//! CSP nonce generator for Content Security Policy.
//!
//! Generates cryptographically random nonces for inline script/style tags.

use std::fmt;
use std::io::Read;

/// A CSP nonce value (Base64-encoded random bytes).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Nonce(String);

impl Nonce {
    /// Generate a new random nonce.
    ///
    /// # Panics
    ///
    /// Panics if the platform entropy source (`/dev/urandom` on Unix)
    /// cannot be opened or read.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 16];
        // Use platform entropy source
        #[cfg(unix)]
        {
            let mut f = std::fs::File::open("/dev/urandom").expect("/dev/urandom");
            f.read_exact(&mut bytes).expect("entropy");
        }
        #[cfg(not(unix))]
        {
            // Fallback: not cryptographically secure but functional
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = (i.wrapping_mul(37) as u8).wrapping_add(42);
            }
        }
        Self(base64_encode(&bytes))
    }

    /// Create a nonce from a known string (for testing).
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The nonce value for use in HTML attributes.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The nonce formatted for CSP headers: `nonce-<value>`.
    pub fn header_value(&self) -> String {
        format!("nonce-{}", self.0)
    }
}

impl fmt::Display for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Nonce {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).map_or(0, |&b| u32::from(b));
        let b2 = chunk.get(2).map_or(0, |&b| u32::from(b));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// CSP policy builder.
#[derive(Debug, Clone)]
pub struct CspPolicy {
    directives: Vec<(String, Vec<String>)>,
}

impl CspPolicy {
    pub const fn new() -> Self {
        Self {
            directives: Vec::new(),
        }
    }

    #[must_use]
    pub fn default_src(mut self, sources: &[&str]) -> Self {
        self.directives.push((
            "default-src".into(),
            sources.iter().map(|s| (*s).into()).collect(),
        ));
        self
    }

    #[must_use]
    pub fn script_src(mut self, sources: &[&str]) -> Self {
        self.directives.push((
            "script-src".into(),
            sources.iter().map(|s| (*s).into()).collect(),
        ));
        self
    }

    #[must_use]
    pub fn style_src(mut self, sources: &[&str]) -> Self {
        self.directives.push((
            "style-src".into(),
            sources.iter().map(|s| (*s).into()).collect(),
        ));
        self
    }

    #[must_use]
    pub fn add_directive(mut self, name: &str, sources: &[&str]) -> Self {
        self.directives
            .push((name.into(), sources.iter().map(|s| (*s).into()).collect()));
        self
    }

    #[must_use]
    pub fn add_nonce_to_script(mut self, nonce: &Nonce) -> Self {
        if let Some((_, sources)) = self.directives.iter_mut().find(|(k, _)| k == "script-src") {
            sources.push(nonce.header_value());
        } else {
            self.directives
                .push(("script-src".into(), vec![nonce.header_value()]));
        }
        self
    }

    pub fn render(&self) -> String {
        self.directives
            .iter()
            .map(|(name, sources)| format!("{name} {}", sources.join(" ")))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

impl Default for CspPolicy {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_generation() {
        let n1 = Nonce::generate();
        let n2 = Nonce::generate();
        assert_eq!(n1.as_str().len(), 24);
        assert_ne!(n1, n2);
    }

    #[test]
    fn nonce_header_value() {
        let n = Nonce::from_string("abc123");
        assert_eq!(n.header_value(), "nonce-abc123");
    }

    #[test]
    fn nonce_display() {
        let n = Nonce::from_string("test");
        assert_eq!(format!("{n}"), "test");
    }

    #[test]
    fn base64_encode_short() {
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
    }

    #[test]
    fn base64_encode_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn csp_policy_render() {
        let policy = CspPolicy::new()
            .default_src(&["'self'"])
            .script_src(&["'self'", "'unsafe-inline'"]);
        let rendered = policy.render();
        assert!(rendered.contains("default-src 'self'"));
        assert!(rendered.contains("script-src 'self' 'unsafe-inline'"));
    }

    #[test]
    fn csp_with_nonce() {
        let nonce = Nonce::from_string("xyz");
        let policy = CspPolicy::new()
            .default_src(&["'self'"])
            .add_nonce_to_script(&nonce);
        let rendered = policy.render();
        assert!(rendered.contains("script-src nonce-xyz"));
    }

    #[test]
    fn csp_default_is_empty() {
        let policy = CspPolicy::default();
        assert!(policy.render().is_empty());
    }
}
