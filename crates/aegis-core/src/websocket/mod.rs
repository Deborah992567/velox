//! RFC 6455 WebSocket support.
//!
//! Two parts make up the protocol: the opening handshake
//! ([`handshake`] — the `101` upgrade negotiation) and the message framing
//! ([`frame`] — the codec that parses, validates, and serializes frames and
//! reassembles fragmented messages).
//!
//! The handshake lives on top of the HTTP/1.x core: [`handshake::is_websocket_upgrade`]
//! classifies a parsed request head, [`handshake::upgrade_response`] answers
//! with the accept value, and [`handshake::client_request`] builds the
//! upstream request a reverse proxy forwards (§8.1.3 requires the proxy to
//! pass the client's key through unchanged).
//!
//! The codec enforces §5 (RSV bits, reserved opcodes, control-frame caps and
//! non-fragmentation, continuation ordering, masking) and §8.1 (close codes
//! and UTF-8), with [`frame::FrameLimits`] bounding frame and message sizes.

pub mod frame;
pub mod handshake;
