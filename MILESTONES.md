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
- [x] Profile, then apply memchr/memmem hot-path primitives — ADR-0008
- [ ] Matcher benchmarks vs zlob, `fast-glob`, and `globset` in CodSpeed;
      verify both budgets — ADR-0013
- [ ] Publish `ferralk-glob` 0.1

## M2 — Portable walker (RFC Phase 2, ~3–4 weeks)

**Exit criterion:** production-usable serial walker on `std::fs`, native path
preservation, structured errors.

- [x] Backend trait + portable `std::fs::read_dir` backend
- [x] `Walker` builder: include/exclude, `WalkOptions`, POSIX-conservative
      defaults — ADR-0011
- [x] Conservative prune planner: literal roots, whole-subtree excludes,
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

- [x] Nested ignore chains via the `ignore` crate's rule matcher;
      per-directory nodes with shared-parent caching — ADR-0006
- [x] Negation-aware pruning guard (never prune when a later rule may
      re-include)
- [x] `ignore.jsonl` corpus green against `git check-ignore`
- [x] Scheduler: `crossbeam-deque` work stealing, lazy worker spawn,
      per-worker scratch and result shards — ADR-0009
- [x] Single cancellation state, lossless error channel, documented panic
      propagation
- [x] Loom models for queue and completion state
- [x] Stress tests: empty/shallow/imbalanced trees, worker panic,
      worker-start failure
- [x] Invariant test: parallel == serial result multiset
- [ ] Walker benchmarks vs parallel `ignore` and zlob in CodSpeed; verify the
      1.0 gate

## M4 — Stabilization & 1.0 (RFC Phase 5, ~2–4 weeks)

**Exit criterion:** 1.0 published, portable on all platforms, Windows tier-2
CI green — ADR-0010.

- [x] Compatibility guide: zlob API mapping and all deliberate divergences
- [ ] Downstream trial: integrate into Palamedes, fold feedback back
- [x] MSRV and feature audit; API review (cargo-semver-checks in CI)
- [x] Dependency and unsafe audit (expected: zero unsafe before M5)
- [x] Oracle retirement check: corpus is self-sufficient, Zig-free CI
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
- Added opt-in root `.gitignore` matching to both `collect()` and
  `stream()`, backed directly by `ignore::Gitignore`. A fixture verifies
  ordinary ignore and `!` re-inclusion; nested per-directory chains and their
  pruning guard remain open M3 work.
- Extended that matcher to evaluate each ancestor `.gitignore` in precedence
  order, so a nested `!` rule can re-include a file excluded by the root.
  The fixture covers both collect and stream. This correctness-first form
  rebuilds matchers on demand; shared-parent caching remains open M3 work.
- Added the M3 negation-aware prune guard: a Git-ignored directory is not
  emitted but is still descended, preventing a nested `.gitignore` from
  losing a later `!` re-inclusion. The fixture exercises that case for both
  collect and stream.
- Added a per-traversal cache of parsed `.gitignore` matchers keyed by their
  directory. It removes repeated parsing while preserving ancestor precedence;
  the stronger immutable shared-parent node representation remains open.
- Added a Walker-level replay of every `ignore.jsonl` case. Each fixture
  writes the same rule chain that the existing `git check-ignore` oracle
  validates, then asserts the Walker's returned-path verdict, tying the Git
  corpus directly to M3 traversal behaviour.
- Added public M2 integration fixtures for disappearing roots in both
  `collect()` and `stream()`, plus a Linux-gated non-UTF-8 path assertion.
  Unreadable-directory behaviour is still pending a permission-stable fixture
  strategy across CI platforms.
- Began the M3 scheduler with a tested `crossbeam_deque::Injector` transfer
  into a worker-local FIFO queue, now used by the serial walk dispatcher. Lazy
  worker spawning, directory task fan-out, result shards, and all multi-thread
  correctness invariants remain open; the scheduler checkbox is intentionally
  not marked complete.
- Replaced the flat per-directory GitIgnore cache with immutable directory
  nodes that retain and share their cached parent chain. Nested precedence and
  re-inclusion continue to replay through the corpus, while a direct unit test
  verifies that sibling nodes reuse the same parent allocation.
- Added an opt-in `Walker::threads` limit and a portable parallel `collect()`
  path. The caller processes the root first, then lazily starts helpers only
  when directory work exists; workers use local FIFO queues, the shared
  injector, stealing, shared cancellation, and lossless mutex-protected error
  collection. A sorted, imbalanced-tree fixture proves the one-worker and
  four-worker result multisets agree. Result shards, loom models, and the
  broader stress suite remain open.
- Added a repeated scheduler stress fixture for empty, shallow, and strongly
  imbalanced directory trees. Thirty-two eight-worker traversals each match a
  serial sorted baseline; worker-start-failure and panic cases remain tracked
  separately before the aggregate stress-test milestone can close.
- Added Loom models for the scheduler's active-task hand-off and terminal
  completion transition. They ensure a helper cannot mistake an empty queue
  for completion while the root is creating a child task, and that exactly one
  worker observes the final completion transition.
- Unified parallel cancellation around the caller-provided
  `CancellationToken` (or one private token when none is supplied), including
  I/O-abort and panic cleanup. Worker panics are joined and resumed on the
  caller thread; concurrent dangling-symlink metadata failures are retained
  and checked against the serial error set.
- Moved successful parallel entries into per-worker result shards and merge
  them after every helper joins. The scheduler now satisfies its queue,
  stealing, lazy-spawn, worker-scratch, and result-shard checkpoint; the
  central synchronized channel is intentionally reserved for errors.
- Completed the direct `fnmatchFull`/`matchExtglob` assertion import from the
  frozen zlob test file, including six previously omitted duplicate and
  negative escape cases. `docs/zlob-fnmatch-test-coverage.md` maps the
  remaining zlob-only result-shaping and private helper assertions, so the
  larger full-suite item remains explicitly open rather than silently
  conflating those APIs with matcher semantics.
- Added a Unix public-API fixture that makes a child directory unreadable,
  verifies its `Collect`-mode `read_dir` error, and restores permissions before
  cleanup. The broader M2 fixture item remains open for a deterministic
  disappearing-*file* strategy and cross-platform coverage.
- Replaced the empty CodSpeed placeholder with executable Criterion-compatible
  matcher and serial/parallel walker benchmarks, and updated the workflow to
  invoke `cargo bench -p bench`. Local short-run smoke measurements execute;
  cross-engine comparisons, committed baselines, and the external CodSpeed
  budget gates remain open.
- Added conservative literal-root planning for include patterns. `src/**/*.rs`
  now avoids opening unrelated sibling directories in serial, streaming, and
  parallel traversal; wildcard-, escape-, and otherwise ambiguous prefixes
  deliberately stay unpruned.
- Added an all-includes extension prefilter: when every include has an
  unambiguous literal suffix, nonmatching files skip full matcher evaluation.
  Braces, classes, escapes, and variable suffixes disable the optimization.
  Together with literal roots, whole-subtree excludes, and the existing
  negation-aware guard, this closes the M2 conservative-pruning checkpoint.
- Added a deterministic backend fixture for a dirent that vanishes between
  `read_dir` and requested metadata collection; it asserts a structured
  `symlink_metadata` error without a timing race. Public unreadable-directory
  and Linux non-UTF-8 integration coverage already exists; the M2 fixture
  aggregate remains open pending an equivalent public disappearing-file hook.
- Added `docs/compatibility-guide.md`, a migration-oriented companion to the
  matrix. It maps the supported matcher and walker APIs and calls out every
  deliberate compatibility difference, deferred result-shaping policy, and
  currently unsupported walker flag.
- Added a pull-request-only SemVer API gate for `ferralk-glob` and `ferralk`
  against the PR base revision. The existing Rust 1.93 MSRV job remains the
  compile gate; the workspace currently defines no Cargo feature sets, which
  was confirmed by the feature audit.
- Added a transparent CI dependency/unsafe-audit job: it installs and runs
  `cargo-audit --locked` and rejects every Rust `unsafe` occurrence before the
  native-backend milestone. The local source scan is clean; the checkbox stays
  open until the RustSec dependency result is observed from CI or a completed
  local audit run.
- Added a parallel `Abort`-policy fixture with independently failing worker
  tasks. It verifies that the returned structured error and the caller's
  shared cancellation token agree, complementing the existing lossless
  `Collect`-channel comparison.
- Completed the local RustSec audit with `cargo-audit` 0.22.2: the current
  121-package lockfile scanned clean against 1,217 advisories. Combined with
  the zero-unsafe source scan and the committed CI gate, this closes the M4
  dependency/unsafe audit before native backend work.
- Release-preflight: `cargo package --no-verify` packages `ferralk-glob`
  0.1.0 successfully. `ferralk` deliberately cannot prepare an upload until
  that dependency exists on crates.io, so publication must happen in order
  (`ferralk-glob` first) and requires maintainer registry authority.
- Added a direct worker-panic-policy test: the shared cancellation state is
  set inside the worker catcher and the original panic resumes on the caller
  after joining. Empty/shallow/imbalanced stress coverage already exists;
  deterministic worker-start-failure injection remains before the aggregate
  stress checkpoint can close.
- Added `WalkOptions::directories_only` as the non-mutating `ZLOB_ONLYDIR`
  mapping. It filters returned files while preserving descent into directories
  across serial, parallel, and streaming walks; `ZLOB_MARK` remains the
  explicit native-path-preservation divergence.
- Confirmed the retirement boundary: default-member tests plus the explicit
  corpus, harness, and bench CI commands replay all 260 matcher cases without
  building the `oracle`, zlob, or Zig. The pinned zlob differential suite
  remains available only through the manual workflow.
- Completed the M3 stress checkpoint. Empty, shallow, and imbalanced trees
  repeatedly match the serial baseline; a worker panic cancels siblings and
  resumes on the caller; and `thread::Builder::spawn_scoped` startup failures
  now become a structured `spawn_worker` error that cancels the shared token.
  The walker has no public visitor callback, so the panic test exercises the
  actual worker catch point that would contain callback execution.
- Added the first apples-to-apples M3 baseline in the CodSpeed walker bench:
  the same 16-branch filtered fixture now runs Ferralk serial, Ferralk with
  four workers, and `ignore::WalkBuilder::build_parallel` with four workers
  and an `**/*.rs` override. A local smoke run completed all three; zlob's
  walker is still only available through the pinned manual Zig-oracle setup,
  so the cross-engine gate and its budget remain open.
- Added the corresponding zlob walker benchmark behind Bench's
  `zlob-oracle` feature, keeping normal CI Zig-free. The manual
  `zlob-benchmark.yml` CodSpeed workflow installs pinned Zig 0.16.0 and runs
  the identical 16-branch, four-worker, `**/*.rs` fixture through zlob's
  `WalkBuilder`; the local smoke median was about 1.02 ms. The M3 benchmark
  checkbox remains open until CodSpeed records the cross-engine budget.
- Added a M1 common-syntax matcher suite for `src/**/*.rs`: compiled Ferralk
  and globset, interpreted `fast-glob` (which has no compile API), and a
  feature-gated compiled zlob reference in the manual CodSpeed workflow. The
  initial ten-sample local smoke is intentionally not a release measurement,
  but it exposes the current optimization blocker: Ferralk was about 3.26 µs
  for the matching case versus about 35.6 ns for zlob and 39.1 ns for globset.
  The 1.5x budget is therefore not met; retain all benchmark checkboxes and
  prioritize the immutable-IR and hot-path work before attempting a gate.
- Replaced the matcher's per-call `HashSet` failure memo with a dense
  token×path-indexed state matrix. The full 260-case matcher corpus and the
  workspace suite remain green; the same local smoke reduced the common
  matching case from about 3.26 µs to 175 ns (and non-matching from 5.33 µs
  to 287 ns). That is material progress, but still roughly 4.9x zlob on the
  matching case, so neither the immutable-IR nor release-budget item is
  checked off.
- Added a deliberately narrow immutable fast-path IR for the compiled token
  shape `literal / **/ * literal`, with an allocation-free prefix/suffix
  matcher and explicit leading-period handling. It is exhaustively checked
  against the general matcher over generated candidates and falls back for
  every other syntax form. The local common matching case is now about
  23.8 ns (ahead of zlob's 35.6 ns and globset's 39.1 ns); the non-matching
  case is still about 11.8 ns versus zlob's 2.57 ns, so the complete M1 gate
  and wider immutable-IR refactor remain open.
- Ported zlob's `SKIP_HIDDEN` walker policy as the explicit
  `WalkOptions::skip_hidden(true)` opt-in. It excludes leading-period files
  and prevents descent into hidden directories before matching or metadata
  work in serial, parallel, and streaming traversal. A public portable fixture
  checks all three modes; the M2 fixture aggregate remains open only for the
  still-unrepresentable public dirent-to-metadata disappearance case.
- Ported `ZLOB_WALK_NO_REPORT_DIRS` as `WalkOptions::files_only(true)`. It
  filters returned directories only after their traversal has been scheduled,
  so nested files remain visible in serial, parallel, and streaming walks. A
  public fixture exercises that invariant in all three modes.
- Completed the `.git` portion of the `GITIGNORE` walker policy: an enabled
  Gitignore walk now skips `.git` before scheduling its subtree, while
  `WalkOptions::keep_git_dir(true)` explicitly restores it. The public fixture
  covers default and opt-in behavior through serial, parallel, and streaming
  traversal.
- Completed ADR-0008's stable-Rust byte-scan step. `ferralk-glob` now uses
  `memmem` for POSIX-class terminators and `memchr` for unescaped brace and
  hot-path hidden-component scans; CodSpeed's matcher bench also tracks the
  relevant compile workload. The local common matching case improved from
  about 23.8 ns to 14.8 ns with the full corpus still green. Cross-engine
  release budgets remain a distinct M1 gate.
