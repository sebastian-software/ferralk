# Oxc fast-glob reference

The second matcher reference is the Rust crate `fast-glob` `1.1.0`, the
version selected by Oxc's workspace at source commit
`8783524015b1e6ff1c39ccf426df0bb07cbbc588` (verified 2026-08-18). Its
upstream repository is `https://github.com/oxc-project/fast-glob`.

The harness runs only `corpus/fast-glob.jsonl` against this reference. Those
cases must use syntax shared by both engines and record a deliberate
disagreement with `oracle_expected` when needed. This is not a replacement for
the zlob oracle: fast-glob's documented `*` separator rule, leading `!`
negation, validation model, and maximum brace nesting differ from zlob and
from ferralk's documented compatibility profile.

## The shared subset

The `ferralk_vs_fast_glob` fuzz target asserts that both engines return the
same verdict, so it must first exclude the syntax they read differently. It
compares against `Pattern::is_match_glob_path`, whose component-local wildcard
policy is the one fast-glob applies; `is_match` is the fnmatch-style form zlob
defines and is not comparable here. Every divergence below is excluded by the
shape of the pattern, so a fuzz failure is always a new finding.

| Divergence | Example | ferralk | fast-glob | Exclusion |
|---|---|---|---|---|
| Leading `!` reads as negation | `!a` vs `b` | `false` | `true` | Patterns starting with `!` |
| `**` is a whole path component, not a recursive wildcard | `**/a` vs `aa` | `true` | `false` | Any `**` except the pattern `**` itself |
| A trailing `**` component elides to nothing | `a/**` vs `a` | `true` | `false` | Same rule |
| A backslash before an ordinary byte unescapes it | `\b` vs `b` | `true` | `false` | `\` only before `* ? [ ] { } \` |
| A class may accept a separator | `[/]` vs `/` | `false` | `true` | `/` inside a class, and every negated class |
| POSIX class names | `[[:alpha:]]` vs `a` | `true` | `false` | `[:` at the start of a class |
| Brace nesting depth | — | nested | capped | More than one open brace |

Brace expansion happens before matching, so an alternative can concatenate
with the surrounding text into a `**` that only ferralk reads recursively
(`{*}*` vs `/`). A star next to brace punctuation is therefore excluded too.

The first four rows and the POSIX row hold for `is_match` as well and are
corpus candidates. The class-versus-separator row exists only under the
component-local policy, which `corpus/fast-glob.jsonl` does not currently
replay.
