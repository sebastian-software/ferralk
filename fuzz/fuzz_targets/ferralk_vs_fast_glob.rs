#![no_main]
//! Differential target: ferralk and Oxc fast-glob over their shared syntax.
//!
//! ADR-0007 designates fast-glob as the second reference for the syntax both
//! engines document the same way. This target generates a pattern and a
//! candidate, keeps only inputs inside that shared subset, and asserts equal
//! verdicts. Every documented divergence is excluded structurally, by the
//! shape of the pattern, so a failure is always a new finding rather than a
//! rediscovery of a known difference. The divergences and the exclusion that
//! covers each are listed in `docs/fast-glob-reference.md`.
//!
//! A disagreement is reported as a ready-to-paste corpus line, so a finding
//! goes straight into `corpus/fast-glob.jsonl` after review.

use corpus::{Case, Source, encode_bytes};
use ferralk_glob::{Pattern, PatternOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (pattern, path) = split_input(data);
    if !in_shared_subset(pattern) {
        return;
    }
    // fast-glob rejects patterns ferralk accepts and the reverse; comparing a
    // verdict either engine declines to produce would compare error models,
    // not matching. `validate` is a parse and stays cheap on every input.
    if fast_glob::validate(pattern).is_err() {
        return;
    }
    // Compiling before `glob_match` is what keeps this target fast: fast-glob
    // backtracks over brace alternatives instead of expanding them, and spends
    // 42 s on the ten-group pattern from issue #42. ferralk's expansion budget
    // rejects that pattern here, so only patterns inside the budget — measured
    // at microseconds in fast-glob — ever reach the comparison. Raising
    // `MAX_BRACE_ALTERNATIVES` would need this checked again.
    let Ok(compiled) = Pattern::compile(pattern, options()) else {
        return;
    };

    // fast-glob keeps every ordinary wildcard inside one path component, which
    // is what `is_match_glob_path` does; `is_match` is the fnmatch-style form
    // zlob defines and is deliberately not comparable here.
    let ours = compiled.is_match_glob_path(path);
    let reference = fast_glob::glob_match(pattern, path);
    assert!(
        ours == reference,
        "ferralk and fast-glob disagree; corpus candidate:\n{}",
        corpus_candidate(pattern, path, ours, reference)
    );
});

/// Brace groups fast-glob answers correctly for.
///
/// Past ten it returns `false` for a pattern that matches, whatever the
/// alternative count and whichever combination the candidate takes: eleven
/// two-way groups miss even their first one. A single group of two thousand
/// alternatives is fine, so the cap counts groups rather than combinations.
/// Measured against fast-glob 1.1.0; see `docs/fast-glob-reference.md`.
const FAST_GLOB_MAX_BRACE_GROUPS: usize = 10;

/// The options that make ferralk speak fast-glob's dialect.
///
/// Extglobs are fast-glob-only syntax, case folding is ferralk-only, and
/// escapes stay on because both engines honour a backslash before a
/// metacharacter.
fn options() -> PatternOptions {
    PatternOptions::default()
        .braces(true)
        .recursive_double_star(true)
        .match_hidden(true)
}

fn split_input(data: &[u8]) -> (&[u8], &[u8]) {
    match data.iter().position(|&byte| byte == b'\n') {
        Some(separator) => (&data[..separator], &data[separator + 1..]),
        None => (data, &[]),
    }
}

/// Whether both engines document this pattern's syntax the same way.
///
/// Each rejection corresponds to one recorded divergence; see
/// `docs/fast-glob-reference.md`.
fn in_shared_subset(pattern: &[u8]) -> bool {
    // fast-glob reads a leading `!` as negation; ferralk has no negation.
    if pattern.first() == Some(&b'!') {
        return false;
    }
    // `**` is a whole path component in fast-glob and an ordinary recursive
    // wildcard in ferralk. The two readings agree only when the pattern is
    // nothing else, so every other consecutive star pair stays out.
    if pattern != b"**" && contains_double_star(pattern) {
        return false;
    }
    let mut index = 0;
    let mut brace_depth = 0_usize;
    let mut brace_groups = 0_usize;
    while index < pattern.len() {
        match pattern[index] {
            b'\\' => {
                let Some(&escaped) = pattern.get(index + 1) else {
                    return false;
                };
                if !is_shared_escape(escaped) {
                    return false;
                }
                index += 2;
                continue;
            }
            b'[' => {
                let Some(next) = class_end(pattern, index) else {
                    return false;
                };
                index = next;
                continue;
            }
            b']' => return false,
            b'{' | b'}' | b',' => {
                match pattern[index] {
                    b'{' => {
                        // fast-glob caps brace nesting; one level is common
                        // ground.
                        brace_depth += 1;
                        if brace_depth > 1 {
                            return false;
                        }
                        // It also caps how many groups a pattern may have at
                        // all, and answers `false` rather than erring past it.
                        brace_groups += 1;
                        if brace_groups > FAST_GLOB_MAX_BRACE_GROUPS {
                            return false;
                        }
                    }
                    b'}' => {
                        if brace_depth == 0 {
                            return false;
                        }
                        brace_depth -= 1;
                    }
                    // A comma separates alternatives inside a brace group and
                    // is an ordinary byte outside one, but fast-glob does not
                    // always return to that reading after a group closes:
                    // `{}{},` matches the empty candidate there and the
                    // literal `,` in ferralk.
                    _ => {
                        if brace_depth == 0 {
                            return false;
                        }
                    }
                }
                // Brace expansion concatenates its alternative with the
                // surrounding text, so a star beside brace punctuation can
                // still produce a `**` that only ferralk reads recursively.
                let star_before = index > 0 && pattern[index - 1] == b'*';
                let star_after = pattern.get(index + 1) == Some(&b'*');
                if star_before || star_after {
                    return false;
                }
            }
            _ => {}
        }
        index += 1;
    }
    brace_depth == 0
}

fn contains_double_star(pattern: &[u8]) -> bool {
    pattern.windows(2).any(|pair| pair == b"**")
}

/// Both engines unescape a metacharacter. Only ferralk also unescapes an
/// ordinary byte, where `\b` becomes a literal `b`.
fn is_shared_escape(byte: u8) -> bool {
    matches!(byte, b'*' | b'?' | b'[' | b']' | b'{' | b'}' | b'\\')
}

/// Returns the index just past a shared-syntax character class.
fn class_end(pattern: &[u8], open: usize) -> Option<usize> {
    let mut scan = open + 1;
    // A negated class implicitly contains the separator, which only fast-glob
    // lets a class accept.
    if matches!(pattern.get(scan), Some(b'!' | b'^')) {
        return None;
    }
    // POSIX class names are ferralk-only.
    if pattern.get(scan) == Some(&b':') {
        return None;
    }
    // A leading `]` is an ordinary member in both engines.
    let mut previous = None;
    if pattern.get(scan) == Some(&b']') {
        previous = Some(b']');
        scan += 1;
    }
    loop {
        match pattern.get(scan) {
            None => return None,
            Some(b']') => return Some(scan + 1),
            // Only fast-glob lets a class accept a separator, and brace
            // expansion inside a class rewrites the class itself.
            Some(b'/' | b'{' | b'}') => return None,
            Some(b'-') => {
                // A `-` with nothing before it, or directly before the closing
                // bracket, is an ordinary member rather than a range.
                let (Some(start), Some(&next)) = (previous, pattern.get(scan + 1)) else {
                    previous = Some(b'-');
                    scan += 1;
                    continue;
                };
                if next == b']' {
                    previous = Some(b'-');
                    scan += 1;
                    continue;
                }
                let (end, after) = if next == b'\\' {
                    let escaped = *pattern.get(scan + 2)?;
                    if !is_shared_escape(escaped) {
                        return None;
                    }
                    (escaped, scan + 3)
                } else if matches!(next, b'/' | b'{' | b'}') {
                    // The separator and brace bytes are excluded as members
                    // above; consuming one as a range endpoint would smuggle
                    // it past that rule (`[0-{src,9]` slipped through and let
                    // brace expansion rewrite the class, found on PR #45).
                    return None;
                } else {
                    (next, scan + 2)
                };
                // A range that spans the separator accepts it, which is the
                // same divergence as writing `/` in the class directly.
                if start <= b'/' && b'/' <= end {
                    return None;
                }
                previous = None;
                scan = after;
            }
            Some(b'\\') => {
                let escaped = *pattern.get(scan + 1)?;
                if !is_shared_escape(escaped) {
                    return None;
                }
                previous = Some(escaped);
                scan += 2;
            }
            Some(&byte) => {
                previous = Some(byte);
                scan += 1;
            }
        }
    }
}

/// Renders a disagreement as one `corpus/fast-glob.jsonl` line.
fn corpus_candidate(pattern: &[u8], path: &[u8], ours: bool, reference: bool) -> String {
    let case = Case {
        id: format!("fastglob-diff-{:016x}", fingerprint(pattern, path)),
        kind: corpus::CaseKind::Matcher,
        paths: Vec::new(),
        matches: Vec::new(),
        oracle_matches: None,
        base_path: String::new(),
        indices: Vec::new(),
        oracle_indices: None,
        pattern: encode_bytes(pattern),
        path: encode_bytes(path),
        flags: vec![
            "braces".to_owned(),
            "recursive_double_star".to_owned(),
            "match_hidden".to_owned(),
        ],
        ignore_rules: Vec::new(),
        nested_ignore_rules: Vec::new(),
        expected: ours,
        oracle_expected: Some(reference),
        error_offset: None,
        error_message: None,
        platform: None,
        source: Source::FastGlob,
        disputed: true,
        note: Some(
            "Found by the ferralk_vs_fast_glob differential target; review before adopting."
                .to_owned(),
        ),
    };
    serde_json::to_string(&case).expect("a corpus case serializes")
}

/// A stable, dependency-free name for one input pair.
fn fingerprint(pattern: &[u8], path: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in pattern.iter().chain(b"\n").chain(path) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}
