# ADR-0015: POSIX escape processing inside bracket classes

- **Status:** Accepted
- **Date:** 2026-09-02

## Context

zlob 1.6.3 does not process backslash escapes inside bracket classes. Bash,
glibc `fnmatch`, BSD `fnmatch`, and Ferralk instead consume an enabled escape
before interpreting class members and ranges. The difference affects escaped
dashes, closing brackets, range endpoints, and whether the backslash itself is
a member.

## Decision

Ferralk keeps POSIX/Bash-style escape processing inside bracket classes. With
escaping enabled, `\\x` contributes `x` to the class grammar and the backslash
is not a member. The nine `class-*` divergences cite this ADR; zlob's result is
retained only as differential evidence.

## Consequences

- Character-class behaviour agrees with the established shell and C-library
  references used by Ferralk.
- Compatibility with zlob's class parser is deliberately not promised.
