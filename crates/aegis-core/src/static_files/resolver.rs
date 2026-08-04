//! URL-path resolution and traversal prevention (architecture §11).
//!
//! Before a request-target can be mapped to a file on disk it is
//! percent-decoded and normalized segment-by-segment. Dot segments (`.` and
//! `..`) are resolved lexically, and any path that would step above the
//! document root — including via percent-encoded `%2e%2e` sequences that only
//! become `..` after decoding — is rejected. A decoded null byte or a
//! backslash is rejected outright as a defense against filesystem and
//! Windows-path confusion. The composed path is then verified to still start
//! with the document root before it is returned.

use std::path::{Path, PathBuf};

/// The outcome of resolving a request-target against a document root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// The normalized path would escape the document root.
    EscapesRoot,
    /// A `%` escape is not followed by two hex digits.
    InvalidPercentEncoding,
    /// The decoded path is not valid UTF-8 and cannot name a file.
    NotUtf8,
    /// The path contains a NUL byte or a backslash.
    InvalidCharacter,
}

/// Resolve `target` (the raw request-target bytes) against `root`.
///
/// Any `?`-query or `#`-fragment is first stripped, the path is
/// percent-decoded, dot segments are resolved, and the resulting segments are
/// joined under `root`. Returns a path guaranteed to sit inside `root`.
pub fn resolve(target: &[u8], root: &Path) -> Result<PathBuf, ResolveError> {
    let path = decode_path(target)?;
    let segments = normalize_segments(&path)?;
    let mut resolved = root.to_path_buf();
    for segment in segments {
        resolved.push(segment);
    }
    if !resolved.starts_with(root) {
        return Err(ResolveError::EscapesRoot);
    }
    Ok(resolved)
}

/// Strip query/fragment, percent-decode, and validate the UTF-8 path.
fn decode_path(target: &[u8]) -> Result<String, ResolveError> {
    let end = target
        .iter()
        .position(|&b| b == b'?' || b == b'#')
        .unwrap_or(target.len());
    let decoded = percent_decode(&target[..end]).ok_or(ResolveError::InvalidPercentEncoding)?;
    String::from_utf8(decoded).map_err(|_| ResolveError::NotUtf8)
}

/// Percent-decode `input` into raw bytes, or `None` on a malformed `%` escape.
fn percent_decode(input: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' {
            let high = hex_value(*input.get(index + 1)?)?;
            let low = hex_value(*input.get(index + 2)?)?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(input[index]);
            index += 1;
        }
    }
    Some(decoded)
}

/// Decode a single hex digit.
const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Resolve `.`/`..` segments in a decoded path, rejecting escapes.
fn normalize_segments(path: &str) -> Result<Vec<&str>, ResolveError> {
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(ResolveError::EscapesRoot);
                }
            }
            other => {
                if other.contains('\0') || other.contains('\\') {
                    return Err(ResolveError::InvalidCharacter);
                }
                segments.push(other);
            }
        }
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::{ResolveError, normalize_segments, resolve};
    use std::path::PathBuf;

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("aegis-resolver-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        root
    }

    fn normalized(target: &[u8]) -> Result<PathBuf, ResolveError> {
        resolve(target, &temp_root())
    }

    #[test]
    fn simple_paths_resolve_verbatim() {
        assert_eq!(
            normalized(b"/index.html"),
            Ok(temp_root().join("index.html"))
        );
        assert_eq!(
            normalized(b"/assets/css/app.css"),
            Ok(temp_root().join("assets/css/app.css"))
        );
        assert_eq!(normalized(b"/"), Ok(temp_root()));
    }

    #[test]
    fn dot_segments_are_resolved() {
        assert_eq!(normalized(b"/a/b/../c"), Ok(temp_root().join("a/c")));
        assert_eq!(normalized(b"/a/./b"), Ok(temp_root().join("a/b")));
        assert_eq!(normalized(b"/a//b"), Ok(temp_root().join("a/b")));
        assert_eq!(normalized(b"/a/.."), Ok(temp_root()));
    }

    #[test]
    fn traversal_is_rejected() {
        assert_eq!(normalized(b"/../secret"), Err(ResolveError::EscapesRoot));
        assert_eq!(normalized(b"/a/../../x"), Err(ResolveError::EscapesRoot));
        assert_eq!(normalized(b"/.."), Err(ResolveError::EscapesRoot));
        assert_eq!(
            normalized(b"/%2e%2e/secret"),
            Err(ResolveError::EscapesRoot)
        );
        assert_eq!(
            normalized(b"/a/%2e%2e/%2e%2e/x"),
            Err(ResolveError::EscapesRoot)
        );
        assert_eq!(
            normalized(b"/%2E%2E/secret"),
            Err(ResolveError::EscapesRoot)
        );
    }

    #[test]
    fn encoded_slashes_are_decoded_before_split() {
        assert_eq!(normalized(b"/a%2Fb"), Ok(temp_root().join("a/b")));
        assert_eq!(normalized(b"/a/%2e%2e%2fb"), Ok(temp_root().join("b")));
    }

    #[test]
    fn malformed_input_is_rejected() {
        assert_eq!(
            normalized(b"/a%ZZb"),
            Err(ResolveError::InvalidPercentEncoding)
        );
        assert_eq!(
            normalized(b"/a%2"),
            Err(ResolveError::InvalidPercentEncoding)
        );
        assert_eq!(normalized(b"/%FF"), Err(ResolveError::NotUtf8));
    }

    #[test]
    fn dangerous_characters_are_rejected() {
        assert_eq!(normalized(b"/a%00b"), Err(ResolveError::InvalidCharacter));
        assert_eq!(normalized(b"/a\\b"), Err(ResolveError::InvalidCharacter));
        assert_eq!(
            normalized(b"/..%5c..%5csecret"),
            Err(ResolveError::InvalidCharacter)
        );
    }

    #[test]
    fn query_and_fragment_are_stripped() {
        assert_eq!(normalized(b"/a/b?download=1"), Ok(temp_root().join("a/b")));
        assert_eq!(normalized(b"/a/b#section"), Ok(temp_root().join("a/b")));
        assert_eq!(
            normalized(b"/caf%C3%A9.txt"),
            Ok(temp_root().join("café.txt"))
        );
    }

    #[test]
    fn normalization_matches_expected_segments() {
        assert_eq!(normalize_segments("/x/y/./z/../w"), Ok(vec!["x", "y", "w"]));
        assert_eq!(normalize_segments("/"), Ok(vec![]));
        assert_eq!(
            normalize_segments("/.hidden/config"),
            Ok(vec![".hidden", "config"])
        );
    }
}
