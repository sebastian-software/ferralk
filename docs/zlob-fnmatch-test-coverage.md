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

The public in-memory API scenarios in `test/test_path_matcher.zig` are
represented in [`corpus/path-matcher.jsonl`](../corpus/path-matcher.jsonl).
They cover ordinary and base-relative path filtering, full-path preservation,
input-order index results, brace and recursive patterns, leading-dot policy,
absolute paths, and `./` normalization. The harness executes the corresponding
`Pattern::{filter_paths,filter_paths_at,filter_path_indices,filter_path_indices_at}`
operations, and the manual oracle invokes the matching zlob Rust APIs.

The root-independent compositional path-list forms from
`test/test_absolute_paths.zig` are recorded in that same corpus: multiple
brace components, brace-plus-suffix expansion, and brace-prefixed recursive
selection. Its root-independent wildcard, class, brace, recursive, and literal
component forms are included as well, along with non-empty-branch Extglob
composition. Filesystem-only fixture setup remains outside this in-memory API
coverage boundary; documented C/Rust disagreements for empty Extglob branches
and `PERIOD` hidden-name filtering stay flagged in the corpus.

One remaining absolute-fixture assertion, `{.a,a,b}.c` without `PERIOD`, is
intentionally not represented as `match_paths`: zlob's filesystem iterator
hides its literal dot-prefixed brace alternative, while the in-memory surface
does not model directory enumeration. It remains a C traversal-surface
difference, not an unrecorded matcher gap.

`test/test_internal.zig` additionally supplies eleven public `matchPaths`
scenarios in the same corpus, covering wildcards, classes, literal and empty
inputs, component paths, and filename punctuation. Its SIMD-helper assertions
remain implementation-private and are excluded below.

[`zlob-test-suite-audit.md`](zlob-test-suite-audit.md) records the wider frozen
test-tree boundary, including C ABI, loader, and system-runtime exclusions.

The anchored, recursive, and allowlist examples from `test/test_gitignore.zig`
are represented in [`corpus/ignore.jsonl`](../corpus/ignore.jsonl). Their
source provenance remains zlob, but the `expected` value is deliberately
recorded as `git_check_ignore`: ADR-0006 makes Git, rather than zlob's private
GitIgnore implementation, Ferralk's normative ignore oracle.

The recursive, anchored, and brace-filtered traversal block in
`test/test_walk.zig` is covered by the Walker regression fixture. In
particular, terminal `src/**` matches `src` itself and all of its descendants,
matching zlob's zero-component recursive-wildcard behaviour.

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
| `test/test_path_matcher.zig` C-string chunking and private component limits | ABI-owned output buffers and private fixed-capacity detail | Ferralk returns native `Vec` values and intentionally has no fixed zlob component limit; the public list and index contracts are corpus-covered. |
| `test/test_absolute_paths.zig` lines 697–719 | Filesystem iterator handling of a literal hidden brace alternative | Ferralk's in-memory matcher cannot express the enumerator gate; the C/Rust distinction is documented above rather than misrepresented as `match_paths`. |

These exclusions are intentionally not counted as corpus coverage. They keep
the broad M0 “whole zlob test suite” item open until a compatible corpus model
or a formal scope decision exists, while making the direct matcher boundary
auditable rather than implicit.
