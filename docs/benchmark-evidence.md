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
| Matcher, wall time | Compiled-pattern matching and compilation, against `globset` and `fast-glob` | [`matcher.rs`](../tools/bench/benches/matcher.rs), run locally back to back and reported in the pull request | No |
| Walker, wall time | Warm-cache traversal of a synthetic tree, ferralk serial and parallel against `ignore` parallel, one job per backend that ships | [`walker-bench.yml`](../.github/workflows/walker-bench.yml), every pull request, medians in the job summary and as an artifact | No |
| Engine comparison | One repository shape, every engine on the same query and the same file set | [`walker_palamedes.rs`](../tools/bench/benches/walker_palamedes.rs), run on demand | No |
| zlob context | The same matcher and walker shapes against zlob 1.6.3 | [`zlob-benchmark.yml`](../.github/workflows/zlob-benchmark.yml), manual dispatch only | No |

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
cargo bench -p bench --bench walker_palamedes --features zlob-oracle
cargo bench -p bench --bench walker_zlob --features zlob-oracle
cargo bench -p bench --bench matcher_zlob --features zlob-oracle
```

zlob is context, never a baseline anything depends on: its benches are behind
the `zlob-oracle` feature, its workflow is manual dispatch only, and no
automatic lane requires Zig.

The measurements below were taken with:

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
rebuilds the *shape* so the comparison can be repeated: 53,600 files, 2,600 of
them TypeScript sources under `src/` and `packages/`, the rest a `node_modules`
tree of 400 packages, some nesting their own dependencies.

Absolute numbers are not comparable with the RFC's; the ratios between engines
on one host are.

The RFC's question — whether existing Rust crates already recover zlob's
advantage — was also answered on two real repositories rather than a
reconstruction, by a consumer who built the `ignore` arm the comparison needs
and then measured against it over four releases. See
[Palamedes adoption](palamedes-adoption.md).

Every arm is run once before timing and has to return the same file count, so no
arm can be fast by finding less. Unscoped finds 7,400 files, scoped 2,600.

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

## Expanded library comparison on this machine, 2026-08-27

The comparison was refreshed on the current checkout and extended with four
popular Rust libraries:

| Library | Locked version | Role in the comparison | Important scope note |
| --- | ---: | --- | --- |
| [`walkdir`](https://crates.io/crates/walkdir/2.5.0) | 2.5.0 | Serial traversal plus a caller-side `globset` | Traversal only; it does not select paths itself. |
| [`jwalk`](https://crates.io/crates/jwalk/0.9.0) | 0.9.0 | Serial and four-thread traversal plus a caller-side `globset` | The current release is marked deprecated by its package metadata. |
| [`globwalk`](https://crates.io/crates/globwalk/0.9.1) | 0.9.1 | Integrated serial glob walking | Built on `walkdir` and `ignore` overrides. |
| [`wax`](https://crates.io/crates/wax/0.7.0) | 0.7.0 | Integrated serial glob walking and compiled matching | UTF-8/regex-based API; the comparison is valid only for this ASCII fixture. |

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
| Base revision | `f2e5139` plus the benchmark extension in this working tree |
| Toolchain | rustc/cargo 1.95.0-nightly, LLVM 22.1.0 |
| Benchmark stack | Criterion 0.8.2, release profile; exact dependency versions are locked in `Cargo.lock` |
| Threads | 4 for every parallel ferralk, `ignore`, and `jwalk` arm |
| Cache | Warm: the fixture is written immediately before the measurement and then reused by all arms in that invocation |
| Fixture | 53,600 files, 2,600 TypeScript sources under `src/` and `packages/`, and 400 dependency packages under `node_modules/` |
| Commands | `cargo bench -p bench --bench matcher -- --output-format bencher --noplot`; `cargo bench -p bench --bench walker_palamedes -- --output-format bencher --noplot`; the same walker command with `--features native-macos` |

The tables report Criterion's point estimates in milliseconds. They are
indicative wall times, not a performance gate. Lower is better. The native run
emitted one Criterion target-time warning for one of the unscoped `jwalk`
arms; those rows have correspondingly high spread and should not be read as a
precise ranking.

### Portable backend

| Arm | `**/*.{ts,tsx}` | `{src,packages}/**/*.{ts,tsx}` |
| --- | ---: | ---: |
| ferralk, serial | 63.71 ms | 13.70 ms |
| **ferralk, 4 threads** | 45.38 ms | **8.78 ms** |
| `ignore` serial + `globset` | 104.96 ms | 98.93 ms |
| `walkdir` serial + `globset` | 81.16 ms | 103.97 ms |
| `jwalk` serial + `globset` | 86.39 ms | 99.84 ms |
| `jwalk`, 4 threads + `globset` | **44.70 ms** | 45.17 ms |
| `globwalk` serial | 81.62 ms | 86.13 ms |
| `wax` serial | 107.13 ms | 20.95 ms |
| `ignore` parallel + overrides | 58.92 ms | 49.11 ms |
| `ignore` parallel + hand-pruned subtree | — | 10.87 ms |

On the unscoped query, `jwalk` plus caller-side `globset` is 1.5% faster than
Ferralk's four-thread arm in this run (44.70 ms versus 45.38 ms). On the
scoped query, Ferralk is faster because it prunes `node_modules` from the
pattern itself: 8.78 ms versus 20.95 ms for `wax`, 45.17 ms for `jwalk`, and
49.11 ms for `ignore` overrides. The hand-pruned `ignore` arm remains the
closest comparison at 10.87 ms, but it needs an extra caller policy that the
Ferralk pattern already expresses.

### macOS-native backend

| Arm | `**/*.{ts,tsx}` | `{src,packages}/**/*.{ts,tsx}` |
| --- | ---: | ---: |
| ferralk, serial | 62.34 ms | 21.61 ms |
| **ferralk, 4 threads** | **45.20 ms** | **8.14 ms** |
| `ignore` serial + `globset` | 90.16 ms | 92.03 ms |
| `walkdir` serial + `globset` | 83.90 ms | 83.69 ms |
| `jwalk` serial + `globset` | 94.85 ms | 88.62 ms |
| `jwalk`, 4 threads + `globset` | 49.79 ms | 47.86 ms |
| `globwalk` serial | 92.48 ms | 87.96 ms |
| `wax` serial | 111.93 ms | 18.18 ms |
| `ignore` parallel + overrides | 74.34 ms | 49.54 ms |
| `ignore` parallel + hand-pruned subtree | — | 11.03 ms |

The native reader changes Ferralk's serial unscoped result from 63.71 ms to
62.34 ms in this expanded run. The parallel result is effectively level with
the portable path, 45.20 ms versus 45.38 ms; this fixture is then limited more
by scheduling and matching than by the directory reader. The scoped native
Ferralk result is the best measured arm at 8.14 ms, while `wax` is the only
other integrated walker close at 18.18 ms.

The new references answer a comparison question, not an adoption question.
`jwalk` is deprecated, `walkdir` has no matching or pruning policy, `globwalk`
uses a different override contract, and `wax` accepts UTF-8 expressions rather
than Ferralk's arbitrary-byte matcher. Exact semantic compatibility still
comes from the checked-in corpus and caller parity tests, not from these
wall-time rows. zlob was not included in this refresh because Zig is not
installed on the host; its last separately dated context measurements remain
in the existing zlob context and macOS-native sections.

### Matcher refresh

The matcher lane was run in the same environment. These rows use the current
point estimates and add `wax` to the existing compiled baselines:

| Case | ferralk | `globset` | `fast-glob` | `wax` |
| --- | ---: | ---: | ---: | ---: |
| `common` matching | **12 ns** | 39 ns | 100 ns | 32 ns |
| `common` non-matching | **3 ns** | 38 ns | 109 ns | 31 ns |
| `long_path` matching | **12 ns** | 197 ns | 549 ns | 184 ns |
| `long_path` non-matching | **3 ns** | 199 ns | 570 ns | 185 ns |
| `backtracking` non-matching | **4 ns** | 98 ns | 253 ns | 80 ns |

`wax` is faster than `globset` on the long-path and adversarial rows here, but
that does not erase the API and byte-semantics differences. The complete
matcher output, including compilation and path-filter shapes, is produced by
[`matcher.rs`](../tools/bench/benches/matcher.rs).

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
themselves; one visited-directory guard per root traversal, shared only with
that root's descendants; and one place for the caller's patterns instead of a
loop that rebuilds them per root. The independent guards preserve overlapping,
duplicate and alias roots as the concatenation of their single-root walks while
still ending genuine cycles within each root. Criterion's sequential arms
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
