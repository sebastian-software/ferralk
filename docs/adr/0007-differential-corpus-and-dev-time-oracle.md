# ADR-0007: Differential corpus as JSONL, oracle as development-time tool

- **Status:** Accepted; amended 2026-09-02
- **Date:** 2026-08-18

## Context

The mechanical port (ADR-0002) is only provably correct against recorded zlob
behavior. Keeping zlob (Zig 0.16 + libclang) in every CI run would reinstate
exactly the toolchain burden ferralk exists to remove. zlob 1.6.3 is frozen,
but the adapter, corpus routing, pinned build inputs, and CI environment are
not: they can regress even when the reference answers do not change. zlob's
own test suite is MIT-licensed and portable 1:1.

## Decision

- **Format:** JSON Lines, one file per topic (`braces.jsonl`, `extglob.jsonl`,
  `ignore.jsonl`, …), one case per line carrying pattern, path, flags,
  expected result, source, and note. Non-UTF-8 bytes use a `\xNN` escape
  convention with a small codec per consuming language.
- **Seed:** zlob's own test suite, ported 1:1.
- **Extension:** differential generation against the live zlob oracle — a
  development-time tool only (unpublished `oracle` workspace member with zlob
  as dev-dependency, excluded from default members). Its dedicated workflow
  runs weekly, on manual dispatch, and for pull requests changing the corpus or
  oracle adapter. It remains outside the canonical Rust-only CI lanes.
- **Second reference:** `fast-glob` (oxc) for the common syntax subset; any
  zlob/fast-glob disagreement is itself a corpus case. For ignore semantics
  the oracle is `git check-ignore` (ADR-0006).
- **CI:** canonical pull-request lanes replay the checked-in corpus only — no
  Zig toolchain. The separate path-filtered oracle workflow is an audit lane,
  not a dependency of ordinary Rust changes. After 1.0 the oracle retires; the
  corpus is the test suite.

## Consequences

- Every oracle disagreement becomes a permanent, reviewable corpus case.
- Corpus diffs are part of code review; large generated batches need curation.
- The frozen 1.6.3 contract means later zlob releases do not silently shift
  our expectations.
- The weekly audit detects broken bindings, toolchain drift, and silently
  skipped corpus cases without imposing Zig on every pull request.
