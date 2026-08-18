# ADR-0001: Independent port under the ferralk name

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

zlob (Zig, MIT) is the fastest glob engine we evaluated, but its Rust crate
requires Zig 0.16, bindgen, and libclang in the build path. The RFC weighed
three ownership models: contributing a Rust engine upstream, publishing an
independent crate, and shipping a drop-in "zlob-rs" (which would require the
maintainer's agreement and bind us to 100% of zlob's semantics, including its
quirks).

## Decision

ferralk is an independent crate family under sebastian-software, licensed MIT.
zlob 1.6.3 serves as semantic oracle and benchmark bar; compatibility is a
documented profile ("compatible with X, documented divergences Y"), never a
drop-in claim. The zlob maintainer receives a courtesy notice only. zlob's MIT
copyright notice is retained in ported modules and in a NOTICE file.

## Consequences

- No external blockers: naming, corpus ownership, and release cadence are ours.
- We give up the zlob brand and its ecosystem; ferralk must earn adoption on
  its own numbers.
- Every deliberate divergence from zlob must be documented and covered by a
  corpus case.
