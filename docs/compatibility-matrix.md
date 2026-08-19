# zlob 1.6.3 compatibility matrix

This is the living behavioural mapping from zlob's public surface to ferralk's
safe Rust API. “Planned” is not a compatibility claim. Each accepted behaviour
must have one or more corpus cases and every deliberate divergence must name
its rationale. For migration examples and the consolidated divergence list,
read the [compatibility guide](compatibility-guide.md).

## Matcher

| zlob capability / flag | ferralk API | Status | Notes |
|---|---|---|---|
| `*`, `?`, separators | `Pattern::compile` / `Pattern::is_match` | In progress (M1) | In-memory `*`/`?` are separator-agnostic; leading periods remain opt-in. |
| `**` | `PatternOptions::recursive_double_star` | Implemented (M1) | Explicit option; corpus import continues. |
| bracket classes, ranges, `[!...]`, `[^...]` | `Pattern::compile` | Implemented (M1) | Byte-first, including ASCII POSIX classes. |
| `ZLOB_BRACE` | `PatternOptions::braces` | Implemented (M1) | Nested and empty alternatives; source-backed corpus import continues. |
| `ZLOB_EXTGLOB` | `PatternOptions::extglob` | Implemented (M1) | `@()`, `?()`, `*()`, `+()`, `!()`; matches zlob's non-nested scope. |
| `ZLOB_PERIOD` | `PatternOptions::match_hidden` | Implemented (M1) | Deliberate in-memory divergence: Ferralk defaults to off; zlob's direct matcher accepts leading periods by default. |
| `ZLOB_NOESCAPE` | `PatternOptions::escape` | Implemented (M1) | Higher-level boolean, not a bitflag. |
| case folding | `PatternOptions::case_insensitive` | Implemented (M1) | Explicit opt-in on every platform. |
| `has_wildcards` | `Pattern::has_wildcards` | Implemented (M1) | Byte-first, flag-sensitive preflight matching zlob's active syntax markers. |
| `zlob_match_paths` / `_at` and index variants | `Pattern::{filter_paths,filter_paths_at,filter_path_indices,filter_path_indices_at}` | Implemented (M1) | Path APIs preserve caller order rather than zlob's default sort; index APIs return input positions. `_at` matches after stripping a component-boundary base path while returning original full paths or indices. Wildcard tokens after an explicit separator stay in that component, while `**` remains recursive. |
| `ZLOB_NOCHECK`, `ZLOB_NOMAGIC` | Walker no-match policy | Deferred (M4 review) | These are C/glob result-shaping semantics, not matcher semantics. |
| `ZLOB_TILDE`, `ZLOB_TILDE_CHECK` | — | Deliberate divergence | Out of scope per RFC non-goals. |

## Walker

| zlob capability / flag | ferralk API | Status | Notes |
|---|---|---|---|
| path traversal | `Walker::new` / `Walker::threads` | In progress (M3/M5) | Portable `std::fs` backend by default. macOS `native-macos` optionally uses audited `getattrlistbulk` batch name/type records, with a portable fallback for unsupported filesystems and a separately tested `getdirentries64` decoder. Linux `native-linux` optionally uses a bounds-checked `getdents64` reader on reviewed architectures, otherwise falling back portably. `collect()` uses a lazy work-stealing parallel scheduler and `stream()` remains incremental and single-threaded. |
| `ZLOB_GITIGNORE` | `Walker::respect_git_ignore` | Implemented (M3) | Nested `.gitignore` chains plus zlob-compatible `.ignore` supplements (loaded after `.gitignore`), negation-aware descent, and shared-parent caching use `ignore`'s matcher. |
| `ZLOB_WALK_KEEP_GIT_DIR` | `WalkOptions::keep_git_dir` | Implemented (M3) | `.git` is skipped with Gitignore by default; this explicit opt-in keeps it. |
| `ZLOB_SKIP_HIDDEN` | `WalkOptions::skip_hidden` | Implemented (M2) | Explicit opt-in; suppresses hidden files and whole hidden subtrees. |
| `ZLOB_FOLLOW_SYMLINKS` | `WalkOptions::follow_symlinks` | Implemented (M2) | Default off; canonical-path cycle guard when enabled. |
| `ZLOB_ONLYDIR` | `WalkOptions::directories_only` | Implemented (M2) | Filters returned files without pruning traversal. |
| `ZLOB_WALK_NO_REPORT_DIRS` | `WalkOptions::files_only` | Implemented (M2) | Filters returned directories without pruning traversal. |
| root-relative glob filter | `Pattern::is_match_glob_path` / `Walker::include` | Implemented (M2) | Ordinary wildcard and Extglob tokens stay within every component; `**` is the recursive form. |
| walker `max_depth` | `WalkOptions::max_depth` | Implemented (M2) | Returns entries through the depth boundary while pruning any deeper descent in serial, parallel, and streaming modes. |
| `ZLOB_MARK` | — | Deliberate divergence | Ferralk preserves native paths instead of appending display-only separators. |
| `ZLOB_ERR` | `ErrorPolicy::{Abort,Skip,Collect}` | Implemented (M2) | `Collect` default. |
| `ZLOB_APPEND`, `ZLOB_DOOFFS` | — | Deliberate divergence | C output-buffer ownership has no Rust equivalent. |
| `zlob_at` | `Walker::new(path)` | Implemented (M2) | Root is an explicit path; Rust results avoid zlob's C output-buffer ownership. |
| metadata masks | `WalkOptions::metadata` | Implemented (M2) | Opt-in `std::fs::Metadata` collection preserves the default no-extra-stat behaviour. |

## Inventory provenance

The zlob 1.6.3 tag, source commit, MIT attribution, Rust API, C API, and every
flag family were verified from upstream. See the complete
[frozen reference](zlob-1.6.3-reference.md). “Planned” still means the mapping
requires corpus-backed implementation; it no longer means that the upstream
contract is unknown.
