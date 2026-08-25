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
crates.io; use the current 0.6 release line for applications.

```toml
[dependencies]
ferralk = "0.6"
ferralk-glob = "0.6"
```

For an unreleased Git dependency, pin a revision for repeatable builds.

## Local benchmark snapshot

The following macOS measurements use Rust 1.95.0 and Criterion's default
100-sample configuration, taken on 2026-08-19 against the checked-in fixtures.
They are a local comparison, not a portable promise.
[Benchmark evidence](docs/benchmark-evidence.md) describes the lanes, what each
does not establish, and how to reproduce them. Matcher values use the common
`src/**/*.rs` syntax; lower is better.

| Matcher | Match | Non-match |
| --- | ---: | ---: |
| Ferralk (compiled) | 11.59 ns | 2.88 ns |
| zlob 1.6.3 (compiled) | 34.74 ns | 2.36 ns |
| globset (compiled) | 38.43 ns | 37.48 ns |
| fast-glob (interpreted) | 98.75 ns | 108.18 ns |

On this common syntax Ferralk is 0.33× zlob for matches and 1.22× zlob for
non-matches, within the local 1.5× zlob target. It is faster than both globset
and fast-glob in both cases.

The Walker comparison uses the checked-in walker fixture — sixteen branches
nested four levels deep, filtered — and lower is better.

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
single-threaded collection. `stream()` is intentionally single-threaded and
unsorted so it can yield entries incrementally.

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
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

See [CONTRIBUTING](CONTRIBUTING.md) for repository policy — commit signing and
how performance claims are evidenced — and
[the contributor-oriented guide](docs/usage.md#development-and-validation) for
the corpus, fuzzing, and benchmark commands.

## License and attribution

Ferralk is MIT licensed. It is an independent project inspired by zlob 1.6.3;
provenance and attribution are recorded in [NOTICE](NOTICE) and the
[frozen reference](docs/zlob-1.6.3-reference.md).
