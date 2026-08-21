//! HTTP/2 protocol implementation (RFC 9113).
//!
//! This module provides the HTTP/2 framing layer, HPACK header compression,
//! stream management, and flow control.

pub mod frame;
pub mod hpack;
pub mod stream;
