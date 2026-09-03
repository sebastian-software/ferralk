# Ferralk documentation

Start with the repository [README](../README.md): it says what Ferralk is for,
how it compares with the crates you may already use, and shows the two
quick-start examples. This directory holds the durable technical record,
grouped by what you are trying to do.

## Using Ferralk

| Document | Use it for |
| --- | --- |
| [Usage guide](usage.md) | Every walker default and the switch that changes it, the three matcher entry points, error handling, cancellation, and platform notes. Start here after the README. |
| [1.x stability contract](stability.md) | What 1.x promises: public API, corpus semantics, MSRV policy, Windows tier, and the explicit exclusions. |
| [Benchmark evidence](benchmark-evidence.md) | What is measured, how to reproduce it, what scoped queries mean, and how Ferralk compares with Rust, Node.js, and zlob libraries. |
| [Palamedes adoption](palamedes-adoption.md) | A consumer integration measured over four releases on two real repositories, and which finding produced which change here. |

## Coming from another library

| Document | Use it for |
| --- | --- |
| [Compatibility guide](compatibility-guide.md) | Migrating zlob 1.6.3 usage to the Ferralk API, porting patterns from `globset` and `fast-glob`, the deliberate differences, and the audit of every contract change since 0.9.0. |
| [Compatibility matrix](compatibility-matrix.md) | A capability-by-capability status against zlob. |
| [fast-glob reference](fast-glob-reference.md) | The pinned `fast-glob` reference and the divergences the differential fuzzer excludes by shape. |

## Contributing

| Document | Use it for |
| --- | --- |
| [Contributing](../CONTRIBUTING.md) | The canonical preflight, commit conventions, how a performance claim is evidenced, and the 1.0 release checklist. |
| [Corpus format](corpus-format.md) | Maintaining or reviewing JSONL behavioural cases; the corpus is the source of truth for matcher and walker semantics. |
| [Fuzzing](../fuzz/README.md) | The parser, matcher, differential, and native-record fuzz targets and their seeds. |
| [ADRs](adr/README.md) | Accepted architectural decisions and their consequences. They are constraints, not proposals. |
| [Deferred follow-up](external-release-gates.md) | Platform state and the open work tracked in GitHub after the initial release. |

## Historical record

| Document | Use it for |
| --- | --- |
| [Architecture RFC](../RFC-zig-free-zlob-port.md) | The original design: full architecture, goals, and non-goals. Historical context; the ADRs record what was decided. |
| [Frozen zlob reference](zlob-1.6.3-reference.md) | Upstream API and provenance baseline for the compatibility target. |
| [zlob test-suite audit](zlob-test-suite-audit.md) | Which upstream tests Ferralk replays and which are outside the two-crate Rust API. |
| [zlob fnmatch test coverage](zlob-fnmatch-test-coverage.md) | The line-addressable coverage boundary for the upstream matcher tests. |

The README, usage guide, compatibility matrix, and ADRs are the preferred
references for current consumer and contributor decisions.
