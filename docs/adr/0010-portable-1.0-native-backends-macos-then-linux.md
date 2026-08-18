# ADR-0010: Portable-only 1.0; native backends macOS → Linux; Windows tier 2

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

The scheduler plus pruning captures most of the speedup portably; native
backends (batched directory syscalls) fight for the last ~10–20% and carry the
project's only substantial `unsafe`. Gating 1.0 on them would put months of
ABI work in front of API stability. We optimize for macOS and Linux; Windows
is not a performance target for us.

## Decision

- 1.0 ships portable (`std::fs`) on all platforms; its performance gate is
  "faster than parallel `ignore` + pruning on all traversal corpora".
- Native backends are post-1.0, feature-gated 1.x releases: macOS first
  (`getdirentries64`/`getattrlistbulk`; daily dogfooding), Linux second
  (`getdents64`).
- Windows is a tier-2 target: built and tested in CI on the portable backend,
  full matching correctness (ADR-0005), but no native backend and no
  performance gates — permanently, unless demand changes.

## Consequences

- API stability is decoupled from unsafe backend work; each backend is
  independently reviewable and can miss a release without blocking anything.
- Windows users get correctness and portable speed only.
- The 20%-of-zlob native gate applies per platform, macOS and Linux only.
