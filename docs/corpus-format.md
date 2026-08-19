# Corpus format

The checked-in corpus is the executable compatibility contract. Each topic is
a UTF-8 JSON Lines (`.jsonl`) file under [`../corpus`](../corpus). One
non-empty line is one independently replayable case. CI validates every file
with `cargo run -p harness -- corpus`; it never invokes Zig or the live zlob
oracle.

## Record shape

Every record conforms to [`corpus.schema.json`](corpus.schema.json):

| Field | Required | Meaning |
|---|---:|---|
| `id` | yes | Stable, topic-local identifier, for example `wildcard-001`. |
| `kind` | no | `matcher` (default), `has_wildcards` for syntax preflight, `match_paths` / `_at` for borrowed list filtering, `match_path_indices` / `_at` for positions, or `compile_error` for a pattern the compiler must reject. |
| `paths` / `matches` | no | Input and Ferralk-selected path lists for `match_paths`, preserving input order. |
| `oracle_matches` | no | zlob's selected list for a deliberate list-result divergence. |
| `base_path` | no | Base directory stripped before matching each input path in a `match_paths_at` case. |
| `indices` / `oracle_indices` | no | Ferralk and divergent-oracle input positions for a `match_path_indices` operation. |
| `pattern` | yes | Glob or ignore expression using the byte codec below. |
| `path` | yes | Candidate path using the byte codec below; empty for syntax-only records. |
| `flags` | no | Ordered behaviour switches from the compatibility matrix. |
| `ignore_rules` | no | Lines written to a synthetic `.gitignore` for an ignore case. |
| `expected` | yes | Whether the expression accepts the candidate. |
| `oracle_expected` | no | The external-oracle result when it deliberately differs from `expected`. |
| `error_offset` | no | Byte offset the rejected construct must be reported at, for a `compile_error` case. |
| `error_message` | no | Stable error text a `compile_error` case must produce. |
| `platform` | no | `posix` or `windows`; restricts a separator-dependent verdict to one host. |
| `source` | yes | `zlob_1_6_3`, `fast_glob`, `git_check_ignore`, or `handwritten`. |
| `disputed` | no | `true` if evidence is recorded but ferralk policy is unsettled. |
| `note` | no | Short explanation or cross-oracle disagreement. |

The topic files are `basic.jsonl`, `braces.jsonl`, `bytes.jsonl`,
`case-folding.jsonl`, `classes.jsonl`, `edge-cases.jsonl`, `errors.jsonl`,
`extglob-suite.jsonl`, `fast-glob.jsonl`, `fnmatch.jsonl`,
`glibc-recursive.jsonl`, `ignore.jsonl`, `match-paths.jsonl`,
`path-matcher.jsonl`, `platform.jsonl`, `preflight.jsonl`, and
`wildcards.jsonl`. Files are added when a topic gains a case. Case IDs do
not change once published; a changed expected value is a new case plus an
explanation in `note`.

For `ignore.jsonl`, `pattern` is the primary rule for quick review and
`ignore_rules` is the complete ordered rule chain. The Git oracle creates an
isolated repository, writes those lines into `.gitignore`, creates `path`, and
uses `git check-ignore --no-index --quiet -- path`. This keeps nested/negated
behaviour tied to Git rather than to ferralk implementation details.

## Rejected patterns

A `compile_error` case records a pattern the compiler must refuse. `path` is
empty and `expected` is `false`, so a rejected pattern stays comparable with
every other kind: it accepts nothing. `error_offset` and `error_message` are
optional; when present the harness asserts the exact byte offset and the
stable message, which pins the diagnostic and not only the failure. Both
fields are rejected on any other kind. Constructs zlob treats as ordinary text
rather than as errors — an unmatched brace, an unterminated extglob — stay
`matcher` cases so the difference is written down instead of assumed.

```json
{"id":"error-unclosed-class","kind":"compile_error","pattern":"[abc","path":"","flags":[],"expected":false,"error_offset":0,"error_message":"unclosed character class","source":"handwritten"}
```

## Platform-specific verdicts

Most verdicts hold everywhere, but the separator set does not: per
[ADR-0005](adr/0005-byte-matching-wtf8-on-windows.md) a backslash separates
components on Windows and is an ordinary byte elsewhere. An optional
`platform` of `posix` or `windows` restricts such a case to the host it
describes; every other host skips it and reports the skip count. A verdict
that differs per platform is therefore two records with the same pattern and
path, one per platform, rather than one record with a hidden assumption.

## Byte codec

`pattern` and `path` are JSON strings that carry either normal UTF-8 text or
canonical `\\xNN` byte escapes. `NN` is exactly two uppercase hexadecimal
digits. The escape represents one byte; it is applied after JSON decoding.

Printable ASCII other than a backslash is written literally. A backslash,
control byte, or any byte that is not part of valid UTF-8 is emitted as
`\\xNN`. Valid non-ASCII Unicode is also escaped byte-by-byte, so corpus diffs
remain ASCII-only and unambiguous. For example:

```json
{"id":"bytes-001","pattern":"*.txt","path":"caf\\xC3\\xA9.txt","flags":[],"expected":true,"source":"handwritten"}
```

The `corpus` crate owns the strict Rust encoder and decoder. Malformed
backslashes such as `\\n`, incomplete escapes, and lower-level JSON decoding
errors are validation failures, never silently interpreted as paths.

## Evidence and disputes

`zlob_1_6_3` cases originate from the pinned oracle. `fast_glob` is used only
where its syntax overlaps. `git_check_ignore` is normative for ignore topics.
Handwritten records are allowed for a documented policy decision or a reduced
regression case. A disagreement is retained with `disputed: true` until an ADR
or compatibility-guide entry resolves it. `expected` is always the ferralk
contract and still runs in the harness; `oracle_expected` holds the diverging
reference result and runs only in the appropriate oracle adapter. Neither the
harness nor a future matcher may discard disputed cases.

## What the zlob oracle skips

The pinned oracle is a `&str`-based Rust API over zlob 1.6.3, so parts of the
contract have no representation there. The adapter in
[`../tools/oracle/tests/zlob_oracle.rs`](../tools/oracle/tests/zlob_oracle.rs)
skips exactly four categories and prints a count for each: a
`case_insensitive` case (zlob 1.6.3 has no case-folding flag), a case whose
pattern, path, or candidate list is not UTF-8, a `compile_error` case, and a
case written for another `platform`. A skipped case is not a weaker case: it
still replays in normal CI through the harness, which is the ferralk
contract. The adapter also asserts a minimum number of replayed cases, so a
change that silently skips the whole corpus fails instead of passing quietly.
