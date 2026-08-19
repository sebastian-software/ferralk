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
| Matcher, simulation | Instruction counts for compiled-pattern matching and compilation, against `globset` and `fast-glob` | [`codspeed.yml`](../.github/workflows/codspeed.yml), every pull request, reported to [CodSpeed](https://app.codspeed.io/sebastian-software/ferralk) | No |
| Walker, wall time | Warm-cache traversal of a synthetic tree, ferralk serial and parallel against `ignore` parallel | [`walker-bench.yml`](../.github/workflows/walker-bench.yml), every pull request, medians in the job summary and as an artifact | No |
| Engine comparison | One repository shape, every engine on the same query and the same file set | [`walker_palamedes.rs`](../tools/bench/benches/walker_palamedes.rs), run on demand | No |
| zlob context | The same matcher and walker shapes against zlob 1.6.3 | [`zlob-benchmark.yml`](../.github/workflows/zlob-benchmark.yml), manual dispatch only | No |

**Why two instruments.** CodSpeed's simulation instrument counts instructions
in a virtual machine. That is stable enough to compare two revisions of the same
matcher on a shared runner, which is why the matcher lane lives there. It
serializes threads and does not model syscall cost, so a parallel-versus-serial
walker comparison in that instrument measures instruction count rather than
speedup; the walker therefore measures wall time instead. CodSpeed's own
walltime instrument would be the natural home for it, but that requires a
dedicated `codspeed-macro` runner this repository does not have.

**What each lane does not establish.** The matcher lane says nothing about
syscall-bound work, and instruction counts are not wall time. The walker lanes
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
