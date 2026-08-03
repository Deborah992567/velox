//! HTTP/1.x wire protocol: parser, chunked coding, and response engine.
//!
//! Everything here is *incremental*: the parser and the chunked decoder
//! consume exactly the bytes available and report how far they got, so the
//! connection manager can feed socket reads into them without buffering
//! entire messages. Strictness is a security feature — see the architecture
//! security model (§11) — so malformed input is rejected rather than
//! repaired, and every framing decision is pinned to a single
//! `Content-Length` or to `chunked`.
//!
//! Sub-modules:
//! - [`parser`]: incremental request-head parser with smuggling defenses.
//! - [`chunked`]: incremental chunked transfer decoder.
//! - [`engine`]: injection-safe response encoder.
