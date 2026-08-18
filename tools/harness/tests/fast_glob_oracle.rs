#![forbid(unsafe_code)]

use std::{fs, path::Path};

use corpus::{Case, Source, decode_bytes};
use ferralk_glob::{Pattern, PatternOptions};

#[test]
fn common_subset_replays_against_oxc_fast_glob() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/fast-glob.jsonl");
    let mut cases = 0_usize;
    for (line_number, line) in fs::read_to_string(&path)
        .expect("read fast-glob corpus")
        .lines()
        .enumerate()
    {
        if line.trim().is_empty() {
            continue;
        }
        let case: Case = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("{}:{}: {error}", path.display(), line_number + 1));
        assert_eq!(case.source, Source::FastGlob);
        let pattern = decode_bytes(&case.pattern).expect("decode pattern");
        let candidate = decode_bytes(&case.path).expect("decode candidate");
        fast_glob::validate(&pattern)
            .unwrap_or_else(|error| panic!("{}:{}: {error}", path.display(), line_number + 1));

        let expected_reference = case.oracle_expected.unwrap_or(case.expected);
        assert_eq!(
            fast_glob::glob_match(&pattern, &candidate),
            expected_reference,
            "{}:{}: fast-glob result",
            path.display(),
            line_number + 1
        );
        let ferralk = Pattern::compile(&pattern, options(&case.flags))
            .unwrap_or_else(|error| panic!("{}:{}: {error}", path.display(), line_number + 1))
            .is_match(&candidate);
        assert_eq!(
            ferralk,
            case.expected,
            "{}:{}: ferralk result",
            path.display(),
            line_number + 1
        );
        cases += 1;
    }
    assert!(cases > 0, "the fast-glob subset must contain cases");
}

fn options(flags: &[String]) -> PatternOptions {
    flags
        .iter()
        .fold(PatternOptions::default(), |options, flag| {
            match flag.as_str() {
                "braces" => options.braces(true),
                "recursive_double_star" => options.recursive_double_star(true),
                "extglob" => options.extglob(true),
                "match_hidden" => options.match_hidden(true),
                "case_insensitive" => options.case_insensitive(true),
                "no_escape" => options.escape(false),
                unknown => panic!("unknown matcher flag {unknown:?}"),
            }
        })
}
