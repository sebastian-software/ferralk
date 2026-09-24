# Migrating from zlob 1.6.3 to Ferralk

Ferralk is a safe, byte-first Rust replacement for the matcher and filesystem
walking portions of zlob. It is not a source-compatible Rust facade and it
does not expose zlob's C ABI. This guide maps supported behavior and makes
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
| recursive `**` | `PatternOptions::recursive_double_star(true)`; only a whole-component `**` is recursive ([ADR-0020](adr/0020-double-star-only-as-a-whole-component.md)) |
| `ZLOB_PERIOD` | `PatternOptions::match_hidden(true)` |
| `ZLOB_NOESCAPE` | `PatternOptions::escape(false)` |
| case-insensitive matching | `PatternOptions::case_insensitive(true)` |
| syntax validation | `Pattern::validate` |
| syntax preflight | `Pattern::has_wildcards` |
| single filesystem-glob candidate | `Pattern::is_match_glob_path` (all ordinary wildcards are component-local) |
| `zlob_match_paths` / `_at` and index variants | `Pattern::{is_match_path,filter_paths,filter_paths_at,filter_path_indices,filter_path_indices_at}` (stable input order; a `*`, `?`, or class directly after `/` is component-local and a later wildcard in that component may cross again, as zlob does; `**` is recursive only with `recursive_double_star` and only as a whole path component, otherwise two ordinary stars) |

Ferralk accepts raw bytes (`AsRef<[u8]>`) for patterns and candidate paths, so
callers do not need lossy UTF-8 conversion.

Extglob groups use the same root and component rule as the selected entry
point; enabling extglob never changes which ordinary wildcard positions may
cross a separator. Under `is_match_path`, a group directly after `/` takes the
position of a wildcard there: `src/@(*.ts)` matches like `src/*.ts`, a
negated group such as `src/!(x)` stays within one component, and a later
wildcard inside an alternative crosses again as in `a/b*`. Only the first
iteration of a repeated group stands behind the separator, and an outer star
run of two or more reads as in plain syntax. Each brace alternative is judged
on its own, so a sibling never changes its verdict.
With recursive double stars enabled, `**/@(x)` matches at depth zero (`x`) as
well as below a directory (`a/x`).

With `recursive_double_star` enabled, `**` is recursive only as a whole path
component: bounded by an unescaped `/` or an end of the pattern on both sides,
as in `**`, `**/x`, `x/**`, and `x/**/y`. A recursive `**/` may match zero
directories but hands over only at a component start, so `**/x` rejects `sx`
and `a/**/b` rejects `a/xb`. Any other run (`a**`, `**b`, `a**/b`, `**.ts`) is
ordinary stars, exactly as with the option disabled. Brace alternatives are
judged after expansion, and an extglob alternative takes its group's position:
`@(**)/y` is recursive, `x@(**)/y` is not. A group reads like its
alternatives written in its place, so `@(**)/y` also matches `y`, and
`@(x)/**` and `x/@(**)` accept `x` as `x/**` does. This is the reading of gitignore,
Bash `globstar`, `globset`, and `fast-glob`; the zlob differences it implies
are listed under [deliberate differences](#deliberate-differences).

## Walking

`Walker` replaces zlob's output-buffer-oriented traversal with owned entries,
structured errors, and an explicit root:

```rust,no_run
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
| `ZLOB_GITIGNORE` | `Walker::respect_git_ignore(true)` (`.git/info/exclude`, then `.gitignore`, then zlob-compatible `.ignore`) |
| `ZLOB_WALK_KEEP_GIT_DIR` | `WalkOptions::keep_git_dir(true)` |
| `ZLOB_SKIP_HIDDEN` | `WalkOptions::skip_hidden(true)` |
| `ZLOB_PERIOD` on a walk | `Walker::match_hidden(true)` for include patterns; exclude patterns always cover a leading period |
| `ZLOB_FOLLOW_SYMLINKS` | `WalkOptions::follow_symlinks(true)` |
| `ZLOB_ERR` | `ErrorPolicy::{Abort, Skip, Collect}` |
| `ZLOB_ONLYDIR` | `WalkOptions::directories_only(true)` |
| `ZLOB_WALK_NO_REPORT_DIRS` | `WalkOptions::files_only(true)` (same symlink handling as zlob; see below) |
| walker `max_depth` | `WalkOptions::max_depth(depth)` |
| walker entry depth | `WalkEntry::depth()` counts relative components below the root |
| walker entry basename | `WalkEntry::basename()` preserves the native `OsStr` name |
| match a walker entry with `Pattern` | `entry.path_bytes()` passes its native encoded bytes to the byte-first matcher without allocation |
| walker entry kind | `WalkEntry::{kind,is_symlink}` exposes file, directory, or symlink identity |
| thread count | `Walker::threads(n)`; `collect()` defaults to available parallelism, held under the platform's measured metadata-concurrency ceiling, and clamps explicit budgets to `1..=256` |
| metadata requests | `WalkOptions::metadata(true)` |
| streaming | `Walker::stream()` returns entry-or-error items incrementally |

`collect()` has no deterministic ordering unless `WalkOptions::sort(true)` is
selected. `stream()` is intentionally single-threaded and unsorted so it can
deliver entries incrementally.

### Git filesystem adaptations

When Git ignore support is enabled, Ferralk reads `core.ignoreCase` and
`core.precomposeUnicode` from repository-local config. A linked worktree reads
the common `config` first and private `config.worktree` last when
`extensions.worktreeConfig=true`. This makes Git's normal per-repository
filesystem probe observable without a runtime Git subprocess: `ignoreCase`
uses ASCII-only rule/candidate folding, and macOS-only `precomposeUnicode`
converts valid UTF-8 candidate components to NFC before matching. Raw invalid
bytes stay raw. The supported local boolean parser follows Git for named forms,
bare keys, empty assignments, and signed base-zero integers with `K`/`M`/`G`
scaling; a malformed value is ignored and therefore cannot override an earlier
valid local value. It consumes only exact top-level `[core]` and `[extensions]`
sections (never a quoted subsection) and applies Git's backslash-newline value
continuations before comment, quote, escape, and boolean processing; indentation
on the continued physical line remains meaningful.

Git's system/global configuration, includes, and environment
overrides are deliberately not inherited by a library walk. If one of those
sources changes Git's effective value, pass it explicitly with
`Walker::git_ignore_case` or (on macOS) `Walker::git_precompose_unicode`; an
explicit Walker setting wins over repository-local config. Use
`clear_git_ignore_case` or `clear_git_precompose_unicode` to resume local
detection on a reused builder. Ferralk identifies a
case variant of `.gitignore`/`.ignore` in a listing only to attempt Git's
canonical open, so a case-sensitive filesystem continues to expose that
variant as an ordinary file.

`Walker::match_hidden` and `WalkOptions::skip_hidden` are separate mechanisms
and not each other's inverse: `match_hidden` is matcher semantics, deciding
whether a wildcard in an include may cover a leading period, while
`skip_hidden` is a traversal filter that removes hidden entries before any
pattern is consulted. A wildcard in an exclude covers a leading period under
either `match_hidden` setting, so excludes apply inside hidden directories the
way `.gitignore` lines do.

Walker include patterns are root-relative. A leading `./` is accepted, on the
pattern and on each brace alternative (`{./src,lib}/*.rs`), and is ignored
once per alternative exactly as `Pattern::is_match_glob_path` ignores it. A
trailing `/` selects matching directories only. Ordinary wildcards stay inside
one path component by default; use recursive `**` to select descendants, or
switch the whole walk to crossing wildcards as described below. A pattern that
starts at a filesystem root is understood as absolute and rewritten, as
described next.

### Patterns are written with `/`, on every platform

**A pattern is not a path.** In a pattern `\` is the escape character —
on Windows too — so a pattern built by joining `PathBuf`s carries separators
the matcher reads as "the next byte, literally":

```rust,no_run
# use ferralk::Walker;
# let root = std::path::PathBuf::from(".");
// WRONG on Windows. `PathBuf::join` produces `C:\repo\src\**\*.ts`, where
// every `\` escapes the byte after it: the pattern asks for a file whose
// name contains a literal `*`, which Windows cannot even create.
let pattern = root.join("src").join("**").join("*.ts");
let walker = Walker::new(&root).include(pattern.to_string_lossy().as_ref())?;

// RIGHT. Build the pattern as a pattern, and let the walker hold the path.
let walker = Walker::new(&root).include("src/**/*.ts")?;
# Ok::<(), ferralk::ferralk_glob::PatternError>(())
```

This is the ADR-0005 line seen from the pattern side: candidate *paths* accept
both separators on Windows, patterns are written with `/`. The rule is not
Windows-specific — `\` escapes on Linux and macOS as well — it is only on
Windows that the platform hands you backslashes without being asked.

**The walker refuses the shapes that cannot work.** Since 0.5.2,
`Walker::include` and `Walker::exclude` reject a pattern that would demand a
literal byte Windows forbids in a name, which is what a joined path asks for:

| Pattern | On Windows | Why |
| --- | --- | --- |
| `C:\repo\src\**\*.ts` | rejected | drive prefix spelled with `\` |
| `C:\repo\node_modules` | rejected | same, no wildcard needed |
| `src\*.ts` | rejected | asks for a literal `*` in a name |
| `\\server\share\x` | rejected | asks for a literal `\` |
| `C:/repo/src/**/*.ts` | works | the spelling this dialect uses |
| `a\b\c` | **accepted** | escaping an ordinary byte is legal; selects `abc` |
| `[a\*]`, `{a,\*}` | **accepted** | inside a group the escape is one member; both still match `a` |

The accepted rows are the limit of the check, and it is deliberate: refusing a
pattern that works would be worse than the silence this replaces. Escaping an
ordinary byte is legal syntax meaning the byte itself, so `a\b\c` really does
select a file named `abc`. And inside a character class or an alternation an
escaped byte is one member among several — `[a\*]` and `{a,\*}` both still
select `a` — so the check reads only the plain text before the first `[`, `{`
or extglob opener.

One consequence is worth naming rather than hiding: a one-alternative group like
`{a\*b}` could never match on Windows and is still accepted, because noticing it
would mean parsing the group here and agreeing with the real parser about every
nesting case. Unnoticed but correct beats noticed but lossy.

What is refused is only what could never have matched, so nothing that used to
select entries stops doing so. On Linux and macOS nothing is refused at all —
there a file may genuinely be named `src*.ts`.

**Converting a path you already hold.** Replace the separators, and remember
that a path is not automatically a valid pattern: if any component contains
`*`, `?`, `[` or `{`, those bytes are syntax and need escaping with `\`.

```rust,no_run
# use ferralk::Walker;
# let root = std::path::PathBuf::from(".");
let as_pattern = root.to_string_lossy().replace('\\', "/");
let walker = Walker::new(&root).include(format!("{as_pattern}/src/**/*.ts"))?;
# Ok::<(), ferralk::ferralk_glob::PatternError>(())
```

Usually there is no need: `Walker::new(root)` already holds the path, and
`include("src/**/*.ts")` is root-relative. Absolute patterns exist for callers
that hold one, not as a way to spell a relative one.

### Several roots in one walk

`Walker::add_root` and `Walker::add_roots` extend a walk to more than one tree.
For a fallible caller-supplied root list, `Walker::try_add_root` borrows the
builder, so one rejected root does not lose the configured walk.
A caller with several source directories used to build one walker per directory,
and with it one thread pool per directory; the roots are now the walk's initial
directories and share the scheduler and helper-spawn floor. The
ancestor-chain guard starts empty per root traversal: each descendant task
extends only its own path of ancestors, while separately supplied roots —
including duplicates, overlaps and symlink aliases — start independent chains.
Following symlinks therefore stops genuine cycles without deduplicating
acyclic aliases, either within one root or across roots.

| Question | Answer |
| --- | --- |
| Which patterns apply? | Every pattern applies under every root, root-relative as always. An absolute pattern is rewritten per root, so a pattern naming one root's tree selects nothing under the others. |
| What is `depth`? | Components between the entry and **its own** root, exactly as in a single-root walk. |
| Which root did an entry come from? | `WalkEntry::root`. A single-root walk answers with that one root, so the accessor reads the same either way. |
| What if roots overlap? | Their overlap is delivered once per root. |
| What if a root cannot be read? | An ordinary walk error for that root's path; the other roots are still walked, subject to the error policy. |
| In what order? | Unspecified, exactly as within a single root: the roots become scheduler tasks like any other. `WalkOptions::sort(true)` is what orders a result. |

The overlap rule is the one worth stating twice, because it is a choice rather
than an accident. A multi-root walk is defined as the concatenation of the
single-root walks — that is what makes it substitutable for the loop it
replaces, and what the invariant tests check on every frontend. Suppressing the
second copy of a shared subtree would need the identity of every directory,
which costs a `stat` per directory that only `follow_symlinks(true)` pays today,
and it would make adding a root able to *remove* entries. A caller who wants
each path once passes roots that do not contain one another.

Because patterns are read per root, an absolute pattern list can be handed to a
multi-root walk unsorted: each pattern selects under the root it names and
falls away under the rest, which is why an out-of-root pattern is a verdict
rather than an error.

### Absolute patterns, and the caller-side rewrite they replace

A caller that knows where a project lives holds `/repo/src/**/*.ts` rather than
`src/**/*.ts`, and until 0.4 had to strip the walk root itself before handing
the pattern over. That arithmetic is short to write and easy to get subtly
wrong — `/repo` against a root of `/repo` is not the same case as against
`/repository`, and a root that ends in a separator leaves a doubled one at the
join — so the walker now does it.

`Walker::include` and `Walker::exclude` detect an absolute pattern and remove
the walk root from it. Detection follows the platform: a leading `/` on Unix; a
drive letter or a UNC share on Windows, where a single leading separator is
drive-relative and so stays an ordinary walker pattern. Patterns are written
with `/` on every platform per ADR-0005, `\` being an escape rather than a
separator.

| Pattern | Walk root | Result |
| --- | --- | --- |
| `/repo/src/**/*.ts` | `/repo` | `src/**/*.ts` |
| `/repo/{src,lib}/**` | `/repo` | `{src,lib}/**`, brace roots intact |
| `/repo//src/*.ts` | `/repo/` | `src/*.ts`, separator noise ignored |
| `/repo/*/x.ts` | `/repo` | `*/x.ts`, a wildcard below the root is fine |
| `/other/**` | `/repo` | selects nothing, and prunes nothing |
| `/repo` | `/repo` | rejected: names the root; add `/**` |
| `/**/*.ts` | `/repo` | rejected: wildcard at or above the root |
| `/repo/../repo/x.ts` | `/repo` | rejected: `..` is not resolved |
| `/b/**` or `/other/**` | `/a/../b` | rejected: the root has a `..` |

The three rejections are the shapes where guessing would silently select the
wrong entries. A wildcard standing where the root's own components are may or
may not cover the root, and deciding that needs matching rather than
arithmetic; write the part below the root instead, where `**/*.ts` says what
`/**/*.ts` was reaching for. A `..` is not folded away because folding it
lexically is wrong across a symlink, and resolving it properly would mean
touching the filesystem to compile a pattern. The same reason rejects a walk
root with a `..` component for every absolute pattern, whichever tree the
pattern names: such a root cannot be related to any absolute path without
resolving it. Relative patterns need no such relation and work under it as
usual. Naming the root itself selects nothing, because the walk emits what is
inside the root.

The same candidate guard applies to relative patterns. Brace alternatives are
expanded, so `{.,..}` is rejected as an attempt to name the unwalkable dot and
dot-dot components. Extglob-only components such as `@(.)`, `@(..)/x`, and
`?(.)/x` remain deliberately opaque matcher text instead; they select no walk
candidate rather than becoming path operations.

A pattern about a different tree is not an error, because a caller may hold one
pattern list and run it against several roots. It selects nothing and — the
part that matters for a walk — prunes nothing: an exclude that cannot reach
this tree never closes a directory in it.

**What this was before.** A pattern starting with `/` used to reach the matcher
unchanged and match no candidate at all, because walk candidates are
root-relative and never start with a separator. An absolute include therefore
produced an empty walk and an absolute exclude did nothing. Nothing that
previously selected entries selects different ones now; the patterns that
change behavior are the ones that selected nothing, which now either work or
say why they cannot.

### Coming from globset, glob, fast-glob, ignore, or walkdir

The defaults below are the ones that make a port compile and then return a
different result. Each Ferralk cell names the switch or the idiom that
restores the old behavior; the section after the table covers pattern lists
in detail, and the crate documentation has tested recipes for
[walking](https://docs.rs/ferralk/latest/ferralk/#recipes) and for
[matching](https://docs.rs/ferralk-glob/latest/ferralk_glob/#recipes).

| Concern | `globset`, `glob` | fast-glob, globby | `ignore`, `walkdir` | Ferralk |
| --- | --- | --- | --- | --- |
| Does `*` cross `/`? | `globset`: yes, unless `literal_separator(true)`. `glob::Pattern::matches`: yes, unless `require_literal_separator`; `glob::glob()` matches per component. | No. | `ignore` overrides and `.gitignore`: no. `walkdir` takes no patterns. | No, in `Walker` and `Pattern::is_match_glob_path`. `Pattern::is_match` crosses. `Walker::wildcard_mode(WildcardMode::SeparatorCrossing)` gives a walk the `globset` reading. |
| A slash-free pattern such as `*.rs` or `target` | `globset`: matches at any depth, because `*` crosses. | Top level only. | `.gitignore` and overrides: at any depth. | Top level only: walker patterns are anchored at the root. Write `**/*.rs` or `**/target/**` for any depth. |
| `**` | Recursive only as a whole component. Elsewhere `globset` reads two `*`, and `glob` rejects it. | Recursive only as a whole component. | `.gitignore` rules. | Recursive only as a whole path component (`**`, `**/x`, `x/**`, `x/**/y`): `**/x` matches `x` and `a/x`, never `sx`. Any other star run is ordinary stars. `Walker` always reads it this way; `ferralk-glob` only with `recursive_double_star(true)`, which `PatternOptions::walker()` sets. `PatternOptions::default()` reads `**` as `*`. |
| Case | `globset`: sensitive unless `case_insensitive(true)`. `glob`: `MatchOptions::case_sensitive`. | Sensitive unless `caseSensitiveMatch: false`. | `ignore` overrides: `case_insensitive(true)`; `.gitignore`: `core.ignoreCase`. | Sensitive on every platform. `Walker::case_insensitive(true)` or `PatternOptions::case_insensitive(true)` folds ASCII case; ignore files follow `git_ignore_case` and `core.ignoreCase`. |
| Braces `{a,b}` | `globset`: on. `glob`: not supported. | On. | – | On in `Walker`. In `ferralk-glob` only with `braces(true)` or `PatternOptions::walker()`; `default()` reads `{` literally. |
| A leading `.` | `globset` and `glob`: `*` matches it, unless `glob`'s `require_literal_leading_dot`. | Not matched unless `dot: true`. | `ignore` skips hidden entries entirely unless `hidden(false)`. `walkdir` yields them. | An include wildcard does not cover it, so `**/*.ts` skips `.cache/x.ts`; opt in with `Walker::match_hidden(true)` or `PatternOptions::match_hidden(true)`. An exclude covers it either way, as a `.gitignore` line does, so `**/node_modules/**` also removes `.cache/node_modules`. Hidden entries are still walked and returned when no pattern leaves them out; `WalkOptions::skip_hidden(true)` drops them as `ignore` does. |
| A leading `!` | Not negation (`[!a]` is a negated class). | Marks an ignore pattern. | Overrides: marks an exclude. `.gitignore`: re-includes. | Not negation. `Walker::include` and `exclude` reject it, and `ferralk-glob` reads it as a literal `!`, so split the list into includes and excludes (below). `!(…)` is a negated extglob. `.gitignore` files keep Git's `!` under `respect_git_ignore(true)`. |
| Several patterns at once | `globset::GlobSet`, with `matches` for the indices. | An array of patterns. | `OverrideBuilder`. | `Walker`: one `include` or `exclude` call per pattern; includes are OR-ed. `ferralk-glob`: a `Vec<Pattern>` asked with `iter().any` or `position`. There is no set type yet ([#405](https://github.com/sebastian-software/ferralk/issues/405)). |
| Pruning a subtree from code | – | – | `filter_entry` (`walkdir`, `ignore`), `WalkState::Skip` (`ignore` parallel). | `Verdict::Prune` from `Walker::visit`, for a directory the walk would return. `Verdict::Skip` drops only the entry. |
| Which entries are returned | `glob::glob()`: matching files and directories. | Files only (`onlyFiles: true`). | Every entry, the root itself first at depth 0. | Files, directories, and symlinks that the patterns select, never the root itself. `WalkOptions::files_only(true)` matches fast-glob's default, `directories_only(true)` its `onlyDirectories`. |
| Shape of a returned path | – | Relative to `cwd`: `src/lib.rs`. | The root joined with the relative path. | The root joined with the relative path: `Walker::new(".")` yields `./src/lib.rs`, `Walker::new("src")` yields `src/lib.rs`. `entry.relative_path()` is the relative part. |
| `.gitignore` | Not read. | Not read (globby: `gitignore: true`). | `ignore`: read by default inside a Git repository, with the global excludes file. `walkdir`: not read. | Not read until `Walker::respect_git_ignore(true)`, which reads `.gitignore` and `.ignore` in the walk root and below it even outside a repository, and inside one also `.git/info/exclude` and the files from the repository root down. Git's global excludes file is not read. |
| Order | `glob::glob()`: sorted. | Unordered. | Unordered unless a `sort_by` option is set. | Unordered. `WalkOptions::sort(true)` sorts `collect()` and `visit()`; `stream()` ignores it. |
| Errors | `glob::glob()`: `GlobError` items. | Rejects, except for `ENOENT`, unless `suppressErrors: true`. | `Result` items. | `collect()` returns `Ok` and lists every recoverable error in `WalkResult::errors()`, a missing root included; `stream()` yields them as `Err` items, and so does iterating a `WalkResult`, after its entries. `WalkError::io_kind()` is the `io::ErrorKind`. `ErrorPolicy::Abort` stops at the first one. |
| Matching a `Path` | `is_match` takes `AsRef<Path>`. | Strings. | – | Patterns take bytes: `ferralk_glob::path_bytes(path)`, or `path_bytes(entry.relative_path())` for a walked entry. A `&str` works as it is. Match the path relative to where the pattern is anchored. |

### Migrating patterns from globset or fast-glob

`globset` reads an unconfigured `*` as crossing separators, so `*.ts` selects
`src/deep/main.ts` there. fast-glob and globby keep `*` inside one component,
as Ferralk does, so their patterns need no mode change. Ferralk's walker reads
patterns as filesystem globs by default, where `*.ts` selects only what sits in
the walk root. Carrying a `globset` pattern over unchanged therefore used to
select strictly less, without saying so.

`Walker::wildcard_mode` makes the choice explicit:

```rust
use ferralk::{WildcardMode, Walker};

// Patterns written for globset keep their meaning.
let walker = Walker::new(".")
    .wildcard_mode(WildcardMode::SeparatorCrossing)
    .include("*.ts")?;
# Ok::<(), ferralk::ferralk_glob::PatternError>(())
```

What the two modes select, for the same pattern:

| Pattern | Candidate | `ComponentScoped` (default) | `SeparatorCrossing` |
| --- | --- | --- | --- |
| `*.ts` | `main.ts` | selected | selected |
| `*.ts` | `src/main.ts` | not selected | selected |
| `*.ts` | `src/deep/main.ts` | not selected | selected |
| `src/*.ts` | `src/main.ts` | selected | selected |
| `src/*.ts` | `src/deep/main.ts` | not selected | selected |
| `src/*.ts` | `other/main.ts` | not selected | not selected |
| `**/*.ts` | `src/deep/main.ts` | selected | selected |

Two things carry over unchanged. A whole-component `**` is recursive under
either mode, so a pattern already written with `**/` means the same thing in
both, and in both `**/x` selects `x` and `a/x` but never `sx`. And a literal
prefix is still a literal prefix: `src/*.ts` never reaches outside `src/`, which
is why the walker can still skip sibling directories without opening them.

The mode governs excludes as well as includes, so a walk reads every pattern the
same way. It is a matching policy, independent of `match_hidden` and of
`WalkOptions::skip_hidden`.

A pattern list from fast-glob or globby also carries negations: a leading `!`
turns a pattern into an ignore. The walker has two lists instead of one, so the
translation sorts the entries rather than rewriting them:

```rust
use ferralk::Walker;

// fast-glob: ["src/**/*.ts", "!src/**/*.test.ts", "!**/generated/**"]
let walker = Walker::new(".")
    .include("src/**/*.ts")?
    .exclude("src/**/*.test.ts")?
    .exclude("**/generated/**")?;
# Ok::<(), ferralk::ferralk_glob::PatternError>(())
```

`include` and `exclude` reject a pattern that starts with `!` rather than read
it as a literal first byte, which used to select nothing without an error.
fast-glob expands braces before it sorts its list, so a brace alternative that
starts with `!` (`{!a,b}/**`) is rejected the same way, at the offset of that
`!`. The rule reads the pattern as the caller wrote it: `!(…)` is the negated
extglob, as it is in fast-glob, `\!` asks for a literal `!`, an interior `!`
is an ordinary byte, and an absolute pattern's `!` below the walk root
(`/repo/!src/**` for a root of `/repo`) names a literal component. A
`!` that re-admits entries into an ignore list, the way `.gitignore` uses it,
has no counterpart among walker excludes, because an entry any exclude matches
is not emitted: narrow the exclude instead, or keep such rules in a
`.gitignore` and use `respect_git_ignore`.

## Windows verification

Windows is a tier-2 target for the portable backend. CI replays the complete
Git-ignore corpus with Git for Windows and compares the walker with `git
ls-files --others --exclude-standard` for 21 root spellings from three working
directories. Those spellings include `.`, `./`, trailing and repeated
separators, nested roots, and lexical `.`/`..` components. The same
differential runs on Linux and macOS; symlink spellings remain Unix-only.

The currently unverified Windows filesystem shapes are junction-based
repository discovery, drive-relative paths such as `C:src`, and repositories
whose result depends on a case-insensitive filesystem rather than Git's
explicit `core.ignoreCase` setting. Pattern matching itself remains byte-first
and covered by the cross-platform corpus. See the
[stability contract](stability.md#windows-evidence-and-limits).

## Deliberate differences

- Ferralk has no C ABI and no zlob-Rust migration facade. It exposes the two
  Rust crates `ferralk-glob` and `ferralk` instead (ADR-0003).
- Direct matching excludes leading-period path components by default. Enable
  `PatternOptions::match_hidden` for a compiled pattern, or
  `Walker::match_hidden` for a walk's includes, to opt in; this is the
  POSIX-conservative default selected by ADR-0011. A walker exclude always
  covers a leading period, as a `.gitignore` line does. It holds inside one
  component too: `*.rs` matches `.rs` only with `match_hidden` enabled. A
  negated extglob such as `!(x)` is an ordinary wildcard for this rule: it
  crosses a separator like `*` in the fnmatch reading, but neither consumes a
  component-leading period nor stops right before one (issue #394). zlob
  1.6.3's list matcher selects `a/.env` for `!(x)`; the corpus records that
  verdict with ADR-0011 provenance in `dotfile-*negated-extglob-*`.
- Under `PatternOptions::case_insensitive`, `[[:upper:]]` and `[[:lower:]]`
  fold symmetrically, so each matches every ASCII letter. Bash's `nocasematch`
  tests a POSIX class against the unfolded byte, so `[[:upper:]]` still
  rejects `a` there. Ranges fold both bounds before comparing, as Bash does,
  so `[A-z]` and `[Z-a]` read the same way in both.
- `ZLOB_TILDE` and `ZLOB_TILDE_CHECK` are out of scope. Callers resolve home
  directories before constructing a `Walker` when that behavior is wanted.
- `ZLOB_APPEND` and `ZLOB_DOOFFS` have no equivalent because Rust results are
  owned vectors, not caller-managed C buffers.
- `ZLOB_NOCHECK` and `ZLOB_NOMAGIC` are result-shaping policies, not matcher
  syntax. They remain deferred rather than being silently approximated.
- `ZLOB_MARK` is deliberately unsupported. Ferralk keeps native paths
  unmodified instead of appending display-only separators.
- `zlob_at` maps naturally to `Walker::new(root)`, but there is no separate
  descriptor-relative entry point yet.
- `WalkOptions::resolve_symlink_kind(true)` is an extension zlob has no
  equivalent for, and it is off by default so the default agrees with zlob.
  Both engines filter on the kind a directory listing reports, and a listing
  reports a symlink as a symlink without saying what it points at. Measured
  against zlob 1.6.3 with the oracle in
  `tools/oracle/tests/zlob_walk_symlinks.rs`, `ZLOB_WALK_NO_REPORT_DIRS`
  returns all three symlink shapes — link to a file, link to a directory, and
  broken link — and suppresses only real directories. `files_only(true)` does
  the same. Callers who mean `Path::is_file`, which follows the link, opt into
  `resolve_symlink_kind(true)`: it classifies symlink entries by their target,
  so `files_only` keeps only links to files and `directories_only` starts
  keeping links to directories, at one `stat` per symlink entry. A broken link
  is then neither kind and is dropped without an error; a `stat` failing for
  any other reason is reported through the `ErrorPolicy`. This differs from
  `follow_symlinks(true)`, where a broken link is a *traversal* failure and is
  reported: following was asked to walk through the link, while resolving only
  asked what it is.
- Backslash escapes inside character classes follow bash and glibc/BSD
  `fnmatch`: an escaped `-` is a literal member and never a range operator
  (`[a\-z]` is exactly `{a, -, z}`). zlob 1.6.3 performs no escape processing
  inside classes and reads the backslash as an ordinary range endpoint. The
  deliberate divergence is adopted by ADR-0015 and recorded with the
  `adr: "0015"` marker beside the external verdict
  (`class-006/008/009/012/016/024/025/047/048`).
- A star run immediately before a `*(` extglob follows bash and ksh grammar:
  the final star opens the zero-or-more group, while any earlier stars remain
  an ordinary wildcard. Thus `**(a)` is `*` followed by `*(a)`, and matches
  `x`; zlob 1.6.3 greedily collapses both stars before checking for a group and
  reads the suffix as literal `(a)`. The shell-compatible reading is recorded
  with ADR-0016 provenance beside the zlob verdicts in
  `extsuite-*star-run-before-zero-or-more` (issue #305).
- `**` is recursive only as a whole path component, the reading of gitignore,
  Bash `globstar`, `globset`, and `fast-glob` (maintainer decision of
  2026-09-24, issue #419, [ADR-0020](adr/0020-double-star-only-as-a-whole-component.md)).
  zlob 1.6.3 has no single reading to follow. Its `matchPaths` already refuses
  `**/x` against `sx`, but once a pattern holds a whole `**` it matches every
  other component on its own, so an attached run stays in one component there:
  `a**/y` rejects `a/x/y` and `**.ts` rejects `src/a.ts`, where Ferralk's
  `is_match` reads the run as ordinary separator-crossing stars. Its segment
  split also ignores extglob groups, so `@(**)/y` and `@(**/x|z)` are not
  recursive there, `@(**)/y` rejects `y`, and `x/@(**)` rejects `x`. A trailing `**/` accepts `a/b` against `a/**/` there, while
  Ferralk demands the final separator. zlob's filesystem glob treats any `**`
  substring as recursive and can drop the text beside it. The corpus records
  these verdicts in `globstar-*` with `adr: "0020"`.
- Ferralk list APIs preserve caller order, normalize one leading `./` on the
  pattern and candidates, and never synthesize a `NOCHECK` result that the
  caller did not supply. ADR-0017 records these Rust API conventions.
- Four zlob verdicts contradict its own frozen tests: the public matcher's two
  escape results and the list matcher's two leading repeating-extglob results.
  Eight fast-glob differences reflect defects or documented limits in that
  secondary oracle. These records carry `oracle_defect: true`; the exact IDs
  and rationale are listed in the
  [corpus format](corpus-format.md#evidence-and-disputes), so none represents
  unsettled Ferralk policy.
- Brace expansion is budgeted. A pattern that would expand to more than 4096
  alternatives is rejected with `too many brace alternatives` at the offset of
  the brace group that starts the expansion. Brace groups multiply, so ten
  nine-way groups fit in 100 bytes and ask for 3.5 billion alternatives; neither
  reference bounds this. Measured on the pattern from issue #42: zlob 1.6.3
  needs 18 s already at eight groups (80 bytes) and extrapolates to about 25
  minutes at ten, and glibc `GLOB_BRACE` takes 64 s at ten. Both abort on
  `{a}` repeated 50,000 times, where the expansion is a single alternative but
  the recursion is 50,000 deep. Ferralk rejects the first shape and expands the
  second iteratively. The boundary is recorded as `compile_error` corpus cases
  (`error-brace-budget-*`, issue #42), which the zlob adapter skips because the
  oracle has no error to compare against.
- Brace expansion is budgeted a second way, in bytes. Expansion rewrites the
  whole pattern once per group it resolves, so the alternative count alone does
  not bound the work: 200,000 one-way groups are 600 KB, expand to a single
  alternative, and took 11.8 s, while 4096 alternatives of a 100 KB pattern is
  400 MB and a second however few groups produced them. A pattern whose
  expansion would write more than 64 MiB is rejected with
  `brace expansion is too large` at the same offset. Neither reference bounds
  this either. The boundary is recorded as `error-brace-work-*` corpus cases
  (issue #54).
- Compilation is budgeted a third way, in compiled units. Neither text budget
  sees what compiling that text costs, and the compiled form is far larger than
  its source: a token per wildcard byte, and for an extglob a program step per
  byte offset of every alternative. A 5 KB pattern that sat inside both other
  budgets compiled to 1.9 GB. A pattern whose compiled program would pass
  1,048,576 units — roughly 40 MB — is rejected with
  `pattern compiles to too much`. The dimension is reachable with and without
  extglob syntax, and the boundary is recorded as `error-compiled-ir-*` corpus
  cases (issue #74).

## Contract-change audit since 0.9.0

This table maps every consumer-visible `!` change from 0.9.0 through the final
0.x contract work to the documentation that states the resulting behavior.
It is an audit of the current contract, not a second changelog.

| Release / change | Resulting documented behavior |
| --- | --- |
| 0.9.0: matcher and walker resource limits | Compile budgets are described under [deliberate differences](#deliberate-differences); ignore-file and thread limits are in the [usage guide](usage.md#walk-filesystems-with-explicit-policy). |
| 0.9.3: traverse every acyclic symlink alias | Each supplied or discovered acyclic alias remains independently traversable; see [several roots](#several-roots-in-one-walk) and the usage guide's symlink policy. |
| 0.9.3: includes below excluded directories | An exclude prunes only when no include can re-admit a descendant; see [walker defaults](usage.md#walk-filesystems-with-explicit-policy). |
| 0.10.0: Git slash wildmatch rules | Escaped separators, attached star runs, and bracket ranges follow the normative Git oracle; see [Git filesystem adaptations](#git-filesystem-adaptations). |
| 0.10.0: Linux native `PATH_MAX` boundary | Native and portable paths share the portable length boundary; native implementation details remain [experimental](usage.md#platform-support). |
| 0.10.0: ignore inheritance for relative roots | `.`, `./`, and relative subtree roots discover and inherit repository rules; see [Git filesystem adaptations](#git-filesystem-adaptations). |
| 0.11.0: extglob star-run grammar | The final star before `*(` opens the group, per [ADR-0016](adr/0016-shell-star-runs-before-extglobs.md) and [deliberate differences](#deliberate-differences). |
| 0.11.0: path entry-point alignment | Path entry points normalize one leading `./`; disabled recursive `**` is an ordinary star run. The [matcher table](#matcher) and [usage guide](usage.md#match-paths-deliberately) define the position rule. |
| 0.11.0: strict leading-period stars | Without `match_hidden`, an ordinary star cannot stop before a component-leading period; see [deliberate differences](#deliberate-differences). |
| 0.11.0: excluded followed-link failures | A terminal excluded followed link keeps the path-exclusion shortcut rather than producing metadata work; see the usage guide's symlink policy. |
| 0.11.0: normalized ignore roots | Trailing separators and parent-relative spellings resolve to the same repository while caller-visible entry spelling is preserved; see [Git filesystem adaptations](#git-filesystem-adaptations). |
| 0.11.0: caller-source error offsets | `PatternError::offset()` identifies original pattern bytes after brace expansion or absolute-root rewriting; see [absolute patterns](#absolute-patterns-and-the-caller-side-rewrite-they-replace). |
| 0.11.0: case-insensitive POSIX upper class | `upper` and `lower` fold symmetrically under `case_insensitive`; see [deliberate differences](#deliberate-differences). |
| 0.11.0: Git 2.52 ignore behavior | Attached star runs, escaped separators, and reversed ranges follow the pinned Git oracle; see [Git filesystem adaptations](#git-filesystem-adaptations). |
| Final 0.x: resumed native listing failures | A native serial listing that cannot be reopened or changed identity reports typed `ReadDir` through `ErrorPolicy`; see the usage guide's error policy and [platform support](usage.md#platform-support). |
| Final 0.x: refused helper thread | `Collect`/`Skip` finish on existing workers; `Collect` records `SpawnWorker`, while `Abort` fails. See the usage guide's parallel-walk notes. |
| Final 0.x: physical repository for symlink roots | Every spelling of a symlinked root is attributed to the repository physically containing the target; see [Git filesystem adaptations](#git-filesystem-adaptations). |
| Final 0.x: extglob escape reading | An extglob escape has only its escaped-byte reading, matching Bash and zlob; see the [matcher entry-point contract](#matcher). |
| Final 0.x: Git bracket classes at slash endpoints | Slash endpoints preserve Git's range state and verdict; see [Git filesystem adaptations](#git-filesystem-adaptations). |
| Final 0.x: settle the 1.0 contract | `WalkError::operation()` is typed; the extensible enums require fallback match arms; compiled `Pattern` values have no representation equality; and extglobs obey the same entry-point rules as plain patterns, including depth-zero `**/@(x)`. See the [stability contract](stability.md#public-enum-policy), [usage guide](usage.md#match-paths-deliberately), and [matcher table](#matcher). |
| 1.0.0: absolute patterns under a root with `..` | A walk root with a `..` component rejects every absolute include or exclude, from `include`, `exclude`, `add_root`, and their `try_` forms, instead of selecting nothing when the pattern diverged from the root's spelling before the `..`; relative patterns are unaffected. See [absolute patterns](#absolute-patterns-and-the-caller-side-rewrite-they-replace). |
| 1.0.0: negated extglobs and hidden components | Without `match_hidden`, `!(…)` selects no hidden component, including one it reaches by crossing a separator; see [deliberate differences](#deliberate-differences) and the [usage guide](usage.md#hidden-paths-two-separate-switches). |
| 1.0.0: walker `./` on brace alternatives | Includes and excludes ignore one leading `./` on every brace-expanded alternative, as the path matchers do: `{./src/*.rs,lib/*.rs}` selects from both directories instead of silently dropping the `./` alternative, and `{./src/*.rs,./lib/*.rs}` is accepted instead of rejected as an unnormalized `.` component. See [walking](#walking) and the [usage guide](usage.md#walk-filesystems-with-explicit-policy). |
| 1.0.0: leading `!` in walker patterns | `include`, `exclude`, and their `try_` forms reject a pattern or brace alternative that starts with `!` not followed by `(`, instead of compiling it as a literal `!` that selects nothing; `\!` and `!(…)` are unchanged. See [migrating from fast-glob](#migrating-patterns-from-globset-or-fast-glob). |
| 1.0.0: extglob position rule in path filters | Under `is_match_path` a group directly after `/` is component-local like a wildcard there (for a repeated group only its first iteration), and brace alternatives are judged independently; under both path entry points a separator-crossing star before a component-local one keeps its backtrack point (`**/*.@(ts\|js)` reaches every depth), a separator or recursive `**` inside a group can cross components, and a star run such as `***` reads as it does outside a group. See the [matcher table](#matcher) and [usage guide](usage.md#match-paths-deliberately). |
| 1.0.0: excludes inside hidden directories | A walker exclude covers a leading period whatever `Walker::match_hidden` says, so `exclude("**/node_modules/**")` removes `.cache/node_modules/a.ts` and `exclude("*.log")` removes `.debug.log`; a covering exclude such as `x/**` also removes, and prunes, hidden descendants an include names literally. What an include selects is unchanged. See the [usage guide](usage.md#hidden-paths-two-separate-switches). |
| 1.0.0: `**` only as a whole path component | With `recursive_double_star`, `**` is recursive only when bounded by `/` or a pattern end on both sides; `**/x` no longer matches `sx`, `a/**/b` no longer matches `a/xb`, and a walker exclude `**/node_modules/**` no longer prunes `my_node_modules`. Any other `**` run is ordinary stars. See the [matcher section](#matcher), [deliberate differences](#deliberate-differences), [ADR-0020](adr/0020-double-star-only-as-a-whole-component.md), and the [usage guide](usage.md#match-paths-deliberately). |
| 1.0.0: extglob groups beside a whole-component `**` | A group reads like its alternatives written in its place: before a trailing `/**` it accepts the path without that suffix (`@(x)/**`, `!(y)/**`, and `*(a)/**` accept `x`, `x`, and `a`), a trailing group holding `**` does the same (`x/@(**)` accepts `x`), and a `**` ending an alternative in front of `/` may stand for no directory (`@(**)/y` accepts `y`, `a/@(**)/b` accepts `a/b`). A walker exclude such as `@(a\|b)/**` therefore excludes `a` itself, and a subtree cover prunes only a directory the exclude matches, so `a/**/**` no longer drops `a` itself. See the [usage guide](usage.md#match-paths-deliberately). |

## Defaults to review

Ferralk does not follow symlinks or apply `.gitignore` rules unless requested,
collects recoverable errors by default, avoids extra metadata syscalls by
default, and leaves ordering unsorted. Configure those choices explicitly when
migrating tool-like zlob usage.
