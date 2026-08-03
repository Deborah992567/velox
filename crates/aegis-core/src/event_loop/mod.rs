//! The per-worker event loop: reactor, executor, and the token slab.
//!
//! Built on top of the platform drivers ([`crate::platform`]) and the timer
//! wheel ([`crate::timers`]). One `event_loop` instance per worker process:
//! a single reactor owns the driver and the connection slab, the executor
//! runs ready tasks, and the loop body ties the two together (see
//! `docs/architecture.md` §3.1 and ADR 0002).

pub mod slab;

pub use slab::Slab;
