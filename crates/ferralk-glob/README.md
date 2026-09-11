# ferralk-glob

`ferralk-glob` compiles a glob once and matches it many times against
arbitrary bytes, so filenames never pass through a lossy UTF-8 conversion.
Syntax that changes meaning stays explicit through `PatternOptions`.

```rust
use ferralk_glob::{Pattern, PatternOptions};

let source_file = Pattern::compile(
    "src/**/*.{rs,toml}",
    PatternOptions::default()
        .recursive_double_star(true)
        .braces(true),
)?;

assert!(source_file.is_match_glob_path("src/lib.rs"));
assert!(!source_file.is_match_glob_path("src/generated/lib.rs.bak"));
# Ok::<(), ferralk_glob::PatternError>(())
```

Wildcard scope comes from the matching entry point. `is_match_glob_path` keeps
every ordinary wildcard in one component, as a shell glob does, and is the
entry point for filesystem paths. `is_match` is separator-agnostic.
`is_match_path` keeps zlob's list-filter rule, where a root wildcard may cross
separators but wildcards after an explicit separator are component-local. With
`recursive_double_star` disabled, `**` is equivalent to `*`; enable it for
recursive separator crossing. Braces, extglobs, hidden-name matching, ASCII
case folding, and changed escaping remain explicit opt-ins.

For the full syntax, error contract, and compatibility notes, see the
[crate documentation](https://docs.rs/ferralk-glob), the
[usage guide](https://github.com/sebastian-software/ferralk/blob/main/docs/usage.md),
and the [Ferralk repository](https://github.com/sebastian-software/ferralk).

<!-- ferramenta-family:start -->
**ferralk** is part of the [Ferramenta](https://ferramenta.dev) family — A family of Rust tools.

Siblings: [ferroni](https://sebastian-software.github.io/ferroni/) — Oniguruma-compatible regex engine · [ferriki](https://github.com/sebastian-software/ferriki) — Shiki-compatible syntax highlighting · [ferromark](https://sebastian-software.github.io/ferromark/) — Markdown to HTML with a secure default and every GFM extension included. · [ferrolex](https://github.com/sebastian-software/ferrolex) — Spell checking for text and code · [ferrocat](https://ferrocat.dev) — Translation catalog engine · [palamedes](https://palamedes.dev) — Internationalization for TypeScript applications · [ferrovia](https://github.com/sebastian-software/ferrovia) — SVGO-compatible SVG optimizer · [ferrugo](https://github.com/sebastian-software/ferrugo) — PDF previews for untrusted files.
<!-- ferramenta-family:end -->

Built by [Sebastian Software](https://oss.sebastian-software.com).
