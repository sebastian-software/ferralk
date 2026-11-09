# ADR-0021: Preserve dependency ranges and reproduce builds from lockfiles

- **Status:** Accepted
- **Date:** 2026-11-09
- **Supersedes:** [ADR-0019](0019-dependency-version-requirements.md)

## Context

ADR-0019 allowed caret requirements in the published crates but required exact
pins for tools, fuzzing, and development dependencies. That split adds manifest
churn for compatible updates even though committed lockfiles already select the
versions used by repository builds. The maintainer decided on 2026-11-09 to use
one range-preserving policy for Cargo and npm across the organization.

## Decision

Cargo and npm dependencies preserve their semver ranges, including tools,
development dependencies, and internal package requirements. Exact pins are not
the default for any of those categories.

Ferralk inherits `rangeStrategy: "update-lockfile"` from the
[organization Renovate preset](https://github.com/sebastian-software/renovate-config)
through [its Renovate configuration](../../renovate.json). Compatible updates
refresh the committed lockfiles without narrowing the manifest ranges. Adopting
a version outside an existing range remains a reviewed manifest change.

Reproducible repository builds use committed `Cargo.lock` and `pnpm-lock.yaml`
files with `--locked` and `--frozen-lockfile`, respectively. A manifest range
states compatibility; its lockfile records the resolved dependency graph.

Deliberate pins keep their narrower purpose:

- The semantic zlob oracle remains frozen under
  [ADR-0007](0007-differential-corpus-and-dev-time-oracle.md).
- [Published-release smoke samples](../../homepage/samples/Cargo.toml) keep exact
  Ferralk release references so they exercise the intended published artifacts.
- Toolchain versions and GitHub Action commit SHAs remain explicitly selected.

Other exact dependency pins require a documented package-specific reason and a
narrow exception. A package being a tool or development dependency is not such a
reason.

## Consequences

Compatible updates primarily change lockfiles, and the repository follows the
shared update policy instead of maintaining a separate pinning policy. CI still
reviews and tests the resolved graph. It does not prove that every version
allowed by a range works; minimum-version validation remains a separate concern.
