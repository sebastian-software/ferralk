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
same verdict, so it must first exclude the input shapes they read differently.
It compares against `Pattern::is_match_glob_path`, whose component-local
wildcard policy is the one fast-glob applies; `is_match` is the fnmatch-style
form zlob defines and is not comparable here. Every divergence below is
excluded by the input shape, so a fuzz failure is always a new finding.

| Divergence | Example | ferralk | fast-glob | Exclusion |
|---|---|---|---|---|
| One leading `./` is normalized by path APIs | `./` vs `` | `true` | `false` | Any brace-expanded pattern or candidate starting with `./` |
| Leading `!` reads as negation | `!a` vs `b` | `false` | `true` | Patterns starting with `!` |
| `**` is a whole path component, not a recursive wildcard | `**/a` vs `aa` | `true` | `false` | Any `**` except the pattern `**` itself |
| A trailing `**` component elides to nothing | `a/**` vs `a` | `true` | `false` | Same rule |
| A backslash before an ordinary byte unescapes it | `\b` vs `b` | `true` | `false` | `\` only before `* ? [ ] { } \` |
| A class may accept a separator | `[/]` vs `/`, `[.-r]` vs `/` | `false` | `true` | `/` inside a class, a range spanning `/`, and every negated class |
| POSIX class names | `[[:alpha:]]` vs `a` | `true` | `false` | `[:` at the start of a class |
| Brace nesting depth | — | nested | capped | More than one open brace |
| More than ten brace groups | `{a,b}` × 11 vs `b` × 11 | `true` | `false` | More than ten groups in a pattern |
| A comma outside a brace group stays an alternative separator | `{}{},` vs `` | `false` | `true` | Any comma at brace depth zero |
| Brace expansion splits inside a class | `{,[a,b]}[c]` vs `a` | `true` | `false` | Any comma inside a class at brace depth > 0 (member or range endpoint) |

The comma row is a fast-glob defect rather than a design difference: after two
brace groups close, the following comma is still read as a separator, so
`{}{},` accepts the empty candidate there and the literal `,` in ferralk. The
differential fuzz target found it within a minute of its first run; reported
upstream: <https://github.com/oxc-project/fast-glob/issues/165>.

The ten-group row is a defect too, and a silent one: past ten brace groups
fast-glob answers `false` for a pattern that matches, rather than reporting that
it gave up. The cap counts groups, not combinations — one group of two thousand
alternatives is answered correctly, while eleven two-way groups miss even their
first combination, which needs no backtracking at all. Found by the differential
target on issue #42, once ferralk's own expansion budget replaced the cap the
fuzz harness used to apply to both engines; reported upstream:
<https://github.com/oxc-project/fast-glob/issues/166>. It is the only reference
that bounds brace expansion at all: zlob 1.6.3 and glibc `GLOB_BRACE` run until
they exhaust the machine, and ferralk reports `too many brace alternatives`.

Brace expansion happens before matching, so an alternative can concatenate
with the surrounding text into a `**` that only ferralk reads recursively
(`{*}*` vs `/`). A star next to brace punctuation is therefore excluded too.
For the same reason, the `./` exclusion checks the expanded alternatives: an
empty brace arm can expose a later prefix (`{x,}./`), while a dot arm can join
the following separator (`{.}/`).

The negation, recursive wildcard, escaping, and POSIX rows hold for `is_match`
as well and are corpus candidates. Leading `./` normalization and the
class-versus-separator row exist only under the component-local policy.
