# ADR-0014: Own gitignore rule matching over ferralk-glob

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

ADR-0006 settled two questions at once: Git is normative for ignore semantics,
and the `ignore` crate's `Gitignore` produces the verdicts. The first has held.
The second has been overtaken.

- **The borrowed engine cannot reach the oracle.** `globset`, which `ignore`
  matches with, has no notion of POSIX class names inside a bracket expression;
  its class parser reads negation and ranges only. Git reads them, so
  `*.[[:digit:]]` ignores `a.7` for the oracle and not for the walker. The
  corpus records that as `ignore-034`, the last entry in `KNOWN_WALKER_GAPS`,
  and it cannot be closed from our side of the dependency. ferralk-glob has
  supported POSIX classes since M1.
- **The evaluation layer is already ours.** Per-directory chains, the verdict a
  subtree carries down, `.git/info/exclude`, and reading each ignore file
  exactly once per walk all live in `crates/ferralk/src/gitignore.rs`. What is
  left of the dependency is parsing a rule line and one `matched()` call.
- **The safety net that justified borrowing is now ours too.** ADR-0006 leaned
  on a battle-tested engine. Today 46 ignore cases replay against a hermetic
  `git check-ignore`, the walker replays the same cases end to end, and the
  matcher is differentially fuzzed. Correctness is held by evidence we own and
  run, not by the reputation of a crate.
- **Dependency weight.** `ignore` is what pulls the regex stack into the walker:
  `globset`, `regex-automata`, `regex-syntax`, `aho-corasick`, `bstr`, `log`,
  `walkdir`, `same-file`. ADR-0013 already refused a regex engine in the match
  path; it is still in the tree, one level down, for ignore rules.

## Decision

Git remains normative. `git check-ignore` stays the corpus oracle for every
ignore verdict, unchanged from ADR-0006 — only the engine behind the verdict
changes.

ferralk parses gitignore rules itself and matches them with ferralk-glob. A
rule line becomes negation, anchoring, directory-only and a pattern body per
`gitignore(5)`; the body compiles to a ferralk-glob pattern with hidden files
matchable, because ignore rules see dotfiles. The evaluation layer above it is
untouched: this replaces the matcher inside the existing structure, not the
structure.

`ignore` leaves the walker's dependencies once the new layer is green, and
stays a dev-dependency for the differential test that compares the two engines
over generated rule and path pairs.

## Consequences

- `ignore-034` becomes fixable, and the gap list can empty.
- The walker's dependency tree goes from 15 crates to 6: `crossbeam-deque` and
  its two `crossbeam` crates, `ferralk-glob`, and `memchr`. The regex stack
  leaves entirely.
- Rule translation is the new risk surface, and it is a real one: anchoring
  (a slash anywhere but the end binds the rule to its own directory), trailing
  spaces that only count when escaped, trailing backslashes, `**` inside rules,
  escapes, and the directory-only slash. These are exactly the corner cases
  ADR-0006 named as the reason to have an oracle at all — which is why the
  corpus, the `git check-ignore` runner, and a differential test against the
  engine being replaced all gate the change rather than follow it.
- ferralk-glob gains a second caller shape. A matcher change now moves ignore
  verdicts as well as glob results, and the ignore corpus becomes part of what
  guards it.
- One more thing to maintain when Git changes. That was already true for the
  evaluation layer; it now covers the rules too.
