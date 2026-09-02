# ADR-0017: Caller-owned list API conventions

- **Status:** Accepted
- **Date:** 2026-09-02

## Context

zlob's tool-shaped list API can synthesize the pattern for `NOCHECK`, sorts
results, and treats a leading `./` asymmetrically. Ferralk's Rust API filters
caller-owned values and returns references or indices into that input, so those
conventions would either invent an ownerless result or obscure input identity.

## Decision

Ferralk's list entry points:

- never synthesize a `NOCHECK` result when the caller supplied no candidate;
- preserve caller input order; and
- normalize one conventional leading `./` on both pattern and candidate while
  returning the candidate with its original spelling.

## Consequences

The three list-result divergences cite this ADR. Callers can map every result
back to their input without sorting or spelling changes.
