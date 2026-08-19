# ADR-0010: Portable-only 1.0; native backends macOS → Linux; Windows tier 2

- **Status:** Accepted (amended 2026-08-19)
- **Date:** 2026-08-18

## Context

The scheduler plus pruning captures most of the speedup portably; native
backends (batched directory syscalls) fight for the last ~10–20% and carry the
project's only substantial `unsafe`. Gating 1.0 on them would put months of
ABI work in front of API stability. We optimize for macOS and Linux; Windows
is not a performance target for us.

## Decision

- 1.0 ships portable (`std::fs`) on all platforms. Performance is assessed
  through reproducible comparison measurements, not a release gate.
- Native backends are post-1.0, feature-gated 1.x releases: macOS first
  (`getdirentries64`/`getattrlistbulk`; daily dogfooding), Linux second
  (`getdents64`).
- Windows is a tier-2 target: built and tested in CI on the portable backend,
  full matching correctness (ADR-0005), but no native backend.

## Consequences

- API stability is decoupled from unsafe backend work; each backend is
  independently reviewable and can miss a release without blocking anything.
- Windows users get correctness and portable speed only.
- Native macOS and Linux measurements inform implementation decisions but do
  not impose zlob-relative, p95, CI, or release thresholds.
- Native metadata continues to expose `std::fs::Metadata`; `statx` is deferred
  until a distinct native-attribute API is approved, rather than adding a
  second syscall without a public result.

## Evidence, 2026-08-19

The portable-first bet is confirmed. On scoped queries — the shape a consumer
actually issues — ferralk measures about 4.3× zlob, because pruning avoids the
work rather than performing it faster. zlob leads by about 20%, and only on
full traversal, where there is nothing to prune. Choosing a portable core with
native backends as an optimisation, rather than the reverse, is what made the
pruning work available on every platform.

Amendment only; the decision above is unchanged.
