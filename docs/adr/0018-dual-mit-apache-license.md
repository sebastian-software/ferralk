# ADR-0018: Dual MIT or Apache-2.0 license

- **Status:** Accepted
- **Date:** 2026-09-06

## Context

[ADR-0001](0001-independent-port-under-the-ferralk-name.md) licensed the
independent port MIT, matching zlob's own terms. The Ferramenta family review
of 2026-09 (decision D5 in `sebastian-software/ferramenta#6`) settled on
`MIT OR Apache-2.0` for every family crate that is not itself a port of a
differently licensed original. That pairing is the Rust ecosystem's default:
MIT keeps the permissive terms this project started from, and Apache-2.0 adds
an express patent grant and a contribution clause that corporate consumers ask
for. Publishing under only one of the two makes Ferralk the odd dependency in
a graph where nearly everything else offers both.

Cargo packages a crate directory, not the repository root, so a license file
that only exists at the root is missing from the published `.crate` tarball.

## Decision

Both published crates carry `license = "MIT OR Apache-2.0"`; a consumer picks
either license. The copyright holder is Sebastian Software GmbH.
`LICENSE-MIT` and `LICENSE-APACHE` live at the repository root and, byte for
byte, in `crates/ferralk/` and `crates/ferralk-glob/`; the doctest harness
asserts the copies stay identical.

This supersedes the license sentence of ADR-0001. The rest of that decision —
independent port, no drop-in claim, zlob credited in the README and NOTICE —
is unchanged, and the terms of any third-party notice a ported file carries
are unaffected by the license this project offers for its own work.

## Consequences

- Contributions are offered under both licenses; a contributor who cannot
  grant one of them cannot contribute, which is the price of the pairing.
- The Apache-2.0 patent grant now covers consumers, which is the practical
  reason the family chose the pair.
- Any future port of third-party source keeps its original notice next to the
  ported file and in NOTICE, exactly as ADR-0001 requires.
- The allowed licenses of the dependency graph are enforced separately, by the
  `deny.toml` allow-list rather than by this decision.
