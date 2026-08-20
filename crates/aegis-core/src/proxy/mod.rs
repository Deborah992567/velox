//! Reverse proxy: upstream configuration, the streaming exchange, the
//! keepalive connection pool, and load balancing across an upstream group.
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
//! and the exchange is bounded by [`ProxyOptions`] timeouts. Phase 10 keeps
//! upstream connections alive between requests via [`UpstreamPool`]. Phase 11
//! groups several servers into an [`UpstreamGroup`] and balances requests
//! across the healthy ones ([`BalancePolicy`]), with passive and active
//! health checks ([`HealthChecker`]).
//!
//! Semantics follow [`crate`] architecture §10 and ADR 0003.

pub mod config;
pub mod exchange;
pub mod pool;
pub mod rewrite;
pub mod upstream;
pub mod websocket;

pub use config::{ProxyOptions, ProxyTarget, UpstreamScheme, parse_proxy_pass};
pub use exchange::{ExchangeError, ProxyOutcome, proxy_exchange, proxy_exchange_pooled};
pub use pool::{PoolOptions, PooledConnection, UpstreamPool};
pub use upstream::{
    BalancePolicy, HealthCheckConfig, HealthChecker, PeerError, ProbeKind, SelectedPeer,
    UpstreamConfig, UpstreamGroup, UpstreamServer, proxy_exchange_lb,
};
