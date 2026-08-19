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
do not rely on a 1.0 stability guarantee yet. The workspace is not currently
published on crates.io. Until the first public release is deliberately made,
use a pinned Git revision in applications that want to trial it.

```toml
[dependencies]
ferralk = { git = "https://github.com/sebastian-software/ferralk", rev = "0063ffbd0c7d7cd25d31135d2295bdf08f7cc4c2" }
ferralk-glob = { git = "https://github.com/sebastian-software/ferralk", rev = "0063ffbd0c7d7cd25d31135d2295bdf08f7cc4c2" }
```

Pin a revision for repeatable builds. The release PR and crates.io publishing
are intentionally separate from development builds.

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
- [External release gates](docs/external-release-gates.md) — work that needs
  remote evidence or maintainer action rather than a local code change.

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

See [the contributor-oriented guide](docs/usage.md#development-and-validation)
for the corpus, fuzzing, and benchmark commands.

## License and attribution

Ferralk is MIT licensed. It is an independent project inspired by zlob 1.6.3;
provenance and attribution are recorded in [NOTICE](NOTICE) and the
[frozen reference](docs/zlob-1.6.3-reference.md).
