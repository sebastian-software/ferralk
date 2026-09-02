#![forbid(unsafe_code)]
//! Replays the Git-verified ignore corpus through every walker frontend.
//!
//! An integration test rather than a unit test because it needs the
//! unpublished `corpus` package. Cargo strips that path-only dev-dependency
//! when packaging, so this file is excluded from the published crate and
//! `cargo test` from the tarball stays buildable.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{WalkEntry, Walker};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let unique = format!(
            "ferralk-corpus-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&root).expect("create fixture root");
        Self { root }
    }

    fn write(&self, path: impl AsRef<Path>) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().expect("fixture file has parent"))
            .expect("create fixture parent");
        fs::write(path, b"fixture").expect("write fixture file");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn relative_paths(entries: &[WalkEntry], root: &Path) -> Vec<PathBuf> {
    entries
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect()
}

/// Git-verified ignore cases the walker does not reproduce yet.
///
/// Empty since ADR-0014: the last entry was `ignore-034`, a POSIX class
/// name in a rule, which the borrowed matcher could not read and the
/// walker's own rule layer does. A case belongs here only while Git and
/// the walker are known to disagree, never as a way to quiet a failure.
const KNOWN_WALKER_GAPS: &[&str] = &[];

fn corpus_cases(kind: corpus::CaseKind) -> Vec<corpus::Case> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    let mut files = fs::read_dir(root)
        .expect("read corpus directory")
        .map(|entry| entry.expect("read corpus entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect::<Vec<_>>();
    files.sort();

    let mut cases = Vec::new();
    for file in files {
        for (line_number, line) in fs::read_to_string(&file)
            .expect("read corpus file")
            .lines()
            .enumerate()
        {
            if line.trim().is_empty() {
                continue;
            }
            let case = corpus::parse_case(line)
                .unwrap_or_else(|error| panic!("{}:{}: {error}", file.display(), line_number + 1));
            if case.kind == kind {
                cases.push(case);
            }
        }
    }
    cases
}

#[test]
fn git_ignore_corpus_replays_through_the_walker() {
    for case in corpus_cases(corpus::CaseKind::Ignore) {
        if !case.runs_on_host() || KNOWN_WALKER_GAPS.contains(&case.id.as_str()) {
            continue;
        }
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join(".gitignore"),
            format!("{}\n", case.ignore_rules.join("\n")).as_bytes(),
        )
        .expect("write fixture gitignore");
        // A case may place further ignore files below the root; Git reads
        // the one closest to the candidate last, and so does the walker.
        for nested in &case.nested_ignore_rules {
            let directory = fixture.root.join(&nested.directory);
            fs::create_dir_all(&directory).expect("create nested ignore directory");
            fs::write(
                directory.join(".gitignore"),
                format!("{}\n", nested.rules.join("\n")).as_bytes(),
            )
            .expect("write nested fixture gitignore");
        }
        // Repository-wide excludes live outside the ignore file chain.
        if !case.exclude_rules.is_empty() {
            let info = fixture.root.join(".git/info");
            fs::create_dir_all(&info).expect("create repository info directory");
            fs::write(
                info.join("exclude"),
                format!("{}\n", case.exclude_rules.join("\n")).as_bytes(),
            )
            .expect("write repository excludes");
        }
        if case.git_ignorecase {
            let git = fixture.root.join(".git");
            fs::create_dir_all(&git).expect("create repository metadata directory");
            fs::write(git.join("config"), b"[core]\nignorecase = true\n")
                .expect("write repository config");
        }
        if case.candidate_is_dir && case.candidate_is_symlink {
            panic!(
                "corpus case {} cannot be both a directory and a symlink",
                case.id
            );
        } else if case.candidate_is_dir {
            fs::create_dir_all(fixture.root.join(&case.path))
                .expect("create fixture candidate directory");
        } else if case.candidate_is_symlink {
            #[cfg(unix)]
            {
                let target = fixture.root.join(".ferralk-symlink-target");
                fs::create_dir_all(&target).expect("create symlink target directory");
                std::os::unix::fs::symlink(target, fixture.root.join(&case.path))
                    .expect("create fixture candidate symlink");
            }
            #[cfg(not(unix))]
            panic!("symlink corpus case {} ran on a non-POSIX host", case.id);
        } else {
            fixture.write(&case.path);
        }

        let candidate = PathBuf::from(&case.path);
        let serial = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(1)
            .collect()
            .expect("serial walk succeeds");
        let parallel = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(4)
            .collect()
            .expect("parallel walk succeeds");
        let streamed = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(4)
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("streaming walk succeeds");
        for (frontend, returned) in [
            (
                "serial collect",
                relative_paths(serial.entries(), &fixture.root).contains(&candidate),
            ),
            (
                "parallel collect",
                relative_paths(parallel.entries(), &fixture.root).contains(&candidate),
            ),
            (
                "stream",
                relative_paths(&streamed, &fixture.root).contains(&candidate),
            ),
        ] {
            assert_eq!(
                !returned, case.expected,
                "{frontend} verdict for corpus case {}",
                case.id
            );
        }
    }
}
