//! Metrics collection — counters, gauges, and histograms.
//!
//! Phase 20: Thread-safe metric primitives for server observability.

pub mod counter;
pub mod gauge;
pub mod histogram;
pub mod prometheus;
pub mod registry;
