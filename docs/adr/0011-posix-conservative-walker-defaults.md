# ADR-0011: POSIX-conservative walker defaults

- **Status:** Accepted
- **Date:** 2026-08-18

## Context

ferralk is a library, not a tool like ripgrep: defaults that silently filter
or transform results ("why are files missing?") surprise library consumers.
Everything behavior-changing should be an explicit builder call.

## Decision

| Option | Default |
|---|---|
| `follow_symlinks` | off |
| `respect_git_ignore` | off |
| `*` matches leading-period names | no (`match_hidden` opts in) |
| sorting | off (unsorted, nondeterministic order) |
| `ErrorPolicy` | `Collect` |
| threads | `available_parallelism()` (once the scheduler exists) |

## Consequences

- Least surprise, POSIX-glob semantics by default; deterministic order and
  ignore handling cost only those who ask for them.
- Consumers like Palamedes configure their policy once in the builder
  (gitignore on, symlinks off, collect errors).
