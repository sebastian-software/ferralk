#![no_main]

use ferralk_glob::{Pattern, PatternOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Brace expansion is budgeted in the matcher now, so an over-budget
    // pattern is a `PatternError` this target is meant to reach rather than an
    // out-of-memory that hides every other finding.
    let bits = data.first().copied().unwrap_or_default();
    let _ = Pattern::compile(data, options_from(bits));
});

fn options_from(bits: u8) -> PatternOptions {
    PatternOptions::default()
        .braces(bits & 1 != 0)
        .recursive_double_star(bits & 2 != 0)
        .extglob(bits & 4 != 0)
        .match_hidden(bits & 8 != 0)
        .case_insensitive(bits & 16 != 0)
        .escape(bits & 32 == 0)
}
