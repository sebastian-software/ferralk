#![forbid(unsafe_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use corpus::{CaseKind, Source, decode_bytes, parse_case};
use ferralk::{WalkOptions, Walker};

/// Set to `1` to turn a too-old Git into a failure instead of a skip. CI's
/// Git oracle job sets it, so a replay that silently did not happen cannot
/// pass there.
const REQUIRE_GIT_ORACLE: &str = "FERRALK_REQUIRE_GIT_ORACLE";
const ROOT_SPELLING_FIXTURE: &str = "FERRALK_ROOT_SPELLING_FIXTURE";
const ROOT_SPELLING: &str = "FERRALK_ROOT_SPELLING";

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

/// Keeps repository discovery, candidate mapping, and caller-visible root
/// spellings tied to Git on every supported host. The parent creates one
/// repository; children isolate the process-wide working directory for each
/// spelling so libtest's parallelism cannot affect another test.
#[test]
fn root_spellings_match_git_ls_files() {
    if let Some(repository) = std::env::var_os(ROOT_SPELLING_FIXTURE) {
        let repository = PathBuf::from(repository);
        let spelling =
            PathBuf::from(std::env::var_os(ROOT_SPELLING).expect("child receives a root spelling"));
        let walked = Walker::new(&spelling)
            .respect_git_ignore(true)
            .options(WalkOptions::default().files_only(true).sort(true))
            .collect()
            .expect("walk root spelling");
        let mut actual = walked
            .entries()
            .iter()
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(&spelling)
                    .expect("entry stays under caller root")
                    .to_path_buf()
            })
            .collect::<Vec<_>>();
        actual.sort();

        let target = std::env::current_dir()
            .expect("read child working directory")
            .join(&spelling);
        let output = isolated_git(&repository)
            .args(["ls-files", "--others", "--exclude-standard", "-z"])
            .current_dir(target)
            .output()
            .expect("run git ls-files oracle");
        assert!(output.status.success(), "git ls-files: {}", output.status);
        let mut expected = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| {
                PathBuf::from(std::str::from_utf8(path).expect("fixture paths are valid UTF-8"))
            })
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(actual, expected, "root spelling {spelling:?}");
        return;
    }

    if !git_oracle_replays() {
        return;
    }

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after epoch")
        .as_nanos();
    let repository = std::env::temp_dir().join(format!(
        "ferralk-root-spelling-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&repository).expect("create root-spelling repository");
    let cleanup = Cleanup(repository.clone());
    assert!(
        isolated_git(&repository)
            .args(["init", "--quiet"])
            .current_dir(&repository)
            .status()
            .expect("initialize root-spelling repository")
            .success()
    );
    for path in [
        ".gitignore",
        "keep.txt",
        "ignored.log",
        "src/keep.rs",
        "src/ignored.log",
        "src/sub/.gitignore",
        "src/sub/keep.rs",
        "src/sub/local.tmp",
        "src/sub/deep/keep.md",
        "src/sub/deep/ignored.log",
    ] {
        let path = repository.join(path);
        fs::create_dir_all(path.parent().expect("fixture file has parent"))
            .expect("create fixture directory");
        let contents = match path.file_name().and_then(|name| name.to_str()) {
            Some(".gitignore") if path.parent() == Some(repository.as_path()) => {
                b"*.log\n".as_slice()
            }
            Some(".gitignore") => b"*.tmp\n".as_slice(),
            _ => b"fixture".as_slice(),
        };
        fs::write(path, contents).expect("write root-spelling fixture");
    }

    let cases = [
        (".", "."),
        (".", "./"),
        (".", "src"),
        (".", "src/"),
        (".", "./src/"),
        (".", "src//sub"),
        (".", "src/./sub/"),
        ("src", "."),
        ("src", "./"),
        ("src", "sub"),
        ("src", "sub/"),
        ("src", "./sub/"),
        ("src", "../src"),
        ("src", "../src/"),
        ("src/sub", "."),
        ("src/sub", "./"),
        ("src/sub", ".."),
        ("src/sub", "../"),
        ("src/sub", "../sub"),
        ("src/sub", "../../src"),
        ("src/sub", "../../src/sub"),
    ];
    for (working_directory, spelling) in cases {
        let status = Command::new(std::env::current_exe().expect("locate test binary"))
            .args(["root_spellings_match_git_ls_files", "--exact"])
            .current_dir(repository.join(working_directory))
            .env(ROOT_SPELLING_FIXTURE, &repository)
            .env(ROOT_SPELLING, spelling)
            .status()
            .expect("run root-spelling child");
        assert!(
            status.success(),
            "root-spelling child failed in {working_directory:?} for {spelling:?}: {status}"
        );
    }
    drop(cleanup);
}

fn isolated_git(repository: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_GLOBAL", repository.join("no-global-config"))
        .env("GIT_CONFIG_SYSTEM", repository.join("no-system-config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME");
    command
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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
