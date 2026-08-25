#![no_main]

use ferralk_glob::{Pattern, PatternOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Brace expansion is budgeted in the matcher now, so an over-budget
    // pattern is a `PatternError` this target is meant to reach rather than an
    // out-of-memory that hides every other finding.
    let (pattern, path) = split_input(data);
    let bits = data.last().copied().unwrap_or_default();
    if let Ok(pattern) = Pattern::compile(pattern, options_from(bits)) {
        // Differential oracle: the fast paths, the bit-parallel sweep engine,
        // and the memoized matcher must answer alike on every entry point.
        assert!(
            pattern.engines_agree(path),
            "match engines disagree on this input"
        );
    }
});

fn split_input(data: &[u8]) -> (&[u8], &[u8]) {
    match data.iter().position(|&byte| byte == b'\n') {
        Some(separator) => (&data[..separator], &data[separator + 1..]),
        None => (data, &[]),
    }
}

fn options_from(bits: u8) -> PatternOptions {
    PatternOptions::default()
        .braces(bits & 1 != 0)
        .recursive_double_star(bits & 2 != 0)
        .extglob(bits & 4 != 0)
        .match_hidden(bits & 8 != 0)
        .case_insensitive(bits & 16 != 0)
        .escape(bits & 32 == 0)
}
