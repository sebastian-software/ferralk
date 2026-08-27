# ferralk-glob

`ferralk-glob` compiles reusable, byte-first glob patterns. It accepts arbitrary
bytes rather than requiring UTF-8 paths, and leaves behaviour-changing syntax
explicit through `PatternOptions`.

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

`*`, `?`, and character classes are component-local by default. Enable
`recursive_double_star` for `**` to cross separators, and opt into braces,
extglobs, hidden-name matching, ASCII case folding, or changed escaping only
when those semantics are required.

For the full syntax, error contract, and compatibility notes, see the
[crate documentation](https://docs.rs/ferralk-glob) and the
[Ferralk repository](https://github.com/sebastian-software/ferralk).
