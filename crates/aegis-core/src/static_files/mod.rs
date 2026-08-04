//! Static file serving.
//!
//! Phase 6 of the roadmap: serving files from a document root with MIME
//! detection, index files, directory listing, validation (`Last-Modified`,
//! `ETag`, conditional requests), and byte-range responses (`206 Partial
//! Content`), plus zero-copy `sendfile` output where the platform allows it.
//!
//! Submodules are added incrementally and only declared here once they are
//! implemented:
//!
//! - [`mime`] — MIME type detection from file extensions.
//! - [`date`] — HTTP date formatting and parsing (IMF-fixdate and the two
//!   obsolete formats a recipient must still accept).

pub mod date;
pub mod mime;
