# Consumer pattern contract

This page is for programs that take glob patterns from their own users, for
example the `include` and `exclude` lists of a configuration file, and pass
them to Ferralk. It covers what such a program can promise its users across
Ferralk 1.x, how to validate a pattern before accepting it, how to embed
literal text, and what changes when the program replaces `globset`. The
[usage guide](usage.md) and the
[compatibility guide](compatibility-guide.md) remain the references for the
switches themselves.

## What 1.x covers

The [stability contract](stability.md) applies to the pattern language as a
whole. A pattern that compiles under 1.x keeps compiling, and keeps selecting
what the [usage guide](usage.md) and the checked-in corpus say it selects, for
a given entry point and set of options. Fixes that bring behavior back to that
documented contract can ship in patch releases. What a pattern means depends
on two choices, and a consumer should fix both and tell its users which ones
it made.

**The dialect, chosen by `PatternOptions`:**

| Syntax | `PatternOptions::default()` | `PatternOptions::walker()` and every `Walker` pattern |
| --- | --- | --- |
| Literals, `*`, `?`, `\` escapes | yes | yes |
| Classes `[abc]`, `[a-z]`, `[!a]`, POSIX classes such as `[[:digit:]]` | yes | yes |
| `**` as a whole path component, recursive ([ADR-0020](adr/0020-double-star-only-as-a-whole-component.md)) | no: two ordinary stars | yes |
| Braces `{a,b}`, nested | no: literal text | yes |
| Extglobs `?(…)`, `*(…)`, `+(…)`, `@(…)`, `!(…)` | no: literal text | yes |

`match_hidden`, `case_insensitive` (ASCII only) and `escape` are further
switches on either preset. A `Walker` has no switch that turns braces,
extglobs or recursive `**` off; its only dialect switch is
`Walker::match_hidden`, and its excludes always cover a leading `.`.

**The reach of a wildcard, chosen by the entry point:**

| Entry point | Ordinary `*` and `?` |
| --- | --- |
| `Pattern::is_match_glob_path`, and a `Walker` in its default `WildcardMode::ComponentScoped` | stay inside one path component |
| `Pattern::is_match`, and a `Walker` in `WildcardMode::SeparatorCrossing` | cross `/`, as `globset` reads them by default |
| `Pattern::is_match_path` | the zlob list rule: crossing in the first component only |

A `Walker` adds rules that the matcher alone does not have. A trailing `/`
selects directories only. An absolute pattern is rewritten against the walk
root. A leading `!`, a `..` component, and an empty or `.` component are
rejected. These rules are part of the covered semantics too. They are listed
under [`Walker::exclude`](https://docs.rs/ferralk/latest/ferralk/struct.Walker.html#method.exclude).

## Validate a user-supplied pattern

Check a pattern the way it will be used. `Pattern::validate` checks the syntax
of one dialect. `Walker::try_include` and `Walker::try_exclude` also apply the
walker rules above, and leave the builder unchanged when they reject a
pattern, so one bad entry in a list costs nothing but its error. An absolute
pattern is checked against every configured root, so add the roots first.

```rust
use ferralk::Walker;
use ferralk::ferralk_glob::{Pattern, PatternOptions};

// Syntax only: this class is never closed.
let error = Pattern::validate("src/[a-z/*.rs", PatternOptions::walker()).unwrap_err();
assert_eq!(error.offset(), 4);

// A leading `!` is valid matcher text but not a walker pattern.
assert!(Pattern::validate("!**/*.test.ts", PatternOptions::walker()).is_ok());

let user_patterns = ["src/**/*.ts", "!**/*.test.ts", "src//*.ts"];
let mut walker = Walker::new(".");
let mut rejected = Vec::new();
for (index, pattern) in user_patterns.iter().enumerate() {
    if let Err(error) = walker.try_include(pattern) {
        rejected.push((index, error.offset()));
    }
}
assert_eq!(rejected, [(1, 0), (2, 4)]);
```

**Which parts of an error are stable.** `PatternError::offset()` is a byte
offset into the pattern exactly as the caller passed it, absolute patterns
included, so it can point at the construct in the user's own text. The
message, `PatternError::message()` and `Display`, is diagnostic text and may
be reworded in any release. Do not compare `PatternError` values with `==`
either, because equality includes the message. When a list is compiled, the
position in the list is the caller's to keep, as above.
`PatternSet::new` does it for you: `PatternSetError::index()` is the failing
glob's position in the list and `PatternSetError::pattern_error()` its
`PatternError`, with the same stable offset and the same unstable message. Walk errors carry `WalkError::operation()` and
`WalkError::io_kind()` for program logic, and `WalkOperation::as_str()` for a
stable spelling of the operation.

Pattern compilation is budgeted. A pattern whose brace expansion or compiled
program would exceed the limits is rejected with an offset, not allowed to
exhaust memory; the limits are listed under
[deliberate differences](compatibility-guide.md#deliberate-differences).

## Embed literal text

`ferralk_glob::escape` (bytes) and `ferralk_glob::escape_str` (`&str`) put a
`\` in front of every byte that is syntax in any dialect. The result matches
exactly the original text, under every option combination that keeps
escaping enabled, and through every entry point. It also stays literal inside
a brace or extglob alternative, and a leading `!` in it is never read as a
walker negation. Escaping marks syntax, not path structure. `/` still
separates components, a leading `./` is still the one the path entry points
ignore, and `case_insensitive` still folds escaped letters. The promise is
that the result is literal. If a later minor release adds syntax, the set of
escaped bytes can grow with it.

```rust
use ferralk::Walker;
use ferralk::ferralk_glob::{Pattern, PatternOptions, escape_str};

// A directory name a user typed, used below a glob of the program's own.
let pattern = format!("{}/**/*.md", escape_str("drafts [old]"));
let compiled = Pattern::compile(&pattern, PatternOptions::walker())?;
assert!(compiled.is_match_glob_path("drafts [old]/2024/notes.md"));
assert!(!compiled.is_match_glob_path("drafts o/2024/notes.md"));

// The walker accepts it as a relative pattern too.
let walker = Walker::new(".").include(&pattern)?;
# let _ = walker;
# Ok::<(), Box<dyn std::error::Error>>(())
```

A walk root whose name contains syntax can be spelled in front of an absolute
pattern the same way: the rewrite compares escaped syntax at or above the root
as the name it spells. Only those escapes are allowed there. On Windows,
replace the separators with `/` before escaping, because an escaped `\` is a
literal byte, never a separator.

What escaping does not change is the shape of a path. The walker still rejects
a relative pattern whose text has an empty component (a leading `/` or `//`),
a `.` or `..` component, or on Windows a byte that no Windows name can contain
(`\ : * ? " < > |`). A path with that shape names no walk candidate anyway.
With `PatternOptions::escape(false)` a backslash is an ordinary byte, so an
escaped string is not literal in that dialect.

## Porting from `globset`

The [migration table](compatibility-guide.md#coming-from-globset-glob-fast-glob-ignore-or-walkdir)
lists every default that differs. For a program that exposes `globset`
patterns to its users, such as palamedes' catalog lists, these are the
differences its users can observe.

- **`*` crosses `/` in `globset`.** Keep that reading with
  `Walker::wildcard_mode(WildcardMode::SeparatorCrossing)`, or with
  `Pattern::is_match` when matching outside a walk. `**` is recursive only as
  a whole component in both.
- **Hidden names.** A `globset` wildcard matches a leading `.`. Set
  `Walker::match_hidden(true)` or `PatternOptions::match_hidden(true)` for
  includes. Excludes cover hidden names either way.
- **Syntax that `globset` reads as text.** Extglobs such as `@(a|b)` and
  `!(a)`, and POSIX classes such as `[[:digit:]]`, are syntax in the walker
  dialect and cannot be switched off there. Brace alternatives follow
  Ferralk's expansion, including an empty alternative: `{,foo}.ts` also names
  `.ts`. A program whose users rely on `globset`'s reading has to announce the
  change, or escape those constructs before passing a pattern on.
- **A leading `!`** is literal text in `globset`, but `Walker::include` and
  `exclude` reject it. `\!` spells the literal.
- **Escaping.** `globset::escape` writes `[*]`, and `globset` reads `\` as an
  escape only on Unix. Ferralk reads `\` as an escape on every platform, and
  `escape_str` writes `\*`. That removes the Unix-only `[\]` workaround, and
  any caller code that decodes `[*]` forms, for example to find a pattern's
  literal prefix, has to decode `\*` instead.
- **Absolute patterns.** The walker rewrites them against its root. There is
  no need to keep a second matcher for the full path, and no need to push
  excludes down selectively. One engine decides, so a pattern Ferralk rejects
  has to be reported to the user rather than skipped.
- **Windows.** Patterns use `/` on every platform, because `\` is always an
  escape. The walker rejects the `\`-separated spellings that could never
  match, as the
  [compatibility guide](compatibility-guide.md#patterns-are-written-with--on-every-platform)
  describes.

```rust
use ferralk::{Walker, WalkOptions, WildcardMode};
use ferralk::ferralk_glob::escape_str;

// A project root the program holds, made literal and spelled with `/`.
let root = std::env::current_dir()?;
let literal_root = escape_str(&root.to_string_lossy().replace('\\', "/"));

let walker = Walker::new(&root)
    // The two switches that give a walk `globset`'s reading.
    .wildcard_mode(WildcardMode::SeparatorCrossing)
    .match_hidden(true)
    .options(WalkOptions::default().files_only(true))
    // The catalog's own patterns, joined to the literal root.
    .include(format!("{literal_root}/app/**/*.{{js,jsx,ts,tsx,mdx}}"))?
    .exclude(format!("{literal_root}/**/node_modules/**"))?;
# let _ = walker;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The [Palamedes adoption](palamedes-adoption.md) record shows how the walker
side of this port was measured.
