//! Core primitives shared across all subsystems: error types and, in later
//! phases, the reactor, event driver, timers, and memory management.

pub mod error;

pub use error::{Context, Error, ErrorKind, Result, SourcePos};
