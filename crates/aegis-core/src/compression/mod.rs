//! Content compression: gzip, deflate, and content-encoding negotiation.
//!
//! Phase 14 adds transparent compression support. The [`Codec`] trait
//! abstracts encode/decode so the proxy and static file layers can
//! compress response bodies without knowing the algorithm.

pub mod codec;
pub mod negotiate;
