# Fuzzing ferralk-glob

`pattern_parser` and `pattern_matcher` exercise parsing and matching with
arbitrary bytes and option combinations. Their checked-in seeds derive from the
executable matcher corpus. On macOS, `macos_dirent_parser` additionally fuzzes
the feature-gated `getdirentries64` and `getattrlistbulk` record validators
without issuing syscalls or touching paths.

`ferralk_vs_fast_glob` is differential: it feeds one pattern and one candidate
to both ferralk and Oxc fast-glob and asserts the same verdict. It keeps only
the syntax both engines document the same way, excluding each recorded
divergence by the shape of the pattern, so a failure is a new finding rather
than a known difference. The divergences and their exclusions are tabulated in
[`docs/fast-glob-reference.md`](../docs/fast-glob-reference.md). A disagreement
is reported as a ready-to-paste `corpus/fast-glob.jsonl` line.

All three glob targets skip patterns whose brace expansion exceeds
`brace_budget::MAX_ALTERNATIVES`. `Pattern::compile` expands braces eagerly
with no budget, so about a hundred bytes of nine-way groups exhausts memory and
libFuzzer reports an out-of-memory that hides every other finding. The cap
still admits thousands of alternatives, far more than a real pattern uses. Lift
it once the matcher bounds its own expansion.

The glob targets run automatically: a short budget per pull request and a long
nightly run, both from `.github/workflows/glob-fuzz.yml`. `fuzz.yml` runs one
chosen target with a custom budget on demand.

Install cargo-fuzz, then run cargo fuzz run pattern_parser, cargo fuzz run
pattern_matcher, or cargo fuzz run ferralk_vs_fast_glob from the repository
root.
Run `cargo fuzz run macos_dirent_parser` on macOS for the native record parser.
Run `cargo fuzz run linux_dirent_parser` on Linux for the native record parser.

Crash artifacts belong under fuzz/artifacts. Minimize a saved input with
`cargo fuzz tmin` followed by the target and artifact, then convert the
minimized behaviour into a source-linked corpus regression case before fixing
it.
