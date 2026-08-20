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

```rust
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
  once per root. See the
  [compatibility guide](compatibility-guide.md#several-roots-in-one-walk).

- Include and exclude patterns may be absolute. `Walker::new("/repo")
  .include("/repo/src/**/*.ts")` selects what `src/**/*.ts` selects, so a
  caller holding absolute patterns does not have to strip the root itself. A
  pattern about a different tree selects nothing rather than erroring; a
  wildcard at or above the root, a `..`, and a pattern naming the root itself
  are rejected. See the
  [compatibility guide](compatibility-guide.md#absolute-patterns-and-the-caller-side-rewrite-they-replace).

- `.gitignore` and `.ignore` files are considered only after
  `respect_git_ignore(true)`, together with the walk root's
  `.git/info/exclude`, which they override. The file closest to an entry
  decides, as in Git.
- Symlinks are not followed unless `WalkOptions::follow_symlinks(true)` is
  selected; a canonical-path guard prevents directory cycles.
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
  recoverable filesystem errors in one `WalkResult`.
- Results are unsorted unless `WalkOptions::sort(true)` is set.
- Metadata is not fetched unless `WalkOptions::metadata(true)` is set.
- `stream()` yields entries and recoverable errors incrementally. It is
  single-threaded and cannot provide global sorting.

`CancellationToken::cancel` requests a cooperative stop before the next
filesystem operation. It is safe to clone the token and keep it outside the
walker.

### Filtering with a matcher of your own

`collect()` hands back every entry it found, so a caller that applies its own
predicate afterwards runs it on one thread over the whole list. On a large tree
that pass is big enough to cancel out the threads the walk just used.

`visit()` asks the predicate on the worker that produced the entry:

```rust
use ferralk::{Verdict, Walker};

let result = Walker::new("src")
    .threads(4)
    .visit(|entry| {
        if my_matcher.is_match(entry.path()) {
            Verdict::Keep
        } else {
            Verdict::Skip
        }
    })?;
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

```rust
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
ferralk = { git = "https://github.com/sebastian-software/ferralk", features = ["native-linux"] }
```

`native-linux` applies only on Linux and `native-macos` only on macOS; other
platforms retain the portable backend. These backends are not a stability or
performance promise while Ferralk remains pre-1.0. See ADR-0010 for the
rollout policy.

## Development and validation

Run the normal workspace suite before changing matcher or walker behaviour:

```sh
cargo test --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

The minimum supported Rust version is written down once, as `rust-version`
under `[workspace.package]` in the root `Cargo.toml`; the CI MSRV job reads
that field instead of pinning a version of its own. Per
[ADR-0004](adr/0004-msrv-stable-minus-two.md) the MSRV tracks current stable
minus two releases, so when a new stable lands, bumping it is one edit to that
field plus a changelog entry — no workflow change.

The checked-in JSONL corpus is the behavioural source of truth. Read
[corpus-format.md](corpus-format.md) before adding a case. Fuzz targets live in
[`fuzz/`](../fuzz/README.md); benchmark evidence and other deferred follow-up
are tracked in [GitHub](https://github.com/sebastian-software/ferralk/issues).
