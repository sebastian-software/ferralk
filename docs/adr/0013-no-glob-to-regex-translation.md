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

- **Wrong machine.** Glob matching needs a machine sized to the token IR — a
  memoized walk over (token, position) pairs, bounded by their product; a
  general regex engine carries program dispatch, captures, and lookaround
  machinery a glob never needs.
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

## Correction, 2026-08-20

The first reason above described the matcher as "the classic two-pointer /
last-star algorithm". It never was one. The general path is a memoized
depth-first walk of the (token, path position) state graph: one star's
repetition branch is deferred to an explicit work list while its other
successor continues in place, and `FailedStates` memoizes visited pairs, which
is what bounds the exploration to `tokens × path` rather than to the single
backtrack point a two-pointer scan keeps.

The distinction matters for the third reason, not just for accuracy. A
two-pointer scan is linear by construction; a memoized walk is linear in
`tokens × path`, which is a weaker guarantee — the `backtracking` row of
[benchmark evidence](../benchmark-evidence.md) is what that costs on a pattern
built to exercise it. Nothing about the decision changes: the state graph is
still the machine a glob needs, still smaller than a regex program, and still
free of the dialect translation the fourth reason is about. What changes is
that the linearity claim is the memoized walk's, and is stated as such.

Amendment only; the decision above is unchanged.
