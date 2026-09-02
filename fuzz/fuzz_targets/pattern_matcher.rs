#![no_main]

use ferralk_fuzz::{
    MAX_PATTERN_MATCHER_PATTERN_BYTES, pattern_matcher_options, split_input,
};
use ferralk_glob::Pattern;
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
    }
    Corpus::Keep
});
