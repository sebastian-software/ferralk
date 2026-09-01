# Ferralk

[![crates.io](https://img.shields.io/crates/v/ferralk.svg)](https://crates.io/crates/ferralk)
[![docs.rs](https://docs.rs/ferralk/badge.svg)](https://docs.rs/ferralk)
[![CI](https://github.com/sebastian-software/ferralk/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/sebastian-software/ferralk/actions/workflows/ci.yml)
[![MSRV 1.96](https://img.shields.io/badge/MSRV-1.96-blue.svg)](docs/adr/0004-msrv-stable-minus-two.md)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Ferralk is a pure-Rust toolkit for byte-first glob matching and portable,
parallel filesystem walking. It is split into two crates so consumers that only
need matching do not pay for traversal dependencies:

- `ferralk-glob` compiles and reuses glob patterns over arbitrary bytes.
- `ferralk` walks filesystems, applies root-relative include/exclude patterns,
  and re-exports `ferralk-glob`.

Ferralk is independently developed and inspired by
[zlob 1.6.3](https://github.com/dmtrKovalenko/zlob). Its matcher and walker
behaviour are checked against a frozen zlob reference, but it is not source
compatible with zlob and deliberately has no C ABI.

## Status

Ferralk is pre-1.0. The public API and release cadence are still being refined;
do not rely on a 1.0 stability guarantee yet. Both crates are published on
crates.io; use the current 0.10.0 release line for applications. <!-- x-release-please-version -->

```toml
[dependencies]
ferralk = "0.10.0" # x-release-please-version
ferralk-glob = "0.10.0" # x-release-please-version
```

For an unreleased Git dependency, pin a revision for repeatable builds.

## Local benchmark snapshot

This snapshot was refreshed on 2026-08-28 on a Mac Studio with an Apple M1
Ultra, 20 cores, 64 GB RAM, macOS 26.5.2, Rust 1.96.0, and Node.js 24.19.0. It
is a local comparison, not a portable promise. Lower is better.

Matcher values use the common `src/**/*.rs` syntax. The Rust and zlob rows are
Criterion point estimates; the Node.js rows are medians of fifteen
order-rotated samples, each containing 100,000 matches.

| Matcher | Match | Non-match |
| --- | ---: | ---: |
| Ferralk (compiled) | **10 ns** | 3 ns |
| zlob 1.6.3 (compiled) | 37 ns | **2 ns** |
| `globset` (compiled) | 39 ns | 39 ns |
| `fast-glob` (interpreted) | 97 ns | 106 ns |
| `wax` (compiled) | 31 ns | 31 ns |
| `picomatch` 4.0.7 (compiled, Node.js) | 80.9 ns | 90.9 ns |
| `micromatch` 4.0.8 (compiled, Node.js) | 80.6 ns | 91.0 ns |
| `minimatch` 10.2.6 (compiled, Node.js) | 238.3 ns | 219.0 ns |

The walker fixture contains 53,600 files. An **unscoped** query such as
`**/*.{ts,tsx}` can match anywhere, so every walker must inspect the complete
tree. A **scoped** query such as `{src,packages}/**/*.{ts,tsx}` names the only
roots that can match. Ferralk can therefore skip the rest of the tree,
especially `node_modules`, directly from the pattern.

| Walker | Unscoped, 7,400 matches | Scoped, 2,600 matches |
| --- | ---: | ---: |
| Ferralk portable, 4 threads | 37.78 ms | **4.29 ms** |
| zlob 1.6.3, same portable run | **20.12 ms** | 20.30 ms |
| Ferralk macOS-native, 4 threads | 33.90 ms | 6.27 ms |
| zlob 1.6.3, same native run | 30.22 ms | 20.93 ms |
| Node.js `node:fs` sync | 286.49 ms | 31.51 ms |
| `glob` 13.0.6 async | 48.56 ms | 8.08 ms |
| `fast-glob` 3.3.3 async | 50.73 ms | 8.29 ms |
| `tinyglobby` 0.2.17 async | 47.34 ms | 8.21 ms |
| `fdir` 6.5.0 + `picomatch` async | 47.08 ms | 42.09 ms |

The Rust walker rows are Criterion point estimates; the Node.js rows are
medians of ten order-rotated samples. The APIs and estimators are not identical,
so these are same-host context rather than a universal ranking. zlob appears
twice to keep each Rust comparison inside one invocation; its spread shows the
host-load noise between the two runs, not a backend change. See
[benchmark evidence](docs/benchmark-evidence.md) for the complete tables,
exact commands, sampling details, semantic differences, and limitations.

For the same comparison on real repositories instead of fixtures — including the
`ignore`-with-hand-pruning arm the RFC's question really turns on — see
[Palamedes adoption](docs/palamedes-adoption.md).

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

## Documentation

- [Usage guide](docs/usage.md) — matching and walking semantics, defaults, and
  operational guidance.
- [Migration guide](docs/compatibility-guide.md) — mapping from zlob 1.6.3 and
  deliberate differences.
- [Compatibility matrix](docs/compatibility-matrix.md) — feature-level status.
- [Corpus format](docs/corpus-format.md) — the JSONL behavioural test corpus.
- [Architecture RFC](RFC-zig-free-zlob-port.md) and [ADRs](docs/adr/README.md)
  — design rationale and non-goals.
- [Palamedes adoption](docs/palamedes-adoption.md) — a consumer integration
  measured over four releases on two real repositories, and what each round
  changed here.
- [Deferred follow-up](docs/external-release-gates.md) — open work tracked in
  GitHub after the initial release.

## Principles

- Keep filesystem paths as `Path`/`PathBuf`; never turn filenames into lossy
  UTF-8 strings.
- Keep ordinary glob wildcards inside one path component. Use explicit `**`
  for recursive traversal.
- Make policy choices explicit: hidden files, symlinks, Git ignores, metadata,
  sorting, error handling, and cancellation all have documented controls.
- Keep matching byte-first and portable. On Windows, the public API preserves
  native paths while normalizing only the matcher input representation.

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
