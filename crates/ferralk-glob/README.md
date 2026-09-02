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

Wildcard scope comes from the matching entry point. `is_match` is
separator-agnostic; `is_match_path` keeps zlob's list-filter rule where a root
wildcard may cross separators but wildcards after an explicit separator are
component-local; `is_match_glob_path` keeps every ordinary wildcard in one
component. With `recursive_double_star` disabled, `**` is equivalent to `*`;
enable it for recursive separator crossing. Braces, extglobs, hidden-name
matching, ASCII case folding, and changed escaping remain explicit opt-ins.

For the full syntax, error contract, and compatibility notes, see the
[crate documentation](https://docs.rs/ferralk-glob) and the
[Ferralk repository](https://github.com/sebastian-software/ferralk).
