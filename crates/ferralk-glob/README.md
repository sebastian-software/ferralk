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
