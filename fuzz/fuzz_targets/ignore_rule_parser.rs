#![no_main]
//! The gitignore rule layer ADR-0014 owns: parsing a rule line and asking it
//! about a candidate.
//!
//! Both halves have to be total. A rule line arrives from a file in the tree
//! being walked, so it is attacker-shaped input in the same sense a pattern is,
//! and a candidate is whatever the filesystem hands over - including bytes that
//! are not UTF-8.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The first line is the rule, the rest is the candidate: one input drives
    // both halves, and the split point is something the fuzzer can move.
    let (line, candidate) = match data.iter().position(|byte| *byte == b'\n') {
        Some(separator) => (&data[..separator], &data[separator + 1..]),
        None => (data, &[][..]),
    };
    // Ignore files are read as text, so a rule line that is not UTF-8 never
    // reaches the parser. Such an input still drives the matcher, with a rule
    // shaped like the ones a repository carries, rather than being thrown away.
    let line = std::str::from_utf8(line).unwrap_or("**/node_modules/**");
    let _ = ferralk::fuzz_ignore_rule(line, candidate, false);
    let _ = ferralk::fuzz_ignore_rule(line, candidate, true);
});
