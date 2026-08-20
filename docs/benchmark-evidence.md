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
| Walker, wall time | Warm-cache traversal of a synthetic tree, ferralk serial and parallel against `ignore` parallel | [`walker-bench.yml`](../.github/workflows/walker-bench.yml), every pull request, medians in the job summary and as an artifact | No |
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

# Engine comparison on a repository-shaped 53k-file tree.
cargo bench -p bench --bench walker_palamedes
```

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
themselves; one visited-directory set; and one place for the caller's patterns
instead of a loop that rebuilds them per root. Criterion's sequential arms
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
| `backtracking` non-matching | 1837 ns | **102 ns** | 258 ns |

The last row is the one to keep in view: on a pattern built to force
backtracking, ferralk is 18x slower than `globset`, which compiles to a regex
engine with no backtracking blow-up. `globset` is also ahead on a literal
non-match and on one deterministic match. Everywhere else the byte-first matcher
is ahead, most clearly on long paths, where the baselines pay for path
normalization ferralk does not do.

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
  `ignore` is a walker with gitignore support, zlob is a Zig library behind a C
  ABI with its own semantics. They are compared here on the one axis they share,
  the shape and speed of a query, and the corpus is where behaviour is compared.
- **Thread count is fixed at four.** A different count changes the parallel arms
  and would change the ordering on a machine with a different core count.
