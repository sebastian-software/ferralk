#![forbid(unsafe_code)]
//! Shared helpers used by the corpus harness and its Git-normative tests.

use std::{
    fs,
    io::{self, ErrorKind},
    path::Path,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static TEMPORARY_REPOSITORY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Evaluates a candidate path against rules with Git's own ignore matcher.
///
/// `rules` become the repository root's `.gitignore`; `files` are the further
/// ignore files a case needs, such as a nested `.gitignore` or
/// `.git/info/exclude`.
///
/// A new repository is created for each call so no caller state, global Git
/// configuration, or checked-out repository changes the verdict.
pub fn git_check_ignore(
    rules: &[String],
    files: &[corpus::IgnoreFile],
    candidate: &str,
) -> io::Result<bool> {
    let repository = TemporaryRepository::create()?;
    run_git(repository.path(), ["init", "--quiet"])?;
    fs::write(repository.path().join(".gitignore"), rules.join("\n"))?;
    for file in files {
        let path = repository.path().join(&file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, file.rules.join("\n"))?;
    }

    let candidate_path = repository.path().join(candidate);
    if let Some(parent) = candidate_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&candidate_path, [])?;

    let status = Command::new("git")
        .arg("check-ignore")
        .arg("--no-index")
        .arg("--quiet")
        .arg("--")
        .arg(candidate)
        .current_dir(repository.path())
        .status()?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        Some(code) => Err(io::Error::other(format!(
            "git check-ignore exited with status {code}"
        ))),
        None => Err(io::Error::other("git check-ignore terminated by signal")),
    }
}

fn run_git<'a>(directory: &Path, arguments: impl IntoIterator<Item = &'a str>) -> io::Result<()> {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("git init exited with {status}")))
    }
}

struct TemporaryRepository {
    path: std::path::PathBuf,
}

impl TemporaryRepository {
    fn create() -> io::Result<Self> {
        let unique = TEMPORARY_REPOSITORY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ferralk-git-ignore-{}-{unique}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => Ok(Self { path }),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Self::create(),
            Err(error) => Err(error),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
