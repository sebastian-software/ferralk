# Migrating from zlob 1.6.3 to Ferralk

Ferralk is a safe, byte-first Rust replacement for the matcher and filesystem
walking portions of zlob. It is not a source-compatible Rust facade and it
does not expose zlob's C ABI. This guide maps supported behaviour and makes
the intentional differences explicit. The authoritative feature-by-feature
status remains the [compatibility matrix](compatibility-matrix.md); upstream
coordinates and test provenance are in the
[frozen reference](zlob-1.6.3-reference.md).

## Matcher

Compile a pattern once, then reuse it:

```rust
use ferralk_glob::{Pattern, PatternOptions};

let pattern = Pattern::compile(
    "src/**/*.{rs,toml}",
    PatternOptions::default()
        .recursive_double_star(true)
        .braces(true),
)?;
assert!(pattern.is_match("src/lib.rs"));
# Ok::<(), ferralk_glob::PatternError>(())
```

| zlob flag / concept | Ferralk mapping |
| --- | --- |
| `ZLOB_BRACE` | `PatternOptions::braces(true)` |
| `ZLOB_EXTGLOB` | `PatternOptions::extglob(true)` |
| recursive `**` | `PatternOptions::recursive_double_star(true)` |
| `ZLOB_PERIOD` | `PatternOptions::match_hidden(true)` |
| `ZLOB_NOESCAPE` | `PatternOptions::escape(false)` |
| case-insensitive matching | `PatternOptions::case_insensitive(true)` |
| syntax validation | `Pattern::validate` |
| syntax preflight | `Pattern::has_wildcards` |
| `zlob_match_paths` / `_at` and index variants | `Pattern::{is_match_path,filter_paths,filter_paths_at,filter_path_indices,filter_path_indices_at}` (stable input order; component-local `*`, `?`, and classes after `/`; `**` is recursive) |

Ferralk accepts raw bytes (`AsRef<[u8]>`) for patterns and candidate paths, so
callers do not need lossy UTF-8 conversion.

## Walking

`Walker` replaces zlob's output-buffer-oriented traversal with owned entries,
structured errors, and an explicit root:

```rust
use ferralk::{ErrorPolicy, WalkOptions, Walker};

let result = Walker::new(".")
    .include("src/**/*.rs")?
    .exclude("**/target/**")?
    .respect_git_ignore(true)
    .threads(4)
    .error_policy(ErrorPolicy::Collect)
    .options(WalkOptions::default().sort(true))
    .collect()?;
# let _ = result;
# Ok::<(), Box<dyn std::error::Error>>(())
```

| zlob concept | Ferralk mapping |
| --- | --- |
| `ZLOB_GITIGNORE` | `Walker::respect_git_ignore(true)` |
| `ZLOB_WALK_KEEP_GIT_DIR` | `WalkOptions::keep_git_dir(true)` |
| `ZLOB_SKIP_HIDDEN` | `WalkOptions::skip_hidden(true)` |
| `ZLOB_FOLLOW_SYMLINKS` | `WalkOptions::follow_symlinks(true)` |
| `ZLOB_ERR` | `ErrorPolicy::{Abort, Skip, Collect}` |
| `ZLOB_ONLYDIR` | `WalkOptions::directories_only(true)` |
| `ZLOB_WALK_NO_REPORT_DIRS` | `WalkOptions::files_only(true)` |
| walker `max_depth` | `WalkOptions::max_depth(depth)` |
| walker entry depth | `WalkEntry::depth()` counts relative components below the root |
| walker entry basename | `WalkEntry::basename()` preserves the native `OsStr` name |
| walker entry kind | `WalkEntry::{kind,is_symlink}` exposes file, directory, or symlink identity |
| thread count | `Walker::threads(n)`; `collect()` defaults to available parallelism |
| metadata requests | `WalkOptions::metadata(true)` |
| streaming | `Walker::stream()` returns entry-or-error items incrementally |

`collect()` has no deterministic ordering unless `WalkOptions::sort(true)` is
selected. `stream()` is intentionally single-threaded and unsorted so it can
deliver entries incrementally.

Walker include patterns are root-relative. A leading `./` is accepted, and a
trailing `/` selects matching directories only.

## Deliberate differences

- Ferralk has no C ABI and no zlob-Rust migration facade. It exposes the two
  Rust crates `ferralk-glob` and `ferralk` instead (ADR-0003).
- Direct matching excludes leading-period path components by default. Enable
  `match_hidden` to opt in; this is the POSIX-conservative default selected by
  ADR-0011.
- `ZLOB_TILDE` and `ZLOB_TILDE_CHECK` are out of scope. Callers resolve home
  directories before constructing a `Walker` when that behaviour is wanted.
- `ZLOB_APPEND` and `ZLOB_DOOFFS` have no equivalent because Rust results are
  owned vectors, not caller-managed C buffers.
- `ZLOB_NOCHECK` and `ZLOB_NOMAGIC` are result-shaping policies, not matcher
  syntax. They remain deferred rather than being silently approximated.
- `ZLOB_MARK` is deliberately unsupported. Ferralk keeps native paths
  unmodified instead of appending display-only separators.
- `zlob_at` maps naturally to `Walker::new(root)`, but there is no separate
  descriptor-relative entry point yet.

## Defaults to review

Ferralk does not follow symlinks or apply `.gitignore` rules unless requested,
collects recoverable errors by default, avoids extra metadata syscalls by
default, and leaves ordering unsorted. Configure those choices explicitly when
migrating tool-like zlob usage.
