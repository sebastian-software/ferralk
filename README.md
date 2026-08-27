# Ferralk

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
crates.io; use the current 0.9.0 release line for applications. <!-- x-release-please-version -->

```toml
[dependencies]
ferralk = "0.9.0" # x-release-please-version
ferralk-glob = "0.9.0" # x-release-please-version
```

For an unreleased Git dependency, pin a revision for repeatable builds.

## Local benchmark snapshot

The matcher snapshot below was refreshed on 2026-08-27 on a Mac Studio with an
Apple M1 Ultra, 20 cores, 64 GB RAM, macOS 26.5.2, Rust 1.96.0, and
Criterion 0.8.2. It is a local comparison, not a portable promise. The zlob
row was refreshed on the same host with zlob 1.6.3, Zig 0.16.0, and libclang
22.1.8. The expanded multi-library walker comparison is in
[benchmark evidence](docs/benchmark-evidence.md), which also describes the
lanes, limitations, and reproduction commands. Matcher values use the common
`src/**/*.rs` syntax; lower is better.

| Matcher | Match | Non-match |
| --- | ---: | ---: |
| Ferralk (compiled, 2026-08-27) | 11 ns | 3 ns |
| zlob 1.6.3 (compiled, 2026-08-27) | 37 ns | 2 ns |
| globset (compiled, 2026-08-27) | 40 ns | 40 ns |
| fast-glob (interpreted, 2026-08-27) | 98 ns | 107 ns |
| wax (compiled, 2026-08-27) | 31 ns | 31 ns |

On this common syntax Ferralk is faster than both `globset` and `fast-glob` in
the current refresh. The complete current matcher table, including long-path
and adversarial cases, is in the benchmark evidence document.

The following small-fixture Walker snapshot is retained from 2026-08-19 for
continuity. The current 53,600-file, multi-library Walker comparison is in the
benchmark evidence document; lower is better.

| Walker | Time |
| --- | ---: |
| Ferralk serial | 1.48 ms |
| Ferralk parallel | 0.99 ms |
| `ignore` parallel | 3.00 ms |
| zlob parallel | 0.86 ms |

Ferralk parallel is about 3× faster than `ignore` on this fixture, while zlob
is about 1.15× faster than Ferralk. That ordering is fixture-dependent: on a
5120-file tree the wall-time lane measures `ignore` parallel level with Ferralk
on Linux and behind it on macOS. Treat any single row as one shape, and see
[benchmark evidence](docs/benchmark-evidence.md) for the lanes that carry the
broader picture.

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

```sh
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p harness -- corpus
```

See [CONTRIBUTING](CONTRIBUTING.md) for repository policy — commit signing and
how performance claims are evidenced — and
[the contributor-oriented guide](docs/usage.md#development-and-validation) for
the corpus, fuzzing, and benchmark commands.

## License and attribution

Ferralk is MIT licensed. It is an independent project inspired by zlob 1.6.3;
provenance and attribution are recorded in
[NOTICE](https://github.com/sebastian-software/ferralk/blob/main/NOTICE) and the
[frozen reference](docs/zlob-1.6.3-reference.md).
