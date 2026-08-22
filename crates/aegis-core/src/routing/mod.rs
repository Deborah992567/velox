//! Request routing and virtual hosts.
//!
//! Phase 7 of the roadmap. An incoming request is routed in two steps,
//! mirroring the architecture §7 flow `virtual_hosts → locations`:
//!
//! 1. **host selection** ([`host`]) — the `Host` header (and, on TLS
//!    connections, the SNI value) is matched against each server's
//!    `server_name` patterns to pick a [`VirtualHost`];
//! 2. **location dispatch** ([`location`]) — the request path is matched
//!    against the selected host's location table with the documented
//!    nginx-style precedence (exact > longest prefix > first regex, with
//!    `^~` short-circuiting the regex pass).
//!
//! The [`router`] module combines both steps into a [`Router`] and exposes
//! named-location lookup for internal redirects.
//!
//! Everything here is pure data and matching logic over a parsed request;
//! the per-location handler configuration is a generic parameter so Phase 9+
//! can attach proxy/static/`fcgi` targets without reworking the router.

pub mod host;
pub mod location;
pub mod params;
pub mod router;
