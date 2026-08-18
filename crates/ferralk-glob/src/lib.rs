#![forbid(unsafe_code)]
#![doc = "Portable, byte-first glob matching.\n\nThe matcher implementation lands in M1; this crate establishes the published\ncrate boundary specified by ADR-0003."]

//! `ferralk-glob` is the matcher-only member of the ferralk crate family.
//! It intentionally has no walker dependencies.

/// Crate version exposed for build and integration diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
