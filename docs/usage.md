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

- `.gitignore` and `.ignore` files are considered only after
  `respect_git_ignore(true)`.
- Symlinks are not followed unless `WalkOptions::follow_symlinks(true)` is
  selected; a canonical-path guard prevents directory cycles.
- `ErrorPolicy::Collect` is the default. It returns accepted entries and
  recoverable filesystem errors in one `WalkResult`.
- Results are unsorted unless `WalkOptions::sort(true)` is set.
- Metadata is not fetched unless `WalkOptions::metadata(true)` is set.
- `stream()` yields entries and recoverable errors incrementally. It is
  single-threaded and cannot provide global sorting.

`CancellationToken::cancel` requests a cooperative stop before the next
filesystem operation. It is safe to clone the token and keep it outside the
walker.

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

The checked-in JSONL corpus is the behavioural source of truth. Read
[corpus-format.md](corpus-format.md) before adding a case. Fuzz targets live in
[`fuzz/`](../fuzz/README.md); benchmark evidence and other deferred follow-up
are tracked in [GitHub](https://github.com/sebastian-software/ferralk/issues).
