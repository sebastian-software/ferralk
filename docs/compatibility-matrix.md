# zlob 1.6.3 compatibility matrix

This is the living behavioural mapping from zlob's public surface to ferralk's
safe Rust API. “Planned” is not a compatibility claim. Each accepted behaviour
must have one or more corpus cases and every deliberate divergence must name
its rationale.

## Matcher

| zlob capability / flag | ferralk API | Status | Notes |
|---|---|---|---|
| `*`, `?`, separators | `Pattern::compile` / `Pattern::is_match` | Planned (M1) | POSIX-leading-period default. |
| `**` | `PatternOptions::recursive_double_star` | Planned (M1) | Explicit option. |
| bracket classes, ranges, `[!...]`, `[^...]` | `Pattern::compile` | Planned (M1) | Byte-first. |
| `ZLOB_BRACE` | `PatternOptions::braces` | Planned (M1) | Nested alternatives. |
| `ZLOB_EXTGLOB` | `PatternOptions::extglob` | Planned (M1) | Bash operators. |
| `ZLOB_PERIOD` | `PatternOptions::match_hidden` | Planned (M1) | Default remains off. |
| `ZLOB_NOESCAPE` | `PatternOptions::escape` | Planned (M1) | Higher-level boolean, not a bitflag. |
| case folding | `PatternOptions::case_insensitive` | Planned (M1) | Explicit opt-in on every platform. |
| `ZLOB_NOCHECK`, `ZLOB_NOMAGIC` | Walker no-match policy | Deferred (M4 review) | These are C/glob result-shaping semantics, not matcher semantics. |
| `ZLOB_TILDE`, `ZLOB_TILDE_CHECK` | — | Deliberate divergence | Out of scope per RFC non-goals. |

## Walker

| zlob capability / flag | ferralk API | Status | Notes |
|---|---|---|---|
| path traversal | `Walker::new` | Planned (M2) | Portable `std::fs` backend first. |
| `ZLOB_GITIGNORE` | `Walker::respect_git_ignore` | Planned (M3) | Git, not zlob, is normative. |
| `ZLOB_FOLLOW_SYMLINKS` | `WalkOptions::follow_symlinks` | Planned (M2) | Default off; cycle detection required. |
| `ZLOB_MARK`, `ZLOB_ONLYDIR` | entry filter / display policy | Planned (M2) | No path-string mutation in core API. |
| `ZLOB_ERR` | `ErrorPolicy::{Abort,Skip,Collect}` | Planned (M2) | `Collect` default. |
| `ZLOB_APPEND`, `ZLOB_DOOFFS` | — | Deliberate divergence | C output-buffer ownership has no Rust equivalent. |
| `zlob_at` | `Walker::new(path)` | Planned (M2) | Root is an explicit path. |
| metadata masks | `WalkOptions` metadata selection | Planned (M2) | Portable abstraction first. |

## Inventory provenance

The provisional inventory above comes from zlob's public README and Rust
examples. Completion requires freezing the exact 1.6.3 tag/commit, inspecting
`include/zlob.h` and the Rust crate API, and replacing “Planned” with
case-backed mappings. That work is blocked by the missing verifiable 1.6.3
release coordinate; see [`../MILESTONES.md`](../MILESTONES.md).
