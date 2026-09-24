# Ferralk 1.x stability contract

This document defines the compatibility promise that starts with Ferralk 1.0.
It is intentionally narrower than "every observable detail stays unchanged":
the items below are the surfaces consumers may rely on across compatible 1.x
releases.

## Covered by semantic versioning

- The public Rust API of the default features of `ferralk` and
  `ferralk-glob`, as listed in [`api/`](api/). CI regenerates those listings
  with `cargo public-api`; `cargo-semver-checks` independently guards removals
  and incompatible signature changes.
- Matcher and walker selection semantics recorded by the checked-in JSONL
  corpus, including byte handling, the three matcher entry points, path-list
  ordering, walker pattern rewriting, and Git-ignore verdicts. A deliberate
  external-oracle difference carries an ADR or an oracle-defect marker.
- The entry-point rules in the
  [compatibility guide](compatibility-guide.md#matcher): `is_match` compares a
  whole byte sequence, `is_match_path` preserves the zlob list-filter position
  rules, and `is_match_glob_path` keeps every ordinary wildcard component-local.
  Extglob syntax changes none of those policies.
- The MSRV policy in [ADR-0004](adr/0004-msrv-stable-minus-two.md): the minimum
  supported Rust version is current stable minus two releases. Starting with
  1.0, an MSRV bump is a minor release with a changelog entry.
- Windows as a tier-2 portable target: the `std::fs` backend and full matching
  correctness are tested in CI. Windows has no native backend commitment.

## Explicitly outside the 1.x contract

- Performance, benchmark numbers, allocation counts, descriptor budgets, and
  internal batching or scheduling mechanisms. They are measured for
  regressions but are not compatibility promises.
- The `native-linux` and `native-macos` feature names and everything selected
  by them. They are experimental backend work under ADR-0010 and may be
  changed or removed in a 1.x minor release. The default portable backend is
  the stable contract.
- The `unstable-test-hooks` feature and every item enabled only by it. It
  exists for the repository's corpus and fuzz harnesses and is not a consumer
  API.
- Human-readable diagnostic text: `PatternError::message()`, the `Display`
  output of `PatternError`, `PatternSetError`, and `WalkError`, and the
  messages of underlying `std::io::Error` values.
  Corpus `error_message` fields keep diagnostics reviewable inside this
  repository, but do not turn the wording into a semver promise. Use
  `PatternError::offset()`, `PatternSetError::index()`, and
  `WalkError::operation()` for program logic.

## Consumer pattern contract

For a program that passes its own users' patterns to Ferralk, the
[consumer pattern contract](consumer-contract.md) spells out what the bullets
above mean for that program. It shows which syntax each `PatternOptions`
preset and entry point covers and how to validate a user's pattern with
`Pattern::validate` or `Walker::try_include`. It also covers escaping literal
text with `ferralk_glob::escape`, and porting from `globset`. In short:

- Everything a pattern means under a given dialect and entry point is
  covered, including the walker's own rules.
- `escape` and `escape_str` promise a literal result under every option
  combination with escaping enabled. The exact set of escaped bytes is not
  promised: it grows if the pattern language gains syntax.
- `PatternError::offset()` is stable program-facing error information.
  `message()` and `Display` are not, as listed below.

## Public enum policy

| Enum | 1.0 decision |
| --- | --- |
| `ErrorPolicy` | Exhaustive. Its three policy outcomes are a closed set. |
| `WildcardMode` | Non-exhaustive; matching modes may grow. |
| `WalkEntryKind` | Non-exhaustive; a backend may expose more filesystem kinds. |
| `Verdict` | Non-exhaustive; visitor control may gain another outcome. |
| `WalkOperation` | Non-exhaustive; new recoverable operations may be reported. |
| `WalkerPathViability` | Non-exhaustive; the matcher-to-walker analysis may learn another rejection class. |

`WalkOperation::as_str()` is the stable machine-readable spelling of the
typed operation. Cycle-key construction reports the operation actually used:
`Metadata` on Unix and `Canonicalize` on other platforms. That platform split
is intentional; the underlying `io::Error` remains available as the error
source.

`Pattern` deliberately does not implement `PartialEq` or `Eq`: equality of a
compiled representation is not matcher semantics. `PatternError::new` remains
public so another path-consuming layer can return the same error type with a
caller-source byte offset.

The `#[doc(hidden)]` `WalkerPathViability` type and the associated `Pattern`
accessors remain public because `ferralk` consumes them across the crate
boundary. The simplified `cargo public-api` listing omits doc-hidden items, so
`cargo-semver-checks` provides their automated compatibility guard and this
paragraph records the explicit decision to support them. All fuzz and
corpus-only hidden exports are instead behind `unstable-test-hooks`.

## Windows evidence and limits

CI replays the Git-ignore corpus with Git for Windows and compares 21 root
spellings from three working directories with `git ls-files`. The spellings
cover `.`, `./`, trailing and repeated separators, nested roots, and lexical
`.`/`..` components. Unix CI covers the additional symlink-root cases.

Known Windows gaps are junction-specific repository discovery, drive-relative
paths such as `C:src`, and repositories whose correctness depends on a
case-insensitive filesystem rather than Git's explicit `core.ignoreCase`
setting. These gaps do not weaken byte-pattern matching itself; they identify
filesystem shapes for which the portable walker has no differential oracle
yet.

## Releasing 1.0

The mechanical checklist lives in
[`CONTRIBUTING.md`](../CONTRIBUTING.md#10-release-checklist). The planned
cadence was two consecutive adversarial review rounds without a
consumer-visible breaking change, then `1.0.0-rc.1`, then one more clean round
before `1.0.0`.

`1.0.0-rc.1` was cut before those rounds, and the round run against it
(2026-09-04) was not clean: it found four consumer-visible defects (#393–#396).
The 1.0 readiness review of 2026-09-24 added decisions that could only be made
before the freeze — rejecting a leading `!` in walker patterns (#397), a
recursive `**` only as a whole path component (#419, ADR-0020), and excludes
that apply inside hidden directories (#424). All of them landed as `!` changes
listed in the
[contract-change audit](compatibility-guide.md#contract-change-audit-since-090).
On 2026-09-24 the maintainer decided to release `1.0.0` directly after that
round of fixes instead of cutting another candidate and waiting for a further
clean round; later defects ship as 1.x patch releases where they are fixes to
the documented contract, and as a major release where they would change it.
From `1.0.0` on, this contract is in force.
