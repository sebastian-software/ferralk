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
/// A new repository is created for each call so no caller state, global Git
/// configuration, or checked-out repository changes the verdict.
pub fn git_check_ignore(rules: &[String], candidate: &str) -> io::Result<bool> {
    git_check_ignore_nested(rules, &[], candidate)
}

/// Evaluates a candidate against a root `.gitignore` plus deeper ones.
///
/// Git reads the ignore file closest to the candidate last, so a nested file
/// overrides the root. `nested` names each further file by the directory that
/// holds it, relative to the repository root.
pub fn git_check_ignore_nested(
    rules: &[String],
    nested: &[(&str, &[String])],
    candidate: &str,
) -> io::Result<bool> {
    let repository = TemporaryRepository::create()?;
    run_git(repository.path(), ["init", "--quiet"])?;
    fs::write(repository.path().join(".gitignore"), rules.join("\n"))?;
    for (directory, rules) in nested {
        let directory = repository.path().join(directory);
        fs::create_dir_all(&directory)?;
        fs::write(directory.join(".gitignore"), rules.join("\n"))?;
    }

    let candidate_path = repository.path().join(candidate);
    if let Some(parent) = candidate_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&candidate_path, [])?;

    let mut command = Command::new("git");
    isolate(&mut command);
    let status = command
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

/// Cuts a Git invocation off from every configuration outside the fixture.
///
/// Without this the developer's own `core.excludesFile` decides corpus
/// verdicts: a global `*.log` rule silently ignores candidates the recorded
/// rules never mention, and the same case then behaves differently in CI.
fn isolate(command: &mut Command) {
    command
        // Git 2.32 and later read these instead of the real config files.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // An excludes file named through the environment outranks the config.
        .env_remove("GIT_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME");
}

fn run_git<'a>(directory: &Path, arguments: impl IntoIterator<Item = &'a str>) -> io::Result<()> {
    let mut command = Command::new("git");
    isolate(&mut command);
    let status = command.args(arguments).current_dir(directory).status()?;
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
