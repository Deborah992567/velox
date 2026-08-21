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
//! root for the roadmap. Currently at **Phase 13** (gateway protocols:
//! `FastCGI`, SCGI, uWSGI), on top of the Phase 12 [`websocket`] layer,
//! Phase 11 load balancing and health checks, Phase 10 upstream connection
//! pooling, Phase 9 [`proxy`] reverse proxy, Phase 8 [`tls`] termination,
//! Phase 7 [`routing`], Phase 6 [`static_files`], and the Phase 5 [`http`]
//! core.

/// The semantic version of this crate, sourced from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The project/product name.
pub const PROJECT_NAME: &str = "velox";

/// The server binary name.
pub const BINARY_NAME: &str = "aegis";

pub mod buffer;
pub mod cache;
pub mod compression;
pub mod config;
pub mod connection;
pub mod core;
pub mod event_loop;
pub mod http;
pub mod logging;
pub mod net;
pub mod platform;
pub mod proxy;
pub mod ratelimit;
pub mod routing;
pub mod static_files;
pub mod timers;
pub mod tls;
pub mod websocket;

/// Log at an explicit level. Message arguments follow `format!` syntax.
///
/// Records are dropped if no global logger is installed or if the level is
/// below the configured minimum.
#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)+) => {
        $crate::logging::log_at(
            $level,
            module_path!(),
            format!($($arg)+),
        )
    };
}

/// Log at `debug` level.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)+) => {
        $crate::log!($crate::logging::Level::Debug, $($arg)+)
    };
}

/// Log at `info` level.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)+) => {
        $crate::log!($crate::logging::Level::Info, $($arg)+)
    };
}

/// Log at `notice` level.
#[macro_export]
macro_rules! log_notice {
    ($($arg:tt)+) => {
        $crate::log!($crate::logging::Level::Notice, $($arg)+)
    };
}

/// Log at `warn` level.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)+) => {
        $crate::log!($crate::logging::Level::Warn, $($arg)+)
    };
}

/// Log at `error` level.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)+) => {
        $crate::log!($crate::logging::Level::Error, $($arg)+)
    };
}

/// Log at `crit` level.
#[macro_export]
macro_rules! log_crit {
    ($($arg:tt)+) => {
        $crate::log!($crate::logging::Level::Crit, $($arg)+)
    };
}

/// Log at `alert` level.
#[macro_export]
macro_rules! log_alert {
    ($($arg:tt)+) => {
        $crate::log!($crate::logging::Level::Alert, $($arg)+)
    };
}

/// Log at `emerg` level.
#[macro_export]
macro_rules! log_emerg {
    ($($arg:tt)+) => {
        $crate::log!($crate::logging::Level::Emerg, $($arg)+)
    };
}

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
