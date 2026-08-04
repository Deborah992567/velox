//! Validators for conditional requests (RFC 9110 §8.8 and §13).
//!
//! Files are validated with a strong [`etag_for`] derived from the file's
//! mtime and size, and with a second-granularity [`last_modified_for`] instant
//! for the `Last-Modified` header. The evaluation helpers implement the
//! matching rules for `If-Match`, `If-None-Match`, `If-Modified-Since`, and
//! `If-Unmodified-Since`, including the correct handling of weak (`W/`)
//! entity-tags, `*` wildcards, and comma-separated lists whose quoted
//! entity-tags may themselves contain commas.

use std::fs::Metadata;
use std::time::UNIX_EPOCH;

use time::{OffsetDateTime, UtcOffset};

use super::date::parse_http_date;

/// Build a strong entity-tag for a file from its metadata.
///
/// The tag is `"<hex(mtime-as-nanos)>-<hex(size)>"`, which changes whenever
/// the file content changes and is stable for an unchanged file. The result
/// is already quoted, as required by the `ETag` field grammar.
pub fn etag_for(metadata: &Metadata) -> String {
    let mtime_nanos = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("\"{mtime_nanos:016x}-{:016x}\"", metadata.len())
}

/// The file's last-modified time, truncated to whole seconds as required for
/// the `Last-Modified` field, or `None` if the platform cannot report it.
pub fn last_modified_for(metadata: &Metadata) -> Option<OffsetDateTime> {
    let instant = modified_instant(metadata)?;
    instant.replace_nanosecond(0).ok()
}

fn modified_instant(metadata: &Metadata) -> Option<OffsetDateTime> {
    let mtime = metadata.modified().ok()?;
    let nanos = i128::try_from(mtime.duration_since(UNIX_EPOCH).ok()?.as_nanos()).ok()?;
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()
        .map(|dt| dt.to_offset(UtcOffset::UTC))
}

/// Whether a `If-Match` header value matches the current entity-tag.
///
/// Uses strong comparison: a `W/`-prefixed entity-tag never matches and must
/// cause the condition to fail. A `*` matches any existing representation,
/// which is only meaningful when the resource exists.
pub fn if_match_passes(value: &[u8], current_etag: &str) -> bool {
    let items = split_list(value);
    !items.is_empty()
        && items
            .iter()
            .any(|item| etag_matches(item, current_etag, false))
}

/// Whether a `If-None-Match` header value matches the current entity-tag.
///
/// Uses weak comparison. Returns `true` when the representation is matched;
/// for `GET`/`HEAD` that means the response should be `304 Not Modified`, and
/// for state-changing methods the request must fail with `412 Precondition
/// Failed`.
pub fn if_none_match_matches(value: &[u8], current_etag: &str) -> bool {
    let items = split_list(value);
    items
        .iter()
        .any(|item| etag_matches(item, current_etag, true))
}

/// Whether a `If-Modified-Since` header indicates the resource was not
/// modified. The stored `last_modified` is truncated to seconds, so an equal
/// timestamp counts as "not modified".
pub fn if_modified_since_matches(value: &[u8], last_modified: OffsetDateTime) -> bool {
    let Some(since) = parse_http_date(value) else {
        return false;
    };
    last_modified <= since
}

/// Whether a `If-Unmodified-Since` header indicates the resource has not
/// changed since the given time, i.e. the condition passes.
pub fn if_unmodified_since_passes(value: &[u8], last_modified: OffsetDateTime) -> bool {
    let Some(since) = parse_http_date(value) else {
        return false;
    };
    last_modified <= since
}

/// Compare a single list item against `current_etag`.
///
/// `allow_weak` selects weak or strong comparison (RFC 9110 §8.8.3.2). An
/// unqualified `*` matches unconditionally.
fn etag_matches(item: &[u8], current_etag: &str, allow_weak: bool) -> bool {
    if item == b"*" {
        return true;
    }
    let current = current_etag.as_bytes();
    if let Some(stripped) = item.strip_prefix(b"W/") {
        return allow_weak && stripped == current;
    }
    item == current
}

/// Split a comma-separated field value into trimmed items, respecting quoted
/// strings so entity-tags containing commas are not split.
fn split_list(value: &[u8]) -> Vec<&[u8]> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    for (index, &byte) in value.iter().enumerate() {
        match byte {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                let item = trim_ows(&value[start..index]);
                if !item.is_empty() {
                    items.push(item);
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    let item = trim_ows(&value[start..]);
    if !item.is_empty() {
        items.push(item);
    }
    items
}

/// Trim optional whitespace (spaces and tabs) from both ends of a value.
fn trim_ows(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{
        etag_for, if_match_passes, if_modified_since_matches, if_none_match_matches,
        if_unmodified_since_passes, last_modified_for, split_list,
    };
    use std::fs::File;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    const ETAG: &str = "\"4b34f50000000000-0000000000000010\"";
    const ETAG_OTHER: &str = "\"4b34f50100000000-0000000000000020\"";

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Create a real temporary file with a controlled size and mtime.
    fn make_file(len: u64, mtime: Duration) -> (PathBuf, File) {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aegis-validators-{}-{unique}.bin",
            std::process::id()
        ));
        let file = File::create(&path).expect("create temp file");
        file.set_len(len).expect("set length");
        file.set_modified(UNIX_EPOCH + mtime).expect("set mtime");
        (path, file)
    }

    #[test]
    fn etag_is_quoted_and_tracks_size_and_mtime() {
        let (path, file) = make_file(16, Duration::from_secs(1_700_000_000));
        let first = etag_for(&file.metadata().expect("metadata"));
        let second = etag_for(&file.metadata().expect("metadata"));
        assert_eq!(first, second);
        assert!(first.starts_with('"') && first.ends_with('"'));
        assert!(first.contains('-'));
        file.set_len(200).expect("resize");
        assert_ne!(etag_for(&file.metadata().expect("metadata")), first);
        file.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_001))
            .expect("retouch");
        assert_ne!(etag_for(&file.metadata().expect("metadata")), first);
        drop(file);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn last_modified_is_truncated_to_seconds() {
        let (path, file) = make_file(100, Duration::from_millis(1_700_000_000_500));
        let last_modified =
            last_modified_for(&file.metadata().expect("metadata")).expect("modified time present");
        assert_eq!(last_modified.nanosecond(), 0);
        drop(file);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn if_match_uses_strong_comparison() {
        assert!(if_match_passes(b"*", ETAG));
        assert!(if_match_passes(
            b"\"abc\", \"4b34f50000000000-0000000000000010\"",
            ETAG
        ));
        assert!(if_match_passes(
            b"\"4b34f50000000000-0000000000000010\"",
            ETAG
        ));
        assert!(!if_match_passes(
            b"W/\"4b34f50000000000-0000000000000010\"",
            ETAG
        ));
        assert!(!if_match_passes(b"\"some-other\"", ETAG));
        assert!(!if_match_passes(b"", ETAG));
    }

    #[test]
    fn if_none_match_uses_weak_comparison() {
        assert!(if_none_match_matches(b"*", ETAG));
        assert!(if_none_match_matches(
            b"\"4b34f50000000000-0000000000000010\"",
            ETAG
        ));
        assert!(if_none_match_matches(
            b"W/\"4b34f50000000000-0000000000000010\"",
            ETAG
        ));
        assert!(!if_none_match_matches(b"W/some-other", ETAG));
        assert!(!if_none_match_matches(ETAG_OTHER.as_bytes(), ETAG));
        assert!(!if_none_match_matches(b"", ETAG));
    }

    #[test]
    fn split_list_respects_quoted_commas() {
        assert_eq!(
            split_list(b" \"a\", W/\"b,c\" , d "),
            [
                b"\"a\"".as_slice(),
                b"W/\"b,c\"".as_slice(),
                b"d".as_slice()
            ]
        );
        assert_eq!(split_list(b""), Vec::<&[u8]>::new());
        assert_eq!(split_list(b"  "), Vec::<&[u8]>::new());
    }

    #[test]
    fn modified_since_comparisons() {
        let modified = super::super::date::parse_http_date(b"Sun, 06 Nov 1994 08:49:37 GMT")
            .expect("valid date");
        assert!(if_modified_since_matches(
            b"Sun, 06 Nov 1994 08:50:00 GMT",
            modified
        ));
        assert!(!if_modified_since_matches(
            b"Sun, 06 Nov 1994 08:49:00 GMT",
            modified
        ));
        assert!(!if_modified_since_matches(b"not-a-date", modified));
        assert!(if_unmodified_since_passes(
            b"Sun, 06 Nov 1994 08:50:00 GMT",
            modified
        ));
        assert!(if_unmodified_since_passes(
            b"Sun, 06 Nov 1994 08:49:37 GMT",
            modified
        ));
        assert!(!if_unmodified_since_passes(
            b"Sun, 06 Nov 1994 08:49:00 GMT",
            modified
        ));
        assert!(!if_unmodified_since_passes(b"not-a-date", modified));
    }
}
