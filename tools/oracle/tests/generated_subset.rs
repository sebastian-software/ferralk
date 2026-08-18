#![forbid(unsafe_code)]

use ferralk_glob::{Pattern, PatternOptions};
use zlob::{ZlobFlags, zlob_match_paths};

#[test]
#[ignore = "requires Zig 0.16 and libclang; run only from the manual oracle workflow"]
fn bounded_common_core_agrees_with_zlob_1_6_3() {
    let patterns = words(b"ab*?", 4);
    let paths = words(b"ab", 4);
    let mut comparisons = 0_usize;

    for pattern in &patterns {
        let ferralk = Pattern::compile(pattern, PatternOptions::default())
            .expect("the generated common core is syntactically valid");
        let pattern = std::str::from_utf8(pattern).expect("generated patterns are ASCII");
        for path in &paths {
            let path = std::str::from_utf8(path).expect("generated paths are ASCII");
            let expected = zlob_match_paths(pattern, &[path], ZlobFlags::empty())
                .expect("zlob accepts generated common-core patterns")
                .is_some();
            assert_eq!(
                ferralk.is_match(path),
                expected,
                "generated disagreement: pattern {pattern:?}, path {path:?}"
            );
            comparisons += 1;
        }
    }

    assert_eq!(comparisons, patterns.len() * paths.len());
}

fn words(alphabet: &[u8], max_length: usize) -> Vec<Vec<u8>> {
    let mut words = vec![Vec::new()];
    let mut current = vec![Vec::new()];
    for _ in 0..max_length {
        let mut next = Vec::new();
        for prefix in current {
            for &byte in alphabet {
                let mut word = prefix.clone();
                word.push(byte);
                next.push(word);
            }
        }
        words.extend(next.iter().cloned());
        current = next;
    }
    words
}
