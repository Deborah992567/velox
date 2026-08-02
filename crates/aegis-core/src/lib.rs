//! Aegis (Velox) — core library.
//!
//! A production-grade, Nginx-class web server, reverse proxy, load balancer,
//! TLS terminator, and application gateway implemented from scratch in Rust.
//!
//! This crate contains the server itself: the platform event drivers, the
//! reactor/executor, the HTTP/1.x · HTTP/2 · HTTP/3 stacks, the proxy and
//! protocol adapters, caching, compression, rate limiting, access control,
//! configuration, logging, metrics, and the master/worker process layer.
//!
//! The implementation follows the approved architecture in
//! [`docs/architecture.md`] and the decision records in [`ADR/`].
//!
//! [`docs/architecture.md`]: https://github.com/Deborah992567/velox/blob/main/docs/architecture.md
//! [`ADR/`]: https://github.com/Deborah992567/velox/blob/main/ADR
//!
//! # Phasing
//!
//! The workspace is developed phase by phase; see `TODO.md` in the repository
//! root for the roadmap. Currently at **Phase 0** (architecture + skeleton):
//! only the version constant and crate scaffolding live here. Subsystem
//! modules land in their phases, each with a test suite and green CI.

/// The semantic version of this crate, sourced from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The project/product name.
pub const PROJECT_NAME: &str = "velox";

/// The server binary name.
pub const BINARY_NAME: &str = "aegis";

#[cfg(test)]
mod tests {
    use super::{BINARY_NAME, PROJECT_NAME, VERSION};

    #[test]
    fn version_matches_package_manifest() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn product_names_are_set() {
        assert!(!PROJECT_NAME.is_empty());
        assert!(!BINARY_NAME.is_empty());
    }
}
