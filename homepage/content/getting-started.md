---
title: Getting started
---

Ferralk is a Rust glob matcher and parallel file walker. Use `ferralk` to find
files on disk, or `ferralk-glob` on its own when you already hold the paths.

## Install

```sh
cargo add ferralk
```

The walker re-exports the matcher. Add only `ferralk-glob` if your application
does not need filesystem traversal.

## Find source files

Name the roots your query can reach. Ferralk can prune the other subtrees before
opening them, and applies Git ignores when you opt in.

```rust
use ferralk::{WalkOptions, Walker};

let result = Walker::new(".")
    .include("{src,packages}/**/*.{ts,tsx}")?
    .respect_git_ignore(true)
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

Recoverable errors are collected alongside the entries by default. A successful
`collect()` does not mean the result has no errors: inspect `result.errors()`,
or choose an explicit error policy for your application.

## Match the paths you hold

```sh
cargo add ferralk-glob
```

```rust
use ferralk_glob::{Pattern, PatternOptions};

let pattern = Pattern::compile("src/**/*.{rs,toml}", PatternOptions::walker())?;
assert!(pattern.is_match_glob_path("src/lib.rs"));
assert!(!pattern.is_match_glob_path("src/generated/lib.rs.bak"));
# Ok::<(), ferralk_glob::PatternError>(())
```

`PatternOptions::walker()` enables recursive `**`, braces and extglobs. The
matcher's default options leave them off. Use `is_match_glob_path` for
component-aware paths; `is_match` treats the input as a whole byte sequence.

## Choose policy deliberately

- Ordinary `*` stays inside one path component. Use `**/*.ts` for descendants.
- Include wildcards skip a leading period. `match_hidden(true)` changes that
  matching policy; `skip_hidden(true)` prevents hidden entries from reaching
  the matcher at all.
- Write `/` inside a pattern on every platform. Backslash is an escape.
- Git ignores, symlink following, metadata and sorting are opt-in. Returned
  paths keep their native representation.

The [usage guide](/guide/usage/) documents every default. Read the
[consumer pattern contract](/reference/patterns/) before accepting globs from
your own users, and the [migration guide](/guide/migration/) when moving from
`globset`, `ignore`, `walkdir`, `fast-glob` or zlob.

## API reference

- [Walker and re-exported matcher on docs.rs](https://docs.rs/ferralk)
- [Standalone matcher on docs.rs](https://docs.rs/ferralk-glob)
- [Runnable examples in the repository](https://github.com/sebastian-software/ferralk/tree/main/crates/ferralk/examples)

The stable portable backend is the default. Experimental native backends and
their feature names are outside the [1.x compatibility contract](/reference/stability/).
