# External release gates

This register records the remaining milestone items that cannot be completed
by a local source change alone. It keeps an external prerequisite distinct
from an implementation defect and names the evidence already available in the
repository.

| Gate | Current local evidence | Required external action |
| --- | --- | --- |
| Courtesy notice to zlob maintainer | ADR-0001 records the independent-port intent and attribution. | A maintainer-approved message sent to the upstream maintainer. |
| M1 CodSpeed matcher budget | Local common-syntax medians and benchmark commands are logged in `MILESTONES.md`; `.github/workflows/zlob-benchmark.yml` defines the remote comparison. | Dispatch the zlob CodSpeed workflow and retain its comparison evidence. |
| `ferralk-glob` 0.1 publication | `cargo package` and `cargo publish --dry-run` completed locally; the dry run is recorded in `MILESTONES.md`. | Crates.io maintainer authority to publish `ferralk-glob` 0.1. |
| `ferralk` 0.1 publication | Cargo's local package verification correctly waits for the exact `ferralk-glob` version to exist on crates.io. | Publish `ferralk-glob` first, then publish `ferralk` with maintainer authority. |
| M3 CodSpeed Walker gate | Local fixture medians and `.github/workflows/zlob-benchmark.yml` cover the intended series. | Dispatch the remote comparison and confirm the 1.0 threshold. |
| M5 macOS native validation | Bounds-checked readers, parser fuzz target, parity option matrix, and a local median within the 20% zlob target are committed. | Run the macOS native CI, sanitizer workflow, filesystem corpus, and stable benchmark series; retain p95 evidence. |
| M5 Linux native validation | `getdents64` parser, fuzz target, parity matrix, and focused Miri workflow are committed; the test configuration cross-checks from macOS. | Run Ubuntu native CI, parser Miri, sanitizer/fuzz workflow, filesystem corpus, and stable benchmark series. |
| M5 Linux `statx` | The native reader preserves `WalkOptions::metadata(true)` by returning `std::fs::Metadata`; raw `statx` data cannot construct that opaque type. | Approve a native-attribute public API or revised metadata contract before adding `statx`. |
| Downstream Palamedes trial | Public API, compatibility guide, package preflight, and local benchmarks are available. | Access to the Palamedes integration target and its maintainer feedback. |
| Benchmark publication and 1.0 release | Local benchmark suite, CodSpeed workflows, API review, and packaging preflight are in the repository. | Accepted remote benchmark evidence and release/publishing authority. |

## Deferred, not blocked

M5 native backends are intentionally scheduled after portable 1.0 by
ADR-0010. The macOS name/type `getdirentries64` and `getattrlistbulk` paths,
including record fuzzing and an unsupported-filesystem fallback, are now
implemented. The feature-gated Linux `getdents64` path is locally
cross-compiled and its parser fuzz target is committed; Ubuntu execution,
`statx`, native parity/sanitizer series, and performance gates remain post-1.0
work.

The Linux `statx` checkbox also has an explicit API dependency: Ferralk's
current `WalkOptions::metadata(true)` contract returns opaque
`std::fs::Metadata`, which cannot be constructed from raw `statx` fields.
Adding a `statx` call before the required portable metadata query would add a
second syscall without improving the public result. A future native-attribute
surface or revised metadata model is therefore required before that checkbox
can be completed; the current native reader intentionally preserves the
correct portable metadata contract instead.
None is a prerequisite for the portable release or evidence of a current
implementation failure.
