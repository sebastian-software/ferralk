# ADR-0008: SIMD via memchr primitives (the Ferroni playbook)

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

zlob is "SIMD-first" using Zig's `@Vector` builtins, which have no stable-Rust
equivalent (`std::simd` is nightly; our MSRV policy excludes it, ADR-0004).
Ferroni — our pure-Rust Oniguruma port — beats the C original across its
measured runtime cases with zero `core::arch` usage: all SIMD comes from
`memchr`/`aho-corasick`, which provide runtime CPU detection internally.

## Decision

The port covers the scalar semantic core only (ADR-0002). zlob's `@Vector`
paths are replaced, not transliterated: hot paths use `memchr`/`memmem`
primitives (literal search, suffix checks, byte scans); `aho-corasick` is a
candidate for multi-pattern literal prefilters. Hand-written intrinsics are
considered only if profiling proves a gap these primitives cannot close.
`ferralk-glob` compiles under `#![forbid(unsafe_code)]`.

## Consequences

- Zero unsafe code in the matcher; runtime feature detection for free.
- Early matcher benchmarks may trail zlob until tuning; comparisons inform
  profiling priorities but do not form a release gate.
- If a gap ever demands intrinsics, `forbid` relaxes to `deny` with a scoped
  `allow` in one SIMD module — a deliberate, reviewable step.
