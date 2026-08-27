# ADR-0004: MSRV policy — current stable minus two releases

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

Consumers are primarily modern dev tools (Palamedes first), not distro
packages. The byte-matching design relies on recent std APIs (e.g.
`OsStr::as_encoded_bytes`, stable since 1.74). A vague or absent MSRV breaks
library consumers; an overly conservative one costs API ergonomics.

## Decision

MSRV is the current stable minus two releases (~3 months), declared via
`rust-version` in Cargo.toml and verified by a dedicated CI job. A separate
scheduled/manual policy check compares that value with Rust's official stable
channel metadata; it deliberately does not run on ordinary pull requests, so
temporary network failures cannot make routine CI flaky. During 0.x a bump may
land in any minor release; from 1.0 on, bumps are minor-version events with a
changelog entry.

## Consequences

- Modern std APIs are usable without nightly.
- Enterprise/distro users on year-old toolchains are not a target audience.
- The policy is revisited at 1.0.
