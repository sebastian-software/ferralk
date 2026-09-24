#![no_main]

use ferralk_fuzz::{
    MAX_PATTERN_MATCHER_PATTERN_BYTES, pattern_matcher_options, split_input,
};
use ferralk_glob::{Pattern, PatternSet};
use libfuzzer_sys::{Corpus, fuzz_target};

fuzz_target!(|data: &[u8]| -> Corpus {
    // Brace expansion is budgeted in the matcher now, so an over-budget
    // pattern is a `PatternError` this target is meant to reach rather than an
    // out-of-memory that hides every other finding.
    let (pattern, path) = split_input(data);
    if pattern.len() > MAX_PATTERN_MATCHER_PATTERN_BYTES {
        return Corpus::Reject;
    }
    if let Ok(pattern) = Pattern::compile(pattern, pattern_matcher_options(data)) {
        // Differential oracle: the fast paths, the bit-parallel sweep engine,
        // and the memoized matcher must answer alike on every entry point.
        assert!(
            pattern.engines_agree(path),
            "match engines disagree on this input"
        );
        // A set's literal index may only skip a member that cannot match, so
        // a one-member set answers exactly like its member.
        let answers = |set: &PatternSet| {
            (
                set.is_match(path),
                set.is_match_path(path),
                set.is_match_glob_path(path),
            )
        };
        let set: PatternSet = std::iter::once(pattern.clone()).collect();
        assert_eq!(
            answers(&set),
            (
                pattern.is_match(path),
                pattern.is_match_path(path),
                pattern.is_match_glob_path(path),
            ),
            "a pattern set disagrees with its member on this input"
        );
    }
    Corpus::Keep
});
