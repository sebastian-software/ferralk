# ADR-0016: Shell grammar for star runs before extglobs

- **Status:** Accepted
- **Date:** 2026-09-02

## Context

In a run immediately followed by `(`, zlob greedily collapses every star before
checking whether the final one opens a `*(` extglob. Bash and ksh instead give
the final star to the group opener and leave earlier stars as ordinary wildcard
syntax.

## Decision

Ferralk follows the Bash/ksh grammar. The last star in a run before `(` opens a
zero-or-more extglob; preceding stars remain ordinary wildcards.

## Consequences

- `**(a)`, `a**(b)`, and `***(a)` retain their shell-compatible readings.
- The recorded zlob divergences cite this ADR and are part of the 1.0 contract.
