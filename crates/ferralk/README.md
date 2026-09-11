# ferralk

`ferralk` finds files. Give it a glob such as `{src,packages}/**/*.{ts,tsx}`
and it walks the tree in parallel, applies `.gitignore` the way Git does, and
opens only the directories the pattern can reach. It re-exports
`ferralk-glob`, so walker patterns and standalone matchers use the same
component-aware language.

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
Hidden-entry matching, symlink following, Git ignores, metadata, and sorting
are off until asked for, and recoverable errors are collected next to the
entries by default. Portable `std::fs` traversal is the default, with optional
native Linux and macOS backends for platform-specific performance work.

See the [crate documentation](https://docs.rs/ferralk) for the full API, the
[usage guide](https://github.com/sebastian-software/ferralk/blob/main/docs/usage.md)
for every default and switch, and the
[Ferralk repository](https://github.com/sebastian-software/ferralk) for
benchmarks, compatibility, and development documentation.

<!-- ferramenta-family:start -->
**ferralk** is part of the [Ferramenta](https://ferramenta.dev) family — A family of Rust tools.

Siblings: [ferroni](https://sebastian-software.github.io/ferroni/) — Oniguruma-compatible regex engine · [ferriki](https://github.com/sebastian-software/ferriki) — Shiki-compatible syntax highlighting · [ferromark](https://sebastian-software.github.io/ferromark/) — Markdown to HTML with a secure default and every GFM extension included. · [ferrolex](https://github.com/sebastian-software/ferrolex) — Spell checking for text and code · [ferrocat](https://ferrocat.dev) — Translation catalog engine · [palamedes](https://palamedes.dev) — Internationalization for TypeScript applications · [ferrovia](https://github.com/sebastian-software/ferrovia) — SVGO-compatible SVG optimizer · [ferrugo](https://github.com/sebastian-software/ferrugo) — PDF previews for untrusted files.
<!-- ferramenta-family:end -->

Built by [Sebastian Software](https://oss.sebastian-software.com).
