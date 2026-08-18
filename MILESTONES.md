# ferralk Milestone Plan

Working backlog for the port. Check items off as they land; one item should
roughly correspond to one PR. A milestone is done when every box is checked
and its exit criterion holds. Architecture: [RFC](RFC-zig-free-zlob-port.md);
individual decisions: [ADRs](docs/adr/README.md).

Milestones M0–M4 are sequential; M5 starts only after 1.0 (ADR-0010).

---

## M0 — Foundation (RFC Phase 0, ~1–2 weeks)

**Exit criterion:** the corpus replays in CI without a Zig toolchain, and
disputed or undefined zlob semantics are captured as flagged corpus cases.

- [x] Scaffold the Cargo workspace: `ferralk-glob`, `ferralk`, plus
      unpublished `corpus`, `harness`, `bench`, and `oracle` (excluded from
      default members) — ADR-0003
- [x] Add LICENSE (MIT) and NOTICE with zlob attribution — ADR-0001
- [x] Set up the Ferroni repo blueprint: CI for Linux/macOS/Windows, dedicated
      MSRV job, release-please, renovate, codecov, CodSpeed — ADR-0004,
      ADR-0012
- [x] Define the corpus schema: JSONL layout, `\xNN` byte-escape codec
      (encoder/decoder in Rust), JSON Schema documentation — ADR-0007
- [x] Inventory the zlob 1.6.3 Rust and C APIs and all flags; write the
      compatibility matrix document
- [ ] Port zlob's own test suite 1:1 into the corpus — ADR-0007
- [x] Build the `oracle` crate (zlob 1.6.3 as dev-dependency) with a manually
      triggered corpus-regen workflow — ADR-0007
- [x] Wire `fast-glob` (oxc) into the harness as second reference for the
      common syntax subset — ADR-0007
- [x] Build the `git check-ignore` oracle runner for ignore cases — ADR-0006
- [x] Record disputed/undefined semantics as corpus cases with a `disputed`
      flag
- [ ] Send the courtesy notice to the zlob maintainer — ADR-0001

## M1 — Matcher port (RFC Phase 1, ~3–5 weeks)

**Exit criterion:** `ferralk-glob` passes the full matcher corpus; portable
matcher ≤1.5x zlob median, ≤1.25x `fast-glob` median on the common subset.

- [ ] Mechanical port: pattern tokenizer/parser (provenance headers) —
      ADR-0002
- [ ] Mechanical port: core wildcard and `**` matching
- [ ] Mechanical port: character classes (incl. `[!...]`/`[^...]`, ranges,
      escapes)
- [ ] Mechanical port: brace expansion (nested alternatives)
- [x] Mechanical port: extglob operators
- [x] Leading-period rules, case-folding option, escape handling
- [x] Public API: `Pattern::compile`, `PatternOptions`, `is_match`,
      `validate`; byte-first with `&str` convenience — ADR-0005
- [x] Matcher corpus green (all topic files except `ignore.jsonl`)
- [x] Run differential generation against the oracle; triage every
      disagreement into the corpus — ADR-0007
- [x] Property tests (literal-only patterns, subset/superset invariants)
- [x] Fuzzers for parser and matcher with seeded corpora
- [ ] Refactor the ported code toward the immutable IR (after corpus green) —
      ADR-0002
- [ ] Profile, then apply memchr/memmem hot-path primitives — ADR-0008
- [ ] Matcher benchmarks vs zlob, `fast-glob`, and `globset` in CodSpeed;
      verify both budgets — ADR-0013
- [ ] Publish `ferralk-glob` 0.1

## M2 — Portable walker (RFC Phase 2, ~3–4 weeks)

**Exit criterion:** production-usable serial walker on `std::fs`, native path
preservation, structured errors.

- [x] Backend trait + portable `std::fs::read_dir` backend
- [x] `Walker` builder: include/exclude, `WalkOptions`, POSIX-conservative
      defaults — ADR-0011
- [ ] Conservative prune planner: literal roots, whole-subtree excludes,
      extension filters, negation guard
- [x] Native path preservation end-to-end (`Path`/`PathBuf`, no lossy
      conversion) — ADR-0005
- [x] Error model: `ErrorPolicy::{Abort,Skip,Collect}` with `Collect` default
- [x] Symlink policy and cycle detection
- [x] Optional sorting and metadata collection
- [x] Streaming iterator with cancellation; `collect()` result shape
- [ ] Filesystem fixture builders + integration tests (unreadable dirs,
      disappearing files, non-UTF-8 names)
- [ ] Publish `ferralk` 0.1 (serial)

## M3 — Ignore semantics & parallel scheduler (RFC Phase 3, ~3–5 weeks)

**Exit criterion:** single- and multi-thread walks produce identical result
multisets across the corpus; no hangs or lost errors under stress; walker
faster than parallel `ignore` + pruning on all traversal corpora.

- [ ] Nested ignore chains via the `ignore` crate's rule matcher;
      per-directory nodes with shared-parent caching — ADR-0006
- [ ] Negation-aware pruning guard (never prune when a later rule may
      re-include)
- [ ] `ignore.jsonl` corpus green against `git check-ignore`
- [ ] Scheduler: `crossbeam-deque` work stealing, lazy worker spawn,
      per-worker scratch and result shards — ADR-0009
- [ ] Single cancellation state, lossless error channel, documented panic
      propagation
- [ ] Loom models for queue and completion state
- [ ] Stress tests: empty/shallow/imbalanced trees, visitor panic,
      worker-start failure
- [ ] Invariant test: parallel == serial result multiset
- [ ] Walker benchmarks vs parallel `ignore` and zlob in CodSpeed; verify the
      1.0 gate

## M4 — Stabilization & 1.0 (RFC Phase 5, ~2–4 weeks)

**Exit criterion:** 1.0 published, portable on all platforms, Windows tier-2
CI green — ADR-0010.

- [ ] Compatibility guide: zlob API mapping and all deliberate divergences
- [ ] Downstream trial: integrate into Palamedes, fold feedback back
- [ ] MSRV and feature audit; API review (cargo-semver-checks in CI)
- [ ] Dependency and unsafe audit (expected: zero unsafe before M5)
- [ ] Oracle retirement check: corpus is self-sufficient, Zig-free CI
      confirmed — ADR-0007
- [ ] Publish benchmarks
- [ ] Release `ferralk-glob` 1.0 and `ferralk` 1.0

## M5 — Native backends (RFC Phase 4, post-1.0, ~6–12 weeks)

**Exit criterion per backend:** safety review passed, portable/native parity
on the corpus, within 20% of zlob median on that platform, p95 regression
≤35% — ADR-0010. Backends ship as feature-gated 1.x releases.

### macOS (first)

- [ ] `getdirentries64` backend behind a feature flag
- [ ] `getattrlistbulk` batch metadata with capability detection and portable
      fallback (SMB/FUSE)
- [ ] Bounds-checked record parsing with module-level invariants; fuzz the
      record decoder
- [ ] Differential tests portable vs native; sanitizers in CI
- [ ] Benchmark gate on macOS

### Linux (second)

- [ ] Batched `getdents64` backend behind a feature flag
- [ ] `statx` only where metadata is requested
- [ ] Bounds-checked record parsing; fuzz the record decoder; Miri where
      executable
- [ ] Differential tests portable vs native; sanitizers in CI
- [ ] Benchmark gate on Linux

## Implementation log

### 2026-08-18 — M0 foundation established

- Completed the workspace boundary, license/NOTICE, repository automation, and
  corpus contract. The normal Zig-free member tests, `cargo run -p harness --
  corpus`, and formatting checks pass locally.
- Added a living compatibility matrix seeded from zlob's public README. It is
  deliberately labelled provisional until the frozen source is verified.
- Resolved the zlob reference: annotated tag `v1.6.3`
  (`b757d57963cbf578aacfee4635c0305ded615417`) peels to source commit
  `4bc4da2cbc823d3911b4a1436448687c398977dd`; the package version, MIT
  attribution, Rust/C APIs, flags, and test-suite paths are recorded in
  [`docs/zlob-1.6.3-reference.md`](docs/zlob-1.6.3-reference.md). This removes
  the prior source-coordinate blocker.
- **Blocker — external actions:** sending a maintainer courtesy notice and
  configuring repository-side Codecov/CodSpeed credentials require maintainer
  authority. The committed workflows are ready; no external message or
  repository setting has been changed.

### 2026-08-18 — M1 matcher baseline started

- Added a safe, byte-first compiled matcher for literals, `*`, `?`, explicit
  recursive `**`, character classes, backslash escapes, leading-period rules,
  and ASCII case folding. The public `Pattern` / `PatternOptions` API is
  intentionally usable now, but its milestone item remains open until braces,
  extglobs, byte-platform parity, and corpus verification are complete.
- The harness now replays non-disputed matcher cases, not just their JSON
  structure. It validates duplicate IDs and rejects unknown feature flags.
- The first source-backed replay corrected a bootstrap assumption: zlob's
  in-memory matching lets `*` cross separators and treats a trailing escape as
  a literal backslash. Ferralk's distinct leading-period policy is recorded as
  a deliberate divergence below; every behaviour has source provenance in the
  corpus.
- Nested and empty brace alternatives are compiled into immutable matcher
  branches. The implementation is covered by direct unit tests and initial
  source-provenanced corpus cases; the mechanical-port checkpoint stays open
  until its provenance review and the remaining upstream brace cases are
  imported under the corpus-port item.
- The manually executed zlob 1.6.3 oracle established the first deliberate
  divergence: its direct `*` matcher includes `.gitignore` without
  `ZLOB_PERIOD`; ferralk's default excludes it under ADR-0011. The corpus now
  records both expectations (`expected` and `oracle_expected`) and replays
  each with the proper engine.
- The manual Oracle workflow now installs Zig 0.16 and runs an ignored test
  against the pinned `zlob = 1.6.3` dev-dependency. It is verified locally;
  normal CI never compiles that dependency and remains Zig-free.
- Added ASCII POSIX character classes (`[:alpha:]`, `[:digit:]`, and the
  remaining standard byte classes) to the compiled matcher. Initial imported
  cases pass both the Zig-free harness and the zlob oracle; the mechanical-port
  checklist entry remains open pending the complete upstream class suite.
- Added an isolated `git check-ignore --no-index` runner and a seed
  `ignore.jsonl` with ordinary, negated, and directory-rule cases. Its normal
  harness integration verifies that Git remains the normative source before
  the M3 `ignore`-crate matcher is introduced.
- Pinned Oxc's `fast-glob` 1.1.0 as the second matcher reference and replayed
  an explicitly scoped common subset in the harness. Its independent oracle
  expectation shares the same corpus mechanism used for zlob divergences.
- Began a line-addressable 1:1 import of `test/test_fnmatch.zig` into
  `fnmatch.jsonl`. Case IDs retain upstream line numbers, so each imported
  assertion has a stable source location; the remaining upstream groups are
  deliberately still open under the corpus-import item.
- The first import block additionally found two public-API differences in
  zlob itself: `zlob_match_paths` does not mirror the internal
  `fnmatchFull` tests for an escaped backslash or an escape before a regular
  literal. Both source assertions remain Ferralk's expected results; their
  direct-API results are retained as explicitly disputed oracle expectations,
  rather than being silently normalized away.
- Extended that import through the simple class, combined wildcard, and
  real-world pattern sections. The checked-in `fnmatch.jsonl` now contains
  98 individually source-line-addressable assertions, replayed by both the
  Zig-free harness and the manual zlob oracle.
- Added the full POSIX class block plus `noescape` option coverage, bringing
  `fnmatch.jsonl` to 127 source-line-addressable assertions. The strict byte
  codec rejected two malformed JSON escapes during import; both were corrected
  before replay and are covered by the existing codec validation path.
- Ported zlob's non-nested extglob evaluator for `@()`, `?()`, `*()`,
  `+()`, and `!()`-style negation, preserving malformed extglobs as
  literals. The complete upstream extglob block is now source-line-addressable
  in `fnmatch.jsonl`; it brings that file to 175 assertions and passes both
  the Zig-free harness and the pinned zlob oracle.
- Started M2 with a safe serial `std::fs::read_dir` backend behind an internal
  backend trait. `Walker` now has include/exclude compilation, explicit
  sorting and symlink options, root-relative byte matching, and
  `ErrorPolicy::{Abort,Skip,Collect}` (`Collect` by default). It preserves
  public `PathBuf` values and has fixture tests for filtering, error policy,
  sorting, symlink cycle de-duplication, and non-UTF-8 names on Linux.
- **Platform test note:** the current macOS fixture filesystem rejected an
  invalid UTF-8 filename with `EPERM`; the native-name integration assertion
  is therefore Linux-gated, where that representation is supported. This does
  not block the portable byte-preserving implementation or Linux CI coverage.
- Extended the M0 1:1 corpus import with the direct brace-expansion assertions
  from `test/test_brace.zig`. `braces.jsonl` now has 41 source-linked cases
  across basic, wildcard, recursive, empty, literal, and multi-group brace
  forms; both the harness and pinned zlob oracle replay the new block.
- Added the first conservative M2 prune rule: only excludes ending in `/**`
  may close a directory subtree, and only when their derived root matcher
  accepts that directory. Ordinary suffix or wildcard excludes never prune.
  Literal include roots, extension prefilters, and the M3 ignore-negation guard
  remain explicit open work under the prune-planner item.
- Expanded the direct brace import through zlob's complex path, character-class,
  no-match, and long-alternative assertions. `braces.jsonl` now contains 57
  source-linked cases; the further brace tests are filesystem/API fixtures and
  remain queued for the M2 walker corpus layer.
- Added deterministic exhaustive property tests for literal-only byte patterns
  and for wildcard subset invariants (`?` and `a*` are subsets of `*`
  with leading-period matching explicitly enabled). These complement, rather
  than replace, the source-backed corpus tests.
- Added independent Cargo-Fuzz parser and matcher targets, seeded with corpus
  syntax, plus a manual nightly workflow with an explicit time budget. Both
  targets compile with Rust 1.93. **Local tooling note:** `cargo-fuzz` is not
  installed in this workspace, so no local mutational fuzz run was possible;
  this does not block the checked-in targets or their CI execution path.
- Added a deterministic differential generator for the shared literal,
  `*`, and `?` core. It compares all patterns over that alphabet through
  length four against all candidate paths through length four (10,571 direct
  zlob comparisons) and reports any pair that requires corpus triage. The
  manual oracle workflow now runs it alongside checked-in corpus replay.
- Added a cloneable cooperative `CancellationToken` to the serial Walker.
  A cancellation request yields a clearly flagged partial `WalkResult`
  without manufacturing an I/O error; the streaming iterator portion of the
  M2 cancellation item remains open.
- Imported zlob's nested, deeply nested, and mixed brace-expansion assertions.
  `braces.jsonl` now contains 68 source-linked cases and passes both the
  Zig-free harness and the pinned zlob oracle.
- Added opt-in metadata collection to `WalkOptions`. Returned entries retain
  `std::fs::Metadata` only when requested, so the default traversal incurs no
  metadata syscall; sorted fixture tests verify both modes and file length.
- Added `Walker::stream()`, an incremental unsorted iterator that carries the
  same filters, conservative pruning, symlink cycle guard, optional metadata,
  error policy, and cooperative cancellation as `collect()`. Streaming errors
  are yielded as items under `Collect`; global sorting intentionally remains a
  collect-only operation.
- Began M3's ignore-engine foundation by adding the Rust `ignore` crate,
  whose `Gitignore` matcher is the normative engine selected by ADR-0006.
  Nested rule-chain propagation and negation-aware pruning remain deliberately
  open until their precedence can be tested end-to-end against the Git corpus.
