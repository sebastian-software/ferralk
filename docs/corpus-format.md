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
| `pattern` | yes | Glob or ignore expression using the byte codec below. |
| `path` | yes | Candidate path using the byte codec below. |
| `flags` | no | Ordered behaviour switches from the compatibility matrix. |
| `expected` | yes | Whether the expression accepts the candidate. |
| `source` | yes | `zlob_1_6_3`, `fast_glob`, `git_check_ignore`, or `handwritten`. |
| `disputed` | no | `true` if evidence is recorded but ferralk policy is unsettled. |
| `note` | no | Short explanation or cross-oracle disagreement. |

The initial topic files are `wildcards.jsonl`, `classes.jsonl`,
`braces.jsonl`, `extglob.jsonl`, `options.jsonl`, `walk.jsonl`, and
`ignore.jsonl`. Files are added when a topic gains a case. Case IDs do not
change once published; a changed expected value is a new case plus an
explanation in `note`.

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
or compatibility-guide entry resolves it; neither the harness nor a future
matcher may discard it.
