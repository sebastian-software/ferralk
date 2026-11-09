# Oxc fast-glob reference

The second matcher reference is the Rust crate `fast-glob` `1.1.2`, locked in
the harness and fuzz workspace lockfiles (verified 2026-11-09). Its upstream repository
is `https://github.com/oxc-project/fast-glob`. The original reference was
`1.1.0`, selected by Oxc's workspace at source commit
`8783524015b1e6ff1c39ccf426df0bb07cbbc588` on 2026-08-18.

The harness runs only `corpus/fast-glob.jsonl` against this reference. Those
cases must use syntax shared by both engines and record a deliberate
disagreement with `oracle_expected` when needed. This is not a replacement for
the zlob oracle: fast-glob's documented `*` separator rule, leading `!`
negation, validation model, and maximum brace nesting differ from zlob and
from Ferralk's documented compatibility profile.

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
| A trailing `**` component elides to nothing | `a/**` vs `a` | `true` | `false` | A whole-component `**` at the end of a pattern that has anything before it |
| A whole-component run of three or more stars stays recursive | `***/a` vs `b/c/a` | `true` | `false` | A star run longer than two that is a whole component |
| An attached `**` hands over to a following `/**/` without its separator | `x**/**/*` vs `xa` | `false` | `true` | A component ending in an attached two-star run, followed by `/**/` |
| Backtracking from a brace can make an earlier attached even star run recursive | `**/a**/{}` vs `a/a` | `false` | `true` | A recursive prefix, then an attached even star run ending a component, then a brace group |
| A backslash before an ordinary byte unescapes it | `\b` vs `b` | `true` | `false` | `\` only before `* ? [ ] { } \` |
| POSIX class names | `[[:alpha:]]` vs `a` | `true` | `false` | `[:` at the start of a class |
| Brace nesting depth | — | nested | capped | More than one open brace |
| More than ten brace groups | `{a,b}` × 11 vs `b` × 11 | `true` | validation error | More than ten groups in a pattern |
| Brace expansion splits inside a class | `{,[a,b]}[c]` vs `a` | `true` | `false` | Any comma inside a class at brace depth > 0 (member or range endpoint) |

Version 1.1.2 fixes the former class-versus-separator defects (`fastglob-037`
and `fastglob-038`) and the comma-after-braces defect (`fastglob-039`, reported
upstream as [issue 165](https://github.com/oxc-project/fast-glob/issues/165)).
The corpus now asserts agreement on those cases; Ferralk's verdicts are
unchanged. The classifier retains their conservative exclusions, so this
reference update does not also expand the differential subset.

The ten-group limit used to return `false` silently for a pattern that matches.
Version 1.1.2 rejects it through `validate` with `TooManyBraceGroups` instead.
The oracle adapter checks that exact validation error for the named eleven-group
case and still checks Ferralk's successful match; it does not assert a matcher
verdict that the reference now documents as unspecified for invalid patterns.
The cap counts groups, not combinations: one group of two thousand alternatives
is answered correctly. The original finding is upstream
[issue 166](https://github.com/oxc-project/fast-glob/issues/166).

The brace-backtracking row records the two scheduled failures on issue #447,
replayed against 1.1.2. Both reduce to the same reference defect: a brace branch
changes the matcher's starting index, but backtracking can revisit an earlier
attached star run. That run is then misclassified as a whole-component
recursive wildcard. `**/a**/{}` accepts `a/a` in the reference even though
expanding the empty group gives `**/a**/`, which requires a final separator.
The corpus preserves both original byte inputs and the reduced example. The
classifier excludes only the structural combination that can reach this bug;
single or odd attached star runs, escaped stars, class members, patterns without
a recursive prefix, and patterns without a later brace remain comparable.

Until issue #419 the recursive-wildcard exclusion covered almost every `**`:
Ferralk let a `**` anywhere cross separators and let `**/` hand over inside a
component, so `**/a` accepted `aa` (`fastglob-034`). Since
[ADR-0020](adr/0020-double-star-only-as-a-whole-component.md) both engines read
`**` as recursive only when it is a whole path component and as ordinary stars
anywhere else, so whole and attached runs remain shared outside the listed
reference boundaries. The classifier judges a star run once, at its first star, and reads
escapes and classes before recognizing one, so an escaped or class-member `**`
is not rejected as recursive syntax.

Brace expansion happens before matching, so an alternative can concatenate
with the surrounding text into a `**` that only Ferralk reads as one run
(`{*}*` vs `/`). A star next to brace punctuation is therefore excluded too.
For the same reason, the `./` exclusion checks the expanded alternatives: an
empty brace arm can expose a later prefix (`{x,}./`), while a dot arm can join
the following separator (`{.}/`).

The negation, trailing-`**`, star-run, escaping, and POSIX rows hold for
`is_match` as well and are corpus candidates. Leading `./` normalization is
specific to the component-local policy.
