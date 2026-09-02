#![forbid(unsafe_code)]

use std::{fs, path::Path};

use corpus::{CaseKind, Source, decode_bytes, parse_case};

/// Set to `1` to turn a too-old Git into a failure instead of a skip. CI's
/// Git oracle job sets it, so a replay that silently did not happen cannot
/// pass there.
const REQUIRE_GIT_ORACLE: &str = "FERRALK_REQUIRE_GIT_ORACLE";

/// Reports the installed Git and whether it can serve as the ignore oracle.
///
/// Printed to stdout rather than only to stderr, because test-output capture
/// swallows a passing test's diagnostics either way and `--show-output` or
/// `--nocapture` reveals stdout.
fn git_oracle_replays() -> bool {
    let git_version = harness::installed_git_version().expect("read installed Git version");
    let replays = git_version >= harness::MINIMUM_GIT_ORACLE_VERSION;
    if replays {
        println!("Git ignore oracle: replayed with Git {git_version}");
    } else {
        println!(
            "Git ignore oracle: skipped, Git >= {} is required but {git_version} is installed",
            harness::MINIMUM_GIT_ORACLE_VERSION
        );
    }
    if !replays && std::env::var_os(REQUIRE_GIT_ORACLE).is_some_and(|value| value == "1") {
        panic!(
            "{REQUIRE_GIT_ORACLE}=1 requires Git >= {}, but {git_version} is installed",
            harness::MINIMUM_GIT_ORACLE_VERSION
        );
    }
    replays
}

/// Always runs, so the preflight output names the Git it found and says
/// whether the corpus was replayed or skipped.
#[test]
fn git_ignore_oracle_version_is_reported() {
    git_oracle_replays();
}

#[test]
fn ignore_corpus_replays_against_git_check_ignore() {
    if !git_oracle_replays() {
        return;
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    let mut files = Vec::new();
    collect_jsonl(&root, &mut files).expect("find corpus files");
    files.sort();
    let mut cases = 0_usize;
    let mut nested_cases = 0_usize;
    let mut exclude_cases = 0_usize;
    for path in files {
        for (line_number, line) in fs::read_to_string(&path)
            .expect("read corpus file")
            .lines()
            .enumerate()
        {
            if line.trim().is_empty() {
                continue;
            }
            let case = parse_case(line)
                .unwrap_or_else(|error| panic!("{}:{}: {error}", path.display(), line_number + 1));
            if case.kind != CaseKind::Ignore {
                continue;
            }
            assert_eq!(case.source, Source::GitCheckIgnore);
            if !case.runs_on_host() {
                continue;
            }
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
            if !case.exclude_rules.is_empty() {
                exclude_cases += 1;
            }
            assert_eq!(
                harness::git_check_ignore_layered_with_options(
                    &case.ignore_rules,
                    &nested,
                    &case.exclude_rules,
                    candidate,
                    case.candidate_is_dir,
                    case.candidate_is_symlink,
                    case.git_ignorecase,
                )
                .expect("run git check-ignore"),
                case.expected,
                "{}:{}: {}",
                path.display(),
                line_number + 1,
                case.id
            );
            cases += 1;
        }
    }
    assert!(cases > 0, "the ignore corpus must contain Git-backed cases");
    assert!(
        nested_cases > 0,
        "the ignore corpus must exercise nested .gitignore precedence"
    );
    assert!(
        exclude_cases > 0,
        "the ignore corpus must exercise .git/info/exclude"
    );
}

fn collect_jsonl(root: &Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jsonl(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
    Ok(())
}
