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

## Evidence, 2026-08-19

The Ferroni playbook this ADR follows is confirmed by measurement rather than
by assumption. Prefilters and specialised paths built on `memchr` primitives
carried an extglob case at 427× and cut the general path by about 70%, with no
hand-written intrinsics written at any point. The escape hatch in the last
consequence above stays unexercised, and `ferralk-glob` still compiles under
`#![forbid(unsafe_code)]`.

Amendment only; the decision above is unchanged.

## Evidence, 2026-08-27

An Apple-Silicon experiment gave hand-written NEON the most favorable common
case: a short suffix was prepared once and matched against the final 16 path
bytes. The intrinsic kernel beat the scalar byte loop, but a safe comparison
over two masked `u64` words produced the same 1024-path filter throughput
(about 7.11 microseconds on an M1 Ultra, down from about 10.09 microseconds).
The optimized assembly for that safe comparison still contains 128-bit NEON
loads and byte-wise `eor`/`and`/`orr`: LLVM generated the SIMD kernel itself.
The safe Apple-Silicon specialization was retained; the intrinsic prototype
and its unsafe-code exception were removed. The crate still forbids unsafe
code, and the decision above remains the threshold for future experiments.
