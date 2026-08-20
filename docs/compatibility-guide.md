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
| single filesystem-glob candidate | `Pattern::is_match_glob_path` (all ordinary wildcards are component-local) |
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
| `ZLOB_GITIGNORE` | `Walker::respect_git_ignore(true)` (`.git/info/exclude`, then `.gitignore`, then zlob-compatible `.ignore`) |
| `ZLOB_WALK_KEEP_GIT_DIR` | `WalkOptions::keep_git_dir(true)` |
| `ZLOB_SKIP_HIDDEN` | `WalkOptions::skip_hidden(true)` |
| `ZLOB_PERIOD` on a walk | `Walker::match_hidden(true)` (include and exclude patterns alike) |
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

`Walker::match_hidden` and `WalkOptions::skip_hidden` are separate mechanisms
and not each other's inverse: `match_hidden` is matcher semantics, deciding
whether a wildcard may cover a leading period, while `skip_hidden` is a
traversal filter that removes hidden entries before any pattern is consulted.

Walker include patterns are root-relative. A leading `./` is accepted, and a
trailing `/` selects matching directories only. Ordinary wildcards stay inside
one path component by default; use recursive `**` to select descendants, or
switch the whole walk to crossing wildcards as described below. A pattern that
starts at a filesystem root is understood as absolute and rewritten, as
described next.

### Several roots in one walk

`Walker::add_root` and `Walker::add_roots` extend a walk to more than one tree.
A caller with several source directories used to build one walker per directory,
and with it one thread pool per directory; the roots are now the walk's initial
directories and everything downstream of that is shared — the scheduler, the
helper-spawn floor, and the visited-directory guard.

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

The three rejections are the shapes where guessing would silently select the
wrong entries. A wildcard standing where the root's own components are may or
may not cover the root, and deciding that needs matching rather than
arithmetic; write the part below the root instead, where `**/*.ts` says what
`/**/*.ts` was reaching for. A `..` is not folded away because folding it
lexically is wrong across a symlink, and resolving it properly would mean
touching the filesystem to compile a pattern. Naming the root itself selects
nothing, because the walk emits what is inside the root.

A pattern about a different tree is not an error, because a caller may hold one
pattern list and run it against several roots. It selects nothing and — the
part that matters for a walk — prunes nothing: an exclude that cannot reach
this tree never closes a directory in it.

**What this was before.** A pattern starting with `/` used to reach the matcher
unchanged and match no candidate at all, because walk candidates are
root-relative and never start with a separator. An absolute include therefore
produced an empty walk and an absolute exclude did nothing. Nothing that
previously selected entries selects different ones now; the patterns that
change behaviour are the ones that selected nothing, which now either work or
say why they cannot.

### Migrating patterns from globset or fast-glob

`globset` and `fast-glob` read an unconfigured `*` as crossing separators, so
`*.ts` selects `src/deep/main.ts` there. Ferralk's walker reads patterns as
filesystem globs by default, where `*.ts` selects only what sits in the walk
root. Carrying a pattern over unchanged therefore used to select strictly less,
without saying so.

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

Two things carry over unchanged. `**` is recursive under either mode, so a
pattern already written with `**` means the same thing in both. And a literal
prefix is still a literal prefix: `src/*.ts` never reaches outside `src/`, which
is why the walker can still skip sibling directories without opening them.

The mode governs excludes as well as includes, so a walk reads every pattern the
same way. It is a matching policy, independent of `match_hidden` and of
`WalkOptions::skip_hidden`.

## Deliberate differences

- Ferralk has no C ABI and no zlob-Rust migration facade. It exposes the two
  Rust crates `ferralk-glob` and `ferralk` instead (ADR-0003).
- Direct matching excludes leading-period path components by default. Enable
  `PatternOptions::match_hidden` for a compiled pattern, or
  `Walker::match_hidden` for a whole walk, to opt in; this is the
  POSIX-conservative default selected by ADR-0011.
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
- Backslash escapes inside character classes follow bash and glibc/BSD
  `fnmatch`: an escaped `-` is a literal member and never a range operator
  (`[a\-z]` is exactly `{a, -, z}`). zlob 1.6.3 performs no escape processing
  inside classes and reads the backslash as an ordinary range endpoint. The
  diverging verdicts are recorded as `disputed` corpus cases with
  `oracle_expected` (`class-006/008/009/012/016/024/025`, issue #16).
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

## Defaults to review

Ferralk does not follow symlinks or apply `.gitignore` rules unless requested,
collects recoverable errors by default, avoids extra metadata syscalls by
default, and leaves ordering unsorted. Configure those choices explicitly when
migrating tool-like zlob usage.
