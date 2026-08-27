#![forbid(unsafe_code)]
#![doc = "Compile-checked Markdown documentation for Ferralk."]

#[doc = include_str!("../../../docs/README.md")]
pub mod index {}

#[doc = include_str!("../../../docs/benchmark-evidence.md")]
pub mod benchmark_evidence {}

#[doc = include_str!("../../../docs/usage.md")]
pub mod usage {}

#[doc = include_str!("../../../docs/compatibility-guide.md")]
pub mod compatibility_guide {}

#[doc = include_str!("../../../docs/compatibility-matrix.md")]
pub mod compatibility_matrix {}

#[doc = include_str!("../../../docs/corpus-format.md")]
pub mod corpus_format {}

#[doc = include_str!("../../../docs/external-release-gates.md")]
pub mod external_release_gates {}

#[doc = include_str!("../../../docs/fast-glob-reference.md")]
pub mod fast_glob_reference {}

#[doc = include_str!("../../../docs/palamedes-adoption.md")]
pub mod palamedes_adoption {}

#[doc = include_str!("../../../docs/zlob-1.6.3-reference.md")]
pub mod zlob_1_6_3_reference {}

#[doc = include_str!("../../../docs/zlob-fnmatch-test-coverage.md")]
pub mod zlob_fnmatch_test_coverage {}

#[doc = include_str!("../../../docs/zlob-test-suite-audit.md")]
pub mod zlob_test_suite_audit {}

#[doc = include_str!("../../../docs/adr/README.md")]
pub mod adr_index {}

#[doc = include_str!("../../../docs/adr/0001-independent-port-under-the-ferralk-name.md")]
pub mod adr_0001 {}

#[doc = include_str!("../../../docs/adr/0002-hybrid-port-strategy.md")]
pub mod adr_0002 {}

#[doc = include_str!("../../../docs/adr/0003-two-published-crates-no-c-abi.md")]
pub mod adr_0003 {}

#[doc = include_str!("../../../docs/adr/0004-msrv-stable-minus-two.md")]
pub mod adr_0004 {}

#[doc = include_str!("../../../docs/adr/0005-byte-matching-wtf8-on-windows.md")]
pub mod adr_0005 {}

#[doc = include_str!("../../../docs/adr/0006-git-normative-ignore-semantics.md")]
pub mod adr_0006 {}

#[doc = include_str!("../../../docs/adr/0007-differential-corpus-and-dev-time-oracle.md")]
pub mod adr_0007 {}

#[doc = include_str!("../../../docs/adr/0008-simd-via-memchr-primitives.md")]
pub mod adr_0008 {}

#[doc = include_str!("../../../docs/adr/0009-own-work-stealing-scheduler.md")]
pub mod adr_0009 {}

#[doc = include_str!("../../../docs/adr/0010-portable-1.0-native-backends-macos-then-linux.md")]
pub mod adr_0010 {}

#[doc = include_str!("../../../docs/adr/0011-posix-conservative-walker-defaults.md")]
pub mod adr_0011 {}

#[doc = include_str!("../../../docs/adr/0012-ferroni-repository-blueprint.md")]
pub mod adr_0012 {}

#[doc = include_str!("../../../docs/adr/0013-no-glob-to-regex-translation.md")]
pub mod adr_0013 {}

#[doc = include_str!("../../../docs/adr/0014-own-gitignore-rule-matching.md")]
pub mod adr_0014 {}
