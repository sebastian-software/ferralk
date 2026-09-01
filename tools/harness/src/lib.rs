#![forbid(unsafe_code)]
//! Shared helpers used by the corpus harness and its Git-normative tests.

use std::{
    fmt, fs,
    io::{self, ErrorKind},
    path::Path,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static TEMPORARY_REPOSITORY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A Git release version relevant to the normative ignore oracle.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GitVersion {
    major: u32,
    minor: u32,
    patch: u32,
    release: bool,
}

impl GitVersion {
    /// Creates a release version from its numeric components.
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            release: true,
        }
    }

    const fn prerelease(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            release: false,
        }
    }
}

impl fmt::Display for GitVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.release {
            formatter.write_str("-prerelease")?;
        }
        Ok(())
    }
}

/// The oldest Git release whose `check-ignore` semantics define the corpus.
pub const MINIMUM_GIT_ORACLE_VERSION: GitVersion = GitVersion::new(2, 52, 0);

/// Returns the installed Git version without reading user configuration.
pub fn installed_git_version() -> io::Result<GitVersion> {
    let mut command = Command::new("git");
    isolate(&mut command);
    let output = command.arg("--version").output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git --version exited with {}",
            output.status
        )));
    }
    let stdout = std::str::from_utf8(&output.stdout).map_err(|error| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("Git version output is not UTF-8: {error}"),
        )
    })?;
    parse_git_version(stdout)
}

fn parse_git_version(output: &str) -> io::Result<GitVersion> {
    let version = output
        .trim()
        .strip_prefix("git version ")
        .and_then(|remainder| remainder.split_whitespace().next())
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidData,
                format!("unexpected Git version output: {output:?}"),
            )
        })?;
    let mut components = version.splitn(3, '.');
    let major = parse_version_component(components.next(), output)?;
    let minor = parse_version_component(components.next(), output)?;
    let patch_component = components.next();
    let patch = parse_version_component(patch_component, output)?;
    let patch_suffix = patch_component
        .unwrap_or_default()
        .trim_start_matches(|character: char| character.is_ascii_digit());
    if patch_suffix.starts_with(".rc") || patch_suffix.starts_with("-rc") {
        Ok(GitVersion::prerelease(major, minor, patch))
    } else {
        Ok(GitVersion::new(major, minor, patch))
    }
}

fn parse_version_component(component: Option<&str>, output: &str) -> io::Result<u32> {
    let digits = component
        .unwrap_or_default()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().map_err(|_| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("unexpected Git version output: {output:?}"),
        )
    })
}

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
    git_check_ignore_layered(rules, nested, &[], candidate)
}

/// Evaluates a candidate against every ignore source of one repository.
///
/// `excludes` are the repository-wide rules in `.git/info/exclude`, which Git
/// reads before any `.gitignore`, so every ignore file overrides them.
pub fn git_check_ignore_layered(
    rules: &[String],
    nested: &[(&str, &[String])],
    excludes: &[String],
    candidate: &str,
) -> io::Result<bool> {
    git_check_ignore_layered_with_options(rules, nested, excludes, candidate, false, false, false)
}

/// Like [`git_check_ignore_layered`], with the candidate's entry kind and the
/// repository-local case-folding setting made explicit for corpus records.
pub fn git_check_ignore_layered_with_options(
    rules: &[String],
    nested: &[(&str, &[String])],
    excludes: &[String],
    candidate: &str,
    candidate_is_dir: bool,
    candidate_is_symlink: bool,
    git_ignorecase: bool,
) -> io::Result<bool> {
    if candidate_is_dir && candidate_is_symlink {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "an ignore candidate cannot be both a directory and a symlink",
        ));
    }
    let repository = TemporaryRepository::create()?;
    run_git(repository.path(), ["init", "--quiet"])?;
    // `git init` turns on `core.ignorecase` when the filesystem folds case, so
    // the same rule would decide differently on macOS and on Linux. ferralk
    // matches bytes (ADR-0005), so the oracle is pinned to the same reading and
    // the corpus means one thing on every host.
    run_git(
        repository.path(),
        [
            "config",
            "core.ignorecase",
            if git_ignorecase { "true" } else { "false" },
        ],
    )?;
    fs::write(
        repository.path().join(".gitignore"),
        ignore_file_contents(rules),
    )?;
    for (directory, rules) in nested {
        let directory = repository.path().join(directory);
        fs::create_dir_all(&directory)?;
        fs::write(directory.join(".gitignore"), ignore_file_contents(rules))?;
    }
    if !excludes.is_empty() {
        let info = repository.path().join(".git/info");
        fs::create_dir_all(&info)?;
        fs::write(info.join("exclude"), ignore_file_contents(excludes))?;
    }

    let candidate_path = repository.path().join(candidate);
    if let Some(parent) = candidate_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if candidate_is_dir {
        fs::create_dir_all(&candidate_path)?;
    } else if candidate_is_symlink {
        let target = repository.path().join(".ferralk-symlink-target");
        fs::create_dir_all(&target)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &candidate_path)?;
        #[cfg(not(unix))]
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "symlink ignore candidates require a POSIX host",
        ));
    } else {
        fs::write(&candidate_path, [])?;
    }

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

/// Serializes rule lines exactly as a text ignore file does. In particular, a
/// rule ending in `\r` becomes a genuine CRLF line, including when it is the
/// final and only rule in a corpus case.
fn ignore_file_contents(rules: &[String]) -> String {
    let mut contents = rules.join("\n");
    contents.push('\n');
    contents
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

#[cfg(test)]
mod tests {
    use super::{GitVersion, MINIMUM_GIT_ORACLE_VERSION, parse_git_version};

    #[test]
    fn git_version_parser_accepts_release_and_vendor_suffixes() {
        assert_eq!(
            parse_git_version("git version 2.52.0\n").unwrap(),
            GitVersion::new(2, 52, 0)
        );
        assert_eq!(
            parse_git_version("git version 2.52.0.windows.1\n").unwrap(),
            GitVersion::new(2, 52, 0)
        );
        assert_eq!(
            parse_git_version("git version 2.52.0 (Apple Git-154)\n").unwrap(),
            GitVersion::new(2, 52, 0)
        );
    }

    #[test]
    fn git_version_parser_rejects_unexpected_output() {
        let error = parse_git_version("Git version unknown\n").unwrap_err();
        assert!(error.to_string().contains("Git version"));
    }

    #[test]
    fn git_version_parser_keeps_prereleases_below_the_final_release() {
        let dot_rc = parse_git_version("git version 2.52.0.rc0\n").unwrap();
        let dash_rc = parse_git_version("git version 2.52.0-rc1\n").unwrap();

        assert!(dot_rc < MINIMUM_GIT_ORACLE_VERSION);
        assert!(dash_rc < MINIMUM_GIT_ORACLE_VERSION);
    }
}
