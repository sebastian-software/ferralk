# ADR-0002: Hybrid port strategy — mechanical matcher port, fresh walker

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

A full line-by-line transliteration of zlob would preserve incidental
architecture, C-ABI constraints, and Zig memory-management idioms. But the
matcher's POSIX/extglob/brace dialect is exactly the part that should not be
reinvented from memory, and Ferroni (our pure-Rust Oniguruma port) proved
in-house that faithful 1:1 porting of a matching engine works and performs.

## Decision

Split the port by component nature:

- **Matcher core:** mechanical, behavior-faithful port of zlob's scalar
  matcher — pure functions, no syscalls, no ABI. Ported modules carry a
  provenance header naming the zlob source file and the v1.6.3 commit. The
  transliterated structure is scaffolding: it is validated against the
  differential corpus first, then refactored toward the immutable IR.
- **Walker, scheduler, filesystem backends:** designed fresh around Rust
  ownership and error models; never transliterated.

## Consequences

- Fast, low-risk capture of the full dialect semantics.
- A planned refactor debt in the matcher (port → IR) that must not be skipped.
- The corpus must exist before matcher code, or port correctness is unprovable
  (see ADR-0007).

## Implementation evidence

The post-corpus refactor is complete. `Pattern::compile` produces immutable
`CompiledAlternative` values, including the derived component-sensitive
path-filter representation. Matching only borrows this compiled IR; the
failure memo is deliberately per invocation and is not part of the compiled
state. The direct IR, fast-path equivalence, corpus, and oracle tests provide
the regression evidence for this boundary.
