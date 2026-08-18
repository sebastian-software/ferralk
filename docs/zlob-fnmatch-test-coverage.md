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

## Deliberate non-matcher exclusions

The remaining assertions are not direct `fnmatch` semantics and have no
equivalent public Ferralk type today:

| Source lines | zlob surface | Ferralk disposition |
| --- | --- | --- |
| 274–331 | `matchPaths` result shaping, including `NOCHECK` | Deferred walker result-policy review; Rust callers own result buffers and Ferralk deliberately has no C-style `NOCHECK` output shaping. |
| 338–385 | `PatternContext` template classification | Internal zlob optimization detail; Ferralk exposes a compiled `Pattern`, not a template-inspection API. |
| 390–435 | `hasWildcards` / flag-sensitive preflight helpers | No public Ferralk analogue; `Pattern::compile` and `validate` are the supported preflight API. |
| 438–448 | private SIMD index helpers | Implementation detail only; ADR-0008 reserves any equivalent optimization for profiling-backed work. |

These exclusions are intentionally not counted as corpus coverage. They keep
the broad M0 “whole zlob test suite” item open until a compatible corpus model
or a formal scope decision exists, while making the direct matcher boundary
auditable rather than implicit.
