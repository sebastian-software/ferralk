# ferralk

`ferralk` is a safe, portable filesystem walker with byte-first,
root-relative glob filtering. It re-exports `ferralk-glob`, so walker patterns
and standalone matchers use the same component-aware language.

```rust,no_run
use ferralk::{ErrorPolicy, WalkOptions, Walker};

let result = Walker::new(".")
    .include("src/**/*.rs")?
    .exclude("**/generated/**")?
    .respect_git_ignore(true)
    .threads(4)
    .error_policy(ErrorPolicy::Collect)
    .options(WalkOptions::default().files_only(true).sort(true))
    .collect()?;

for entry in result.entries() {
    println!("{}", entry.path().display());
}

for error in result.errors() {
    eprintln!("{error}");
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Paths remain native `Path` values throughout traversal; use
`WalkEntry::path_bytes()` only when passing a path to a byte-first matcher.
Portable `std::fs` traversal is the default, with optional native Linux and
macOS backends for platform-specific performance work.

See the [crate documentation](https://docs.rs/ferralk) for the full API and the
[Ferralk repository](https://github.com/sebastian-software/ferralk) for
compatibility and development documentation.
