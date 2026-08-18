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
