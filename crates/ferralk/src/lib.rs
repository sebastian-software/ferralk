#![forbid(unsafe_code)]
#![doc = "Portable filesystem walking.\n\nThe serial walker implementation lands in M2; this crate establishes the\npublished crate boundary specified by ADR-0003."]

//! `ferralk` will layer filesystem traversal on top of [`ferralk_glob`].

pub use ferralk_glob;

/// Crate version exposed for build and integration diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
