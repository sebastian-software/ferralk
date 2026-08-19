# ADR-0012: Ferroni repository blueprint for tooling

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

Ferroni established a proven repo setup for a performance-focused pure-Rust
port at sebastian-software: continuous benchmark regression tracking, coverage
reporting, automated releases and dependency updates. Diverging per project
costs onboarding time and comparability.

## Decision

Adopt the Ferroni blueprint unchanged: `criterion` with
`codspeed-criterion-compat` in CI (continuous regression tracking instead of
one-off measurements), codecov, release-please for versioning and changelogs,
renovate for dependency updates. Bench corpora follow the RFC's performance
section (flat trees, deep trees, dependency-heavy trees, git repos with
negation, non-UTF-8 names, symlink cycles, real manifests).

## Consequences

- Zero tooling ramp-up; the performance story has a regression curve from
  day one.
- Portfolio consistency across sebastian-software Rust projects.

## Amendment, 2026-08-19: CodSpeed removed

The blueprint's continuous regression tracking is withdrawn for this
repository. `codspeed-criterion-compat` is replaced by plain `criterion`, and
the CodSpeed workflow is deleted.

The reason is evidence rather than preference. Over the period it ran, the
CodSpeed lane produced four false alarms and no true finding; each one was a
stale baseline attributed to whichever pull request happened to be open, and
each cost a round of investigation to attribute correctly. What did catch
regressions was the back-to-back measurement a contributor takes locally,
before and after, and reports in the pull request. For a library of this scope
that discipline is the effective protection, and the walker wall-time lane
keeps an automated check on the part where elapsed time is the only meaningful
unit.

This is a deliberate divergence from the Ferroni blueprint, recorded here so
the next repository adopting it can weigh the same trade-off. Everything else
in the decision — codecov, release-please, renovate, the bench corpora — is
unchanged.
