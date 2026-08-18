#![no_main]

use ferralk_glob::{Pattern, PatternOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let options = options_from(data);
    let _ = Pattern::compile(data, options);
});

fn options_from(data: &[u8]) -> PatternOptions {
    let bits = data.first().copied().unwrap_or_default();
    PatternOptions::default()
        .braces(bits & 1 != 0)
        .recursive_double_star(bits & 2 != 0)
        .extglob(bits & 4 != 0)
        .match_hidden(bits & 8 != 0)
        .case_insensitive(bits & 16 != 0)
        .escape(bits & 32 == 0)
}
