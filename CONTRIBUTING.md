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

There is no continuous wall-time threshold. The deterministic allocation-count
test in
[`allocation_regression.rs`](crates/ferralk/tests/allocation_regression.rs)
does gate matcher and serial-walker hot-path allocation floors on every
platform and native backend. The walker wall-time lane in
[`walker-bench.yml`](.github/workflows/walker-bench.yml) remains non-gating: it
runs on every pull request and publishes medians as a job summary and artifact.

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

## Communicate pre-1.0 contract changes

During `0.x`, a consumer-facing behaviour change must be marked as breaking in
its Conventional Commit. Put `!` after the type or scope (for example,
`fix(walker)!: preserve caller cancellation`) and add a filled-in `BREAKING
CHANGE:` footer that states the old and new observable behaviour. The marker
selects the version bump; the footer gives Release Please the consumer-facing
text it renders into the changelog. Do this even when the Rust type signatures
are unchanged: changed runtime errors, validation, cancellation, traversal,
matching, and default policy are all part of the consumer contract.

Release Please recognizes those markers and renders a dedicated breaking-change
section in the release notes. Describe the old and new observable behaviour in
the pull request as well, so the generated summary has the context consumers
need.

Every pull-request title and every non-merge commit subject on its head branch
must use `<type>[(scope)][!]: <summary>`. Pull requests are squash-merged, and
the pull-request title becomes the single subject that Release Please sees on
the default branch. CI ignores merge commits within a head branch because
Release Please ignores their unparsable subjects too. CI accepts `feat`, `fix`,
`perf`, `deps`, `chore`, `docs`, `refactor`, `test`, `build`, and `ci`; use `!`
and the `BREAKING CHANGE:` footer described above for a consumer-facing contract
change.

## Before opening a pull request

```sh
cargo fmt --all --check
cargo clippy --workspace --exclude oracle --all-targets --locked -- -D warnings
cargo test --workspace --exclude oracle --locked
cargo run -p harness -- corpus
cargo check --manifest-path fuzz/Cargo.toml \
  --bin pattern_parser --bin pattern_matcher --bin ferralk_vs_fast_glob --locked
```

This is the canonical portable preflight for a pull request and needs no Zig
installation. Its Git-backed ignore test requires Git 2.52.0 or newer. On an
older Git release that test skips and still passes; its companion
`git_ignore_oracle_version_is_reported` prints the detected version and whether
the corpus was replayed or skipped, visible with
`cargo test -p harness --test git_check_ignore -- --show-output`, and fails
when `FERRALK_REQUIRE_GIT_ORACLE=1` is set. CI sets that variable while
replaying with the exact reference release, Git 2.52.0. The separate fuzz
workspace is
included because root-workspace commands do not compile it. The
development-only `oracle` package links zlob; include it by dropping
`--exclude oracle` only after installing Zig 0.16 and libclang.

CI has additional platform, sanitizer, coverage, and policy lanes. In
particular, coverage includes `oracle` and installs Zig itself; that CI setup
does not add Zig to this local contributor preflight.

Changes to the native backends also need `--features native-macos` or
`--features native-linux` on the platform that has them; the corresponding CI
jobs are the gate for the other one.
