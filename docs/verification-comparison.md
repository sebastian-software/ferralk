# Verification depth next to comparable crates

[Benchmark evidence](benchmark-evidence.md) answers how fast Ferralk is. This
answers a different question a reader is entitled to ask before depending on a
young library: how much checkable evidence stands behind its behaviour,
compared with the crates it is measured against.

It is a count of what each project has checked in, taken from the upstream
repositories on 2026-09-04. Counts are a weak proxy for reliability and this
document says where they mislead. Nothing here is a quality judgement of
another project.

## Method

Counting the published crate tarballs would be wrong: `wax` ships none of its
tests to crates.io, and `walkdir` and `globset` ship no separate `tests/`
directory. Every row below is counted from a shallow clone of the upstream
repository instead, scoped to the library rather than to any application
around it.

```sh
git clone --depth 1 https://github.com/BurntSushi/ripgrep      # ignore, globset
git clone --depth 1 https://github.com/BurntSushi/walkdir
git clone --depth 1 https://github.com/olson-sean-k/wax
git clone --depth 1 https://github.com/oxc-project/fast-glob
git clone --depth 1 https://github.com/Byron/jwalk
git clone --depth 1 https://github.com/Gilnaa/globwalk
```

Test functions are `#[test]` attributes. Data-driven cases are counted
separately because two projects here express most of their coverage as data
rather than as functions: `wax` through `rstest`'s `#[case]` parameters, and
Ferralk through the checked-in JSONL corpus. A `#[test]` count alone reports
zero tests for `wax`, which is wrong by more than two orders of magnitude, and
that is the trap this table exists to avoid rather than to set.

## Counts

| Project | Test functions | Data-driven cases | Total | Fuzz targets |
| --- | ---: | ---: | ---: | ---: |
| ferralk | 392 | 822 | **1,214** | 7 |
| `wax` | 0 | 599 | 599 | 0 |
| `ignore` | 106 | — | 106 | — |
| `jwalk` | 52 | — | 52 | 0 |
| `walkdir` | 48 | — | 48 | 0 |
| `globset` | 27 | — | 27 | — |
| `fast-glob` | 24 | — | 24 | 0 |
| `globwalk` | 11 | — | 11 | 0 |

Documentation, counted as Markdown in the same repository scope: Ferralk has 41
documents totalling 5,406 lines, including 17 ADRs. The next largest is
`jwalk` at 3 documents and 636 lines; `wax` has one 495-line README; the
`ignore` crate ships a 59-line README. Continuous integration: 31 checks on a
Ferralk pull request across 11 workflows, against 1 to 5 workflows elsewhere.

## Where these counts mislead

**Field exposure is missing from the table, and it is the strongest evidence
any of these projects has.** `ignore` and `globset` are ripgrep's, exercised by
its 349 end-to-end `rgtest!` cases and by a decade of use at a scale Ferralk
has not approached. A defect surviving that is rarer than a defect surviving
any checked-in suite, this one included. Ferralk is weeks old. Read the table
as "how much is written down", never as "how much is known to work".

**A test count says nothing about what is tested.** A project may reach high
confidence with few, well-chosen cases over a small surface. `walkdir` deals
with traversal alone and has no matching or ignore semantics to specify;
counting it next to a walker that also implements Git's ignore rules compares
different amounts of problem, not different amounts of care.

## What the count does not capture, and what actually differs

The number worth attention is not the total. It is that of the seven projects
above, **none replays its ignore semantics against Git**. Searching every
repository for `check-ignore` or `ls-files` returns no match outside Ferralk.
The others implement Git's rules from a reading of the documentation, which is
the ordinary and reasonable choice.

Ferralk instead treats `git check-ignore` as the oracle, pinned to Git 2.52.0
and replayed in CI on Linux and on Windows, with `git ls-files` covering root
spellings. Where it diverges deliberately, the corpus case carries an ADR
reference or a recorded oracle defect, and all 44 divergences carry exactly
one. See [ADR-0006](adr/0006-git-normative-ignore-semantics.md) and the
[corpus format](corpus-format.md).

That is the difference a consumer can act on. For a library whose real question
is "does this agree with Git?", an oracle answers it and a test count only
suggests an answer.
