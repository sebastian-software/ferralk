# Contributing

Start with the [documentation index](docs/README.md). The
[usage guide](docs/usage.md) carries the commands a change has to pass, the
[corpus format](docs/corpus-format.md) governs behavioral cases, and the
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
does gate matcher, serial-walker, and parallel wide-sibling hot-path allocation
floors on every platform and native backend. The walker wall-time lane in
[`walker-bench.yml`](.github/workflows/walker-bench.yml) remains non-gating: it
runs on every pull request and publishes medians as a job summary and artifact.

A change that claims a performance effect carries its own evidence: run the
relevant bench before and after on one machine, back to back, and put both
numbers in the pull request body along with the fixture they describe. State
what the measurement does not establish — a warm page cache, one tree shape,
one platform — rather than leaving a reader to assume it generalizes.

The CodSpeed simulation lane that used to run here was removed on 2026-08-19.
Over the period it ran it produced four false alarms and no true finding, each
one a stale baseline attributed to whichever pull request was open at the time.
[ADR-0012](docs/adr/0012-ferroni-repository-blueprint.md) records that
amendment, and [benchmark evidence](docs/benchmark-evidence.md) describes the
lanes that remain.

## Communicate pre-1.0 contract changes

During `0.x`, a consumer-facing behavior change must be marked as breaking in
its Conventional Commit. Put `!` after the type or scope (for example,
`fix(walker)!: preserve caller cancellation`) and add a filled-in `BREAKING
CHANGE:` footer that states the old and new observable behavior. The marker
selects the version bump; the footer gives Release Please the consumer-facing
text it renders into the changelog. Do this even when the Rust type signatures
are unchanged: changed runtime errors, validation, cancellation, traversal,
matching, and default policy are all part of the consumer contract.

Release Please recognizes those markers and renders a dedicated breaking-change
section in the release notes. Describe the old and new observable behavior in
the pull request as well, so the generated summary has the context consumers
need.

The marker convention does not change at 1.0; what the marker *costs* does.
Release Please's `bump-minor-pre-major` setting is what turns a `!` into a minor
bump today. From 1.0 on, the same `!` proposes a major release, so the decision
of whether a change is consumer-visible stops being a changelog-formatting
question and becomes the decision of whether to spend a major version. Use the
[1.x stability contract](docs/stability.md) to answer it: a change confined to
the surfaces that document lists as outside the contract — performance,
scheduling and batching internals, native-backend features, diagnostic
wording — is not a `!` change however visible it is in a benchmark.

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
./scripts/readme-family-block.sh --check
mise run readme:check
```

This is the canonical portable preflight for a pull request and needs no Zig
installation. Its Git-backed ignore test requires Git 2.52.0 or newer. On an
older Git release that test skips and still passes; its companion
`git_ignore_oracle_version_is_reported` prints the detected version and whether
the corpus was replayed or skipped, visible with
`cargo test -p harness --test git_check_ignore -- --show-output`, and fails
when `FERRALK_REQUIRE_GIT_ORACLE=1` is set. CI sets that variable while
replaying with the exact reference release, Git 2.52.0. The separate fuzz
workspace is included because root-workspace commands do not compile it. The
development-only `oracle` package links zlob; include it by dropping
`--exclude oracle` only after installing Zig 0.16 and libclang.

Ignore rules are split for the same reason. The root `.gitignore` anchors its
build entry at `/target/`, so it deliberately does not reach into the fuzz
workspace; `fuzz/.gitignore` covers `target/` and the `artifacts/` directory
`cargo fuzz` writes when a target crashes. Keep both files, and add a fuzz
ignore rule to `fuzz/.gitignore` rather than to the root.

CI has additional platform, sanitizer, coverage, and policy lanes. In
particular, coverage includes `oracle` and installs Zig itself; that CI setup
does not add Zig to this local contributor preflight.

Coverage gates rather than only reports, and this repository's own `coverage`
job is the whole gate — no external coverage service is involved. The job
enforces the line floor with `--fail-under-lines` and writes
`Line coverage: X% (gate: ≥ N%)` into the run summary whether it passes or
fails, so the measured figure is readable from the run itself.

The floor is declared once, as `COVERAGE_MIN_LINES` in that job in
[.github/workflows/ci.yml](.github/workflows/ci.yml). The number in the command
below and the one in the README's coverage badge repeat it, and
`cargo test -p doc-tests` fails if either drifts from the workflow — so change
the workflow first and let that contract point at the rest. Reproducing the
gate locally is not part of the preflight above, but it is the same command:

```sh
cargo llvm-cov --workspace --lcov --output-path lcov.info \
  --fail-under-lines 90
```

Without Zig 0.16 and libclang, add `--exclude oracle`; the resulting figure is
then a little different from CI's, which measures the whole workspace. The
`lcov.info` the run drops in the repository root is ignored by Git.

Changes to the native backends also need `--features native-macos` or
`--features native-linux` on the platform that has them; the corresponding CI
jobs are the gate for the other one.

## The Ferramenta family block

The root README is generated by native mdtheme from `README.md.src`. Sebastian
Software is the outer frame and Ferramenta the inner frame. Edit project prose
in the source, then run `mise run readme:write`; `mise run readme:check` checks
the entire result. See [README themes](docs/readme-theme.md) for installation,
CI, and the pre-push command. Standards 0.11.0 explicitly delegates README
ownership to mdtheme and does not append a company footer.

Published subpackage READMEs retain compact, plain-Markdown family blocks.
They use the pinned Ferramenta registry generator and require Node, pnpm, and
network access:

```sh
./scripts/readme-family-block.sh --write
./scripts/readme-family-block.sh --check
```

Update `FERRAMENTA_PIN` in the generator script to adopt a new family revision.
For the root README, update `mdtheme.yaml` and regenerate separately. Commit
pins and outputs together. Never edit generated family text by hand.

## 1.0 release checklist

Use this checklist for the 1.0 release train; the compatibility promise it
protects is defined in [`docs/stability.md`](docs/stability.md).

- [ ] Every child of epic #342 is closed, and every external-oracle divergence
  in the corpus has exactly one `adr` or `oracle_defect` marker.
- [ ] Two consecutive adversarial review rounds have completed without a
  consumer-visible breaking change.
- [ ] Run the canonical preflight above on the release candidate commit and
  verify all platform, oracle, semver, and policy CI jobs.
- [ ] Using `cargo-public-api` 0.52.0, regenerate `docs/api/ferralk.txt` and
  `docs/api/ferralk-glob.txt` with `cargo public-api -p <crate> --simplified
  --color never`; review every changed line as API rather than accepting
  generated output mechanically.
- [ ] Confirm the README still links the stability contract and that the
  contract covers the public API, corpus semantics, matcher entry points,
  Windows tier, MSRV policy, and explicit exclusions.
- [ ] Audit every `!` change since 0.9.0 against the
  [compatibility guide](docs/compatibility-guide.md#contract-change-audit-since-090)
  and usage guide, then check the release notes describe the final behavior.
- [ ] Cut `1.0.0-rc.1` only after the two clean rounds. Keep the release
  candidate for one further adversarial round; cut `1.0.0` only if that round
  also produces no consumer-visible breaking change.
- [ ] If a candidate is cut before the two rounds have run, say so in the
  release pull request. The candidate is then the artifact the rounds run
  against rather than their result, and `1.0.0` still waits for a clean one.
- [ ] Tell Release Please the version explicitly. Its `bump-minor-pre-major`
  setting turns every breaking change on `0.x` into a minor bump, so it never
  proposes a major version on its own. Land a commit on `main` whose footer
  reads `Release-As: 1.0.0-rc.1` for the candidate, and later one with
  `Release-As: 1.0.0`; the release pull request then carries that version and
  the ordinary bump rules resume from it.
- [ ] Check the footer actually landed: `git log -1 --format=%B origin/main`
  must print it. This repository squashes with the pull-request body as the
  commit body, so a footer written in that body reaches `main` only if the
  body is kept at merge time. Clearing or replacing the message in the merge
  dialog silently drops it, Release Please proposes the ordinary bump instead,
  and the release pull request is the first place anyone notices. A one-line
  follow-up commit carrying only the footer fixes it.

#
