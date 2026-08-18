# ADR-0013: Dedicated glob matcher — no glob-to-regex translation

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

Globs are mechanically translatable to regexes, so the match path could reuse
an existing regex engine — Ferroni in-house, or the `globset` approach (glob →
regex on `regex-automata`), which powers ripgrep. That would avoid porting a
matcher at all.

## Decision

ferralk matches globs with a dedicated matcher; no regex engine sits in the
match path. Ferroni remains a methodological precedent (ADR-0008, ADR-0012),
not a dependency.

Reasons:

- **Wrong machine.** Glob matching runs in linear time with tiny constants via
  the classic two-pointer/last-star algorithm; a general regex engine carries
  program dispatch, captures, and lookaround machinery a glob never needs.
- **Wrong workload shape.** A walker compiles once and then matches millions
  of short (20–80 byte) paths anchored end-to-end; regex engines are tuned for
  finding matches in long texts, so per-call overhead dominates here.
- **Backtracking risk.** Ferroni is deliberately a backtracker (Oniguruma
  class); translated `*a*b*c*` patterns can go superlinear on adversarial
  paths, while the dedicated algorithm stays linear. Even `globset` on
  non-backtracking `regex-automata` measurably trails dedicated glob matchers
  — the empirical confirmation.
- **Translation is a bug source.** Bit-exact zlob compatibility would have to
  survive an extra glob→regex-dialect mapping (leading-dot per segment, `**`
  special cases, `[!...]`, extglob, escapes) — precisely where divergences
  breed.
- **Dependency weight.** `ferralk-glob` stays memchr-only (ADR-0003); a full
  regex engine would land in every matcher consumer.

## Consequences

- We own a matcher implementation — mitigated by the mechanical port and the
  corpus (ADR-0002, ADR-0007).
- `globset` may be added to the matcher benchmarks as the representative of
  the translation approach, keeping this trade-off documented as a measured
  series rather than an argument.
