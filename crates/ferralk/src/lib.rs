#![forbid(unsafe_code)]
#![doc = "Portable filesystem walking."]

//! A safe std::fs walker with a portable `std::fs` backend.
//!
//! Paths stay as PathBuf throughout the public API. Patterns are matched
//! against root-relative encoded path bytes; no filesystem result is converted
//! through UTF-8.

use std::{
    collections::VecDeque,
    collections::{HashMap, HashSet},
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

mod parallel;
mod scheduler;

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
    directories_only: bool,
    files_only: bool,
    skip_hidden: bool,
    keep_git_dir: bool,
    max_depth: Option<usize>,
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

    /// Returns only directories while continuing to traverse through them.
    #[must_use]
    pub const fn directories_only(mut self, enabled: bool) -> Self {
        self.directories_only = enabled;
        self
    }

    /// Returns only files while continuing to traverse through directories.
    #[must_use]
    pub const fn files_only(mut self, enabled: bool) -> Self {
        self.files_only = enabled;
        self
    }

    /// Excludes entries with a leading-period path component and does not
    /// descend into hidden directories.
    #[must_use]
    pub const fn skip_hidden(mut self, enabled: bool) -> Self {
        self.skip_hidden = enabled;
        self
    }

    /// Keeps `.git` directories when Gitignore matching is enabled.
    #[must_use]
    pub const fn keep_git_dir(mut self, enabled: bool) -> Self {
        self.keep_git_dir = enabled;
        self
    }

    /// Includes entries through `max_depth` components below the root without
    /// descending into directories at that depth.
    #[must_use]
    pub const fn max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = Some(max_depth);
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
    includes: Vec<TraversalPattern>,
    excludes: Vec<TraversalPattern>,
    options: WalkOptions,
    error_policy: ErrorPolicy,
    cancellation: Option<CancellationToken>,
    respect_git_ignore: bool,
    threads: usize,
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
            threads: std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1),
        }
    }

    /// Adds an OR-ed include pattern. No includes means every non-excluded
    /// entry is returned.
    pub fn include(mut self, pattern: impl AsRef<[u8]>) -> Result<Self, PatternError> {
        self.includes
            .push(TraversalPattern::compile(pattern.as_ref())?);
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

    /// Limits `collect()` to this many workers. Zero is clamped to one;
    /// `stream()` remains single-threaded to preserve incremental delivery.
    #[must_use]
    pub const fn threads(mut self, threads: usize) -> Self {
        self.threads = if threads == 0 { 1 } else { threads };
        self
    }

    /// Runs the portable backend to completion, using the configured workers.
    ///
    /// A panic inside a worker stops the sibling workers and is resumed on the
    /// calling thread after they have been joined.
    pub fn collect(self) -> Result<WalkResult, WalkError> {
        if self.threads > 1 {
            return parallel::collect(self);
        }
        let backend = StdBackend;
        let mut state = WalkState::new(&self);
        // Use the same injector-to-worker transfer as the forthcoming parallel
        // backend. The serial baseline owns one local queue and deliberately
        // drains it before returning; worker creation and task fan-out remain
        // M3 work.
        let scheduler = scheduler::Scheduler::new();
        scheduler.push(self.root.clone());
        let worker = scheduler.worker();
        while let Some(directory) = scheduler.steal_into(&worker).or_else(|| worker.pop()) {
            state.walk_directory(&backend, directory)?;
        }
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
            gitignore_cache: HashMap::new(),
            cancelled: false,
            stopped: false,
        }
    }

    fn may_descend_into(&self, relative: &[u8]) -> bool {
        self.includes.is_empty()
            || self
                .includes
                .iter()
                .any(|pattern| pattern.could_match_descendant(relative))
    }

    fn may_descend_path(&self, relative: &Path, bytes: &[u8]) -> bool {
        self.includes_depth(relative)
            && self
                .options
                .max_depth
                .is_none_or(|max_depth| relative.components().count() < max_depth)
            && self.may_descend_into(bytes)
    }

    fn includes_depth(&self, relative: &Path) -> bool {
        self.options
            .max_depth
            .is_none_or(|max_depth| relative.components().count() <= max_depth)
    }

    fn may_include_file(&self, relative: &[u8]) -> bool {
        self.includes.is_empty()
            || self
                .includes
                .iter()
                .any(|pattern| pattern.matches_extension(relative))
    }
}

fn has_hidden_component(path: &[u8]) -> bool {
    path.split(is_path_separator)
        .any(|component| component.first() == Some(&b'.'))
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
    literal_root: Option<Vec<u8>>,
    extension: Option<Vec<u8>>,
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
            literal_root: literal_pattern_root(pattern),
            extension: literal_extension(pattern),
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

    fn could_match_descendant(&self, path: &[u8]) -> bool {
        let Some(root) = &self.literal_root else {
            return true;
        };
        root == path
            || root
                .strip_prefix(path)
                .is_some_and(|suffix| suffix.starts_with(b"/"))
            || path
                .strip_prefix(root.as_slice())
                .is_some_and(|suffix| suffix.starts_with(b"/"))
    }

    fn matches_extension(&self, path: &[u8]) -> bool {
        let Some(extension) = &self.extension else {
            return true;
        };
        final_extension(path).is_some_and(|candidate| candidate == extension)
    }
}

fn literal_pattern_root(pattern: &[u8]) -> Option<Vec<u8>> {
    let magic = pattern.iter().enumerate().position(|(index, byte)| {
        matches!(byte, b'*' | b'?' | b'[')
            || (*byte == b'\\')
            || (*byte == b'{' && has_closing_brace(pattern, index))
            || (matches!(byte, b'@' | b'+' | b'!')
                && pattern.get(index + 1) == Some(&b'(')
                && has_closing_parenthesis(pattern, index + 1))
    });
    let prefix = &pattern[..magic.unwrap_or(pattern.len())];
    let root = if magic.is_some() {
        if let Some(prefix) = prefix.strip_suffix(b"/") {
            prefix
        } else {
            prefix
                .iter()
                .rposition(|byte| *byte == b'/')
                .map_or(prefix, |separator| &prefix[..separator])
        }
    } else {
        prefix
    };
    (!root.is_empty()).then(|| root.to_vec())
}

fn literal_extension(pattern: &[u8]) -> Option<Vec<u8>> {
    let extension = final_extension(pattern)?;
    if extension.is_empty()
        || extension.iter().any(|byte| {
            matches!(
                byte,
                b'*' | b'?' | b'[' | b']' | b'{' | b'}' | b'\\' | b'(' | b')' | b'|'
            )
        })
    {
        return None;
    }
    Some(extension.to_vec())
}

fn final_extension(path: &[u8]) -> Option<&[u8]> {
    let name = path.rsplit(is_path_separator).next().unwrap_or(path);
    let dot = name.iter().rposition(|byte| *byte == b'.')?;
    name.get(dot + 1..)
}

fn is_path_separator(byte: &u8) -> bool {
    *byte == b'/' || (cfg!(windows) && *byte == b'\\')
}

fn has_closing_brace(pattern: &[u8], open: usize) -> bool {
    let mut depth = 0_usize;
    let mut index = open;
    while index < pattern.len() {
        if pattern[index] == b'\\' {
            index += 2;
            continue;
        }
        match pattern[index] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
            }
            _ => {}
        }
        index += 1;
    }
    false
}

fn has_closing_parenthesis(pattern: &[u8], open: usize) -> bool {
    let mut depth = 0_usize;
    let mut index = open;
    while index < pattern.len() {
        if pattern[index] == b'\\' {
            index += 2;
            continue;
        }
        match pattern[index] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return true;
                }
            }
            _ => {}
        }
        index += 1;
    }
    false
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
    gitignore_cache: HashMap<PathBuf, Arc<GitIgnoreNode>>,
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
        if !self.walker.includes_depth(relative) {
            return None;
        }
        let bytes = relative.as_os_str().as_encoded_bytes();
        if self.walker.options.skip_hidden && has_hidden_component(bytes) {
            return None;
        }
        if should_skip_git_directory(&self.walker, &entry.path) {
            return None;
        }
        if self
            .walker
            .excludes
            .iter()
            .any(|pattern| pattern.matches(bytes))
        {
            return None;
        }
        let git_ignored = is_git_ignored(
            &self.walker,
            &entry.path,
            entry.is_dir,
            &mut self.gitignore_cache,
        );
        if git_ignored && !entry.is_dir {
            return None;
        }
        if entry.is_symlink && self.walker.options.follow_symlinks {
            match fs::metadata(&entry.path) {
                Ok(metadata) => entry.is_dir = metadata.is_dir(),
                Err(source) => return self.error("metadata", entry.path, source),
            }
        }
        if !entry.is_dir && !self.walker.may_include_file(bytes) {
            return None;
        }
        if entry.is_dir
            && !self
                .walker
                .excludes
                .iter()
                .any(|pattern| pattern.covers_subtree(bytes))
            && self.walker.may_descend_path(relative, bytes)
        {
            self.pending_directories.push(entry.path.clone());
        }
        if !self.walker.includes.is_empty()
            && !self
                .walker
                .includes
                .iter()
                .any(|pattern| pattern.matches(bytes))
        {
            return None;
        }
        if git_ignored {
            return None;
        }
        if self.walker.options.directories_only && !entry.is_dir {
            return None;
        }
        if self.walker.options.files_only && entry.is_dir {
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
    gitignore_cache: HashMap<PathBuf, Arc<GitIgnoreNode>>,
    cancelled: bool,
}

impl<'walker> WalkState<'walker> {
    fn new(walker: &'walker Walker) -> Self {
        Self {
            walker,
            entries: Vec::new(),
            errors: Vec::new(),
            visited_directories: HashSet::new(),
            gitignore_cache: HashMap::new(),
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
        if !self.walker.includes_depth(relative) {
            return Ok(());
        }
        let bytes = relative.as_os_str().as_encoded_bytes();
        if self.walker.options.skip_hidden && has_hidden_component(bytes) {
            return Ok(());
        }
        if should_skip_git_directory(self.walker, &entry.path) {
            return Ok(());
        }
        if self
            .walker
            .excludes
            .iter()
            .any(|pattern| pattern.matches(bytes))
        {
            return Ok(());
        }
        let git_ignored = is_git_ignored(
            self.walker,
            &entry.path,
            entry.is_dir,
            &mut self.gitignore_cache,
        );
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
        if !entry.is_dir && !self.walker.may_include_file(bytes) {
            return Ok(());
        }
        if entry.is_dir
            && !self
                .walker
                .excludes
                .iter()
                .any(|pattern| pattern.covers_subtree(bytes))
            && self.walker.may_descend_path(relative, bytes)
        {
            self.walk_directory(backend, entry.path.clone())?;
        }

        if self.walker.options.files_only && entry.is_dir {
            return Ok(());
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

        if (!self.walker.options.directories_only || entry.is_dir)
            && (self.walker.includes.is_empty()
                || self
                    .walker
                    .includes
                    .iter()
                    .any(|pattern| pattern.matches(bytes)))
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

fn is_git_ignored(
    walker: &Walker,
    path: &Path,
    is_dir: bool,
    cache: &mut HashMap<PathBuf, Arc<GitIgnoreNode>>,
) -> bool {
    if !walker.respect_git_ignore {
        return false;
    }
    let directory = path
        .parent()
        .filter(|parent| parent.starts_with(&walker.root));
    directory
        .is_some_and(|directory| gitignore_node(walker, directory, cache).is_ignored(path, is_dir))
}

fn should_skip_git_directory(walker: &Walker, path: &Path) -> bool {
    walker.respect_git_ignore
        && !walker.options.keep_git_dir
        && path.file_name().is_some_and(|name| name == ".git")
}

/// One directory's parsed ignore rules plus its immutable inherited chain.
/// Nodes are cached per walk, so siblings reuse the same parent evaluation.
struct GitIgnoreNode {
    rules: Gitignore,
    parent: Option<Arc<Self>>,
}

impl fmt::Debug for GitIgnoreNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitIgnoreNode")
            .field("has_parent", &self.parent.is_some())
            .finish_non_exhaustive()
    }
}

impl GitIgnoreNode {
    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut ignored = self
            .parent
            .as_ref()
            .is_some_and(|parent| parent.is_ignored(path, is_dir));
        let matched = self.rules.matched_path_or_any_parents(path, is_dir);
        if !matched.is_none() {
            ignored = matched.is_ignore();
        }
        ignored
    }
}

fn gitignore_node(
    walker: &Walker,
    directory: &Path,
    cache: &mut HashMap<PathBuf, Arc<GitIgnoreNode>>,
) -> Arc<GitIgnoreNode> {
    if let Some(node) = cache.get(directory) {
        return Arc::clone(node);
    }
    let parent = (directory != walker.root)
        .then(|| {
            directory
                .parent()
                .filter(|parent| parent.starts_with(&walker.root))
        })
        .flatten()
        .map(|parent| gitignore_node(walker, parent, cache));
    let node = Arc::new(GitIgnoreNode {
        rules: Gitignore::new(directory.join(".gitignore")).0,
        parent,
    });
    cache.insert(directory.to_path_buf(), Arc::clone(&node));
    node
}

/// Crate version exposed for build and integration diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        CancellationToken, ErrorPolicy, TraversalPattern, WalkEntry, WalkOptions, Walker,
        gitignore_node, literal_extension, literal_pattern_root,
    };

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
    fn directories_only_filters_results_without_pruning_descendants() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs");
        fixture.write("src/nested/lib.rs");
        let options = WalkOptions::default().directories_only(true).sort(true);

        let serial = Walker::new(&fixture.root)
            .threads(1)
            .options(options)
            .collect()
            .expect("serial walk succeeds");
        let parallel = Walker::new(&fixture.root)
            .threads(4)
            .options(options)
            .collect()
            .expect("parallel walk succeeds");
        let streamed = Walker::new(&fixture.root)
            .options(options)
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream succeeds");
        let expected = vec![PathBuf::from("src"), PathBuf::from("src/nested")];

        assert_eq!(relative_paths(serial.entries(), &fixture.root), expected);
        assert_eq!(relative_paths(parallel.entries(), &fixture.root), expected);
        assert_eq!(relative_paths(&streamed, &fixture.root), expected);
        assert!(streamed.iter().all(WalkEntry::is_dir));
    }

    #[test]
    fn max_depth_keeps_boundary_entries_without_descending() {
        let fixture = Fixture::new();
        fixture.write("top.txt");
        fixture.write("d1/mid.txt");
        fixture.write("d1/d2/bottom.txt");

        for (max_depth, expected) in [
            (0, vec![]),
            (1, vec![PathBuf::from("d1"), PathBuf::from("top.txt")]),
            (
                2,
                vec![
                    PathBuf::from("d1"),
                    PathBuf::from("d1/d2"),
                    PathBuf::from("d1/mid.txt"),
                    PathBuf::from("top.txt"),
                ],
            ),
        ] {
            let options = WalkOptions::default().max_depth(max_depth).sort(true);
            let serial = Walker::new(&fixture.root)
                .threads(1)
                .options(options)
                .collect()
                .expect("serial walk succeeds");
            let parallel = Walker::new(&fixture.root)
                .threads(4)
                .options(options)
                .collect()
                .expect("parallel walk succeeds");
            let mut streamed = Walker::new(&fixture.root)
                .options(options)
                .stream()
                .collect::<Result<Vec<_>, _>>()
                .expect("stream succeeds");
            streamed.sort_by(|left, right| left.path.cmp(&right.path));

            assert_eq!(relative_paths(serial.entries(), &fixture.root), expected);
            assert_eq!(relative_paths(parallel.entries(), &fixture.root), expected);
            assert_eq!(relative_paths(&streamed, &fixture.root), expected);
        }
    }

    #[test]
    fn parallel_collect_matches_the_serial_result_multiset() {
        let fixture = Fixture::new();
        fixture.write("wide/a.txt");
        fixture.write("wide/b.txt");
        fixture.write("deep/one/two/three/leaf.txt");
        fixture.write("ignored.tmp");
        fs::write(fixture.root.join(".gitignore"), b"*.tmp\n").expect("write gitignore");

        let serial = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(1)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("serial walk succeeds");
        let parallel = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(4)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("parallel walk succeeds");

        assert_eq!(
            relative_paths(parallel.entries(), &fixture.root),
            relative_paths(serial.entries(), &fixture.root)
        );
        assert!(parallel.errors().is_empty());
        assert!(serial.errors().is_empty());
    }

    #[test]
    fn parallel_collect_stress_covers_empty_shallow_and_imbalanced_trees() {
        let empty = Fixture::new();
        assert!(
            Walker::new(&empty.root)
                .threads(8)
                .collect()
                .expect("empty parallel walk succeeds")
                .entries()
                .is_empty()
        );

        let fixture = Fixture::new();
        fixture.write("shallow.txt");
        for branch in 0..8 {
            fixture.write(format!("wide/{branch}/leaf.txt"));
        }
        for depth in 0..20 {
            fixture.write(format!("deep/{depth}/next/leaf.txt"));
        }

        let serial = Walker::new(&fixture.root)
            .threads(1)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("serial walk succeeds");
        let expected = relative_paths(serial.entries(), &fixture.root);
        for _ in 0..32 {
            let actual = Walker::new(&fixture.root)
                .threads(8)
                .options(WalkOptions::default().sort(true))
                .collect()
                .expect("parallel stress walk succeeds");
            assert_eq!(relative_paths(actual.entries(), &fixture.root), expected);
            assert!(actual.errors().is_empty());
        }
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
    fn gitignore_nodes_share_their_immutable_parent_chain() {
        let fixture = Fixture::new();
        let walker = Walker::new(&fixture.root).respect_git_ignore(true);
        let mut cache = HashMap::new();
        let left = gitignore_node(&walker, &fixture.root.join("left/nested"), &mut cache);
        let right = gitignore_node(&walker, &fixture.root.join("left/sibling"), &mut cache);

        let left_parent = left.parent.as_ref().expect("nested left parent");
        let right_parent = right.parent.as_ref().expect("nested right parent");
        assert!(Arc::ptr_eq(left_parent, right_parent));
        assert_eq!(cache.len(), 4, "root, left, and two child nodes are cached");
    }

    #[test]
    fn git_ignore_corpus_replays_through_the_walker() {
        let corpus_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ignore.jsonl");
        for line in fs::read_to_string(corpus_path)
            .expect("read ignore corpus")
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            let case: corpus::Case = serde_json::from_str(line).expect("valid ignore corpus case");
            let fixture = Fixture::new();
            fs::write(
                fixture.root.join(".gitignore"),
                case.ignore_rules.join("\n").as_bytes(),
            )
            .expect("write fixture gitignore");
            fixture.write(&case.path);

            let result = Walker::new(&fixture.root)
                .respect_git_ignore(true)
                .collect()
                .expect("walk succeeds");
            let returned =
                relative_paths(result.entries(), &fixture.root).contains(&PathBuf::from(case.path));
            assert_eq!(
                !returned, case.expected,
                "walker verdict for corpus case {}",
                case.id
            );
        }
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

        assert_eq!(
            literal_pattern_root(b"src/foo/*.rs"),
            Some(b"src/foo".to_vec())
        );
        assert_eq!(literal_pattern_root(b"src/foo*.rs"), Some(b"src".to_vec()));
        assert_eq!(literal_pattern_root(b"**/*.rs"), None);
        assert_eq!(
            literal_pattern_root(b"foo+bar/**/*.rs"),
            Some(b"foo+bar".to_vec())
        );
        assert_eq!(
            literal_pattern_root(b"foo@(bar/**/*.rs"),
            Some(b"foo@(bar".to_vec())
        );

        let rust_sources = TraversalPattern::compile(b"src/**/*.rs").expect("valid suffix");
        assert!(rust_sources.matches_extension(b"src/lib.rs"));
        assert!(!rust_sources.matches_extension(b"src/lib.txt"));
        assert_eq!(literal_extension(b"src/**/*.{rs,ts}"), None);
        assert_eq!(literal_extension(b"src/**/*.rs"), Some(b"rs".to_vec()));
    }

    #[test]
    fn literal_include_roots_prune_unrelated_sibling_directories() {
        struct RecordingBackend {
            entries: HashMap<PathBuf, Vec<super::BackendEntry>>,
            reads: RefCell<Vec<PathBuf>>,
        }

        impl super::DirectoryBackend for RecordingBackend {
            fn read_directory(&self, path: &Path) -> std::io::Result<Vec<super::BackendEntry>> {
                self.reads.borrow_mut().push(path.to_path_buf());
                Ok(self.entries.get(path).cloned().unwrap_or_default())
            }
        }

        let root = PathBuf::from("/fixture");
        let source = root.join("src");
        let docs = root.join("docs");
        let mut entries = HashMap::new();
        entries.insert(
            root.clone(),
            vec![
                super::BackendEntry {
                    path: source.clone(),
                    is_dir: true,
                    is_symlink: false,
                },
                super::BackendEntry {
                    path: docs.clone(),
                    is_dir: true,
                    is_symlink: false,
                },
            ],
        );
        entries.insert(
            source.clone(),
            vec![super::BackendEntry {
                path: source.join("main.rs"),
                is_dir: false,
                is_symlink: false,
            }],
        );
        let backend = RecordingBackend {
            entries,
            reads: RefCell::new(Vec::new()),
        };
        let walker = Walker::new(&root)
            .include("src/**/*.rs")
            .expect("valid include");
        let mut state = super::WalkState::new(&walker);

        state
            .walk_directory(&backend, root.clone())
            .expect("backend walk succeeds");

        assert_eq!(backend.reads.into_inner(), vec![root, source]);
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries[0].path(), Path::new("/fixture/src/main.rs"));
    }

    #[test]
    fn metadata_error_is_retained_when_a_dirent_disappears_before_stat() {
        struct DisappearingFileBackend {
            root: PathBuf,
            disappeared: PathBuf,
        }

        impl super::DirectoryBackend for DisappearingFileBackend {
            fn read_directory(&self, path: &Path) -> std::io::Result<Vec<super::BackendEntry>> {
                if path == self.root {
                    Ok(vec![super::BackendEntry {
                        path: self.disappeared.clone(),
                        is_dir: false,
                        is_symlink: false,
                    }])
                } else {
                    Ok(Vec::new())
                }
            }
        }

        let fixture = Fixture::new();
        let disappeared = fixture.root.join("gone.rs");
        let walker = Walker::new(&fixture.root)
            .threads(1)
            .options(WalkOptions::default().metadata(true));
        let backend = DisappearingFileBackend {
            root: fixture.root.clone(),
            disappeared: disappeared.clone(),
        };
        let mut state = super::WalkState::new(&walker);

        state
            .walk_directory(&backend, fixture.root.clone())
            .expect("collect policy retains the metadata error");

        assert!(state.entries.is_empty());
        assert_eq!(state.errors.len(), 1);
        assert_eq!(state.errors[0].operation(), "symlink_metadata");
        assert_eq!(state.errors[0].path(), disappeared);
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

    #[cfg(unix)]
    #[test]
    fn parallel_collect_retains_concurrent_metadata_errors() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("left/ok.txt");
        fixture.write("right/ok.txt");
        symlink("missing-left", fixture.root.join("left/dangling"))
            .expect("create left dangling symlink");
        symlink("missing-right", fixture.root.join("right/dangling"))
            .expect("create right dangling symlink");

        let options = WalkOptions::default().follow_symlinks(true).sort(true);
        let serial = Walker::new(&fixture.root)
            .threads(1)
            .options(options)
            .error_policy(ErrorPolicy::Collect)
            .collect()
            .expect("serial walk retains errors");
        let parallel = Walker::new(&fixture.root)
            .threads(4)
            .options(options)
            .error_policy(ErrorPolicy::Collect)
            .collect()
            .expect("parallel walk retains errors");

        let error_paths = |result: &super::WalkResult| {
            let mut errors = result
                .errors()
                .iter()
                .map(|error| {
                    (
                        error.operation(),
                        error
                            .path()
                            .strip_prefix(&fixture.root)
                            .expect("error is rooted in fixture")
                            .to_path_buf(),
                    )
                })
                .collect::<Vec<_>>();
            errors.sort_unstable();
            errors
        };
        assert_eq!(error_paths(&parallel), error_paths(&serial));
        assert_eq!(parallel.errors().len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn parallel_abort_returns_an_error_and_cancels_the_shared_token() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("left/ok.txt");
        fixture.write("right/ok.txt");
        symlink("missing-left", fixture.root.join("left/dangling"))
            .expect("create left dangling symlink");
        symlink("missing-right", fixture.root.join("right/dangling"))
            .expect("create right dangling symlink");
        let cancellation = CancellationToken::default();

        let error = Walker::new(&fixture.root)
            .threads(4)
            .options(WalkOptions::default().follow_symlinks(true))
            .error_policy(ErrorPolicy::Abort)
            .cancellation(cancellation.clone())
            .collect()
            .expect_err("abort policy returns the first metadata error");

        assert_eq!(error.operation(), "metadata");
        assert!(cancellation.is_cancelled());
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
