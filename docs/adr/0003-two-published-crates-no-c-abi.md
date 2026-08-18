# ADR-0003: Two published crates, no C ABI

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

Matcher-only consumers (the fast-glob audience) should not pull walker
dependencies like `ignore` and `crossbeam-deque`. The C API and the zlob
Rust-API migration facade were artifacts of the abandoned upstream-rewrite
model. Corpus, harness, oracle, and bench tooling need workspace members
regardless of the publishing layout.

## Decision

One Cargo workspace with exactly two published crates:

- **`ferralk-glob`** — the matcher: dependency-light (`memchr` only),
  `#![forbid(unsafe_code)]`.
- **`ferralk`** — the walker engine; depends on and re-exports
  `ferralk-glob`.

Unpublished members: `corpus`, `harness`, `oracle`, `bench`. No C ABI. No zlob
migration facade; a compatibility guide documents the API mapping instead.

## Consequences

- Clean dependency story per audience; the stable matcher can version
  independently from the faster-moving walker.
- Two versions to coordinate at release time.
- Non-Rust consumers are out of scope for now.
