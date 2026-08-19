#![forbid(unsafe_code)]

use std::{fs, path::Path};

use corpus::{Case, Source, decode_bytes};

#[test]
fn ignore_corpus_replays_against_git_check_ignore() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ignore.jsonl");
    let mut cases = 0_usize;
    for (line_number, line) in fs::read_to_string(&path)
        .expect("read ignore corpus")
        .lines()
        .enumerate()
    {
        if line.trim().is_empty() {
            continue;
        }
        let case: Case = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("{}:{}: {error}", path.display(), line_number + 1));
        assert_eq!(case.source, Source::GitCheckIgnore);
        let candidate_bytes = decode_bytes(&case.path).expect("decode candidate path");
        let candidate = std::str::from_utf8(&candidate_bytes)
            .expect("Git oracle corpus paths must be valid UTF-8");
        // A disputed case records Git's verdict in `oracle_expected` and
        // ferralk's own policy in `expected`; the oracle is held to its own.
        let expected_reference = case.oracle_expected.unwrap_or(case.expected);
        assert_eq!(
            harness::git_check_ignore(&case.ignore_rules, &case.ignore_files, candidate)
                .expect("run git check-ignore"),
            expected_reference,
            "{}:{}: {}",
            path.display(),
            line_number + 1,
            case.id
        );
        cases += 1;
    }
    assert!(cases > 0, "the ignore corpus must contain Git-backed cases");
}
