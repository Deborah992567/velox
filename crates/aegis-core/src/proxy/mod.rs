//! Reverse proxy: upstream configuration and the streaming exchange.
//!
//! Phase 9 adds an nginx-class reverse proxy on top of the Phase 5 [`http`]
//! model and the Phase 2 [`net`] transport. The proxy terminates the client
//! connection and, per [`crate::routing`] location matching, forwards the
//! request to an upstream:
//!
//! - `proxy_pass http://host[:port];` passes the original request URI through;
//! - `proxy_pass http://host[:port]/prefix;` replaces the matched location
//!   prefix with `/prefix`;
//! - `proxy_pass unix:/path.sock;` targets a Unix domain socket.
//!
//! Request/response heads are encoded and parsed with the [`crate::http::http1`]
//! engine, bodies stream through with chunked decoding on the upstream side,
//! and the exchange is bounded by [`ProxyOptions`] timeouts.
//!
//! Semantics follow [`crate`] architecture §10 and ADR 0003.

pub mod config;
pub mod exchange;
pub mod rewrite;

pub use config::{ProxyOptions, ProxyTarget, UpstreamScheme, parse_proxy_pass};
pub use exchange::{ExchangeError, ProxyOutcome, proxy_exchange};
