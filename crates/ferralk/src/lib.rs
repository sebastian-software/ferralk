#![deny(unsafe_code)]
#![doc = "Portable filesystem walking."]

//! A safe std::fs walker with a portable `std::fs` backend.
//!
//! Paths stay as PathBuf throughout the public API. Patterns are matched
//! against root-relative encoded path bytes; no filesystem result is converted
//! through UTF-8.

use std::{
    borrow::Cow,
    collections::HashSet,
    error::Error,
    ffi::{OsStr, OsString},
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ferralk_glob::{Pattern, PatternError, PatternOptions};

pub use ferralk_glob;

#[cfg(all(feature = "native-linux", target_os = "linux"))]
#[allow(unsafe_code)]
mod linux_native;
#[cfg(all(feature = "native-macos", target_os = "macos"))]
#[allow(unsafe_code)]
mod macos_native;
/// Differential parity between the active native backend and the portable one.
#[cfg(all(
    test,
    any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    )
))]
mod native_parity;
#[cfg(all(feature = "native-linux", target_os = "linux"))]
#[doc(hidden)]
pub use linux_native::fuzz_validate_records as fuzz_validate_linux_dirent_records;
#[cfg(all(feature = "native-macos", target_os = "macos"))]
#[doc(hidden)]
pub use macos_native::fuzz_validate_bulk_record as fuzz_validate_macos_bulk_record;
#[cfg(all(feature = "native-macos", target_os = "macos"))]
#[doc(hidden)]
pub use macos_native::fuzz_validate_records as fuzz_validate_macos_dirent_records;
mod classify;
mod gitignore;
mod ignore_rules;

/// Fuzz entry point for the gitignore rule layer (ADR-0014), exported the way
/// the native dirent parsers are: for the harness in `fuzz/`, not for consumers.
#[doc(hidden)]
pub use ignore_rules::fuzz_rule as fuzz_ignore_rule;
mod parallel;
mod scheduler;

use classify::{DirectoryTask, EmittedEntry, EntryAction, classify_entry};
use gitignore::IgnoreScope;

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

/// How far an ordinary wildcard reaches in a walker pattern.
///
/// `*`, `?` and character classes are the ordinary wildcards; `**` crosses
/// separators under either mode.
///
/// ```
/// use ferralk::{WildcardMode, Walker};
///
/// // The default. `*.ts` selects a TypeScript file in the walk root, the way
/// // a shell glob does, and `src/*.ts` selects one directly inside `src`.
/// let scoped = Walker::new(".").include("*.ts")?;
///
/// // A wildcard spans separators, the way `globset` and `fast-glob` read a
/// // pattern by default: `*.ts` now also selects `a/b.ts`.
/// let crossing = Walker::new(".")
///     .wildcard_mode(WildcardMode::SeparatorCrossing)
///     .include("*.ts")?;
/// # Ok::<(), ferralk::ferralk_glob::PatternError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WildcardMode {
    /// A wildcard stays inside one path component: `*.ts` matches `main.ts`
    /// and not `src/main.ts`. Filesystem-glob semantics, and the default.
    #[default]
    ComponentScoped,
    /// A wildcard spans separators: `*.ts` matches `main.ts` and `src/main.ts`
    /// alike. This is how `globset` and `fast-glob` read an unconfigured
    /// pattern, so it is the mode to pick when porting patterns from them.
    SeparatorCrossing,
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
    ///
    /// This is a traversal filter, not matcher semantics, and therefore not the
    /// same switch as [`Walker::match_hidden`]: it removes hidden entries from
    /// the walk before any include or exclude pattern is consulted, while
    /// `match_hidden` decides whether a wildcard is allowed to cover a leading
    /// period at all. They compose in one direction only - with `skip_hidden`
    /// enabled no hidden path survives long enough for `match_hidden` to have
    /// anything to say about it.
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

/// Filesystem kind observed for one walked entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkEntryKind {
    /// A regular non-directory, non-symlink entry.
    File,
    /// A directory entry.
    Directory,
    /// A symbolic link, including one followed for traversal.
    Symlink,
}

/// One matching filesystem entry.
#[derive(Debug)]
pub struct WalkEntry {
    path: PathBuf,
    is_dir: bool,
    is_symlink: bool,
    depth: usize,
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

    /// Whether this entry was observed as a symbolic link.
    #[must_use]
    pub const fn is_symlink(&self) -> bool {
        self.is_symlink
    }

    /// Filesystem kind observed for this entry.
    #[must_use]
    pub const fn kind(&self) -> WalkEntryKind {
        if self.is_symlink {
            WalkEntryKind::Symlink
        } else if self.is_dir {
            WalkEntryKind::Directory
        } else {
            WalkEntryKind::File
        }
    }

    /// Number of path components between the walk root and this entry.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Native basename of this entry, when the path has a final component.
    #[must_use]
    pub fn basename(&self) -> Option<&std::ffi::OsStr> {
        self.path.file_name()
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

/// What a [`Walker::visit`] visitor decides about one entry.
///
/// The visitor runs on the thread that produced the entry, so a caller with a
/// matcher of its own filters in parallel instead of over the returned list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Keep the entry in the result.
    Keep,
    /// Leave the entry out of the result. Traversal is unaffected: a directory
    /// is still descended into, because pruning a subtree is what
    /// [`Walker::exclude`] expresses, and one verdict meaning different things
    /// for files and directories would be a trap.
    Skip,
    /// Leave the entry out and end the walk, the way a cancellation request
    /// does. A caller that wants the entry which stopped the walk records it in
    /// the visitor, where the decision was made anyway.
    Stop,
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

    /// Whether a cancellation request or a [`Verdict::Stop`] stopped traversal
    /// before completion.
    #[must_use]
    pub const fn was_cancelled(&self) -> bool {
        self.cancelled
    }
}

/// A borrowed per-entry visitor, shared by every worker of one walk.
///
/// Behind a reference rather than a generic parameter: the walk already reaches
/// its filesystem through `&dyn DirectoryBackend`, and one indirect call per
/// entry is not measurable against a traversal made of syscalls.
pub(crate) type EntryVisitor<'a> = &'a (dyn Fn(&WalkEntry) -> Verdict + Sync + 'a);

/// The visitor [`Walker::collect`] runs: every entry survives.
pub(crate) fn keep_every_entry(_: &WalkEntry) -> Verdict {
    Verdict::Keep
}

/// Builder for a portable serial traversal.
#[derive(Debug, Clone)]
pub struct Walker {
    root: PathBuf,
    /// Byte index at which the root-relative part of any path this walk builds
    /// begins. See [`Walker::relative_start`].
    relative_start: usize,
    includes: Vec<TraversalPattern>,
    excludes: Vec<TraversalPattern>,
    match_hidden: bool,
    options: WalkOptions,
    error_policy: ErrorPolicy,
    cancellation: Option<CancellationToken>,
    respect_git_ignore: bool,
    wildcard_mode: WildcardMode,
    threads: usize,
}

impl Walker {
    /// Where the root-relative part of a walked path starts, in bytes.
    ///
    /// Every path the walk produces is the root with names pushed onto it, so
    /// the offset is the same for all of them and is worth deriving once
    /// instead of running `strip_prefix` — a component-by-component comparison
    /// — over every entry. Pushing a name is what settles the question: it is
    /// what inserts the separator, and it inserts none when the root already
    /// ends with one or is empty.
    fn relative_start(root: &Path) -> usize {
        let mut probe = root.to_path_buf();
        probe.push("x");
        probe.as_os_str().as_encoded_bytes().len() - 1
    }

    /// Starts a walk rooted at root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            relative_start: Self::relative_start(&root),
            root,
            includes: Vec::new(),
            excludes: Vec::new(),
            match_hidden: false,
            options: WalkOptions::default(),
            error_policy: ErrorPolicy::default(),
            cancellation: None,
            respect_git_ignore: false,
            wildcard_mode: WildcardMode::default(),
            threads: std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1),
        }
    }

    /// Adds an OR-ed include pattern. No includes means every non-excluded
    /// entry is returned.
    pub fn include(mut self, pattern: impl AsRef<[u8]>) -> Result<Self, PatternError> {
        let options = traversal_pattern_options(self.match_hidden);
        self.includes
            .push(TraversalPattern::compile(pattern.as_ref(), options)?);
        Ok(self)
    }

    /// Adds an OR-ed exclude pattern. Excluded directories are not descended.
    pub fn exclude(mut self, pattern: impl AsRef<[u8]>) -> Result<Self, PatternError> {
        let options = traversal_pattern_options(self.match_hidden);
        self.excludes
            .push(TraversalPattern::compile(pattern.as_ref(), options)?);
        Ok(self)
    }

    /// Lets an ordinary wildcard cover a leading period, so `**/*.ts` also
    /// reaches `.react-router/routes.ts`. Off by default, per ADR-0011.
    ///
    /// The switch is matcher semantics and applies to include and exclude
    /// patterns alike: what a wildcard may reach, a wildcard may also prune.
    /// It is not [`WalkOptions::skip_hidden`], which drops hidden entries from
    /// the traversal before any pattern sees them; a literal `.cache/**`
    /// selects a hidden path with either setting, because a literal period is
    /// not a wildcard.
    ///
    /// Builder order does not matter: patterns added before this call are
    /// recompiled under the new setting.
    #[must_use]
    pub fn match_hidden(mut self, enabled: bool) -> Self {
        if self.match_hidden == enabled {
            return self;
        }
        self.match_hidden = enabled;
        let options = traversal_pattern_options(enabled);
        for pattern in self.includes.iter_mut().chain(self.excludes.iter_mut()) {
            pattern.recompile(options);
        }
        self
    }

    /// Chooses how far an ordinary wildcard reaches.
    ///
    /// The default, [`WildcardMode::ComponentScoped`], keeps `*`, `?` and
    /// character classes inside one path component, so `*.ts` selects a file in
    /// the walk root and `src/*.ts` one directly inside `src`.
    ///
    /// [`WildcardMode::SeparatorCrossing`] lets them span separators, which is
    /// how `globset` and `fast-glob` read a pattern that was not configured
    /// otherwise. Under it `*.ts` also selects `src/deep/main.ts`. Patterns
    /// carried over from those crates keep their meaning here instead of
    /// quietly selecting less; see the migration note in the compatibility
    /// guide.
    ///
    /// The mode applies to includes and excludes alike, and builder order does
    /// not matter.
    ///
    /// ```
    /// use ferralk::{WildcardMode, Walker};
    ///
    /// let walker = Walker::new(".")
    ///     .wildcard_mode(WildcardMode::SeparatorCrossing)
    ///     .include("*.ts")?;
    /// # Ok::<(), ferralk::ferralk_glob::PatternError>(())
    /// ```
    #[must_use]
    pub const fn wildcard_mode(mut self, mode: WildcardMode) -> Self {
        self.wildcard_mode = mode;
        self
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

    /// Applies `.gitignore` rules plus zlob-compatible `.ignore` supplements.
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

    /// Runs the selected filesystem backend to completion, using the configured workers.
    ///
    /// A panic inside a worker stops the sibling workers and is resumed on the
    /// calling thread after they have been joined.
    pub fn collect(self) -> Result<WalkResult, WalkError> {
        self.collect_with(&SystemBackend)
    }

    /// Runs the walk and asks `visitor` about every entry, on the worker that
    /// produced it.
    ///
    /// This is [`Walker::collect`] with a filter that runs in parallel. A
    /// caller whose predicate is not expressible as a ferralk glob — another
    /// glob engine, a content check, a lookup — otherwise pays a
    /// single-threaded pass over every entry after the walk, which is enough to
    /// cancel out the threads the walk just used.
    ///
    /// The visitor is shared rather than cloned per worker, so it takes `&self`
    /// and must be `Sync`. Per-worker state belongs in a thread-local, which is
    /// how ferralk's own matcher keeps its scratch buffers.
    ///
    /// Cancellation, the error policy, panic propagation and sorting behave
    /// exactly as they do for [`Walker::collect`]; only which entries survive
    /// differs. A [`Verdict::Stop`] ends the walk and is reported by
    /// [`WalkResult::was_cancelled`].
    ///
    /// ```no_run
    /// use ferralk::{Verdict, Walker};
    ///
    /// let result = Walker::new(".")
    ///     .threads(4)
    ///     .visit(|entry| {
    ///         if entry.path().extension().is_some_and(|kind| kind == "rs") {
    ///             Verdict::Keep
    ///         } else {
    ///             Verdict::Skip
    ///         }
    ///     })?;
    /// # Ok::<(), ferralk::WalkError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// The same failures [`Walker::collect`] reports.
    pub fn visit<V>(self, visitor: V) -> Result<WalkResult, WalkError>
    where
        V: Fn(&WalkEntry) -> Verdict + Sync,
    {
        self.walk(&SystemBackend, &visitor)
    }

    /// The collect implementation both frontends share, with the filesystem
    /// behind a backend so tests can drive either of them with a mock.
    fn collect_with<B: DirectoryBackend + Sync>(
        self,
        backend: &B,
    ) -> Result<WalkResult, WalkError> {
        self.walk(backend, &keep_every_entry)
    }

    /// The one traversal `collect` and `visit` share, so a visitor cannot
    /// observe a different tree than a plain collect would.
    fn walk<B: DirectoryBackend + Sync>(
        self,
        backend: &B,
        visitor: EntryVisitor<'_>,
    ) -> Result<WalkResult, WalkError> {
        if self.threads > 1 {
            return parallel::collect(self, backend, visitor);
        }
        let mut state = WalkState::new(&self, visitor);
        // Use the same injector-to-worker transfer as the parallel backend. The
        // serial baseline owns one local queue and deliberately drains it
        // before returning.
        let scheduler = scheduler::Scheduler::new();
        scheduler.push(DirectoryTask {
            path: self.root.clone(),
            depth: 0,
            ignores: IgnoreScope::root(&self, backend),
        });
        let worker = scheduler.worker();
        while let Some(task) = scheduler.steal_into(&worker).or_else(|| worker.pop()) {
            state.walk_directory(backend, task)?;
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
        let ignores = IgnoreScope::root(&self, &SystemBackend);
        WalkStream {
            pending_directories: vec![DirectoryTask {
                path: self.root.clone(),
                depth: 0,
                ignores,
            }],
            walker: self,
            listing: Listing::default(),
            next_entry: 0,
            path: PathBuf::new(),
            visited_directories: HashSet::new(),
            ignores: IgnoreScope::default(),
            depth: 0,
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

    /// Whether the walk may traverse into a directory found at `depth`. The
    /// caller counted the components once and passes the result in.
    fn may_descend_at(&self, depth: usize, bytes: &[u8]) -> bool {
        self.options
            .max_depth
            .is_none_or(|max_depth| depth < max_depth)
            && self.may_descend_into(bytes)
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

/// Gives glob matchers a root-relative path with `/` separators on every
/// platform. Native `Path` values retain their platform representation; only
/// the byte-oriented pattern language is normalized.
pub(crate) fn glob_path_bytes(path: &Path) -> Cow<'_, [u8]> {
    glob_bytes(path.as_os_str().as_encoded_bytes())
}

/// The same normalization for a path the caller already has as bytes.
pub(crate) fn glob_bytes(bytes: &[u8]) -> Cow<'_, [u8]> {
    #[cfg(windows)]
    {
        Cow::Owned(
            bytes
                .iter()
                .map(|&byte| if byte == b'\\' { b'/' } else { byte })
                .collect(),
        )
    }
    #[cfg(not(windows))]
    {
        Cow::Borrowed(bytes)
    }
}

/// The pattern dialect every walker pattern is compiled in. Only
/// `match_hidden` is caller-selectable; the other three are what a filesystem
/// glob means here and are not negotiable per walk.
fn traversal_pattern_options(match_hidden: bool) -> PatternOptions {
    PatternOptions::default()
        .braces(true)
        .recursive_double_star(true)
        .extglob(true)
        .match_hidden(match_hidden)
}

#[derive(Debug, Clone)]
struct TraversalPattern {
    /// The pattern as the caller wrote it, so a later
    /// [`Walker::match_hidden`] can recompile it instead of forcing the caller
    /// to order the builder calls.
    source: Vec<u8>,
    matcher: Pattern,
    directories_only: bool,
    subtree_root: Option<Pattern>,
    /// Literal root of every brace alternative, or `None` when one of them has
    /// none. A prefilter that does not hold for every alternative would prune
    /// something the pattern can still match.
    literal_roots: Option<Vec<Vec<u8>>>,
    /// Literal final extension of every brace alternative, on the same terms.
    extensions: Option<Vec<Vec<u8>>>,
}

impl TraversalPattern {
    fn compile(source: &[u8], options: PatternOptions) -> Result<Self, PatternError> {
        // Walker candidates are always root-relative, so retain zlob glob's
        // conventional leading `./` spelling without making it part of the
        // candidate path representation.
        let pattern = source.strip_prefix(b"./").unwrap_or(source);
        let directories_only = pattern.len() > 1 && pattern.ends_with(b"/");
        let pattern = if directories_only {
            &pattern[..pattern.len() - 1]
        } else {
            pattern
        };
        let subtree_root = pattern
            .strip_suffix(b"/**")
            .map(|root| Pattern::compile(root, options))
            .transpose()?;
        // The matcher expands braces before it compiles; the prefilters are
        // derived from the same expansion, so `**/*.{ts,tsx}` keeps the
        // extension filter and `{src,lib}/**` keeps its roots. A pattern
        // without braces expands to itself, which is the previous behaviour.
        let alternatives = ferralk_glob::expand_braces(pattern, options)?;
        Ok(Self {
            source: source.to_vec(),
            matcher: Pattern::compile(pattern, options)?,
            directories_only,
            subtree_root,
            // The prefilters are literal prefixes and literal extensions, so
            // they are the same under either `match_hidden`: a hidden
            // component below a visible literal root stays reachable, and a
            // hidden literal root stays its own root.
            literal_roots: prefilter_of_every_alternative(&alternatives, literal_pattern_root),
            extensions: prefilter_of_every_alternative(&alternatives, literal_extension),
        })
    }

    /// Recompiles the pattern under changed matcher options.
    ///
    /// `match_hidden` is a matching-time policy - it decides whether a wildcard
    /// may cover a leading period - and never a question of syntax, so a source
    /// that compiled once compiles again.
    fn recompile(&mut self, options: PatternOptions) {
        let source = std::mem::take(&mut self.source);
        *self = Self::compile(&source, options)
            .expect("a compiled pattern stays valid when only match_hidden changes");
    }

    /// Whether the pattern selects this candidate under `mode`.
    ///
    /// The two readings differ only in how far an ordinary wildcard reaches, so
    /// they are the same compiled pattern asked a different question rather
    /// than two compilations.
    fn matches(&self, path: &[u8], is_dir: bool, mode: WildcardMode) -> bool {
        if self.directories_only && !is_dir {
            return false;
        }
        match mode {
            WildcardMode::ComponentScoped => self.matcher.is_match_glob_path(path),
            WildcardMode::SeparatorCrossing => self.matcher.is_match(path),
        }
    }

    /// Whether the exclude covers everything below `path`, so the walk may skip
    /// opening it.
    ///
    /// The subtree root is asked under the same mode as the pattern itself.
    /// Reading it as separator-crossing while the walk scopes wildcards to a
    /// component prunes subtrees the exclude does not cover: `*.tmp/**` would
    /// close `a/b.tmp` even though a component-scoped `*.tmp` cannot match the
    /// component `a`.
    fn covers_subtree(&self, path: &[u8], mode: WildcardMode) -> bool {
        self.subtree_root.as_ref().is_some_and(|root| match mode {
            WildcardMode::ComponentScoped => root.is_match_glob_path(path),
            WildcardMode::SeparatorCrossing => root.is_match(path),
        })
    }

    fn could_match_descendant(&self, path: &[u8]) -> bool {
        let Some(roots) = &self.literal_roots else {
            return true;
        };
        roots
            .iter()
            .any(|root| shares_a_line_of_descent(root, path))
    }

    fn matches_extension(&self, path: &[u8]) -> bool {
        let Some(extensions) = &self.extensions else {
            return true;
        };
        final_extension(path)
            .is_some_and(|candidate| extensions.iter().any(|extension| extension == candidate))
    }
}

/// Whether `path` is the literal root, or one of the two contains the other:
/// only then can something under `path` still match.
fn shares_a_line_of_descent(root: &[u8], path: &[u8]) -> bool {
    root == path
        || root
            .strip_prefix(path)
            .is_some_and(|suffix| suffix.starts_with(b"/"))
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with(b"/"))
}

/// Collects one prefilter value per brace alternative, or `None` as soon as an
/// alternative has none. Pruning on a value that only some alternatives share
/// would drop paths the pattern still matches, so the filter is either complete
/// or off.
fn prefilter_of_every_alternative(
    alternatives: &[Vec<u8>],
    of_alternative: impl Fn(&[u8]) -> Option<Vec<u8>>,
) -> Option<Vec<Vec<u8>>> {
    if alternatives.is_empty() {
        return None;
    }
    let mut values = alternatives
        .iter()
        .map(|alternative| of_alternative(alternative))
        .collect::<Option<Vec<_>>>()?;
    values.sort_unstable();
    values.dedup();
    Some(values)
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
        match prefix.strip_suffix(b"/") {
            // The wildcard starts its own component, so everything before it is
            // a proven prefix.
            Some(complete) => complete,
            // Otherwise the wildcard shares a component with the literal before
            // it, and that literal proves nothing about a directory: `src*`
            // selects inside `srcfoo` as readily as inside `src`. Only what
            // precedes the last separator is proven, and a prefix without one
            // proves nothing at all.
            None => {
                let separator = prefix.iter().rposition(|byte| *byte == b'/')?;
                &prefix[..separator]
            }
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

/// The filesystem calls that traversal and classification make, so one mock
/// can drive the serial and the parallel frontend alike. Reading `.gitignore`
/// files is not part of it: those go through the `ignore` crate, which owns
/// its own IO.
trait DirectoryBackend {
    /// Reads one directory into `listing`, replacing whatever it held.
    ///
    /// The listing is the caller's, and the caller reuses it for every
    /// directory it reads, so a backend that can name an entry without
    /// allocating leaves the walk allocating nothing per entry at all.
    fn read_directory(&self, path: &Path, listing: &mut Listing) -> std::io::Result<()>;

    /// Follows symlinks; decides whether a link points at a directory.
    fn metadata(&self, path: &Path) -> std::io::Result<fs::Metadata> {
        fs::metadata(path)
    }

    /// Does not follow symlinks; fills in the metadata option of an entry.
    fn symlink_metadata(&self, path: &Path) -> std::io::Result<fs::Metadata> {
        fs::symlink_metadata(path)
    }

    /// Resolves a directory for the follow-symlinks loop guard.
    #[cfg(not(unix))]
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        fs::canonicalize(path)
    }

    /// Reads one ignore file. Missing files are the common case and are
    /// reported as an error rather than probed for beforehand.
    fn read_ignore_file(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        fs::read(path)
    }

    /// Identifies a directory for the follow-symlinks loop guard.
    ///
    /// The call follows symlinks, which is what the guard needs: every path
    /// that reaches one directory has to produce one key.
    fn cycle_key(&self, path: &Path) -> std::io::Result<CycleKey> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let metadata = self.metadata(path)?;
            Ok((metadata.dev(), metadata.ino()))
        }
        #[cfg(not(unix))]
        {
            self.canonicalize(path)
        }
    }
}

/// What identifies a directory that the follow-symlinks guard has seen.
///
/// On Unix this is `(st_dev, st_ino)`: sixteen `Copy` bytes from the one
/// `metadata` call the guard already has to make, with no path resolution and
/// no allocation. Two names for one directory, a symlink and the real path
/// among them, share an inode and are therefore recognised as one place --
/// which a resolved path only manages when the resolution agrees.
///
/// Elsewhere the resolved path stays the key. Windows exposes a file index
/// through `std::os::windows::fs::MetadataExt`, but only on the unstable
/// `windows_by_handle` feature, and a walker on stable cannot depend on that.
#[cfg(unix)]
type CycleKey = (u64, u64);
#[cfg(not(unix))]
type CycleKey = PathBuf;

/// Names the call whose failure ends a directory in follow mode, so the
/// reported operation stays the one that actually ran.
#[cfg(unix)]
const CYCLE_KEY_OPERATION: &str = "metadata";
#[cfg(not(unix))]
const CYCLE_KEY_OPERATION: &str = "canonicalize";

/// One directory's entries, held in buffers the walk reuses.
///
/// An entry is its name, not its path. The walk builds a whole path only for
/// the entries it acts on, by pushing the name onto the scratch path it
/// already holds for the directory — where a `PathBuf` per entry copied the
/// parent path as well and allocated for it, once for every entry the walk
/// was about to throw away.
///
/// The name buffers are cleared rather than dropped between directories, so a
/// listing that has been used once names further entries without allocating.
/// It holds no more than the `Vec<PathBuf>` it replaced: a name is a suffix of
/// the path that used to be stored whole.
#[derive(Debug, Default)]
pub(crate) struct Listing {
    entries: Vec<ListedEntry>,
    /// Entries in use. `entries` may be longer: the tail is buffers kept for
    /// the next directory.
    len: usize,
}

/// One entry of a [`Listing`].
#[derive(Debug, Default, Clone)]
pub(crate) struct ListedEntry {
    name: OsString,
    is_dir: bool,
    is_symlink: bool,
}

impl ListedEntry {
    pub(crate) fn name(&self) -> &OsStr {
        &self.name
    }

    pub(crate) const fn is_dir(&self) -> bool {
        self.is_dir
    }

    pub(crate) const fn is_symlink(&self) -> bool {
        self.is_symlink
    }
}

impl Listing {
    /// Drops the previous directory's entries, keeping their buffers.
    pub(crate) fn clear(&mut self) {
        self.len = 0;
    }

    /// Adds one entry, reusing the buffer left by the directory before.
    pub(crate) fn push(&mut self, name: &OsStr, is_dir: bool, is_symlink: bool) {
        if self.len == self.entries.len() {
            self.entries.push(ListedEntry::default());
        }
        let entry = &mut self.entries[self.len];
        entry.name.clear();
        entry.name.push(name);
        entry.is_dir = is_dir;
        entry.is_symlink = is_symlink;
        self.len += 1;
    }

    pub(crate) fn entries(&self) -> &[ListedEntry] {
        &self.entries[..self.len]
    }

    /// Whether the directory holds an entry of this name, which is how the
    /// ignore chain recognizes its own files without probing for them.
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.entries().iter().any(|entry| entry.name == *name)
    }
}

struct StdBackend;

impl DirectoryBackend for StdBackend {
    fn read_directory(&self, path: &Path, listing: &mut Listing) -> std::io::Result<()> {
        listing.clear();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            // `DirEntry` hands its name out by value and nothing else out at
            // all, so this one allocation is the standard library's and is the
            // floor for the portable backend. The native backends read names
            // out of a buffer they own and reach zero.
            listing.push(
                &entry.file_name(),
                file_type.is_dir(),
                file_type.is_symlink(),
            );
        }
        Ok(())
    }
}

/// Selects the feature-gated native backend where it is available and the
/// portable backend everywhere else.
struct SystemBackend;

impl DirectoryBackend for SystemBackend {
    fn read_directory(&self, path: &Path, listing: &mut Listing) -> std::io::Result<()> {
        #[cfg(all(feature = "native-macos", target_os = "macos"))]
        {
            match macos_native::read_directory(path, listing) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                    StdBackend.read_directory(path, listing)
                }
                Err(error) => Err(error),
            }
        }
        #[cfg(all(
            feature = "native-linux",
            target_os = "linux",
            not(all(feature = "native-macos", target_os = "macos"))
        ))]
        {
            match linux_native::read_directory(path, listing) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {
                    StdBackend.read_directory(path, listing)
                }
                Err(error) => Err(error),
            }
        }
        #[cfg(not(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        )))]
        StdBackend.read_directory(path, listing)
    }
}

/// Incremental portable traversal produced by Walker stream.
#[derive(Debug)]
pub struct WalkStream {
    walker: Walker,
    pending_directories: Vec<DirectoryTask>,
    /// The directory being delivered and how far through it the stream is.
    listing: Listing,
    next_entry: usize,
    /// That directory's path, with the entry being classified pushed onto it.
    path: PathBuf,
    visited_directories: HashSet<CycleKey>,
    /// Ignore rules of the directory whose entries are being delivered.
    ignores: IgnoreScope,
    /// Depth of that same directory, so its entries need not recount it.
    depth: usize,
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

    fn prepare_directory(&mut self, task: DirectoryTask) -> Option<Result<WalkEntry, WalkError>> {
        let DirectoryTask {
            path,
            depth,
            ignores,
        } = task;
        if self.walker.options.follow_symlinks {
            match SystemBackend.cycle_key(&path) {
                Ok(key) => {
                    if !self.visited_directories.insert(key) {
                        return None;
                    }
                }
                Err(source) => return self.error(CYCLE_KEY_OPERATION, path, source),
            }
        }
        match SystemBackend.read_directory(&path, &mut self.listing) {
            Ok(()) => {
                // The directory's own ignore files join the chain here, once,
                // recognized in the listing that was just read.
                self.ignores = ignores.enter(&self.walker, &SystemBackend, &path, &self.listing);
                self.depth = depth;
                self.next_entry = 0;
                self.path.clear();
                self.path.push(&path);
                None
            }
            Err(source) => self.error("read_dir", path, source),
        }
    }

    /// Classifies the entry at `index` of the directory being delivered.
    ///
    /// The entry's path is assembled onto the scratch buffer and taken back off
    /// it again, so the stream holds one path buffer rather than one per entry.
    fn process_entry(&mut self, index: usize) -> Option<Result<WalkEntry, WalkError>> {
        self.path.push(self.listing.entries()[index].name());
        let action = classify_entry(
            &self.walker,
            &SystemBackend,
            &self.path,
            &self.listing.entries()[index],
            &self.ignores,
            self.depth,
        );
        // Only an emitted entry needs a path of its own, and the stream hands
        // every one of them to the caller.
        let emitted = match action {
            EntryAction::Skip => None,
            EntryAction::Descend(task) => {
                self.pending_directories.push(task);
                None
            }
            EntryAction::Emit(entry) => Some(Ok(entry.with_path(self.path.clone()))),
            EntryAction::DescendAndEmit(entry, task) => {
                let entry = entry.with_path(self.path.clone());
                self.pending_directories.push(task);
                Some(Ok(entry))
            }
            EntryAction::Failed { failure, descend } => {
                if let Some(task) = descend {
                    self.pending_directories.push(task);
                }
                self.error(failure.operation, failure.path, failure.source)
            }
        };
        self.path.pop();
        emitted
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
            if self.next_entry < self.listing.entries().len() {
                let index = self.next_entry;
                self.next_entry += 1;
                if let Some(result) = self.process_entry(index) {
                    return Some(result);
                }
                continue;
            }
            let task = self.pending_directories.pop()?;
            if let Some(result) = self.prepare_directory(task) {
                return Some(result);
            }
        }
        None
    }
}

struct WalkState<'walker> {
    walker: &'walker Walker,
    visitor: EntryVisitor<'walker>,
    entries: Vec<WalkEntry>,
    errors: Vec<WalkError>,
    visited_directories: HashSet<CycleKey>,
    /// Buffers of directories this frontend has finished with. It descends by
    /// recursion, so several directories are open at once and each needs its
    /// own; a pool hands the deepest frame the buffers the last one returned.
    scratch: Vec<DirectoryScratch>,
    /// The path buffer the last dropped entry left behind. See [`own_path`].
    spare: PathBuf,
    cancelled: bool,
}

/// What reading one directory needs: its listing, and the path buffer its
/// entries are assembled onto.
#[derive(Default)]
struct DirectoryScratch {
    listing: Listing,
    path: PathBuf,
}

/// Copies `path` into a buffer of its own, reusing `spare` when an entry the
/// walk dropped left one behind.
///
/// This is what makes a `Verdict::Skip` free: the visitor needs a `WalkEntry`
/// and a `WalkEntry` owns its path, so one has to be built — but the buffer it
/// is built in can be the one the previous skipped entry gave back, and then
/// no allocator call happens at all.
fn own_path(spare: &mut PathBuf, path: &Path) -> PathBuf {
    let mut owned = std::mem::take(spare);
    if owned.capacity() == 0 {
        // Nothing came back, so this entry buys its own buffer, sized exactly.
        return path.to_path_buf();
    }
    owned.clear();
    owned.as_mut_os_string().push(path.as_os_str());
    owned
}

impl<'walker> WalkState<'walker> {
    fn new(walker: &'walker Walker, visitor: EntryVisitor<'walker>) -> Self {
        Self {
            walker,
            visitor,
            entries: Vec::new(),
            errors: Vec::new(),
            visited_directories: HashSet::new(),
            scratch: Vec::new(),
            spare: PathBuf::new(),
            cancelled: false,
        }
    }

    /// Asks the visitor about one entry and keeps it if it said so.
    ///
    /// A verdict that drops the entry hands its path buffer back, so the next
    /// entry is assembled in it rather than in a fresh allocation.
    fn emit(&mut self, path: &Path, emitted: EmittedEntry) {
        let entry = emitted.with_path(own_path(&mut self.spare, path));
        match (self.visitor)(&entry) {
            Verdict::Keep => self.entries.push(entry),
            Verdict::Skip => self.spare = entry.path,
            Verdict::Stop => {
                self.spare = entry.path;
                self.cancelled = true;
            }
        }
    }

    fn walk_directory(
        &mut self,
        backend: &impl DirectoryBackend,
        task: DirectoryTask,
    ) -> Result<(), WalkError> {
        if self.check_cancellation() {
            return Ok(());
        }
        let DirectoryTask {
            path,
            depth,
            ignores,
        } = task;
        if self.walker.options.follow_symlinks && !self.mark_directory(backend, &path)? {
            return Ok(());
        }
        let mut scratch = self.scratch.pop().unwrap_or_default();
        let outcome = self.walk_listing(backend, &path, depth, ignores, &mut scratch);
        scratch.listing.clear();
        self.scratch.push(scratch);
        outcome
    }

    /// The body of [`WalkState::walk_directory`], with the directory's buffers
    /// held apart so they are returned to the pool however it ends.
    fn walk_listing(
        &mut self,
        backend: &impl DirectoryBackend,
        path: &Path,
        depth: usize,
        ignores: IgnoreScope,
        scratch: &mut DirectoryScratch,
    ) -> Result<(), WalkError> {
        if let Err(source) = backend.read_directory(path, &mut scratch.listing) {
            return self.handle_error("read_dir", path.to_path_buf(), source);
        }
        // The directory's own ignore files join the chain here, once,
        // recognized in the listing that was just read.
        let ignores = ignores.enter(self.walker, backend, path, &scratch.listing);
        scratch.path.clear();
        scratch.path.push(path);
        for index in 0..scratch.listing.entries().len() {
            if self.check_cancellation() {
                return Ok(());
            }
            // The entry's path exists only for as long as it is being decided
            // about; anything that outlives that copies it out.
            scratch.path.push(scratch.listing.entries()[index].name());
            let action = classify_entry(
                self.walker,
                backend,
                &scratch.path,
                &scratch.listing.entries()[index],
                &ignores,
                depth,
            );
            let outcome = self.act(backend, action, &scratch.path);
            scratch.path.pop();
            outcome?;
        }
        Ok(())
    }

    fn mark_directory(
        &mut self,
        backend: &impl DirectoryBackend,
        directory: &Path,
    ) -> Result<bool, WalkError> {
        match backend.cycle_key(directory) {
            Ok(key) => Ok(self.visited_directories.insert(key)),
            Err(source) => {
                self.handle_error(CYCLE_KEY_OPERATION, directory.to_path_buf(), source)?;
                Ok(false)
            }
        }
    }

    /// Carries out what classification decided about one entry.
    ///
    /// `path` is the entry's path, borrowed from the scratch of the directory
    /// being read. A subtree walked from here takes its buffers from the pool,
    /// so it never disturbs that scratch and the path stays valid across the
    /// descent.
    fn act(
        &mut self,
        backend: &impl DirectoryBackend,
        action: EntryAction,
        path: &Path,
    ) -> Result<(), WalkError> {
        match action {
            EntryAction::Skip => Ok(()),
            EntryAction::Descend(task) => self.walk_directory(backend, task),
            EntryAction::Emit(entry) => {
                self.emit(path, entry);
                Ok(())
            }
            // The subtree is walked before the directory itself is recorded,
            // which is the depth-first order this frontend has always had.
            EntryAction::DescendAndEmit(entry, task) => {
                self.walk_directory(backend, task)?;
                self.emit(path, entry);
                Ok(())
            }
            EntryAction::Failed { failure, descend } => {
                self.handle_error(failure.operation, failure.path, failure.source)?;
                if let Some(task) = descend {
                    self.walk_directory(backend, task)?;
                }
                Ok(())
            }
        }
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

fn should_skip_git_directory(walker: &Walker, name: &OsStr) -> bool {
    walker.respect_git_ignore && !walker.options.keep_git_dir && name == ".git"
}

/// Crate version exposed for build and integration diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::{HashMap, HashSet},
        fs,
        path::{Path, PathBuf},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        CancellationToken, ErrorPolicy, TraversalPattern, Verdict, WalkEntry, WalkEntryKind,
        WalkOptions, Walker, WildcardMode, literal_extension, literal_pattern_root,
        traversal_pattern_options,
    };

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    /// Compiles one walker pattern the way `include` and `exclude` do, in the
    /// default dialect where a wildcard does not cover a leading period.
    fn traversal_pattern(pattern: &[u8]) -> TraversalPattern {
        TraversalPattern::compile(pattern, traversal_pattern_options(false))
            .expect("valid walker pattern")
    }

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

    /// What one frontend observed for a walk. Under `Abort` only the operation
    /// that stopped the walk is comparable: which entry loses the race is up to
    /// the traversal order, which the three frontends are free to differ on.
    #[derive(Debug, PartialEq, Eq)]
    enum FrontendOutcome {
        Completed {
            entries: Vec<PathBuf>,
            errors: Vec<(&'static str, PathBuf)>,
        },
        Aborted(&'static str),
    }

    fn error_multiset(errors: &[super::WalkError], root: &Path) -> Vec<(&'static str, PathBuf)> {
        let mut errors = errors
            .iter()
            .map(|error| {
                (
                    error.operation(),
                    error
                        .path()
                        .strip_prefix(root)
                        .unwrap_or(error.path())
                        .to_path_buf(),
                )
            })
            .collect::<Vec<_>>();
        errors.sort_unstable();
        errors
    }

    fn collect_outcome(
        result: Result<super::WalkResult, super::WalkError>,
        root: &Path,
    ) -> FrontendOutcome {
        match result {
            Ok(result) => {
                let mut entries = relative_paths(result.entries(), root);
                entries.sort_unstable();
                FrontendOutcome::Completed {
                    entries,
                    errors: error_multiset(result.errors(), root),
                }
            }
            Err(error) => FrontendOutcome::Aborted(error.operation()),
        }
    }

    fn stream_outcome(
        stream: super::WalkStream,
        root: &Path,
        policy: ErrorPolicy,
    ) -> FrontendOutcome {
        let mut entries = Vec::new();
        let mut errors = Vec::new();
        for item in stream {
            match item {
                Ok(entry) => entries.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("entry is rooted in fixture")
                        .to_path_buf(),
                ),
                Err(error) => {
                    if policy == ErrorPolicy::Abort {
                        return FrontendOutcome::Aborted(error.operation());
                    }
                    errors.push(error);
                }
            }
        }
        entries.sort_unstable();
        FrontendOutcome::Completed {
            entries,
            errors: error_multiset(&errors, root),
        }
    }

    /// Serial `collect`, parallel `collect`, `stream` and `visit` share one
    /// classification pipeline, so they have to report the same entries and the
    /// same errors for the same tree under every policy.
    ///
    /// `visit` is checked twice: keeping everything has to reproduce `collect`
    /// exactly, and filtering in the visitor has to reproduce the same filter
    /// applied to `collect`'s result. The second is the property the API
    /// exists for — a caller moves its predicate into the workers and must get
    /// the same set back.
    fn assert_frontends_agree(label: &str, root: &Path, build: impl Fn() -> Walker) {
        for policy in [ErrorPolicy::Collect, ErrorPolicy::Skip, ErrorPolicy::Abort] {
            let serial = collect_outcome(build().threads(1).error_policy(policy).collect(), root);
            let parallel = collect_outcome(build().threads(4).error_policy(policy).collect(), root);
            let streamed = stream_outcome(build().error_policy(policy).stream(), root, policy);
            assert_eq!(
                parallel, serial,
                "{label}: parallel and serial disagree under {policy:?}"
            );
            assert_eq!(
                streamed, serial,
                "{label}: stream and serial disagree under {policy:?}"
            );

            for threads in [1, 4] {
                let visited = collect_outcome(
                    build()
                        .threads(threads)
                        .error_policy(policy)
                        .visit(|_| Verdict::Keep),
                    root,
                );
                assert_eq!(
                    visited, serial,
                    "{label}: keep-everything visit on {threads} threads disagrees under {policy:?}"
                );

                // An arbitrary predicate that splits the tree, applied in the
                // workers here and to the collected list below.
                let keeps = |entry: &WalkEntry| {
                    entry
                        .path()
                        .to_string_lossy()
                        .bytes()
                        .filter(|byte| *byte == b'a')
                        .count()
                        % 2
                        == 0
                };
                let filtered = collect_outcome(
                    build()
                        .threads(threads)
                        .error_policy(policy)
                        .visit(|entry| {
                            if keeps(entry) {
                                Verdict::Keep
                            } else {
                                Verdict::Skip
                            }
                        }),
                    root,
                );
                let expected = match &serial {
                    FrontendOutcome::Aborted(operation) => FrontendOutcome::Aborted(operation),
                    FrontendOutcome::Completed { errors, .. } => {
                        let mut entries = collect_outcome(
                            build().threads(threads).error_policy(policy).collect(),
                            root,
                        );
                        if let FrontendOutcome::Completed {
                            entries: collected, ..
                        } = &mut entries
                        {
                            collected.retain(|path| keeps_relative(path, root, keeps));
                        }
                        match entries {
                            FrontendOutcome::Completed { entries, .. } => {
                                FrontendOutcome::Completed {
                                    entries,
                                    errors: errors.clone(),
                                }
                            }
                            aborted => aborted,
                        }
                    }
                };
                assert_eq!(
                    filtered, expected,
                    "{label}: filtering visit on {threads} threads disagrees under {policy:?}"
                );
            }
        }
    }

    /// Re-applies a visitor predicate to a path `collect_outcome` made relative.
    fn keeps_relative(relative: &Path, root: &Path, keeps: impl Fn(&WalkEntry) -> bool) -> bool {
        let entry = WalkEntry {
            path: root.join(relative),
            is_dir: false,
            is_symlink: false,
            depth: 0,
            metadata: None,
        };
        keeps(&entry)
    }

    /// Builds the root task the way the walk frontends do, for tests that
    /// drive `WalkState` directly.
    fn directory_task(
        walker: &Walker,
        backend: &impl super::DirectoryBackend,
        path: PathBuf,
    ) -> super::DirectoryTask {
        super::DirectoryTask {
            path,
            depth: 0,
            ignores: super::IgnoreScope::root(walker, backend),
        }
    }

    /// Counts what a walk reads, so tests can pin how often it happens.
    #[derive(Default)]
    struct CountingBackend {
        ignore_reads: std::sync::Mutex<HashMap<PathBuf, usize>>,
    }

    impl CountingBackend {
        fn ignore_reads(&self) -> Vec<(PathBuf, usize)> {
            let mut reads = self
                .ignore_reads
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .map(|(path, count)| (path.clone(), *count))
                .collect::<Vec<_>>();
            reads.sort_unstable();
            reads
        }
    }

    impl super::DirectoryBackend for CountingBackend {
        fn read_directory(&self, path: &Path, listing: &mut super::Listing) -> std::io::Result<()> {
            super::StdBackend.read_directory(path, listing)
        }

        fn read_ignore_file(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            *self
                .ignore_reads
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(path.to_path_buf())
                .or_default() += 1;
            fs::read(path)
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

    fn relative_paths_and_depths(entries: &[WalkEntry], root: &Path) -> Vec<(PathBuf, usize)> {
        entries
            .iter()
            .map(|entry| {
                (
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("entry is rooted in fixture")
                        .to_path_buf(),
                    entry.depth(),
                )
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
    fn source_walk_glob_patterns_filter_recursive_anchored_and_brace_paths() {
        let fixture = Fixture::new();
        fixture.write("src/a.rs");
        fixture.write("src/b.txt");
        fixture.write("src/deep/e.rs");
        fixture.write("lib/c.rs");
        fixture.write("docs/d.md");
        fixture.write("top.rs");
        let options = WalkOptions::default().sort(true);

        let recursive = Walker::new(&fixture.root)
            .include("**/*.rs")
            .expect("valid recursive include")
            .options(options)
            .collect()
            .expect("recursive walk succeeds");
        assert_eq!(
            relative_paths(recursive.entries(), &fixture.root),
            vec![
                PathBuf::from("lib/c.rs"),
                PathBuf::from("src/a.rs"),
                PathBuf::from("src/deep/e.rs"),
                PathBuf::from("top.rs"),
            ]
        );

        let anchored = Walker::new(&fixture.root)
            .include("src/**")
            .expect("valid anchored include")
            .options(options)
            .collect()
            .expect("anchored walk succeeds");
        assert_eq!(
            relative_paths(anchored.entries(), &fixture.root),
            vec![
                PathBuf::from("src"),
                PathBuf::from("src/a.rs"),
                PathBuf::from("src/b.txt"),
                PathBuf::from("src/deep"),
                PathBuf::from("src/deep/e.rs"),
            ]
        );

        let brace = Walker::new(&fixture.root)
            .include("**/*.{md,txt}")
            .expect("valid brace include")
            .options(options)
            .collect()
            .expect("brace walk succeeds");
        assert_eq!(
            relative_paths(brace.entries(), &fixture.root),
            vec![PathBuf::from("docs/d.md"), PathBuf::from("src/b.txt")]
        );
    }

    #[test]
    fn source_rust_glob_patterns_replay_as_walker_filters() {
        // Ported from the root-relative pattern cases in
        // zlob/test/test_rust_glob.zig. The Walker deliberately returns its
        // own entries instead of zlob's C-shaped glob result buffer.
        let fixture = Fixture::new();
        for path in [
            "xyz/x",
            "xyz/y",
            "xyz/z",
            "aaa/tomato/tomato.txt",
            "aaa/tomato/tomoto.txt",
            "bbb/specials/[",
            "bbb/specials/!",
            "bbb/specials/]",
        ] {
            fixture.write(path);
        }
        for path in ["aaa/apple", "aaa/orange"] {
            fs::create_dir_all(fixture.root.join(path)).expect("create source fixture directory");
        }

        let paths_for = |pattern: &str| {
            let result = Walker::new(&fixture.root)
                .threads(1)
                .include(pattern)
                .expect("valid source pattern")
                .options(WalkOptions::default().sort(true))
                .collect()
                .expect("source walk succeeds");
            relative_paths(result.entries(), &fixture.root)
        };

        assert_eq!(paths_for("aaa"), vec![PathBuf::from("aaa")]);
        assert_eq!(paths_for("./aaa"), vec![PathBuf::from("aaa")]);
        assert_eq!(paths_for("aaa/"), vec![PathBuf::from("aaa")]);
        assert!(paths_for("aaa/tomato/tomato.txt/").is_empty());
        assert!(paths_for("nope").is_empty());
        assert_eq!(paths_for("a*"), vec![PathBuf::from("aaa")]);
        assert_eq!(paths_for("@(a*)"), vec![PathBuf::from("aaa")]);
        assert_eq!(paths_for("a*a"), vec![PathBuf::from("aaa")]);
        assert_eq!(paths_for("*a*a*a*"), vec![PathBuf::from("aaa")]);
        assert_eq!(paths_for("aaa/apple"), vec![PathBuf::from("aaa/apple")]);
        assert_eq!(paths_for("./*"), paths_for("*"));
        assert_eq!(
            paths_for("???/"),
            vec![
                PathBuf::from("aaa"),
                PathBuf::from("bbb"),
                PathBuf::from("xyz"),
            ]
        );
        assert_eq!(
            paths_for("xyz/?"),
            vec![
                PathBuf::from("xyz/x"),
                PathBuf::from("xyz/y"),
                PathBuf::from("xyz/z"),
            ]
        );
        assert_eq!(
            paths_for("aaa/tomato/tom?to.txt"),
            vec![
                PathBuf::from("aaa/tomato/tomato.txt"),
                PathBuf::from("aaa/tomato/tomoto.txt"),
            ]
        );
        assert_eq!(
            paths_for("aaa/*"),
            vec![
                PathBuf::from("aaa/apple"),
                PathBuf::from("aaa/orange"),
                PathBuf::from("aaa/tomato"),
            ]
        );
        let component_local = paths_for("aaa/*");
        let parallel = Walker::new(&fixture.root)
            .threads(4)
            .include("aaa/*")
            .expect("valid source pattern")
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("parallel source walk succeeds");
        assert_eq!(
            relative_paths(parallel.entries(), &fixture.root),
            component_local
        );
        let mut streamed = Walker::new(&fixture.root)
            .include("aaa/*")
            .expect("valid source pattern")
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream source walk succeeds");
        streamed.sort_by(|left, right| left.path().cmp(right.path()));
        assert_eq!(relative_paths(&streamed, &fixture.root), component_local);
        let trailing_directory = paths_for("aaa/");
        let trailing_parallel = Walker::new(&fixture.root)
            .threads(4)
            .include("aaa/")
            .expect("valid trailing directory pattern")
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("parallel trailing directory walk succeeds");
        assert_eq!(
            relative_paths(trailing_parallel.entries(), &fixture.root),
            trailing_directory
        );
        let mut trailing_streamed = Walker::new(&fixture.root)
            .include("aaa/")
            .expect("valid trailing directory pattern")
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream trailing directory walk succeeds");
        trailing_streamed.sort_by(|left, right| left.path().cmp(right.path()));
        assert_eq!(
            relative_paths(&trailing_streamed, &fixture.root),
            trailing_directory
        );
        assert_eq!(
            paths_for("*/*/*.txt"),
            vec![
                PathBuf::from("aaa/tomato/tomato.txt"),
                PathBuf::from("aaa/tomato/tomoto.txt"),
            ]
        );
        assert_eq!(paths_for("aa[a]"), vec![PathBuf::from("aaa")]);
        assert_eq!(paths_for("aa[!b]"), vec![PathBuf::from("aaa")]);
        assert!(paths_for("aa[b]").is_empty());
        assert_eq!(
            paths_for("*/*/t[aob]m?to[.]t[!y]t"),
            vec![
                PathBuf::from("aaa/tomato/tomato.txt"),
                PathBuf::from("aaa/tomato/tomoto.txt"),
            ]
        );
        assert_eq!(
            paths_for("bbb/specials/[[]"),
            vec![PathBuf::from("bbb/specials/["),]
        );
        assert_eq!(
            paths_for("bbb/specials/[]]"),
            vec![PathBuf::from("bbb/specials/]"),]
        );
    }

    #[test]
    fn source_walk_entry_depths_are_relative_component_counts() {
        let fixture = Fixture::new();
        fixture.write("a.txt");
        fixture.write("src/b.txt");
        fixture.write("src/sub/c.txt");
        let options = WalkOptions::default().sort(true);
        let expected = vec![
            (PathBuf::from("a.txt"), 1),
            (PathBuf::from("src"), 1),
            (PathBuf::from("src/b.txt"), 2),
            (PathBuf::from("src/sub"), 2),
            (PathBuf::from("src/sub/c.txt"), 3),
        ];

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

        assert_eq!(
            relative_paths_and_depths(serial.entries(), &fixture.root),
            expected
        );
        assert_eq!(
            relative_paths_and_depths(parallel.entries(), &fixture.root),
            expected
        );
        assert_eq!(
            relative_paths_and_depths(&streamed, &fixture.root),
            expected
        );

        let a = serial
            .entries()
            .iter()
            .find(|entry| entry.basename() == Some(std::ffi::OsStr::new("a.txt")))
            .expect("a.txt is present");
        assert!(!a.is_dir());
        assert_eq!(a.kind(), WalkEntryKind::File);
        assert_eq!(a.depth(), 1);

        let src = serial
            .entries()
            .iter()
            .find(|entry| entry.basename() == Some(std::ffi::OsStr::new("src")))
            .expect("src is present");
        assert!(src.is_dir());
        assert_eq!(src.kind(), WalkEntryKind::Directory);
        assert_eq!(src.depth(), 1);

        let c = serial
            .entries()
            .iter()
            .find(|entry| entry.basename() == Some(std::ffi::OsStr::new("c.txt")))
            .expect("c.txt is present");
        assert!(!c.is_dir());
        assert_eq!(c.kind(), WalkEntryKind::File);
        assert_eq!(c.depth(), 3);
    }

    /// Depth is now carried down the tree instead of being recounted from each
    /// entry's path. The two have to agree, so the definition it replaced is
    /// the oracle: the components between the walk root and the entry.
    #[test]
    fn carried_depth_matches_the_component_count_it_replaced() {
        let fixture = Fixture::new();
        fixture.write("a.txt");
        fixture.write("one/b.txt");
        fixture.write("one/two/c.txt");
        fixture.write("one/two/three/d.txt");
        fixture.write("one/two/three/four/e.txt");

        for threads in [1, 4] {
            let walked = Walker::new(&fixture.root)
                .threads(threads)
                .collect()
                .expect("walk succeeds");
            assert!(!walked.entries().is_empty());
            for entry in walked.entries() {
                let counted = entry
                    .path()
                    .strip_prefix(&fixture.root)
                    .expect("entry is rooted in the fixture")
                    .components()
                    .count();
                assert_eq!(
                    entry.depth(),
                    counted,
                    "{} on {threads} thread(s)",
                    entry.path().display()
                );
            }
        }
    }

    /// The root-relative part of a path is taken as a byte suffix at a fixed
    /// offset. Deriving that offset by pushing a name is what makes it survive
    /// a root that already ends in a separator, where adding one would put the
    /// slice one byte out.
    #[test]
    fn a_root_with_a_trailing_separator_walks_like_one_without() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs");
        fixture.write("src/nested/lib.rs");
        let options = WalkOptions::default().sort(true);

        let plain = Walker::new(&fixture.root)
            .threads(1)
            .include("src/**/*.rs")
            .expect("valid include")
            .options(options)
            .collect()
            .expect("walk succeeds");
        // The platform's own separator, so the walked paths are comparable
        // byte for byte. A hardcoded `/` would leave one forward slash in an
        // otherwise backslash path on Windows, and the test would be measuring
        // that rather than the offset it is about.
        let mut trailing_root = fixture.root.clone().into_os_string();
        trailing_root.push(std::path::MAIN_SEPARATOR_STR);
        let trailing = Walker::new(PathBuf::from(trailing_root))
            .threads(1)
            .include("src/**/*.rs")
            .expect("valid include")
            .options(options)
            .collect()
            .expect("walk succeeds");

        let paths = |result: &super::WalkResult| {
            result
                .entries()
                .iter()
                .map(|entry| entry.path().to_path_buf())
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(&trailing), paths(&plain));
        assert_eq!(plain.entries().len(), 2);
    }

    /// The serial frontend descends by recursion while the parent directory is
    /// still being listed. Entries after a subdirectory must therefore still be
    /// built on the parent's path, not on whatever the descent left behind.
    #[test]
    fn entries_after_a_subdirectory_keep_their_own_paths() {
        let fixture = Fixture::new();
        // Several of each, interleaved by name, so the listing order the
        // filesystem happens to return still puts a file after a directory.
        for index in 0..4 {
            fixture.write(format!("outer/dir-{index}/inner.txt"));
            fixture.write(format!("outer/file-{index}.txt"));
        }
        let options = WalkOptions::default().sort(true).files_only(true);

        for threads in [1, 4] {
            let walked = Walker::new(&fixture.root)
                .threads(threads)
                .options(options)
                .collect()
                .expect("walk succeeds");
            let mut relative = relative_paths(walked.entries(), &fixture.root);
            relative.sort();
            let mut expected = (0..4)
                .flat_map(|index| {
                    [
                        PathBuf::from(format!("outer/dir-{index}/inner.txt")),
                        PathBuf::from(format!("outer/file-{index}.txt")),
                    ]
                })
                .collect::<Vec<_>>();
            expected.sort();
            assert_eq!(relative, expected, "on {threads} thread(s)");
        }
    }

    /// One listing buffer serves every directory a worker reads. A directory
    /// with fewer entries than the one before it must report only its own, and
    /// not the tail the longer listing left in the buffer.
    #[test]
    fn a_short_directory_after_a_long_one_reports_only_its_own_entries() {
        let fixture = Fixture::new();
        for index in 0..40 {
            fixture.write(format!("crowded/file-{index:02}.txt"));
        }
        fixture.write("sparse/only.txt");

        // One thread, so both directories go through the same buffer, and the
        // crowded one is read first.
        let walked = Walker::new(&fixture.root)
            .threads(1)
            .include("sparse/**")
            .expect("valid include")
            .options(WalkOptions::default().sort(true).files_only(true))
            .collect()
            .expect("walk succeeds");

        assert_eq!(
            relative_paths(walked.entries(), &fixture.root),
            vec![PathBuf::from("sparse/only.txt")]
        );
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
    fn source_walk_metadata_preserves_portable_and_unix_fields() {
        // Ported from zlob/test/test_walk.zig's metadata scenario. Ferralk
        // exposes std::fs::Metadata directly instead of a validity bitset.
        let fixture = Fixture::new();
        fs::write(fixture.root.join("five.bin"), b"12345").expect("write metadata fixture");

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
        let metadata = with_metadata
            .entries()
            .iter()
            .find(|entry| entry.path().ends_with("five.bin"))
            .expect("fixture file is returned")
            .metadata()
            .expect("metadata is requested");
        assert_eq!(metadata.len(), 5);
        assert!(metadata.is_file());
        assert!(metadata.modified().is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            assert_ne!(metadata.ino(), 0);
            assert_ne!(metadata.mode() & 0o400, 0);
        }
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
        // `build/` is excluded, so Git never reads the ignore file inside it
        // and its negation never applies. The walker does the same.
        assert!(!collected_paths.contains(&PathBuf::from("build/keep.txt")));
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
        assert!(!streamed_paths.contains(&PathBuf::from("build/keep.txt")));
        assert!(!streamed_paths.contains(&PathBuf::from("build")));
    }

    #[test]
    fn source_walk_nested_gitignore_overrides_and_skips_dot_git() {
        // Ported from zlob/test/test_walk.zig's nested Gitignore scenario.
        // Ferralk makes Gitignore application explicit at the Walker boundary.
        let fixture = Fixture::new();
        for path in [
            "build/artifact.o",
            "root.log",
            "keep.txt",
            "sub/important.log",
            "sub/other.log",
            "sub/temp/scratch.txt",
            ".git/config",
        ] {
            fixture.write(path);
        }
        fs::write(fixture.root.join(".gitignore"), b"*.log\nbuild/\n")
            .expect("write root gitignore");
        fs::write(
            fixture.root.join("sub/.gitignore"),
            b"!important.log\ntemp/\n",
        )
        .expect("write nested gitignore");

        let result = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(1)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("Gitignore walk succeeds");
        let paths = relative_paths(result.entries(), &fixture.root);
        for ignored in [
            "build",
            "build/artifact.o",
            "root.log",
            "sub/other.log",
            "sub/temp",
            "sub/temp/scratch.txt",
            ".git",
            ".git/config",
        ] {
            assert!(
                !paths.contains(&PathBuf::from(ignored)),
                "ignored source path {ignored} was returned"
            );
        }
        for kept in ["keep.txt", "sub", "sub/.gitignore", "sub/important.log"] {
            assert!(
                paths.contains(&PathBuf::from(kept)),
                "kept source path {kept} was omitted"
            );
        }

        let parallel = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(4)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("parallel Gitignore walk succeeds");
        assert_eq!(relative_paths(parallel.entries(), &fixture.root), paths);
        let mut streamed = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream Gitignore walk succeeds");
        streamed.sort_by(|left, right| left.path().cmp(right.path()));
        assert_eq!(relative_paths(&streamed, &fixture.root), paths);

        let without_ignore = Walker::new(&fixture.root)
            .threads(1)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("unfiltered source walk succeeds");
        let unfiltered_paths = relative_paths(without_ignore.entries(), &fixture.root);
        assert!(unfiltered_paths.contains(&PathBuf::from(".git/config")));
        assert!(unfiltered_paths.len() > paths.len());
    }

    #[test]
    fn source_walk_ignore_file_overrides_gitignore_rules() {
        // Ported from zlob/test/test_walk.zig's reusable IgnoreRules scenario.
        let fixture = Fixture::new();
        for path in [
            "app.log",
            "keep.log",
            "scratch.tmp",
            "important.tmp",
            "old.bak",
            "src/main.rs",
            "src/old.bak",
            "build/artifact.txt",
        ] {
            fixture.write(path);
        }
        fs::write(
            fixture.root.join(".gitignore"),
            b"*.log\nbuild/\n!keep.log\n*.tmp\n",
        )
        .expect("write root gitignore");
        fs::write(fixture.root.join(".ignore"), b"!important.tmp\n")
            .expect("write root ignore supplement");
        fs::write(fixture.root.join("src/.gitignore"), b"*.bak\n").expect("write nested gitignore");

        let expected = vec![
            PathBuf::from(".gitignore"),
            PathBuf::from(".ignore"),
            PathBuf::from("important.tmp"),
            PathBuf::from("keep.log"),
            PathBuf::from("old.bak"),
            PathBuf::from("src"),
            PathBuf::from("src/.gitignore"),
            PathBuf::from("src/main.rs"),
        ];
        let options = WalkOptions::default().sort(true);
        let serial = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(1)
            .options(options)
            .collect()
            .expect("serial ignore walk succeeds");
        let parallel = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(4)
            .options(options)
            .collect()
            .expect("parallel ignore walk succeeds");
        let mut streamed = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .options(options)
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream ignore walk succeeds");
        streamed.sort_by(|left, right| left.path().cmp(right.path()));

        assert_eq!(relative_paths(serial.entries(), &fixture.root), expected);
        assert_eq!(relative_paths(parallel.entries(), &fixture.root), expected);
        assert_eq!(relative_paths(&streamed, &fixture.root), expected);
    }

    #[test]
    fn source_walk_allowlist_gitignore_descends_into_reincluded_directories() {
        // Ported from zlob/test/test_walk.zig's allowlist Gitignore regression.
        let fixture = Fixture::new();
        for path in [
            "main.rs",
            "Makefile",
            ".keep",
            "src/lib.rs",
            "src/noext",
            "src/deep/a.txt",
            "dir.d/x.md",
            "dir.d/noext",
            "plain/y.txt",
            ".git/config",
        ] {
            fixture.write(path);
        }
        fs::write(
            fixture.root.join(".gitignore"),
            b"# Ignore all\n*\n\n# Unignore all with extensions\n!*.*\n\n# Unignore all dirs\n!/**/\n",
        )
        .expect("write root gitignore");

        let expected = vec![
            PathBuf::from(".gitignore"),
            PathBuf::from(".keep"),
            PathBuf::from("dir.d"),
            PathBuf::from("dir.d/x.md"),
            PathBuf::from("main.rs"),
            PathBuf::from("plain"),
            PathBuf::from("plain/y.txt"),
            PathBuf::from("src"),
            PathBuf::from("src/deep"),
            PathBuf::from("src/deep/a.txt"),
            PathBuf::from("src/lib.rs"),
        ];
        let options = WalkOptions::default().sort(true);
        let serial = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(1)
            .options(options)
            .collect()
            .expect("serial Gitignore walk succeeds");
        let parallel = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(4)
            .options(options)
            .collect()
            .expect("parallel Gitignore walk succeeds");
        let mut streamed = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .options(options)
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream Gitignore walk succeeds");
        streamed.sort_by(|left, right| left.path().cmp(right.path()));

        assert_eq!(relative_paths(serial.entries(), &fixture.root), expected);
        assert_eq!(relative_paths(parallel.entries(), &fixture.root), expected);
        assert_eq!(relative_paths(&streamed, &fixture.root), expected);
    }

    /// The ignore chain travels with the directory tasks, so no directory is
    /// read twice however many workers share the walk.
    #[test]
    fn every_ignore_file_is_read_once_per_walk() {
        let fixture = Fixture::new();
        fixture.write(".gitignore");
        fixture.write("src/.gitignore");
        fixture.write("src/nested/.gitignore");
        for branch in 0..6 {
            fixture.write(format!("src/nested/branch-{branch}/leaf.txt"));
            fixture.write(format!("docs/branch-{branch}/leaf.md"));
        }

        for threads in [1, 4] {
            let backend = CountingBackend::default();
            Walker::new(&fixture.root)
                .threads(threads)
                .respect_git_ignore(true)
                .collect_with(&backend)
                .expect("walk succeeds");

            let repeated = backend
                .ignore_reads()
                .into_iter()
                .filter(|(_, reads)| *reads > 1)
                .collect::<Vec<_>>();
            assert!(
                repeated.is_empty(),
                "with {threads} threads these ignore files were read more than once: {repeated:?}"
            );
            assert!(
                backend
                    .ignore_reads()
                    .iter()
                    .any(|(path, _)| path.ends_with("src/.gitignore")),
                "the walk has to read the nested ignore files through the backend"
            );
        }
    }

    /// The chain is rebuilt per directory now, so a rule at the root still has
    /// to reach an entry several levels down, and a rule closer to the entry
    /// still has to win.
    #[test]
    fn root_rules_reach_deep_entries_and_deeper_rules_win() {
        let fixture = Fixture::new();
        fixture.write("a/b/c/deep.log");
        fixture.write("a/b/other.log");
        fixture.write("a/b/c/keep.txt");
        fs::write(fixture.root.join(".gitignore"), b"*.log\n").expect("write root gitignore");
        fs::write(fixture.root.join("a/b/c/.gitignore"), b"!deep.log\n")
            .expect("write nested gitignore");

        let walked = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");
        let paths = relative_paths(walked.entries(), &fixture.root);

        assert!(
            !paths.contains(&PathBuf::from("a/b/other.log")),
            "a root rule has to reach entries below it"
        );
        assert!(
            paths.contains(&PathBuf::from("a/b/c/deep.log")),
            "the ignore file closest to the entry decides"
        );
        assert!(paths.contains(&PathBuf::from("a/b/c/keep.txt")));
    }

    /// Directory-only rules and rules that span levels have to behave the same
    /// however the entry is reached.
    #[test]
    fn directory_rules_and_spanning_rules_apply_per_directory() {
        let fixture = Fixture::new();
        fixture.write("logs");
        fixture.write("build/main.o");
        fixture.write("a/b/temp/c/note.txt");
        fixture.write("a/b/kept.txt");
        fs::write(
            fixture.root.join(".gitignore"),
            b"logs/\nbuild/\n**/temp/**\n",
        )
        .expect("write root gitignore");

        let walked = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");
        let paths = relative_paths(walked.entries(), &fixture.root);

        assert!(
            paths.contains(&PathBuf::from("logs")),
            "a directory-only rule must not match a file of the same name"
        );
        assert!(!paths.contains(&PathBuf::from("build")));
        assert!(!paths.contains(&PathBuf::from("build/main.o")));
        assert!(!paths.contains(&PathBuf::from("a/b/temp/c/note.txt")));
        assert!(paths.contains(&PathBuf::from("a/b/kept.txt")));
    }

    /// Each frontend now builds the ignore chain along its own descent, so the
    /// three have to agree on a tree that exercises it.
    #[test]
    fn the_three_frontends_agree_on_nested_ignore_files() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs");
        fixture.write("src/debug.log");
        fixture.write("src/keep.log");
        fixture.write("src/nested/deep.log");
        fixture.write("docs/guide.md");
        fixture.write("build/main.o");
        fixture.write("build/keep.txt");
        fs::write(fixture.root.join(".gitignore"), b"*.log\nbuild/\n")
            .expect("write root gitignore");
        fs::write(fixture.root.join("src/.gitignore"), b"!keep.log\n")
            .expect("write nested gitignore");
        fs::write(fixture.root.join("build/.gitignore"), b"!keep.txt\n").expect("write re-include");
        fs::create_dir_all(fixture.root.join(".git/info")).expect("create git directory");
        fs::write(fixture.root.join(".git/info/exclude"), b"*.md\n")
            .expect("write repository excludes");

        assert_frontends_agree("nested ignore files", &fixture.root, || {
            Walker::new(&fixture.root).respect_git_ignore(true)
        });

        let walked = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");
        let paths = relative_paths(walked.entries(), &fixture.root);
        assert!(
            !paths.contains(&PathBuf::from("docs/guide.md")),
            "the repository excludes have to apply"
        );
        assert!(paths.contains(&PathBuf::from("src/keep.log")));
        assert!(!paths.contains(&PathBuf::from("src/nested/deep.log")));
    }

    /// Git-verified ignore cases the walker does not reproduce yet.
    ///
    /// Empty since ADR-0014: the last entry was `ignore-034`, a POSIX class
    /// name in a rule, which the borrowed matcher could not read and the
    /// walker's own rule layer does. A case belongs here only while Git and
    /// the walker are known to disagree, never as a way to quiet a failure.
    const KNOWN_WALKER_GAPS: &[&str] = &[];

    #[test]
    fn git_ignore_corpus_replays_through_the_walker() {
        let corpus_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/ignore.jsonl");
        for line in fs::read_to_string(corpus_path)
            .expect("read ignore corpus")
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            let case: corpus::Case = serde_json::from_str(line).expect("valid ignore corpus case");
            if KNOWN_WALKER_GAPS.contains(&case.id.as_str()) {
                continue;
            }
            let fixture = Fixture::new();
            fs::write(
                fixture.root.join(".gitignore"),
                case.ignore_rules.join("\n").as_bytes(),
            )
            .expect("write fixture gitignore");
            // A case may place further ignore files below the root; Git reads
            // the one closest to the candidate last, and so does the walker.
            for nested in &case.nested_ignore_rules {
                let directory = fixture.root.join(&nested.directory);
                fs::create_dir_all(&directory).expect("create nested ignore directory");
                fs::write(
                    directory.join(".gitignore"),
                    nested.rules.join("\n").as_bytes(),
                )
                .expect("write nested fixture gitignore");
            }
            // Repository-wide excludes live outside the ignore file chain.
            if !case.exclude_rules.is_empty() {
                let info = fixture.root.join(".git/info");
                fs::create_dir_all(&info).expect("create repository info directory");
                fs::write(
                    info.join("exclude"),
                    case.exclude_rules.join("\n").as_bytes(),
                )
                .expect("write repository excludes");
            }
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

    /// Both prefilters are derived from the expanded alternatives, and both go
    /// quiet as soon as one alternative cannot contribute a value.
    #[test]
    fn brace_alternatives_carry_the_planner_prefilters() {
        let sources = traversal_pattern(b"**/*.{ts,tsx}");
        assert!(sources.matches_extension(b"src/app.ts"));
        assert!(sources.matches_extension(b"src/app.tsx"));
        assert!(!sources.matches_extension(b"src/app.js"));
        assert!(!sources.matches_extension(b"src/app"));

        let scoped = traversal_pattern(b"{src,lib}/**/*.ts");
        assert!(scoped.could_match_descendant(b"src"));
        assert!(scoped.could_match_descendant(b"lib"));
        assert!(scoped.could_match_descendant(b"src/nested"));
        assert!(!scoped.could_match_descendant(b"docs"));
        assert!(!scoped.could_match_descendant(b"node_modules"));

        let nested = traversal_pattern(b"{src/{a,b},lib}/**");
        assert!(nested.could_match_descendant(b"src"));
        assert!(nested.could_match_descendant(b"src/a"));
        assert!(!nested.could_match_descendant(b"src/c"));

        // One alternative without a literal root, or without a literal
        // extension, switches the whole prefilter off rather than pruning what
        // that alternative could still match.
        let partial_root = traversal_pattern(b"{src,*}/**/*.ts");
        assert!(partial_root.could_match_descendant(b"docs"));
        let partial_extension = traversal_pattern(b"**/*.{ts,*}");
        assert!(partial_extension.matches_extension(b"src/app.js"));
    }

    /// The planner prefilters are literal prefixes and literal extensions, so
    /// `match_hidden` changes what the matcher accepts without changing what
    /// the planner is allowed to prune.
    #[test]
    fn match_hidden_widens_the_matcher_without_moving_the_planner_prefilters() {
        let hidden = traversal_pattern_options(true);
        let scoped = TraversalPattern::compile(b"site/**/*.ts", hidden).expect("valid include");
        assert!(scoped.could_match_descendant(b"site/.react-router"));
        assert!(scoped.matches(
            b"site/.react-router/routes.ts",
            false,
            WildcardMode::ComponentScoped
        ));
        assert!(scoped.matches_extension(b"site/.react-router/routes.ts"));
        assert!(!scoped.could_match_descendant(b".react-router"));

        // Same pattern, same prefilters, and only the matcher verdict differs.
        let default = traversal_pattern(b"site/**/*.ts");
        assert!(default.could_match_descendant(b"site/.react-router"));
        assert!(!default.matches(
            b"site/.react-router/routes.ts",
            false,
            WildcardMode::ComponentScoped
        ));
        assert_eq!(default.literal_roots, scoped.literal_roots);
        assert_eq!(default.extensions, scoped.extensions);

        // A hidden literal root is its own prefilter under either setting: a
        // literal period is not a wildcard.
        let literal = TraversalPattern::compile(b".claude/**/*.ts", hidden).expect("valid include");
        assert!(literal.could_match_descendant(b".claude"));
        assert!(traversal_pattern(b".claude/**/*.ts").matches(
            b".claude/agents/run.ts",
            false,
            WildcardMode::ComponentScoped
        ));
    }

    /// The option and the patterns are set in either order, so a builder that
    /// enables it last still compiles every pattern under it.
    #[test]
    fn match_hidden_applies_to_patterns_added_before_and_after_it() {
        let fixture = Fixture::new();
        fixture.write(".react-router/types.ts");
        fixture.write("src/app.ts");

        let walk = |walker: Walker| {
            relative_paths(
                walker
                    .options(WalkOptions::default().sort(true))
                    .collect()
                    .expect("walk succeeds")
                    .entries(),
                &fixture.root,
            )
        };
        let before = walk(
            Walker::new(&fixture.root)
                .match_hidden(true)
                .include("**/*.ts")
                .expect("valid include"),
        );
        let after = walk(
            Walker::new(&fixture.root)
                .include("**/*.ts")
                .expect("valid include")
                .match_hidden(true),
        );
        assert_eq!(
            before,
            vec![
                PathBuf::from(".react-router/types.ts"),
                PathBuf::from("src/app.ts"),
            ]
        );
        assert_eq!(after, before);

        // And switching it back off returns the default verdict.
        assert_eq!(
            walk(
                Walker::new(&fixture.root)
                    .match_hidden(true)
                    .include("**/*.ts")
                    .expect("valid include")
                    .match_hidden(false),
            ),
            vec![PathBuf::from("src/app.ts")]
        );
    }

    /// The planner expands before it compiles, so a pattern the expansion
    /// itself rejects has to reach the caller as that rejection.
    #[test]
    fn an_unexpandable_include_is_reported_as_a_pattern_error() {
        let beyond = "{a,b}".repeat(13);
        let error = Walker::new(".")
            .include(&beyond)
            .expect_err("the expansion budget rejects this pattern");
        assert_eq!(error.message(), "too many brace alternatives");
        assert_eq!(
            error.offset(),
            ferralk_glob::Pattern::compile(&beyond, traversal_pattern_options(false))
                .expect_err("the matcher rejects it the same way")
                .offset()
        );
    }

    /// A braced include may return exactly what its alternatives return
    /// together: the prefilters must not narrow that, and must not widen it.
    #[test]
    fn a_brace_include_returns_the_union_of_its_alternatives() {
        let fixture = Fixture::new();
        fixture.write("src/app.ts");
        fixture.write("src/app.tsx");
        fixture.write("src/app.js");
        fixture.write("src/nested/deep.ts");
        fixture.write("lib/util.ts");
        fixture.write("lib/util.rs");
        fixture.write("docs/guide.md");
        fixture.write("docs/nested/notes.md");
        fixture.write("node_modules/pkg/index.ts");

        let walk = |pattern: &str| -> Vec<PathBuf> {
            let result = Walker::new(&fixture.root)
                .include(pattern)
                .expect("valid include")
                .options(WalkOptions::default().sort(true))
                .collect()
                .expect("walk succeeds");
            relative_paths(result.entries(), &fixture.root)
        };

        for pattern in [
            "**/*.{ts,tsx}",
            "{src,lib}/**/*.ts",
            "{src,docs}/**",
            "src/{app,nested}*",
            "{src,lib}/**/*.{ts,rs}",
        ] {
            let alternatives =
                ferralk_glob::expand_braces(pattern, traversal_pattern_options(false))
                    .expect("expandable pattern");
            let mut union = alternatives
                .iter()
                .flat_map(|alternative| {
                    walk(std::str::from_utf8(alternative).expect("ASCII fixture pattern"))
                })
                .collect::<Vec<_>>();
            union.sort_unstable();
            union.dedup();
            assert_eq!(walk(pattern), union, "{pattern}");
        }
    }

    /// The two readings, through the walker rather than the matcher.
    ///
    /// This is the whole point of the option: a pattern carried over from
    /// `globset` selects what it selected there.
    #[test]
    fn the_wildcard_mode_decides_how_far_a_wildcard_reaches() {
        let fixture = Fixture::new();
        fixture.write("main.ts");
        fixture.write("src/app.ts");
        fixture.write("src/deep/nested.ts");
        fixture.write("other/stray.ts");

        let walk = |mode: WildcardMode, pattern: &str| -> Vec<PathBuf> {
            let result = Walker::new(&fixture.root)
                .wildcard_mode(mode)
                .include(pattern)
                .expect("valid include")
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            relative_paths(result.entries(), &fixture.root)
        };

        assert_eq!(
            walk(WildcardMode::ComponentScoped, "*.ts"),
            vec![PathBuf::from("main.ts")],
            "the default keeps a wildcard inside its component"
        );
        assert_eq!(
            walk(WildcardMode::SeparatorCrossing, "*.ts"),
            vec![
                PathBuf::from("main.ts"),
                PathBuf::from("other/stray.ts"),
                PathBuf::from("src/app.ts"),
                PathBuf::from("src/deep/nested.ts"),
            ],
            "crossing reads the pattern the way globset does"
        );

        // A literal prefix still holds under crossing, which is what keeps the
        // planner allowed to prune on it.
        assert_eq!(
            walk(WildcardMode::SeparatorCrossing, "src/*.ts"),
            vec![
                PathBuf::from("src/app.ts"),
                PathBuf::from("src/deep/nested.ts")
            ],
            "crossing reaches below the prefix, never outside it"
        );
        assert_eq!(
            walk(WildcardMode::ComponentScoped, "src/*.ts"),
            vec![PathBuf::from("src/app.ts")]
        );
    }

    /// Excludes are read the same way as includes, so one mode governs the
    /// whole walk rather than half of it.
    #[test]
    fn the_wildcard_mode_governs_excludes_too() {
        let fixture = Fixture::new();
        fixture.write("keep.rs");
        fixture.write("drop.tmp");
        fixture.write("src/drop.tmp");

        let walk = |mode: WildcardMode| -> Vec<PathBuf> {
            let result = Walker::new(&fixture.root)
                .wildcard_mode(mode)
                .exclude("*.tmp")
                .expect("valid exclude")
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            relative_paths(result.entries(), &fixture.root)
        };

        assert_eq!(
            walk(WildcardMode::ComponentScoped),
            vec![PathBuf::from("keep.rs"), PathBuf::from("src/drop.tmp")],
            "a component-scoped exclude only reaches the root component"
        );
        assert_eq!(
            walk(WildcardMode::SeparatorCrossing),
            vec![PathBuf::from("keep.rs")],
            "a crossing exclude reaches every level"
        );
    }

    /// How far a wildcard reaches and whether it may reach a dot-leading name
    /// are separate questions, and the mode answers only the first.
    #[test]
    fn the_wildcard_mode_leaves_the_hidden_policy_alone() {
        let fixture = Fixture::new();
        fixture.write("visible.ts");
        fixture.write(".hidden.ts");
        fixture.write("src/visible.ts");
        fixture.write("src/.hidden.ts");
        fixture.write(".config/inside.ts");

        let walk = |match_hidden: bool| -> Vec<PathBuf> {
            let result = Walker::new(&fixture.root)
                .wildcard_mode(WildcardMode::SeparatorCrossing)
                .match_hidden(match_hidden)
                .include("*.ts")
                .expect("valid include")
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            relative_paths(result.entries(), &fixture.root)
        };

        assert_eq!(
            walk(false),
            vec![PathBuf::from("src/visible.ts"), PathBuf::from("visible.ts")],
            "crossing reaches deeper, not into hidden names"
        );
        assert_eq!(
            walk(true),
            vec![
                PathBuf::from(".config/inside.ts"),
                PathBuf::from(".hidden.ts"),
                PathBuf::from("src/.hidden.ts"),
                PathBuf::from("src/visible.ts"),
                PathBuf::from("visible.ts"),
            ],
            "match_hidden opens hidden names at every level a crossing wildcard reaches"
        );

        // Traversal still decides what the matcher ever sees.
        let skipped = Walker::new(&fixture.root)
            .wildcard_mode(WildcardMode::SeparatorCrossing)
            .match_hidden(true)
            .include("*.ts")
            .expect("valid include")
            .options(
                WalkOptions::default()
                    .sort(true)
                    .files_only(true)
                    .skip_hidden(true),
            )
            .collect()
            .expect("walk succeeds");
        assert_eq!(
            relative_paths(skipped.entries(), &fixture.root),
            vec![PathBuf::from("src/visible.ts"), PathBuf::from("visible.ts")],
            "skip_hidden keeps hidden entries away from the matcher under either mode"
        );
    }

    /// Subtree pruning must decide what the per-entry exclude would have
    /// decided, under either mode.
    ///
    /// `*.tmp/**` used to close `a/b.tmp` in both modes, because the subtree
    /// root was always read as separator-crossing. In the default mode the
    /// exclude does not reach that directory at all - `*.tmp` cannot match the
    /// component `a` - so its contents went missing from the walk without any
    /// pattern saying they should.
    #[test]
    fn subtree_pruning_agrees_with_the_exclude_it_came_from() {
        let fixture = Fixture::new();
        fixture.write("a/b.tmp/keep.rs");
        fixture.write("b.tmp/gone.rs");
        fixture.write("a/plain/keep.rs");

        let walk = |mode: WildcardMode| -> Vec<PathBuf> {
            let result = Walker::new(&fixture.root)
                .wildcard_mode(mode)
                .exclude("*.tmp/**")
                .expect("valid exclude")
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            relative_paths(result.entries(), &fixture.root)
        };

        assert_eq!(
            walk(WildcardMode::ComponentScoped),
            vec![
                PathBuf::from("a/b.tmp/keep.rs"),
                PathBuf::from("a/plain/keep.rs"),
            ],
            "a nested `.tmp` directory is out of a component-scoped exclude's reach"
        );
        assert_eq!(
            walk(WildcardMode::SeparatorCrossing),
            vec![PathBuf::from("a/plain/keep.rs")],
            "a crossing exclude reaches the nested one, and pruning may follow it"
        );
    }

    /// A literal that shares a component with a wildcard proves nothing about a
    /// directory, so the planner may not prune on it.
    ///
    /// `src*` selects inside `srcfoo` as readily as inside `src`; the root
    /// prefilter used to cut at the last separator even when there was none,
    /// keeping the walk out of `srcfoo` entirely. The matcher was always right
    /// about this - corpus case `wildcard-mode-046-scoped` - only the traversal
    /// never asked it.
    #[test]
    fn a_partial_component_literal_does_not_prune_its_siblings() {
        let fixture = Fixture::new();
        fixture.write("src/x.ts");
        fixture.write("srcfoo/x.ts");
        fixture.write("other/x.ts");

        for mode in [
            WildcardMode::ComponentScoped,
            WildcardMode::SeparatorCrossing,
        ] {
            let result = Walker::new(&fixture.root)
                .wildcard_mode(mode)
                .include("src*/x.ts")
                .expect("valid include")
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            assert_eq!(
                relative_paths(result.entries(), &fixture.root),
                vec![PathBuf::from("src/x.ts"), PathBuf::from("srcfoo/x.ts")],
                "{mode:?}: a partial-component literal must not prune a sibling it matches"
            );
        }

        // What is still proven, and still pruned: a literal that ends at a
        // separator.
        assert_eq!(
            literal_pattern_root(b"src/*.ts"),
            Some(b"src".to_vec()),
            "a complete component is still a root"
        );
        assert_eq!(
            literal_pattern_root(b"src*/x.ts"),
            None,
            "a partial component is not"
        );
        assert_eq!(
            literal_pattern_root(b"docs/api*/x.ts"),
            Some(b"docs".to_vec()),
            "what precedes the last separator is still proven"
        );
    }

    #[test]
    fn prune_planner_only_accepts_explicit_whole_subtree_excludes() {
        let scoped = WildcardMode::ComponentScoped;
        let subtree = traversal_pattern(b"src/**");
        assert!(subtree.covers_subtree(b"src", scoped));
        assert!(!subtree.covers_subtree(b"src/nested", scoped));

        let suffix = traversal_pattern(b"*.tmp");
        assert!(!suffix.covers_subtree(b"cache", scoped));

        let nested = traversal_pattern(b"**/target/**");
        assert!(nested.covers_subtree(b"target", scoped));
        assert!(nested.covers_subtree(b"crates/ferralk/target", scoped));

        // The subtree root is read under the walk's own mode. A component-scoped
        // `*.tmp` cannot match the component `a`, so `a/b.tmp` must stay open;
        // a crossing one matches the whole path, so it may be closed.
        let wildcard_subtree = traversal_pattern(b"*.tmp/**");
        assert!(!wildcard_subtree.covers_subtree(b"a/b.tmp", scoped));
        assert!(wildcard_subtree.covers_subtree(b"b.tmp", scoped));
        assert!(wildcard_subtree.covers_subtree(b"a/b.tmp", WildcardMode::SeparatorCrossing));

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

        let rust_sources = traversal_pattern(b"src/**/*.rs");
        assert!(rust_sources.matches_extension(b"src/lib.rs"));
        assert!(!rust_sources.matches_extension(b"src/lib.txt"));
        assert_eq!(literal_extension(b"src/**/*.{rs,ts}"), None);
        assert_eq!(literal_extension(b"src/**/*.rs"), Some(b"rs".to_vec()));
    }

    #[test]
    fn a_visitor_skip_drops_the_entry_without_pruning_the_subtree() {
        // Skip is a result filter, not a traversal filter: excluding a
        // directory from the result must leave its children reachable.
        let fixture = Fixture::new();
        fixture.write("keep/inside.txt");

        for threads in [1, 4] {
            let result = Walker::new(&fixture.root)
                .threads(threads)
                .visit(|entry| {
                    if entry.path().file_name().is_some_and(|name| name == "keep") {
                        Verdict::Skip
                    } else {
                        Verdict::Keep
                    }
                })
                .expect("visited walk succeeds");
            let paths = relative_paths(result.entries(), &fixture.root);
            assert_eq!(
                paths,
                vec![PathBuf::from("keep/inside.txt")],
                "the skipped directory must still have been descended into"
            );
            assert!(!result.was_cancelled());
        }
    }

    #[test]
    fn a_visitor_stop_ends_the_walk_and_is_reported() {
        let fixture = Fixture::new();
        for index in 0..64 {
            fixture.write(format!("file-{index}.txt"));
        }

        for threads in [1, 4] {
            let seen = AtomicUsize::new(0);
            let result = Walker::new(&fixture.root)
                .threads(threads)
                .visit(|_| {
                    if seen.fetch_add(1, Ordering::AcqRel) >= 8 {
                        Verdict::Stop
                    } else {
                        Verdict::Keep
                    }
                })
                .expect("visited walk succeeds");
            assert!(
                result.was_cancelled(),
                "a stop must be reported the way a cancellation is"
            );
            assert!(result.entries().len() <= 64);
        }
    }

    #[test]
    fn a_visitor_stop_leaves_a_caller_owned_cancellation_token_alone() {
        // The token belongs to the caller and may drive other work, so ending
        // one walk must not cancel it.
        let fixture = Fixture::new();
        fixture.write("only.txt");
        let cancellation = CancellationToken::default();

        for threads in [1, 4] {
            let result = Walker::new(&fixture.root)
                .threads(threads)
                .cancellation(cancellation.clone())
                .visit(|_| Verdict::Stop)
                .expect("visited walk succeeds");
            assert!(result.was_cancelled());
            assert!(
                !cancellation.is_cancelled(),
                "a stop must not cancel the caller's token"
            );
        }
    }

    #[test]
    fn a_visitor_panic_is_resumed_on_the_caller() {
        let fixture = Fixture::new();
        for branch in 0..12 {
            fixture.write(format!("branch-{branch}/file.txt"));
        }

        for threads in [1, 4] {
            let root = fixture.root.clone();
            let panicked = std::panic::catch_unwind(move || {
                let _ = Walker::new(&root)
                    .threads(threads)
                    .visit(|_| panic!("visitor panic"));
            });
            assert!(
                panicked.is_err(),
                "a panic inside the visitor must reach the caller on {threads} threads"
            );
        }
    }

    #[test]
    fn a_small_tree_stays_on_one_thread() {
        // The size floor: every parallel arm of the Palamedes trial lost to its
        // own serial form on a twelve-file tree.
        let fixture = Fixture::new();
        for index in 0..12 {
            fixture.write(format!("one/file-{index}.txt"));
        }
        fixture.write("two/only.txt");

        let threads = Mutex::new(HashSet::new());
        let result = Walker::new(&fixture.root)
            .threads(4)
            .visit(|_| {
                threads
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(std::thread::current().id());
                Verdict::Keep
            })
            .expect("visited walk succeeds");

        let observed = threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        assert_eq!(
            observed, 1,
            "a tree below the size floor must not pay for helper threads"
        );
        // Two directories and the thirteen files under them.
        assert_eq!(result.entries().len(), 2 + 13);
    }

    #[test]
    fn the_three_frontends_agree_on_entries_and_errors() {
        let fixture = Fixture::new();
        fixture.write("src/lib.rs");
        fixture.write("src/nested/mod.rs");
        fixture.write("docs/guide.md");
        fixture.write("docs/notes/todo.md");
        fs::create_dir_all(fixture.root.join("empty")).expect("create empty fixture directory");

        assert_frontends_agree("plain tree", &fixture.root, || Walker::new(&fixture.root));
        assert_frontends_agree("with metadata", &fixture.root, || {
            Walker::new(&fixture.root).options(WalkOptions::default().metadata(true))
        });
        assert_frontends_agree("include filtered", &fixture.root, || {
            Walker::new(&fixture.root)
                .options(WalkOptions::default().metadata(true))
                .include("**/*.md")
                .expect("valid include")
        });
        assert_frontends_agree("directories only", &fixture.root, || {
            Walker::new(&fixture.root).options(
                WalkOptions::default()
                    .metadata(true)
                    .directories_only(true)
                    .max_depth(2),
            )
        });
    }

    #[cfg(unix)]
    #[test]
    fn the_three_frontends_agree_when_stat_fails() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("src/lib.rs");
        fixture.write("docs/guide.md");
        symlink("missing-target", fixture.root.join("src/dangling"))
            .expect("create dangling symlink");
        symlink("missing-target", fixture.root.join("docs/dangling"))
            .expect("create second dangling symlink");

        // Following the links stats them, and both stats fail: every frontend
        // has to report that the same way.
        assert_frontends_agree("dangling symlinks", &fixture.root, || {
            Walker::new(&fixture.root).options(
                WalkOptions::default()
                    .metadata(true)
                    .follow_symlinks(true)
                    .sort(true),
            )
        });
    }

    /// An entry whose `stat` would fail but that no filter emits must not be
    /// stat-ed at all, so the walk stays silent about it. The filters used here
    /// are the ones that only decide emission - `directories_only` and an
    /// include pattern without a literal extension - because those are exactly
    /// the ones the serial walk used to apply *after* its stat, which made it
    /// report an error the other two frontends never saw.
    #[cfg(unix)]
    #[test]
    fn a_filtered_entry_is_never_stat_ed() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        fixture.write("keep/note.md");
        fixture.write("blocked/hidden.txt");
        let blocked = fixture.root.join("blocked");
        // Readable but not searchable: the directory still lists, stat-ing what
        // is inside it does not.
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o400))
            .expect("restrict fixture directory");

        // Running as root, or a filesystem that needs a stat to report the file
        // type, would not exercise this at all; leave it rather than fail.
        let listable = fs::read_dir(&blocked).is_ok_and(|entries| {
            entries
                .into_iter()
                .all(|entry| entry.is_ok_and(|entry| entry.file_type().is_ok()))
        });
        let stat_fails = fs::symlink_metadata(blocked.join("hidden.txt")).is_err();
        let outcomes = (listable && stat_fails).then(|| {
            let directories_only = || {
                Walker::new(&fixture.root)
                    .options(WalkOptions::default().metadata(true).directories_only(true))
            };
            let included = || {
                Walker::new(&fixture.root)
                    .options(WalkOptions::default().metadata(true))
                    .include("**/keep/**")
                    .expect("valid include")
            };
            assert_frontends_agree(
                "unreadable file, directories only",
                &fixture.root,
                directories_only,
            );
            assert_frontends_agree("unreadable file, not included", &fixture.root, included);
            (
                collect_outcome(directories_only().threads(1).collect(), &fixture.root),
                collect_outcome(included().threads(1).collect(), &fixture.root),
            )
        });
        // Restore before the fixture removes itself, and before any assertion
        // below can unwind past this point.
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700))
            .expect("restore fixture directory");

        let Some((directories_only, included)) = outcomes else {
            return;
        };
        assert_eq!(
            directories_only,
            FrontendOutcome::Completed {
                entries: vec![PathBuf::from("blocked"), PathBuf::from("keep")],
                errors: Vec::new(),
            },
            "a file dropped by directories_only must not be stat-ed"
        );
        assert_eq!(
            included,
            FrontendOutcome::Completed {
                entries: vec![PathBuf::from("keep"), PathBuf::from("keep/note.md")],
                errors: Vec::new(),
            },
            "an entry dropped by the include patterns must not be stat-ed"
        );
    }

    #[test]
    fn a_mock_backend_drives_the_parallel_walker() {
        struct InjectingBackend {
            failing_stat: PathBuf,
            reads: std::sync::Mutex<Vec<PathBuf>>,
        }

        impl super::DirectoryBackend for InjectingBackend {
            fn read_directory(
                &self,
                path: &Path,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                self.reads
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(path.to_path_buf());
                super::StdBackend.read_directory(path, listing)
            }

            fn symlink_metadata(&self, path: &Path) -> std::io::Result<fs::Metadata> {
                if path == self.failing_stat {
                    return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
                }
                fs::symlink_metadata(path)
            }
        }

        let fixture = Fixture::new();
        fixture.write("src/lib.rs");
        fixture.write("src/nested/mod.rs");
        fixture.write("docs/guide.md");
        let failing_stat = fixture.root.join("src/nested/mod.rs");
        let backend = InjectingBackend {
            failing_stat: failing_stat.clone(),
            reads: std::sync::Mutex::new(Vec::new()),
        };

        let result = Walker::new(&fixture.root)
            .threads(4)
            .options(WalkOptions::default().metadata(true).sort(true))
            .collect_with(&backend)
            .expect("collect policy retains the injected error");

        let reads = backend
            .reads
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            reads.contains(&fixture.root.join("src")),
            "the parallel walker has to read through the injected backend"
        );
        assert_eq!(result.errors().len(), 1);
        assert_eq!(result.errors()[0].operation(), "symlink_metadata");
        assert_eq!(result.errors()[0].path(), failing_stat);
        assert!(
            !result
                .entries()
                .iter()
                .any(|entry| entry.path() == failing_stat)
        );

        // The same injected failure on an entry that is dropped before the
        // stat stays invisible. `directories_only` is the filter to use here:
        // it decides emission alone, so nothing rejects the file earlier.
        let backend = InjectingBackend {
            failing_stat: failing_stat.clone(),
            reads: std::sync::Mutex::new(Vec::new()),
        };
        let filtered = Walker::new(&fixture.root)
            .threads(4)
            .options(
                WalkOptions::default()
                    .metadata(true)
                    .directories_only(true)
                    .sort(true),
            )
            .collect_with(&backend)
            .expect("filtered walk succeeds");
        assert!(
            filtered.errors().is_empty(),
            "a filtered entry must not be stat-ed on the parallel path either"
        );
        assert_eq!(
            relative_paths(filtered.entries(), &fixture.root),
            vec![
                PathBuf::from("docs"),
                PathBuf::from("src"),
                PathBuf::from("src/nested")
            ]
        );
    }

    #[test]
    fn literal_include_roots_prune_unrelated_sibling_directories() {
        /// One directory of the mock tree: entry names and whether each is a
        /// directory.
        type MockDirectory = Vec<(&'static str, bool)>;

        struct RecordingBackend {
            entries: HashMap<PathBuf, MockDirectory>,
            reads: RefCell<Vec<PathBuf>>,
        }

        impl super::DirectoryBackend for RecordingBackend {
            fn read_directory(
                &self,
                path: &Path,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                self.reads.borrow_mut().push(path.to_path_buf());
                listing.clear();
                for &(name, is_dir) in self.entries.get(path).into_iter().flatten() {
                    listing.push(name.as_ref(), is_dir, false);
                }
                Ok(())
            }
        }

        let root = PathBuf::from("/fixture");
        let source = root.join("src");
        let mut entries = HashMap::new();
        entries.insert(root.clone(), vec![("src", true), ("docs", true)]);
        entries.insert(source.clone(), vec![("main.rs", false)]);
        let backend = RecordingBackend {
            entries,
            reads: RefCell::new(Vec::new()),
        };
        let walker = Walker::new(&root)
            .include("src/**/*.rs")
            .expect("valid include");
        let mut state = super::WalkState::new(&walker, &super::keep_every_entry);

        state
            .walk_directory(&backend, directory_task(&walker, &backend, root.clone()))
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
            fn read_directory(
                &self,
                path: &Path,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                listing.clear();
                if path == self.root {
                    let name = self
                        .disappeared
                        .file_name()
                        .expect("the disappearing entry has a name");
                    listing.push(name, false, false);
                }
                Ok(())
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
        let mut state = super::WalkState::new(&walker, &super::keep_every_entry);

        state
            .walk_directory(
                &backend,
                directory_task(&walker, &backend, fixture.root.clone()),
            )
            .expect("collect policy retains the metadata error");

        assert!(state.entries.is_empty());
        assert_eq!(state.errors.len(), 1);
        assert_eq!(state.errors[0].operation(), "symlink_metadata");
        assert_eq!(state.errors[0].path(), disappeared);
    }

    #[cfg(any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ))]
    #[test]
    fn native_backend_matches_portable_across_walker_option_matrix() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("src/lib.rs");
        fixture.write("src/nested/mod.rs");
        fixture.write("src/generated.tmp");
        fixture.write("ignored/skip.rs");
        fixture.write(".hidden/skip.rs");
        for index in 0..192 {
            fixture.write(format!("many/{index:03}-{}", "x".repeat(180)));
        }
        fixture.write(".gitignore");
        fs::write(fixture.root.join(".gitignore"), b"ignored/\n").expect("write ignore rule");
        symlink("src", fixture.root.join("source-link")).expect("create directory symlink");
        symlink("missing-target", fixture.root.join("dangling-link"))
            .expect("create dangling symlink");

        let cases = [
            ("baseline", WalkOptions::default().sort(true)),
            ("metadata", WalkOptions::default().sort(true).metadata(true)),
            (
                "directories_only",
                WalkOptions::default().sort(true).directories_only(true),
            ),
            (
                "files_only",
                WalkOptions::default().sort(true).files_only(true),
            ),
            (
                "skip_hidden",
                WalkOptions::default().sort(true).skip_hidden(true),
            ),
            (
                "follow_symlinks",
                WalkOptions::default().sort(true).follow_symlinks(true),
            ),
            ("max_depth", WalkOptions::default().sort(true).max_depth(1)),
        ];
        for (name, options) in cases {
            let walker = Walker::new(&fixture.root)
                .threads(1)
                .include("**/*")
                .expect("valid include")
                .exclude("**/*.tmp")
                .expect("valid exclude")
                .respect_git_ignore(true)
                .error_policy(ErrorPolicy::Collect)
                .options(options);
            let native = walker.clone().collect().expect("native walk succeeds");
            let (portable_entries, portable_errors) = collect_with_portable_backend(&walker);

            assert_eq!(
                describe_entries(native.entries(), &fixture.root),
                describe_entries(&portable_entries, &fixture.root),
                "native {name} differs from portable"
            );
            assert_eq!(
                describe_errors(native.errors(), &fixture.root),
                describe_errors(&portable_errors, &fixture.root),
                "native {name} errors differ from portable"
            );
            if name == "follow_symlinks" {
                assert_eq!(
                    describe_errors(native.errors(), &fixture.root),
                    vec![(PathBuf::from("dangling-link"), "metadata")]
                );
            } else {
                assert!(native.errors().is_empty(), "native {name} errors");
            }
        }
    }

    #[cfg(any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ))]
    #[test]
    fn native_backend_matches_portable_unreadable_directory_error() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let fixture = Fixture::new();
        if fs::metadata(&fixture.root)
            .expect("fixture root metadata")
            .uid()
            == 0
        {
            return;
        }
        fixture.write("visible.rs");
        fixture.write("locked/secret.rs");
        let locked = fixture.root.join("locked");
        let original_permissions = fs::metadata(&locked)
            .expect("locked directory metadata")
            .permissions();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o0))
            .expect("make locked directory unreadable");

        let walker = Walker::new(&fixture.root)
            .threads(1)
            .error_policy(ErrorPolicy::Collect)
            .options(WalkOptions::default().sort(true));
        let native = walker.clone().collect().expect("native walk succeeds");
        let (portable_entries, portable_errors) = collect_with_portable_backend(&walker);

        fs::set_permissions(&locked, original_permissions)
            .expect("restore locked directory permissions");
        assert_eq!(
            describe_entries(native.entries(), &fixture.root),
            describe_entries(&portable_entries, &fixture.root),
            "native unreadable-directory entries differ from portable"
        );
        assert_eq!(
            describe_errors(native.errors(), &fixture.root),
            describe_errors(&portable_errors, &fixture.root),
            "native unreadable-directory errors differ from portable"
        );
        assert_eq!(
            describe_errors(native.errors(), &fixture.root),
            vec![(PathBuf::from("locked"), "read_dir")]
        );
    }

    #[cfg(any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ))]
    type DescribedEntry = (PathBuf, bool, bool, usize, Option<(u64, bool, bool)>);

    #[cfg(any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ))]
    fn collect_with_portable_backend(walker: &Walker) -> (Vec<WalkEntry>, Vec<super::WalkError>) {
        let mut state = super::WalkState::new(walker, &super::keep_every_entry);
        state
            .walk_directory(
                &super::StdBackend,
                directory_task(walker, &super::StdBackend, walker.root.clone()),
            )
            .expect("portable walk succeeds");
        if walker.options.sort {
            state
                .entries
                .sort_by(|left, right| left.path.cmp(&right.path));
        }
        (state.entries, state.errors)
    }

    #[cfg(any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ))]
    fn describe_entries(entries: &[WalkEntry], root: &Path) -> Vec<DescribedEntry> {
        entries
            .iter()
            .map(|entry| {
                (
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("entry belongs to fixture")
                        .to_path_buf(),
                    entry.is_dir(),
                    entry.is_symlink(),
                    entry.depth(),
                    entry.metadata().map(|metadata| {
                        (
                            metadata.len(),
                            metadata.file_type().is_dir(),
                            metadata.file_type().is_symlink(),
                        )
                    }),
                )
            })
            .collect()
    }

    #[cfg(any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ))]
    fn describe_errors(errors: &[super::WalkError], root: &Path) -> Vec<(PathBuf, &'static str)> {
        errors
            .iter()
            .map(|error| {
                (
                    error
                        .path()
                        .strip_prefix(root)
                        .expect("error belongs to fixture")
                        .to_path_buf(),
                    error.operation(),
                )
            })
            .collect()
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
    fn one_directory_under_several_names_is_entered_once_by_every_frontend() {
        use std::os::unix::fs::symlink;

        // Three names, one directory. The guard keys on what a name reaches,
        // not on the name, so whichever one the walk happens to take first,
        // the other two are recognised as the same place. Each frontend keeps
        // its own visited set, so each is checked.
        let fixture = Fixture::new();
        fixture.write("real/inside.txt");
        symlink("real", fixture.root.join("first")).expect("create first directory symlink");
        symlink("real", fixture.root.join("second")).expect("create second directory symlink");
        let options = WalkOptions::default().follow_symlinks(true).sort(true);

        let count_inside = |paths: Vec<PathBuf>| {
            paths
                .iter()
                .filter(|path| path.file_name().is_some_and(|name| name == "inside.txt"))
                .count()
        };

        for threads in [1, 4] {
            let result = Walker::new(&fixture.root)
                .threads(threads)
                .options(options)
                .collect()
                .expect("walk succeeds");
            assert_eq!(
                count_inside(relative_paths(result.entries(), &fixture.root)),
                1,
                "collect with {threads} thread(s) entered the directory more than once"
            );
        }

        let streamed = Walker::new(&fixture.root)
            .options(options)
            .stream()
            .map(|entry| entry.expect("fixture has no I/O errors"))
            .collect::<Vec<_>>();
        assert_eq!(
            count_inside(relative_paths(&streamed, &fixture.root)),
            1,
            "the stream entered the directory more than once"
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
        let link = without_following
            .entries()
            .iter()
            .find(|entry| entry.basename() == Some(std::ffi::OsStr::new("linked")))
            .expect("symlink is reported");
        assert!(link.is_symlink());
        assert_eq!(link.kind(), WalkEntryKind::Symlink);

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
