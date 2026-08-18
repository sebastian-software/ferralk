#![forbid(unsafe_code)]
#![doc = "Portable, serial filesystem walking."]

//! A safe std::fs walker used as the portable M2 baseline.
//!
//! Paths stay as PathBuf throughout the public API. Patterns are matched
//! against root-relative encoded path bytes; no filesystem result is converted
//! through UTF-8.

use std::{
    collections::HashSet,
    collections::VecDeque,
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ferralk_glob::{Pattern, PatternError, PatternOptions};
use ignore::gitignore::Gitignore;

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

/// Cloneable cooperative cancellation handle for a walk.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Requests that the walker stop before its next filesystem operation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Behaviour switches for a Walker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WalkOptions {
    follow_symlinks: bool,
    sort: bool,
    metadata: bool,
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

    /// Collects filesystem metadata for every returned entry.
    #[must_use]
    pub const fn metadata(mut self, enabled: bool) -> Self {
        self.metadata = enabled;
        self
    }
}

/// One matching filesystem entry.
#[derive(Debug)]
pub struct WalkEntry {
    path: PathBuf,
    is_dir: bool,
    metadata: Option<fs::Metadata>,
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

    /// Metadata collected when WalkOptions metadata is enabled.
    #[must_use]
    pub fn metadata(&self) -> Option<&fs::Metadata> {
        self.metadata.as_ref()
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
    cancelled: bool,
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

    /// Whether a cancellation request stopped traversal before completion.
    #[must_use]
    pub const fn was_cancelled(&self) -> bool {
        self.cancelled
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
    cancellation: Option<CancellationToken>,
    respect_git_ignore: bool,
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
            cancellation: None,
            respect_git_ignore: false,
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

    /// Associates an externally owned cooperative cancellation handle.
    #[must_use]
    pub fn cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Applies the root .gitignore with Git-compatible matching semantics.
    #[must_use]
    pub const fn respect_git_ignore(mut self, enabled: bool) -> Self {
        self.respect_git_ignore = enabled;
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
            cancelled: state.cancelled,
        })
    }

    /// Starts an incremental unsorted traversal. Unlike collect, recoverable
    /// errors are yielded as individual iterator items under Collect; sorting
    /// is intentionally a collect-only global operation.
    #[must_use]
    pub fn stream(self) -> WalkStream {
        WalkStream {
            pending_directories: vec![self.root.clone()],
            walker: self,
            pending_entries: VecDeque::new(),
            visited_directories: HashSet::new(),
            cancelled: false,
            stopped: false,
        }
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

/// Incremental portable traversal produced by Walker stream.
#[derive(Debug)]
pub struct WalkStream {
    walker: Walker,
    pending_directories: Vec<PathBuf>,
    pending_entries: VecDeque<BackendEntry>,
    visited_directories: HashSet<PathBuf>,
    cancelled: bool,
    stopped: bool,
}

impl WalkStream {
    /// Whether a cancellation request ended this stream.
    #[must_use]
    pub const fn was_cancelled(&self) -> bool {
        self.cancelled
    }

    fn check_cancellation(&mut self) -> bool {
        self.cancelled |= self
            .walker
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled);
        self.cancelled
    }

    fn error(
        &mut self,
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    ) -> Option<Result<WalkEntry, WalkError>> {
        let error = WalkError::new(operation, path, source);
        match self.walker.error_policy {
            ErrorPolicy::Abort => {
                self.stopped = true;
                Some(Err(error))
            }
            ErrorPolicy::Skip => None,
            ErrorPolicy::Collect => Some(Err(error)),
        }
    }

    fn prepare_directory(&mut self, directory: PathBuf) -> Option<Result<WalkEntry, WalkError>> {
        if self.walker.options.follow_symlinks {
            match fs::canonicalize(&directory) {
                Ok(canonical) => {
                    if !self.visited_directories.insert(canonical) {
                        return None;
                    }
                }
                Err(source) => return self.error("canonicalize", directory, source),
            }
        }
        match StdBackend.read_directory(&directory) {
            Ok(entries) => {
                self.pending_entries = entries.into();
                None
            }
            Err(source) => self.error("read_dir", directory, source),
        }
    }

    fn process_entry(&mut self, mut entry: BackendEntry) -> Option<Result<WalkEntry, WalkError>> {
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
            return None;
        }
        let git_ignored = is_git_ignored(&self.walker, &entry.path, entry.is_dir);
        if git_ignored && !entry.is_dir {
            return None;
        }
        if entry.is_symlink && self.walker.options.follow_symlinks {
            match fs::metadata(&entry.path) {
                Ok(metadata) => entry.is_dir = metadata.is_dir(),
                Err(source) => return self.error("metadata", entry.path, source),
            }
        }
        if entry.is_dir
            && !self
                .walker
                .excludes
                .iter()
                .any(|pattern| pattern.covers_subtree(bytes))
        {
            self.pending_directories.push(entry.path.clone());
        }
        if !self.walker.includes.is_empty()
            && !self
                .walker
                .includes
                .iter()
                .any(|pattern| pattern.is_match(bytes))
        {
            return None;
        }
        if git_ignored {
            return None;
        }
        let metadata = if self.walker.options.metadata {
            match fs::symlink_metadata(&entry.path) {
                Ok(metadata) => Some(metadata),
                Err(source) => return self.error("symlink_metadata", entry.path, source),
            }
        } else {
            None
        };
        Some(Ok(WalkEntry {
            path: entry.path,
            is_dir: entry.is_dir,
            metadata,
        }))
    }
}

impl Iterator for WalkStream {
    type Item = Result<WalkEntry, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.stopped {
            if self.check_cancellation() {
                self.stopped = true;
                return None;
            }
            if let Some(entry) = self.pending_entries.pop_front() {
                if let Some(result) = self.process_entry(entry) {
                    return Some(result);
                }
                continue;
            }
            let directory = self.pending_directories.pop()?;
            if let Some(result) = self.prepare_directory(directory) {
                return Some(result);
            }
        }
        None
    }
}

struct WalkState<'walker> {
    walker: &'walker Walker,
    entries: Vec<WalkEntry>,
    errors: Vec<WalkError>,
    visited_directories: HashSet<PathBuf>,
    cancelled: bool,
}

impl<'walker> WalkState<'walker> {
    fn new(walker: &'walker Walker) -> Self {
        Self {
            walker,
            entries: Vec::new(),
            errors: Vec::new(),
            visited_directories: HashSet::new(),
            cancelled: false,
        }
    }

    fn walk_directory(
        &mut self,
        backend: &impl DirectoryBackend,
        directory: PathBuf,
    ) -> Result<(), WalkError> {
        if self.check_cancellation() {
            return Ok(());
        }
        if self.walker.options.follow_symlinks && !self.mark_directory(&directory)? {
            return Ok(());
        }
        let entries = match backend.read_directory(&directory) {
            Ok(entries) => entries,
            Err(source) => return self.handle_error("read_dir", directory, source),
        };
        for entry in entries {
            if self.check_cancellation() {
                return Ok(());
            }
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
        if self.check_cancellation() {
            return Ok(());
        }
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
        let git_ignored = is_git_ignored(self.walker, &entry.path, entry.is_dir);
        if git_ignored && !entry.is_dir {
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

        let metadata = if self.walker.options.metadata {
            match fs::symlink_metadata(&entry.path) {
                Ok(metadata) => Some(metadata),
                Err(source) => {
                    self.handle_error("symlink_metadata", entry.path.clone(), source)?;
                    return Ok(());
                }
            }
        } else {
            None
        };

        if self.walker.includes.is_empty()
            || self
                .walker
                .includes
                .iter()
                .any(|pattern| pattern.is_match(bytes))
        {
            if git_ignored {
                return Ok(());
            }
            self.entries.push(WalkEntry {
                path: entry.path,
                is_dir: entry.is_dir,
                metadata,
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

    fn check_cancellation(&mut self) -> bool {
        self.cancelled |= self
            .walker
            .cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled);
        self.cancelled
    }
}

fn is_git_ignored(walker: &Walker, path: &Path, is_dir: bool) -> bool {
    if !walker.respect_git_ignore {
        return false;
    }
    let mut directories = Vec::new();
    let mut current = path
        .parent()
        .filter(|parent| parent.starts_with(&walker.root));
    while let Some(directory) = current {
        directories.push(directory);
        if directory == walker.root {
            break;
        }
        current = directory.parent();
    }
    let mut ignored = false;
    for directory in directories.into_iter().rev() {
        let (rules, _) = Gitignore::new(directory.join(".gitignore"));
        let matched = rules.matched_path_or_any_parents(path, is_dir);
        if !matched.is_none() {
            ignored = matched.is_ignore();
        }
    }
    ignored
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

    use super::{CancellationToken, ErrorPolicy, TraversalPattern, WalkEntry, WalkOptions, Walker};

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
    fn metadata_collection_is_explicit_and_preserves_file_size() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs");

        let without_metadata = Walker::new(&fixture.root)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");
        assert!(
            without_metadata
                .entries()
                .iter()
                .all(|entry| entry.metadata().is_none())
        );

        let with_metadata = Walker::new(&fixture.root)
            .options(WalkOptions::default().sort(true).metadata(true))
            .collect()
            .expect("walk succeeds");
        assert_eq!(
            with_metadata
                .entries()
                .iter()
                .find(|entry| entry.path().ends_with("main.rs"))
                .expect("fixture file is returned")
                .metadata()
                .expect("metadata is requested")
                .len(),
            7
        );
    }

    #[test]
    fn root_gitignore_rules_and_negation_apply_to_collect_and_stream() {
        let fixture = Fixture::new();
        fixture.write("generated.tmp");
        fixture.write("keep.tmp");
        fixture.write("src/main.rs");
        fixture.write("src/keep.tmp");
        fixture.write("build/keep.txt");
        fs::write(
            fixture.root.join(".gitignore"),
            b"*.tmp\n!keep.tmp\nbuild/\n",
        )
        .expect("write root gitignore");
        fs::write(fixture.root.join("src/.gitignore"), b"!keep.tmp\n")
            .expect("write nested gitignore");
        fs::write(fixture.root.join("build/.gitignore"), b"!keep.txt\n")
            .expect("write nested re-include");

        let collected = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");
        let collected_paths = relative_paths(collected.entries(), &fixture.root);
        assert!(!collected_paths.contains(&PathBuf::from("generated.tmp")));
        assert!(collected_paths.contains(&PathBuf::from("keep.tmp")));
        assert!(collected_paths.contains(&PathBuf::from("src/keep.tmp")));
        assert!(collected_paths.contains(&PathBuf::from("build/keep.txt")));
        assert!(!collected_paths.contains(&PathBuf::from("build")));

        let streamed = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream has no I/O errors");
        let streamed_paths = relative_paths(&streamed, &fixture.root);
        assert!(!streamed_paths.contains(&PathBuf::from("generated.tmp")));
        assert!(streamed_paths.contains(&PathBuf::from("keep.tmp")));
        assert!(streamed_paths.contains(&PathBuf::from("src/keep.tmp")));
        assert!(streamed_paths.contains(&PathBuf::from("build/keep.txt")));
        assert!(!streamed_paths.contains(&PathBuf::from("build")));
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

    #[test]
    fn cancellation_returns_a_partial_result_without_an_io_error() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs");
        let cancellation = CancellationToken::default();
        cancellation.cancel();

        let result = Walker::new(&fixture.root)
            .cancellation(cancellation)
            .collect()
            .expect("cancellation is a normal partial result");
        assert!(result.was_cancelled());
        assert!(result.entries().is_empty());
        assert!(result.errors().is_empty());
    }

    #[test]
    fn stream_yields_filtered_entries_incrementally_and_honours_cancellation() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs");
        fixture.write("src/lib.txt");

        let mut stream = Walker::new(&fixture.root)
            .include("**/*.rs")
            .expect("valid include")
            .stream();
        let entries = stream
            .by_ref()
            .map(|entry| entry.expect("fixture has no I/O errors"))
            .collect::<Vec<_>>();
        assert_eq!(
            relative_paths(&entries, &fixture.root),
            vec![PathBuf::from("src/main.rs")]
        );
        assert!(!stream.was_cancelled());

        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let mut cancelled = Walker::new(&fixture.root)
            .cancellation(cancellation)
            .stream();
        assert!(cancelled.next().is_none());
        assert!(cancelled.was_cancelled());
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
