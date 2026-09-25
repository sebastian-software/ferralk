---
name: ferralk
description: Use when Rust code finds files or matches glob patterns with the ferralk or ferralk-glob crates - walking a directory tree with include and exclude globs or .gitignore rules, matching paths against configured globs, or porting such code from globset, glob, fast-glob, globby, ignore, or walkdir. Says which crate and entry point to use, gives tested recipes, and lists the defaults that compile but return the wrong result.
---

# Using ferralk and ferralk-glob

## Pick the crate

- Files on disk: `ferralk::Walker`, with root-relative `include` and `exclude`
  globs. Excludes prune: an excluded directory is never opened.
- A path or string you already hold: `ferralk_glob::Pattern`, or
  `ferralk_glob::PatternSet` for a list of globs (the `globset::GlobSet`
  counterpart). Depend on
  `ferralk-glob` alone if you never walk; `ferralk` re-exports it as
  `ferralk::ferralk_glob`.
- Both are synchronous. From async code, run the walk in
  `tokio::task::spawn_blocking` with a `CancellationToken`; the
  [async recipe](https://docs.rs/ferralk/latest/ferralk/#walk-from-async-code)
  shows the drop guard.

## Recipes

List files the ignore rules leave in, relative to the root:

```rust,no_run
use ferralk::{WalkOptions, Walker};

let root = std::path::Path::new("project");
let result = Walker::new(root)
    .include("**/*.rs")?
    .respect_git_ignore(true)
    .options(WalkOptions::default().files_only(true).sort(true))
    .collect()?;
for error in result.errors() {
    eprintln!("not walked: {error}"); // `collect()` was `Ok` anyway
}
for entry in result.entries() {
    println!("{}", entry.relative_path().display()); // `src/lib.rs`, as matched
}
# Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
```

Walk with a fast-glob or globby list, whose `!` entries are excludes:

```rust,no_run
use ferralk::{WalkOptions, Walker};

let globs = ["src/**/*.ts", "!src/**/*.test.ts", "!**/node_modules/**"];
let mut walker = Walker::new("project").options(WalkOptions::default().files_only(true));
for glob in globs {
    match glob.strip_prefix('!').filter(|rest| !rest.starts_with('(')) {
        Some(excluded) => walker.try_exclude(excluded)?,
        None => walker.try_include(glob)?,
    };
}
let result = walker.collect()?;
# let _ = result;
# Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
```

Match paths you hold against several globs with a `PatternSet`, not a loop:

```rust
use ferralk_glob::{PatternOptions, PatternSet, PatternSetError};

let globs = ["src/**/*.rs", "*.toml"];
let set = PatternSet::new(globs, PatternOptions::walker())?; // error.index() names the bad glob

assert!(set.is_match_glob_path("src/parser/lexer.rs"));
assert!(set.is_match_glob_path("Cargo.toml"));
assert!(!set.is_match_glob_path("crates/cli/Cargo.toml"));

let mut which = Vec::new(); // cleared and refilled, in ascending order
set.matches_glob_path_into("Cargo.toml", &mut which);
assert_eq!(which, [1]);
# Ok::<(), PatternSetError>(())
```

More, all tested: [ferralk recipes](https://docs.rs/ferralk/latest/ferralk/#recipes)
(stop early, keep going past errors, cancel, async) and
[ferralk-glob recipes](https://docs.rs/ferralk-glob/latest/ferralk_glob/#recipes)
(match a `Path`, filter lists, report an invalid pattern).

## Traps

These compile and return a plausible, wrong result:

- `PatternOptions::default()` reads `**`, braces and extglobs as plain text.
  Use `PatternOptions::walker()`, the dialect `Walker` uses.
- Use `is_match_glob_path` for paths. `is_match` lets `*` cross `/`, and
  `is_match_path` does so for a wildcard in the first component.
- `*` never crosses `/` in a walk: `*.rs` is top-level only; write `**/*.rs`.
  Only a `globset` port wants `WildcardMode::SeparatorCrossing`.
- Walker patterns are anchored at the root: `exclude("target/**")` prunes only
  the top-level `target`; `**/target/**` prunes every one.
- `**` is recursive only as a whole path component: `**/x` never matches `sx`.
- Matching is case-sensitive on every platform, even where the filesystem is
  not: `**/*.RS` misses `main.rs`. Use `Walker::case_insensitive(true)?`, or
  `PatternOptions::case_insensitive(true)` for a `Pattern`.
- Include wildcards, `**` included, skip a leading `.`: `**/*.ts` misses
  `.cache/x.ts`. Use `match_hidden(true)` or a literal `.cache/**`. Excludes
  cover hidden names either way, as `.gitignore` lines do:
  `exclude("**/node_modules/**")` also removes `.cache/node_modules`.
- `respect_git_ignore(true)` applies the ignore files in the walk root and
  below even outside a Git repository, unlike the `ignore` crate.
- A leading `!` is not negation: the walker rejects it and the matcher reads
  a literal `!`. Split lists as shown above; `!(…)` stays an extglob.
- `collect()?` is `Ok` even for a missing root; the failure is in
  `result.errors()`, and `for item in result` yields it as an `Err` after
  the entries. `ErrorPolicy::Skip` discards errors below the root. Branch on
  `error.io_kind()`, not on the message.
- `options()` replaces all `WalkOptions`; pass one value once.
- In `visit()`, `Verdict::Skip` drops an entry but still walks a directory's
  subtree; return `Verdict::Prune` to leave the directory unopened.
- `stream()` ignores `sort(true)`, and `take(n)` counts `Err` items.
  `stream()` is single-threaded whatever `threads(n)` says; use
  `stream_parallel()` to stream from `threads(n)` workers, in no particular
  order, with the same two traps.
- Entry paths include the root (`./src/lib.rs` for `Walker::new(".")`), and so
  does `path_bytes()`. Use `entry.relative_path()` for matching or printing
  relative paths.
- Directories are returned unless `files_only(true)`; the root never is.
- Patterns use `/` and `\` escapes on every platform; never build one with
  `PathBuf::join`. Match a `Path` as `ferralk_glob::path_bytes(path)`.
  Put a path or user text into a pattern with `ferralk_glob::escape_str`.

## Before you finish

- Check `result.errors()` or choose `ErrorPolicy::Abort`; do not drop a
  `WalkResult` unread.
- Run the walk against a small fixture tree and assert the exact relative
  paths, including one hidden file, one nested match, and one excluded
  directory at depth two or more.

## Reference

- [llms.txt](https://raw.githubusercontent.com/sebastian-software/ferralk/main/llms.txt):
  defaults table, entry points, and traps on one screen.
- [Usage guide](https://raw.githubusercontent.com/sebastian-software/ferralk/main/docs/usage.md):
  every default and switch.
- [Migration table](https://raw.githubusercontent.com/sebastian-software/ferralk/main/docs/compatibility-guide.md):
  from `globset`, `glob`, fast-glob, globby, `ignore`, and `walkdir`.
- API: [ferralk](https://docs.rs/ferralk), [ferralk-glob](https://docs.rs/ferralk-glob).
