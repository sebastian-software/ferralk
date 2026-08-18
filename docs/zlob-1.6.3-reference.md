# Frozen zlob 1.6.3 reference

This document fixes the external compatibility target used by the corpus and
mechanical-port work. It was verified from the upstream Git repository on
2026-08-18, not inferred from a moving branch or a documentation cache.

| Item | Value |
|---|---|
| Repository | `https://github.com/dmtrKovalenko/zlob.git` |
| Release tag | Annotated tag `v1.6.3` (`b757d57963cbf578aacfee4635c0305ded615417`) |
| Peeled source commit | `4bc4da2cbc823d3911b4a1436448687c398977dd` |
| Rust package | `zlob` `1.6.3`, edition 2024, MSRV 1.85 |
| License | MIT — Copyright (c) 2026 Dmitriy Kovalenko |
| Matcher sources | `src/fnmatch.zig`, `src/compiled_pattern.zig`, `src/path_matcher.zig`, `src/brace_optimizer.zig`, `src/suffix_match.zig` |
| Test sources | `test/test_fnmatch.zig`, `test/test_brace.zig`, `test/test_extglob.zig`, `test/test_path_matcher.zig`, `test/test_posix.zig`, `test/test_glibc.zig`, plus walker and gitignore suites |

The tag object and the source commit are both recorded because `v1.6.3` is an
annotated tag. Corpus records use the source name `zlob_1_6_3` and refer to the
peeled source commit above in their import metadata.

## Public Rust API inventory

| Area | zlob 1.6.3 public surface | ferralk direction |
|---|---|---|
| Filesystem glob | `zlob`, `zlob_at`, `Zlob`, `ZlobIter` | `Walker`; no C-shaped output buffer. |
| In-memory paths | `zlob_match_paths`, `_at`, `_indices`, `_indices_at`, `ZlobMatch`, `ZlobIndicies`, `AsZlobPaths` | `Pattern::is_match` first; batch APIs evaluated after corpus parity. |
| Compiled pattern | `ZlobPattern::{compile,matches,matches_default,match_paths,match_indices}` | `Pattern::{compile,is_match,validate}`; immutable, byte-first. |
| Syntax detection | `has_wildcards` | Planned helper after syntax parity. |
| Errors | `ZlobError` | Typed compile error now; typed walker errors in M2. |
| Walker | `walk::{WalkBuilder, WalkFlags, WalkMetadata, WalkState, WalkEntryKind, WalkEntry, WalkResults, IgnoreRules}` | Fresh `Walker`, `WalkOptions`, `ErrorPolicy`; no ABI mirroring. |

## C API inventory

The C ABI exposes `zlob`, `zlob_at`, `zlobfree`, in-memory `zlob_match_paths*`
and `zlob_match_paths_indices*`, `zlob_has_wildcards`, the compiled-pattern
allocation/match functions, and the walker functions `zlob_walk`,
`zlob_walk_collect`, `zlob_walk_max_workers`, result/ignore-rule frees, and
`zlob_ignore_rules_match_path`. Its public structs are `zlob_t`,
`zlob_slice_t`, `zlob_indices_t`, opaque `zlob_pattern_t`,
`zlob_walk_options_t`, `zlob_walk_entry_t`, and `zlob_walk_result_t`.

Ferralk deliberately ships no C ABI (ADR-0003), so ABI ownership, append,
offset, alternate-directory callbacks, raw result pointers, and free functions
are documented divergences rather than porting targets.

## Flag inventory and mapping

| zlob flag | ferralk status |
|---|---|
| `ERR`, `MARK`, `NOSORT`, `NOCHECK`, `NOESCAPE`, `PERIOD`, `BRACE`, `ONLYDIR`, `DOUBLESTAR_RECURSIVE`, `EXTGLOB`, `FOLLOW_SYMLINKS` | Mapped in the living [compatibility matrix](compatibility-matrix.md); implementation follows the milestone sequence. |
| `DOOFFS`, `APPEND`, `ALTDIRFUNC`, `MAGCHAR` | Deliberate C-ABI/output-buffer divergence. |
| `NOMAGIC` | Deferred result-shaping policy review. |
| `TILDE`, `TILDE_CHECK` | Deliberate divergence; out of RFC scope. |
| `GITIGNORE` | M3; Git itself, not zlob, is normative. |
| `ZLOB_WALK_*` (`GITIGNORE`, `SKIP_HIDDEN`, `FOLLOW_SYMLINKS`, `NO_REPORT_DIRS`, `SORT`, `ABORT_ON_ERROR`, `KEEP_GIT_DIR`) | M2/M3 fresh walker policies. |
| `ZLOB_META_*` (`SIZE`, `MTIME`, `ATIME`, `CTIME`, `BTIME`, `INODE`, `NLINK`, `MODE`, `UID`, `GID`) | M2 metadata selection, portable first. |

`ZLOB_RECOMMENDED` combines `BRACE`, `DOUBLESTAR_RECURSIVE`, `NOSORT`,
`TILDE`, and `TILDE_CHECK`. Ferralk has no equivalent bundle: defaults remain
POSIX-conservative and each semantic change is explicit (ADR-0011).
