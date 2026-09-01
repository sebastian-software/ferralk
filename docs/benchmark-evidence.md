# Benchmark evidence

Performance evidence for the matcher and the walker: what is measured, where it
runs, how to reproduce it, and what it does not establish.

These numbers are decision support. There is no performance gate: no CI job
fails on a threshold, no release is blocked on a measurement, and nothing here
is a claim to be the fastest library. [Deferred follow-up](external-release-gates.md)
records the same for releases.

## The lanes

| Lane | What it measures | Where | Gating |
| --- | --- | --- | --- |
| Matcher, wall time | Compiled-pattern matching and compilation, against `globset`, `fast-glob`, and `wax` | [`matcher.rs`](../tools/bench/benches/matcher.rs), run locally back to back and reported in the pull request | No |
| Walker, wall time | Warm-cache traversal of synthetic repository-shaped trees plus a 400-level chain, including an include-plus-covering-exclude pruning arm, ferralk serial and parallel against `ignore` parallel, one job per backend that ships | [`walker-bench.yml`](../.github/workflows/walker-bench.yml), every pull request, medians in the job summary and as an artifact | No |
| Engine comparison | One repository shape with unscoped include, scoped include, include-plus-exclude, and gitignore-pruned queries | [`walker_palamedes.rs`](../tools/bench/benches/walker_palamedes.rs), run on demand | No |
| zlob ablations | Ferralk and zlob on one fixed fixture, split into traversal, filtering, result retention, and path-representation costs | [`walker_zlob_ablation.rs`](../tools/bench/benches/walker_zlob_ablation.rs), run on demand | No |
| zlob context | The same matcher and walker shapes against zlob 1.6.3 | [`zlob-benchmark.yml`](../.github/workflows/zlob-benchmark.yml), manual dispatch only | No |
| Node.js ecosystem context | The same matcher cases and repository-shaped walker fixture against current locked Node libraries | [`tools/bench/node`](../tools/bench/node), run on demand | No |

**Why every lane measures wall time.** An earlier revision ran the matcher
under CodSpeed's simulation instrument, which counts instructions in a virtual
machine. It was removed on 2026-08-19: over the period it ran it produced four
false alarms and no true finding, every one of them a stale baseline rather
than a change in the code under test. For a library of this scope the
back-to-back measurement a contributor takes on one machine, before and after,
proved to be the evidence that actually caught things. Automated protection
now comes from the walker wall-time lane, which runs on every pull request.

That instrument was never right for the walker in any case: it serializes
threads and does not model syscall cost, so a parallel-versus-serial comparison
there measures instruction count rather than speedup.

**What each lane does not establish.** The matcher lane says nothing about
syscall-bound work. The walker lanes
run on synthetic trees written immediately before measurement, so every result
describes a warm page cache; cold-cache behaviour is a different measurement and
is not made anywhere here. Shared runners are noisy: compare arms measured in
the same invocation, not numbers from different runs. And no lane says anything
about API or semantic scope — the engines compared below do not all offer the
same guarantees, and speed is one input among several.

## Reproducing

Rust-only, no extra toolchain:

```sh
# Matcher, against globset and fast-glob. Add --output-format bencher for one
# line per benchmark instead of criterion's reports.
cargo bench -p bench --bench matcher

# Walker wall time, the shape the pull-request lane measures.
cargo bench -p bench --bench walker -- --warm-up-time 1 --measurement-time 5 --sample-size 20

# Engine comparison on a repository-shaped 53k-file tree. This includes
# walkdir, jwalk, globwalk, and wax in addition to the existing arms.
cargo bench -p bench --bench walker_palamedes -- --output-format bencher --noplot
```

Node.js ecosystem context requires Node 24 or later. Install the exact locked
dependencies once, then run the self-validating harness:

```sh
cd tools/bench/node
npm ci
npm run bench
```

**Both walker benches measure the portable backend unless a feature says
otherwise.** The native backends are separate code paths and are only measured
when they are asked for, on the platform that has them:

```sh
cargo bench -p bench --bench walker_palamedes --features native-macos
cargo bench -p bench --bench walker_palamedes --features native-linux
```

Leaving the feature off is how the macOS bulk reader spent its whole life
unmeasured: every lane and every reproduction command here read directories
through `std::fs`, so a native backend that was slower than the portable one
produced no number that said so. The walker wall-time lane now runs one job per
backend, macOS included; the engine comparison stays portable-only in CI and is
compared against a native build locally.

Optional zlob context, which needs **Zig 0.16** and libclang because zlob 1.6.3
builds through `build.rs` and bindgen:

```sh
# macOS/Homebrew setup used for the current local run:
brew install zig llvm
export LIBCLANG_PATH="$(brew --prefix llvm)/lib"

cargo bench -p bench --bench walker_palamedes --features zlob-oracle
cargo bench -p bench --bench walker_zlob --features zlob-oracle
cargo bench -p bench --bench walker_zlob_ablation --features native-macos,zlob-oracle
cargo bench -p bench --bench matcher_zlob --features zlob-oracle
```

zlob is context, never a baseline anything depends on: its benches are behind
the `zlob-oracle` feature, its workflow is manual dispatch only, and no
automatic lane requires Zig. The current local run used Zig 0.16.0 and
libclang 22.1.8 from Homebrew.

The engine-comparison measurements in the next section were taken with:

| | |
| --- | --- |
| Host | Apple M1 Pro, 10 cores, macOS 26.5.2 |
| Toolchain | rustc 1.95.0, release profile |
| Threads | 4 for every parallel arm, so the arms are comparable rather than a reading of `available_parallelism` |
| Cache | Warm: each fixture is written immediately before its measurements |
| Method | criterion point estimate; for the engine comparison, the median of three full runs |

## Engine comparison, repository shape

The RFC that started this project opened with one local measurement of a
50,000-file tree — serial `ignore + globset` at 2,355 ms, `ignore` parallel with
hand-written subtree pruning at 150 ms, zlob at 140 ms — and concluded that
existing Rust crates already recovered nearly all of zlob's advantage. That
measurement was made against a different codebase on different hardware and was
never re-runnable. [`walker_palamedes.rs`](../tools/bench/benches/walker_palamedes.rs)
rebuilds the *shape* so the comparison can be repeated: 53,601 files, 2,600 of
them TypeScript sources under `src/` and `packages/`, the rest a `node_modules`
tree of 400 top-level packages and 200 nested dependency packages, plus the
checked-in `.gitignore` fixture copied to the benchmark root.

Absolute numbers are not comparable with the RFC's; the ratios between engines
on one host are.

The RFC's question — whether existing Rust crates already recover zlob's
advantage — was also answered on two real repositories rather than a
reconstruction, by a consumer who built the `ignore` arm the comparison needs
and then measured against it over four releases. See
[Palamedes adoption](palamedes-adoption.md).

Every arm is run once before timing and has to return the same file count, so no
arm can be fast by finding less. The **unscoped** query can match anywhere and
must inspect the whole tree; it finds 7,400 files. The **scoped** query names
`src` and `packages` as its only possible roots, so a pattern-aware walker can
skip `node_modules`; it finds 2,600 files. Here, scoped describes the query's
root constraint, not a benchmark or process isolation boundary.

The harness also measures an **exclude-pruned** query: the unscoped include
paired with `exclude("**/node_modules/**")`. It must return the same 2,600 files
as the scoped query, but exercises covering-exclude pruning instead. A companion
arm obtains the same policy from `respect_git_ignore(true)` and the fixture's
`node_modules/` rule. These arms were added after the recorded tables below;
they are reported by new runs of the reproducible command above.

### `**/*.{ts,tsx}` — nothing can be pruned

| Arm | Median | Relative |
| --- | ---: | ---: |
| zlob 1.6.3, 4 threads | **30.90 ms** | 1.00x |
| `ignore` parallel + overrides, 4 threads | 37.15 ms | 1.20x |
| ferralk, 4 threads | 37.19 ms | 1.20x |
| `ignore` serial + `globset` | 75.58 ms | 2.45x |
| ferralk, serial | 79.64 ms | 2.58x |

### `{src,packages}/**/*.{ts,tsx}` — the query names its roots

| Arm | Median | Relative |
| --- | ---: | ---: |
| ferralk, 4 threads | **7.34 ms** | 1.00x |
| `ignore` parallel + hand-written subtree pruning, 4 threads | 9.41 ms | 1.28x |
| ferralk, serial | 13.72 ms | 1.87x |
| zlob 1.6.3, 4 threads | 31.74 ms | 4.32x |
| `ignore` parallel + overrides, 4 threads | 37.91 ms | 5.16x |
| `ignore` serial + `globset` | 75.74 ms | 10.32x |

### Reading these

- **Where the whole tree must be read, zlob is still ahead** — 20% over both
  Rust walkers, which are level with each other to within the noise of this
  host. That is the honest state of the unscoped query.
- **Where the query names its roots, ferralk is ahead**, and it gets there from
  the pattern alone. The `ignore` arm that comes close needs a hand-written
  `filter_entry`; the arm that only passes the globs as overrides is 5.2x
  behind, because overrides decide what is yielded, not what is opened. zlob
  reads the whole tree for this query, which is why it lands near its unscoped
  time.
- **Serial ferralk beats parallel `ignore` on the scoped query** (13.72 ms
  against 37.91 ms) purely by not opening `node_modules`. Pruning is worth more
  than threads on this shape.
- The 2,355 ms in the RFC's table was a serial walk doing more per file than
  this reconstruction does; the 75.58 ms here is the same *approach*, not the
  same code.

### Which backend those rows measure

The portable one. `cargo bench -p bench --bench walker_palamedes` compiles
`ferralk` without a native feature, so both `ferralk` arms above read
directories through `std::fs`, on macOS as everywhere else.

## Expanded library comparison on this machine, 2026-08-28

The comparison was refreshed on the current checkout. It covers four popular
Rust libraries plus zlob as the optional cross-language reference, and now
includes a separate Node.js ecosystem harness on the same fixture shape:

| Library | Locked version | Role in the comparison | Important scope note |
| --- | ---: | --- | --- |
| [`walkdir`](https://crates.io/crates/walkdir/2.5.0) | 2.5.0 | Serial traversal plus a caller-side `globset` | Traversal only; it does not select paths itself. |
| [`jwalk`](https://crates.io/crates/jwalk/0.9.0) | 0.9.0 | Serial and four-thread traversal plus a caller-side `globset` | The current release is marked deprecated by its package metadata. |
| [`globwalk`](https://crates.io/crates/globwalk/0.9.1) | 0.9.1 | Integrated serial glob walking | Built on `walkdir` and `ignore` overrides. |
| [`wax`](https://crates.io/crates/wax/0.7.0) | 0.7.0 | Integrated serial glob walking and compiled matching | UTF-8/regex-based API; the comparison is valid only for this ASCII fixture. |
| [`zlob`](https://github.com/dmtrKovalenko/zlob) | 1.6.3 | Integrated parallel glob walking and compiled matching | Zig library behind a C ABI; local builds need Zig 0.16 and libclang. |

`ignore` + `globset`, `globset`, `fast-glob`, ferralk, and zlob remain in the
existing lanes. Every walker arm was checked for the exact same result count
before it was timed: **7,400** files for the unscoped query and **2,600** for
the scoped query. The new caller-side arms compile their `globset` inside the
timed operation, as the existing ferralk and `ignore` arms compile their
selection inside the operation. `globwalk` and `wax` likewise include their
builder work in their timed operation. Matcher-only baselines are compiled
once outside their match loop.

### Host and method

| | |
| --- | --- |
| Host | Mac Studio (Mac13,2), Apple M1 Ultra, 20 cores (16 performance, 4 efficiency), 64 GB RAM |
| OS | macOS 26.5.2, build 25F84; Darwin 25.5.0, arm64 |
| Benchmark revision | `ceab189` (merged source tree used for the walker refresh) |
| Toolchain | rustc/cargo 1.96.0, LLVM 22.1.2 |
| Benchmark stack | Criterion 0.8.2, release profile; exact dependency versions are locked in `Cargo.lock` |
| zlob toolchain | Zig 0.16.0, libclang 22.1.8 from Homebrew |
| Node toolchain | Node.js 24.19.0, npm 11.17.0; exact dependency versions are locked in `tools/bench/node/package-lock.json` |
| Threads | 4 for every parallel ferralk, `ignore`, and `jwalk` arm |
| Cache | Warm: the fixture is written immediately before the measurement and then reused by all arms in that invocation |
| Fixture | 53,601 files, 2,600 TypeScript sources under `src/` and `packages/`, plus 400 top-level and 200 nested dependency packages under `node_modules/` and one `.gitignore` |

The complete refresh used these commands. `+stable` selects the installed 1.96.0
toolchain explicitly because this host's default nightly predates the workspace
MSRV:

```sh
cargo +stable bench -p bench --bench matcher -- --output-format bencher --noplot
LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib cargo +stable bench -p bench --bench matcher_zlob --features zlob-oracle -- --output-format bencher --noplot
LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib cargo +stable bench -p bench --bench walker_palamedes --features zlob-oracle -- --output-format bencher --noplot
LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib cargo +stable bench -p bench --bench walker_palamedes --features native-macos,zlob-oracle -- --output-format bencher --noplot
cd tools/bench/node && npm run bench
```

The tables report Criterion's point estimates in milliseconds. They are
indicative wall times, not a performance gate. Lower is better. The zlob arm is
inside both walker invocations, so each table's Rust and zlob rows share the
same fixture and host load.

### Portable backend

| Arm | `**/*.{ts,tsx}` | `{src,packages}/**/*.{ts,tsx}` |
| --- | ---: | ---: |
| ferralk, serial | 58.59 ms | 10.58 ms |
| ferralk, 4 threads | 37.78 ms | **4.29 ms** |
| `ignore` serial + `globset` | 75.31 ms | 74.68 ms |
| `walkdir` serial + `globset` | 74.06 ms | 71.67 ms |
| `jwalk` serial + `globset` | 77.66 ms | 74.47 ms |
| `jwalk`, 4 threads + `globset` | 37.98 ms | 25.85 ms |
| `globwalk` serial | 76.29 ms | 74.77 ms |
| `wax` serial | 98.66 ms | 14.92 ms |
| `ignore` parallel + overrides | 29.57 ms | 28.43 ms |
| `ignore` parallel + hand-pruned subtree | — | 7.14 ms |
| zlob, 4 threads | **20.12 ms** | 20.30 ms |

On the unscoped query, zlob is the fastest arm at 20.12 ms. Ferralk is level
with four-thread `jwalk` at 37.78 and 37.98 ms. On the scoped query, Ferralk is
fastest because it prunes `node_modules` from the pattern itself: 4.29 ms versus
7.14 ms for hand-pruned `ignore`, 14.92 ms for `wax`, and 20.30 ms for zlob. The
`ignore` arm needs an
extra caller policy that the Ferralk pattern already expresses.

### macOS-native backend

| Arm | `**/*.{ts,tsx}` | `{src,packages}/**/*.{ts,tsx}` |
| --- | ---: | ---: |
| ferralk, serial | 48.08 ms | 9.44 ms |
| ferralk, 4 threads | 33.90 ms | **6.27 ms** |
| `ignore` serial + `globset` | 76.68 ms | 75.76 ms |
| `walkdir` serial + `globset` | 84.72 ms | 75.35 ms |
| `jwalk` serial + `globset` | 85.08 ms | 77.45 ms |
| `jwalk`, 4 threads + `globset` | 49.50 ms | 26.16 ms |
| `globwalk` serial | 83.85 ms | 73.85 ms |
| `wax` serial | 100.05 ms | 14.97 ms |
| `ignore` parallel + overrides | 39.11 ms | 27.73 ms |
| `ignore` parallel + hand-pruned subtree | — | 7.13 ms |
| zlob, 4 threads | **30.22 ms** | 20.93 ms |

The native run puts Ferralk at 33.90 ms unscoped, 3.68 ms behind the zlob arm in
the same invocation. Scoped Ferralk remains ahead at 6.27 ms because it avoids
the dependency subtree. The portable run happened under materially different
host load: its zlob estimate is 20.12 ms, while the adjacent native invocation
puts the same zlob code at 30.22 ms. Do not attribute differences between the
two tables to Ferralk's backend; compare arms inside a table, and use the paired
ablations below for claims about a code change.

The targeted, before/after comparisons attribute a 4.9% improvement to
parent-relative directory opens and a further 10.3% to depth-first local
queues. These are separate wall-time runs, not percentages that can be summed
into a complete-table improvement.

The new references answer a comparison question, not an adoption question.
`jwalk` is deprecated, `walkdir` has no matching or pruning policy, `globwalk`
uses a different override contract, and `wax` accepts UTF-8 expressions rather
than Ferralk's arbitrary-byte matcher. Exact semantic compatibility still
comes from the checked-in corpus and caller parity tests, not from these
wall-time rows. zlob was run in the same fixture and invocation as the other
walker arms with Zig 0.16.0 and libclang 22.1.8 installed locally. It remains an
optional context lane rather than an automated baseline.

### What zlob's unscoped lead comes from

A source audit found no single matcher shortcut hidden behind the unscoped
number. Its walker combines several low-level choices: a raw
`getdirentries64` scanner on macOS, retained parent directory handles with
`openat` for children, last-in-first-out local work queues, fixed per-worker
path storage, contiguous name bytes with compact entry records, and lazy helper
startup. Ferralk already used the raw scanner and lazy startup. The ablation
lane tested the remaining plausible differences one at a time on the same
53,600-file fixture.

| Variant | Targeted result | Decision |
| --- | --- | --- |
| Retain a parent directory capability and open queued children relative to it | **4.90% faster on macOS**; 95% interval 2.62–7.36%, `p < 0.01`. The Linux lane also reports a dedicated 400-level chain so repeated full-path resolution remains visible separately from repository shape. | Kept on macOS and Linux for path-independent traversal. Retention is capped per backend at the smaller of 256 descriptors or one quarter of `RLIMIT_NOFILE`; full-path opens remain the fallback both at that limit and when ignore, cycle, or requested metadata operations still need the reported path. This prevents one task combining descriptor-relative entries with state from a replaced path. No cross-host percentage is inferred for Linux. |
| Change only worker-local queues from FIFO to LIFO | **10.33% faster** over 53,600 files; 95% interval 6.72–13.27%, `p < 0.01` | Kept. A 5,120-file control improved 3.5%; a 128-file control regressed 0.04 ms (4%), an accepted sub-millisecond trade-off. |
| Reset a scratch `PathBuf` by truncating its Unix bytes | 6 ns microbenchmark versus 12 ns for copying the parent and 22 ns for `pop`; no significant complete-walk change | Rejected: the isolated operation is not an end-to-end bottleneck. |
| Store listing names in one byte vector plus offsets | Complete-walk interval −3.20% to +6.55%, `p = 0.51` | Rejected: no measurable improvement. |
| Retain result paths in shared 256 KiB chunks | 145 µs versus 193 µs to retain 7,400 synthetic paths | Rejected for now: the maximum isolated saving is small and would complicate the public owned-path representation. |
| Start helpers at three queued directories instead of eight | 1.56% lower point estimate, inside Criterion's noise threshold | Rejected: it conflicts with the existing 36-shape startup sweep and does not establish a robust win. |

The two retained changes copy zlob's useful *shape*, not its implementation.
Ferralk uses safe queue primitives and keeps unsafe descriptor operations
inside the native backends. Local LIFO processing also preserves the public
contract: unsorted parallel result order is deliberately unspecified. The
ablation benchmark is diagnostic rather than a headline; its collect/count and
path microbenchmarks explain where time can go, while `walker_palamedes` remains
the complete engine comparison.

### Matcher refresh

The matcher lane was run in the same environment. These rows use the current
point estimates and add `wax` and zlob to the existing compiled baselines:

| Case | ferralk | zlob | `globset` | `fast-glob` | `wax` |
| --- | ---: | ---: | ---: | ---: | ---: |
| `common` matching | **10 ns** | 37 ns | 39 ns | 97 ns | 31 ns |
| `common` non-matching | 3 ns | **2 ns** | 39 ns | 106 ns | 31 ns |
| `long_path` matching | **10 ns** | 74 ns | 201 ns | 533 ns | 182 ns |
| `long_path` non-matching | 3 ns | **2 ns** | 200 ns | 544 ns | 180 ns |
| `backtracking` non-matching | **3 ns** | 29 ns | 104 ns | 250 ns | 79 ns |

`wax` is faster than `globset` on the long-path and adversarial rows here, but
that does not erase the API and byte-semantics differences. zlob is competitive
on short non-matches and the adversarial case, while Ferralk remains faster on
the matching and long-path cases. The Ferralk/baseline output is produced by
[`matcher.rs`](../tools/bench/benches/matcher.rs), and the optional zlob rows by
[`matcher_zlob.rs`](../tools/bench/benches/matcher_zlob.rs).

### Node/TypeScript brace catalogs

The matcher refresh now includes the pattern shape that motivated the latest
optimization. One compiled brace expression covers the six common
Node/TypeScript extensions; the catalog arm compiles six independent patterns
and asks them in order. The scoped expression additionally proves the cross
product of three roots and six extensions. All three forms select 768 of the
same 1,024 generated paths.

| Operation | One brace expression | Six-pattern catalog | Scoped brace expression |
| --- | ---: | ---: | ---: |
| Compile | 4.12 µs | — | 15.94 µs |
| First extension/root | 11 ns | **8 ns** | 12 ns |
| Last extension/root | **13 ns** | 32 ns | 25 ns |
| Rejected extension/root | **6 ns** | 23 ns | 10 ns |
| Filter 1,024 paths | **10.74 µs** | 19.86 µs | 36.36 µs |

The catalog can win when its first pattern matches, but its cost depends on
where a suffix appears. The brace trie keeps first and last extensions close
and filters the full list 46% faster than the six-pattern catalog. The scoped
row is not a pure suffix comparison: it also checks the allowed-root set.

### Node.js ecosystem context

[`tools/bench/node`](../tools/bench/node) recreates the same fixture and queries
through Node's built-in glob plus current locked ecosystem packages:

| Package | Locked version | Measured role |
| --- | ---: | --- |
| Node.js `node:fs` glob | 24.19.0 runtime | Sync and async walking |
| `glob` | 13.0.6 | Sync and async walking |
| `fast-glob` | 3.3.3 | Sync and async walking |
| `tinyglobby` | 0.2.17 | Sync and async walking |
| `fdir` + `picomatch` | 6.5.0 + 4.0.7 | Sync and async traversal with caller-side matching |
| `picomatch` | 4.0.7 | Compiled matching |
| `micromatch` | 4.0.8 | Compiled matching |
| `minimatch` | 10.2.6 | Compiled matching |

The walker harness takes two warmups and ten order-rotated samples, and reports
the median. Every candidate must return 7,400 files for the unscoped query and
2,600 for the scoped query before it is timed.

| Node.js walker | `**/*.{ts,tsx}` | `{src,packages}/**/*.{ts,tsx}` |
| --- | ---: | ---: |
| `node:fs` sync | 286.49 ms | 31.51 ms |
| `node:fs` async | 359.11 ms | 43.83 ms |
| `glob` sync | 89.36 ms | 16.14 ms |
| `glob` async | 48.56 ms | **8.08 ms** |
| `fast-glob` sync | 94.41 ms | 13.95 ms |
| `fast-glob` async | 50.73 ms | 8.29 ms |
| `tinyglobby` sync | 90.06 ms | 14.02 ms |
| `tinyglobby` async | 47.34 ms | 8.21 ms |
| `fdir` + `picomatch` sync | 87.31 ms | 80.60 ms |
| `fdir` + `picomatch` async | **47.08 ms** | 42.09 ms |

The fastest Node medians are 47.08 ms unscoped and 8.08 ms scoped. The current
Criterion runs put portable Ferralk at 37.78 and 4.29 ms and native Ferralk at
33.90 and 6.27 ms. That is useful same-host context, not a formal ranking: the
Rust table uses Criterion point estimates while the Node harness reports a
median of ten samples, and the APIs do not offer identical semantics.

The matcher harness takes five warmups and fifteen order-rotated samples of
100,000 matches per case. The adversarial case uses 100 matches per sample so a
backtracking engine cannot make the run unbounded.

| Node.js matcher | Common match | Common reject | Long match | Long reject | Adversarial reject |
| --- | ---: | ---: | ---: | ---: | ---: |
| `picomatch` | 80.9 ns | 90.9 ns | 290.8 ns | **299.7 ns** | 393.1 µs |
| `micromatch` | **80.6 ns** | 91.0 ns | **286.9 ns** | **299.7 ns** | 392.2 µs |
| `minimatch` | 238.3 ns | 219.0 ns | 573.8 ns | 498.8 ns | **388.8 µs** |

Ferralk's corresponding rows are 10/3 ns for the common case, 10/3 ns for the
long case, and 3 ns for the adversarial rejection. This is compiled native Rust
against JavaScript libraries, not a Node binding comparison; it isolates
matcher work and says nothing about interop or application-level throughput.

## The macOS native backend, 2026-08-20

Every lane and every reproduction command in this file left
`--features native-macos` off, so the macOS backend had never appeared in a
number here. Turning it on found it losing to the portable backend it exists to
beat: `getattrlistbulk` assembles a per-entry attribute record, and a listing
keeps an entry's name, is-dir and is-symlink — the two facts `getdirentries64`
already returns, in a record a third the size. Routing the reader to
`getdirentries64` is what the rows below measure.

Four `walker_palamedes` invocations on one host — the same M1 Pro as above,
macOS 26.5.2, rustc 1.97.1, four threads — each with the zlob 1.6.3 arm timed
**inside the same invocation** as its anchor. Read down a column against that
anchor rather than across rows: these ran while the host had other work on it,
which the unscoped serial spreads show plainly (±13.6 ms on the portable row
against ±0.5 ms on the second `getdirentries64` round).

Unscoped, `**/*.{ts,tsx}` — the query that has to read the whole tree:

| Reader | Serial | 4 threads | zlob, same run | Parallel ÷ zlob |
| --- | ---: | ---: | ---: | ---: |
| portable, `std::fs` | 78.4 ms | 35.2 ms | 30.4 ms | 1.16x |
| native, `getattrlistbulk` — before | 81.0 ms | 36.6 ms | 30.1 ms | 1.22x |
| native, `getdirentries64` — after, round 1 | 58.7 ms | 33.9 ms | 29.4 ms | 1.15x |
| native, `getdirentries64` — after, round 2 | 58.6 ms | 33.1 ms | 30.7 ms | **1.08x** |

Scoped, `{src,packages}/**/*.{ts,tsx}` — the query that prunes:

| Reader | Serial | 4 threads |
| --- | ---: | ---: |
| portable, `std::fs` | 12.4 ms | 6.63 ms |
| native, `getattrlistbulk` — before | 13.9 ms | 7.44 ms |
| native, `getdirentries64` — after, round 1 | 11.6 ms | 6.18 ms |
| native, `getdirentries64` — after, round 2 | 11.6 ms | 6.40 ms |

**The serial arm is where the reader shows.** 81.0 ms to 58.7 ms is 28% off a
walk that is nothing but directory reads and classification, and it is the row
with the least to hide behind: one thread, one syscall stream. The bulk reader
was 3% *behind* the portable backend there, which is the finding this section
exists to record.

**The parallel arm moves less because it is not reader-bound.** 36.6 ms to
33.1–33.9 ms is 7–10%, and against the anchor 1.22x to 1.08–1.15x. Four threads
against a warm page cache spend a good share of the walk in the coordination
and classification the reader change does not touch.

**What this does not establish.** One host, one tree shape, a warm cache, and
an APFS volume. `getattrlistbulk` returns more than a name and a type, and a
future walk that wanted those attributes would be reading a different trade;
today nothing downstream of `Listing::push` can accept one. The spread on the
noisier rows is real, which is why the anchor is inside each run and why the
`getdirentries64` measurement was taken twice.

### Final-batch EOF flag, 2026-08-27

Darwin's extended `__getdirentries64` buffer reserves its final four bytes for
`GETDIRENTRIES64_EOF` when the buffer is at least 1024 bytes. A small-directory
C probe on the same APFS setup returned data and this flag on its first call;
the old reader then issued one empty terminal syscall. The native reader now
checks that tail only after parsing `buffer[..byte_count]`, so the flag cannot
expand parser input or weaken record bounds. A final flagged batch now follows
`open + read + close`, rather than `open + read + empty read + close`: one
directory-read syscall is removed for directories that fit their final batch.
This is a syscall-count observation, not a new wall-time benchmark; cache,
tree shape, filesystem and concurrent work determine the realised speedup.

## Selecting without a caller-side matcher

The `caller_match` arms in [`walker.rs`](../tools/bench/benches/walker.rs) model
a caller that keeps a `GlobSet` of its own because the walker cannot express
what it selects. `WildcardMode::SeparatorCrossing` removes that reason for a
catalog of `*.ext` globs: the same 24 patterns become walker includes, and
`walker_includes_crossing` measures the walk with nothing running per entry
outside it. All three arms were checked to select the identical set before
being timed.

Medians of 41 interleaved rounds on one host, both arms inside each round with
the order flipped every round, because this host's load drifted enough during a
sequential criterion run to charge one arm for the other's noise:

| Arm | mini (12 files) | large (5120 files, 2560 selected) |
| --- | ---: | ---: |
| `collect_then_filter` — `GlobSet` on one thread after the walk | 110.6 µs | 6.09 ms |
| `visit_in_worker` — `GlobSet` inside the workers | 111.1 µs | 5.96 ms |
| `walker_includes_crossing` — includes at the walker, no caller matcher | 130.0 µs | 6.15 ms |

At repository scale the three are level: the includes arm lands 3% behind
`visit_in_worker` and inside the spread of both `GlobSet` arms. Handing the
selection to the walker costs nothing measurable, and it removes a matcher the
caller had to keep in sync.

On the 12-file tree the includes arm is ~19 µs slower, and a control arm that
compiles the 24 includes and never walks accounts for it: 12–16 µs. That is
pattern compilation, paid once per walker, which a 12-file tree has no entries
to amortise it over. A caller that builds one walker and reuses it, or walks
anything larger than a toy tree, does not see it.

What this does not show: a catalog of extension globs is the case the extension
prefilter handles well. A pattern set the planner can prove nothing about would
run every include against every entry, which is the shape where a compiled
`GlobSet` should still win.

## When a pool is worth starting

The helper floor decides whether a walk starts threads at all, so what it
weighs is a measurement question. This sweep is the evidence behind
`DIRECTORY_WEIGHT` and `HELPER_WORK_FLOOR` in
[`parallel.rs`](../crates/ferralk/src/parallel.rs).

Thirty-six shapes, directory count against files per directory. Each is walked
process-fresh — one walk per process, because thread startup is the cost under
test and repeated iterations in one process amortise it away — with helpers
forced off and forced on, the two arms alternating every round, medians of 51
rounds. The cell is pooled ÷ serial: **below 1.00 pooling wins**.

| Directories | 0 files each | 1 | 4 | 16 |
| ---: | ---: | ---: | ---: | ---: |
| 4 | | 1.48 | 1.50 | 1.38 |
| 8 | | 1.24 | 1.17 | 1.10 |
| 10 | | 1.08 | 1.15 | 1.02 |
| 12 | | 1.08 | 1.03 | 1.03 |
| 14 | | 1.03 | 1.04 | **0.85** |
| 16 | 1.01 | 0.98 | 1.01 | **0.83** |
| 20 | 0.99 | 0.99 | **0.89** | **0.81** |
| 24 | **0.91** | **0.85** | **0.83** | **0.80** |
| 28 | **0.74** | | | |
| 31 | **0.95** | | | |
| 32 | | **0.86** | **0.85** | **0.71** |
| 40 | **0.70** | | | |
| 48 | | **0.85** | **0.77** | **0.68** |
| 64 | **0.83** | | | |
| 96 | **0.79** | | | |

**Directories cost about twenty times what an entry costs.** Holding the entry
count at 150 and spreading it across more directories takes the walk from 200 µs
at 2 directories to 1374 µs at 75 — 16 µs per directory, with the entries
unchanged — while adding 135 entries to a fixed 9 directories costs 110 µs, or
0.8 µs each. That ratio is why the floor counts a listing as twenty entries.

**The break-even is one number, not one per shape.** The first pooling win
arrives at 24 directories of 1 file, 20 of 4, and 14 of 16 — shapes with little
in common except that the serial walk takes 480 µs (509, 478, 476). Pooling wins
once there is about half a millisecond of walk to divide, which is what it costs
to start and join the threads.

**What the sweep changed.** The floor counted entries alone, which reads a tree
of empty directories as trivial: 24 to 31 empty directories stayed serial
although pooling wins there by up to 26%, and 32 pooled only because 32 names in
the root listing are 32 entries. Weighting the listing removes that cliff and
also stops one shape the old floor pooled by mistake, 14 directories of 4 files.
Decisions were read from a spawn trace rather than inferred from time — a helper
walking empty directories never reaches the visitor, so counting visitor threads
would call a running pool serial.

**What it did not change.** The pair that prompted the issue, 9 directories of
16 files against 17, was already decided correctly. The entry floor never stood
alone: requiring `HELPER_QUEUE_FLOOR` directories still queued when it is met
already weighed directories, just implicitly.

Against this host, the floor now picks the better arm on all 36 shapes; the
previous one missed 4. Every ratio here is one machine's, and the constants are
a judgement about where a thread stops paying for itself, not a threshold any
lane enforces.

## Several roots, one walker or several

The `multi_root` arms in [`walker.rs`](../tools/bench/benches/walker.rs) walk
three trees as one walk and as three, producing the same entries either way.
What differs is how many times the walk pays for its setup: one thread pool
instead of three, one `available_parallelism` query instead of three.

Medians of 41 interleaved rounds, order flipped every round:

| Shape | One walker, three roots | One walker per root |
| --- | ---: | ---: |
| 3 × 8 entries, below the helper floor | **234 µs** | 241 µs |
| 3 × 54 entries, just above the floor | 434 µs | **426 µs** |
| 3 × 2192 entries | 6.73 ms | **6.69 ms** |

**This is not a throughput feature, and the numbers say so.** The saving is a
fixed per-root cost of a few microseconds, which is visible on a walk small
enough for setup to matter — about 3% on the smallest shape, reproduced across
runs — and is lost in the noise of anything repository-sized. Helper threads are
spawned lazily and only once the work floor is crossed, so even the shape built
to isolate pool startup shows no reliable difference: spawning a scoped thread
is cheap next to reading a few hundred directory entries.

What one walker actually buys is structural rather than measurable here: one
helper-spawn decision taken over the whole walk instead of per tree, so three
small roots stay serial where three separate walkers would each weigh
themselves; one independent ancestor-chain root per traversal task; and one
place for the caller's patterns instead of a loop that rebuilds them per root.
The independent chains preserve overlapping, duplicate and alias roots as the
concatenation of their single-root walks while still ending genuine cycles.
Criterion's sequential arms
disagreed with the paired measurement on the smallest shape — it reported the
one-walker arm slower — which is drift between arms measured minutes apart, and
the reason the table above is paired.

## Matcher, against Rust baselines

Wall time per match, same host, one line per benchmark
(`--output-format bencher`). Both baselines are compiled once outside the timed
region, as ferralk's pattern is.

| Benchmark | ferralk | `globset` | `fast-glob` |
| --- | ---: | ---: | ---: |
| `common` matching | **12 ns** | 38 ns | 100 ns |
| `common` non-matching | **3 ns** | 38 ns | 114 ns |
| `literal` matching | **18 ns** | 38 ns | — |
| `literal` non-matching | 17 ns | **9 ns** | — |
| `recursive_casefold` matching | **13 ns** | 28 ns | — |
| `recursive_casefold` non-matching | **3 ns** | 29 ns | — |
| `deterministic` matching | 26 ns | **23 ns** | — |
| `deterministic` non-matching | **11 ns** | 20 ns | — |
| `long_path` matching | **17 ns** | 194 ns | 551 ns |
| `long_path` non-matching | **3 ns** | 193 ns | 554 ns |
| `backtracking` non-matching † | **3 ns** | 102 ns | 258 ns |

† The ferralk cell of that row was re-measured on 2026-08-20, on the same host
under rustc 1.97.1, after the prefilter below landed. The two baseline cells are
the 2026-08-19 numbers for code that did not change.

**The last row used to be the one to keep in view.** On a pattern built to force
backtracking — `a*a*a*a*b` against a run of `a`s — ferralk ran 1837 ns against
`globset`'s 102 ns, because nothing in the general engine consulted the
pattern's trailing literal before exploring: the memoized walk visited the whole
`tokens × path` state space and only then failed on the final `b`.

Three facts are now read off the token IR when a pattern compiles — the leading
run of literal and separator tokens, the trailing run, and the bytes the pattern
consumes with every star empty — and checked before the engine starts. On the
same host, ferralk arms only:

| Benchmark | before | after |
| --- | ---: | ---: |
| `backtracking` non-matching | 1863 ns | **3 ns** |
| `general_ir` non-matching (`src/*[a-z]?*.rs` vs `src/deep/main.txt`) | 294 ns | **4 ns** |
| `general_ir` matching (same pattern vs `src/deep/main.rs`) | **79 ns** | 83 ns |

The middle row is the one that matters for a walker: a traversal filter spends
most of its calls on candidates that do not match. The third row is what the
check costs when it passes — the engine still runs, and the filter is work on
top of it. Every other row of the table above reaches a fast path and is
unchanged to within a nanosecond; the full before/after listing is in the pull
request that introduced this.

`globset` is still ahead on a literal non-match and on one deterministic match.
Everywhere else the byte-first matcher is ahead, most clearly on long paths,
where the baselines pay for path normalization ferralk does not do.

Pattern compilation is measured separately (`compile/*` in the same bench) and
is not compared against the baselines, whose builders accept different syntax.

## Limitations

- **Synthetic fixtures.** Every tree here is generated: uniform file sizes, no
  fragmentation, no permission variety, no network filesystems. A real
  repository differs in ways that matter for I/O.
  [Palamedes adoption](palamedes-adoption.md) is the one record here taken on
  real repositories, and carries its own limitations.
- **Warm cache only.** Nothing here measures a cold page cache, which is what a
  first walk after boot pays.
- **One host per table.** These numbers were taken on one machine. The lanes in
  CI run on shared runners and move with them; that is why nothing gates on
  them.
- **Scope differs between engines.** `globset` and `fast-glob` are matchers,
  `walkdir` is traversal only, `jwalk` is deprecated, `globwalk` is a serial
  wrapper around `walkdir` and `ignore`, `wax` is UTF-8/regex-based, `ignore` is
  a walker with gitignore support, and zlob is a Zig library behind a C ABI with
  its own semantics. They are compared here on the one axis they share, the
  shape and speed of a query, and the corpus is where behaviour is compared.
- **Thread count is fixed at four.** A different count changes the parallel arms
  and would change the ordering on a machine with a different core count.
