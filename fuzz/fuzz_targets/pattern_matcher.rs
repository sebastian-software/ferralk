#![no_main]

use ferralk_glob::{Pattern, PatternOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (pattern, path) = split_input(data);
    if let Ok(pattern) = Pattern::compile(pattern, options_from(data)) {
        let _ = pattern.is_match(path);
    }
});

fn split_input(data: &[u8]) -> (&[u8], &[u8]) {
    match data.iter().position(|&byte| byte == b'\n') {
        Some(separator) => (&data[..separator], &data[separator + 1..]),
        None => (data, &[]),
    }
}

fn options_from(data: &[u8]) -> PatternOptions {
    let bits = data.last().copied().unwrap_or_default();
    PatternOptions::default()
        .braces(bits & 1 != 0)
        .recursive_double_star(bits & 2 != 0)
        .extglob(bits & 4 != 0)
        .match_hidden(bits & 8 != 0)
        .case_insensitive(bits & 16 != 0)
        .escape(bits & 32 == 0)
}
