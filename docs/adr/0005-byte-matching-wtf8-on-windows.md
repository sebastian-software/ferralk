# ADR-0005: Byte-based matching on all platforms, WTF-8 on Windows

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

Filesystem names are not guaranteed UTF-8 (arbitrary bytes on Unix, unpaired
UTF-16 surrogates on Windows). RFC goal #4 forbids lossy conversion in the
public path API. The alternatives were a second UTF-16 matcher for Windows
(double test surface, double SIMD work) or lossy UTF-8 (`to_string_lossy`),
where distinct files can collapse to the same U+FFFD-containing name.

## Decision

Matching operates over bytes on every platform: raw bytes via
`OsStrExt::as_bytes()` on Unix, and the lossless WTF-8 representation via
`OsStr::as_encoded_bytes()` (stable since Rust 1.74) on Windows. One byte
matcher serves all targets. On Windows both `/` and `\` act as separators;
patterns are written with `/`. Matching is case-sensitive by default on all
platforms, with explicit opt-in case folding.

## Consequences

- A single, SIMD-friendly matcher code path; unpaired surrogates survive
  round-trips.
- Windows filesystem case-insensitivity is not mirrored by default; consumers
  wanting it must opt in (documented).
- String-only convenience APIs remain available separately.
