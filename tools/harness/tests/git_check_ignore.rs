#![forbid(unsafe_code)]

use std::{fs, path::Path};

use corpus::{Case, Source, decode_bytes};

#[test]
fn ignore_corpus_replays_against_git_check_ignore() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ignore.jsonl");
    let mut cases = 0_usize;
    let mut nested_cases = 0_usize;
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
        let nested: Vec<(&str, &[String])> = case
            .nested_ignore_rules
            .iter()
            .map(|file| (file.directory.as_str(), file.rules.as_slice()))
            .collect();
        if !nested.is_empty() {
            nested_cases += 1;
        }
        assert_eq!(
            harness::git_check_ignore_nested(&case.ignore_rules, &nested, candidate)
                .expect("run git check-ignore"),
            case.expected,
            "{}:{}: {}",
            path.display(),
            line_number + 1,
            case.id
        );
        cases += 1;
    }
    assert!(cases > 0, "the ignore corpus must contain Git-backed cases");
    assert!(
        nested_cases > 0,
        "the ignore corpus must exercise nested .gitignore precedence"
    );
}
