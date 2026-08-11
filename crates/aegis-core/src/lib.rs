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
//! root for the roadmap. Currently at **Phase 9** (reverse proxy), on top of
//! the Phase 8 [`tls`] layer — rustls termination with SNI-based certificate
//! selection, bounded session resumption, live certificate reload, and a
//! blocking `TlsStream` wrapper over any transport — and the Phase 7
//! [`routing`] layer — virtual hosts with `server_name`/SNI/port
//! matching and nginx-style location precedence — and the Phase 6
//! [`static_files`] server: MIME detection, HTTP-date handling, strong
//! validators with conditional requests, byte ranges, traversal-safe path
//! resolution, directory listings, a full static file handler, and zero-copy
//! `sendfile` output — all over the Phase 5 [`http`] core model and strict
//! [`http::http1`] parser.

/// The semantic version of this crate, sourced from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The project/product name.
pub const PROJECT_NAME: &str = "velox";

/// The server binary name.
pub const BINARY_NAME: &str = "aegis";

pub mod buffer;
pub mod config;
pub mod connection;
pub mod core;
pub mod event_loop;
pub mod http;
pub mod logging;
pub mod net;
pub mod platform;
pub mod proxy;
pub mod routing;
pub mod static_files;
pub mod timers;
pub mod tls;

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
