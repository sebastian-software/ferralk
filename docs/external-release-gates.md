# External release gates

This register records the remaining milestone items that cannot be completed
by a local source change alone. It keeps an external prerequisite distinct
from an implementation defect and names the evidence already available in the
repository.

| Gate | Current local evidence | Required external action |
| --- | --- | --- |
| M1 CodSpeed matcher budget | Local common-syntax medians and benchmark commands are logged in `MILESTONES.md`; `.github/workflows/zlob-benchmark.yml` defines the remote comparison. | Dispatch the zlob CodSpeed workflow and retain its comparison evidence. |
| First `ferralk-glob` publication | `cargo package` and `cargo publish --dry-run` completed locally; the release workflow validates the locked workspace and publishes it first. | Decide the release version and timing, configure crates.io trusted publishing, then approve a release PR. |
| First `ferralk` publication | The same logical release increments both crates and its exact `ferralk-glob` dependency; the publish workflow orders it after `ferralk-glob`. | Publish `ferralk-glob` first, then use crates.io trusted publishing for `ferralk` at the maintainer-selected version. |
| M3 CodSpeed Walker gate | Local fixture medians and `.github/workflows/zlob-benchmark.yml` cover the intended series. | Dispatch the remote comparison and confirm the 1.0 threshold. |
| M5 macOS native validation | Bounds-checked readers, parser fuzz target, parity option matrix, local median within the 20% zlob target, and macOS native CI on `main` are confirmed. | Run the sanitizer workflow, filesystem corpus, and stable benchmark series; retain p95 evidence. |
| M5 Linux native validation | `getdents64` parser, fuzz target, parity matrix, focused Miri workflow, and Ubuntu native CI on `main` are confirmed. | Run parser Miri, sanitizer/fuzz workflow, filesystem corpus, and stable benchmark series. |
| Benchmark publication and later stable release | Local benchmark suite, CodSpeed workflows, API review, and packaging preflight are in the repository. | Accepted remote benchmark evidence and release/publishing authority at the maintainer-selected time. |

## Deferred, not blocked

M5 native backends are intentionally scheduled after portable 1.0 by
ADR-0010. The macOS name/type `getdirentries64` and `getattrlistbulk` paths,
including record fuzzing and an unsupported-filesystem fallback, are now
implemented. The feature-gated Linux `getdents64` path is locally
cross-compiled and its parser fuzz target is committed; Ubuntu execution,
native parity/sanitizer series, and performance gates remain post-1.0 work.

The Linux metadata decision is settled for this milestone: Ferralk preserves
the opaque `std::fs::Metadata` contract and does not add a redundant `statx`
syscall. Any future native-attribute surface or revised metadata model is a
separate API proposal.

None is a prerequisite for the portable release or evidence of a current
implementation failure.

The Palamedes trial is also deferred to separate downstream work. Its feedback
may inform a later public release, but it is not a blocker for completion of
the portable RFC implementation.
