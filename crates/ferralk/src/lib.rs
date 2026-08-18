#![forbid(unsafe_code)]
#![doc = "Portable, serial filesystem walking."]

//! A safe std::fs walker used as the portable M2 baseline.
//!
//! Paths stay as PathBuf throughout the public API. Patterns are matched
//! against root-relative encoded path bytes; no filesystem result is converted
//! through UTF-8.

use std::{
    collections::HashSet,
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
};

use ferralk_glob::{Pattern, PatternError, PatternOptions};

pub use ferralk_glob;

/// Controls what a walk does after a recoverable filesystem error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorPolicy {
    /// Stop immediately and return the first error.
    Abort,
    /// Continue walking and do not retain recoverable errors.
    Skip,
    /// Continue walking and return accumulated recoverable errors.
    #[default]
    Collect,
}

/// Behaviour switches for a Walker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WalkOptions {
    follow_symlinks: bool,
    sort: bool,
}

impl WalkOptions {
    /// Follows directory symlinks while retaining a canonical-path cycle guard.
    #[must_use]
    pub const fn follow_symlinks(mut self, enabled: bool) -> Self {
        self.follow_symlinks = enabled;
        self
    }

    /// Sorts final entries by their native path representation.
    #[must_use]
    pub const fn sort(mut self, enabled: bool) -> Self {
        self.sort = enabled;
        self
    }
}

/// One matching filesystem entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkEntry {
    path: PathBuf,
    is_dir: bool,
}

impl WalkEntry {
    /// Absolute or caller-relative path preserved from the walker.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this entry is a directory according to the selected backend.
    #[must_use]
    pub const fn is_dir(&self) -> bool {
        self.is_dir
    }
}

/// A recoverable I/O failure observed while walking.
#[derive(Debug)]
pub struct WalkError {
    operation: &'static str,
    path: PathBuf,
    source: std::io::Error,
}

impl WalkError {
    fn new(operation: &'static str, path: PathBuf, source: std::io::Error) -> Self {
        Self {
            operation,
            path,
            source,
        }
    }

    /// Operation that produced the error.
    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    /// Path passed to the failed operation.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Display for WalkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {}: {}",
            self.operation,
            self.path.display(),
            self.source
        )
    }
}

impl Error for WalkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Completed entries and recoverable errors.
#[derive(Debug)]
pub struct WalkResult {
    entries: Vec<WalkEntry>,
    errors: Vec<WalkError>,
}

impl WalkResult {
    /// Entries accepted by the include/exclude filters.
    #[must_use]
    pub fn entries(&self) -> &[WalkEntry] {
        &self.entries
    }

    /// Recoverable traversal failures retained by the configured error policy.
    #[must_use]
    pub fn errors(&self) -> &[WalkError] {
        &self.errors
    }

    /// Consumes the result and returns its two result channels.
    #[must_use]
    pub fn into_parts(self) -> (Vec<WalkEntry>, Vec<WalkError>) {
        (self.entries, self.errors)
    }
}

/// Builder for a portable serial traversal.
#[derive(Debug, Clone)]
pub struct Walker {
    root: PathBuf,
    includes: Vec<Pattern>,
    excludes: Vec<TraversalPattern>,
    options: WalkOptions,
    error_policy: ErrorPolicy,
}

impl Walker {
    /// Starts a walk rooted at root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            includes: Vec::new(),
            excludes: Vec::new(),
            options: WalkOptions::default(),
            error_policy: ErrorPolicy::default(),
        }
    }

    /// Adds an OR-ed include pattern. No includes means every non-excluded
    /// entry is returned.
    pub fn include(mut self, pattern: impl AsRef<[u8]>) -> Result<Self, PatternError> {
        self.includes
            .push(Pattern::compile(pattern, traversal_pattern_options())?);
        Ok(self)
    }

    /// Adds an OR-ed exclude pattern. Excluded directories are not descended.
    pub fn exclude(mut self, pattern: impl AsRef<[u8]>) -> Result<Self, PatternError> {
        self.excludes
            .push(TraversalPattern::compile(pattern.as_ref())?);
        Ok(self)
    }

    /// Replaces all traversal options.
    #[must_use]
    pub const fn options(mut self, options: WalkOptions) -> Self {
        self.options = options;
        self
    }

    /// Chooses the recoverable error policy.
    #[must_use]
    pub const fn error_policy(mut self, error_policy: ErrorPolicy) -> Self {
        self.error_policy = error_policy;
        self
    }

    /// Runs the serial portable backend to completion.
    pub fn collect(self) -> Result<WalkResult, WalkError> {
        let backend = StdBackend;
        let mut state = WalkState::new(&self);
        state.walk_directory(&backend, self.root.clone())?;
        if self.options.sort {
            state
                .entries
                .sort_by(|left, right| left.path.cmp(&right.path));
        }
        Ok(WalkResult {
            entries: state.entries,
            errors: state.errors,
        })
    }
}

fn traversal_pattern_options() -> PatternOptions {
    PatternOptions::default()
        .braces(true)
        .recursive_double_star(true)
        .extglob(true)
}

#[derive(Debug, Clone)]
struct TraversalPattern {
    matcher: Pattern,
    subtree_root: Option<Pattern>,
}

impl TraversalPattern {
    fn compile(pattern: &[u8]) -> Result<Self, PatternError> {
        let options = traversal_pattern_options();
        let subtree_root = pattern
            .strip_suffix(b"/**")
            .map(|root| Pattern::compile(root, options))
            .transpose()?;
        Ok(Self {
            matcher: Pattern::compile(pattern, options)?,
            subtree_root,
        })
    }

    fn matches(&self, path: &[u8]) -> bool {
        self.matcher.is_match(path)
    }

    fn covers_subtree(&self, path: &[u8]) -> bool {
        self.subtree_root
            .as_ref()
            .is_some_and(|root| root.is_match(path))
    }
}

trait DirectoryBackend {
    fn read_directory(&self, path: &Path) -> std::io::Result<Vec<BackendEntry>>;
}

#[derive(Debug, Clone)]
struct BackendEntry {
    path: PathBuf,
    is_dir: bool,
    is_symlink: bool,
}

struct StdBackend;

impl DirectoryBackend for StdBackend {
    fn read_directory(&self, path: &Path) -> std::io::Result<Vec<BackendEntry>> {
        fs::read_dir(path)?
            .map(|entry| {
                let entry = entry?;
                let file_type = entry.file_type()?;
                Ok(BackendEntry {
                    path: entry.path(),
                    is_dir: file_type.is_dir(),
                    is_symlink: file_type.is_symlink(),
                })
            })
            .collect()
    }
}

struct WalkState<'walker> {
    walker: &'walker Walker,
    entries: Vec<WalkEntry>,
    errors: Vec<WalkError>,
    visited_directories: HashSet<PathBuf>,
}

impl<'walker> WalkState<'walker> {
    fn new(walker: &'walker Walker) -> Self {
        Self {
            walker,
            entries: Vec::new(),
            errors: Vec::new(),
            visited_directories: HashSet::new(),
        }
    }

    fn walk_directory(
        &mut self,
        backend: &impl DirectoryBackend,
        directory: PathBuf,
    ) -> Result<(), WalkError> {
        if self.walker.options.follow_symlinks && !self.mark_directory(&directory)? {
            return Ok(());
        }
        let entries = match backend.read_directory(&directory) {
            Ok(entries) => entries,
            Err(source) => return self.handle_error("read_dir", directory, source),
        };
        for entry in entries {
            self.visit_entry(backend, entry)?;
        }
        Ok(())
    }

    fn mark_directory(&mut self, directory: &Path) -> Result<bool, WalkError> {
        match fs::canonicalize(directory) {
            Ok(canonical) => Ok(self.visited_directories.insert(canonical)),
            Err(source) => {
                self.handle_error("canonicalize", directory.to_path_buf(), source)?;
                Ok(false)
            }
        }
    }

    fn visit_entry(
        &mut self,
        backend: &impl DirectoryBackend,
        mut entry: BackendEntry,
    ) -> Result<(), WalkError> {
        let relative = entry
            .path
            .strip_prefix(&self.walker.root)
            .unwrap_or(entry.path.as_path());
        let bytes = relative.as_os_str().as_encoded_bytes();
        if self
            .walker
            .excludes
            .iter()
            .any(|pattern| pattern.matches(bytes))
        {
            return Ok(());
        }
        if entry.is_symlink && self.walker.options.follow_symlinks {
            match fs::metadata(&entry.path) {
                Ok(metadata) => entry.is_dir = metadata.is_dir(),
                Err(source) => {
                    self.handle_error("metadata", entry.path.clone(), source)?;
                    return Ok(());
                }
            }
        }
        if entry.is_dir {
            if self
                .walker
                .excludes
                .iter()
                .any(|pattern| pattern.covers_subtree(bytes))
            {
                return Ok(());
            }
            self.walk_directory(backend, entry.path.clone())?;
        }

        if self.walker.includes.is_empty()
            || self
                .walker
                .includes
                .iter()
                .any(|pattern| pattern.is_match(bytes))
        {
            self.entries.push(WalkEntry {
                path: entry.path,
                is_dir: entry.is_dir,
            });
        }
        Ok(())
    }

    fn handle_error(
        &mut self,
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    ) -> Result<(), WalkError> {
        let error = WalkError::new(operation, path, source);
        match self.walker.error_policy {
            ErrorPolicy::Abort => Err(error),
            ErrorPolicy::Skip => Ok(()),
            ErrorPolicy::Collect => {
                self.errors.push(error);
                Ok(())
            }
        }
    }
}

/// Crate version exposed for build and integration diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{ErrorPolicy, TraversalPattern, WalkEntry, WalkOptions, Walker};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let unique = format!(
                "ferralk-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after unix epoch")
                    .as_nanos()
                    + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
            );
            let root = std::env::temp_dir().join(unique);
            fs::create_dir_all(&root).expect("create fixture root");
            Self { root }
        }

        fn write(&self, relative: impl AsRef<Path>) {
            let path = self.root.join(relative);
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

    #[test]
    fn include_exclude_and_sort_are_applied_to_relative_paths() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs");
        fixture.write("src/lib.txt");
        fixture.write("target/generated.rs");

        let result = Walker::new(&fixture.root)
            .include("**/*.rs")
            .expect("valid include")
            .exclude("**/target/**")
            .expect("valid exclude")
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");

        assert_eq!(
            relative_paths(result.entries(), &fixture.root),
            vec![PathBuf::from("src/main.rs")]
        );
        assert!(result.errors().is_empty());
    }

    #[test]
    fn prune_planner_only_accepts_explicit_whole_subtree_excludes() {
        let subtree = TraversalPattern::compile(b"src/**").expect("valid subtree pattern");
        assert!(subtree.covers_subtree(b"src"));
        assert!(!subtree.covers_subtree(b"src/nested"));

        let suffix = TraversalPattern::compile(b"*.tmp").expect("valid suffix pattern");
        assert!(!suffix.covers_subtree(b"cache"));

        let nested =
            TraversalPattern::compile(b"**/target/**").expect("valid recursive subtree pattern");
        assert!(nested.covers_subtree(b"target"));
        assert!(nested.covers_subtree(b"crates/ferralk/target"));
    }

    #[test]
    fn collect_and_skip_distinguish_recoverable_root_errors() {
        let missing = std::env::temp_dir().join(format!(
            "ferralk-missing-{}",
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let collected = Walker::new(&missing)
            .error_policy(ErrorPolicy::Collect)
            .collect()
            .expect("collect policy retains the error");
        assert_eq!(collected.errors().len(), 1);
        assert!(
            Walker::new(&missing)
                .error_policy(ErrorPolicy::Skip)
                .collect()
                .expect("skip policy ignores the error")
                .errors()
                .is_empty()
        );
        assert!(
            Walker::new(&missing)
                .error_policy(ErrorPolicy::Abort)
                .collect()
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_policy_prevents_or_deduplicates_directory_cycles() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("real/inside.txt");
        symlink("real", fixture.root.join("linked")).expect("create directory symlink");

        let without_following = Walker::new(&fixture.root)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");
        assert!(
            !relative_paths(without_following.entries(), &fixture.root)
                .contains(&PathBuf::from("linked/inside.txt"))
        );

        let with_following = Walker::new(&fixture.root)
            .options(WalkOptions::default().follow_symlinks(true).sort(true))
            .collect()
            .expect("walk succeeds");
        assert_eq!(
            relative_paths(with_following.entries(), &fixture.root)
                .iter()
                .filter(|path| path.file_name().is_some_and(|name| name == "inside.txt"))
                .count(),
            1
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn preserves_non_utf8_native_paths() {
        use std::os::unix::ffi::OsStringExt;

        let fixture = Fixture::new();
        let name = std::ffi::OsString::from_vec(vec![b'n', 0xFF]);
        fixture.write(PathBuf::from(&name));

        let result = Walker::new(&fixture.root)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");
        assert_eq!(
            relative_paths(result.entries(), &fixture.root),
            vec![PathBuf::from(name)]
        );
    }
}
