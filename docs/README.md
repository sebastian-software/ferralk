# Ferralk documentation

Start with the repository [README](../README.md) for installation status and
small working examples. This directory contains the durable technical record.

| Document | Use it for |
| --- | --- |
| [Contributing](../CONTRIBUTING.md) | Repository policy: commit signing, and how a performance claim is evidenced. |
| [Usage guide](usage.md) | Matcher and walker defaults, options, errors, and validation commands. |
| [Compatibility guide](compatibility-guide.md) | Migrating supported zlob 1.6.3 behaviour to the Ferralk API. |
| [Compatibility matrix](compatibility-matrix.md) | A precise capability-by-capability status. |
| [Corpus format](corpus-format.md) | Maintaining or reviewing JSONL behavioural cases. |
| [Benchmark evidence](benchmark-evidence.md) | What is measured, how to reproduce it, and how ferralk compares with `globset`, `fast-glob`, `ignore` and zlob. |
| [Deferred follow-up](external-release-gates.md) | Open work tracked in GitHub after the initial release. |
| [ADRs](adr/README.md) | Accepted architectural decisions and their consequences. |
| [Frozen zlob reference](zlob-1.6.3-reference.md) | Upstream API and provenance baseline. |

The RFC at the repository root is historical design context. The README, usage
guide, compatibility matrix, and ADRs are the preferred references for current
consumer and contributor decisions.
