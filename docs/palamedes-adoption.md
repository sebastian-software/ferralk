# Palamedes adoption

One consumer integrated ferralk for source discovery over four measurement
rounds, from a trial that concluded *do not adopt* to a shipped dependency. This
records what was measured, which finding produced which change, and what the
result does not establish.

It is the only evidence here taken on real repositories rather than generated
fixtures. [Benchmark evidence](benchmark-evidence.md) covers the synthetic
lanes and the reproducible comparisons; this covers the one caller.

The integration is [sebastian-software/palamedes#878](https://github.com/sebastian-software/palamedes/pull/878);
the round-by-round reports are on [ferralk#13](https://github.com/sebastian-software/ferralk/issues/13).
Neither is ferralk's to merge, and none of it gates a ferralk release.

## The four rounds

Ratios are against hand-pruned parallel `ignore` — `ignore` parallel with
`filter_entry` subtree pruning and per-worker shards, which is what
[palamedes#875](https://github.com/sebastian-software/palamedes/issues/875)
proposed building on the stack that caller already had. That is the arm ferralk
had to beat to be worth a dependency at all, and it is not the code palamedes
was running.

| Round | ferralk | palamedes | monorepo | What changed on the caller's side |
| --- | --- | ---: | ---: | --- |
| 1 | 0.1.2 | **0.77x** | **0.68x** | `collect()` and a `GlobSet` post-filter, single-threaded |
| 2 | 0.3.0 | 1.07x | 1.05x | `visit()` moved matching onto the producing worker |
| 3 | 0.4.0 | 1.40x | 1.24x | no caller-side matcher |
| **4** | **0.5.0** | **1.56x** | **1.43x** | no caller-side filter either |

Round 1's verdict was *no measured case for ferralk*: level serially, behind in
parallel, and silently dropping 18–50% of the files it was asked for. The
sequence only worked because that round reported the loss rather than looking
for a framing in which ferralk won.

### Where round 4 lands, in absolute terms

The palamedes repository — 84,592 files on disk, 986 discovered, 6,046
surviving the prune:

| Arm | Median | vs the walker it replaces | vs #875 x4 |
| --- | ---: | ---: | ---: |
| `ignore` serial — what palamedes had | 643.6 ms | 1.00x | 0.04x |
| `ignore` pruned x4 — the #875 plan | 24.1 ms | 26.7x | 1.00x |
| ferralk serial | 29.0 ms | 22.2x | 0.83x |
| **ferralk x4** | **15.5 ms** | **41.5x** | **1.56x** |

A React/Next monorepo — 127,083 files on disk, 7,286 discovered, 14,947
surviving the prune:

| Arm | Median | vs the walker it replaces | vs #875 x4 |
| --- | ---: | ---: | ---: |
| `ignore` serial — what palamedes had | 845.5 ms | 1.00x | 0.05x |
| `ignore` pruned x4 — the #875 plan | 40.5 ms | 20.9x | 1.00x |
| ferralk serial | 61.6 ms | 13.7x | 0.66x |
| **ferralk x4** | **28.4 ms** | **29.8x** | **1.43x** |

Most of the two orders of magnitude over the previous walker is not the walker
at all — it is not descending into `node_modules`. The `ignore` pruned arm gets
the same win from the same idea, which is why it, and not the old code, is the
comparison that decides anything.

### Method

Warm cache. Median of 9 rounds per arm, arms interleaved in one process over
the same tree, `--release`, each reported figure the median of 5 full runs on an
idle machine. Apple M1 Pro, 10 cores, macOS 26.5.2, rustc 1.95.0, 4 walker
threads.

Round 2 is a caution worth keeping. It was first reported as 1.12x / 1.05x and
corrected to **1.07x / 1.05x** once the earlier run turned out to have shared
cores with a Time Machine backup and — with some irony — one of ferralk's own
walker benches. The baseline arm was the one that suffered, so the error
flattered ferralk. Interleaving the arms inside one process is what makes a
busy stretch land on all of them instead of on whichever ran during it; a
control arm that the change under test cannot affect is what makes the
disturbance visible at all.

## What the trial found, and what it changed

Every round's friction list became issues, and the next round measured the
result. This is the mapping.

### Round 1 → #63, #64, #65, #49

- **[#63](https://github.com/sebastian-software/ferralk/issues/63) `match_hidden` on the `Walker`.** `PatternOptions::match_hidden`
  existed but nothing on `Walker` reached it. That single gap was the *entire*
  parity divergence: 179 files under `site/.react-router/` and 3,655 under
  `.claude/`, `.agents/` and a nested worktree, every one of them a path
  component starting with `.`. `WalkOptions::skip_hidden(false)` does not help —
  it governs traversal, not what a wildcard may cover.
- **[#64](https://github.com/sebastian-software/ferralk/issues/64) parallel per-entry visitor.** `collect()` materialised every entry
  and returned it, so a caller's own matcher ran single-threaded over the whole
  result while `stream()` was serial by design. That is what cost the parallel
  arms. The trial carried its own controlled proof: an arm doing the *identical*
  traversal but matching in-worker ran 32.9 → 20.1 ms and 62.3 → 35.2 ms.
- **[#65](https://github.com/sebastian-software/ferralk/issues/65) builder ergonomics.** `include`/`exclude` consumed `self` and
  returned `Result<Self, _>`, so a rejected pattern ate the builder and skipping
  one bad pattern meant cloning the `Walker` per pattern.
- **[#49](https://github.com/sebastian-software/ferralk/issues/49) ferralk's own gitignore engine.** ferralk depended on `ignore`, so
  adopting it *added* crates rather than replacing any. Owning the engine is
  what later let `cargo tree -i ignore -e normal` print nothing for palamedes'
  production tree.

### Round 2 → #73, #76

- **[#73](https://github.com/sebastian-software/ferralk/issues/73) entry materialisation.** The visitor from #64 closed most of the gap
  but not all of it, and the trial supplied two real-caller points that matched
  ferralk's own synthetic curve: ~6,000 surviving entries → 1.07x, ~15,000 →
  1.05x, against ferralk's own measurement of 25,600 → 0.96x. Monotone in
  surviving entries, and consistent with the cause — discovery discards five of
  every six entries on the palamedes repo and one of every two on the monorepo,
  and each discard paid for an owned `PathBuf` first.
- **[#76](https://github.com/sebastian-software/ferralk/issues/76) the helper-spawn floor.** Below the floor the earlier fix was
  complete — 3 directories, 1.60x, no pool penalty, against `ignore`'s 0.18x on
  the same tree. Just above it a residual remained: at 16 directories holding 12
  files the pool started and could not pay for itself. The floor counted
  directories read, which says nothing about how much work is in them. Two
  ferralk-side follow-ups came out of that thread,
  [#81](https://github.com/sebastian-software/ferralk/issues/81) and
  [#88](https://github.com/sebastian-software/ferralk/issues/88).

### Round 3 → #77, #78, #79

Round 3 was the first arrangement in which ferralk's matcher decided alone, and
the three issues it named are what made that possible:

- **[#79](https://github.com/sebastian-software/ferralk/issues/79) separator-crossing wildcard mode** retired both `GlobSet`s and the
  matching body of the visitor.
- **[#78](https://github.com/sebastian-software/ferralk/issues/78) absolute include/exclude patterns** deleted the caller's
  hand-written absolute-to-relative rewrite and its five unit tests.
- **[#77](https://github.com/sebastian-software/ferralk/issues/77) multi-root walks** removed the per-root loop that built one
  `Walker` and one thread pool per root.

### Round 4 → #89, and a trap that was the caller's

- **[#89](https://github.com/sebastian-software/ferralk/issues/89) `resolve_symlink_kind`.** The visitor had survived three rounds for
  one reason: `Path::is_file` follows a symlink, so a link to a file is a source
  and a broken one is not, while an entry kind describes the link. Classifying a
  symlink by its target inside the walk deleted the last caller-side filter.
- **[#94](https://github.com/sebastian-software/ferralk/issues/94) Windows backslash patterns** is the one finding still open, and it
  is a documentation gap rather than a defect. palamedes builds patterns by
  joining `PathBuf`s, so on Windows they carry `\`, which a walker pattern reads
  as an escape: `C:\repo\app\**` compiled to the literal `C:repoapp**` and
  selected nothing. No error, just an empty result — `globset` had normalised
  this internally, so it only surfaced once the walker did the matching, as 19
  failures on the windows-2025 CI leg. The caller's fix is one line. What makes
  it worth an issue on this side is that a rejected pattern is loud and this one
  was silent.

## Parity, and the episode in the middle

Parity was checked path by path against the `ignore` + `globset` implementation
that stood before the branch, on four trees:

| Tree | files on disk | expected | round 4 |
| --- | ---: | ---: | ---: |
| tiny | 7 | 6 | 6 |
| synthetic edge cases | 40 | 12 | 12 |
| palamedes | 84,592 | 986 | 986 |
| React/Next monorepo | 127,083 | 7,286 | 7,286 |

The claim strengthened as the arrangement simplified. In rounds 1 and 2 a
`GlobSet` stood behind ferralk, so parity only ever proved the backstop worked.
From round 3 there was nothing behind it, and exact agreement means ferralk's
matcher and `globset` read this caller's real pattern catalog the same way.

**The 13-versus-12 episode is the part worth recording.** Between 0.4.0 and
0.5.0 the branch did diverge: the synthetic edge tree discovered 13 files
instead of 12, because a broken symlink counted as a source. That was #89, open
at the time; it was parked with `#[ignore]`d tests rather than patched around
locally, and both went green unchanged when 0.5.0 landed.

The two real repositories never showed it. palamedes stayed at 986 and the
monorepo at 7,286 through the entire window, because neither happens to contain
a broken symlink where a source pattern would reach it. Only the hand-built
edge-case tree — hidden files, hidden directories, `node_modules` under a hidden
directory, gitignored sources, `.git` contents, a symlink to a file, a symlink to
a directory, a broken symlink — caught it.

That is the same lesson as round 1's 179 missing files, which were invisible to
palamedes' entire test suite because no existing test used a hidden source file.
A large real tree is a weak oracle: it exercises whatever it happens to contain,
and it is silent about everything else. Both defects were found by a corpus
built on purpose to contain the awkward cases, and both would have shipped had
the check been a file count rather than a path-by-path diff.

## What this does not establish

- **Warm cache only.** Every figure describes a warm page cache. The first walk
  after boot is a different measurement and was not taken.
- **One host, one architecture.** Apple M1 Pro, macOS, arm64. There are no Linux
  or Windows numbers; Windows appears in this record only as the CI failure that
  found #94.
- **Symlink-heavy trees are unmeasured.** The 0.4.0 → 0.5.0 step removed a
  `stat` per surviving entry and `resolve_symlink_kind` pays one back per
  *symlink* entry. These trees have almost none, so the trade was free here.
  Nothing measured says where it turns.
- **One caller, one catalog shape.** palamedes' generated patterns are all
  `**`-rooted, which cannot distinguish the two wildcard readings — telling them
  apart needed a hand-written single-star include in a fixture. A real tree would
  not have caught a wrong default.
- **Two trees is a small sample**, both of them JavaScript/TypeScript
  repositories whose file count is dominated by `node_modules`. That shape is
  what makes subtree pruning decisive; a tree without it would rank differently.
- **`globset` did not leave the caller.** Discovery no longer uses it, but
  palamedes' file watcher still does, so it remains in that tree.

## The integration

The whole of discovery, in round 4's form:

```text
Walker::new(first)
    .wildcard_mode(WildcardMode::SeparatorCrossing)
    .match_hidden(true)
    .threads(discovery_threads(config))
    .error_policy(ErrorPolicy::Skip)
    .options(WalkOptions::default().files_only(true).resolve_symlink_kind(true))
    .add_roots(rest.iter())?
// plus the catalog's include and exclude patterns, absolute and unmodified
// (each `include`/`exclude` call returns Result and wants `?` too), then
// collect()
```

Three switches make ferralk read that catalog the way the previous stack did,
and each has a test that fails without it: `wildcard_mode(SeparatorCrossing)`
because `globset` as palamedes builds it lets an ordinary wildcard cross `/`,
`match_hidden(true)` because `globset` wildcards cover a leading period and this
caller discovers hidden sources deliberately, and `resolve_symlink_kind(true)`
for the `Path::is_file` reading. Builder order does not matter for any of
them: `wildcard_mode` is consulted when entries are matched, not when patterns
compile, so setting it before or after the pattern calls yields the same walk.

### The line count went up

Worth stating because the summary invites the opposite assumption. In the
caller's discovery module, production code went from **136 lines to 148** — 12
lines *larger* than before ferralk existed in it.

What came out were concepts, not lines: both `GlobSet`s, the hand-written
pattern rewrite and its five unit tests, the per-root loop, and the
`Walker::clone()` per pattern. What went in was the builder chain, root
resolution, the Windows respelling, and — mostly — surfacing a rejected pattern
as an error instead of silently skipping it. The previous arrangement could
afford to ignore a pattern it could not rewrite, because a second matcher caught
it anyway; with one engine there is no second chance. That is a correctness
improvement priced in lines, and the honest way to report it is as a cost.
