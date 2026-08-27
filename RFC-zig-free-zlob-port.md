# RFC: ferralk — A Zig-Free Rust Port of zlob

- **Status:** Accepted (2026-08-18); all open design questions resolved in the
  end-to-end design review of 2026-08-18
- **Date:** 2026-08-18
- **Name:** `ferralk` — independent crate family (`ferralk-glob` matcher,
  `ferralk` walker) under sebastian-software
- **License:** MIT (zlob's MIT attribution retained in ported modules)
- **Reference implementation:** zlob 1.6.3
- **Audience:** Rust tooling maintainers who need fast, portable glob matching and filesystem traversal

## Summary

This RFC proposes a Rust-native implementation of zlob's matcher and filesystem
walker that builds with Cargo alone. It removes the Zig compiler, bindgen,
libclang, generated C bindings, and the C ABI from the Rust dependency path while
preserving a safe Rust API and the performance techniques that materially
differentiate zlob.

“Zig-free” does not mean “syscall-free” or “entirely safe Rust.” Matching,
scheduling, ignore evaluation, and the public API should be safe Rust. Optional
optimized filesystem backends may contain small, audited `unsafe` modules for
macOS and Linux. A portable `std::fs` backend remains available on all targets
and is the only backend on Windows.

The delivery order is compatibility first, portable traversal second,
parallelism third, and platform-specific fast paths last. The implementation
strategy is deliberately hybrid: the matcher core is ported mechanically from
zlob's Zig source to capture its dialect semantics faithfully, then refactored
toward the IR architecture described below, while the walker, scheduler, and
filesystem backends are designed fresh around Rust ownership and error models.
zlob 1.6.3 remains the semantic oracle; `fast-glob` (oxc) serves as the matcher
performance baseline and as a second differential reference on the common
syntax subset.

## Motivation

zlob is attractive because it combines multiple optimizations that most Rust
glob crates provide only separately:

- compiled glob, brace, extglob, and fnmatch-style matching;
- SIMD-assisted literal and suffix matching;
- early pruning of excluded directory trees;
- nested `.gitignore` and `.ignore` evaluation;
- lazy parallel traversal with per-worker state;
- batched directory reads using operating-system-specific APIs.

The current Rust crate also introduces a substantial build and packaging
surface. zlob 1.6.3 requires Zig 0.16, invokes it from `build.rs`, runs bindgen,
and crosses a generated C ABI. Consumers therefore need Zig and libclang even if
the rest of their dependency graph is Rust-only. Cross-compilation, reproducible
builds, minimal containers, and contributor onboarding all become harder.

A local Palamedes benchmark illustrates both the opportunity and the limit of
the business case. On a warm-cache 50,000-file tree:

| Implementation | Median | Relative to current Palamedes |
|---|---:|---:|
| Current serial `ignore + globset` | 2,355.256 ms | 1.00x |
| `ignore` parallel + safe subtree pruning | 149.667 ms | 15.74x |
| zlob 1.6.3 walker | 139.820 ms | 16.84x |

That measurement was taken against a different codebase on different hardware
and is kept here as the historical business case. The same comparison on the
current code, as a committed benchmark rather than a one-off, is in
[benchmark evidence](docs/benchmark-evidence.md).

On that workload, existing Rust crates recover nearly all of zlob's advantage.
This RFC is therefore not a recommendation to rewrite zlob solely for
Palamedes. It defines how to do the rewrite if broader reuse, simpler packaging,
or zlob-compatible semantics justify it.

## Goals

The implementation MUST:

1. Build with stable Rust and Cargo without Zig, a C compiler, bindgen, or
   libclang.
2. Expose a safe Rust API for compiled patterns, in-memory matching, and
   directory walking.
3. Define a documented compatibility profile against zlob 1.6.3.
4. Preserve native filesystem paths without unchecked UTF-8 conversion.
5. Support Linux and macOS as first-class targets; support Windows through the
   portable backend (built and tested in CI, no performance gates).
6. Provide deterministic optional sorting and an allocation-conscious unsorted
   mode.
7. Support early include/exclude pruning and nested Git-style ignore rules.
8. Return structured traversal errors instead of silently discarding them.
9. Provide a portable backend and allow optimized native backends behind a
   feature or runtime capability check.
10. Ship differential, property, fuzz, concurrency, and platform integration
    tests before claiming compatibility.

The implementation SHOULD:

- document the mapping from zlob's Rust API in a compatibility guide;
- keep the minimum supported Rust version explicit and tested (policy: current
  stable minus two releases, declared via `rust-version`, verified by a
  dedicated CI job; bumps are minor-version events during 0.x);
- permit consumers to disable walking or native fast paths;
- make thread count, symlink policy, ordering, metadata collection, and error
  policy explicit;
- keep unsafe code isolated by operating system and reviewable in small files.

## Non-goals

The first stable release does not need to:

- preserve zlob's Zig or C source-level API;
- be implementation-identical to the Zig code;
- eliminate all `unsafe` code from optimized platform backends;
- beat zlob on every matcher microbenchmark;
- implement tilde expansion or shell-specific behavior before its semantics and
  security implications are specified;
- replace mature Rust crates merely to avoid dependencies;
- expose platform syscall details through the public API;
- provide a native Windows filesystem backend or Windows-specific performance
  guarantees;
- ship a C ABI or a zlob Rust-API migration facade.

## Compatibility target

Compatibility is behavioral, not architectural. zlob 1.6.3 should be frozen as
a differential-test oracle before implementation begins.

The compatibility matrix MUST separately track:

- `*`, `?`, character classes, escapes, and path separators;
- recursive `**` behavior;
- leading-period behavior;
- brace expansion and nested alternatives;
- extglob behavior;
- case sensitivity and platform defaults;
- no-match, no-check, directory-only, and mark-directory modes;
- sorted versus unsorted results;
- symlink following and cycle detection;
- nested `.gitignore`/`.ignore`, negation, anchoring, and precedence;
- unreadable directories, disappearing files, and partial results;
- non-UTF-8 Unix names and Windows native path representation;
- metadata and file-type reporting.

Flags that exist only for C `glob(3)` compatibility, memory ownership, or the
legacy ABI MAY be represented by higher-level Rust methods rather than copied as
bit values. Any deliberate divergence MUST be listed in a compatibility
document and covered by a test.

## Proposed public API

The public API should use builders and typed options instead of carrying C ABI
constraints into the rewrite.

```rust,no_run
use ferralk_glob::{Pattern, PatternOptions};

let pattern = Pattern::compile(
    "**/*.{js,jsx,ts,tsx}",
    PatternOptions::default()
        .braces(true)
        .recursive_double_star(true)
        .match_hidden(true),
)?;

assert!(pattern.is_match("src/main.ts"));
# Ok::<(), ferralk_glob::PatternError>(())
```

```rust,no_run
use ferralk::{ErrorPolicy, WalkOptions, Walker};

let entries = Walker::new(".")
    .include("**/*.{js,jsx,ts,tsx}")?
    .exclude("**/node_modules/**")?
    .respect_git_ignore(true)
    .threads(4)
    .error_policy(ErrorPolicy::Collect)
    .options(WalkOptions::default().follow_symlinks(false))
    .collect()?;
# let _ = entries;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The iterator form SHOULD support streaming and cancellation. `collect()` MAY use
per-worker result shards and merge once traversal completes. The API MUST not
promise deterministic order unless sorting is requested.

Defaults follow POSIX glob semantics; anything behavior-changing is explicit
opt-in: symlinks are not followed, gitignore evaluation is off, `*` does not
match leading-period names, results are unsorted, the error policy is
`Collect`, and the thread count defaults to `available_parallelism()` once the
parallel scheduler exists.

## Architecture

### 1. Pattern compiler

Patterns are parsed once into an immutable intermediate representation. The IR
should distinguish literals, component wildcards, recursive wildcards,
character classes, brace alternatives, and extglob operations. Compilation
performs validation and safe simplifications such as literal-prefix extraction,
suffix grouping, and alternative deduplication.

The matcher core starts as a scoped, behavior-faithful port of zlob's Zig
matcher. This is the one component where transliteration is cheap and low-risk:
it consists of pure functions with no syscalls, no C ABI, and no Zig-specific
memory management, and the POSIX/extglob/brace dialect is precisely the part
that should not be reinvented from memory. The ported code is validated against
the differential corpus first and refactored toward the IR described above
second; the transliterated structure is scaffolding, not the release
architecture.

The port covers the scalar semantic core only. zlob's `@Vector` SIMD paths are
not transliterated — they have no stable-Rust equivalent. Following the
in-house Ferroni playbook (a pure-Rust Oniguruma port that beats the C original
without a single hand-written intrinsic), hot paths use `memchr`/`memmem`
primitives, which supply SIMD with runtime feature detection and no unsafe
code; `aho-corasick` is a candidate for multi-pattern literal prefilters.
Hand-written intrinsics are considered only if profiling proves a gap these
primitives cannot close. `ferralk-glob` compiles under
`#![forbid(unsafe_code)]`; matching operates on bytes (`&[u8]`) with a `&str`
convenience layer on top. Ported modules carry a module-level provenance header
naming the zlob source file and the v1.6.3 commit.

### 2. Prune planner

The compiler derives conservative traversal hints from include and exclude
patterns:

- a longest literal root or prefix;
- directory patterns that cover an entire subtree;
- suffix or extension filters that can reject files cheaply;
- whether a rejected directory could contain a later match.

Pruning MUST be conservative. If the compiler cannot prove that no descendant
can match, the walker descends. Gitignore negation also prevents pruning when a
later rule may re-include a descendant.

### 3. Walker frontend

The walker owns platform-neutral policy:

- root normalization;
- include/exclude evaluation;
- ignore-rule chains;
- symlink and cycle policy;
- metadata requests;
- cancellation and errors;
- ordering and result delivery.

The frontend consumes directory batches from a backend trait. Backend-specific
handles and buffers do not escape into the public API.

### 4. Scheduler

The scheduler should start on the caller thread and create helper threads only
after parallel work exists. Each worker owns its path buffer, directory-read
buffer, matcher scratch space, and result shard. Directory tasks use local
queues with work stealing or a bounded shared injector. Completion detection
must not spin when the tree is shallow or temporarily imbalanced.

There MUST be a single cancellation state and a lossless error channel. Panics
in visitors or workers must stop the walk and be propagated according to a
documented policy.

### 5. Filesystem backends

Delivery starts with a portable `std::fs::read_dir` backend. Optimized backends
are optional and selected at compile time plus runtime capability checks:

- **macOS** (first native backend): `getdirentries64` for names/types and
  `getattrlistbulk` when batch metadata is beneficial, with a portable fallback
  for unsupported filesystems;
- **Linux** (second): batched `getdents64`, with `statx` only when required;
- **Windows:** portable backend only (tier 2); no native backend is planned;
- **Other targets:** portable backend.

Native backends are post-1.0 features behind feature flags; 1.0 ships portable
on all platforms.

Raw records MUST be bounds-checked before field access. Each unsafe backend
requires module-level invariants, focused tests, fuzzable record parsers where
possible, and a safe adapter boundary.

### 6. Ignore engine

Git itself is normative: `git check-ignore` verdicts are the corpus oracle for
ignore semantics. The `ignore` crate's Gitignore rule matcher is the engine
(its parallel walker is not reused), and known divergences from Git are
documented and decided case by case. Per-directory
ignore matchers are immutable nodes linked to their parent rule set. Workers may
share parsed nodes through reference-counted ownership and a path-keyed cache.

Negation and precedence are correctness requirements. Optimizations may group
literal suffix rules or cache decisions only after the full ordered-rule result
is preserved.

### 7. Path representation

The public walker accepts and returns `Path`/`PathBuf`. Matching operates over
bytes on every platform: raw bytes via `OsStrExt::as_bytes()` on Unix, and the
lossless WTF-8 representation via `OsStr::as_encoded_bytes()` (stable since
Rust 1.74) on Windows, so a single byte matcher serves all targets and unpaired
surrogates survive round-trips. On Windows both `/` and `\` act as separators;
patterns are written with `/`. Matching is case-sensitive by default on all
platforms, with explicit opt-in case folding.

No public convenience function may use `from_utf8_unchecked` for filesystem
results. String-only matching remains available as a separate API.

## Error model

The current ecosystem varies between aborting, silently skipping, and returning
partial results. This port makes that choice explicit:

```rust
pub enum ErrorPolicy {
    Abort,
    Skip,
    Collect,
}
```

Errors include the operation, path, platform error, and whether traversal can
continue. `Collect` returns entries plus errors and is the default policy.
Pattern syntax errors are always
returned before traversal. Resource exhaustion and internal invariant failures
always abort.

## Safety and dependency policy

The crate root SHOULD deny unsafe operations outside the backend modules. Each
unsafe module documents:

- ownership and lifetime of handles and buffers;
- alignment and bounds requirements for kernel records;
- thread-safety assumptions;
- fallback behavior when a syscall is unavailable;
- links to the relevant operating-system ABI documentation.

The default feature set should be small. `ferralk-glob` depends on `memchr`
only (with `aho-corasick` as a prefilter candidate). `ferralk` adds `ignore`
(gitignore rule matcher only), `crossbeam-deque` for the scheduler, and
`bitflags` if needed; each dependency must justify its compile-time and
maintenance cost. Rayon is rejected: its global-pool semantics conflict with
per-walk thread limits and cancellation.

## Performance requirements

Benchmarks must report matcher-only and complete-walk costs separately. Every
measurement records operating system, filesystem, CPU, Rust version, thread
count, cache state, tree shape, match rate, exclude rate, metadata mode, and
sorting mode. Benchmarks use `criterion` with CodSpeed integration in CI for
continuous regression tracking, mirroring the Ferroni setup.

Required corpora:

1. flat directories with many non-matches;
2. deep source trees;
3. dependency-heavy trees with prunable `node_modules`/`target` directories;
4. Git repositories with nested ignore files and negation;
5. non-UTF-8 Unix paths and Windows-specific names;
6. symlink cycles, unreadable directories, and files removed during traversal;
7. real manifests captured from at least three consumer repositories.

Comparison criteria:

- record portable matcher results against Rust baselines (`globset` and
  `fast-glob`) on their shared syntax subset;
- record portable Walker results against `ignore` on the same fixture;
- retain the optional zlob comparison as useful context, but never as the sole
  baseline: it requires Zig and has a different implementation boundary;
- record native-backend measurements per supported platform when they inform a
  change;
- never relax correctness or error reporting for a faster result.

Measurements are decision support, not release, CI, or p95 performance gates.
They make trade-offs visible without claiming that every filesystem or workload
has one universal winner.

## Test strategy

### Differential tests

Run generated and curated pattern/path pairs through zlob 1.6.3, `fast-glob`
(oxc), and the Rust implementation as a three-way harness. zlob 1.6.3 is the
semantic oracle for the full dialect; `fast-glob` is a second reference for the
common subset (`*`, `?`, `**`, character classes, braces), where any
zlob/`fast-glob` disagreement is itself a valuable corpus case. For ignore
semantics the oracle is `git check-ignore`.

The corpus is seeded by porting zlob's own test suite 1:1, then extended by
differential generation. The live zlob oracle is a development-time tool only:
it runs locally or in a manually triggered workflow (an unpublished `oracle`
workspace member with zlob as dev-dependency, excluded from default members),
never on a schedule — the frozen 1.6.3 reference cannot produce new answers.
Normal CI replays the checked-in corpus without any Zig toolchain. Every
disagreement the oracle uncovers is checked in as a permanent case; after 1.0
the oracle retires and the corpus is the test suite.

Cases are stored as JSON Lines, one file per topic (`braces.jsonl`,
`extglob.jsonl`, `ignore.jsonl`, …), one case per line carrying pattern, path,
flags, expected result, source, and note. Non-UTF-8 bytes use a `\xNN` escape
convention with a small codec per consuming language.

### Property tests

Check invariants such as:

- compiling a literal pattern matches only the corresponding literal;
- sorted collection equals sorting the unsorted multiset;
- adding a proven whole-subtree exclude cannot add results;
- parallel and single-thread walks produce the same result multiset;
- portable and native backends produce the same entries and error classes.

### Fuzzing

Fuzz the parser, compiled matcher, ignore parser, native record decoders, and
backend differential adapters. Seed corpora include shell edge cases, nested
braces/extglobs, long components, malformed byte sequences, and ignore
negations.

### Concurrency tests

Use deterministic scheduler tests where possible and Loom for small queue and
completion-state models. Stress cancellation, visitor panic, worker-start
failure, empty trees, shallow trees, and highly imbalanced trees.

### Platform CI

CI covers stable Rust on Linux, macOS, and Windows. It also tests portable-only
builds, native-fastpath builds, minimum Rust version, feature powerset where
practical, sanitizers on native parsers, Miri for safe/unsafe boundaries that it
can execute, and cross-compilation checks.

## Delivery plan

### Phase 0: Freeze semantics and oracle — 1–2 weeks

- inventory the Rust and C APIs and all flags;
- write the compatibility matrix;
- port zlob's own test suite 1:1 into the corpus;
- extract further differential fixtures from zlob 1.6.3;
- scaffold the workspace and the Ferroni-style repository blueprint
  (release-please, renovate, codecov, CodSpeed).

Licensing (MIT), repository (`sebastian-software/ferralk`), crate names, and
the collaboration model (independent, public zlob attribution without
maintainer outreach) are already decided.

Exit criterion: disputed or undefined semantics are documented as open cases.

### Phase 1: Pattern engine — 3–5 weeks

- three-way differential harness (zlob 1.6.3 oracle, `fast-glob`
  cross-reference) as the first piece of infrastructure;
- mechanical, behavior-faithful port of zlob's matcher core, validated against
  the corpus;
- refactor of the ported matcher into the parser and immutable IR;
- core wildcard, recursive, class, brace, and extglob semantics;
- reusable compiled pattern API;
- differential/property/fuzz tests;
- portable optimizations.

Exit criterion: agreed matcher corpus passes; benchmark evidence is
reproducible and its scope is documented.

### Phase 2: Portable walker — 3–4 weeks

- `std::fs` backend;
- streaming and collecting APIs;
- conservative prune planner;
- native path preservation;
- sorting, metadata, errors, symlink policy, and cancellation.

Exit criterion: portable walker is production-usable without parallelism or
native syscalls.

### Phase 3: Ignore semantics and parallel scheduler — 3–5 weeks

- nested ignore chains and caches;
- lazy worker creation and work distribution;
- per-worker scratch and result shards;
- concurrency and error-propagation hardening.

Exit criterion: single- and multi-thread results are identical across the
corpus; no hangs or lost errors under stress.

### Phase 4: Native backends (post-1.0) — 6–12 weeks

- macOS backend first, Linux second, each independently reviewable;
- no Windows backend (portable only);
- runtime fallbacks and backend differential tests;
- repeatable platform measurements when a native change needs evaluation.

Exit criterion: each backend independently meets safety and parity criteria.
Its measurements are reproducible and documented. 1.0 does not wait for this
phase; backends ship as feature-gated 1.x releases.

### Phase 5: Stabilization and a later public release — 2–4 weeks

- compatibility guide documenting the zlob mapping and deliberate divergences;
- MSRV and feature audit;
- release candidate, security review, and benchmark publication.

The Palamedes integration trial is separate downstream follow-up work, not a
Phase 5 exit criterion. Its feedback may inform a later release but does not
block the portable RFC implementation or release readiness.

The phases imply roughly 3–6 months for a hardened cross-platform port, with
some operating-system work proceeding in parallel.

## Migration and release strategy

Decided: ferralk is an independent crate family under its own name. The README
and NOTICE credit zlob as an inspiration; no naming, endorsement, upstream
agreement, or maintainer outreach is required or implied. Compatibility with
zlob is a documented profile ("compatible with X, documented divergences Y"),
never a drop-in claim.

Releases start at `0.x`; 1.0 requires a stable API, documented compatibility,
and the normal validation suite. Compatibility claims are made per API area:
matcher, walker, and ignores. Native fast paths ship as feature-gated 1.x
releases and remain opt-in until they have production use on their platform.

## Alternatives considered

### Keep zlob's Zig core

This preserves current performance and semantics but retains the exact build and
cross-compilation problem this RFC addresses. Prebuilt native artifacts reduce
consumer setup but add supply-chain, target-matrix, and release-signing work.

### Improve only the Rust bindings

Handwritten bindings could remove bindgen/libclang while retaining Zig. This is
a useful intermediate packaging improvement, but it does not produce a
Cargo-only source build.

### Use `ignore` plus early pruning

This is the recommended solution for Palamedes today. It is mature, Rust-native,
and measured within about 7% of zlob on the large dependency-heavy benchmark.
It does not provide the full zlob syntax/API or its batched native backends.

### Use `wax`

Wax provides expressive Rust-native glob walking and effective exclusion
pruning. It is a strong off-the-shelf choice when its semantics fit, but it is
not a zlob-compatible high-performance backend.

### Use `fast-glob` (oxc)

`fast-glob` is a matcher-only crate forked from `glob-match`, with correctness
fixes and substantially faster matching. It supports `*`, `?`, `**`, character
classes, and nested braces, but provides no extglob, no filesystem walking, no
gitignore handling, and deliberately does not report invalid patterns. It
cannot be the engine, but it is adopted as the matcher performance baseline and
as a second reference implementation in the differential harness for the common
syntax subset.

### Port the Zig source line by line

**Adopted for the matcher core only.** The matcher is pure computation — no
syscalls, no C ABI, no Zig-specific memory management — so transliteration is
cheap, low-risk, and the fastest way to capture the full dialect semantics; the
ported structure is then refactored toward the IR architecture. For the walker,
scheduler, and native backends a transliteration would preserve incidental
architecture and ABI constraints, make review difficult, and risk reproducing
undefined assumptions; those components follow Rust ownership and error models
from the start.

## Risks

- Pattern dialect compatibility is larger than the common `*`/`**` subset.
- Direct syscall code creates a long-term platform maintenance obligation.
- Windows path encoding and case behavior can invalidate Unix-centric designs.
- Gitignore negation makes aggressive pruning unsound if modeled incorrectly.
- Parallel traversal can improve throughput while worsening tail latency on
  small or I/O-constrained trees.
- Benchmarks can overfit warm caches and dependency-heavy synthetic trees.
- Reusing `ignore` may constrain exact zlob precedence or performance.
- A new crate may fragment maintenance unless coordinated upstream.

## Resolved questions (design review 2026-08-18)

1. Upstream acceptance — moot: ferralk is independent and credits zlob
   publicly; no maintainer outreach is required.
2. Contractual zlob API stability — not applicable; compatibility is a
   documented profile, not a contract.
3. MSRV — current stable minus two releases, declared via `rust-version` and
   CI-tested; bumps are minor-version events during 0.x.
4. Windows matching representation — WTF-8 bytes via
   `OsStr::as_encoded_bytes()`; `/` and `\` both act as separators;
   case-sensitive by default everywhere with opt-in folding.
5. Gitignore reference — Git itself (`git check-ignore` as corpus oracle); the
   `ignore` crate's rule matcher is the engine; divergences are documented and
   decided case by case.
6. C API — out of scope.
7. Native fast paths — opt-in feature flags, post-1.0, macOS first then Linux;
   no Windows backend.
8. Consumers — Palamedes is the first consumer; the two-consumer gate was
   superseded by the acceptance decision.

## Decision

Accepted on 2026-08-18: proceed with the port using the hybrid strategy above.
The matcher core is ported mechanically from zlob's Zig source and then
refactored toward the IR; the walker, scheduler, and filesystem backends are
designed fresh for Rust. `fast-glob` (oxc) serves as the matcher performance
baseline and as a second differential reference on the common syntax subset;
zlob 1.6.3 remains the semantic oracle.

Phase 0 is not skipped: the differential corpus is built before matcher code,
because the mechanical port cannot be judged correct without it. The earlier
gate requiring two committed consumers before a full rewrite is superseded by
this decision.

The end-to-end design review of 2026-08-18 resolved all remaining open
questions (see "Resolved questions"). Headline decisions: independent
`ferralk` workspace (matcher crate `ferralk-glob`, walker crate `ferralk`),
MIT license, MSRV stable−2, Git-normative ignore semantics, byte/WTF-8
matching on all platforms, the Ferroni SIMD playbook (memchr primitives,
`forbid(unsafe_code)` in the matcher), an own work-stealing scheduler, a
portable-only 1.0 with native macOS→Linux backends as feature-gated 1.x
releases, Windows as a tier-2 portable-only target, and the Ferroni repository
blueprint (criterion + CodSpeed, codecov, release-please, renovate). Each
decision is recorded individually under [docs/adr/](docs/adr/README.md).

## References

- [zlob repository](https://github.com/dmtrKovalenko/zlob)
- [zlob 1.6.3 release](https://github.com/dmtrKovalenko/zlob/releases/tag/v1.6.3)
- [zlob 1.6.3 Rust manifest](https://github.com/dmtrKovalenko/zlob/blob/v1.6.3/rust/Cargo.toml)
- [zlob 1.6.3 Rust build script](https://github.com/dmtrKovalenko/zlob/blob/v1.6.3/rust/build.rs)
- [fast-glob (oxc) repository](https://github.com/oxc-project/fast-glob)
- [glob-match repository](https://github.com/devongovett/glob-match)
- [Ferroni — in-house pure-Rust Oniguruma port](https://github.com/sebastian-software/ferroni)
- [Palamedes source-discovery optimization issue #875](https://github.com/sebastian-software/palamedes/issues/875)
