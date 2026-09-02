#![forbid(unsafe_code)]

//! Shared support and executable invariants for the glob fuzz targets.
//!
//! Keeping the subset classifier in a library makes its exclusions and the
//! checked-in seed corpus executable tests instead of untested target-local
//! control flow.

use ferralk_glob::{PatternOptions, expand_braces};

/// Largest pattern the matcher target sends through the full compiler.
///
/// Candidate bytes remain unbounded, preserving the long-path regression
/// seeds while oversized random patterns are rejected before expensive brace
/// and extglob work crowds structural mutations out of the corpus.
pub const MAX_PATTERN_MATCHER_PATTERN_BYTES: usize = 384;

/// Brace groups fast-glob answers correctly for.
///
/// Past ten it returns `false` for a pattern that matches, whatever the
/// alternative count and whichever combination the candidate takes. Measured
/// against fast-glob 1.1.0; see `docs/fast-glob-reference.md`.
const FAST_GLOB_MAX_BRACE_GROUPS: usize = 10;

/// The matcher options that make ferralk speak fast-glob's shared dialect.
pub fn matcher_options() -> PatternOptions {
    PatternOptions::default()
        .braces(true)
        .recursive_double_star(true)
        .match_hidden(true)
}

/// Splits one fuzz input into its pattern and candidate halves.
pub fn split_input(data: &[u8]) -> (&[u8], &[u8]) {
    match data.iter().position(|&byte| byte == b'\n') {
        Some(separator) => (&data[..separator], &data[separator + 1..]),
        None => (data, &[]),
    }
}

/// Derives the matcher target's option combination from one fuzz input.
///
/// A final newline is ignored so compact, reviewable text seeds can still
/// select options with their last candidate byte. Arbitrary binary inputs and
/// all existing non-newline-terminated seeds keep their previous reading.
pub fn pattern_matcher_options(data: &[u8]) -> PatternOptions {
    let without_final_newline = data.strip_suffix(b"\n").unwrap_or(data);
    let bits = without_final_newline.last().copied().unwrap_or_default();
    PatternOptions::default()
        .braces(bits & 1 != 0)
        .recursive_double_star(bits & 2 != 0)
        .extglob(bits & 4 != 0)
        .match_hidden(bits & 8 != 0)
        .case_insensitive(bits & 16 != 0)
        .escape(bits & 32 == 0)
}

/// Whether both engines document this pattern and candidate the same way.
///
/// Each rejection corresponds to one recorded divergence; see
/// `docs/fast-glob-reference.md`.
pub fn in_shared_subset(pattern: &[u8], path: &[u8]) -> bool {
    // ferralk path entry points normalize one conventional current-directory
    // prefix on both sides; fast-glob compares those bytes literally.
    if path.starts_with(b"./") {
        return false;
    }
    // fast-glob reads a leading `!` as negation; ferralk has no negation.
    if pattern.first() == Some(&b'!') {
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
                let Some(next) = class_end(pattern, index, brace_depth > 0) else {
                    return false;
                };
                index = next;
                continue;
            }
            b']' => return false,
            b'*' if pattern.get(index + 1) == Some(&b'*') => {
                if !double_star_is_shared(pattern, index) {
                    return false;
                }
            }
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
                    // always return to that reading after a group closes.
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
    brace_depth == 0 && !has_leading_dot_slash_alternative(pattern)
}

/// Whether one syntactic `**` is confined to a shape both engines accept.
///
/// Ferralk lets recursive `**` consume part of a path component, while
/// fast-glob requires a complete component. The languages still agree for a
/// bare `**`, and when a complete `**/` component is followed by an ordinary
/// component-leading `*`: that star can absorb every partial-component match
/// ferralk's recursive wildcard could otherwise contribute. Other positions
/// retain the documented structural exclusion.
fn double_star_is_shared(pattern: &[u8], index: usize) -> bool {
    let starts_component = index == 0 || pattern[index - 1] == b'/';
    if !starts_component {
        return false;
    }

    let after_pair = index + 2;
    if after_pair == pattern.len() {
        return index == 0;
    }
    if pattern.get(after_pair) != Some(&b'/') {
        return false;
    }

    let following_star = after_pair + 1;
    pattern.get(following_star) == Some(&b'*')
        && pattern.get(following_star + 1) != Some(&b'*')
        && pattern.get(following_star + 1) != Some(&b'(')
}

/// Whether brace expansion exposes a current-directory prefix.
fn has_leading_dot_slash_alternative(pattern: &[u8]) -> bool {
    if !pattern.contains(&b'{') {
        return pattern.starts_with(b"./");
    }
    expand_braces(pattern, matcher_options())
        .is_ok_and(|alternatives| alternatives.iter().any(|pattern| pattern.starts_with(b"./")))
}

/// Both engines unescape a metacharacter.
fn is_shared_escape(byte: u8) -> bool {
    matches!(byte, b'*' | b'?' | b'[' | b']' | b'{' | b'}' | b'\\')
}

/// Returns the index just past a shared-syntax character class.
fn class_end(pattern: &[u8], open: usize, inside_brace: bool) -> Option<usize> {
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
            Some(b',') if inside_brace => return None,
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
                } else if matches!(next, b'/' | b'{' | b'}') || (next == b',' && inside_brace) {
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

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use ferralk_glob::Pattern;

    use super::{
        MAX_PATTERN_MATCHER_PATTERN_BYTES, in_shared_subset, matcher_options,
        pattern_matcher_options, split_input,
    };

    #[test]
    fn every_checked_in_matcher_seed_reaches_the_target_logic() {
        let seed_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/pattern_matcher");
        let mut seeds = fs::read_dir(&seed_dir)
            .expect("read matcher seed directory")
            .map(|entry| entry.expect("read matcher seed entry").path())
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        seeds.sort();
        assert!(!seeds.is_empty(), "the matcher seed corpus is empty");

        for seed in seeds {
            let data = fs::read(&seed).expect("read matcher seed");
            let (pattern, _) = split_input(&data);
            assert!(
                pattern.len() <= MAX_PATTERN_MATCHER_PATTERN_BYTES,
                "{} has a {}-byte pattern above the {}-byte target ceiling",
                seed.display(),
                pattern.len(),
                MAX_PATTERN_MATCHER_PATTERN_BYTES
            );

            match ferralk_glob::Pattern::compile(pattern, pattern_matcher_options(&data)) {
                Ok(compiled) => assert!(
                    compiled.engines_agree(split_input(&data).1),
                    "{} reaches a matcher-engine disagreement",
                    seed.display()
                ),
                Err(error)
                    if seed.file_name().and_then(|name| name.to_str())
                        == Some("compiled-ir-budget") =>
                {
                    assert_eq!(error.message(), "pattern compiles to too much");
                }
                Err(error) => panic!("{} unexpectedly fails to compile: {error}", seed.display()),
            }
        }
    }

    #[test]
    fn globstar_exclusion_keeps_only_proven_shared_shapes() {
        for pattern in [
            b"**".as_slice(),
            b"**/*.rs",
            b"src/**/*.rs",
            b"a/**/*",
        ] {
            assert!(
                in_shared_subset(pattern, b"src/main.rs"),
                "expected {pattern:?} in the shared subset"
            );
        }
        for pattern in [
            b"**/a".as_slice(),
            b"a/**",
            b"a/**/b",
            b"a**/*.rs",
            b"a/**/?b",
            b"a/**/[ab]",
            b"a/**/**/*.rs",
            b"**/.*",
        ] {
            assert!(
                !in_shared_subset(pattern, b"a/ab"),
                "expected {pattern:?} outside the shared subset"
            );
        }

        assert!(in_shared_subset(br"\**", b"*suffix"));
        assert!(in_shared_subset(b"[**]", b"*"));
        assert!(!in_shared_subset(b"**/*.rs", b"./main.rs"));
    }

    #[test]
    fn every_checked_in_differential_seed_reaches_both_matchers() {
        let seed_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("corpus/ferralk_vs_fast_glob");
        let mut seeds = fs::read_dir(&seed_dir)
            .expect("read differential seed directory")
            .map(|entry| entry.expect("read differential seed entry").path())
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        seeds.sort();
        assert!(!seeds.is_empty(), "the differential seed corpus is empty");

        for seed in seeds {
            let data = fs::read(&seed).expect("read differential seed");
            let (pattern, candidate) = split_input(&data);
            assert!(
                in_shared_subset(pattern, candidate),
                "{} is outside the shared subset",
                seed.display()
            );
            fast_glob::validate(pattern)
                .unwrap_or_else(|error| panic!("{}: {error}", seed.display()));
            let compiled = Pattern::compile(pattern, matcher_options())
                .unwrap_or_else(|error| panic!("{}: {error}", seed.display()));
            assert_eq!(
                compiled.is_match_glob_path(candidate),
                fast_glob::glob_match(pattern, candidate),
                "{} does not reach an agreeing comparison",
                seed.display()
            );
        }
    }
}
