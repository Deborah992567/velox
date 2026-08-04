//! MIME type detection for static content.
//!
//! Detection is a pure, data-driven lookup on a file's extension — no I/O, no
//! magic-byte sniffing. It answers "what `Content-Type` should the response
//! carry" and is deliberately conservative: unknown extensions fall back to
//! `application/octet-stream`, and an explicitly configured type always
//! overrides the table (handlers apply the table only when the caller did not
//! set a type).

/// The `Content-Type` for a file path, based on its final extension.
pub fn mime_type_for_path(path: &std::path::Path) -> &'static str {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map_or("application/octet-stream", mime_type_for_extension)
}

/// The `Content-Type` for an extension with or without the leading dot,
/// compared case-insensitively.
pub fn mime_type_for_extension(extension: &str) -> &'static str {
    let ext = extension
        .strip_prefix('.')
        .unwrap_or(extension)
        .to_ascii_lowercase();
    match ext.as_str() {
        // Text
        "html" | "htm" | "shtml" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "jsonld" => "application/ld+json",
        "txt" => "text/plain; charset=utf-8",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "rtf" => "application/rtf",
        // Images
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" | "svgz" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "heic" | "heif" => "image/heic",
        "apng" => "image/apng",
        // Audio
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "weba" => "audio/webm",
        "opus" => "audio/opus",
        "mid" | "midi" => "audio/midi",
        // Video
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" | "qt" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "mkv" => "video/x-matroska",
        "flv" => "video/x-flv",
        "mpeg" | "mpg" => "video/mpeg",
        "3gp" => "video/3gpp",
        // Documents
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "epub" => "application/epub+zip",
        // Archives and binaries
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "zst" => "application/zstd",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "apk" => "application/vnd.android.package-archive",
        "exe" => "application/x-msdownload",
        "dmg" => "application/x-apple-diskimage",
        "iso" => "application/x-iso9660-image",
        "deb" => "application/vnd.debian.binary-package",
        "rpm" => "application/x-rpm",
        "wasm" => "application/wasm",
        // Fonts
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        // Web feeds and misc
        "rss" => "application/rss+xml",
        "atom" => "application/atom+xml",
        "ics" => "text/calendar",
        "vtt" => "text/vtt",
        "manifest" | "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::mime_type_for_extension;

    #[test]
    fn common_extensions_resolve() {
        assert_eq!(mime_type_for_extension("html"), "text/html; charset=utf-8");
        assert_eq!(mime_type_for_extension("png"), "image/png");
        assert_eq!(mime_type_for_extension("jpg"), "image/jpeg");
        assert_eq!(mime_type_for_extension("pdf"), "application/pdf");
        assert_eq!(mime_type_for_extension("woff2"), "font/woff2");
        assert_eq!(
            mime_type_for_extension("js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_type_for_extension("wasm"), "application/wasm");
    }

    #[test]
    fn comparison_is_case_insensitive() {
        assert_eq!(mime_type_for_extension("HTML"), "text/html; charset=utf-8");
        assert_eq!(mime_type_for_extension("JpEg"), "image/jpeg");
        assert_eq!(mime_type_for_extension(".CSS"), "text/css; charset=utf-8");
    }

    #[test]
    fn unknown_extensions_fall_back() {
        assert_eq!(
            mime_type_for_extension("definitely-not-a-type"),
            "application/octet-stream"
        );
        assert_eq!(mime_type_for_extension(""), "application/octet-stream");
    }
}
