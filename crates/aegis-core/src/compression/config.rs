//! Compression configuration and level presets.
//!
//! Configures per-algorithm compression levels and minimum size thresholds.

use std::fmt;

/// Compression algorithm identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompressionAlgo {
    Gzip,
    Deflate,
    Zstd,
    Brotli,
    Zlib,
    None,
}

impl fmt::Display for CompressionAlgo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gzip => write!(f, "gzip"),
            Self::Deflate => write!(f, "deflate"),
            Self::Zstd => write!(f, "zstd"),
            Self::Brotli => write!(f, "br"),
            Self::Zlib => write!(f, "zlib"),
            Self::None => write!(f, "none"),
        }
    }
}

impl CompressionAlgo {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "gzip" => Some(Self::Gzip),
            "deflate" => Some(Self::Deflate),
            "zstd" => Some(Self::Zstd),
            "brotli" | "br" => Some(Self::Brotli),
            "zlib" => Some(Self::Zlib),
            "none" | "identity" => Some(Self::None),
            _ => None,
        }
    }
}

/// Compression level for an algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionLevel {
    Fastest,
    Default,
    Best,
    Custom(u8),
}

impl CompressionLevel {
    /// Map to a numeric level (0-9 for gzip/zlib/deflate).
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Fastest => 1,
            Self::Default => 6,
            Self::Best => 9,
            Self::Custom(level) => level.min(9),
        }
    }
}

/// Per-algorithm compression config.
#[derive(Debug, Clone)]
pub struct CompressionConfig {
    pub algo: CompressionAlgo,
    pub level: CompressionLevel,
    /// Minimum response body size in bytes to trigger compression.
    pub min_size: usize,
    /// Whether to compress even if client doesn't send Accept-Encoding.
    pub force: bool,
}

impl CompressionConfig {
    pub const fn new(algo: CompressionAlgo, level: CompressionLevel) -> Self {
        Self {
            algo,
            level,
            min_size: 256,
            force: false,
        }
    }

    #[must_use]
    pub const fn gzip_default() -> Self {
        Self::new(CompressionAlgo::Gzip, CompressionLevel::Default)
    }

    #[must_use]
    pub const fn deflate_default() -> Self {
        Self::new(CompressionAlgo::Deflate, CompressionLevel::Default)
    }

    #[must_use]
    pub fn should_compress(&self, body_size: usize) -> bool {
        self.algo != CompressionAlgo::None && body_size >= self.min_size
    }

    #[must_use]
    pub const fn with_min_size(mut self, min_size: usize) -> Self {
        self.min_size = min_size;
        self
    }

    #[must_use]
    pub const fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self::gzip_default()
    }
}

/// Client preference parsed from Accept-Encoding.
#[derive(Debug, Clone)]
pub struct ClientPreference {
    pub encodings: Vec<(CompressionAlgo, f32)>,
}

impl ClientPreference {
    pub fn parse(header: &str) -> Self {
        let mut encodings = Vec::new();
        for part in header.split(',') {
            let part = part.trim();
            let (name, q) = part.find(';').map_or((part, 1.0), |pos| {
                let name = part[..pos].trim();
                let q_str = part[pos + 1..].trim();
                let q = q_str
                    .strip_prefix("q=")
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0);
                (name, q)
            });
            if let Some(algo) = CompressionAlgo::from_str(name) {
                encodings.push((algo, q));
            }
        }
        encodings.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Self { encodings }
    }

    pub fn best_match(&self, server: &[CompressionAlgo]) -> Option<CompressionAlgo> {
        for (algo, q) in &self.encodings {
            if *q > 0.0 && server.contains(algo) {
                return Some(*algo);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn algo_display() {
        assert_eq!(CompressionAlgo::Gzip.to_string(), "gzip");
        assert_eq!(CompressionAlgo::Brotli.to_string(), "br");
    }

    #[test]
    fn algo_from_str() {
        assert_eq!(
            CompressionAlgo::from_str("gzip"),
            Some(CompressionAlgo::Gzip)
        );
        assert_eq!(
            CompressionAlgo::from_str("br"),
            Some(CompressionAlgo::Brotli)
        );
        assert_eq!(
            CompressionAlgo::from_str("zstd"),
            Some(CompressionAlgo::Zstd)
        );
        assert_eq!(CompressionAlgo::from_str("unknown"), None);
    }

    #[test]
    fn level_values() {
        assert_eq!(CompressionLevel::Fastest.as_u8(), 1);
        assert_eq!(CompressionLevel::Default.as_u8(), 6);
        assert_eq!(CompressionLevel::Best.as_u8(), 9);
        assert_eq!(CompressionLevel::Custom(5).as_u8(), 5);
        assert_eq!(CompressionLevel::Custom(15).as_u8(), 9);
    }

    #[test]
    fn should_compress_threshold() {
        let config = CompressionConfig::gzip_default();
        assert!(!config.should_compress(100));
        assert!(config.should_compress(300));
    }

    #[test]
    fn none_algo_never_compresses() {
        let config = CompressionConfig::new(CompressionAlgo::None, CompressionLevel::Default);
        assert!(!config.should_compress(1_000_000));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn parse_accept_encoding() {
        let pref = ClientPreference::parse("gzip, deflate;q=0.5, br;q=1.0");
        assert_eq!(pref.encodings.len(), 3);
        assert_eq!(pref.encodings[0].0, CompressionAlgo::Gzip);
        assert_eq!(pref.encodings[1].0, CompressionAlgo::Brotli);
        assert_eq!(pref.encodings[2].0, CompressionAlgo::Deflate);
        assert_eq!(pref.encodings[0].1, 1.0);
    }

    #[test]
    fn best_match_picks_first_compatible() {
        let pref = ClientPreference::parse("gzip, deflate");
        let server = vec![CompressionAlgo::Gzip, CompressionAlgo::Deflate];
        assert_eq!(pref.best_match(&server), Some(CompressionAlgo::Gzip));
    }

    #[test]
    fn best_match_none_if_no_compatible() {
        let pref = ClientPreference::parse("br");
        let server = vec![CompressionAlgo::Gzip];
        assert_eq!(pref.best_match(&server), None);
    }

    #[test]
    fn builder_chain() {
        let config = CompressionConfig::gzip_default()
            .with_min_size(512)
            .with_force(true);
        assert_eq!(config.min_size, 512);
        assert!(config.force);
    }
}
