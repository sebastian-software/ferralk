# Architecture Decision Records

Decisions from the end-to-end design review of 2026-08-18 (see
[RFC](../../RFC-zig-free-zlob-port.md) for the full architecture).

| ADR | Decision |
|---|---|
| [0001](0001-independent-port-under-the-ferralk-name.md) | Independent port under the ferralk name (MIT, no drop-in claim) |
| [0002](0002-hybrid-port-strategy.md) | Hybrid port strategy — mechanical matcher port, fresh walker |
| [0003](0003-two-published-crates-no-c-abi.md) | Two published crates (`ferralk-glob`, `ferralk`), no C ABI |
| [0004](0004-msrv-stable-minus-two.md) | MSRV = current stable minus two releases |
| [0005](0005-byte-matching-wtf8-on-windows.md) | Byte-based matching everywhere, WTF-8 on Windows |
| [0006](0006-git-normative-ignore-semantics.md) | Git-normative ignore semantics (`git check-ignore` oracle) |
| [0007](0007-differential-corpus-and-dev-time-oracle.md) | JSONL differential corpus, oracle as dev-time tool |
| [0008](0008-simd-via-memchr-primitives.md) | SIMD via memchr primitives (Ferroni playbook) |
| [0009](0009-own-work-stealing-scheduler.md) | Own work-stealing scheduler (no WalkParallel, no rayon) |
| [0010](0010-portable-1.0-native-backends-macos-then-linux.md) | Portable-only 1.0; native backends macOS → Linux; Windows tier 2 |
| [0011](0011-posix-conservative-walker-defaults.md) | POSIX-conservative walker defaults |
| [0012](0012-ferroni-repository-blueprint.md) | Ferroni repository blueprint for tooling |
| [0013](0013-no-glob-to-regex-translation.md) | Dedicated glob matcher — no glob-to-regex translation |
| [0014](0014-own-gitignore-rule-matching.md) | Own gitignore rule matching over ferralk-glob (engine half of 0006) |
| [0015](0015-posix-escapes-in-bracket-classes.md) | POSIX escape processing inside bracket classes |
| [0016](0016-shell-star-runs-before-extglobs.md) | Shell grammar for star runs before extglobs |
| [0017](0017-caller-owned-list-api-conventions.md) | Caller-owned list API conventions |

Convention: [Nygard-style ADRs](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions),
numbered sequentially, never rewritten once accepted — superseding decisions
get a new ADR that links back.
