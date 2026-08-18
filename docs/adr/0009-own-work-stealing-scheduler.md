# ADR-0009: Own work-stealing scheduler instead of WalkParallel or rayon

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

The Palamedes benchmark puts parallel `ignore` + pruning at 15.74x and zlob at
16.84x over the serial baseline — the scheduler is where the remaining gap and
the API freedom live. `ignore::WalkParallel`'s visitor model cannot cleanly
carry our streaming, sharded-collect, cancellation, and error-policy
semantics; wrapping it would let its design leak into our public API and cap
performance at `ignore`'s ceiling. rayon's global pool conflicts with per-walk
thread limits and cancellation.

## Decision

Build our own scheduler (Phase 3): work stealing via `crossbeam-deque`, lazy
worker spawn (start on the caller thread, add helpers only when parallel work
exists), per-worker path/read/matcher scratch and result shards, a single
cancellation state, and a lossless error channel. The `ignore` crate
contributes only its gitignore rule matcher (ADR-0006).

## Consequences

- This is the project's performance differentiator — and its largest
  concurrency-testing obligation (loom models, stress tests, panic
  propagation).
- Phase 2 ships a single-threaded portable walker first; parallelism is
  layered on, and single- vs multi-thread result equality is a corpus
  invariant.
