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
| threads | `min(available_parallelism(), metadata concurrency ceiling, 256)`; explicit budgets clamp to `1..=256` |

The metadata concurrency ceiling is unbounded except on Apple Silicon macOS,
where it is `hw.perflevel0.cpusperl2` — the performance cores sharing one L2.
A warm walk is not CPU work: 95% of its samples sit in `openat`,
`getdirentries64` and `close`, and on that platform the kernel's namespace
layer serves those fastest when the workers stay inside one performance
cluster. Past it, aggregate throughput falls rather than plateaus.
`docs/benchmark-evidence.md` carries the measurement, including the raw C and
zlob controls that show the knee is the platform's rather than ferralk's.

## Consequences

- Least surprise, POSIX-glob semantics by default; deterministic order and
  ignore handling cost only those who ask for them.
- Consumers like Palamedes configure their policy once in the builder
  (gitignore on, symlinks off, collect errors).
- The thread default is a starting point sized for the walk shape a glob
  walker usually gets, not a reading of the core count. A walk whose per-entry
  matching cost dwarfs its filesystem cost — measured, that means a serial walk
  roughly four times its metadata-only time — is faster with more workers and
  should call `Walker::threads`.
