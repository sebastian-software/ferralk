# zlob fnmatch test coverage

This document records the line-addressable coverage boundary for the frozen
zlob v1.6.3 source commit
`4bc4da2cbc823d3911b4a1436448687c398977dd`, specifically
`test/test_fnmatch.zig`.

## Direct matcher cases

Every `testing.expect` assertion that invokes `fnmatchFull` or the
`matchExtglob` helper is represented by a matching `fnmatch-lNNNN` case in
[`corpus/fnmatch.jsonl`](../corpus/fnmatch.jsonl). This includes the final
POSIX bracket-expression and `noescape` assertions at lines 560–618. The
Corpus IDs intentionally preserve the source line, including duplicate input
assertions, so an import is one-to-one rather than merely behaviourally
deduplicated.

The manual oracle verifies those cases against the pinned zlob crate. The
normal harness verifies them without Zig.

The additional direct matcher assertions in `test/test_edge_cases.zig` are
represented in [`corpus/edge-cases.jsonl`](../corpus/edge-cases.jsonl), with
that file's three path-list assertions added to `match-paths.jsonl`.

The source-level Extglob path-list scenarios in `test/test_extglob.zig` are
represented in [`corpus/extglob-suite.jsonl`](../corpus/extglob-suite.jsonl).

## Syntax preflight cases

The flag-sensitive `hasWildcards` assertions are represented by
[`corpus/preflight.jsonl`](../corpus/preflight.jsonl) records with
`"kind":"has_wildcards"`. They cover always-active basic markers, brace
markers only with `braces`, and extglob operators only with `extglob`. The
harness and manual oracle run the dedicated preflight operation rather than
pretending that an empty candidate is a full fnmatch assertion.

## Path-list cases

The five `matchPaths` assertions are represented by
[`corpus/match-paths.jsonl`](../corpus/match-paths.jsonl) records and replayed
through `Pattern::filter_paths`. The empty-list `NOCHECK` result is retained as
a disputed corpus case: Ferralk returns no caller-owned paths, while zlob's
frozen Zig suite returns the pattern. zlob 1.6.3's Rust FFI aborts for an empty
list and exposes corrupted string data for this synthetic result, so its manual
Rust oracle deliberately skips that one case while replaying the other four.
Ferralk's list API intentionally preserves caller input order, while zlob's
default `matchPaths` result is sorted; the affected edge case records both
orders explicitly.

## Deliberate non-matcher exclusions

The remaining assertions are not direct `fnmatch` semantics and have no
equivalent public Ferralk type today:

| Source lines | zlob surface | Ferralk disposition |
| --- | --- | --- |
| 338–385 | `PatternContext` template classification | Internal zlob optimization detail; Ferralk exposes a compiled `Pattern`, not a template-inspection API. |
| 438–448 | private SIMD index helpers | Implementation detail only; ADR-0008 reserves any equivalent optimization for profiling-backed work. |

These exclusions are intentionally not counted as corpus coverage. They keep
the broad M0 “whole zlob test suite” item open until a compatible corpus model
or a formal scope decision exists, while making the direct matcher boundary
auditable rather than implicit.
