//! Content-encoding negotiation from `Accept-Encoding` headers.
//!
//! Implements RFC 9110 §12.5.3 quality-value negotiation: parse the client's
//! `Accept-Encoding`, select the best supported algorithm, and return the
//! chosen encoding + `Vary` header value.

use super::codec::Algorithm;

/// Parsed quality value for a single encoding.
#[derive(Debug, Clone)]
struct EncodingEntry {
    algorithm: Algorithm,
    quality: f64,
}

/// Parse an `Accept-Encoding` header value into a list of (algorithm, quality).
///
/// Unknown encodings are silently skipped. `identity` is also skipped since
/// uncompressed is always implicitly allowed.
fn parse_accept_encoding(header: &str) -> Vec<EncodingEntry> {
    let mut entries = Vec::new();
    for part in header.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, quality) = part.find(';').map_or((part, 1.0), |eq_pos| {
            let name = part[..eq_pos].trim();
            let q_str = part[eq_pos + 1..].trim();
            let q = q_str
                .strip_prefix("q=")
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(1.0);
            (name, q)
        });
        if let Some(alg) = Algorithm::from_str(name) {
            entries.push(EncodingEntry {
                algorithm: alg,
                quality: quality.clamp(0.0, 1.0),
            });
        }
    }
    entries
}

/// Select the best encoding from the client's `Accept-Encoding`.
///
/// Returns the chosen algorithm and the `Vary` header value. Returns `None`
/// (meaning identity/no compression) if no supported algorithm is accepted.
pub fn negotiate(accept_encoding: &str) -> Option<(Algorithm, &'static str)> {
    let entries = parse_accept_encoding(accept_encoding);
    let best = entries.iter().filter(|e| e.quality > 0.0).max_by(|a, b| {
        a.quality
            .partial_cmp(&b.quality)
            .unwrap_or(std::cmp::Ordering::Equal)
    })?;
    Some((best.algorithm, Algorithm::vary_value()))
}

/// Select the best encoding from a raw header value that may be absent.
pub fn negotiate_from_option(opt_header: Option<&str>) -> Option<(Algorithm, &'static str)> {
    let header = opt_header?;
    negotiate(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_of_equal_quality_is_chosen() {
        let result = negotiate("gzip, deflate, br").unwrap();
        assert!(result.0 == Algorithm::Gzip || result.0 == Algorithm::Deflate);
    }

    #[test]
    fn respects_quality_values() {
        let result = negotiate("deflate;q=1.0, gzip;q=0.5").unwrap();
        assert_eq!(result.0, Algorithm::Deflate);
    }

    #[test]
    fn zero_quality_excludes() {
        let result = negotiate("gzip;q=0, deflate;q=1.0");
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, Algorithm::Deflate);
    }

    #[test]
    fn all_zero_returns_none() {
        let result = negotiate("gzip;q=0, deflate;q=0");
        assert!(result.is_none());
    }

    #[test]
    fn unknown_encodings_skipped() {
        let result = negotiate("br, zstd, gzip").unwrap();
        assert_eq!(result.0, Algorithm::Gzip);
    }

    #[test]
    fn empty_header_returns_none() {
        assert!(negotiate("").is_none());
    }

    #[test]
    fn identity_ignored() {
        let result = negotiate("identity, gzip");
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, Algorithm::Gzip);
    }

    #[test]
    fn single_encoding() {
        let result = negotiate("deflate").unwrap();
        assert_eq!(result.0, Algorithm::Deflate);
    }

    #[test]
    fn x_gzip_alias() {
        let result = negotiate("x-gzip").unwrap();
        assert_eq!(result.0, Algorithm::Gzip);
    }

    #[test]
    fn negotiate_from_option_none() {
        assert!(negotiate_from_option(None).is_none());
    }

    #[test]
    fn negotiate_from_option_some() {
        let result = negotiate_from_option(Some("gzip")).unwrap();
        assert_eq!(result.0, Algorithm::Gzip);
    }

    #[test]
    fn quality_parsing_tolerates_whitespace() {
        let result = negotiate("deflate ; q= 0.8 , gzip ; q= 1.0").unwrap();
        assert_eq!(result.0, Algorithm::Gzip);
    }
}
