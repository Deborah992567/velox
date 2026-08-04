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
//! - [`validators`] — strong `ETag`s, `Last-Modified`, and conditional-request
//!   evaluation.
//! - [`range`] — byte-range parsing for `206 Partial Content` responses.
//! - [`resolver`] — percent-decoding, normalization, and traversal
//!   prevention when mapping request-targets to disk paths.
//! - [`listing`] — HTML directory listing generation.
//! - [`handler`] — the [`Request`]-to-response orchestration that combines
//!   everything above.

pub mod date;
pub mod handler;
pub mod listing;
pub mod mime;
pub mod range;
pub mod resolver;
pub mod validators;
