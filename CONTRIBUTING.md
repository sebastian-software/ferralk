# Contributing

Start with the [documentation index](docs/README.md). The
[usage guide](docs/usage.md) carries the commands a change has to pass, the
[corpus format](docs/corpus-format.md) governs behavioural cases, and the
[ADRs](docs/adr/README.md) record decisions that are not up for re-litigation
in a pull request.

## Commit signing

Commits in this repository are unsigned. That is a deliberate maintainer
decision taken on 2026-08-19: signing every development commit added friction
without adding a trust property this project relies on.

The trust anchors are the pull request history — every change arrives through a
reviewed pull request whose CI run is recorded — and the merge and release
commits, which GitHub signs with its own key. Verify a release against those,
not against individual authored commits. Do not add signing configuration to
your local clone on this project's behalf.

## Performance evidence

There is no continuous performance gate. Automated protection is the walker
wall-time lane in
[`walker-bench.yml`](.github/workflows/walker-bench.yml), which runs on every
pull request and publishes medians as a job summary and an artifact.

A change that claims a performance effect carries its own evidence: run the
relevant bench before and after on one machine, back to back, and put both
numbers in the pull request body along with the fixture they describe. State
what the measurement does not establish — a warm page cache, one tree shape,
one platform — rather than leaving a reader to assume it generalises.

The CodSpeed simulation lane that used to run here was removed on 2026-08-19.
Over the period it ran it produced four false alarms and no true finding, each
one a stale baseline attributed to whichever pull request was open at the time.
[ADR-0012](docs/adr/0012-ferroni-repository-blueprint.md) records that
amendment, and [benchmark evidence](docs/benchmark-evidence.md) describes the
lanes that remain.

## Before opening a pull request

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p harness -- corpus
```

Changes to the native backends also need `--features native-macos` or
`--features native-linux` on the platform that has them; the corresponding CI
jobs are the gate for the other one.
