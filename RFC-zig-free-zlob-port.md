# RFC: A Zig-Free Rust Port of zlob

- **Status:** Accepted (2026-08-18)
- **Date:** 2026-08-18
- **Working name:** `zlob-rs` (name and upstream ownership are undecided)
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
Linux, macOS, and Windows. A portable `std::fs` backend remains available on all
targets.

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
5. Support Linux, macOS, and Windows as first-class targets.
6. Provide deterministic optional sorting and an allocation-conscious unsorted
   mode.
7. Support early include/exclude pruning and nested Git-style ignore rules.
8. Return structured traversal errors instead of silently discarding them.
9. Provide a portable backend and allow optimized native backends behind a
   feature or runtime capability check.
10. Ship differential, property, fuzz, concurrency, and platform integration
    tests before claiming compatibility.

The implementation SHOULD:

- offer a migration layer close to the existing Rust API;
- keep the minimum supported Rust version explicit and tested;
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
- expose platform syscall details through the public API.

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

```rust
use zlob::{Pattern, PatternOptions};

let pattern = Pattern::compile(
    "**/*.{js,jsx,ts,tsx}",
    PatternOptions::default()
        .braces(true)
        .recursive_double_star(true)
        .match_hidden(true),
)?;

assert!(pattern.is_match("src/main.ts"));
```

```rust
use zlob::{ErrorPolicy, WalkOptions, Walker};

let entries = Walker::new(".")
    .include("**/*.{js,jsx,ts,tsx}")?
    .exclude("**/node_modules/**")?
    .respect_git_ignore(true)
    .threads(4)
    .error_policy(ErrorPolicy::Collect)
    .options(WalkOptions::default().follow_symlinks(false))
    .collect()?;
```

The iterator form SHOULD support streaming and cancellation. `collect()` MAY use
per-worker result shards and merge once traversal completes. The API MUST not
promise deterministic order unless sorting is requested.

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

The matcher begins with portable byte/character loops and stable dependencies
such as `memchr` where appropriate. Architecture-specific SIMD can be introduced
only after profiles identify a hot loop. Runtime feature detection is preferred
over producing binaries that require a new CPU baseline.

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

- **Linux:** batched `getdents64`, with `statx` only when required;
- **macOS:** `getdirentries64` for names/types and `getattrlistbulk` when batch
  metadata is beneficial, with a portable fallback for unsupported filesystems;
- **Windows:** `NtQueryDirectoryFile` or a documented Win32 batch API, with
  careful version and structure validation;
- **Other targets:** portable backend.

Raw records MUST be bounds-checked before field access. Each unsafe backend
requires module-level invariants, focused tests, fuzzable record parsers where
possible, and a safe adapter boundary.

### 6. Ignore engine

The first implementation SHOULD reuse the `ignore` crate's mature Gitignore
matching unless differential tests show an incompatible hot path. Per-directory
ignore matchers are immutable nodes linked to their parent rule set. Workers may
share parsed nodes through reference-counted ownership and a path-keyed cache.

Negation and precedence are correctness requirements. Optimizations may group
literal suffix rules or cache decisions only after the full ordered-rule result
is preserved.

### 7. Path representation

The public walker accepts and returns `Path`/`PathBuf`. On Unix, matching can use
`OsStrExt::as_bytes()` without UTF-8 assumptions. On Windows, the implementation
must define whether matching operates over WTF-8, UTF-16 code units, or a
lossless internal encoding and then test separators, case folding, and invalid
surrogates accordingly.

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
continue. `Collect` returns entries plus errors. Pattern syntax errors are always
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

The default feature set should be small. Candidate dependencies include
`bitflags`, `crossbeam-deque`, `ignore`, and `memchr`; each must justify its
compile-time and maintenance cost. Rayon is not required if its global-pool
semantics conflict with per-walk thread limits and cancellation.

## Performance requirements

Benchmarks must report matcher-only and complete-walk costs separately. Every
measurement records operating system, filesystem, CPU, Rust version, thread
count, cache state, tree shape, match rate, exclude rate, metadata mode, and
sorting mode.

Required corpora:

1. flat directories with many non-matches;
2. deep source trees;
3. dependency-heavy trees with prunable `node_modules`/`target` directories;
4. Git repositories with nested ignore files and negation;
5. non-UTF-8 Unix paths and Windows-specific names;
6. symlink cycles, unreadable directories, and files removed during traversal;
7. real manifests captured from at least three consumer repositories.

Initial release budgets:

- portable matcher: no more than 1.5x zlob 1.6.3 median on the agreed matcher
  corpus;
- portable matcher on the common syntax subset (no extglob): within 1.25x of
  the `fast-glob` median on patterns both dialects support;
- portable walker: faster than serial `ignore + globset` on all traversal
  corpora and within 2x zlob on dependency-heavy warm-cache walks;
- optimized native backend: within 20% of zlob's median on each supported
  operating system, with no p95 regression larger than 35%;
- no correctness or error-reporting relaxation to meet a performance target.

These are release gates, not promises that every filesystem will behave alike.

## Test strategy

### Differential tests

Run generated and curated pattern/path pairs through zlob 1.6.3, `fast-glob`
(oxc), and the Rust implementation as a three-way harness. zlob 1.6.3 is the
semantic oracle for the full dialect; `fast-glob` is a second reference for the
common subset (`*`, `?`, `**`, character classes, braces), where any
zlob/`fast-glob` disagreement is itself a valuable corpus case. Both references
are test-only dev-dependencies, and the Zig toolchain is confined to a single
CI job. Store cases and expected behavior in a language-neutral corpus so both
oracles can eventually be removed from normal CI.

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
- extract differential fixtures from zlob 1.6.3;
- decide licensing, repository, crate name, and upstream collaboration model.

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

Exit criterion: agreed matcher corpus passes and meets the portable matcher
budget.

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

### Phase 4: Native backends — 6–12 weeks

- Linux backend first;
- macOS and Windows backends independently reviewable;
- runtime fallbacks and backend differential tests;
- platform benchmark gates.

Exit criterion: each backend independently meets safety, parity, and performance
gates. A platform may ship portable-only until its backend is ready.

### Phase 5: Compatibility release — 2–4 weeks

- migration facade and documentation;
- downstream trials in at least two consumers;
- MSRV and feature audit;
- release candidate, security review, and benchmark publication.

The phases imply roughly 3–6 months for a hardened cross-platform port, with
some operating-system work proceeding in parallel.

## Migration and release strategy

Two ownership models are viable:

1. **Upstream rewrite:** contribute the Rust engine to the existing zlob project
   and eventually make it the implementation behind the existing Rust crate.
2. **Independent crate:** publish under a distinct name and provide an optional
   compatibility module.

Using the `zlob` crate name or claiming drop-in compatibility requires agreement
with the current maintainer. Until that agreement exists, documentation must use
the working name `zlob-rs` and avoid implying endorsement.

The first release should be `0.x`. Compatibility claims are made per API area:
matcher, walker, ignores, and legacy glob behavior. Native fast paths remain
feature-gated until they have platform-specific production use.

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

## Open questions

1. Will the zlob maintainer accept a Rust engine or compatibility corpus
   upstream?
2. Which Rust API surface and flags are contractually stable today?
3. What MSRV is required by intended consumers?
4. What exact Windows matching representation should be normative?
5. Should Gitignore compatibility target Git, ripgrep/`ignore`, or current zlob
   when they differ?
6. Is the C API in scope after the Rust implementation stabilizes?
7. Should native fast paths be enabled by default or opt-in?
8. Which consumers beyond Palamedes justify maintaining the full feature set?

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

## References

- [zlob repository](https://github.com/dmtrKovalenko/zlob)
- [zlob 1.6.3 release](https://github.com/dmtrKovalenko/zlob/releases/tag/v1.6.3)
- [zlob 1.6.3 Rust manifest](https://github.com/dmtrKovalenko/zlob/blob/v1.6.3/rust/Cargo.toml)
- [zlob 1.6.3 Rust build script](https://github.com/dmtrKovalenko/zlob/blob/v1.6.3/rust/build.rs)
- [fast-glob (oxc) repository](https://github.com/oxc-project/fast-glob)
- [glob-match repository](https://github.com/devongovett/glob-match)
- [Palamedes source-discovery optimization issue #875](https://github.com/sebastian-software/palamedes/issues/875)
- [Local source-discovery benchmark notes](palamedes/benchmarks/source-discovery-prototype/NOTES.md)
