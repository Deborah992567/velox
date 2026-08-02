//! Networking core: addresses, sockets, listeners, and connections.
//!
//! Phase 2 covers the transport layer only: IPv4, IPv6, and Unix domain
//! stream sockets with configurable options and non-blocking support. The
//! event-driven I/O (Phase 3) consumes [`Listener`] and [`Connection`].
//!
//! # Safety policy
//!
//! The workspace defaults to `unsafe_code = "warn"`. This module is the one
//! place in the crate that legitimately calls the C socket API (`libc`), so
//! the lint is scoped off here. Every `unsafe` block carries a `// SAFETY:`
//! comment; the wrappers present a safe, Rust-idiomatic interface to the rest
//! of the codebase.
#![allow(unsafe_code)]

pub mod addr;
pub mod connection;
pub mod listener;
mod socket;

pub use addr::InetAddr;
pub use connection::Connection;
pub use listener::Listener;
pub use socket::{SocketOptions, create_socket, set_nonblocking};
