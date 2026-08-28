# Using Ferralk

Ferralk has one matcher crate and one walker crate. The walker re-exports the
matcher crate, but depending on `ferralk-glob` alone keeps applications that do
not traverse filesystems dependency-light.

## Match paths deliberately

`Pattern` accepts `AsRef<[u8]>`, so callers can match filenames without lossy
Unicode conversion. `Pattern::is_match` compares an entire byte sequence. For
paths, prefer `is_match_glob_path`: ordinary `*`, `?`, classes, and Extglob
operators stay within one component, while an explicitly enabled `**` crosses
components.

```rust
use ferralk_glob::{Pattern, PatternOptions};

let options = PatternOptions::default()
    .braces(true)
    .extglob(true)
    .recursive_double_star(true)
    .match_hidden(false);
let pattern = Pattern::compile("{src,tests}/**/*.rs", options)?;

assert!(pattern.is_match_glob_path("src/lib.rs"));
assert!(pattern.is_match_glob_path("tests/unit/parser.rs"));
assert!(!pattern.is_match_glob_path(".cache/src/lib.rs"));
# Ok::<(), ferralk_glob::PatternError>(())
```

The `PatternOptions` default is intentionally conservative:

| Option | Default | Effect when enabled |
| --- | --- | --- |
| `braces` | off | Enables nested `{a,b}` alternatives. |
| `recursive_double_star` | off | Gives consecutive `**` recursive semantics. |
| `extglob` | off | Enables Bash-style `@()`, `?()`, `*()`, `+()`, and `!()`. |
| `match_hidden` | off | Allows wildcard tokens to match a leading period. |
| `case_insensitive` | off | Uses ASCII-only case folding. |
| `escape` | on | Interprets backslash as an escape. |

Use `Pattern::validate` for syntax-only checks and `Pattern::has_wildcards` to
choose between a literal and glob path without compiling an application-level
policy twice.

## Walk filesystems with explicit policy

`Walker` filters paths relative to its root. Include patterns are OR-ed; no
include accepts every non-excluded entry. Excluded directories are pruned only
when the walker can prove that no include can re-admit a descendant.

```rust,no_run
use ferralk::{CancellationToken, ErrorPolicy, WalkOptions, Walker};

let cancellation = CancellationToken::default();
let result = Walker::new("workspace")
    .include("**/*.{rs,toml}")?
    .exclude("**/target/**")?
    .respect_git_ignore(true)
    .cancellation(cancellation.clone())
    .threads(4)
    .error_policy(ErrorPolicy::Collect)
    .options(
        WalkOptions::default()
            .files_only(true)
            .sort(true)
            .max_depth(8),
    )
    .collect()?;

if result.was_cancelled() {
    eprintln!("walk stopped early");
}
# let _ = result;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Keep a configured walker when input patterns are invalid

The chaining `Walker::include` and `Walker::exclude` methods intentionally
consume and return the builder, which keeps ordinary configuration concise. For
a caller-supplied pattern list, use their borrowed `try_` counterparts instead.
Each rejected pattern leaves the complete `Walker` unchanged, so valid entries
before and after it still compose across all configured roots and matcher modes.

```rust,no_run
use ferralk::{WalkOptions, Walker};

let mut walker = Walker::new("workspace")
    .add_root("generated-workspace")?
    .options(WalkOptions::default().files_only(true));

for pattern in ["src/**/*.rs", "[a", "tests/**/*.rs"] {
    if let Err(error) = walker.try_include(pattern) {
        eprintln!("skipping invalid include {pattern:?}: {error}");
    }
}
for pattern in ["**/generated/**", "[a", "**/*.tmp"] {
    if let Err(error) = walker.try_exclude(pattern) {
        eprintln!("skipping invalid exclude {pattern:?}: {error}");
    }
}

let result = walker.collect()?;
# let _ = result;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Important defaults:

- Ordinary wildcards (`*`, `?`, classes) stay inside one path component, so
  `*.ts` selects a file in the walk root and not `src/main.ts`. `**` is
  recursive under either setting, and
  `Walker::wildcard_mode(WildcardMode::SeparatorCrossing)` switches to the
  reading `globset` and `fast-glob` use; see the
  [compatibility guide](compatibility-guide.md).

- **Patterns use `/` on every platform, and `\` is the escape character** -
  on Windows too. A pattern built by joining `PathBuf`s therefore does not mean
  what it looks like: `root.join("src").join("*.ts")` produces `C:\root\src\*.ts`,
  where `\*` asks for a literal `*` in a filename. Write `include("src/**/*.ts")`
  and let `Walker::new(root)` hold the path. Windows rejects the shapes that
  could never match, with a message saying so; see the
  [compatibility guide](compatibility-guide.md#patterns-are-written-with--on-every-platform).

- A walk may have several roots. `Walker::new(first).add_root(second)?` walks
  both trees with one thread pool, and `WalkEntry::root` says which root an
  entry came from. Patterns apply under every root, `depth` is counted from the
  entry's own root, and roots that contain one another deliver their overlap
  once per root. A root must be a directory: a file root reports `read_dir`
  with a not-a-directory error and emits no entry for the file. A directory
  symlink supplied as a root is always traversed; `follow_symlinks` only
  affects symlinks found below a root. See the
  [compatibility guide](compatibility-guide.md#several-roots-in-one-walk).

- Include and exclude patterns may be absolute. `Walker::new("/repo")
  .include("/repo/src/**/*.ts")` selects what `src/**/*.ts` selects, so a
  caller holding absolute patterns does not have to strip the root itself. A
  pattern about a different tree selects nothing rather than erroring; a
  wildcard at or above the root, a `..`, and a pattern naming the root itself
  are rejected. Relative patterns receive the same guardrails: `.` and `./`
  name the root, and a `.` component after the conventional leading `./`
  (such as `src/./main.rs`) or a real `..` component is rejected with guidance
  instead of silently selecting nothing. See the
  [compatibility guide](compatibility-guide.md#absolute-patterns-and-the-caller-side-rewrite-they-replace).

- `.gitignore` and `.ignore` files are considered only after
  `respect_git_ignore(true)`, together with the walk root's `.git/info/exclude`,
  which they override. Linked-worktree and submodule `.git` pointer files are
  resolved (including one `commondir`), and the file closest to an entry
  decides, as in Git. In-tree `.gitignore` and `.ignore` symlinks are ignored
  rather than followed; `.git/info/exclude` keeps Git's link-following behavior.
  Each ignore or repository-metadata file is capped at 8 MiB, and each rule
  file at 100,000 lines. A rule file that exceeds either limit, or otherwise
  cannot be read, contributes no rules and is reported as a `read_ignore`
  failure through the configured `ErrorPolicy`.

  Ferralk reads `core.ignoreCase` and `core.precomposeUnicode` from the
  repository-local `.git/config`; a linked worktree also reads its private
  `config.worktree` after the common config when
  `extensions.worktreeConfig=true`. `core.ignoreCase=true` uses Git-compatible
  ASCII folding for rules and candidates. A differently cased `.Gitignore` is
  loaded only when opening the canonical `.gitignore` name resolves on the
  filesystem, so a case-sensitive filesystem never treats it as a magic file.
  On macOS only, `core.precomposeUnicode=true` NFC-normalizes valid UTF-8
  candidate components before matching and leaves invalid bytes untouched.
  These local booleans accept Git's named forms, bare keys, empty assignments,
  and signed base-zero integer forms (including `K`/`M`/`G` scaling); malformed
  values do not override an earlier valid repository-local value. Only the
  exact top-level `[core]` and `[extensions]` sections apply: quoted
  subsections are distinct. Git backslash-newline continuations are decoded
  before comments, quotes, escapes, and boolean parsing, preserving the next
  line's indentation.

  System/global config, includes, and environment overrides are
  intentionally not consulted: they are process-wide state outside a library
  walk. Use `Walker::git_ignore_case(value)` and
  `Walker::git_precompose_unicode(value)` to supply Git's effective values;
  each explicit Walker value takes precedence over repository-local config.
  `clear_git_ignore_case` and `clear_git_precompose_unicode` resume
  repository-local detection on a reused builder.

  Ferralk deliberately continues into nested repositories, as ripgrep does:
  outer ignore rules remain active inside them and their own ignore files are
  loaded too. This differs from Git's repository-boundary behavior.
- Symlinks discovered below a root are not followed unless
  `WalkOptions::follow_symlinks(true)` is selected; a canonical-path guard
  prevents directory cycles within each root traversal. Descendants of one root
  share its guard, but separately supplied roots — including duplicates,
  overlaps and symlink aliases — use independent guards. A root that is itself
  a directory symlink is opened regardless, because the caller supplied it as
  the tree to walk.
- `files_only(true)` and `directories_only(true)` filter on the kind a
  directory listing reports, and a listing reports a symlink as a symlink and
  nothing about its target. So by default `files_only` keeps every symlink -
  including a broken one and one pointing at a directory - and
  `directories_only` keeps none of them. That matches zlob's
  `ZLOB_WALK_NO_REPORT_DIRS`, which is why it is the default.

  `WalkOptions::resolve_symlink_kind(true)` classifies symlink entries by their
  target instead, which is what callers who mean `Path::is_file` want:

  | the link points at | `files_only` | `directories_only` |
  |---|---|---|
  | a file | kept | dropped |
  | a directory | dropped | kept |
  | nothing (broken) | dropped | dropped |

  It costs one `stat` per symlink entry, paid only when one of the two filters
  is on and the walk is not already following symlinks. A broken link is
  dropped without an error - there is no target, so it is neither kind - while
  a `stat` that fails for any other reason leaves the kind unknown and is
  reported through the `ErrorPolicy`. The entry's own `kind()` still reports
  `Symlink` either way: the switch answers what the link points at, not what
  the entry is.
- `ErrorPolicy::Collect` is the default. It returns accepted entries and
  recoverable filesystem errors in one `WalkResult`. `Skip` still drops
  recoverable failures discovered below a root, but it always reports a root
  open/read failure: a missing, unreadable, or non-directory caller-supplied
  root must not look like an empty tree. Other roots continue, so a multi-root
  walk can return their entries alongside the failed root's error. `stream()`
  yields that root error as an item under both `Collect` and `Skip`; `Abort`
  returns it immediately. Ignore-rule read and safety-limit failures use the
  `read_ignore` operation; under `Skip` they are omitted like other
  descendant-level recoverable failures.
- Results are unsorted unless `WalkOptions::sort(true)` is set.
- Metadata is not fetched unless `WalkOptions::metadata(true)` is set.
- `stream()` yields entries and recoverable errors incrementally. It is
  single-threaded and cannot provide global sorting.

`CancellationToken::cancel` requests a cooperative stop. It is safe to clone the
token and keep it outside the walker; walkers only observe it, so their own
abort errors, worker-start failures, visitor stops, and panics do not cancel a
token the caller may share or reuse.

The request is observed at a bounded granularity rather than between every two
filesystem operations. `collect()`, serial and parallel alike, reads the token
when a worker takes a directory and then once every 64 entries while it works
through the listing, so a cancelled walk classifies at most that many more
entries per worker before stopping and never opens another directory.
`stream()` reads it before every entry it yields. `Verdict::Stop` reaches a
parallel walk's workers, the one that returned it included, at that same
granularity; a serial walk stops at the very next entry, because there the
verdict is a local flag rather than shared state.

Reading shared cancellation state per entry bought a promptness nothing can
observe — a walk is free to finish the listing it has already read either way —
and cost a load of a shared atomic on the walk's hottest loop.

### Filtering with a matcher of your own

`collect()` hands back every entry it found, so a caller that applies its own
predicate afterwards runs it on one thread over the whole list. On a large tree
that pass is big enough to cancel out the threads the walk just used.

`visit()` asks the predicate on the worker that produced the entry. A
`WalkEntry` keeps its native `Path`, while `Pattern` is byte-first; pass
`WalkEntry::path_bytes()` to bridge them without allocating or converting
through UTF-8. The bytes are the native `OsStr::as_encoded_bytes()` form: raw
filesystem bytes on Unix and lossless WTF-8 on Windows (ADR-0005).

```rust,no_run
use ferralk::{Verdict, Walker, ferralk_glob::{Pattern, PatternOptions}};

let my_matcher = Pattern::compile(
    "**/*.rs",
    PatternOptions::default().recursive_double_star(true),
)?;

let result = Walker::new("src")
    .threads(4)
    .visit(|entry| {
        if my_matcher.is_match(entry.path_bytes()) {
            Verdict::Keep
        } else {
            Verdict::Skip
        }
    })?;
# let _ = result;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Everything else behaves as it does for `collect()`: cancellation, the error
policy, panic propagation and sorting are unchanged, and only which entries
survive differs.

- `Verdict::Skip` drops the entry from the result. It does **not** prune: a
  directory is still descended into, because pruning a subtree is what
  `exclude()` expresses.
- `Verdict::Stop` ends the walk the way a cancellation request does, and
  `WalkResult::was_cancelled` reports it. A caller-owned `CancellationToken` is
  left alone.
- The visitor is shared across workers rather than cloned, so it takes `&self`
  and must be `Sync`. Per-worker state belongs in a thread-local.

Below a small tree size the walk stays on one thread whatever `threads()` says:
starting workers costs more than a handful of directories does.

### Hidden paths: two separate switches

An ordinary wildcard does not cover a leading period, so `**/*.ts` skips
`.react-router/routes.ts` — the period belongs to a directory component, and
the whole subtree stays out of the result. `Walker::match_hidden(true)` opts in
for include and exclude patterns alike:

```rust,no_run
use ferralk::{WalkOptions, Walker};

let result = Walker::new("workspace")
    .match_hidden(true)
    .include("site/**/*.ts")?
    .exclude("**/node_modules/**")?
    .options(WalkOptions::default().sort(true))
    .collect()?;
# let _ = result;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Builder order does not matter; patterns added before the call are recompiled.
A literal period is not a wildcard, so `.claude/**` selects a hidden directory
with either setting.

`WalkOptions::skip_hidden(true)` is a different mechanism, not the inverse of
this one. It is a traversal filter: it drops every entry with a leading-period
component and never descends into a hidden directory, before any pattern is
consulted. `match_hidden` only decides what a wildcard is allowed to cover.
With `skip_hidden` enabled no hidden path survives long enough for
`match_hidden` to matter.

## Platform support

The portable backend uses `std::fs` on Linux, macOS, and Windows. Experimental
native backends are feature-gated:

```toml
ferralk = { version = "0.9.1", features = ["native-linux"] } # x-release-please-version
```

`native-linux` applies only on Linux and `native-macos` only on macOS; other
platforms retain the portable backend. These backends are not a stability or
performance promise while Ferralk remains pre-1.0. See ADR-0010 for the
rollout policy. `native-macos` links Darwin's private `__getdirentries64`
stub: it is what libc's `readdir` uses today, but it is not an Apple public API
and can be rejected by App Store private-symbol checks. App-Store-distributed
applications should keep the default portable backend. If a filesystem or a
future OS refuses the native call at runtime, Ferralk maps that refusal to the
portable fallback; this preserves correctness but not native performance.

On Unix, a native no-follow walk opens scheduled non-root directories with
`O_NOFOLLOW`, closing the directory-to-symlink replacement window at the final
component while retaining the portable reader's support for a user-supplied
directory-symlink root. A changed scheduled path is rejected (`ELOOP` on
Linux; Darwin may report `ENOTDIR` with `O_DIRECTORY`). If a native syscall is
unsupported, Ferralk reports that scheduled directory rather than reopening it
through a symlink-following `std::fs::read_dir(path)` call. This is the native
reader's deliberate safety advantage; a portable-only walk cannot offer the
same atomic final-component guarantee.

## Development and validation

Run the normal workspace suite before changing matcher or walker behaviour:

```sh
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p harness -- corpus
```

The minimum supported Rust version is written down once, as `rust-version`
under `[workspace.package]` in the root `Cargo.toml`; the CI MSRV job reads
that field instead of pinning a version of its own. Per
[ADR-0004](adr/0004-msrv-stable-minus-two.md) the MSRV tracks current stable
minus two releases. The scheduled/manual policy check compares that field with
Rust's official stable-channel metadata, so a new stable release turns drift
into a targeted maintenance update rather than a surprise in ordinary CI.

The checked-in JSONL corpus is the behavioural source of truth. Read
[corpus-format.md](corpus-format.md) before adding a case. Fuzz targets live in
[`fuzz/`](../fuzz/README.md); benchmark evidence and other deferred follow-up
are tracked in [GitHub](https://github.com/sebastian-software/ferralk/issues).
