# Ferralk

[![crates.io](https://img.shields.io/crates/v/ferralk.svg)](https://crates.io/crates/ferralk)
[![docs.rs](https://docs.rs/ferralk/badge.svg)](https://docs.rs/ferralk)
[![CI](https://github.com/sebastian-software/ferralk/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/sebastian-software/ferralk/actions/workflows/ci.yml)
[![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue.svg)](docs/adr/0004-msrv-stable-minus-two.md)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Ferralk finds files. Give it a glob such as `{src,packages}/**/*.{ts,tsx}` and
it walks the tree in parallel, applies `.gitignore` the way Git does, and opens
only the directories the pattern can reach. Filenames stay raw bytes and native
`Path` values from the first directory read to the last match, so nothing is
lost to a UTF-8 conversion.

On the repository-shaped fixture in the
[benchmark snapshot](#local-benchmark-snapshot) below it is the fastest arm
measured: ahead of `ignore`, `jwalk`, `walkdir`, `globwalk` and `wax`, and
level with or ahead of the Zig library it learned from. The same comparison on
Linux CI puts the margin wider — 2.1x over the next-fastest arm on the
unscoped query and 3.4x on the scoped one — and there Ferralk walking
*serially* still beats four-thread `jwalk`. One tree shape and a warm cache,
which the snapshot states: a reproducible measurement rather than a claim to
be the fastest walker everywhere.

It is published as two crates, so a consumer that only needs matching does not
pay for traversal dependencies:

- `ferralk-glob` compiles a glob once and matches it against arbitrary bytes.
- `ferralk` walks filesystems with root-relative include and exclude patterns,
  and re-exports `ferralk-glob`.

Ferralk is independently developed and inspired by
[zlob 1.6.3](https://github.com/dmtrKovalenko/zlob). Its matcher and walker
behaviour are checked against a frozen zlob reference, but it is not source
compatible with zlob and deliberately has no C ABI.

## Why Ferralk

- **The pattern prunes the walk.** `{src,packages}/**/*.{ts,tsx}` names the
  only roots that can match, so `node_modules` is never opened. Ferralk works
  that out from the pattern. With `ignore`, the same saving needs a
  hand-written `filter_entry`, and passing the globs as overrides alone still
  reads the whole tree. On the 53,601-file fixture in the
  [benchmark snapshot](#local-benchmark-snapshot) below, that is 4.32 ms
  against 7.61 ms for hand-pruned `ignore` — and Ferralk walking *serially*
  still beats parallel `ignore` with the globs as overrides, because not
  opening a directory beats opening it on four threads.
- **Sized for the machine, not for the core count.** A warm walk is filesystem
  metadata work: profiled on macOS, 95% of it is `openat`, `getdirentries64`,
  and `close`, and a raw C walker issuing the same syscalls is no faster. What
  actually limits it is how much concurrency the kernel's namespace layer
  rewards, which on Apple Silicon is one performance cluster rather than every
  core. Ferralk defaults to that measured ceiling instead of
  `available_parallelism`, which is worth 1.7–1.9x out of the box on the test
  host; `Walker::threads` overrides it. The
  [evidence](docs/benchmark-evidence.md#the-default-worker-budget) records the
  controls, the one workload shape that wants more threads, and the hosts this
  was not measured on.
- **Git semantics come from Git.** Ignore verdicts are replayed against
  `git check-ignore` in CI, on Linux and on Windows, instead of being
  reimplemented from memory. Nested `.gitignore` chains, `.ignore` files,
  `.git/info/exclude`, linked worktrees, and `core.ignoreCase` are covered.
- **Nothing surprising is on by default.** Hidden files, symlink following,
  Git ignores, metadata, sorting, and error handling are explicit switches,
  and the [usage guide](docs/usage.md) documents each one with its default.
- **Safe Rust.** The matcher crate forbids `unsafe`, the portable walker denies
  it, and CI fails on any `unsafe` outside the two audited native-backend
  modules, which are opt-in features.
- **Git is the oracle, not a spec someone read once.** The 822 checked-in
  behavioural cases are a machine-readable corpus rather than a side effect of
  the tests, and the ignore cases among them are replayed against
  `git check-ignore` itself, on Linux and on Windows, pinned to Git 2.52.0. If
  Git disagrees, CI fails; where Ferralk diverges on purpose, the case carries
  an ADR reference or a recorded oracle defect, and all 44 of them do.
- **Verification beyond a test suite.** Seven fuzz targets, differential checks
  against `fast-glob` and the frozen zlob reference, AddressSanitizer, Miri,
  and loom models of the scheduler protocol, across Linux, macOS, and Windows
  — 32 checks on a pull request. Seventeen ADRs record the decisions rather
  than leaving them to be rediscovered. Performance is measured on every pull
  request and never used as a gate.

### Next to the crates you may already use

| Crate | What it is | Where Ferralk differs |
| --- | --- | --- |
| [`ignore`](https://crates.io/crates/ignore) | ripgrep's parallel walker with Git-ignore support | Its overrides decide what is yielded, not what is opened, so pruning a subtree needs a hand-written `filter_entry`. Ferralk prunes from the include pattern itself. |
| [`globset`](https://crates.io/crates/globset) | Regex-backed matcher in which `*` crosses `/` by default | Ferralk's `*` stays inside one path component, as in a shell, and no pattern is translated to a regex. `WildcardMode::SeparatorCrossing` switches a walk to the `globset` reading when porting patterns. |
| [`walkdir`](https://crates.io/crates/walkdir) | Serial traversal with no selection of its own | Ferralk walks in parallel and selects while it walks. |
| [`zlob`](https://github.com/dmtrKovalenko/zlob) | Zig library behind a C ABI | Ferralk is an independent safe-Rust port of its matcher and walker semantics: no C ABI and no source compatibility. See the [migration guide](docs/compatibility-guide.md). |

## Status

Ferralk is in its 1.0 release candidate. The
[1.x stability contract](docs/stability.md) is settled and unchanged since it
was written: it states which public API, corpus semantics, entry-point rules,
platform tier, and MSRV policy become stable at 1.0, and which implementation
details and feature flags do not. The checked-in
[`cargo public-api` listings](docs/api/) have not moved either, and no
consumer-visible breaking change has landed since the contract was settled.

What the candidate is for is one more adversarial review round. Until `1.0.0`
itself is tagged, treat the guarantee as intended rather than in force. Both
crates are published on crates.io; use the current
0.12.0 release line for applications. <!-- x-release-please-version -->

## Install

```toml
[dependencies]
ferralk = "0.12.0" # x-release-please-version
ferralk-glob = "0.12.0" # x-release-please-version
```

Depend on `ferralk-glob` alone when you only match strings or paths you already
hold. For an unreleased Git dependency, pin a revision for repeatable builds.

## Quick start

Compile a pattern once and match it many times. Syntax that changes meaning is
always opt-in:

```rust
use ferralk_glob::{Pattern, PatternOptions};

let source_file = Pattern::compile(
    "src/**/*.{rs,toml}",
    PatternOptions::default()
        .recursive_double_star(true)
        .braces(true),
)?;

assert!(source_file.is_match_glob_path("src/lib.rs"));
assert!(!source_file.is_match_glob_path("src/generated/lib.rs.bak"));
# Ok::<(), ferralk_glob::PatternError>(())
```

Walk a tree with the same component-aware pattern language:

```rust
use ferralk::{ErrorPolicy, WalkOptions, Walker};

let result = Walker::new(".")
    .include("src/**/*.rs")?
    .exclude("**/generated/**")?
    .respect_git_ignore(true)
    .threads(4)
    .error_policy(ErrorPolicy::Collect)
    .options(WalkOptions::default().files_only(true).sort(true))
    .collect()?;

for entry in result.entries() {
    println!("{}", entry.path().display());
}

for error in result.errors() {
    eprintln!("{error}");
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`collect()` uses available parallelism by default; use `threads(1)` for a
single-threaded collection. Worker budgets are clamped to `1..=256` to bound
per-walk queues and operating-system threads. `stream()` is intentionally
single-threaded and unsorted so it can yield entries incrementally.

For a runnable, Git-ignore-aware mini-find using both crates, try:

```sh
cargo run --example find -- 'src/**/*.rs'
```

The source is in [`crates/ferralk/examples/find.rs`](crates/ferralk/examples/find.rs).

### Four things worth knowing on day one

1. **Ordinary wildcards stay inside one path component.** `*.ts` selects
   `main.ts` in the walk root and not `src/main.ts`; write `**/*.ts` for
   descendants. The walker enables `**`, braces, and extglobs for you, while
   `ferralk-glob` leaves each one opt-in through `PatternOptions`.
2. **Wildcards do not cover a leading period.** `**/*.ts` skips
   `.cache/routes.ts` and the whole `.cache` subtree. Opt in with
   `Walker::match_hidden(true)`, or drop hidden entries before any pattern is
   consulted with `WalkOptions::skip_hidden(true)`.
3. **Patterns use `/` on every platform, and `\` escapes.** Keep the root in
   `Walker::new(root)` and write the pattern relative to it. Joining a
   `PathBuf` into a pattern on Windows produces `\*`, which asks for a literal
   star.
4. **Nothing runs until you ask.** `.gitignore` is not read, symlinks below the
   root are not followed, metadata is not fetched, and entries are not sorted
   by default. Recoverable errors are collected next to the entries under the
   default `ErrorPolicy::Collect`, so a walk never silently returns less than
   it found.

The [usage guide](docs/usage.md) starts with a table of every default and the
switch that changes it.

## Local benchmark snapshot

This snapshot was refreshed on 2026-09-04 on a MacBook Pro with an Apple M1
Pro, 10 cores, 32 GB RAM, macOS 26.6.2, Rust 1.97.1, and Node.js 26.8.1. Each
lane was run on its own on an idle machine rather than chained behind the
others, which matters more than it sounds: an earlier chained refresh on this
same host and commit read roughly 60% slower across every arm. It is a local
comparison, not a portable promise. Lower is better.

Reading it in one line: Ferralk walks the unscoped query in 20.83 ms against
23.15 ms for the next-fastest Rust arm, and the scoped query in 4.32 ms
against 7.61 ms for `ignore` with hand-written pruning — the second gap is the
structural one, because it comes from never opening `node_modules` rather than
from opening it faster.

Matcher values use the common `src/**/*.rs` syntax. The Rust and zlob rows are
Criterion point estimates; the Node.js rows are medians of fifteen
order-rotated samples, each containing 100,000 matches.

| Matcher | Match | Non-match |
| --- | ---: | ---: |
| Ferralk (compiled) | **11 ns** | 3 ns |
| zlob 1.6.5 (compiled) | 35 ns | **2 ns** |
| `globset` (compiled) | 38 ns | 37 ns |
| `fast-glob` (interpreted) | 99 ns | 109 ns |
| `wax` (compiled) | 32 ns | 31 ns |
| `picomatch` 4.0.7 (compiled, Node.js) | 75.0 ns | 85.4 ns |
| `micromatch` 4.0.8 (compiled, Node.js) | 71.7 ns | 84.5 ns |
| `minimatch` 10.2.6 (compiled, Node.js) | 207.0 ns | 191.9 ns |

The walker fixture contains 53,601 files. An **unscoped** query such as
`**/*.{ts,tsx}` can match anywhere, so every walker must inspect the complete
tree. A **scoped** query such as `{src,packages}/**/*.{ts,tsx}` names the only
roots that can match. Ferralk can therefore skip the rest of the tree,
especially `node_modules`, directly from the pattern.

| Walker | Unscoped, 7,400 matches | Scoped, 2,600 matches |
| --- | ---: | ---: |
| Ferralk portable, 4 threads | 20.83 ms | 4.32 ms |
| zlob 1.6.5, same portable run | 30.09 ms | 22.08 ms |
| Ferralk macOS-native, 4 threads | **20.83 ms** | **3.68 ms** |
| zlob 1.6.5, same native run | 21.45 ms | 22.32 ms |
| Node.js `node:fs` sync | 201.53 ms | 26.42 ms |
| `glob` 13.0.6 async | 35.12 ms | 6.34 ms |
| `fast-glob` 3.3.3 async | 41.14 ms | 6.62 ms |
| `tinyglobby` 0.2.17 async | 36.14 ms | 7.14 ms |
| `fdir` 6.5.0 + `picomatch` async | 35.12 ms | 31.39 ms |

The Rust walker rows are Criterion point estimates; the Node.js rows are
medians of ten order-rotated samples. The APIs and estimators are not identical,
so these are same-host context rather than a universal ranking. zlob appears
twice to keep each Rust comparison inside one invocation; against the native
backend the two engines are level on the unscoped query, and the wider portable
margin is what reading directories through `std::fs` costs. See
[benchmark evidence](docs/benchmark-evidence.md) for the complete tables,
exact commands, sampling details, semantic differences, and limitations.

For the same comparison on real repositories instead of fixtures — including the
`ignore`-with-hand-pruning arm the RFC's question really turns on — see
[Palamedes adoption](docs/palamedes-adoption.md).

## Documentation

Using Ferralk:

- [Usage guide](docs/usage.md) — every default and switch, matching and
  walking semantics, and operational guidance.
- [1.x stability contract](docs/stability.md) — the semver-covered surface,
  MSRV policy, and explicit exclusions.
- [Benchmark evidence](docs/benchmark-evidence.md) — what is measured, how to
  reproduce it, and what it does not establish.
- [Verification depth](docs/verification-comparison.md) — the checked-in
  evidence counted next to comparable crates, and where counting misleads.
- [Palamedes adoption](docs/palamedes-adoption.md) — a consumer integration
  measured over four releases on two real repositories, and what each round
  changed here.

Coming from another library:

- [Migration guide](docs/compatibility-guide.md) — mapping from zlob 1.6.3,
  porting patterns from `globset` and `fast-glob`, and the deliberate
  differences.
- [Compatibility matrix](docs/compatibility-matrix.md) — feature-level status
  against zlob.

Contributing:

- [Contributing](CONTRIBUTING.md) — the preflight, commit conventions, and
  the 1.0 release checklist.
- [Corpus format](docs/corpus-format.md) — the JSONL behavioural test corpus
  that is the source of truth for matcher and walker semantics.
- [Architecture RFC](RFC-zig-free-zlob-port.md) and [ADRs](docs/adr/README.md)
  — design rationale and non-goals.
- [Deferred follow-up](docs/external-release-gates.md) — open work tracked in
  GitHub after the initial release.

The [documentation index](docs/README.md) lists everything, including the
historical audit records.

## Principles

- Keep filesystem paths as `Path`/`PathBuf`; never turn filenames into lossy
  UTF-8 strings.
- Keep ordinary glob wildcards inside one path component. Use explicit `**`
  for recursive traversal.
- Make policy choices explicit: hidden files, symlinks, Git ignores, metadata,
  sorting, error handling, and cancellation all have documented controls.
- Keep matching byte-first and portable. On Windows, the public API preserves
  native paths while normalizing only the matcher input representation.

The default portable backend is the stable target. The `native-linux` and
`native-macos` features are experimental and may be renamed or removed in a
1.x minor release; everything behind them is outside the 1.x compatibility
contract.

## Development

Run the single canonical [pull-request preflight in
CONTRIBUTING](CONTRIBUTING.md#before-opening-a-pull-request). It includes the
separate fuzz workspace and does not require Zig; CI installs Zig for its
additional coverage lane. That guide also records repository policy, including
commit signing and how performance claims are evidenced. See
[the contributor-oriented guide](docs/usage.md#development-and-validation) for
the corpus, fuzzing, and benchmark commands.

## License and attribution

Ferralk is MIT licensed. It is an independent project inspired by zlob 1.6.3;
provenance and attribution are recorded in
[NOTICE](https://github.com/sebastian-software/ferralk/blob/main/NOTICE) and the
[frozen reference](docs/zlob-1.6.3-reference.md). Report vulnerabilities through
the private process in [SECURITY.md](SECURITY.md).
