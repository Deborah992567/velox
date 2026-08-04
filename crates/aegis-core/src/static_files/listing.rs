//! HTML directory listing.
//!
//! When a request targets a directory and no index file matches, a listing
//! page is generated from the on-disk entries. Directories sort before files,
//! both ordered case-insensitively by name; dotfiles are hidden; and every
//! name is HTML-escaped for display and percent-encoded for its `href`, so the
//! page is safe against both HTML injection and broken links.

use std::fmt::Write as _;
use std::path::Path;

use super::date::format_http_date;
use super::validators::last_modified_for;

/// A single entry found in a directory.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// The display name (already UTF-8).
    pub name: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Size in bytes of a regular file (`0` for directories).
    pub len: u64,
    /// Last-modified time, when the platform reports it.
    pub modified: Option<time::OffsetDateTime>,
}

/// Read and sort the entries of `path`.
///
/// Hidden entries (names starting with `.`) and entries that are neither a
/// regular file nor a directory (e.g. sockets) are skipped.
pub fn list_directory(path: &Path) -> std::io::Result<Vec<DirEntry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let file_type = entry.file_type()?;
        let (is_dir, len, modified) = if file_type.is_dir() {
            (true, 0, None)
        } else if file_type.is_file() {
            let metadata = entry.metadata()?;
            (false, metadata.len(), last_modified_for(&metadata))
        } else {
            continue;
        };
        entries.push(DirEntry {
            name,
            is_dir,
            len,
            modified,
        });
    }
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(entries)
}

/// Render an HTML listing page for directory `dir_url` (the normalized URL
/// path of the directory, e.g. `/assets/`).
pub fn render_listing(dir_url: &str, entries: &[DirEntry]) -> String {
    let title = escape_html(dir_url);
    let base = dir_url.trim_end_matches('/');
    let mut html = String::new();
    html.push_str(
        "<!DOCTYPE html>\n<html lang=\"en\"><head>\n<meta charset=\"utf-8\">\n<title>Index of ",
    );
    html.push_str(&title);
    html.push_str("</title>\n<style>\nbody { font-family: sans-serif; margin: 2em; }\n");
    html.push_str("table { border-collapse: collapse; }\nth, td { text-align: left; padding: 0.25em 1.5em 0.25em 0; }\n");
    html.push_str("th { border-bottom: 2px solid #ccc; }\n</style>\n</head><body>\n<h1>Index of ");
    html.push_str(&title);
    html.push_str("</h1>\n<table>\n<thead><tr><th>Name</th><th>Size</th><th>Last modified</th></tr></thead>\n<tbody>\n");
    if dir_url != "/" {
        html.push_str("<tr><td><a href=\"../\">../</a></td><td></td><td></td></tr>\n");
    }
    for entry in entries {
        let display = if entry.is_dir {
            format!("{}/", escape_html(&entry.name))
        } else {
            escape_html(&entry.name)
        };
        let href = format!(
            "{base}/{}{}",
            url_encode(&entry.name),
            if entry.is_dir { "/" } else { "" }
        );
        let size = if entry.is_dir {
            String::new()
        } else {
            format_size(entry.len)
        };
        let modified = entry.modified.map_or_else(String::new, format_http_date);
        html.push_str("<tr><td><a href=\"");
        html.push_str(&escape_html(&href));
        html.push_str("\">");
        html.push_str(&display);
        html.push_str("</a></td><td>");
        html.push_str(&size);
        html.push_str("</td><td>");
        html.push_str(&modified);
        html.push_str("</td></tr>\n");
    }
    html.push_str("</tbody>\n</table>\n</body></html>\n");
    html
}

/// Percent-encode every byte except RFC 3986 unreserved characters.
fn url_encode(input: &str) -> String {
    let mut encoded = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// HTML-escape text so it is safe to embed in a document or attribute.
fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Format a byte count as a human-readable size (nginx-style, integer math).
fn format_size(len: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut scaled = len;
    let mut remainder = 0;
    let mut unit = 0;
    while scaled >= 1024 && unit < UNITS.len() - 1 {
        remainder = scaled % 1024;
        scaled /= 1024;
        unit += 1;
    }
    if unit == 0 {
        return format!("{len} B");
    }
    let tenths = remainder * 10 / 1024;
    if tenths == 0 {
        format!("{scaled} {}", UNITS[unit])
    } else {
        format!("{scaled}.{tenths} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::{DirEntry, escape_html, format_size, list_directory, render_listing, url_encode};

    #[test]
    fn sizes_format_human_readably() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1 K");
        assert_eq!(format_size(1536), "1.5 K");
        assert_eq!(format_size(1024 * 1024), "1 M");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5 G");
    }

    #[test]
    fn url_encoding_is_unreserved_only() {
        assert_eq!(url_encode("plain-name.txt"), "plain-name.txt");
        assert_eq!(url_encode("a b&c"), "a%20b%26c");
        assert_eq!(url_encode("café"), "caf%C3%A9");
        assert_eq!(url_encode("~-_."), "~-_.");
    }

    #[test]
    fn html_escaping_prevents_injection() {
        assert_eq!(
            escape_html("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
        assert_eq!(escape_html("a&b\"c'd"), "a&amp;b&quot;c&#39;d");
    }

    #[test]
    fn listings_sort_directories_first_case_insensitively() {
        let root = std::env::temp_dir().join(format!("aegis-listing-{}", std::process::id()));
        let _ = std::fs::create_dir_all(root.join("zeta"));
        let _ = std::fs::create_dir_all(root.join("Alpha"));
        std::fs::write(root.join("beta.txt"), b"data").expect("write file");
        std::fs::write(root.join(".hidden"), b"x").expect("write file");

        let entries = list_directory(&root).expect("list directory");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "zeta", "beta.txt"]);
        assert!(entries[0].is_dir && entries[1].is_dir && !entries[2].is_dir);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rendered_page_escapes_and_links() {
        let entries = vec![
            DirEntry {
                name: "a<script>.txt".into(),
                is_dir: false,
                len: 5,
                modified: None,
            },
            DirEntry {
                name: "sub dir".into(),
                is_dir: true,
                len: 0,
                modified: None,
            },
        ];
        let html = render_listing("/assets/", &entries);
        assert!(html.contains("&lt;script&gt;"), "display name escaped");
        assert!(
            html.contains("href=\"/assets/a%3Cscript%3E.txt\""),
            "href encoded"
        );
        assert!(
            html.contains("href=\"/assets/sub%20dir/\""),
            "directory link"
        );
        assert!(html.contains(">sub dir/</a>"), "directory display suffix");
        assert!(html.contains("Index of /assets/"));
        assert!(html.contains("href=\"../\""), "parent link present");
    }

    #[test]
    fn root_listing_has_no_parent_link() {
        let html = render_listing("/", &[]);
        assert!(html.contains("Index of /"));
        assert!(!html.contains("href=\"../\""));
    }
}
