//! Bundled coordination plugins.
//!
//! All writes go through `controller::execute`, reads go through the `Store`
//! trait, and kernel logic never names plugin items except the lookup arms.

pub mod council;
pub mod triage;
