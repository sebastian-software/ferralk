# ADR-0020: `**` is recursive only as a whole path component

- **Status:** Accepted
- **Date:** 2026-09-24

## Context

With `recursive_double_star` enabled, Ferralk made every `**` run recursive,
wherever it stood, and let a `**/` prefix hand over in the middle of a
component. So `**/x` matched `sx`, `a/**/b` matched `a/xb`, and a walker
exclude `**/node_modules/**` pruned `my_node_modules`. The typical JavaScript
exclude list therefore silently dropped unrelated directories whose names
merely ended in the literal (issue #419).

gitignore, Bash `globstar`, `globset`, and Oxc `fast-glob` all read `**` as
special only when it is a whole path component. The corpus recorded the
`fast-glob` disagreement as `fastglob-034`, marked as an oracle defect.

zlob 1.6.3 does not have one reading to be faithful to. Its surfaces disagree
with one another, and its tests and docs describe the whole-component reading
that its filesystem glob does not implement:

- **`matchPaths`** (the surface the corpus oracle replays). It splits a pattern
  on `/` once it contains `**` (`src/compiled_pattern.zig:51`). Only a segment
  that is exactly `**` is recursive (`:258`), so `**/x` already refuses `sx`
  there. Every other segment is matched on its own, inside one component:
  `a**/y` refuses `a/x/y` and `**.ts` refuses `src/a.ts`. A pattern without a
  `**` segment is matched against the whole path, where `*` crosses
  separators (`:317-321`). So `*.ts` accepts `src/a.ts` while `**.ts` does not.
- **The filesystem glob** looks for the first `**` substring anywhere
  (`src/zlob.zig:825`). It then walks `a**/b` as `a/**/b`, and can drop the
  literal next to the run. Partial `**` crosses separators there. A relative
  pattern and its absolute spelling can select different entries.
- **The gitignore module** treats a whole segment of two or more stars as
  recursive (`src/path_matcher.zig:32-34`), which is Git's reading.
- **Docs and tests.** The Rust binding documents `**` as "Matches zero or more
  path components" (`rust/src/lib.rs:139`). The only zlob tests of a partial
  `**` are gitignore tests, and they assert the whole-component rule: "A
  segment that merely *contains* stars is NOT a doublestar"
  (`test/test_gitignore.zig:239-245`, with `***` behaving as `**` at
  `:221-229`). No `matchPaths` or filesystem-glob test covers a partial `**`
  or a candidate like `sx`. The CLI help (`src/main.zig:73-74`) describes `**`
  as any sequence including `/`, which only the filesystem glob comes close
  to.

## Decision

Maintainer decision of 2026-09-24: parity with gitignore, `fast-glob`,
`globset`, and Bash `globstar` outweighs zlob parity. Before 1.0, `**` becomes
recursive only as a whole path component. The rule is one classification that
the tokenizer and the extglob interpreter share:

- A star run is **whole-component** when it is bounded on each side by an
  unescaped `/` or by an end of the pattern. Examples: `**`, `**/x`, `x/**`,
  `x/**/y`. An escaped `\/` is a literal byte, not a pattern separator, so it
  bounds nothing.
- Braces expand first, so each alternative is judged as the whole pattern it
  expands to. `{**,x}/y` has a recursive `**/y`; `a{**,x}/y` has an ordinary
  `a**/y`.
- An extglob alternative stands where its group stands. Its start is a
  component boundary only when the group operator is. Its end is one only
  when the closing parenthesis is. `@(**)/y` is recursive; `x@(**)/y` and
  `@(**)y` are not.
- A whole-component `**/` consumes zero or more whole components, each with
  its separator. It may hand over only at a component start, so `**/x`
  matches `x` and `a/x` but not `sx`, and a trailing `a/**/` needs the final
  separator. A whole-component `**` at the end consumes the rest of the path,
  and `x/**` still accepts `x`.
- A whole-component run of three or more stars stays recursive, with the
  token shape it had before this decision.
- Every other run (`a**`, `**b`, `a**/b`, `**.ts`) is ordinary stars, read
  exactly as with `recursive_double_star` disabled:
  - component-local under `is_match_glob_path`;
  - the position rule under `is_match_path`: only a wildcard directly behind a
    separator is local;
  - separator-crossing like any `*` under `is_match`.

## Consequences

- `fastglob-034` flips: `fast-glob` and Ferralk agree on it. The
  `ferralk_vs_fast_glob` fuzz subset now admits every whole-component `**`,
  except two shapes that still differ: the trailing `/**` elision
  (`fastglob-033`) and whole-component runs of three or more stars. It also
  admits every attached run.
- Walker includes and excludes select and prune by whole components.
  `**/node_modules/**` no longer covers `my_node_modules`.
- The zlob verdicts that now differ are recorded in `globstar-*` with
  `adr: "0020"`. They come from zlob's segment-wise `matchPaths`:
  - an attached run crossing separators under `is_match` (`a**/y` against
    `a/x/y`, `**.ts` against `src/a.ts`);
  - the list filter's attached `a**/y`;
  - recursive `**` inside a whole-component extglob group (`@(**)/y`,
    `@(**/x|z)`);
  - a trailing `**/` without its separator (`a/**/` against `a/b`).

  None of the previously recorded zlob verdicts changed.
- The consumer-visible contract changes before 1.0; the compatibility guide's
  contract-change audit carries the entry.
