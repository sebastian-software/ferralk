# Fuzzing ferralk-glob

The two targets exercise parsing and matching with arbitrary bytes and option
combinations. Their checked-in seeds derive from the executable matcher corpus.
On macOS, `macos_dirent_parser` additionally fuzzes the feature-gated
`getdirentries64` record validator without issuing syscalls or touching paths.

Install cargo-fuzz, then run cargo fuzz run pattern_parser or cargo fuzz run
pattern_matcher from the repository root.
Run `cargo fuzz run macos_dirent_parser` on macOS for the native record parser.

Crash artifacts belong under fuzz/artifacts. Minimize a saved input with
`cargo fuzz tmin` followed by the target and artifact, then convert the
minimized behaviour into a source-linked corpus regression case before fixing
it.
