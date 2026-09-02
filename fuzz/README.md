# Fuzzing ferralk-glob

`pattern_parser` and `pattern_matcher` exercise parsing and matching with
arbitrary bytes and option combinations. Their checked-in seeds derive from the
executable matcher corpus. On macOS, `macos_dirent_parser` and
`macos_bulk_record_parser` separately fuzz the feature-gated `getdirentries64`
and `getattrlistbulk` record validators without issuing syscalls or touching
paths. Linux has the equivalent `linux_dirent_parser`. The native targets keep
small structure-aware checked-in records and CI preserves each target's corpus
between runs, so their bounded budgets start from valid parser shapes.
`cargo run --manifest-path fuzz/Cargo.toml --bin generate_macos_native_seeds`
and `cargo run --manifest-path fuzz/Cargo.toml --bin generate_linux_native_seeds`
regenerate the native records byte-for-byte; the matching native tests replay
and validate every checked-in seed before CI fuzzes it. The Linux generator
finds the repository from its Cargo manifest, so it can run from any directory;
its checked-in native corpus is intentionally little-endian, matching the
reviewed Linux native-backend targets.

`ferralk_vs_fast_glob` is differential: it feeds one pattern and one candidate
to both ferralk and Oxc fast-glob and asserts the same verdict. It keeps only
the syntax both engines document the same way, excluding each recorded
divergence by the shape of the pattern, so a failure is a new finding rather
than a known difference. The divergences and their exclusions are tabulated in
[`docs/fast-glob-reference.md`](../docs/fast-glob-reference.md). A disagreement
is reported as a ready-to-paste `corpus/fast-glob.jsonl` line.
Filtered or unparseable inputs return libFuzzer's rejected-corpus verdict, so
they cannot displace comparable inputs in the evolving corpus. The shared
globstar subset includes bare `**` and complete `**/` components followed by
an ordinary component-leading `*`, such as `src/**/*.rs`; other positions keep
the documented structural exclusion.

`cargo test --manifest-path fuzz/Cargo.toml --lib --locked` checks the subset
boundary and replays every checked-in differential seed through both matchers.
The automated and manual differential jobs also print a shared-subset hit-rate
checkpoint after fuzzing. The checkpoint is updated every 4,096 executions, so
the displayed numerator and denominator omit at most 4,095 final inputs; the
adjacent libFuzzer final statistics retain its exact execution count.

The glob targets used to skip patterns whose brace expansion was too large,
because `Pattern::compile` expanded braces eagerly with no budget and about a
hundred bytes of nine-way groups exhausted memory. `Pattern::compile` now
rejects those patterns itself, so the targets hand them straight to it and
exercise the real error path.

`pattern_matcher` sends at most 384 pattern bytes into the compiler and
returns libFuzzer's rejected-corpus verdict for larger patterns. Candidate
length stays unrestricted, so the long-path seeds still exercise matcher
stack and state bounds. A compact sub-kilobyte seed retains the compiled-IR
budget rejection path, and the fuzz-library tests ensure every checked-in
matcher seed remains below the pattern ceiling and replays it through the
compiler. Automated and manual workflow runs additionally set libFuzzer's
total input limit to 128 bytes for this target, keeping nightly mutations
focused on syntax shape; direct seed replay retains the original long-candidate
coverage outside that mutation limit.

`ferralk_vs_fast_glob` depends on that budget for its own speed: fast-glob
backtracks over brace alternatives rather than expanding them, and spends 42 s
on the ten-group pattern from issue #42. Compiling before `fast_glob::glob_match`
keeps such a pattern away from it. Raising `MAX_BRACE_ALTERNATIVES` means
re-measuring fast-glob at the new limit.

The glob targets run automatically: a short budget per pull request and a long
nightly run, both from `.github/workflows/glob-fuzz.yml`. A failed scheduled
run opens or refreshes the repository's nightly glob-fuzz tracking issue, in
addition to uploading its reproducer. `fuzz.yml` runs one chosen target with a
custom budget on demand.

Install cargo-fuzz, then run cargo fuzz run pattern_parser, cargo fuzz run
pattern_matcher, or cargo fuzz run ferralk_vs_fast_glob from the repository
root.
Run `cargo fuzz run macos_dirent_parser` or `cargo fuzz run
macos_bulk_record_parser` on macOS for the native record parsers. Run `cargo
fuzz run linux_dirent_parser` on Linux for the native record parser.

Crash artifacts belong under fuzz/artifacts. Minimize a saved input with
`cargo fuzz tmin` followed by the target and artifact, then convert the
minimized behaviour into a source-linked corpus regression case before fixing
it.
