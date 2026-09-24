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

## Amendment, 2026-09-04: a user-space CPU lane, Callgrind rather than CodSpeed

The 2026-08-19 amendment removed instruction-count measurement entirely. That
went one step too far, and #352 is the evidence: a change can execute much more
user-space work while wall time on a shared runner hides it behind filesystem
and kernel time. Profiling the macOS walk in #362 put a number on how much room
there is to hide in — 95% of a warm walk's samples are in `openat`,
`getdirentries64` and `close`, so the walker's own work could triple and barely
move a median.

A Linux-only Callgrind lane therefore measures instructions for one serial and
one four-thread walk of the repository fixture, merge base and head in the same
job. What makes this different from the lane that was removed is not the
instrument but the baseline: CodSpeed compared against stored history and
misattributed its staleness to whichever pull request was open, while this
compares two commits built and measured in one runner, so there is no history
to go stale.

It is not a speed measurement and the lane says so in its own output.
Callgrind serializes threads and does not model syscall latency, so the
four-thread row reports work performed rather than time taken; moving work
between threads without removing any would look identical. Elapsed time remains
the walker wall-time lane's job, and neither lane gates a release.

The lane lands non-gating. A threshold — provisionally 5% over the merge base —
starts failing the job only after the harness is in the merge base and its
repeatability has been observed across real pull requests, which is the
sequencing #358 asks for and the discipline the CodSpeed experience was missing.

## Amendment, 2026-09-07: Codecov removed, the gate is CI's own

The blueprint's Codecov membership is withdrawn for this repository. The
`codecov/codecov-action` upload is deleted from the `coverage` job and the
badge in the README is replaced by one that names the enforced floor, so no
part of CI reports to a third-party coverage service any more.

Coverage enforcement is unchanged and stays where it already was. The
`--fail-under-lines` floor in that job has been the blocking check since the
job was written; Codecov only ever received a second, non-blocking copy of the
same lcov report, with `fail_ci_if_error: false` precisely so a third-party
outage could not make deterministic CI flaky. A signal that is not allowed to
fail the build is not a gate, and a project this size does not need a hosted
history of a number its own CI already refuses to let drop.

What the removal costs is the per-line web view on pull requests. What it buys
is one fewer third-party service between a pull request and its verdict, and a
figure that is legible without leaving the run: the job now writes `Line
coverage: X% (gate: ≥ N%)` into the step summary, on failure as well as on
success. The floor itself is written down once, as `COVERAGE_MIN_LINES` in
that job, and CONTRIBUTING names it and shows how to reproduce the check
locally.

This is a second deliberate divergence from the Ferroni blueprint, recorded
here for the same reason as the CodSpeed one above. Everything else in the
decision — release-please, renovate, the bench corpora — is unchanged.

## Amendment, 2026-09-24: the reference point is the family baseline, not Ferroni

The decision above measured this repository against one sibling, the Ferroni
repository, and the amendments since were written against it. That reference
point no longer exists in the form the decision assumed. The Ferramenta
family review (sebastian-software/ferramenta#6) replaced "copy the neighbor
that got it right" with a baseline defined once for every family repository:
the repository-hygiene set in
sebastian-software/ferramenta#11 — community files, agent guidance, the ADR
convention, toolchain files — and the CI and release baseline in
sebastian-software/ferramenta#13. Ferroni falls under that baseline like
every other family repository; it is no longer the source.

The baseline is not prose to be copied either. It ships as the
`@sebastian-software/standards` package, and this repository is onboarded to
it: `.repometa.json` records the standards version the repository is stamped
at, the `standards` job in `.github/workflows/ci.yml` runs `standards check`
with a pinned CLI on every pull request, managed files such as `rustfmt.toml`
and the marker-delimited section of `AGENTS.md` are rewritten only by
`standards apply`, and seeded files such as `rust-toolchain.toml`,
`SECURITY.md` and the feature and question issue forms came from its reference
copies and are maintained here. New standards versions arrive as Renovate pull
requests and are adopted through the migration steps the package documents.

From this amendment on, "the blueprint" in this record means that baseline.
Where this repository deliberately deviates from it, the deviation is recorded
in this ADR or in an ADR of its own, as the CodSpeed, Callgrind and Codecov
amendments above already are. All three remain in force against the new
reference point; the Codecov one matters most, since the baseline proposal in
sebastian-software/ferramenta#13 still lists a Codecov upload. The rest of
the original decision — release-please, Renovate and the bench corpora — is
unchanged. What changes is only where a question of "how should this
repository be set up" is answered first: in the standards package and the
family baseline, not in another repository's tree.
