# ADR-0007: Differential corpus as JSONL, oracle as development-time tool

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

The mechanical port (ADR-0002) is only provably correct against recorded zlob
behavior. Keeping zlob (Zig 0.16 + libclang) in every CI run would reinstate
exactly the toolchain burden ferralk exists to remove. zlob 1.6.3 is frozen,
so a scheduled oracle run can never produce new answers. zlob's own test suite
is MIT-licensed and portable 1:1.

## Decision

- **Format:** JSON Lines, one file per topic (`braces.jsonl`, `extglob.jsonl`,
  `ignore.jsonl`, …), one case per line carrying pattern, path, flags,
  expected result, source, and note. Non-UTF-8 bytes use a `\xNN` escape
  convention with a small codec per consuming language.
- **Seed:** zlob's own test suite, ported 1:1.
- **Extension:** differential generation against the live zlob oracle — a
  development-time tool only, run locally or via a manually triggered workflow
  (unpublished `oracle` workspace member with zlob as dev-dependency, excluded
  from default members). Never scheduled.
- **Second reference:** `fast-glob` (oxc) for the common syntax subset; any
  zlob/fast-glob disagreement is itself a corpus case. For ignore semantics
  the oracle is `git check-ignore` (ADR-0006).
- **CI:** replays the checked-in corpus only — no Zig toolchain. After 1.0 the
  oracle retires; the corpus is the test suite.

## Consequences

- Every oracle disagreement becomes a permanent, reviewable corpus case.
- Corpus diffs are part of code review; large generated batches need curation.
- The frozen 1.6.3 contract means later zlob releases do not silently shift
  our expectations.
