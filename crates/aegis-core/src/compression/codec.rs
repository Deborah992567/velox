//! Compression codecs: gzip and deflate implementations.
//!
//! Each codec wraps the `flate2` crate behind the [`Codec`] trait so the rest
//! of the server can compress/decompress without knowing the algorithm.

use std::io::{self, Read, Write};

use flate2::Compression;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};

/// Supported compression algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Algorithm {
    Gzip,
    Deflate,
    Zlib,
}

impl Algorithm {
    /// The `Content-Encoding` token for this algorithm.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Gzip => "gzip",
            Self::Deflate => "deflate",
            Self::Zlib => "zlib",
        }
    }

    /// Parse from a `Content-Encoding` value.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "gzip" | "x-gzip" => Some(Self::Gzip),
            "deflate" => Some(Self::Deflate),
            "zlib" => Some(Self::Zlib),
            _ => None,
        }
    }

    /// The `Vary` header fragment for this algorithm.
    pub const fn vary_value() -> &'static str {
        "Accept-Encoding"
    }
}

/// A streaming compression/decompression codec.
pub trait Codec {
    /// The algorithm this codec implements.
    fn algorithm(&self) -> Algorithm;

    /// Compress `input` and write to `output`. Returns bytes written.
    fn compress(&self, input: &[u8], output: &mut dyn Write) -> io::Result<usize>;

    /// Decompress `input` and write to `output`. Returns bytes written.
    fn decompress(&self, input: &[u8], output: &mut dyn Write) -> io::Result<usize>;
}

/// Gzip codec (default compression level).
#[derive(Debug, Clone, Copy)]
pub struct GzipCodec;

impl Codec for GzipCodec {
    fn algorithm(&self) -> Algorithm {
        Algorithm::Gzip
    }

    fn compress(&self, input: &[u8], output: &mut dyn Write) -> io::Result<usize> {
        let mut encoder = GzEncoder::new(output, Compression::default());
        encoder.write_all(input)?;
        let _ = encoder.finish()?;
        Ok(input.len())
    }

    fn decompress(&self, input: &[u8], output: &mut dyn Write) -> io::Result<usize> {
        let mut decoder = GzDecoder::new(input);
        let mut buf = [0u8; 8192];
        let mut total = 0;
        loop {
            let n = decoder.read(&mut buf)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n])?;
            total += n;
        }
        Ok(total)
    }
}

/// Deflate (raw) codec.
#[derive(Debug, Clone, Copy)]
pub struct DeflateCodec;

impl Codec for DeflateCodec {
    fn algorithm(&self) -> Algorithm {
        Algorithm::Deflate
    }

    fn compress(&self, input: &[u8], output: &mut dyn Write) -> io::Result<usize> {
        let mut encoder = DeflateEncoder::new(output, Compression::default());
        encoder.write_all(input)?;
        let _ = encoder.finish()?;
        Ok(input.len())
    }

    fn decompress(&self, input: &[u8], output: &mut dyn Write) -> io::Result<usize> {
        let mut decoder = DeflateDecoder::new(input);
        let mut buf = [0u8; 8192];
        let mut total = 0;
        loop {
            let n = decoder.read(&mut buf)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n])?;
            total += n;
        }
        Ok(total)
    }
}

/// Zlib codec (RFC 1950 wrapper).
#[derive(Debug, Clone, Copy)]
pub struct ZlibCodec;

impl Codec for ZlibCodec {
    fn algorithm(&self) -> Algorithm {
        Algorithm::Zlib
    }

    fn compress(&self, input: &[u8], output: &mut dyn Write) -> io::Result<usize> {
        let mut encoder = ZlibEncoder::new(output, Compression::default());
        encoder.write_all(input)?;
        let _ = encoder.finish()?;
        Ok(input.len())
    }

    fn decompress(&self, input: &[u8], output: &mut dyn Write) -> io::Result<usize> {
        let mut decoder = ZlibDecoder::new(input);
        let mut buf = [0u8; 8192];
        let mut total = 0;
        loop {
            let n = decoder.read(&mut buf)?;
            if n == 0 {
                break;
            }
            output.write_all(&buf[..n])?;
            total += n;
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_round_trip() {
        let data = b"Hello, world! This is a test of gzip compression. \
                      Repeated text helps compression ratios: \
                      the quick brown fox jumps over the lazy dog. \
                      the quick brown fox jumps over the lazy dog.";
        let codec = GzipCodec;
        let mut compressed = Vec::new();
        codec.compress(data, &mut compressed).unwrap();
        assert!(
            compressed.len() < data.len(),
            "gzip should compress repetitive data"
        );
        let mut decompressed = Vec::new();
        codec.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn deflate_round_trip() {
        let data = b"Deflate codec test data with enough repetition for compression.";
        let codec = DeflateCodec;
        let mut compressed = Vec::new();
        codec.compress(data, &mut compressed).unwrap();
        let mut decompressed = Vec::new();
        codec.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn zlib_round_trip() {
        let data = b"Zlib codec test data with enough repetition for compression.";
        let codec = ZlibCodec;
        let mut compressed = Vec::new();
        codec.compress(data, &mut compressed).unwrap();
        let mut decompressed = Vec::new();
        codec.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn gzip_empty() {
        let codec = GzipCodec;
        let mut compressed = Vec::new();
        codec.compress(b"", &mut compressed).unwrap();
        let mut decompressed = Vec::new();
        codec.decompress(&compressed, &mut decompressed).unwrap();
        assert!(decompressed.is_empty());
    }

    #[test]
    fn algorithm_from_str() {
        assert_eq!(Algorithm::from_str("gzip"), Some(Algorithm::Gzip));
        assert_eq!(Algorithm::from_str("x-gzip"), Some(Algorithm::Gzip));
        assert_eq!(Algorithm::from_str("deflate"), Some(Algorithm::Deflate));
        assert_eq!(Algorithm::from_str("zlib"), Some(Algorithm::Zlib));
        assert_eq!(Algorithm::from_str("br"), None);
        assert_eq!(Algorithm::from_str("identity"), None);
    }

    #[test]
    fn algorithm_as_str_roundtrip() {
        for alg in [Algorithm::Gzip, Algorithm::Deflate, Algorithm::Zlib] {
            assert_eq!(Algorithm::from_str(alg.as_str()), Some(alg));
        }
    }

    #[test]
    fn gzip_large_payload() {
        let chunk = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut data = Vec::new();
        for _ in 0..1000 {
            data.extend_from_slice(chunk);
        }
        let codec = GzipCodec;
        let mut compressed = Vec::new();
        codec.compress(&data, &mut compressed).unwrap();
        let mut decompressed = Vec::new();
        codec.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decompressed, data);
    }
}
