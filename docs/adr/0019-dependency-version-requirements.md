# ADR-0019: Caret requirements in the published crates, exact pins everywhere else

- **Status:** Accepted
- **Date:** 2026-09-06

## Context

Renovate ran with `rangeStrategy: "pin"` for the whole repository, so every
Cargo requirement was written as `=` — including the library dependencies of
the two crates that are published to crates.io. For an application, a binary,
or a development tool that is the right default: one resolved version, a
reproducible build, and every update arriving as a reviewed pull request.

For a library it is not. Cargo resolves at most one version per
semver-compatible range, so `memchr = "=2.8.3"` in `ferralk` and
`memchr = "2.9"` anywhere else in the consumer's graph is an unsatisfiable
resolution rather than a shared dependency. The same pin also means a consumer
cannot take a patch release of a transitive dependency — a soundness or
security fix included — until Ferralk itself publishes again. None of the ADRs
recorded a reason to pay that; the pins were a side effect of one Renovate
setting applying to every manifest in the workspace.

Reproducibility comes from `Cargo.lock`, which is checked in, and from the
`--locked` flag the preflight and every CI lane already pass. That guarantee
does not depend on how the requirement is spelled.

## Decision

The library dependencies of `crates/ferralk` and `crates/ferralk-glob` — the
`[dependencies]` and target-conditional `[dependencies]` tables — are caret
requirements naming the minimum version the code actually needs, for example
`memchr = "2.8.3"`.

Everything else stays exactly pinned: `[workspace.dependencies]`, the
`tools/*` packages, the separate `fuzz` workspace, and the dev-dependencies of
the published crates, none of which reach a consumer. Renovate keeps
`rangeStrategy: "pin"` as the repository default and a package rule scoped to
the two crate manifests' `dependencies` uses `rangeStrategy: "replace"`, so an
update there moves the caret floor instead of re-pinning it. The
[ADR-0007](0007-differential-corpus-and-dev-time-oracle.md) freeze on
`tools/oracle/**` is untouched.

## Consequences

- A consumer can unify Ferralk's dependencies with the rest of its graph and
  take patch updates without waiting for a Ferralk release.
- Repository builds stay byte-reproducible through `Cargo.lock` and
  `--locked`; nothing about local or CI determinism changes.
- The declared floor becomes a claim CI does not check: a crate that starts
  using an API added after the named minimum still builds here, because the
  lock file resolves the newest compatible version. If that bites, the answer
  is a `-Z minimal-versions` resolution lane, not a return to `=`.
- Renovate now opens fewer pull requests against the published crates, because
  an in-range release no longer needs a manifest edit. The lock file still
  moves, and that update is still reviewed.
