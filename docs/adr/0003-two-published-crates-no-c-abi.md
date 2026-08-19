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
  independently from the faster-moving walker at the API level.
- One workspace version governs both published crates. Release Please updates
  the workspace manifest, the internal `ferralk` → `ferralk-glob` dependency,
  and the workspace lockfile in one release PR; the publish workflow sends
  `ferralk-glob` to crates.io before `ferralk`.
- Non-Rust consumers are out of scope for now.

## Scope confirmation

On 2026-08-19, the Rust-only boundary was reconfirmed for Ferralk 1.0. The
frozen zlob corpus covers the public Rust-shaped semantics; C ABI, loader/TLS,
callback, result-buffer, and libc-shell surfaces remain explicit non-goals.
A future compatibility layer is a separate product decision rather than a
compatibility requirement of these two crates.
