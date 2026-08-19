# External release gates

This register records the remaining milestone items that cannot be completed
by a local source change alone. It keeps an external prerequisite distinct
from an implementation defect and names the evidence already available in the
repository.

| Gate | Current local evidence | Required external action |
| --- | --- | --- |
| Courtesy notice to zlob maintainer | ADR-0001 records the independent-port intent and attribution. | A maintainer-approved message sent to the upstream maintainer. |
| M0 complete zlob suite | `docs/zlob-test-suite-audit.md` covers the public Rust-shaped semantics and identifies C ABI, loader, visitor-callback, and C error-callback surfaces without a Ferralk equivalent. | A scope decision accepting the Rust-only boundary, or approval to build a compatibility layer. |
| M1 CodSpeed matcher budget | Local common-syntax medians and benchmark commands are logged in `MILESTONES.md`; `.github/workflows/zlob-benchmark.yml` defines the remote comparison. | Dispatch the zlob CodSpeed workflow and retain its comparison evidence. |
| `ferralk-glob` 0.1 publication | `cargo package` and `cargo publish --dry-run` completed locally; the dry run is recorded in `MILESTONES.md`. | Crates.io maintainer authority to publish `ferralk-glob` 0.1. |
| `ferralk` 0.1 publication | Cargo's local package verification correctly waits for the exact `ferralk-glob` version to exist on crates.io. | Publish `ferralk-glob` first, then publish `ferralk` with maintainer authority. |
| M3 CodSpeed Walker gate | Local fixture medians and `.github/workflows/zlob-benchmark.yml` cover the intended series. | Dispatch the remote comparison and confirm the 1.0 threshold. |
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
None is a prerequisite for the portable release or evidence of a current
implementation failure.
