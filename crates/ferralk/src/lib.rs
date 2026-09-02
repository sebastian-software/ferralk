#![deny(unsafe_code)]
#![warn(missing_docs)]
#![doc = "Portable filesystem walking."]

//! A safe std::fs walker with a portable `std::fs` backend.
//!
//! Paths stay as PathBuf throughout the public API. Patterns are matched
//! against root-relative encoded path bytes; no filesystem result is converted
//! through UTF-8.

use std::{
    borrow::Cow,
    error::Error,
    ffi::{OsStr, OsString},
    fmt, fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use ferralk_glob::{Pattern, PatternError, PatternOptions, WalkerPathViability};

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
#[cfg(all(
    test,
    any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    )
))]
mod retained_directory_test {
    use std::cell::RefCell;

    #[derive(Clone, Copy, Debug)]
    pub(super) struct Stats {
        pub(super) current: usize,
        pub(super) high_water: usize,
        pub(super) denied: usize,
    }

    #[derive(Debug)]
    struct State {
        limit: usize,
        current: usize,
        high_water: usize,
        denied: usize,
    }

    std::thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
        static DENIAL_HOOK: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    pub(super) struct Guard;

    impl Guard {
        pub(super) fn new(limit: usize) -> Self {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                assert!(state.is_none(), "retention test override is already active");
                *state = Some(State {
                    limit,
                    current: 0,
                    high_water: 0,
                    denied: 0,
                });
            });
            Self
        }

        pub(super) fn stats(&self) -> Stats {
            STATE.with(|state| {
                let state = state.borrow();
                let state = state.as_ref().expect("retention test override is active");
                Stats {
                    current: state.current,
                    high_water: state.high_water,
                    denied: state.denied,
                }
            })
        }

        pub(super) fn set_limit(&self, limit: usize) {
            STATE.with(|state| {
                state
                    .borrow_mut()
                    .as_mut()
                    .expect("retention test override is active")
                    .limit = limit;
            });
        }

        pub(super) fn on_next_denial(&self, hook: impl FnOnce() + 'static) {
            DENIAL_HOOK.with(|slot| {
                let mut slot = slot.borrow_mut();
                assert!(slot.is_none(), "retention denial hook is already armed");
                *slot = Some(Box::new(hook));
            });
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            STATE.with(|state| *state.borrow_mut() = None);
            DENIAL_HOOK.with(|hook| *hook.borrow_mut() = None);
        }
    }

    /// Returns `None` without an active override, or whether the test budget
    /// granted a permit when one is active.
    pub(super) fn try_acquire() -> Option<bool> {
        let granted = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let state = state.as_mut()?;
            if state.current >= state.limit {
                state.denied += 1;
                return Some(false);
            }
            state.current += 1;
            state.high_water = state.high_water.max(state.current);
            Some(true)
        });
        if granted == Some(false) {
            let hook = DENIAL_HOOK.with(|hook| hook.borrow_mut().take());
            if let Some(hook) = hook {
                hook();
            }
        }
        granted
    }

    pub(super) fn release() {
        STATE.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                debug_assert!(state.current > 0);
                state.current -= 1;
            }
        });
    }
}
#[cfg(all(feature = "native-linux", target_os = "linux"))]
#[doc(hidden)]
pub use linux_native::fuzz_validate_records as fuzz_validate_linux_dirent_records;
#[cfg(all(feature = "native-macos", target_os = "macos"))]
#[doc(hidden)]
pub use macos_native::fuzz_validate_bulk_record as fuzz_validate_macos_bulk_record;
#[cfg(all(feature = "native-macos", target_os = "macos"))]
#[doc(hidden)]
pub use macos_native::fuzz_validate_records as fuzz_validate_macos_dirent_records;
mod absolute;

/// Walker-pattern entry point for the corpus harness, exported the way the
/// fuzz entry points are: for `tools/`, not for consumers.
///
/// Returns the pattern after the walker's absolute-root rewrite, or `None`
/// when the pattern names paths outside `root` and so can select nothing. The
/// walker additionally compiles and validates this spelling before using it.
/// `windows_paths`
/// chooses which spelling of a path the rules read instead of the host's, so
/// one corpus case describes one rule on every platform - including the rules
/// that only apply to Windows, which would otherwise be recorded where nothing
/// replays them.
#[doc(hidden)]
pub fn corpus_rewrite_absolute_pattern(
    pattern: &[u8],
    root: &[u8],
    windows_paths: bool,
) -> Result<Option<Vec<u8>>, PatternError> {
    let syntax = if windows_paths {
        absolute::Syntax::Windows
    } else {
        absolute::Syntax::Posix
    };
    rewrite_pattern_for_root(pattern, root, syntax)
}
mod classify;
mod gitignore;
mod ignore_rules;

/// Fuzz entry point for the gitignore rule layer (ADR-0014), exported the way
/// the native dirent parsers are: for the harness in `fuzz/`, not for consumers.
#[doc(hidden)]
pub use ignore_rules::fuzz_rule as fuzz_ignore_rule;
#[doc(hidden)]
pub use ignore_rules::fuzz_rule_bytes as fuzz_ignore_rule_bytes;
mod parallel;
mod scheduler;

use classify::{DirectoryTask, EmittedEntry, EntryAction, TraversalContext, classify_entry};
use gitignore::{IgnoreReadError, IgnoreScope};

/// Controls what a walk does after a recoverable filesystem error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorPolicy {
    /// Stop immediately and return the first error.
    Abort,
    /// Continue walking and do not retain recoverable errors discovered below
    /// a root. A caller-supplied root that cannot be opened is still retained
    /// (or yielded by [`Walker::stream`]), so it cannot look like an empty
    /// tree.
    Skip,
    /// Continue walking and return accumulated recoverable errors.
    #[default]
    Collect,
}

/// Cloneable, caller-controlled cooperative cancellation handle for a walk.
///
/// A walker only observes this handle; internal aborts, worker startup
/// failures, visitor stops, and panics never cancel it. This lets callers
/// share one token across walks or reuse it after an individual walk fails.
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
    resolve_symlink_kind: bool,
    skip_hidden: bool,
    keep_git_dir: bool,
    max_depth: Option<usize>,
}

impl WalkOptions {
    /// Follows directory symlinks discovered below a walk root while retaining
    /// an ancestor-chain cycle guard.
    ///
    /// A root supplied to [`Walker::new`] or [`Walker::add_root`] is always
    /// opened as a directory, so a root that is a symlink to a directory is
    /// traversed even when this option is `false`. This option controls only
    /// symlink entries discovered while walking that root.
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
    ///
    /// A symlink is reported by its own kind, so a link pointing at a directory
    /// is *not* a directory here unless the walk resolves it - by following
    /// symlinks, or by [`WalkOptions::resolve_symlink_kind`].
    #[must_use]
    pub const fn directories_only(mut self, enabled: bool) -> Self {
        self.directories_only = enabled;
        self
    }

    /// Returns only files while continuing to traverse through directories.
    ///
    /// A symlink is reported by its own kind, so every symlink counts as a file
    /// here - including a broken one and one pointing at a directory - unless
    /// the walk resolves it. [`WalkOptions::resolve_symlink_kind`] is what makes
    /// this filter agree with `Path::is_file`.
    #[must_use]
    pub const fn files_only(mut self, enabled: bool) -> Self {
        self.files_only = enabled;
        self
    }

    /// Classifies symlink entries by what they point at, for the
    /// [`files_only`](WalkOptions::files_only) and
    /// [`directories_only`](WalkOptions::directories_only) filters.
    ///
    /// A directory listing reports a symlink as a symlink and says nothing
    /// about its target, so without this the kind filters see every symlink as
    /// a non-directory: `files_only` keeps broken links and links to
    /// directories, and `directories_only` drops links to directories. Callers
    /// who mean `Path::is_file` - which follows the link - get neither.
    ///
    /// With this enabled, one `metadata` call per symlink entry decides:
    ///
    /// | the link points at | `files_only` | `directories_only` |
    /// |---|---|---|
    /// | a file | kept | dropped |
    /// | a directory | dropped | kept |
    /// | nothing (broken) | dropped | dropped |
    ///
    /// A broken link is an answer rather than a failure - there is no target,
    /// so the entry is neither a file nor a directory - and is dropped without
    /// an error. A `metadata` call that fails for any other reason leaves the
    /// kind unknown, and that *is* reported through the configured
    /// [`ErrorPolicy`], with the entry dropped either way.
    ///
    /// The stat is paid only for symlink entries, only when one of the two kind
    /// filters is on, and only when the walk is not already following symlinks -
    /// following resolves the same question on its own. It does not change what
    /// [`WalkEntry::kind`] reports, which stays what the listing observed, nor
    /// which entries a pattern matches, nor where the walk descends: this
    /// switch answers "what is this", while
    /// [`follow_symlinks`](WalkOptions::follow_symlinks) answers "does the walk
    /// go through it".
    ///
    /// Off by default, because turning it on changes which entries a walk
    /// returns.
    #[must_use]
    pub const fn resolve_symlink_kind(mut self, enabled: bool) -> Self {
        self.resolve_symlink_kind = enabled;
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
///
/// Entries are [`Clone`], so callers can retain an entry in more than one
/// collection. Cloning preserves the captured path, walk-root identity, entry
/// flags, depth, and optional metadata snapshot.
#[derive(Debug, Clone)]
pub struct WalkEntry {
    path: PathBuf,
    /// The walk root this entry was found under. Shared per root rather than
    /// copied per entry.
    root: Arc<Path>,
    is_dir: bool,
    is_symlink: bool,
    depth: usize,
    /// Boxed rather than inline: `fs::Metadata` is the platform `stat` struct,
    /// around 144 bytes on Unix, and carrying it in the entry made every
    /// `WalkEntry` that size whether or not
    /// [`WalkOptions::metadata`](WalkOptions::metadata) was ever asked for. A
    /// walk collects millions of these into one `Vec`, and the default walk
    /// leaves the option off. The box costs one allocation per entry on the
    /// walks that do ask, which already paid a `stat` syscall for it.
    metadata: Option<Box<fs::Metadata>>,
}

impl WalkEntry {
    /// Absolute or caller-relative path preserved from the walker.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Encoded bytes of [`Self::path`], ready for byte-first matchers.
    ///
    /// This is exactly [`OsStr::as_encoded_bytes`] on the path's native
    /// representation: raw filesystem bytes on Unix and lossless WTF-8 on
    /// Windows. It performs no allocation or Unicode conversion, so it is the
    /// bridge to [`ferralk_glob::Pattern`] when a visitor needs to match the
    /// entry itself.
    ///
    /// ```no_run
    /// use ferralk::{Verdict, Walker, ferralk_glob::{Pattern, PatternOptions}};
    ///
    /// let matcher = Pattern::compile("**/*.rs", PatternOptions::default().recursive_double_star(true))?;
    /// let result = Walker::new("src").visit(|entry| {
    ///     matcher.is_match(entry.path_bytes()).then_some(Verdict::Keep).unwrap_or(Verdict::Skip)
    /// })?;
    /// # let _ = result;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn path_bytes(&self) -> &[u8] {
        self.path.as_os_str().as_encoded_bytes()
    }

    /// The walk root this entry was found under, exactly as it was given to
    /// [`Walker::new`] or [`Walker::add_root`].
    ///
    /// A single-root walk answers with that one root, so the accessor means the
    /// same thing however many roots there are. It exists because a path alone
    /// does not settle the question once roots may contain one another: an
    /// entry under both `/a` and `/a/b` is produced once per root, and only the
    /// root it came from distinguishes the two. [`WalkEntry::depth`] is counted
    /// from this root as well.
    ///
    /// ```no_run
    /// # use ferralk::Walker;
    /// let result = Walker::new("crates").add_root("tools")?.collect()?;
    /// for entry in result.entries() {
    ///     let inside = entry.path().strip_prefix(entry.root()).expect("under its root");
    ///     println!("{} in {}", inside.display(), entry.root().display());
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
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
        self.metadata.as_deref()
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

/// One root of a walk, with the patterns as they read for it.
///
/// A relative pattern means the same thing under every root, but an absolute
/// one does not: `/repo/src/**` selects everything under a root of `/repo` and
/// nothing at all under a root of `/other`. Compiling per root is what lets one
/// pattern list serve several roots, and it is why the rewrite in
/// [`crate::absolute`] takes the root as an argument.
#[derive(Debug, Clone)]
struct RootPlan {
    path: PathBuf,
    /// Shared with every entry produced under this root, so an entry can name
    /// its root without each one owning a copy of the path.
    shared_path: Arc<Path>,
    /// Byte index at which the root-relative part of any path built under this
    /// root begins. See [`RootPlan::relative_start`].
    relative_start: usize,
    includes: Vec<TraversalPattern>,
    excludes: Vec<TraversalPattern>,
}

/// Builder for a portable serial traversal.
#[derive(Debug, Clone)]
pub struct Walker {
    /// Never empty: [`Walker::new`] establishes the first one.
    roots: Vec<RootPlan>,
    /// Include patterns as the caller wrote them, kept so that a root added
    /// later can be given its own reading of each one.
    include_sources: Vec<Vec<u8>>,
    /// Exclude patterns, on the same terms.
    exclude_sources: Vec<Vec<u8>>,
    match_hidden: bool,
    options: WalkOptions,
    error_policy: ErrorPolicy,
    cancellation: Option<CancellationToken>,
    respect_git_ignore: bool,
    /// Explicit effective Git settings. `None` reads supported
    /// repository-local config; `Some` wins over it.
    git_ignore_case: Option<bool>,
    git_precompose_unicode: Option<bool>,
    wildcard_mode: WildcardMode,
    threads: usize,
}

/// Hard ceiling for eagerly allocated worker slots. The scheduler still
/// starts helpers lazily, but every configured slot owns queues and scratch
/// buffers as soon as a walk widens.
const MAX_WORKERS: usize = 256;

impl RootPlan {
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

    /// A root with no patterns compiled for it yet.
    fn new(path: PathBuf) -> Self {
        Self {
            relative_start: Self::relative_start(&path),
            shared_path: Arc::from(path.as_path()),
            path,
            includes: Vec::new(),
            excludes: Vec::new(),
        }
    }
}

impl Walker {
    /// Starts a walk rooted at root.
    ///
    /// Further roots may be added with [`Walker::add_root`] or
    /// [`Walker::try_add_root`]. `root` must name
    /// a readable directory: a plain file produces a `read_dir` not-a-directory
    /// error and no entry for the file. A directory symlink supplied as the
    /// root is traversed regardless of [`WalkOptions::follow_symlinks`], which
    /// only controls symlinks discovered below the root.
    /// A relative root is resolved against the process's current directory
    /// when [`Walker::collect`], [`Walker::visit`] or [`Walker::stream`] starts;
    /// changing that directory after construction therefore changes which
    /// directory the walk opens.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            roots: vec![RootPlan::new(root.into())],
            include_sources: Vec::new(),
            exclude_sources: Vec::new(),
            match_hidden: false,
            options: WalkOptions::default(),
            error_policy: ErrorPolicy::default(),
            cancellation: None,
            respect_git_ignore: false,
            git_ignore_case: None,
            git_precompose_unicode: None,
            wildcard_mode: WildcardMode::default(),
            threads: std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(1)
                .min(MAX_WORKERS),
        }
    }

    /// Adds an OR-ed include pattern and returns the builder for consuming
    /// chains. No includes means every non-excluded entry is returned.
    ///
    /// The pattern may be absolute. See [`Walker::exclude`] for what that
    /// means and when it is rejected.
    ///
    /// ```
    /// use ferralk::Walker;
    ///
    /// // These select the same entries. The absolute spelling follows the
    /// // host's path syntax: `/repo` on Unix and `C:/repo` on Windows.
    /// let (root, absolute) = if cfg!(windows) {
    ///     ("C:/repo", "C:/repo/src/**/*.ts")
    /// } else {
    ///     ("/repo", "/repo/src/**/*.ts")
    /// };
    /// let written_relative = Walker::new(root).include("src/**/*.ts")?;
    /// let held_absolute = Walker::new(root).include(absolute)?;
    /// # Ok::<(), ferralk::ferralk_glob::PatternError>(())
    /// ```
    ///
    /// For a caller-supplied list that may contain invalid patterns, use
    /// [`Walker::try_include`] instead. It borrows the builder, so rejecting a
    /// pattern leaves the caller's configured walker available for the next
    /// pattern.
    pub fn include(mut self, pattern: impl AsRef<[u8]>) -> Result<Self, PatternError> {
        self.try_include(pattern)?;
        Ok(self)
    }

    /// Adds an OR-ed include pattern without consuming the builder.
    ///
    /// This is the borrowed counterpart to [`Walker::include`]. It is useful
    /// when applying a user-supplied pattern list: an invalid pattern returns
    /// its [`PatternError`] and leaves this walker unchanged, so later entries
    /// can still be considered. A valid pattern has the same semantics as
    /// [`Walker::include`] and composes with every root and matcher mode
    /// already configured on this walker.
    ///
    /// ```no_run
    /// use ferralk::Walker;
    ///
    /// let mut walker = Walker::new("workspace");
    /// for pattern in ["src/**/*.rs", "[a", "tests/**/*.rs"] {
    ///     if let Err(error) = walker.try_include(pattern) {
    ///         eprintln!("skipping {pattern:?}: {error}");
    ///     }
    /// }
    ///
    /// // The two valid patterns remain configured despite the rejected `[a`.
    /// let result = walker.collect()?;
    /// # let _ = result;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn try_include(&mut self, pattern: impl AsRef<[u8]>) -> Result<&mut Self, PatternError> {
        let pattern = pattern.as_ref();
        // Compiled for every root before any of them is changed, so a pattern
        // one root rejects leaves the walker as it was rather than half updated.
        let compiled = self.compile_for_every_root(pattern)?;
        for (root, pattern) in self.roots.iter_mut().zip(compiled) {
            root.includes.push(pattern);
        }
        self.include_sources.push(pattern.to_vec());
        Ok(self)
    }

    /// Adds an OR-ed exclude pattern. A matching directory is not emitted and
    /// is pruned only when no include can select a descendant.
    ///
    /// # Absolute patterns
    ///
    /// A pattern that starts at a filesystem root is understood as naming
    /// absolute paths, and the walk root is removed from it so that it selects
    /// the same entries a caller would have written by hand. This is detected
    /// rather than requested, because a caller holding a mixed list of patterns
    /// would otherwise have to sort them itself - which is the arithmetic this
    /// exists to remove. What counts as absolute is the platform's own rule:
    /// a leading `/` on Unix, a drive letter or a UNC share on Windows, where a
    /// single leading separator is drive-relative and so is compiled as a
    /// walker pattern rather than treated as absolute.
    ///
    /// A pattern that names paths outside the walk root selects nothing, which
    /// is what it would have selected had it been matched against absolute
    /// paths. It is not an error: a caller filtering one list across several
    /// roots expects the patterns meant for other roots to fall away here.
    ///
    /// Three shapes are rejected with a [`PatternError`], because guessing at
    /// them would silently select the wrong entries:
    ///
    /// - a wildcard at or above the walk root (`/*/x.ts`, `/**/*.ts`, or
    ///   `/repo*/x.ts` for a root of `/repo`), which may or may not cover the
    ///   root and cannot be decided without matching. Write the part below the
    ///   root instead: `**/*.ts` selects everything under it.
    /// - a `..` component, which is not resolved here. Folding it away
    ///   lexically is wrong across a symlink, and resolving it properly would
    ///   mean touching the filesystem to compile a pattern.
    /// - a pattern naming the walk root itself, which selects nothing because
    ///   the walk emits what is inside the root. Add `/**`.
    ///
    /// Separators, repeated or trailing, and `.` components are ignored on
    /// both sides, so `/repo//src/*.ts` against a root of `/repo/` rewrites the
    /// same as the tidy spelling.
    ///
    /// # Patterns are written with `/`
    ///
    /// On every platform, per ADR-0005: `\` is the escape character, not a
    /// separator, so a pattern built by joining `PathBuf`s on Windows asks for
    /// each separator's next byte literally and matches nothing. On Windows
    /// such a pattern is rejected rather than left to fail silently, but only
    /// where the plain text of the pattern demands a byte Windows forbids in a
    /// name - `C:\repo\**`, `src\*.ts`, `\\server\share`. Escaping an
    /// ordinary byte is legal, so `a\b\c` selects a file named `abc`; and
    /// inside a group the escape is one member among several, so `[a\*]` and
    /// `{a,\*}` still select `a`. Build the pattern as a pattern and let
    /// [`Walker::new`] hold the path.
    ///
    /// ```
    /// use ferralk::Walker;
    ///
    /// // Both of these are about Unix spelling, where a leading `/` is what
    /// // makes a path absolute.
    /// if cfg!(unix) {
    ///     // Selects nothing: the pattern is about a different tree.
    ///     let elsewhere = Walker::new("/repo").exclude("/other/**").unwrap();
    ///
    ///     // Rejected: the wildcard sits above the root.
    ///     assert!(Walker::new("/repo").exclude("/**/*.tmp").is_err());
    /// }
    /// ```
    ///
    /// For a caller-supplied list that may contain invalid patterns, use
    /// [`Walker::try_exclude`] instead. It preserves the configured builder
    /// when a pattern is rejected.
    pub fn exclude(mut self, pattern: impl AsRef<[u8]>) -> Result<Self, PatternError> {
        self.try_exclude(pattern)?;
        Ok(self)
    }

    /// Adds an OR-ed exclude pattern without consuming the builder.
    ///
    /// This is the borrowed counterpart to [`Walker::exclude`]. It has the
    /// same all-or-nothing compilation rule as [`Walker::try_include`]: a
    /// rejected pattern leaves every root and previously configured filter
    /// unchanged, while a valid one composes with them.
    pub fn try_exclude(&mut self, pattern: impl AsRef<[u8]>) -> Result<&mut Self, PatternError> {
        let pattern = pattern.as_ref();
        let compiled = self.compile_for_every_root(pattern)?;
        for (root, pattern) in self.roots.iter_mut().zip(compiled) {
            root.excludes.push(pattern);
        }
        self.exclude_sources.push(pattern.to_vec());
        Ok(self)
    }

    /// Adds another root to the same walk.
    ///
    /// One walker, one thread pool, several trees: the roots become the walk's
    /// initial directories and share the scheduler and helper-spawn floor. Each
    /// root starts with its own ancestor-chain guard, so following symlinks still
    /// preserves the same concatenation semantics as separate walks. A caller
    /// with several source trees no longer pays pool startup per tree.
    ///
    /// # Semantics
    ///
    /// - **Patterns are per root.** Include and exclude patterns stay
    ///   root-relative and are applied under every root, so `src/**/*.ts`
    ///   selects that subtree of each. An absolute pattern is rewritten for
    ///   each root separately, which means a pattern naming one root's tree
    ///   selects nothing under the others - the reading
    ///   [`Walker::exclude`] describes, and the reason it treats an
    ///   out-of-root pattern as a verdict rather than an error.
    /// - **`depth` and [`WalkEntry::root`] are relative to the root the entry
    ///   came from**, so a walk of several trees says the same thing about each
    ///   entry that a walk of that one tree would.
    /// - **Overlapping roots deliver their overlap more than once.** Adding
    ///   `/a` and `/a/b` yields everything under `/a/b` twice, because a
    ///   multi-root walk is defined as the concatenation of the single-root
    ///   walks. Suppressing that would need the identity of every directory, a
    ///   `stat` per directory that only the symlink-following mode pays today,
    ///   and would make adding a root able to remove entries. A caller who
    ///   wants each path once passes roots that do not contain one another.
    /// - **A root that cannot be read is an ordinary walk error** for that
    ///   root's path, and the other roots are still walked, subject to
    ///   [`ErrorPolicy`]. Even [`ErrorPolicy::Skip`] reports that root error,
    ///   because a caller-supplied root must not be indistinguishable from an
    ///   empty tree.
    ///
    /// The order roots are visited in is not part of the contract, any more
    /// than the order of entries within one root is: the scheduler hands the
    /// initial tasks out like any others. `WalkOptions::sort(true)` is what
    /// orders a result.
    ///
    /// # Errors
    ///
    /// Returns the error an already-added absolute pattern produces when it is
    /// rewritten for this root - the same rejections [`Walker::exclude`] lists.
    /// Builder order does not matter: adding the root first and the pattern
    /// second reports the same error from [`Walker::include`] instead.
    ///
    /// ```
    /// use ferralk::Walker;
    ///
    /// let walker = Walker::new("crates")
    ///     .add_root("tools")?
    ///     .include("**/*.rs")?;
    /// # Ok::<(), ferralk::ferralk_glob::PatternError>(())
    /// ```
    pub fn add_root(mut self, root: impl Into<PathBuf>) -> Result<Self, PatternError> {
        self.try_add_root(root)?;
        Ok(self)
    }

    /// Adds another root without consuming the builder.
    ///
    /// This is the borrowed counterpart to [`Walker::add_root`]. It is useful
    /// when applying a caller-supplied root list: if an already-added absolute
    /// pattern cannot be rewritten for one root, this method returns its
    /// [`PatternError`] and leaves the configured walker unchanged, so later
    /// roots can still be considered. A valid root has the same semantics as
    /// [`Walker::add_root`].
    ///
    /// ```no_run
    /// use ferralk::Walker;
    ///
    /// let mut walker = Walker::new("workspace");
    /// for root in ["generated", "vendor"] {
    ///     if let Err(error) = walker.try_add_root(root) {
    ///         eprintln!("skipping {root:?}: {error}");
    ///     }
    /// }
    ///
    /// let result = walker.collect()?;
    /// # let _ = result;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn try_add_root(&mut self, root: impl Into<PathBuf>) -> Result<&mut Self, PatternError> {
        let mut plan = RootPlan::new(root.into());
        let options = traversal_pattern_options(self.match_hidden);
        let root_bytes = glob_path_bytes(&plan.path);
        for source in &self.include_sources {
            plan.includes
                .push(compile_for_root(source, root_bytes.as_ref(), options)?);
        }
        for source in &self.exclude_sources {
            plan.excludes
                .push(compile_for_root(source, root_bytes.as_ref(), options)?);
        }
        drop(root_bytes);
        self.roots.push(plan);
        Ok(self)
    }

    /// Adds several roots, in order.
    ///
    /// # Errors
    ///
    /// The failures [`Walker::add_root`] reports, for the first root that
    /// produces one.
    pub fn add_roots<P: Into<PathBuf>>(
        mut self,
        roots: impl IntoIterator<Item = P>,
    ) -> Result<Self, PatternError> {
        for root in roots {
            self = self.add_root(root)?;
        }
        Ok(self)
    }

    /// The roots this walk starts from, in the order they were added.
    #[must_use]
    pub fn roots(&self) -> impl ExactSizeIterator<Item = &Path> {
        self.roots.iter().map(|root| root.path.as_path())
    }

    /// Compiles one pattern once per root, or reports the first rejection.
    fn compile_for_every_root(
        &self,
        pattern: &[u8],
    ) -> Result<Vec<TraversalPattern>, PatternError> {
        let options = traversal_pattern_options(self.match_hidden);
        self.roots
            .iter()
            .map(|root| compile_for_root(pattern, glob_path_bytes(&root.path).as_ref(), options))
            .collect()
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
        for root in &mut self.roots {
            for pattern in root.includes.iter_mut().chain(root.excludes.iter_mut()) {
                pattern.recompile(options);
            }
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
    ///
    /// An explicitly supplied walk root is entered even when an inherited
    /// rule ignores that directory itself, matching ripgrep's explicit-root
    /// behavior. Inherited rules still filter entries below that root.
    #[must_use]
    pub const fn respect_git_ignore(mut self, enabled: bool) -> Self {
        self.respect_git_ignore = enabled;
        self
    }

    /// Overrides the repository-local `core.ignoreCase` value used by
    /// [`Walker::respect_git_ignore`].
    ///
    /// Git sets this value when `git init` or `git clone` probes a
    /// case-insensitive filesystem. When enabled, Ferralk mirrors Git's
    /// ASCII-only ignore-rule case folding. The override takes precedence over
    /// the repository's local config and is useful when Git's effective value
    /// comes from a global config, include, or environment that Ferralk does
    /// not read.
    #[must_use]
    pub const fn git_ignore_case(mut self, enabled: bool) -> Self {
        self.git_ignore_case = Some(enabled);
        self
    }

    /// Clears an explicit [`Walker::git_ignore_case`] override.
    ///
    /// Subsequent Git-ignore walks resume detection from each repository's
    /// local config. This is useful for a reusable builder whose caller first
    /// supplied Git's effective value and then wants its normal local-config
    /// behaviour back.
    #[must_use]
    pub const fn clear_git_ignore_case(mut self) -> Self {
        self.git_ignore_case = None;
        self
    }

    /// Overrides the repository-local `core.precomposeUnicode` value used by
    /// [`Walker::respect_git_ignore`].
    ///
    /// Git implements this filesystem adaptation only on macOS. There, an
    /// enabled value converts valid UTF-8 candidate path components to NFC
    /// before ignore matching; invalid bytes remain byte-exact. On other
    /// platforms this method is retained for portable builder code but has no
    /// effect, matching Git's platform applicability. The override takes
    /// precedence over repository-local config.
    #[must_use]
    pub const fn git_precompose_unicode(mut self, enabled: bool) -> Self {
        self.git_precompose_unicode = Some(enabled);
        self
    }

    /// Clears an explicit [`Walker::git_precompose_unicode`] override.
    ///
    /// Subsequent Git-ignore walks resume detection from each repository's
    /// local config. On non-macOS platforms this retains Git's no-op
    /// applicability for this setting.
    #[must_use]
    pub const fn clear_git_precompose_unicode(mut self) -> Self {
        self.git_precompose_unicode = None;
        self
    }

    /// Limits `collect()` to this many workers. Values are clamped to
    /// `1..=256`; `stream()` remains single-threaded to preserve incremental
    /// delivery. The upper bound caps the queues, scratch buffers, and
    /// potential operating-system threads one walk can reserve.
    #[must_use]
    pub const fn threads(mut self, threads: usize) -> Self {
        self.threads = if threads == 0 {
            1
        } else if threads > MAX_WORKERS {
            MAX_WORKERS
        } else {
            threads
        };
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
        for task in self.root_tasks(backend) {
            scheduler.push(task);
        }
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
        // Reversed, because the stream pops from the back and the roots are
        // walked in the order the caller added them.
        let mut pending_directories = self.root_tasks(&SystemBackend);
        pending_directories.reverse();
        WalkStream {
            pending_directories,
            walker: self,
            listing: Listing::default(),
            glob_bytes: Vec::new(),
            next_entry: 0,
            path: PathBuf::new(),
            directory: PathBuf::new(),
            ancestors: AncestorChain::default(),
            ignores: IgnoreScope::default(),
            depth: 0,
            root: 0,
            pending_errors: Vec::new(),
            cancelled: false,
            stopped: false,
        }
    }

    fn may_descend_into(&self, root: usize, relative: &[u8]) -> bool {
        let includes = &self.roots[root].includes;
        includes.is_empty()
            || includes
                .iter()
                .any(|pattern| pattern.could_match_descendant(relative))
    }

    /// Whether the walk may traverse into a directory found at `depth`. The
    /// caller counted the components once and passes the result in.
    fn may_descend_at(&self, root: usize, depth: usize, bytes: &[u8]) -> bool {
        self.options
            .max_depth
            .is_none_or(|max_depth| depth < max_depth)
            && self.may_descend_into(root, bytes)
    }

    fn may_include_file(&self, root: usize, relative: &[u8]) -> bool {
        let includes = &self.roots[root].includes;
        includes.is_empty()
            || includes
                .iter()
                .any(|pattern| pattern.matches_extension(relative))
    }

    /// Whether a queued directory can be consumed without resolving its
    /// reported path for any later filesystem operation.
    ///
    /// Descriptor-relative opens deliberately stay out of modes that load
    /// ignore files, identify symlink cycles, or request entry metadata. Until
    /// those operations also accept a directory capability, mixing them with
    /// an `openat` listing could observe two trees after an ancestor rename.
    fn allows_descriptor_relative_descent(&self) -> bool {
        !self.respect_git_ignore
            && !self.options.follow_symlinks
            && !self.options.metadata
            && !self.options.resolve_symlink_kind
    }

    /// The tasks a walk starts from: one per root, in order.
    fn root_tasks<B: DirectoryBackend + ?Sized>(&self, backend: &B) -> Vec<DirectoryTask> {
        self.roots
            .iter()
            .enumerate()
            .map(|(index, plan)| {
                let (ignores, ignore_errors) = IgnoreScope::for_root(self, backend, &plan.path);
                DirectoryTask {
                    path: plan.path.clone(),
                    open: DirectoryOpen::default(),
                    depth: 0,
                    root: index,
                    ancestors: AncestorChain::default(),
                    ignores,
                    ignore_errors,
                }
            })
            .collect()
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

/// Normalizes a path into caller-owned reusable storage on Windows.
#[cfg(windows)]
pub(crate) fn glob_bytes_into<'a>(bytes: &[u8], scratch: &'a mut Vec<u8>) -> &'a [u8] {
    scratch.clear();
    scratch.extend(
        bytes
            .iter()
            .map(|&byte| if byte == b'\\' { b'/' } else { byte }),
    );
    scratch
}

/// Compiles one include or exclude for one root, rewriting it if it is
/// absolute.
///
/// Free rather than a method, because a root is compiled against before it
/// belongs to the walker, and because it is the unit a multi-root walk repeats.
fn compile_for_root(
    pattern: &[u8],
    root: &[u8],
    options: PatternOptions,
) -> Result<TraversalPattern, PatternError> {
    match walker_pattern_for_root(pattern, root, absolute::Syntax::NATIVE, options)? {
        Some(usable) => TraversalPattern::compile(&usable, options),
        // Compiled even though it can never match, so that a pattern the caller
        // wrote badly is still reported, and kept rather than dropped, because
        // dropping an include would widen the walk to everything instead of
        // narrowing it to nothing.
        None => {
            let mut compiled = TraversalPattern::compile(pattern, options)?;
            compiled.never_matches = true;
            Ok(compiled)
        }
    }
}

/// The bytes the walker will compile for `root`, or the reason it will not.
///
/// `None` is the verdict that the pattern names paths outside this root and can
/// select nothing here. The path-shaped check runs after the rewrite and only
/// on the outcomes that still have to match something: a pattern already known
/// to select nothing needs no second reason, and refusing it would turn the
/// verdict a multi-root walk depends on into an error.
fn walker_pattern_for_root(
    pattern: &[u8],
    root: &[u8],
    syntax: absolute::Syntax,
    options: PatternOptions,
) -> Result<Option<Vec<u8>>, PatternError> {
    let Some(rewritten) = rewrite_pattern_for_root_with_source(pattern, root, syntax)? else {
        return Ok(None);
    };
    let parsed = Pattern::compile(pattern_without_directory_marker(&rewritten.bytes), options)
        .map_err(|error| rebase_pattern_error(error, rewritten.source_start))?;
    reject_unwalkable_relative_pattern(
        parsed.walker_path_viability(),
        parsed.walker_path_problem_offset(),
    )
    .map_err(|error| rebase_pattern_error(error, rewritten.source_start))?;
    Ok(Some(rewritten.bytes))
}

struct RewrittenPattern {
    bytes: Vec<u8>,
    source_start: usize,
}

fn rebase_pattern_error(error: PatternError, source_start: usize) -> PatternError {
    PatternError::new(source_start + error.offset(), error.message())
}

/// Applies only the root-to-pattern relation shared by the walk and corpus
/// harness. Walker construction adds the compiled root-relative candidate
/// validation afterwards; the corpus records absolute rewrite semantics even
/// for synthetic platform spellings that do not describe this host's walker.
fn rewrite_pattern_for_root(
    pattern: &[u8],
    root: &[u8],
    syntax: absolute::Syntax,
) -> Result<Option<Vec<u8>>, PatternError> {
    Ok(rewrite_pattern_for_root_with_source(pattern, root, syntax)?
        .map(|rewritten| rewritten.bytes))
}

fn rewrite_pattern_for_root_with_source(
    pattern: &[u8],
    root: &[u8],
    syntax: absolute::Syntax,
) -> Result<Option<RewrittenPattern>, PatternError> {
    match absolute::rewrite_in(pattern, root, syntax)? {
        absolute::Rewrite::Relative => {
            absolute::reject_path_shaped(pattern, syntax)?;
            Ok(Some(RewrittenPattern {
                bytes: pattern.to_vec(),
                source_start: 0,
            }))
        }
        absolute::Rewrite::Rooted {
            bytes,
            source_start,
        } => {
            absolute::reject_path_shaped(&bytes, syntax)
                .map_err(|error| rebase_pattern_error(error, source_start))?;
            Ok(Some(RewrittenPattern {
                bytes,
                source_start,
            }))
        }
        absolute::Rewrite::Outside => Ok(None),
    }
}

/// The one trailing slash [`TraversalPattern`] treats as a directory-only
/// marker is not part of the root-relative candidate spelling. Validation uses
/// the same marker-free bytes so `aaa/` remains valid while `src//bar` and
/// other leading or interior empty components stay unselectable.
fn pattern_without_directory_marker(pattern: &[u8]) -> &[u8] {
    if pattern.len() > 1 {
        pattern.strip_suffix(b"/").unwrap_or(pattern)
    } else {
        pattern
    }
}

/// Rejects root-relative path spellings that name no walk candidate.
///
/// A leading `./` remains the conventional harmless spelling for a pattern
/// below the root, but `.` and `./` name the root itself, which a walk never
/// emits. Any other real `.` component (`src/./x` or `src/.`) likewise names
/// a spelling no root-relative candidate uses. Brace alternatives are expanded
/// and reject every arm containing `..`, including mixed forms such as
/// `{src,..}`, while an extglob-only
/// component such as `@(.)` remains deliberately opaque matcher text and
/// selects nothing. The glob compiler summarizes its actual expanded
/// alternatives and extglob branches, so this policy never reparses matcher
/// syntax.
fn reject_unwalkable_relative_pattern(
    viability: WalkerPathViability,
    offset: Option<usize>,
) -> Result<(), PatternError> {
    let message = match viability {
        WalkerPathViability::Viable => return Ok(()),
        WalkerPathViability::ParentComponent => {
            "`..` in a walker-relative pattern is not resolved, because resolving it lexically would be wrong across a symlink"
        }
        WalkerPathViability::Root => {
            "a walker-relative pattern that names the walk root itself selects nothing; add `/**` to select what is inside it"
        }
        WalkerPathViability::TrailingDot => {
            "a walker-relative pattern ending in `/.` selects that directory itself; add `/**` to select what is inside it"
        }
        WalkerPathViability::DotComponent => {
            "a walker-relative pattern with a `.` component is not normalized; remove `/.` to name the entry below that directory"
        }
    };
    // Brace expansion can create several equally valid source locations. The
    // compiler carries an offset whenever it is determinate and uses this
    // established fallback only for genuinely ambiguous expanded branches.
    Err(PatternError::new(offset.unwrap_or(0), message))
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
    /// Set for an absolute pattern that named paths outside this walk root.
    /// Such a pattern selects nothing and prunes nothing, and saying so once
    /// here keeps the decision out of every caller of the four predicates.
    never_matches: bool,
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
            never_matches: false,
        })
    }

    /// Recompiles the pattern under changed matcher options.
    ///
    /// `match_hidden` is a matching-time policy - it decides whether a wildcard
    /// may cover a leading period - and never a question of syntax, so a source
    /// that compiled once compiles again.
    fn recompile(&mut self, options: PatternOptions) {
        let source = std::mem::take(&mut self.source);
        // Whether the pattern can reach this root is a question about the root,
        // which `match_hidden` does not change.
        let never_matches = self.never_matches;
        *self = Self::compile(&source, options)
            .expect("a compiled pattern stays valid when only match_hidden changes");
        self.never_matches = never_matches;
    }

    /// Whether the pattern selects this candidate under `mode`.
    ///
    /// The two readings differ only in how far an ordinary wildcard reaches, so
    /// they are the same compiled pattern asked a different question rather
    /// than two compilations.
    fn matches(&self, path: &[u8], is_dir: bool, mode: WildcardMode) -> bool {
        if self.never_matches || (self.directories_only && !is_dir) {
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
        if self.never_matches || self.directories_only {
            return false;
        }
        self.subtree_root.as_ref().is_some_and(|root| match mode {
            WildcardMode::ComponentScoped => root.is_match_glob_path(path),
            WildcardMode::SeparatorCrossing => root.is_match(path),
        })
    }

    fn could_match_descendant(&self, path: &[u8]) -> bool {
        if self.never_matches {
            return false;
        }
        let Some(roots) = &self.literal_roots else {
            return true;
        };
        roots
            .iter()
            .any(|root| shares_a_line_of_descent(root, path))
    }

    /// Whether this include can reach below `path` and explicitly select a
    /// component that wildcard-hidden policy leaves outside a covering
    /// exclude. The matcher compiler owns the syntax analysis, including
    /// brace and extglob alternatives; the walker adds only root reachability.
    fn could_match_hidden_descendant(&self, path: &[u8]) -> bool {
        self.could_match_descendant(path)
            && self
                .matcher
                .can_match_hidden_component_without_match_hidden()
    }

    fn matches_extension(&self, path: &[u8]) -> bool {
        if self.never_matches {
            return false;
        }
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

/// Offset of the first byte that stops the pattern from being a literal path.
///
/// A brace or an extglob opener only counts once its closer is present, because
/// an unpaired one is an ordinary byte. Shared with the absolute-pattern rewrite
/// so that "how far is this pattern a plain path" has one answer.
pub(crate) fn first_metacharacter(pattern: &[u8]) -> Option<usize> {
    pattern.iter().enumerate().position(|(index, byte)| {
        matches!(byte, b'*' | b'?' | b'[')
            || (*byte == b'\\')
            || (*byte == b'{' && has_closing_brace(pattern, index))
            || (matches!(byte, b'@' | b'+' | b'!')
                && pattern.get(index + 1) == Some(&b'(')
                && has_closing_parenthesis(pattern, index + 1))
    })
}

fn literal_pattern_root(pattern: &[u8]) -> Option<Vec<u8>> {
    let magic = first_metacharacter(pattern);
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

/// A backend-specific capability retained when a child directory is queued.
#[derive(Debug, Clone, Default)]
pub(crate) enum DirectoryOpen {
    #[default]
    None,
    #[cfg(all(feature = "native-macos", target_os = "macos"))]
    MacosRelative(macos_native::RelativeDirectoryOpen),
    #[cfg(all(feature = "native-linux", target_os = "linux"))]
    LinuxRelative(linux_native::RelativeDirectoryOpen),
    #[cfg(any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ))]
    Suspended(DirectoryIdentity),
}

#[cfg(any(
    all(feature = "native-macos", target_os = "macos"),
    all(feature = "native-linux", target_os = "linux")
))]
type DirectoryIdentity = (u64, u64);
#[cfg(not(any(
    all(feature = "native-macos", target_os = "macos"),
    all(feature = "native-linux", target_os = "linux")
)))]
type DirectoryIdentity = ();

impl DirectoryOpen {
    fn suspended(_identity: DirectoryIdentity) -> Self {
        #[cfg(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        ))]
        return Self::Suspended(_identity);
        #[cfg(not(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        )))]
        {
            Self::None
        }
    }

    fn suspended_identity(&self) -> Option<DirectoryIdentity> {
        #[cfg(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        ))]
        if let Self::Suspended(identity) = self {
            return Some(*identity);
        }
        None
    }
}

/// The filesystem calls that traversal and classification make, so one mock
/// can drive the serial and the parallel frontend alike.
trait DirectoryBackend {
    /// Reads one directory into `listing`, replacing whatever it held.
    ///
    /// The listing is the caller's, and the caller reuses it for every
    /// directory it reads, so a backend that can name an entry without
    /// allocating leaves the walk allocating nothing per entry at all.
    fn read_directory(
        &self,
        path: &Path,
        follow_symlinks: bool,
        refuse_final_symlink: bool,
        listing: &mut Listing,
    ) -> std::io::Result<()>;

    /// Reads a queued directory through a retained capability when the backend
    /// has one. Path-based backends and tests keep the ordinary method above.
    fn read_scheduled_directory(
        &self,
        path: &Path,
        open: &DirectoryOpen,
        follow_symlinks: bool,
        refuse_final_symlink: bool,
        listing: &mut Listing,
    ) -> std::io::Result<()> {
        let _ = open;
        self.read_directory(path, follow_symlinks, refuse_final_symlink, listing)
    }

    /// Retains what this backend needs to open one child relative to the
    /// directory represented by `listing`.
    fn child_directory_open(
        &self,
        listing: &Listing,
        name: &OsStr,
        allow_relative: bool,
    ) -> DirectoryOpen {
        let _ = (listing, name, allow_relative);
        DirectoryOpen::default()
    }

    /// Identity of the retained directory capability, when this backend has
    /// one. Serial suspension records it before releasing the descriptor.
    fn directory_identity(&self, listing: &Listing) -> Option<DirectoryIdentity> {
        let _ = listing;
        None
    }

    /// Reacquires the current directory capability after a serial frame
    /// resumes and verifies it still names the directory that was suspended.
    /// `false` means the mutable path now reaches a different identity, so its
    /// cached entries must not be used as names below that replacement.
    fn restore_directory_open(
        &self,
        path: &Path,
        expected: Option<DirectoryIdentity>,
        refuse_final_symlink: bool,
        listing: &mut Listing,
    ) -> bool {
        let _ = (path, expected, refuse_final_symlink, listing);
        true
    }

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

    /// Reads an in-tree ignore file. Missing files are the common case and
    /// are reported as an error rather than probed for beforehand.
    ///
    /// On Unix this refuses symlinks atomically. Git likewise refuses an
    /// in-tree `.gitignore` that resolves through a link, so its rules cannot
    /// redirect a walk to an arbitrary file outside the tree.
    fn read_ignore_file(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        read_in_tree_ignore_file(path)
    }

    /// Reads repository metadata such as `.git/info/exclude`.
    ///
    /// Unlike in-tree rule files, Git permits this file to be a link. Keeping
    /// this separate prevents a repository-wide exclude from weakening the
    /// in-tree no-follow rule above.
    fn read_repository_file(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        read_bounded_file(path)
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

/// Ignore and repository-metadata files are configuration, not bulk input.
/// Eight MiB leaves orders of magnitude above measured repository files while
/// bounding one attacker-controlled read below the matcher's own 64 MiB brace
/// expansion ceiling.
pub(crate) const MAX_IGNORE_FILE_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn read_bounded_file(path: &Path) -> std::io::Result<Vec<u8>> {
    read_bounded(fs::File::open(path)?)
}

fn read_bounded(file: fs::File) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let mut contents = Vec::new();
    file.take(MAX_IGNORE_FILE_BYTES + 1)
        .read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_IGNORE_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "configuration file exceeds the 8 MiB safety limit",
        ));
    }
    Ok(contents)
}

/// Reads a rule file found in the tree being walked.
///
/// Unix opens the final path component with `O_NOFOLLOW`, so exchanging a
/// regular rule file for a symlink between a metadata check and an open cannot
/// make a walk read a target outside the tree. Other supported platforms do
/// not expose an equivalent through `std`, so they reject a symlink before the
/// regular `fs::read`; Windows will receive an atomic implementation once its
/// stable standard-library API exposes one.
#[cfg(unix)]
fn read_in_tree_ignore_file(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    read_bounded(file)
}

#[cfg(not(unix))]
fn read_in_tree_ignore_file(path: &Path) -> std::io::Result<Vec<u8>> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    read_bounded_file(path)
}

/// What identifies a directory for the follow-symlinks ancestor-chain guard.
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

/// Directories on the path from one follow-mode task's root to itself.
///
/// A link may name a directory reached by a sibling path without forming a
/// loop, so the key is compared only with this task's ancestors. Persistent
/// links make extending the chain cheap when a task is queued to a worker.
#[derive(Debug, Default, Clone)]
pub(crate) struct AncestorChain(Option<Arc<AncestorLink>>);

#[derive(Debug)]
struct AncestorLink {
    key: CycleKey,
    parent: Option<Arc<AncestorLink>>,
}

impl AncestorChain {
    /// Extends this task's chain with `key`, unless it would revisit an
    /// ancestor and therefore close a directory cycle.
    fn enter(&self, key: CycleKey) -> Option<Self> {
        let mut ancestor = self.0.as_deref();
        while let Some(link) = ancestor {
            if link.key == key {
                return None;
            }
            ancestor = link.parent.as_deref();
        }
        Some(Self(Some(Arc::new(AncestorLink {
            key,
            parent: self.0.clone(),
        }))))
    }
}

/// Names the call whose failure ends a directory in follow mode, so the
/// reported operation stays the one that actually ran.
#[cfg(unix)]
const CYCLE_KEY_OPERATION: &str = "metadata";
#[cfg(not(unix))]
const CYCLE_KEY_OPERATION: &str = "canonicalize";

const IGNORE_FILE_OPERATION: &str = "read_ignore";

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
    #[cfg(all(feature = "native-macos", target_os = "macos"))]
    native_directory: Option<Arc<macos_native::RetainedDirectory>>,
    #[cfg(all(feature = "native-linux", target_os = "linux"))]
    native_directory: Option<Arc<linux_native::RetainedDirectory>>,
    /// Entries in use. `entries` may be longer: the tail is buffers kept for
    /// the next directory.
    len: usize,
    /// Per-entry failures discovered while a backend was completing an
    /// otherwise usable listing. They are delivered after its siblings, so a
    /// persistent stat failure cannot make those siblings disappear.
    deferred_errors: Vec<DeferredListingError>,
}

#[derive(Debug)]
pub(crate) struct DeferredListingError {
    path: PathBuf,
    source: DeferredIoError,
}

/// An `io::Error` representation that can live in the public stream without
/// changing its auto-trait contract. `std::io::Error` may carry an arbitrary
/// error object, which is not necessarily unwind-safe; a deferred listing
/// error needs only the externally observable kind and message until it is
/// delivered as a fresh `io::Error`.
#[derive(Debug)]
struct DeferredIoError {
    kind: std::io::ErrorKind,
    message: String,
}

impl From<std::io::Error> for DeferredIoError {
    fn from(source: std::io::Error) -> Self {
        Self {
            kind: source.kind(),
            message: source.to_string(),
        }
    }
}

impl DeferredIoError {
    fn into_io_error(self) -> std::io::Error {
        std::io::Error::new(self.kind, self.message)
    }
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
        #[cfg(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        ))]
        {
            self.native_directory = None;
        }
        self.deferred_errors.clear();
    }

    /// Releases the native handle while a serial listing is suspended. The
    /// queued child already owns the clone it needs for its one relative open.
    fn release_directory_open(&mut self) {
        #[cfg(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        ))]
        {
            self.native_directory = None;
        }
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

    /// Records a failure for one entry without throwing away the successfully
    /// read siblings. Consumers report these after the listing is consumed.
    pub(crate) fn defer_error(&mut self, path: PathBuf, source: std::io::Error) {
        self.deferred_errors.push(DeferredListingError {
            path,
            source: source.into(),
        });
    }

    pub(crate) fn take_deferred_error(&mut self) -> Option<DeferredListingError> {
        (!self.deferred_errors.is_empty()).then(|| self.deferred_errors.remove(0))
    }

    /// Whether the directory holds an entry of this name, which is how the
    /// ignore chain recognizes its own files without probing for them.
    pub(crate) fn contains(&self, name: &str) -> bool {
        self.entries().iter().any(|entry| entry.name == *name)
    }

    /// Whether a directory contains a possible spelling of a Git ignore file.
    /// The caller still opens the canonical name: on a case-sensitive
    /// filesystem that open fails for `.GITIGNORE`, while on APFS/NTFS it
    /// resolves exactly as Git's `open(".gitignore")` does.
    pub(crate) fn contains_git_ignore_name(&self, name: &str) -> bool {
        self.contains(name)
            || self.entries().iter().any(|entry| {
                entry
                    .name
                    .as_encoded_bytes()
                    .eq_ignore_ascii_case(name.as_bytes())
            })
    }
}

#[cfg_attr(
    any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ),
    allow(dead_code)
)]
struct StdBackend;

impl DirectoryBackend for StdBackend {
    fn read_directory(
        &self,
        path: &Path,
        _follow_symlinks: bool,
        _refuse_final_symlink: bool,
        listing: &mut Listing,
    ) -> std::io::Result<()> {
        read_portable_directory(path, path, listing)
    }
}

/// Reads a directory through the portable `std::fs` backend. `directory` is
/// the path opened by the operating system; `reported_path` keeps deferred
/// per-entry errors anchored at the caller's path when a native no-follow
/// descriptor is exposed through `/dev/fd` for a safe fallback.
#[cfg_attr(
    any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ),
    allow(dead_code)
)]
fn read_portable_directory(
    directory: &Path,
    reported_path: &Path,
    listing: &mut Listing,
) -> std::io::Result<()> {
    listing.clear();
    for entry in fs::read_dir(directory)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                defer_entry_stat_error(listing, reported_path.to_path_buf(), error)?;
                continue;
            }
        };
        // `DirEntry` only exposes its name by value. Retain that one
        // allocation for the listing and do not construct a full path unless
        // the uncommon `file_type` failure needs it for error reporting.
        let name = entry.file_name();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                defer_entry_stat_error(listing, reported_path.join(&name), error)?;
                continue;
            }
        };
        // The `file_name` allocation is the portable backend's visible floor.
        // On Linux, `ReadDir` has already copied the readdir name into its own
        // CString as well. Native backends read names out of a buffer they own
        // and reach zero.
        listing.push(&name, file_type.is_dir(), file_type.is_symlink());
    }
    Ok(())
}

/// Keeps the three directory readers on the same `DT_UNKNOWN` contract.
///
/// `NotFound` and `NotADirectory` describe a path that changed after its
/// directory record was read, so that one entry is dropped. `PermissionDenied`
/// is persistent often enough that silently treating it as a race loses data;
/// it is delayed until the usable listing has been delivered. Other failures
/// make the directory read fail normally.
pub(crate) fn defer_entry_stat_error(
    listing: &mut Listing,
    path: PathBuf,
    error: std::io::Error,
) -> std::io::Result<()> {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => Ok(()),
        std::io::ErrorKind::PermissionDenied => {
            listing.defer_error(path, error);
            Ok(())
        }
        _ => Err(error),
    }
}

/// Selects the feature-gated native backend where it is available and the
/// portable backend everywhere else.
struct SystemBackend;

#[cfg(any(
    all(feature = "native-macos", target_os = "macos"),
    all(feature = "native-linux", target_os = "linux")
))]
#[cfg_attr(
    any(
        all(feature = "native-macos", target_os = "macos"),
        all(feature = "native-linux", target_os = "linux")
    ),
    allow(dead_code)
)]
fn read_native_or_portable(
    listing: &mut Listing,
    native: impl FnOnce(&mut Listing) -> std::io::Result<()>,
    fallback: impl FnOnce(&mut Listing) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match native(listing) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => fallback(listing),
        Err(error) => Err(error),
    }
}

impl DirectoryBackend for SystemBackend {
    fn read_directory(
        &self,
        path: &Path,
        follow_symlinks: bool,
        refuse_final_symlink: bool,
        listing: &mut Listing,
    ) -> std::io::Result<()> {
        #[cfg(all(feature = "native-macos", target_os = "macos"))]
        {
            // macOS performs its capability fallback inside the native module,
            // where it still owns the protected directory descriptor. Keeping
            // ordinary `Unsupported` I/O errors out of the generic adapter is
            // essential: a DT_UNKNOWN stat may report that kind after a batch,
            // and a path fallback would discard the usable siblings.
            let _ = follow_symlinks;
            macos_native::read_directory(path, None, refuse_final_symlink, listing)
                .map_err(macos_native::NativeDirectoryReadError::into_io_error)
        }
        #[cfg(all(
            feature = "native-linux",
            target_os = "linux",
            not(all(feature = "native-macos", target_os = "macos"))
        ))]
        {
            // Linux performs its capability fallback inside the native module,
            // where it still owns the `O_NOFOLLOW` directory descriptor. A
            // generic path fallback would reopen a scheduled descendant after
            // a replacement race; an ordinary Unsupported after a batch must
            // also remain an ordinary walker error rather than restart it.
            let _ = follow_symlinks;
            linux_native::read_directory(path, None, refuse_final_symlink, listing)
        }
        #[cfg(not(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        )))]
        StdBackend.read_directory(path, follow_symlinks, refuse_final_symlink, listing)
    }

    fn read_scheduled_directory(
        &self,
        path: &Path,
        _open: &DirectoryOpen,
        follow_symlinks: bool,
        refuse_final_symlink: bool,
        listing: &mut Listing,
    ) -> std::io::Result<()> {
        #[cfg(all(feature = "native-macos", target_os = "macos"))]
        {
            let _ = follow_symlinks;
            let relative = match _open {
                DirectoryOpen::MacosRelative(relative) => Some(relative),
                _ => None,
            };
            macos_native::read_directory(path, relative, refuse_final_symlink, listing)
                .map_err(macos_native::NativeDirectoryReadError::into_io_error)
        }
        #[cfg(all(
            feature = "native-linux",
            target_os = "linux",
            not(all(feature = "native-macos", target_os = "macos"))
        ))]
        {
            let _ = follow_symlinks;
            let relative = match _open {
                DirectoryOpen::LinuxRelative(relative) => Some(relative),
                _ => None,
            };
            linux_native::read_directory(path, relative, refuse_final_symlink, listing)
        }
        #[cfg(not(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        )))]
        self.read_directory(path, follow_symlinks, refuse_final_symlink, listing)
    }

    fn child_directory_open(
        &self,
        listing: &Listing,
        name: &OsStr,
        allow_relative: bool,
    ) -> DirectoryOpen {
        if !allow_relative {
            return DirectoryOpen::default();
        }
        #[cfg(all(feature = "native-macos", target_os = "macos"))]
        {
            listing
                .native_directory
                .as_ref()
                .map_or_else(DirectoryOpen::default, |directory| {
                    DirectoryOpen::MacosRelative(macos_native::RelativeDirectoryOpen {
                        parent: Arc::clone(directory),
                        name: name.to_os_string(),
                    })
                })
        }
        #[cfg(all(
            feature = "native-linux",
            target_os = "linux",
            not(all(feature = "native-macos", target_os = "macos"))
        ))]
        {
            listing
                .native_directory
                .as_ref()
                .map_or_else(DirectoryOpen::default, |directory| {
                    DirectoryOpen::LinuxRelative(linux_native::RelativeDirectoryOpen {
                        parent: Arc::clone(directory),
                        name: name.to_os_string(),
                    })
                })
        }
        #[cfg(not(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        )))]
        {
            let _ = (listing, name, allow_relative);
            DirectoryOpen::default()
        }
    }

    fn directory_identity(&self, listing: &Listing) -> Option<DirectoryIdentity> {
        #[cfg(all(feature = "native-macos", target_os = "macos"))]
        return macos_native::retained_directory_identity(listing);
        #[cfg(all(
            feature = "native-linux",
            target_os = "linux",
            not(all(feature = "native-macos", target_os = "macos"))
        ))]
        return linux_native::retained_directory_identity(listing);
        #[cfg(not(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        )))]
        {
            let _ = listing;
            None
        }
    }

    fn restore_directory_open(
        &self,
        path: &Path,
        expected: Option<DirectoryIdentity>,
        refuse_final_symlink: bool,
        listing: &mut Listing,
    ) -> bool {
        #[cfg(all(feature = "native-macos", target_os = "macos"))]
        return macos_native::restore_retained_directory(
            path,
            expected,
            refuse_final_symlink,
            listing,
        );
        #[cfg(all(
            feature = "native-linux",
            target_os = "linux",
            not(all(feature = "native-macos", target_os = "macos"))
        ))]
        return linux_native::restore_retained_directory(
            path,
            expected,
            refuse_final_symlink,
            listing,
        );
        #[cfg(not(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux")
        )))]
        {
            let _ = (path, expected, refuse_final_symlink, listing);
            true
        }
    }
}

/// Incremental portable traversal produced by Walker stream.
#[derive(Debug)]
pub struct WalkStream {
    walker: Walker,
    pending_directories: Vec<DirectoryTask>,
    /// The directory being delivered and how far through it the stream is.
    listing: Listing,
    glob_bytes: Vec<u8>,
    next_entry: usize,
    /// That directory's path, with the entry being classified pushed onto it.
    path: PathBuf,
    /// The same directory without an entry on it, kept so `path` can be reset
    /// by [`reset_to_directory`] rather than by `PathBuf::pop`.
    directory: PathBuf,
    /// The ancestor chain of the directory currently being delivered.
    ancestors: AncestorChain,
    /// Ignore rules of the directory whose entries are being delivered.
    ignores: IgnoreScope,
    /// Depth of that same directory, so its entries need not recount it.
    depth: usize,
    /// Which root that directory sits under, for the same reason.
    root: usize,
    /// Ignore-file failures discovered while preparing the current directory.
    pending_errors: Vec<PendingWalkError>,
    cancelled: bool,
    stopped: bool,
}

/// Unwind-safe error data kept between iterator calls. `std::io::Error` may own
/// arbitrary error objects that are not unwind-safe, while `WalkStream` has
/// historically guaranteed the standard unwind auto traits.
#[derive(Debug)]
struct PendingWalkError {
    operation: &'static str,
    path: PathBuf,
    kind: std::io::ErrorKind,
    message: String,
}

impl PendingWalkError {
    fn from_ignore(error: IgnoreReadError) -> Self {
        let (path, source) = error.into_parts();
        Self {
            operation: IGNORE_FILE_OPERATION,
            path,
            kind: source.kind(),
            message: source.to_string(),
        }
    }

    fn into_walk_error(self) -> WalkError {
        WalkError::new(
            self.operation,
            self.path,
            std::io::Error::new(self.kind, self.message),
        )
    }
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
        is_root: bool,
    ) -> Option<Result<WalkEntry, WalkError>> {
        let error = WalkError::new(operation, path, source);
        match self.walker.error_policy {
            ErrorPolicy::Abort => {
                self.stopped = true;
                Some(Err(error))
            }
            ErrorPolicy::Skip if !is_root => None,
            ErrorPolicy::Skip | ErrorPolicy::Collect => Some(Err(error)),
        }
    }

    fn queue_ignore_errors(
        &mut self,
        errors: Vec<IgnoreReadError>,
    ) -> Option<Result<WalkEntry, WalkError>> {
        match self.walker.error_policy {
            ErrorPolicy::Skip => None,
            ErrorPolicy::Abort => errors.into_iter().next().map(|error| {
                self.stopped = true;
                let (path, source) = error.into_parts();
                Err(WalkError::new(IGNORE_FILE_OPERATION, path, source))
            }),
            ErrorPolicy::Collect => {
                self.pending_errors
                    .extend(errors.into_iter().rev().map(PendingWalkError::from_ignore));
                self.pending_errors
                    .pop()
                    .map(PendingWalkError::into_walk_error)
                    .map(Err)
            }
        }
    }

    fn prepare_directory(&mut self, task: DirectoryTask) -> Option<Result<WalkEntry, WalkError>> {
        let DirectoryTask {
            path,
            open,
            depth,
            root,
            ancestors,
            ignores,
            mut ignore_errors,
        } = task;
        let ancestors = if self.walker.options.follow_symlinks {
            match SystemBackend.cycle_key(&path) {
                Ok(key) => ancestors.enter(key)?,
                Err(source) => return self.error(CYCLE_KEY_OPERATION, path, source, depth == 0),
            }
        } else {
            ancestors
        };
        match SystemBackend.read_scheduled_directory(
            &path,
            &open,
            self.walker.options.follow_symlinks,
            !self.walker.options.follow_symlinks && depth > 0,
            &mut self.listing,
        ) {
            Ok(()) => {
                // The directory's own ignore files join the chain here, once,
                // recognized in the listing that was just read.
                let (ignores, mut entered_errors) =
                    ignores.enter(&self.walker, &SystemBackend, &path, &self.listing);
                self.ignores = ignores;
                ignore_errors.append(&mut entered_errors);
                self.depth = depth;
                self.root = root;
                self.ancestors = ancestors;
                self.next_entry = 0;
                self.directory = path;
                reset_to_directory(&mut self.path, &self.directory);
                self.queue_ignore_errors(ignore_errors)
            }
            Err(source) => {
                // A backend can fail after appending entries. They belong to
                // the failed directory and must never be classified against
                // the directory the stream delivered previously.
                self.listing.clear();
                self.next_entry = 0;
                self.error("read_dir", path, source, depth == 0)
            }
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
            TraversalContext {
                root: self.root,
                ancestors: &self.ancestors,
                listing: &self.listing,
                glob_bytes_scratch: &mut self.glob_bytes,
            },
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
                self.error(failure.operation, failure.path, failure.source, false)
            }
        };
        reset_to_directory(&mut self.path, &self.directory);
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
            if let Some(error) = self.pending_errors.pop() {
                return Some(Err(error.into_walk_error()));
            }
            if self.next_entry < self.listing.entries().len() {
                let index = self.next_entry;
                self.next_entry += 1;
                if let Some(result) = self.process_entry(index) {
                    return Some(result);
                }
                continue;
            }
            if let Some(error) = self.listing.take_deferred_error() {
                if let Some(result) =
                    self.error("read_dir", error.path, error.source.into_io_error(), false)
                {
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
    /// Buffers of directories this frontend has finished with. A paused
    /// directory owns its scratch while a child is being processed; completed
    /// frames return it here for the next directory.
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
    glob_bytes: Vec<u8>,
}

/// One suspended serial-directory listing. A child directory is scheduled
/// separately, then this frame resumes at the next entry, which retains the
/// serial walk's depth-first order without borrowing a call-stack frame. The
/// listing keeps its entry buffers while suspended but releases its native
/// handle after handing one clone to the child; resume reacquires an optional
/// handle instead of pinning one descriptor per ancestor.
struct DirectoryFrame {
    task: DirectoryTask,
    ignores: IgnoreScope,
    scratch: DirectoryScratch,
    next_entry: usize,
}

impl DirectoryFrame {
    fn suspend(&mut self, backend: &impl DirectoryBackend) {
        let identity = backend.directory_identity(&self.scratch.listing);
        // If identity capture failed, keep the rare descriptor rather than
        // reopening an identity we could not later verify.
        if let Some(identity) = identity {
            self.task.open = DirectoryOpen::suspended(identity);
            self.scratch.listing.release_directory_open();
        }
    }
}

/// Work the serial frontend has yet to carry out. The LIFO order models the
/// former recursive calls while keeping arbitrarily deep trees off the Rust
/// call stack.
enum SerialTask {
    Directory(DirectoryTask),
    Resume(DirectoryFrame),
    Emit(WalkEntry),
}

/// Entries between two cancellation checks inside one directory.
///
/// Both frontends check when they take a directory and then once every this
/// many entries. Checking per entry bought a granularity nothing observes: a
/// walk is already free to finish the directory it has started, and the check
/// reads shared state — an atomic the parallel frontend's workers all load, and
/// a token behind an `Arc` in the serial one.
///
/// A power of two, so the test is a mask, and small enough that a cancelled
/// walk keeps classifying for the width of a stride rather than the width of a
/// listing.
const CANCELLATION_STRIDE: usize = 64;

/// Puts `path` back to the directory its entries are assembled onto.
///
/// `PathBuf::pop` finds the parent by walking the path's components backwards,
/// which is a question the caller already knows the answer to: it pushed one
/// name onto a directory whose length it recorded. `OsString::truncate` would
/// say exactly that, but it is unstable (rust#133262) and this crate denies
/// unsafe, so the directory is re-copied instead — a `memcpy` of a short path,
/// with no component parsing and no separator scan.
fn reset_to_directory(path: &mut PathBuf, directory: &Path) {
    path.clear();
    path.as_mut_os_string().push(directory.as_os_str());
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
        self.emit_owned(entry);
    }

    fn emit_owned(&mut self, entry: WalkEntry) {
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
        let mut pending = vec![SerialTask::Directory(task)];
        while let Some(task) = pending.pop() {
            match task {
                SerialTask::Directory(task) => self.start_directory(backend, task, &mut pending)?,
                SerialTask::Resume(frame) => self.resume_directory(backend, frame, &mut pending)?,
                SerialTask::Emit(entry) => self.emit_owned(entry),
            }
        }
        Ok(())
    }

    /// Opens one directory and schedules its first listing step. Its scratch
    /// moves into that step, so a child cannot overwrite its parent's path.
    fn start_directory(
        &mut self,
        backend: &impl DirectoryBackend,
        mut task: DirectoryTask,
        pending: &mut Vec<SerialTask>,
    ) -> Result<(), WalkError> {
        if self.check_cancellation() {
            return Ok(());
        }
        let is_root = task.depth == 0;
        for error in std::mem::take(&mut task.ignore_errors) {
            let (path, source) = error.into_parts();
            self.handle_error(IGNORE_FILE_OPERATION, path, source, false)?;
        }
        if self.walker.options.follow_symlinks {
            let Some(ancestors) =
                self.enter_directory(backend, &task.ancestors, &task.path, is_root)?
            else {
                return Ok(());
            };
            task.ancestors = ancestors;
        }
        let mut scratch = self.scratch.pop().unwrap_or_default();
        let path = task.path.as_path();
        let depth = task.depth;
        let is_root = depth == 0;
        let read_result = backend.read_scheduled_directory(
            path,
            &task.open,
            self.walker.options.follow_symlinks,
            !self.walker.options.follow_symlinks && depth > 0,
            &mut scratch.listing,
        );
        // The capability has done its one job: opening this scheduled child.
        // Keeping it in the child's suspended frame would pin its parent for
        // the rest of the depth-first subtree.
        task.open = DirectoryOpen::default();
        if let Err(source) = read_result {
            let result = self.handle_error("read_dir", path.to_path_buf(), source, is_root);
            scratch.listing.clear();
            self.scratch.push(scratch);
            return result;
        }
        // The directory's own ignore files join the chain here, once,
        // recognized in the listing that was just read.
        let (ignores, ignore_errors) =
            std::mem::take(&mut task.ignores).enter(self.walker, backend, path, &scratch.listing);
        for error in ignore_errors {
            let (path, source) = error.into_parts();
            if let Err(error) = self.handle_error(IGNORE_FILE_OPERATION, path, source, false) {
                scratch.listing.clear();
                self.scratch.push(scratch);
                return Err(error);
            }
        }
        scratch.path.clear();
        scratch.path.push(path);
        pending.push(SerialTask::Resume(DirectoryFrame {
            task,
            ignores,
            scratch,
            next_entry: 0,
        }));
        Ok(())
    }

    /// Continues a paused directory until it needs to descend, at which point
    /// it requeues itself above the child. That is the iterative equivalent of
    /// a recursive call followed by a return to this listing.
    fn resume_directory(
        &mut self,
        backend: &impl DirectoryBackend,
        mut frame: DirectoryFrame,
        pending: &mut Vec<SerialTask>,
    ) -> Result<(), WalkError> {
        let path = frame.task.path.as_path();
        let depth = frame.task.depth;
        let suspended_identity = frame.task.open.suspended_identity();
        if frame.next_entry < frame.scratch.listing.entries().len()
            && !backend.restore_directory_open(
                path,
                suspended_identity,
                !self.walker.options.follow_symlinks && depth > 0,
                &mut frame.scratch.listing,
            )
        {
            return self.finish_directory(frame);
        }
        frame.task.open = DirectoryOpen::default();
        while frame.next_entry < frame.scratch.listing.entries().len() {
            // A `Verdict::Stop` is this walk's own decision, already on a local
            // field, and is honoured on the very next entry. The caller's token
            // is polled every [`CANCELLATION_STRIDE`] entries instead of every
            // one, which is what costs a load through the shared `Arc`.
            if self.cancelled
                || (frame.next_entry.is_multiple_of(CANCELLATION_STRIDE)
                    && self.check_cancellation())
            {
                return self.finish_directory(frame);
            }
            // The entry's path exists only for as long as it is being decided
            // about; anything that outlives that copies it out.
            let index = frame.next_entry;
            frame.next_entry += 1;
            frame
                .scratch
                .path
                .push(frame.scratch.listing.entries()[index].name());
            let action = classify_entry(
                self.walker,
                backend,
                &frame.scratch.path,
                &frame.scratch.listing.entries()[index],
                &frame.ignores,
                depth,
                TraversalContext {
                    root: frame.task.root,
                    ancestors: &frame.task.ancestors,
                    listing: &frame.scratch.listing,
                    glob_bytes_scratch: &mut frame.scratch.glob_bytes,
                },
            );
            match action {
                EntryAction::Skip => reset_to_directory(&mut frame.scratch.path, path),
                EntryAction::Emit(entry) => {
                    self.emit(&frame.scratch.path, entry);
                    reset_to_directory(&mut frame.scratch.path, path);
                }
                EntryAction::Descend(task) => {
                    reset_to_directory(&mut frame.scratch.path, path);
                    frame.suspend(backend);
                    pending.push(SerialTask::Resume(frame));
                    pending.push(SerialTask::Directory(task));
                    return Ok(());
                }
                // The subtree is walked before the directory itself is
                // recorded, the depth-first order this frontend has always
                // exposed. Its path must now outlive the paused frame.
                EntryAction::DescendAndEmit(entry, task) => {
                    let entry = entry.with_path(own_path(&mut self.spare, &frame.scratch.path));
                    reset_to_directory(&mut frame.scratch.path, path);
                    frame.suspend(backend);
                    pending.push(SerialTask::Resume(frame));
                    pending.push(SerialTask::Emit(entry));
                    pending.push(SerialTask::Directory(task));
                    return Ok(());
                }
                EntryAction::Failed { failure, descend } => {
                    reset_to_directory(&mut frame.scratch.path, path);
                    if let Err(error) =
                        self.handle_error(failure.operation, failure.path, failure.source, false)
                    {
                        self.finish_directory(frame)?;
                        return Err(error);
                    }
                    if let Some(task) = descend {
                        frame.suspend(backend);
                        pending.push(SerialTask::Resume(frame));
                        pending.push(SerialTask::Directory(task));
                        return Ok(());
                    }
                }
            }
        }
        while let Some(error) = frame.scratch.listing.take_deferred_error() {
            if let Err(error) =
                self.handle_error("read_dir", error.path, error.source.into_io_error(), false)
            {
                self.finish_directory(frame)?;
                return Err(error);
            }
        }
        self.finish_directory(frame)
    }

    fn finish_directory(&mut self, mut frame: DirectoryFrame) -> Result<(), WalkError> {
        frame.scratch.listing.clear();
        self.scratch.push(frame.scratch);
        Ok(())
    }

    fn enter_directory(
        &mut self,
        backend: &impl DirectoryBackend,
        ancestors: &AncestorChain,
        directory: &Path,
        is_root: bool,
    ) -> Result<Option<AncestorChain>, WalkError> {
        match backend.cycle_key(directory) {
            Ok(key) => Ok(ancestors.enter(key)),
            Err(source) => {
                self.handle_error(
                    CYCLE_KEY_OPERATION,
                    directory.to_path_buf(),
                    source,
                    is_root,
                )?;
                Ok(None)
            }
        }
    }

    fn handle_error(
        &mut self,
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
        is_root: bool,
    ) -> Result<(), WalkError> {
        let error = WalkError::new(operation, path, source);
        match self.walker.error_policy {
            ErrorPolicy::Abort => Err(error),
            ErrorPolicy::Skip if !is_root => Ok(()),
            ErrorPolicy::Skip | ErrorPolicy::Collect => {
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
        process::Command,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        CancellationToken, ErrorPolicy, Pattern, PatternOptions, TraversalPattern, Verdict,
        WalkEntry, WalkEntryKind, WalkOptions, WalkStream, Walker, WildcardMode, glob_path_bytes,
        literal_extension, literal_pattern_root, traversal_pattern_options,
    };

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    const HOSTILE_GIT_CONFIG_FIXTURE: &str = "FERRALK_HOSTILE_GIT_CONFIG_FIXTURE";
    const PARENT_ROOT_IGNORE_FIXTURE: &str = "FERRALK_PARENT_ROOT_IGNORE_FIXTURE";
    const RELATIVE_ROOT_IGNORE_FIXTURE: &str = "FERRALK_RELATIVE_ROOT_IGNORE_FIXTURE";
    const RELATIVE_ROOT_SPELLING: &str = "FERRALK_RELATIVE_ROOT_SPELLING";
    #[cfg(unix)]
    const SYMLINK_PARENT_ROOT_FIXTURE: &str = "FERRALK_SYMLINK_PARENT_ROOT_FIXTURE";

    #[test]
    fn walk_stream_keeps_its_unwind_auto_traits() {
        fn assert_unwind_safe<T: std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}

        assert_unwind_safe::<WalkStream>();
    }

    #[test]
    fn worker_budget_is_bounded_at_both_ends() {
        assert_eq!(Walker::new(".").threads(0).threads, 1);
        assert_eq!(
            Walker::new(".").threads(usize::MAX).threads,
            super::MAX_WORKERS
        );
    }

    #[test]
    fn descriptor_relative_descent_requires_path_independent_options() {
        assert!(Walker::new(".").allows_descriptor_relative_descent());
        assert!(
            Walker::new(".")
                .options(WalkOptions::default().files_only(true))
                .allows_descriptor_relative_descent(),
            "kind filters use the listing unless symlink resolution is requested"
        );
        assert!(
            !Walker::new(".")
                .respect_git_ignore(true)
                .allows_descriptor_relative_descent()
        );
        for options in [
            WalkOptions::default().follow_symlinks(true),
            WalkOptions::default().metadata(true),
            WalkOptions::default().resolve_symlink_kind(true),
        ] {
            assert!(
                !Walker::new(".")
                    .options(options)
                    .allows_descriptor_relative_descent()
            );
        }
    }

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

    impl Fixture {
        /// The fixture root spelled the way an absolute pattern must spell it:
        /// `/` separators on every platform, drive letter and all. Built at run
        /// time so the absolute-pattern tests are the same test everywhere
        /// rather than one test per platform.
        fn absolute(&self, suffix: &str) -> String {
            let root = String::from_utf8(glob_path_bytes(&self.root).into_owned())
                .expect("the temporary directory is UTF-8 on a test host");
            format!("{root}{suffix}")
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
            root: std::sync::Arc::from(root),
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
        let (ignores, ignore_errors) = super::IgnoreScope::for_root(walker, backend, &path);
        super::DirectoryTask {
            path: path.clone(),
            open: super::DirectoryOpen::default(),
            depth: 0,
            root: 0,
            ancestors: super::AncestorChain::default(),
            ignores,
            ignore_errors,
        }
    }

    /// `collect` used to keep one Rust call frame per directory. A mock tree
    /// avoids host path-length limits while still reaching past the depth that
    /// previously exhausted the serial walk's stack.
    #[test]
    fn serial_collect_handles_a_deep_directory_chain_without_recursion() {
        const DEPTH: usize = 4_096;

        struct DeepChainBackend {
            root: PathBuf,
        }

        impl super::DirectoryBackend for DeepChainBackend {
            fn read_directory(
                &self,
                path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                listing.clear();
                let depth = path
                    .strip_prefix(&self.root)
                    .expect("walk only reads descendants of its root")
                    .components()
                    .count();
                if depth < DEPTH {
                    listing.push("child".as_ref(), true, false);
                }
                Ok(())
            }
        }

        let backend = DeepChainBackend {
            root: PathBuf::from("/serial-deep-chain"),
        };
        let result = Walker::new(&backend.root)
            .threads(1)
            .options(WalkOptions::default().files_only(true))
            .collect_with(&backend)
            .expect("the serial walker does not consume one call frame per directory");

        assert!(
            result.entries().is_empty(),
            "the mock tree has only directories"
        );
        assert!(result.errors().is_empty());
    }

    /// The explicit task stack must model the old recursive call exactly:
    /// finish one directory's subtree, emit that directory, then continue with
    /// its next sibling in the backend's listing order.
    #[test]
    fn serial_unsorted_directories_are_emitted_after_their_own_subtrees() {
        struct OrderedTreeBackend {
            root: PathBuf,
        }

        impl super::DirectoryBackend for OrderedTreeBackend {
            fn read_directory(
                &self,
                path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                listing.clear();
                let relative = path
                    .strip_prefix(&self.root)
                    .expect("walk only reads descendants of its root");
                match relative.to_str().expect("fixture paths are UTF-8") {
                    "" => {
                        listing.push("b".as_ref(), true, false);
                        listing.push("a".as_ref(), true, false);
                    }
                    "b" => listing.push("f2.txt".as_ref(), false, false),
                    "a" => listing.push("f1.txt".as_ref(), false, false),
                    other => panic!("unexpected directory read: {other}"),
                }
                Ok(())
            }
        }

        let backend = OrderedTreeBackend {
            root: PathBuf::from("/serial-emission-order"),
        };
        let result = Walker::new(&backend.root)
            .threads(1)
            .collect_with(&backend)
            .expect("mock walk succeeds");

        assert_eq!(
            relative_paths(result.entries(), &backend.root),
            [
                PathBuf::from("b/f2.txt"),
                PathBuf::from("b"),
                PathBuf::from("a/f1.txt"),
                PathBuf::from("a"),
            ]
        );
    }

    #[test]
    fn stream_discards_partial_listing_after_a_directory_read_error() {
        let fixture = Fixture::new();
        let missing = fixture.root.join("missing");
        let mut stream = Walker::new(&missing)
            .error_policy(ErrorPolicy::Collect)
            .stream();

        // Model a backend that appended an entry before it reported its error.
        stream
            .listing
            .push(std::ffi::OsStr::new("must-not-leak"), false, false);

        let task = stream
            .pending_directories
            .pop()
            .expect("the root is queued for reading");
        let error = stream
            .prepare_directory(task)
            .expect("the failed root produces an error")
            .expect_err("a missing root cannot yield an entry");
        assert_eq!(error.operation(), "read_dir");
        assert!(stream.listing.entries().is_empty());
        assert!(stream.next().is_none(), "partial entries must not leak");
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
        fn read_directory(
            &self,
            path: &Path,
            follow_symlinks: bool,
            refuse_final_symlink: bool,
            listing: &mut super::Listing,
        ) -> std::io::Result<()> {
            super::StdBackend.read_directory(path, follow_symlinks, refuse_final_symlink, listing)
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

    fn rooted_relative_paths(entries: &[WalkEntry]) -> Vec<(PathBuf, PathBuf)> {
        let mut paths = entries
            .iter()
            .map(|entry| {
                (
                    entry.root().to_path_buf(),
                    entry
                        .path()
                        .strip_prefix(entry.root())
                        .expect("entry is rooted in its declared root")
                        .to_path_buf(),
                )
            })
            .collect::<Vec<_>>();
        paths.sort_unstable();
        paths
    }

    type RootFilterSources = (Vec<Vec<u8>>, Vec<Vec<u8>>);

    fn configured_filter_sources(walker: &Walker) -> Vec<RootFilterSources> {
        walker
            .roots
            .iter()
            .map(|root| {
                (
                    root.includes
                        .iter()
                        .map(|pattern| pattern.source.clone())
                        .collect(),
                    root.excludes
                        .iter()
                        .map(|pattern| pattern.source.clone())
                        .collect(),
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
    fn borrowed_pattern_lists_keep_the_builder_and_filter_every_frontend() {
        let fixture = Fixture::new();
        let alpha = fixture.root.join("alpha");
        let beta = fixture.root.join("beta");
        for root in ["alpha", "beta"] {
            fixture.write(format!("{root}/src/keep.rs"));
            fixture.write(format!("{root}/src/remove.rs"));
            fixture.write(format!("{root}/src/ignore.txt"));
            fixture.write(format!("{root}/.hidden.rs"));
        }

        let mut walker = Walker::new(&alpha)
            .add_root(&beta)
            .expect("valid second root")
            .match_hidden(true)
            .wildcard_mode(WildcardMode::SeparatorCrossing)
            .options(WalkOptions::default().files_only(true).sort(true));

        walker
            .try_include("*.rs")
            .expect("first supplied include is valid");
        let before_bad_include = walker.clone();
        assert!(walker.try_include("[a").is_err());
        assert_eq!(walker.include_sources, before_bad_include.include_sources);
        assert_eq!(walker.exclude_sources, before_bad_include.exclude_sources);
        assert_eq!(
            configured_filter_sources(&walker),
            configured_filter_sources(&before_bad_include),
            "a rejected include must not update any root"
        );
        walker
            .try_include("**/also.rs")
            .expect("a later supplied include still composes");

        walker
            .try_exclude("**/remove.rs")
            .expect("first supplied exclude is valid");
        let before_bad_exclude = walker.clone();
        assert!(walker.try_exclude("[a").is_err());
        assert_eq!(walker.include_sources, before_bad_exclude.include_sources);
        assert_eq!(walker.exclude_sources, before_bad_exclude.exclude_sources);
        assert_eq!(
            configured_filter_sources(&walker),
            configured_filter_sources(&before_bad_exclude),
            "a rejected exclude must not update any root"
        );
        walker
            .try_exclude("**/generated/**")
            .expect("a later supplied exclude still composes");

        let expected = vec![
            (alpha.clone(), PathBuf::from(".hidden.rs")),
            (alpha.clone(), PathBuf::from("src/keep.rs")),
            (beta.clone(), PathBuf::from(".hidden.rs")),
            (beta.clone(), PathBuf::from("src/keep.rs")),
        ];
        for threads in [1, 4] {
            let collected = walker
                .clone()
                .threads(threads)
                .collect()
                .expect("collect succeeds");
            assert_eq!(rooted_relative_paths(collected.entries()), expected);

            let visited = walker
                .clone()
                .threads(threads)
                .visit(|_| Verdict::Keep)
                .expect("visit succeeds");
            assert_eq!(rooted_relative_paths(visited.entries()), expected);
        }
        let streamed = walker
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream succeeds");
        assert_eq!(rooted_relative_paths(&streamed), expected);
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
    fn relative_patterns_that_name_no_candidate_are_rejected_with_guidance() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs");
        fixture.write("src/a.rs");
        fixture.write("src/].rs");
        // Windows normalizes a path component ending in dots, so these two
        // valid matcher strings cannot be represented as fixture paths there.
        #[cfg(not(windows))]
        {
            fixture.write("prefix../bar");
            fixture.write("foo/..suffix/bar");
        }

        for mode in [
            WildcardMode::ComponentScoped,
            WildcardMode::SeparatorCrossing,
        ] {
            for add in [
                |walker: Walker| walker.include("."),
                |walker: Walker| walker.include("./"),
                |walker: Walker| walker.include("src/."),
                |walker: Walker| walker.include("src/./main.rs"),
                |walker: Walker| walker.include("././src/**"),
                |walker: Walker| walker.include("src/../main.rs"),
                |walker: Walker| walker.include("+(dead/../branch)"),
                |walker: Walker| walker.exclude("."),
                |walker: Walker| walker.exclude("./"),
                |walker: Walker| walker.exclude("src/."),
                |walker: Walker| walker.exclude("src/./main.rs"),
                |walker: Walker| walker.exclude("././src/**"),
                |walker: Walker| walker.exclude("src/../main.rs"),
                |walker: Walker| walker.exclude("+(dead/../branch)"),
            ] {
                let error = add(Walker::new(&fixture.root).wildcard_mode(mode))
                    .expect_err("unwalkable relative pattern is refused");
                assert!(
                    error.message().contains("select")
                        || error.message().contains("not normalized")
                        || error.message().starts_with("`..`"),
                    "the rejection explains why the pattern cannot select: {error}"
                );
            }
        }

        // On a relative walk root `/bar` cannot be rewritten as an absolute
        // pattern; on Windows it remains a root-relative empty-leading
        // spelling. Either path must refuse it at pattern-add time.
        for mode in [
            WildcardMode::ComponentScoped,
            WildcardMode::SeparatorCrossing,
        ] {
            assert!(
                Walker::new(".")
                    .wildcard_mode(mode)
                    .include("/bar")
                    .is_err(),
                "include rejects an unselectable leading empty component under {mode:?}"
            );
            assert!(
                Walker::new(".")
                    .wildcard_mode(mode)
                    .exclude("/bar")
                    .is_err(),
                "exclude rejects an unselectable leading empty component under {mode:?}"
            );
        }

        // Dots that are matcher text, rather than slash-delimited path
        // components, remain valid in every mode. In particular this keeps
        // escaped, class and extglob literals out of the path check.
        for pattern in [
            "./src/**",
            "...",
            ".hidden",
            r"\.\.",
            r"src\..\main.rs",
            "[.]",
            "@(..)",
            "src/[[:alpha:]/../].rs",
            "src/[]/../].rs",
            "@(dead/../branch|src/main.rs)",
            "src/[{],a}/../].rs",
            "@(dead/{),x}/../branch|src/main.rs)",
            "prefix@(../bar)",
            "@(foo/..)suffix/bar",
        ] {
            for mode in [
                WildcardMode::ComponentScoped,
                WildcardMode::SeparatorCrossing,
            ] {
                Walker::new(&fixture.root)
                    .wildcard_mode(mode)
                    .include(pattern)
                    .unwrap_or_else(|error| panic!("{pattern} must stay matcher text: {error}"));
            }
        }

        // Braces expose literal path components to the viability analysis, so
        // their dot forms are rejected. An extglob remains one opaque matcher
        // component; it cannot name `.` or `..` as a walk path component and
        // therefore stays valid while selecting no entry.
        let brace_dot_components = "{.,..}";
        assert!(
            Walker::new(&fixture.root)
                .include(brace_dot_components)
                .is_err(),
            "brace-expanded {brace_dot_components:?} is an unwalkable path spelling"
        );
        for pattern in ["@(.)", "@(..)/x", "?(.)/x"] {
            Walker::new(&fixture.root)
                .include(pattern)
                .unwrap_or_else(|error| panic!("{pattern} remains opaque matcher text: {error}"));
        }

        // A dead extglob arm remains matcher syntax rather than brace-expanded
        // walker input. Character classes use the glob parser's POSIX and
        // leading-`]` grammar, so slash or `..` text stays matcher syntax too.
        // Extglobs retain their component-scoped matching rule, so only the
        // separator-crossing walk reads its slash-containing arm as one path.
        for (pattern, matching_paths, component_scoped_paths) in [
            (
                "@(dead/../branch|src/main.rs)",
                &["src/main.rs"][..],
                &[][..],
            ),
            (
                "src/[[:alpha:]/../].rs",
                &["src/a.rs"][..],
                &["src/a.rs"][..],
            ),
            ("src/[]/../].rs", &["src/].rs"][..], &["src/].rs"][..]),
            (
                "src/[{],a}/../].rs",
                &["src/a.rs", "src/].rs"][..],
                &["src/a.rs", "src/].rs"][..],
            ),
            (
                "@(dead/{),x}/../branch|src/main.rs)",
                &["src/main.rs"][..],
                &[][..],
            ),
            ("prefix@(../bar)", &["prefix../bar"][..], &[][..]),
            ("@(foo/..)suffix/bar", &["foo/..suffix/bar"][..], &[][..]),
        ] {
            let matcher = Pattern::compile(pattern, traversal_pattern_options(false))
                .expect("the reviewer regression is valid matcher syntax");
            for matching_path in matching_paths {
                assert!(matcher.is_match(matching_path));
            }
            let matching_paths_on_disk =
                if cfg!(windows) && matches!(pattern, "prefix@(../bar)" | "@(foo/..)suffix/bar") {
                    &[][..]
                } else {
                    matching_paths
                };
            for mode in [
                WildcardMode::ComponentScoped,
                WildcardMode::SeparatorCrossing,
            ] {
                Walker::new(&fixture.root)
                    .wildcard_mode(mode)
                    .exclude(pattern)
                    .expect("the viable group arm is accepted for excludes");
                for threads in [1, 4] {
                    let result = Walker::new(&fixture.root)
                        .wildcard_mode(mode)
                        .threads(threads)
                        .include(pattern)
                        .expect("the viable group arm is accepted")
                        .options(WalkOptions::default().sort(true).files_only(true))
                        .collect()
                        .expect("walk succeeds");
                    let mut expected = if mode == WildcardMode::ComponentScoped {
                        component_scoped_paths
                    } else {
                        matching_paths_on_disk
                    }
                    .iter()
                    .map(PathBuf::from)
                    .collect::<Vec<_>>();
                    expected.sort();
                    assert_eq!(
                        relative_paths(result.entries(), &fixture.root),
                        expected,
                        "{pattern} must keep its viable arm under {mode:?} on {threads} threads"
                    );
                }
                let streamed = Walker::new(&fixture.root)
                    .wildcard_mode(mode)
                    .include(pattern)
                    .expect("the viable group arm is accepted")
                    .options(WalkOptions::default().files_only(true))
                    .stream()
                    .collect::<Result<Vec<_>, _>>()
                    .expect("stream succeeds");
                let mut actual = relative_paths(&streamed, &fixture.root);
                actual.sort();
                let mut expected = if mode == WildcardMode::ComponentScoped {
                    component_scoped_paths
                } else {
                    matching_paths_on_disk
                }
                .iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>();
                expected.sort();
                assert_eq!(actual, expected);
            }
        }

        for pattern in [
            "src/../main.rs",
            "src/{live,dead}/../main.rs",
            "{..,src}",
            "src/{a,..}",
            "{dead/../branch,src/main.rs}",
            "{dead/[}]]/../x,src/main.rs}",
            "@(dead/[)]]/../x|src/main.rs)",
            "{dead/../branch}",
            "@(dead/../branch)",
            "@(dead|src)/../main.rs",
            "src/@(./a.rs)",
            "@(./a.rs)",
            "{./a.rs}",
            "src/?(./a.rs)",
            "src/*(./a.rs)",
            "src/?(./a.rs)/bar",
            "src/*(./a.rs)/bar",
            "src/?()/bar",
            "src/*()/bar",
            "?()/bar",
            "*()/bar",
            "src//bar",
            "src/{}/bar",
            "src/{,./a.rs}/bar",
        ] {
            assert!(
                Walker::new(&fixture.root).include(pattern).is_err(),
                "a parser-top-level `..` component stays invalid: {pattern}"
            );
        }
        for mode in [
            WildcardMode::ComponentScoped,
            WildcardMode::SeparatorCrossing,
        ] {
            for pattern in [
                "{..,src}",
                "src/{a,..}",
                "{dead/../branch,src/main.rs}",
                "{dead/[}]]/../x,src/main.rs}",
                "@(dead/[)]]/../x|src/main.rs)",
                "{dead/../branch}",
                "@(dead/../branch)",
                "@(dead|src)/../main.rs",
                "src/@(./a.rs)",
                "@(./a.rs)",
                "{./a.rs}",
                "src/?(./a.rs)",
                "src/*(./a.rs)",
                "src/?(./a.rs)/bar",
                "src/*(./a.rs)/bar",
                "src/?()/bar",
                "src/*()/bar",
                "?()/bar",
                "*()/bar",
                "src//bar",
                "src/{}/bar",
                "src/{,./a.rs}/bar",
            ] {
                assert!(
                    Walker::new(&fixture.root)
                        .wildcard_mode(mode)
                        .include(pattern)
                        .is_err(),
                    "include rejects parser-top-level `..` under {mode:?}: {pattern}"
                );
                assert!(
                    Walker::new(&fixture.root)
                        .wildcard_mode(mode)
                        .exclude(pattern)
                        .is_err(),
                    "exclude rejects parser-top-level `..` under {mode:?}: {pattern}"
                );
            }
        }
    }

    #[test]
    fn relative_pattern_errors_keep_determinate_component_offsets() {
        for (pattern, offset) in [
            ("src/../main.rs", 4),
            ("src/./main.rs", 4),
            ("src/.", 4),
            ("é/../main.rs", 3),
            ("src/@(./a.rs)", 6),
            ("{src}/../main.rs", 6),
            ("{a,b}/../main.rs", 6),
            ("{a,..}/main.rs", 3),
            ("src/{x}/.", 8),
        ] {
            for mode in [
                WildcardMode::ComponentScoped,
                WildcardMode::SeparatorCrossing,
            ] {
                for error in [
                    Walker::new(".")
                        .wildcard_mode(mode)
                        .include(pattern)
                        .expect_err("unwalkable include is refused"),
                    Walker::new(".")
                        .wildcard_mode(mode)
                        .exclude(pattern)
                        .expect_err("unwalkable exclude is refused"),
                ] {
                    assert_eq!(
                        error.offset(),
                        offset,
                        "{pattern} keeps its component offset under {mode:?}"
                    );
                }
            }
        }

        for pattern in [r"src/\./main.rs", r"src/\../main.rs", "src/@(..)suffix"] {
            Walker::new(".")
                .include(pattern)
                .unwrap_or_else(|error| panic!("{pattern} remains matcher text: {error}"));
        }
    }

    #[test]
    fn absolute_pattern_errors_keep_caller_source_offsets() {
        let options = traversal_pattern_options(false);
        for (pattern, root, syntax, offset, invalid) in [
            (
                "/repo/src/[",
                "/repo",
                super::absolute::Syntax::Posix,
                10,
                b'[',
            ),
            (
                "/repo/src/./x",
                "/repo",
                super::absolute::Syntax::Posix,
                10,
                b'.',
            ),
            (
                "/repo/a{b,[}",
                "/repo",
                super::absolute::Syntax::Posix,
                10,
                b'[',
            ),
            (
                "/repo/src/../x",
                "/repo",
                super::absolute::Syntax::Posix,
                10,
                b'.',
            ),
            (
                "/repo/src/{a,..}",
                "/repo",
                super::absolute::Syntax::Posix,
                13,
                b'.',
            ),
            (
                "//?/C:/repo/src/[",
                "//?/C:/repo",
                super::absolute::Syntax::Windows,
                16,
                b'[',
            ),
            (
                "//?/UNC/server/share/src/[",
                "//?/UNC/server/share",
                super::absolute::Syntax::Windows,
                25,
                b'[',
            ),
        ] {
            let error = super::walker_pattern_for_root(
                pattern.as_bytes(),
                root.as_bytes(),
                syntax,
                options,
            )
            .expect_err("the absolute walker pattern is invalid");
            assert_eq!(error.offset(), offset, "{pattern}: {error}");
            assert_eq!(
                pattern.as_bytes().get(error.offset()),
                Some(&invalid),
                "{pattern} points into the caller's source: {error}"
            );
        }
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
    fn walk_entries_are_cloneable_with_their_metadata_snapshot() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("five.bin"), b"12345").expect("write clone fixture");

        let result = Walker::new(&fixture.root)
            .options(WalkOptions::default().sort(true).metadata(true))
            .collect()
            .expect("walk succeeds");
        let entry = result
            .entries()
            .iter()
            .find(|entry| entry.path().ends_with("five.bin"))
            .expect("fixture file is returned");
        let clone = entry.clone();

        assert_eq!(clone.path(), entry.path());
        assert_eq!(clone.root(), entry.root());
        assert_eq!(clone.kind(), entry.kind());
        assert_eq!(clone.depth(), entry.depth());
        assert_eq!(clone.metadata().map(fs::Metadata::len), Some(5));
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
    fn subtree_walk_inherits_repository_ignore_rules_excludes_and_config() {
        let fixture = Fixture::new();
        for path in [
            "src/debug.LOG",
            "src/secret.txt",
            "src/ignored-by-info",
            "src/deep/trace.log",
            "src/kept.txt",
        ] {
            fixture.write(path);
        }
        let initialized = git_command()
            .args(["init", "--quiet"])
            .current_dir(&fixture.root)
            .status()
            .expect("initialize Git fixture");
        assert!(initialized.success());
        let configured = git_command()
            .args(["config", "core.ignoreCase", "true"])
            .current_dir(&fixture.root)
            .status()
            .expect("configure Git fixture");
        assert!(configured.success());
        fs::write(fixture.root.join(".gitignore"), b"*.log\n/src/secret.txt\n")
            .expect("write repository ignore rules");
        fs::write(fixture.root.join(".git/info/exclude"), b"ignored-by-info\n")
            .expect("write repository excludes");

        let root = fixture.root.join("src");
        let walked = Walker::new(&root)
            .respect_git_ignore(true)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("subtree walk succeeds");
        let paths = relative_paths(walked.entries(), &root);
        for candidate in [
            "src/debug.LOG",
            "src/secret.txt",
            "src/ignored-by-info",
            "src/deep/trace.log",
        ] {
            let relative = Path::new(candidate)
                .strip_prefix("src")
                .expect("candidate is below subtree root")
                .to_path_buf();
            assert_eq!(
                !paths.contains(&relative),
                git_check_ignore(&fixture.root, candidate),
                "subtree walk must agree with Git for {candidate}",
            );
        }
        assert!(paths.contains(&PathBuf::from("kept.txt")));
    }

    #[test]
    fn an_explicitly_ignored_walk_root_is_still_entered() {
        let fixture = Fixture::new();
        fixture.write("src2/kept.txt");
        fs::create_dir(fixture.root.join(".git")).expect("create repository metadata directory");
        fs::write(fixture.root.join(".gitignore"), b"/src2/\n").expect("write root ignore rule");

        let root = fixture.root.join("src2");
        let walked = Walker::new(&root)
            .respect_git_ignore(true)
            .collect()
            .expect("explicit subtree walk succeeds");
        let paths = relative_paths(walked.entries(), &root);
        assert!(paths.contains(&PathBuf::from("kept.txt")));
    }

    #[test]
    fn a_dot_segment_in_a_subtree_root_preserves_inherited_anchors() {
        let fixture = Fixture::new();
        for path in ["src/anch.txt", "src/rel.txt", "src/kept.txt"] {
            fixture.write(path);
        }
        fs::create_dir(fixture.root.join(".git")).expect("create repository metadata directory");
        fs::write(
            fixture.root.join(".gitignore"),
            b"/src/anch.txt\nsrc/rel.txt\n",
        )
        .expect("write anchored ignore rules");

        let root = fixture.root.join(".").join("src");
        let walked = Walker::new(&root)
            .respect_git_ignore(true)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("dotted subtree walk succeeds");
        assert_eq!(
            relative_paths(walked.entries(), &root),
            vec![PathBuf::from("kept.txt")]
        );
    }

    #[test]
    fn relative_root_spellings_inherit_repository_ignore_rules() {
        if let Some(root) = std::env::var_os(RELATIVE_ROOT_IGNORE_FIXTURE) {
            let root = PathBuf::from(root);
            let spelling = std::env::var_os(RELATIVE_ROOT_SPELLING)
                .expect("child receives its relative root spelling");
            let spelling = PathBuf::from(spelling);
            let walked = Walker::new(&spelling)
                .respect_git_ignore(true)
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("relative-root walk succeeds");
            let expected = if matches!(spelling.to_str(), Some("sub/" | "./sub/")) {
                vec![PathBuf::from("keep.rs")]
            } else {
                vec![PathBuf::from("keep.rs"), PathBuf::from("sub/keep.rs")]
            };
            assert_eq!(
                relative_paths(walked.entries(), &spelling),
                expected,
                "root spelling {spelling:?} must inherit repository rules from {root:?}",
            );
            return;
        }

        let fixture = Fixture::new();
        fixture.write("src/debug.log");
        fixture.write("src/secret.txt");
        fixture.write("src/keep.rs");
        fixture.write("src/sub/secret.txt");
        fixture.write("src/sub/keep.rs");
        fs::create_dir(fixture.root.join(".git")).expect("create repository metadata directory");
        fs::write(
            fixture.root.join(".gitignore"),
            b"*.log\n/src/secret.txt\n/src/sub/secret.txt\n",
        )
        .expect("write repository ignore rules");

        for (working_directory, spelling) in [
            (fixture.root.join("src"), "."),
            (fixture.root.join("src"), "./"),
            (fixture.root.clone(), "src"),
            (fixture.root.clone(), "src/"),
            (fixture.root.clone(), "./src/"),
            (fixture.root.join("src"), "sub/"),
            (fixture.root.join("src"), "./sub/"),
            (fixture.root.join("src"), "../src/"),
        ] {
            let status = Command::new(std::env::current_exe().expect("locate test binary"))
                .args([
                    "tests::relative_root_spellings_inherit_repository_ignore_rules",
                    "--exact",
                ])
                .current_dir(working_directory)
                .env(RELATIVE_ROOT_IGNORE_FIXTURE, &fixture.root)
                .env(RELATIVE_ROOT_SPELLING, spelling)
                .status()
                .expect("run isolated relative-root regression test");
            assert!(
                status.success(),
                "relative-root child failed for {spelling:?}: {status}"
            );
        }
    }

    #[test]
    fn parent_components_in_relative_roots_keep_ignore_anchors() {
        if let Some(root) = std::env::var_os(PARENT_ROOT_IGNORE_FIXTURE) {
            let root = PathBuf::from(root);
            let spelling = std::env::var_os(RELATIVE_ROOT_SPELLING)
                .expect("child receives its relative root spelling");
            let spelling = PathBuf::from(spelling);
            let walked = Walker::new(&spelling)
                .respect_git_ignore(true)
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("parent-relative-root walk succeeds");
            let paths = relative_paths(walked.entries(), &spelling);
            let (visible, hidden): (&[&str], &[&str]) = match spelling.to_str() {
                Some("..") => (
                    &["keep.rs", "local.txt", "sub/keep.rs"],
                    &["secret.txt", "sub/deep.txt", "sub/local.txt"],
                ),
                Some("../src/sub") => (&["keep.rs"], &["deep.txt", "local.txt"]),
                Some("../..") => (
                    &["src/keep.rs", "src/local.txt", "src/sub/keep.rs"],
                    &["src/secret.txt", "src/sub/deep.txt", "src/sub/local.txt"],
                ),
                other => panic!("unexpected parent-relative root {other:?}"),
            };
            for path in visible {
                assert!(
                    paths.contains(&PathBuf::from(path)),
                    "root spelling {spelling:?} must keep {path} under {root:?}: {paths:?}"
                );
            }
            for path in hidden {
                assert!(
                    !paths.contains(&PathBuf::from(path)),
                    "root spelling {spelling:?} must ignore {path} under {root:?}: {paths:?}"
                );
            }
            return;
        }

        let fixture = Fixture::new();
        for path in [
            "src/secret.txt",
            "src/local.txt",
            "src/keep.rs",
            "src/sub/deep.txt",
            "src/sub/local.txt",
            "src/sub/keep.rs",
        ] {
            fixture.write(path);
        }
        fs::create_dir(fixture.root.join(".git")).expect("create repository metadata directory");
        fs::write(
            fixture.root.join(".gitignore"),
            b"/src/secret.txt\n/src/sub/deep.txt\n",
        )
        .expect("write repository ignore rules");
        fs::write(fixture.root.join("src/sub/.gitignore"), b"local.txt\n")
            .expect("write subtree ignore rule");

        for (working_directory, spelling) in [
            (fixture.root.join("src/sub"), ".."),
            (fixture.root.join("src"), "../src/sub"),
            (fixture.root.join("src/sub"), "../.."),
        ] {
            let status = Command::new(std::env::current_exe().expect("locate test binary"))
                .args([
                    "tests::parent_components_in_relative_roots_keep_ignore_anchors",
                    "--exact",
                ])
                .current_dir(working_directory)
                .env(PARENT_ROOT_IGNORE_FIXTURE, &fixture.root)
                .env(RELATIVE_ROOT_SPELLING, spelling)
                .status()
                .expect("run isolated parent-relative-root regression test");
            assert!(
                status.success(),
                "parent-relative-root child failed for {spelling:?}: {status}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn parent_components_after_symlinks_use_the_physical_repository() {
        if std::env::var_os(SYMLINK_PARENT_ROOT_FIXTURE).is_some() {
            let root = Path::new("portal/..");
            let walked = Walker::new(root)
                .respect_git_ignore(true)
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("symlink-parent-root walk succeeds");
            assert_eq!(
                relative_paths(walked.entries(), root),
                vec![PathBuf::from("keep.txt")],
                "ignore discovery must resolve `..` after following `portal`"
            );
            return;
        }

        let fixture = Fixture::new();
        fixture.write("physical/repository/sub/secret.txt");
        fixture.write("physical/repository/sub/keep.txt");
        fs::create_dir_all(fixture.root.join("physical/repository/sub/nested"))
            .expect("create symlink target directory");
        fs::create_dir(fixture.root.join("physical/repository/.git"))
            .expect("create physical repository metadata directory");
        fs::write(
            fixture.root.join("physical/repository/.gitignore"),
            b"/sub/secret.txt\n",
        )
        .expect("write physical repository ignore rule");

        fs::create_dir_all(fixture.root.join("lexical/work"))
            .expect("create lexical working directory");
        fs::create_dir(fixture.root.join("lexical/.git"))
            .expect("create lexical repository metadata directory");
        fs::write(fixture.root.join("lexical/.gitignore"), b"/work/keep.txt\n")
            .expect("write lexical repository ignore rule");
        std::os::unix::fs::symlink(
            fixture.root.join("physical/repository/sub/nested"),
            fixture.root.join("lexical/work/portal"),
        )
        .expect("create directory symlink");

        let status = Command::new(std::env::current_exe().expect("locate test binary"))
            .args([
                "tests::parent_components_after_symlinks_use_the_physical_repository",
                "--exact",
            ])
            .current_dir(fixture.root.join("lexical/work"))
            .env(SYMLINK_PARENT_ROOT_FIXTURE, "1")
            .status()
            .expect("run isolated symlink-parent-root regression test");
        assert!(
            status.success(),
            "symlink-parent-root child failed: {status}"
        );
    }

    /// Starts a Git oracle process that is independent of the developer's
    /// global and system configuration, just like the corpus harness.
    fn git_command() -> Command {
        let mut command = Command::new("git");
        command
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    fn git_check_ignore(root: &Path, candidate: &str) -> bool {
        let status = git_command()
            .args(["check-ignore", "--no-index", "--quiet", "--", candidate])
            .current_dir(root)
            .status()
            .expect("run Git ignore oracle");
        match status.code() {
            Some(0) => true,
            Some(1) => false,
            other => panic!("git check-ignore failed with {other:?}"),
        }
    }

    fn git_config_bool(root: &Path, key: &str) -> bool {
        let output = git_command()
            .args(["config", "--type=bool", "--get", key])
            .current_dir(root)
            .output()
            .expect("run Git config boolean oracle");
        assert!(
            output.status.success(),
            "Git config boolean oracle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        match String::from_utf8(output.stdout)
            .expect("Git config boolean output is UTF-8")
            .trim()
        {
            "true" => true,
            "false" => false,
            value => panic!("unexpected Git config boolean output: {value:?}"),
        }
    }

    fn git_config_file_bool(config: &Path, key: &str) -> bool {
        let output = git_command()
            .args(["config", "--file"])
            .arg(config)
            .args(["--type=bool", "--get", key])
            .output()
            .expect("run Git config file boolean oracle");
        assert!(
            output.status.success(),
            "Git config file boolean oracle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        match String::from_utf8(output.stdout)
            .expect("Git config boolean output is UTF-8")
            .trim()
        {
            "true" => true,
            "false" => false,
            value => panic!("unexpected Git config boolean output: {value:?}"),
        }
    }

    #[test]
    fn git_oracle_ignores_an_inherited_global_excludes_file() {
        if let Some(root) = std::env::var_os(HOSTILE_GIT_CONFIG_FIXTURE) {
            let root = PathBuf::from(root);
            assert!(
                !git_check_ignore(&root, "unignored.log"),
                "the oracle must not read the hostile inherited excludes file"
            );
            return;
        }

        let fixture = Fixture::new();
        fixture.write("unignored.log");
        let initialized = git_command()
            .args(["init", "--quiet"])
            .current_dir(&fixture.root)
            .status()
            .expect("initialize hostile-config Git fixture");
        assert!(initialized.success());
        let excludes = fixture.root.join("hostile-excludes");
        fs::write(&excludes, b"*.log\n").expect("write hostile global excludes file");
        let config = fixture.root.join("hostile-gitconfig");
        let excludes = excludes.display().to_string();
        let excludes = excludes.replace('\\', "\\\\").replace('"', "\\\"");
        fs::write(
            &config,
            format!("[core]\n\texcludesFile = \"{excludes}\"\n"),
        )
        .expect("write hostile global Git config");

        // Environment mutation is process-global, so run a second copy of
        // this one test instead. It inherits the hostile config just as a
        // developer's test process would, while Git itself is still spawned
        // exclusively through `git_command`.
        let status = Command::new(std::env::current_exe().expect("locate test binary"))
            .args([
                "tests::git_oracle_ignores_an_inherited_global_excludes_file",
                "--exact",
            ])
            .env(HOSTILE_GIT_CONFIG_FIXTURE, &fixture.root)
            .env("GIT_CONFIG_GLOBAL", &config)
            .status()
            .expect("run isolated Git oracle regression test");
        assert!(
            status.success(),
            "hostile-config child test failed: {status}"
        );
    }

    #[test]
    fn repository_ignorecase_matches_git_for_rules_negation_and_anchors() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.root.join(".git")).expect("create Git metadata");
        fs::write(
            fixture.root.join(".git/config"),
            b"[CoRe]\nignoreCase = YeS\n",
        )
        .expect("write local config");
        fs::write(
            fixture.root.join(".gitignore"),
            b"Build.LOG\nDist/\n!Kept.LOG\n/src/Anchored.LOG\n",
        )
        .expect("write ignore rules");
        for path in [
            "BUILD.log",
            "DIST/deep.txt",
            "kept.log",
            "SRC/ANCHORED.log",
            "other/ANCHORED.log",
        ] {
            fixture.write(path);
        }

        // The Git oracle is configured exactly as the repository is. This
        // catches the rule compilation, not merely a hand-written expectation.
        let initialized = git_command()
            .args(["init", "--quiet"])
            .current_dir(&fixture.root)
            .status()
            .expect("initialize Git oracle");
        assert!(initialized.success());
        let configured = git_command()
            .args(["config", "core.ignoreCase", "true"])
            .current_dir(&fixture.root)
            .status()
            .expect("configure Git oracle");
        assert!(configured.success());

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
        let streamed = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect("stream succeeds");
        for paths in [
            relative_paths(serial.entries(), &fixture.root),
            relative_paths(parallel.entries(), &fixture.root),
            relative_paths(&streamed, &fixture.root),
        ] {
            for candidate in [
                "BUILD.log",
                "DIST/deep.txt",
                "kept.log",
                "SRC/ANCHORED.log",
                "other/ANCHORED.log",
            ] {
                assert_eq!(
                    !paths.contains(&PathBuf::from(candidate)),
                    git_check_ignore(&fixture.root, candidate),
                    "Ferralk must agree with Git for {candidate}",
                );
            }
        }
    }

    #[test]
    fn non_utf8_unrelated_config_value_does_not_hide_repository_ignorecase() {
        let fixture = Fixture::new();
        fixture.write("BUILD.log");
        fs::write(fixture.root.join(".gitignore"), b"build.log\n").expect("write rule");
        let initialized = git_command()
            .args(["init", "--quiet"])
            .current_dir(&fixture.root)
            .status()
            .expect("initialize Git oracle");
        assert!(initialized.success());
        fs::write(
            fixture.root.join(".git/config"),
            b"[user]\nname = Jos\xe9\n[core]\nignorecase = true\n",
        )
        .expect("write Latin-1 local config");

        assert!(git_config_bool(&fixture.root, "core.ignoreCase"));
        assert!(git_check_ignore(&fixture.root, "BUILD.log"));
        let walked = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .collect()
            .expect("walk with non-UTF-8 config");
        assert!(
            !relative_paths(walked.entries(), &fixture.root).contains(&PathBuf::from("BUILD.log")),
            "Ferralk must retain core.ignoreCase and agree with Git"
        );
    }

    #[test]
    fn explicit_ignorecase_false_and_walker_override_take_precedence() {
        let fixture = Fixture::new();
        fixture.write("BUILD.log");
        fs::create_dir_all(fixture.root.join(".git")).expect("create Git metadata");
        fs::write(
            fixture.root.join(".git/config"),
            b"[core]\nignorecase = true\nIGNORECASE = off\n",
        )
        .expect("write last-value local config");
        fs::write(fixture.root.join(".gitignore"), b"build.log\n").expect("write rule");

        let local_false = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .collect()
            .expect("walk local false");
        assert!(
            relative_paths(local_false.entries(), &fixture.root)
                .contains(&PathBuf::from("BUILD.log"))
        );

        let override_true = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .git_ignore_case(true)
            .collect()
            .expect("walk override true");
        assert!(
            !relative_paths(override_true.entries(), &fixture.root)
                .contains(&PathBuf::from("BUILD.log"))
        );

        let cleared = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .git_ignore_case(true)
            .clear_git_ignore_case()
            .collect()
            .expect("cleared override resumes local config");
        assert!(
            relative_paths(cleared.entries(), &fixture.root).contains(&PathBuf::from("BUILD.log")),
            "clearing an override restores the repository-local false value"
        );
    }

    #[test]
    fn numeric_and_empty_ignorecase_values_match_the_git_oracle() {
        let fixture = Fixture::new();
        fixture.write("BUILD.log");
        let initialized = git_command()
            .args(["init", "--quiet"])
            .current_dir(&fixture.root)
            .status()
            .expect("initialize Git oracle");
        assert!(initialized.success());
        fs::write(fixture.root.join(".gitignore"), b"build.log\n").expect("write rule");
        let config = fixture.root.join(".git/config");

        for (value, expected) in [
            ("", false),
            ("\"\"", false),
            ("+0", false),
            ("-0", false),
            ("+2", true),
            ("-7", true),
            ("  +2  # whitespace and comment", true),
        ] {
            fs::write(
                &config,
                format!("[core]\nignoreCase = {}\nIGNORECASE = {value}\n", !expected),
            )
            .expect("write duplicate config");
            assert_eq!(
                git_config_bool(&fixture.root, "core.ignoreCase"),
                expected,
                "Git must accept {value:?}",
            );
            let walked = Walker::new(&fixture.root)
                .respect_git_ignore(true)
                .collect()
                .expect("walk with Git boolean value");
            assert_eq!(
                !relative_paths(walked.entries(), &fixture.root)
                    .contains(&PathBuf::from("BUILD.log")),
                expected,
                "the later {value:?} value must override the earlier opposite value",
            );
        }

        fs::write(&config, b"[core]\nignoreCase = false\nIGNORECASE\n")
            .expect("write bare boolean config");
        assert!(git_config_bool(&fixture.root, "core.ignoreCase"));
        let bare_true = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .collect()
            .expect("walk with bare true");
        assert!(
            !relative_paths(bare_true.entries(), &fixture.root)
                .contains(&PathBuf::from("BUILD.log")),
            "a bare key must retain Git's true behavior"
        );

        fs::write(
            &config,
            b"[core]\nignoreCase = false\nIGNORECASE = t\\\nr\\\nue\n\
              [core \"unrelated\"]\nignoreCase = false\n",
        )
        .expect("write continued and subsection config");
        assert!(git_config_bool(&fixture.root, "core.ignoreCase"));
        let continued_true = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .collect()
            .expect("walk with continued boolean config");
        assert!(
            !relative_paths(continued_true.entries(), &fixture.root)
                .contains(&PathBuf::from("BUILD.log")),
            "a later subsection must not override the top-level continued true value"
        );
    }

    #[test]
    fn case_variant_ignore_file_follows_the_filesystem_canonical_open() {
        let fixture = Fixture::new();
        fixture.write("build.log");
        fs::write(fixture.root.join(".GITIGNORE"), b"build.log\n")
            .expect("write case variant ignore file");
        let canonical_open_resolves = fs::read(fixture.root.join(".gitignore")).is_ok();

        let walked = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");
        let paths = relative_paths(walked.entries(), &fixture.root);
        assert!(paths.contains(&PathBuf::from(".GITIGNORE")));
        assert_eq!(
            !paths.contains(&PathBuf::from("build.log")),
            canonical_open_resolves,
            "a case variant is a rule file only if Git's canonical open resolves it"
        );
    }

    #[test]
    fn multi_root_walks_keep_each_repository_ignorecase_setting() {
        let fixture = Fixture::new();
        let nested = fixture.root.join("nested");
        fixture.write("nested/BUILD.log");
        fs::create_dir_all(fixture.root.join(".git")).expect("create outer Git metadata");
        fs::create_dir_all(nested.join(".git")).expect("create nested Git metadata");
        fs::write(
            fixture.root.join(".git/config"),
            b"[core]\nignorecase = true\n",
        )
        .expect("write outer config");
        fs::write(nested.join(".git/config"), b"[core]\nignorecase = false\n")
            .expect("write nested config");
        fs::write(fixture.root.join(".gitignore"), b"build.log\n").expect("write outer rule");
        fs::write(nested.join(".gitignore"), b"build.log\n").expect("write nested rule");

        let walked = Walker::new(&fixture.root)
            .add_root(&nested)
            .expect("add nested root")
            .add_root(&nested)
            .expect("add duplicate nested root")
            .respect_git_ignore(true)
            .collect()
            .expect("multi-root walk succeeds");
        let emitted = walked
            .entries()
            .iter()
            .filter(|entry| entry.path() == nested.join("BUILD.log"))
            .count();
        // The outer root folds and suppresses this entry. Each independent
        // nested root uses its explicit false setting and emits one copy.
        assert_eq!(emitted, 2);
    }

    #[test]
    fn try_add_root_keeps_the_builder_after_a_rejected_root() {
        let fixture = Fixture::new();
        let first_extra = fixture.root.clone();
        let last_extra = fixture.root.clone();
        let mut walker = Walker::new(&fixture.root)
            .include(fixture.absolute("/**"))
            .expect("absolute pattern applies to the first absolute root");

        walker
            .try_add_root(&first_extra)
            .expect("the absolute pattern applies to an equal root");
        let error = walker
            .try_add_root("relative-root")
            .expect_err("an absolute pattern needs an absolute added root");
        assert_eq!(
            error.message(),
            "an absolute pattern needs an absolute walk root"
        );
        assert_eq!(
            walker.roots().collect::<Vec<_>>(),
            vec![fixture.root.as_path(), first_extra.as_path()],
            "a rejected borrowed addition cannot partially append a root"
        );
        walker
            .try_add_root(&last_extra)
            .expect("the retained builder can consider later roots");
        assert_eq!(
            walker.roots().collect::<Vec<_>>(),
            vec![
                fixture.root.as_path(),
                first_extra.as_path(),
                last_extra.as_path(),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn repository_precomposeunicode_matches_the_macos_git_oracle() {
        let fixture = Fixture::new();
        let decomposed = "cafe\u{301}.txt";
        fs::write(fixture.root.join(".gitignore"), "caf\u{e9}.txt\n").expect("write NFC rule");
        fixture.write(decomposed);
        let initialized = git_command()
            .args(["init", "--quiet"])
            .current_dir(&fixture.root)
            .status()
            .expect("initialize Git oracle");
        assert!(initialized.success());
        let config = fixture.root.join(".git/config");
        fs::write(
            &config,
            b"[core]\nprecomposeUnicode = false\nPRECOMPOSEUNICODE = t\\\nr\\\nue\n\
              [core \"unrelated\"]\nprecomposeUnicode = false\n",
        )
        .expect("write continued precompose Unicode config");
        assert!(git_config_bool(&fixture.root, "core.precomposeUnicode"));

        assert!(git_check_ignore(&fixture.root, decomposed));
        let enabled = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .collect()
            .expect("walk with NFC adaptation");
        assert!(
            !relative_paths(enabled.entries(), &fixture.root).contains(&PathBuf::from(decomposed))
        );

        let forced_false = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .git_precompose_unicode(false)
            .collect()
            .expect("walk with explicit false override");
        assert!(
            relative_paths(forced_false.entries(), &fixture.root)
                .contains(&PathBuf::from(decomposed))
        );

        fs::write(
            &config,
            b"[core]\nprecomposeUnicode = \"\"\n\
              [core \"unrelated\"]\nprecomposeUnicode = true\n",
        )
        .expect("write empty precompose Unicode config");
        assert!(!git_config_bool(&fixture.root, "core.precomposeUnicode"));
        assert!(!git_check_ignore(&fixture.root, decomposed));
        let disabled = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .collect()
            .expect("walk without NFC adaptation");
        assert!(
            relative_paths(disabled.entries(), &fixture.root).contains(&PathBuf::from(decomposed))
        );

        let forced_true = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .git_precompose_unicode(true)
            .collect()
            .expect("walk with explicit true override");
        assert!(
            !relative_paths(forced_true.entries(), &fixture.root)
                .contains(&PathBuf::from(decomposed))
        );

        let cleared = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .git_precompose_unicode(true)
            .clear_git_precompose_unicode()
            .collect()
            .expect("cleared override resumes local config");
        assert!(
            relative_paths(cleared.entries(), &fixture.root).contains(&PathBuf::from(decomposed)),
            "clearing an override restores the repository-local false value"
        );
    }

    #[test]
    fn clearing_git_adaptation_overrides_restores_the_unset_state() {
        let walker = Walker::new("workspace")
            .git_ignore_case(true)
            .clear_git_ignore_case()
            .git_precompose_unicode(true)
            .clear_git_precompose_unicode();
        assert_eq!(walker.git_ignore_case, None);
        assert_eq!(walker.git_precompose_unicode, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_walk_gitignore_matches_non_utf8_file_names_on_linux() {
        use std::os::unix::ffi::OsStringExt;

        let fixture = Fixture::new();
        let name = std::ffi::OsString::from_vec(b"\xE9latin1.txt".to_vec());
        fs::write(fixture.root.join(".gitignore"), b"\xE9latin1.txt\n")
            .expect("write byte-pattern gitignore");
        fixture.write(Path::new(&name));

        let ignored = fixture.root.join(&name);
        let result = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .collect()
            .expect("walk succeeds");
        assert!(
            !result
                .entries()
                .iter()
                .any(|entry| entry.path() == ignored.as_path()),
            "the byte-pattern rule has to hide its byte-named file"
        );
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

    #[cfg(unix)]
    #[test]
    fn symlinked_in_tree_ignore_files_do_not_apply_but_repository_excludes_can_follow_links() {
        use std::os::unix::fs::symlink;

        for ignore_file in [".gitignore", ".ignore"] {
            let fixture = Fixture::new();
            fixture.write("linked.tmp");
            fs::write(fixture.root.join("rules-source"), b"linked.tmp\n")
                .expect("write linked ignore source");
            symlink("rules-source", fixture.root.join(ignore_file))
                .expect("create linked in-tree ignore file");

            let result = Walker::new(&fixture.root)
                .respect_git_ignore(true)
                .options(WalkOptions::default().sort(true))
                .collect()
                .expect("walk succeeds when a linked ignore file is refused");
            assert!(
                relative_paths(result.entries(), &fixture.root)
                    .contains(&PathBuf::from("linked.tmp")),
                "{ignore_file} must not redirect in-tree rules through its target"
            );
        }

        let fixture = Fixture::new();
        fixture.write("repository-only.tmp");
        fs::write(fixture.root.join("rules-source"), b"repository-only.tmp\n")
            .expect("write repository exclude source");
        fs::create_dir_all(fixture.root.join(".git/info")).expect("create git info directory");
        symlink("../../rules-source", fixture.root.join(".git/info/exclude"))
            .expect("link repository exclude");

        let result = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .collect()
            .expect("walk succeeds when repository exclude is linked");
        assert!(
            !relative_paths(result.entries(), &fixture.root)
                .contains(&PathBuf::from("repository-only.tmp")),
            ".git/info/exclude remains allowed to follow links"
        );
    }

    #[test]
    fn linked_worktree_and_submodule_gitdir_pointers_read_the_right_info_exclude() {
        let fixture = Fixture::new();

        // This is the shape `git worktree add` writes: the checkout's `.git`
        // points at a private worktree directory, whose `commondir` points
        // back at the main repository's `.git` directory.
        let worktree = fixture.root.join("linked-worktree");
        let common_git = fixture.root.join("main/.git");
        let private_git = common_git.join("worktrees/linked-worktree");
        fs::create_dir_all(private_git.join("refs")).expect("create private git directory");
        fs::create_dir_all(common_git.join("info")).expect("create common git info");
        fs::write(common_git.join("info/exclude"), b"worktree-secret.txt\n")
            .expect("write common exclude");
        fs::write(private_git.join("commondir"), b"../..\n").expect("write commondir");
        fs::create_dir_all(&worktree).expect("create linked worktree");
        fs::write(
            worktree.join(".git"),
            b"gitdir: ../main/.git/worktrees/linked-worktree\n",
        )
        .expect("write linked-worktree pointer");
        fs::write(worktree.join("worktree-secret.txt"), b"fixture")
            .expect("write linked-worktree secret");

        let worktree_result = Walker::new(&worktree)
            .respect_git_ignore(true)
            .collect()
            .expect("walk linked worktree");
        assert!(
            !relative_paths(worktree_result.entries(), &worktree)
                .contains(&PathBuf::from("worktree-secret.txt")),
            "the linked worktree must use its common info/exclude"
        );

        // A submodule has the same pointer-file form but normally points
        // directly at `.git/modules/<name>` with no `commondir` file.
        let submodule = fixture.root.join("super/dependency");
        let submodule_git = fixture.root.join("super/.git/modules/dependency");
        fs::create_dir_all(submodule_git.join("info")).expect("create submodule git info");
        fs::write(
            submodule_git.join("info/exclude"),
            b"submodule-secret.txt\n",
        )
        .expect("write submodule exclude");
        fs::create_dir_all(&submodule).expect("create submodule checkout");
        fs::write(
            submodule.join(".git"),
            b"gitdir: ../.git/modules/dependency\n",
        )
        .expect("write submodule pointer");
        fs::write(submodule.join("submodule-secret.txt"), b"fixture")
            .expect("write submodule secret");

        let submodule_result = Walker::new(&submodule)
            .respect_git_ignore(true)
            .collect()
            .expect("walk submodule checkout");
        assert!(
            !relative_paths(submodule_result.entries(), &submodule)
                .contains(&PathBuf::from("submodule-secret.txt")),
            "the submodule pointer must use its own info/exclude"
        );
    }

    #[test]
    fn linked_worktree_config_overrides_the_common_repository_setting() {
        let fixture = Fixture::new();
        let checkout = fixture.root.join("linked-worktree");
        let common_git = fixture.root.join("main/.git");
        let private_git = common_git.join("worktrees/linked-worktree");
        fs::create_dir_all(&checkout).expect("create linked checkout");
        fs::create_dir_all(&private_git).expect("create private Git directory");
        fs::write(private_git.join("commondir"), b"../..\n").expect("write commondir");
        fs::create_dir_all(&common_git).expect("create common Git directory");
        fs::write(
            common_git.join("config"),
            b"[extensions]\nworktreeConfig = f\\\nalse\nWORKTREECONFIG = t\\\nr\\\nue\n\
              [extensions \"unrelated\"]\nworktreeConfig = false\n\
              [core]\nignorecase = false\n",
        )
        .expect("write continued common config");
        fs::write(
            private_git.join("config.worktree"),
            b"[CORE]\nignoreCASE = t\\\nr\\\nue\n\
              [core \"unrelated\"]\nignoreCASE = false\n",
        )
        .expect("write continued private worktree config");
        fs::write(
            checkout.join(".git"),
            b"gitdir: ../main/.git/worktrees/linked-worktree\n",
        )
        .expect("write worktree pointer");
        fs::write(checkout.join(".gitignore"), b"build.log\n").expect("write ignore rule");
        fs::write(checkout.join("BUILD.log"), b"fixture").expect("write mixed-case candidate");

        assert!(git_config_file_bool(
            &common_git.join("config"),
            "extensions.worktreeConfig"
        ));

        let walked = Walker::new(&checkout)
            .respect_git_ignore(true)
            .collect()
            .expect("walk linked worktree");
        assert!(
            !relative_paths(walked.entries(), &checkout).contains(&PathBuf::from("BUILD.log")),
            "private config.worktree must win after the common config"
        );
    }

    #[test]
    fn absolute_and_malformed_gitdir_pointers_are_handled_without_rules() {
        let fixture = Fixture::new();
        let checkout = fixture.root.join("absolute-worktree");
        let git_directory = fixture.root.join("separate-git-directory");
        fs::create_dir_all(git_directory.join("info")).expect("create separate git info");
        fs::write(git_directory.join("info/exclude"), b"absolute-secret.txt\n")
            .expect("write absolute exclude");
        fs::create_dir_all(&checkout).expect("create absolute checkout");
        fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", git_directory.display()),
        )
        .expect("write absolute pointer");
        fs::write(checkout.join("absolute-secret.txt"), b"fixture").expect("write absolute secret");

        let absolute_result = Walker::new(&checkout)
            .respect_git_ignore(true)
            .collect()
            .expect("walk absolute pointer checkout");
        assert!(
            !relative_paths(absolute_result.entries(), &checkout)
                .contains(&PathBuf::from("absolute-secret.txt"))
        );

        let malformed = fixture.root.join("malformed-pointer");
        fs::create_dir_all(&malformed).expect("create malformed checkout");
        fs::write(malformed.join(".git"), b"gitdir: \nextra data\n")
            .expect("write malformed pointer");
        fs::write(malformed.join("not-excluded.txt"), b"fixture")
            .expect("write malformed candidate");
        let malformed_result = Walker::new(&malformed)
            .respect_git_ignore(true)
            .collect()
            .expect("malformed metadata is skipped");
        assert!(
            relative_paths(malformed_result.entries(), &malformed)
                .contains(&PathBuf::from("not-excluded.txt")),
            "a malformed pointer must add no repository rules"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_gitdir_pointer_is_skipped_like_unreadable_repository_metadata() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let checkout = fixture.root.join("unreadable-pointer");
        fs::create_dir_all(&checkout).expect("create checkout");
        let pointer = checkout.join(".git");
        fs::write(&pointer, b"gitdir: ../missing-git-directory\n").expect("write pointer file");
        fs::write(checkout.join("not-excluded.txt"), b"fixture").expect("write candidate");

        let original_permissions = fs::metadata(&pointer)
            .expect("read pointer metadata")
            .permissions();
        fs::set_permissions(&pointer, fs::Permissions::from_mode(0o000))
            .expect("make pointer unreadable");
        if fs::read(&pointer).is_ok() {
            // A privileged test process can bypass mode bits. There is no
            // portable way to arrange an unreadable regular file for it.
            fs::set_permissions(&pointer, original_permissions)
                .expect("restore pointer permissions");
            return;
        }

        let result = Walker::new(&checkout)
            .respect_git_ignore(true)
            .collect()
            .expect("unreadable metadata is skipped rather than reported");
        fs::set_permissions(&pointer, original_permissions).expect("restore pointer permissions");

        assert!(
            result.errors().is_empty(),
            "unreadable repository metadata follows the existing silent policy"
        );
        assert!(
            relative_paths(result.entries(), &checkout)
                .contains(&PathBuf::from("not-excluded.txt")),
            "an unreadable pointer must add no repository rules"
        );
    }

    #[test]
    fn nested_repositories_remain_traversed_with_the_outer_ignore_chain() {
        let fixture = Fixture::new();
        fixture.write("nested/.git/config");
        fixture.write("nested/outer.tmp");
        fixture.write("nested/keep.tmp");
        fixture.write("nested/inner-only.tmp");
        fs::write(fixture.root.join(".gitignore"), b"*.tmp\n").expect("write outer ignore rules");
        fs::write(
            fixture.root.join("nested/.gitignore"),
            b"!keep.tmp\ninner-only.tmp\n",
        )
        .expect("write nested repository rules");

        let result = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk nested repository");
        let paths = relative_paths(result.entries(), &fixture.root);
        assert!(paths.contains(&PathBuf::from("nested/keep.tmp")));
        assert!(!paths.contains(&PathBuf::from("nested/outer.tmp")));
        assert!(!paths.contains(&PathBuf::from("nested/inner-only.tmp")));
        assert!(
            !paths.contains(&PathBuf::from("nested/.git/config")),
            "the nested repository's control directory remains skipped"
        );
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

    #[test]
    fn oversized_ignore_files_follow_the_configured_error_policy() {
        let fixture = Fixture::new();
        fixture.write("visible.txt");
        let ignore_path = fixture.root.join(".gitignore");
        let ignore_file = fs::File::create(&ignore_path).expect("create oversized ignore file");
        ignore_file
            .set_len(super::MAX_IGNORE_FILE_BYTES + 1)
            .expect("size oversized ignore file");

        for threads in [1, 4] {
            let result = Walker::new(&fixture.root)
                .respect_git_ignore(true)
                .threads(threads)
                .error_policy(ErrorPolicy::Collect)
                .collect()
                .expect("collect policy keeps walking");
            assert_eq!(result.errors().len(), 1);
            assert_eq!(result.errors()[0].operation(), "read_ignore");
            assert_eq!(result.errors()[0].path(), ignore_path);
            assert!(
                relative_paths(result.entries(), &fixture.root)
                    .contains(&PathBuf::from("visible.txt")),
                "an unreadable rule file cannot silently hide unrelated entries"
            );
        }

        let skipped = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(1)
            .error_policy(ErrorPolicy::Skip)
            .collect()
            .expect("skip policy keeps walking");
        assert!(skipped.errors().is_empty());

        let aborted = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .threads(1)
            .error_policy(ErrorPolicy::Abort)
            .collect()
            .expect_err("abort policy reports the ignore failure immediately");
        assert_eq!(aborted.operation(), "read_ignore");
        assert_eq!(aborted.path(), ignore_path);

        let streamed = Walker::new(&fixture.root)
            .respect_git_ignore(true)
            .error_policy(ErrorPolicy::Collect)
            .stream()
            .collect::<Vec<_>>();
        assert_eq!(
            streamed.iter().filter(|item| item.is_err()).count(),
            1,
            "the stream yields the ignore failure exactly once"
        );
        assert!(streamed.iter().any(|item| {
            item.as_ref()
                .is_ok_and(|entry| entry.path().ends_with("visible.txt"))
        }));
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
                let case = corpus::parse_case(line).unwrap_or_else(|error| {
                    panic!("{}:{}: {error}", file.display(), line_number + 1)
                });
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

    /// An absolute pattern selects what the same pattern written relative to
    /// the root selects, which is the whole point of rewriting it.
    #[test]
    fn an_absolute_pattern_means_what_its_relative_spelling_means() {
        let fixture = Fixture::new();
        fixture.write("src/a.ts");
        fixture.write("src/deep/b.ts");
        fixture.write("other/c.ts");

        let walk = |pattern: &str| -> Vec<PathBuf> {
            let result = Walker::new(&fixture.root)
                .include(pattern)
                .expect("valid include")
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            relative_paths(result.entries(), &fixture.root)
        };

        for (absolute, relative) in [
            ("/src/*.ts", "src/*.ts"),
            ("/**/*.ts", "**/*.ts"),
            ("/src/**", "src/**"),
            // A brace root from #20 survives the rewrite.
            ("/{src,other}/*.ts", "{src,other}/*.ts"),
            // The separator that joins the root to the rest is not doubled
            // into the pattern, which is what a root ending in one produces.
            ("//src/*.ts", "src/*.ts"),
            ("/./src/*.ts", "src/*.ts"),
        ] {
            assert_eq!(
                walk(&fixture.absolute(absolute)),
                walk(relative),
                "absolute {absolute} must select what {relative} selects"
            );
        }
        // Not vacuously equal.
        assert_eq!(
            walk(&fixture.absolute("/src/*.ts")),
            vec![PathBuf::from("src/a.ts")]
        );
    }

    /// The mode from #83 is about how far a wildcard reaches, and rewriting is
    /// about where the pattern starts, so the two compose without either
    /// knowing about the other.
    #[test]
    fn an_absolute_pattern_is_read_under_the_walk_s_wildcard_mode() {
        let fixture = Fixture::new();
        fixture.write("src/a.ts");
        fixture.write("src/deep/b.ts");

        let walk = |mode: WildcardMode| -> Vec<PathBuf> {
            let result = Walker::new(&fixture.root)
                .wildcard_mode(mode)
                .include(fixture.absolute("/src/*.ts"))
                .expect("valid include")
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            relative_paths(result.entries(), &fixture.root)
        };

        assert_eq!(
            walk(WildcardMode::ComponentScoped),
            vec![PathBuf::from("src/a.ts")]
        );
        assert_eq!(
            walk(WildcardMode::SeparatorCrossing),
            vec![PathBuf::from("src/a.ts"), PathBuf::from("src/deep/b.ts")]
        );
    }

    /// A pattern about a different tree selects nothing, and - the part worth
    /// pinning - prunes nothing either. An exclude that cannot reach this walk
    /// must not close a directory in it.
    #[test]
    fn an_absolute_pattern_outside_the_root_selects_and_prunes_nothing() {
        let fixture = Fixture::new();
        fixture.write("src/a.ts");
        fixture.write("keep/b.ts");

        let elsewhere = format!("{}-elsewhere/**", fixture.absolute(""));

        let included = Walker::new(&fixture.root)
            .include(&elsewhere)
            .expect("an unrelated tree is not an error")
            .options(WalkOptions::default().sort(true).files_only(true))
            .collect()
            .expect("walk succeeds");
        assert!(
            relative_paths(included.entries(), &fixture.root).is_empty(),
            "an include about another tree selects nothing here"
        );

        let excluded = Walker::new(&fixture.root)
            .exclude(&elsewhere)
            .expect("an unrelated tree is not an error")
            .options(WalkOptions::default().sort(true).files_only(true))
            .collect()
            .expect("walk succeeds");
        assert_eq!(
            relative_paths(excluded.entries(), &fixture.root),
            vec![PathBuf::from("keep/b.ts"), PathBuf::from("src/a.ts")],
            "an exclude about another tree removes nothing here"
        );
    }

    /// The rewrite happens before compilation, so the traversal prefilters see
    /// an ordinary relative pattern and keep working. Without this an absolute
    /// include would open every directory in the tree.
    #[test]
    fn the_planner_prefilters_still_apply_to_a_rewritten_pattern() {
        let fixture = Fixture::new();
        let walker = Walker::new(&fixture.root)
            .include(fixture.absolute("/src/**/*.ts"))
            .expect("valid include");
        let pattern = &walker.roots[0].includes[0];

        assert_eq!(
            pattern.literal_roots,
            Some(vec![b"src".to_vec()]),
            "the root prefilter survives the rewrite"
        );
        assert_eq!(
            pattern.extensions,
            Some(vec![b"ts".to_vec()]),
            "the extension prefilter survives the rewrite"
        );
        assert!(pattern.could_match_descendant(b"src"));
        assert!(!pattern.could_match_descendant(b"node_modules"));
        assert!(pattern.matches_extension(b"src/app.ts"));
        assert!(!pattern.matches_extension(b"src/app.js"));

        // A pattern about another tree proves the strongest prefilter of all:
        // nothing under this root is worth opening for it.
        let outside = Walker::new(&fixture.root)
            .include(format!("{}-elsewhere/**/*.ts", fixture.absolute("")))
            .expect("an unrelated tree is not an error");
        assert!(!outside.roots[0].includes[0].could_match_descendant(b"src"));
        assert!(
            !outside.roots[0].includes[0].covers_subtree(b"src", WildcardMode::ComponentScoped)
        );
    }

    /// The shapes the rewrite refuses to guess at, reported instead of quietly
    /// selecting the wrong entries.
    #[test]
    fn an_unprovable_absolute_pattern_is_rejected() {
        let fixture = Fixture::new();
        let parent = fixture
            .root
            .parent()
            .expect("the fixture root has a parent")
            .to_path_buf();
        let parent = String::from_utf8(glob_path_bytes(&parent).into_owned())
            .expect("the temporary directory is UTF-8 on a test host");

        // A wildcard standing where the root's own components are.
        let above = Walker::new(&fixture.root)
            .include(format!("{parent}/*/x.ts"))
            .expect_err("a wildcard above the root is rejected");
        assert!(
            above
                .message()
                .starts_with("a wildcard at or above the walk root"),
        );

        // `..`, which is not resolved here.
        let dot_dot = Walker::new(&fixture.root)
            .include(fixture.absolute("/../x.ts"))
            .expect_err("`..` is rejected");
        assert!(dot_dot.message().starts_with("`..`"));

        // The root itself, which the walk never emits.
        let root_itself = Walker::new(&fixture.root)
            .exclude(fixture.absolute(""))
            .expect_err("naming the root is rejected");
        assert!(
            root_itself
                .message()
                .starts_with("an absolute pattern that names the walk root itself")
        );

        // An absolute pattern cannot be placed against a relative root without
        // reading the process's working directory.
        let relative_root = Walker::new("relative/dir")
            .include(fixture.absolute("/x.ts"))
            .expect_err("a relative root is rejected");
        assert_eq!(
            relative_root.message(),
            "an absolute pattern needs an absolute walk root"
        );
    }

    /// Subtree pruning must decide what the per-entry exclude would have
    /// decided, under either mode.
    ///
    /// What a multi-root walk observed, in the form the acceptance criterion
    /// compares: every entry with the root it was found under and its depth
    /// below that root, plus the recoverable errors. Sorted, so it is a
    /// multiset and a duplicate stays a duplicate.
    #[derive(Debug, PartialEq, Eq)]
    struct RootedOutcome {
        entries: Vec<(PathBuf, PathBuf, usize)>,
        errors: Vec<(&'static str, PathBuf)>,
    }

    impl RootedOutcome {
        fn of(result: &super::WalkResult) -> Self {
            let mut entries = result
                .entries()
                .iter()
                .map(|entry| {
                    (
                        entry.root().to_path_buf(),
                        entry.path().to_path_buf(),
                        entry.depth(),
                    )
                })
                .collect::<Vec<_>>();
            let mut errors = result
                .errors()
                .iter()
                .map(|error| (error.operation(), error.path().to_path_buf()))
                .collect::<Vec<_>>();
            entries.sort_unstable();
            errors.sort_unstable();
            Self { entries, errors }
        }

        /// The concatenation two single-root walks would have produced.
        fn concatenated(parts: impl IntoIterator<Item = Self>) -> Self {
            let mut joined = Self {
                entries: Vec::new(),
                errors: Vec::new(),
            };
            for part in parts {
                joined.entries.extend(part.entries);
                joined.errors.extend(part.errors);
            }
            joined.entries.sort_unstable();
            joined.errors.sort_unstable();
            joined
        }
    }

    /// The frontends a multi-root walk has to agree across.
    #[derive(Debug, Clone, Copy)]
    enum Frontend {
        Collect,
        Visit,
        Stream,
    }

    /// Runs one frontend over `roots` and reports what it saw.
    fn multi_root_outcome(
        roots: &[PathBuf],
        include: Option<&str>,
        threads: usize,
        frontend: Frontend,
    ) -> RootedOutcome {
        multi_root_outcome_with_following(roots, include, threads, frontend, false)
    }

    /// The same acceptance harness under either symlink policy. Keeping the
    /// policy explicit lets the multi-root contract pin the cycle guard too:
    /// following links must change what one root sees, never whether another
    /// root gets to see its own traversal.
    fn multi_root_outcome_with_following(
        roots: &[PathBuf],
        include: Option<&str>,
        threads: usize,
        frontend: Frontend,
        follow_symlinks: bool,
    ) -> RootedOutcome {
        let (first, rest) = roots.split_first().expect("at least one root");
        let mut walker = Walker::new(first).threads(threads).options(
            WalkOptions::default()
                .files_only(true)
                .follow_symlinks(follow_symlinks),
        );
        for root in rest {
            walker = walker.add_root(root).expect("the root takes the patterns");
        }
        if let Some(pattern) = include {
            walker = walker.include(pattern).expect("valid include");
        }
        match frontend {
            Frontend::Collect => {
                RootedOutcome::of(&walker.collect().expect("collect walks under Collect"))
            }
            Frontend::Visit => RootedOutcome::of(
                &walker
                    .visit(|_| Verdict::Keep)
                    .expect("visit walks under Collect"),
            ),
            Frontend::Stream => {
                // The stream reports errors as items rather than in a result,
                // so it is reassembled into the same shape.
                let mut entries = Vec::new();
                let mut errors = Vec::new();
                for item in walker.stream() {
                    match item {
                        Ok(entry) => entries.push(entry),
                        Err(error) => errors.push(error),
                    }
                }
                RootedOutcome::of(&super::WalkResult {
                    entries,
                    errors,
                    cancelled: false,
                })
            }
        }
    }

    /// The acceptance criterion for multi-root walks: walking several roots at
    /// once observes exactly what walking each of them separately observes,
    /// counted as a multiset, on every frontend and at every thread count.
    ///
    /// Entries carry the root they were found under and their depth below it,
    /// so this pins more than the path set: an entry from a multi-root walk has
    /// to say the same things about itself as its single-root counterpart.
    #[test]
    fn a_multi_root_walk_is_the_concatenation_of_the_single_root_walks() {
        let fixture = Fixture::new();
        fixture.write("alpha/src/one.rs");
        fixture.write("alpha/src/deep/two.rs");
        fixture.write("alpha/notes.txt");
        fixture.write("beta/src/three.rs");
        fixture.write("beta/four.txt");
        fixture.write("gamma/src/five.rs");

        let roots = [
            fixture.root.join("alpha"),
            fixture.root.join("beta"),
            fixture.root.join("gamma"),
            // A root that does not exist, so the invariant covers errors too.
            fixture.root.join("missing"),
        ];

        for include in [None, Some("src/**/*.rs"), Some("**/*.rs")] {
            for threads in [1, 4] {
                for frontend in [Frontend::Collect, Frontend::Visit, Frontend::Stream] {
                    let together = multi_root_outcome(&roots, include, threads, frontend);
                    let separately = RootedOutcome::concatenated(roots.iter().map(|root| {
                        multi_root_outcome(std::slice::from_ref(root), include, threads, frontend)
                    }));
                    assert_eq!(
                        together, separately,
                        "{frontend:?} on {threads} threads with include {include:?}"
                    );
                }
            }
        }

        // Not vacuous: the walk really did see all three trees and the failure.
        let all = multi_root_outcome(&roots, None, 4, Frontend::Collect);
        assert_eq!(all.entries.len(), 6);
        assert_eq!(all.errors.len(), 1);
        assert_eq!(all.errors[0].0, "read_dir");
    }

    /// Roots that contain one another deliver their overlap once per root.
    ///
    /// This is the concatenation rule taken seriously rather than an oversight:
    /// suppressing the second copy would need every directory's identity - a
    /// `stat` per directory that only the symlink-following mode pays today -
    /// and would make adding a root able to remove entries.
    #[test]
    fn overlapping_roots_deliver_their_overlap_once_per_root() {
        let fixture = Fixture::new();
        fixture.write("outer/inner/shared.rs");
        fixture.write("outer/own.rs");

        let outer = fixture.root.join("outer");
        let inner = outer.join("inner");
        let result = Walker::new(&outer)
            .add_root(&inner)
            .expect("nested roots are allowed")
            .threads(1)
            .options(WalkOptions::default().sort(true).files_only(true))
            .collect()
            .expect("walk succeeds");

        let shared = inner.join("shared.rs");
        let copies = result
            .entries()
            .iter()
            .filter(|entry| entry.path() == shared)
            .collect::<Vec<_>>();
        assert_eq!(copies.len(), 2, "the overlap is delivered once per root");
        // The two copies differ in exactly what the root makes different.
        let mut seen = copies
            .iter()
            .map(|entry| (entry.root().to_path_buf(), entry.depth()))
            .collect::<Vec<_>>();
        seen.sort_unstable();
        assert_eq!(
            seen,
            vec![(outer.clone(), 2), (inner.clone(), 1)],
            "each copy is one level below the root it came from"
        );
    }

    #[cfg(unix)]
    #[test]
    fn following_links_keeps_overlapping_and_duplicate_roots_independent() {
        use std::os::unix::fs::symlink;

        // `back` makes a genuine cycle for every root traversal. The nested
        // and duplicate roots must nevertheless reproduce their corresponding
        // single-root walks exactly, including the root and depth carried by
        // every overlapping path.
        let fixture = Fixture::new();
        fixture.write("outer/own.txt");
        fixture.write("outer/inner/shared.txt");
        fixture.write("outer/inner/deep/leaf.txt");
        symlink("..", fixture.root.join("outer/inner/back")).expect("create cycle");

        let outer = fixture.root.join("outer");
        let inner = outer.join("inner");
        let alias = fixture.root.join("outer-alias");
        symlink("outer", &alias).expect("create root alias");
        let overlapping = [outer.clone(), inner.clone()];
        let duplicate = [outer.clone(), outer.clone()];
        let aliases = [outer.clone(), alias];
        for roots in [&overlapping[..], &duplicate[..], &aliases[..]] {
            for (frontend, threads) in [
                (Frontend::Collect, 1),
                (Frontend::Collect, 4),
                (Frontend::Visit, 1),
                (Frontend::Visit, 4),
                (Frontend::Stream, 1),
            ] {
                let together =
                    multi_root_outcome_with_following(roots, None, threads, frontend, true);
                let separately = RootedOutcome::concatenated(roots.iter().map(|root| {
                    multi_root_outcome_with_following(
                        std::slice::from_ref(root),
                        None,
                        threads,
                        frontend,
                        true,
                    )
                }));
                assert_eq!(
                    together, separately,
                    "{frontend:?} with {threads} thread(s) and roots {roots:?}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn parallel_following_links_keeps_root_attribution_stable_under_stress() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let outer = fixture.root.join("outer");
        let inner = outer.join("inner");
        for branch in 0..32 {
            for leaf in 0..8 {
                fixture.write(format!("outer/inner/branch-{branch}/leaf-{leaf}.txt"));
            }
        }
        symlink("..", inner.join("back")).expect("create cycle");
        let roots = [outer, inner];
        let expected = RootedOutcome::concatenated(roots.iter().map(|root| {
            multi_root_outcome_with_following(
                std::slice::from_ref(root),
                None,
                1,
                Frontend::Collect,
                true,
            )
        }));

        // The workers can race over the two roots' overlapping subtrees on
        // every run. A shared guard used to make which root won observable in
        // both `root()` and `depth()`; repeat far beyond one schedule.
        for run in 0..24 {
            assert_eq!(
                multi_root_outcome_with_following(&roots, None, 4, Frontend::Collect, true),
                expected,
                "parallel run {run} changed overlap attribution"
            );
        }
    }

    /// Patterns are root-relative and apply under every root; an absolute one
    /// is rewritten per root, so it selects only under the root it names.
    #[test]
    fn patterns_are_read_once_per_root() {
        let fixture = Fixture::new();
        fixture.write("alpha/src/one.rs");
        fixture.write("alpha/other/two.rs");
        fixture.write("beta/src/three.rs");
        fixture.write("beta/other/four.rs");
        let alpha = fixture.root.join("alpha");
        let beta = fixture.root.join("beta");

        let walk = |pattern: String| -> Vec<PathBuf> {
            let result = Walker::new(&alpha)
                .add_root(&beta)
                .expect("the root takes the patterns")
                .include(pattern)
                .expect("valid include")
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            result
                .entries()
                .iter()
                .map(|entry| entry.path().to_path_buf())
                .collect()
        };

        // A relative pattern selects that subtree of every root.
        assert_eq!(
            walk("src/*.rs".to_owned()),
            vec![alpha.join("src/one.rs"), beta.join("src/three.rs")],
        );

        // An absolute pattern names one tree, and #85's out-of-root reading is
        // what makes it select nothing under the other.
        let alpha_glob = String::from_utf8(glob_path_bytes(&alpha).into_owned())
            .expect("the temporary directory is UTF-8 on a test host");
        assert_eq!(
            walk(format!("{alpha_glob}/src/*.rs")),
            vec![alpha.join("src/one.rs")],
        );
    }

    /// Builder order does not matter, including for the rejection an absolute
    /// pattern can produce: the pattern meets every root either way.
    #[test]
    fn a_root_and_a_pattern_meet_whichever_order_they_arrive_in() {
        let fixture = Fixture::new();
        fixture.write("alpha/src/one.rs");
        fixture.write("beta/src/two.rs");
        let alpha = fixture.root.join("alpha");
        let beta = fixture.root.join("beta");

        let entries = |walker: Walker| -> Vec<PathBuf> {
            walker
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds")
                .entries()
                .iter()
                .map(|entry| entry.path().to_path_buf())
                .collect()
        };

        let root_first = entries(
            Walker::new(&alpha)
                .add_root(&beta)
                .expect("root")
                .include("src/*.rs")
                .expect("include"),
        );
        let pattern_first = entries(
            Walker::new(&alpha)
                .include("src/*.rs")
                .expect("include")
                .add_root(&beta)
                .expect("root"),
        );
        assert_eq!(root_first, pattern_first);
        assert_eq!(root_first.len(), 2);

        // A pattern no root can be given is refused from whichever side it is
        // added, rather than only when the root happens to come first.
        let unprovable = fixture.absolute("/../x.rs");
        assert!(
            Walker::new(&alpha)
                .add_root(&beta)
                .expect("root")
                .include(&unprovable)
                .is_err()
        );
        assert!(
            Walker::new(&alpha)
                .include(&unprovable)
                .expect_err("`..` is refused for the first root already")
                .message()
                .starts_with("`..`")
        );
    }

    /// A root that cannot be read is that root's error, and the walk goes on to
    /// the others under `Collect`.
    #[test]
    fn an_unreadable_root_does_not_stop_the_other_roots() {
        let fixture = Fixture::new();
        fixture.write("alpha/one.rs");
        fixture.write("gamma/two.rs");
        let missing = fixture.root.join("beta");

        for threads in [1, 4] {
            let result = Walker::new(fixture.root.join("alpha"))
                .add_root(&missing)
                .expect("root")
                .add_root(fixture.root.join("gamma"))
                .expect("root")
                .threads(threads)
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("Collect keeps walking");
            assert_eq!(
                relative_paths(result.entries(), &fixture.root),
                vec![PathBuf::from("alpha/one.rs"), PathBuf::from("gamma/two.rs")],
                "the readable roots are walked on {threads} threads"
            );
            assert_eq!(result.errors().len(), 1);
            assert_eq!(result.errors()[0].operation(), "read_dir");
            assert_eq!(result.errors()[0].path(), missing);
        }
    }

    #[test]
    fn skip_reports_a_failed_root_while_walking_the_other_roots() {
        let fixture = Fixture::new();
        fixture.write("alpha/one.rs");
        fixture.write("gamma/two.rs");
        let missing = fixture.root.join("beta");
        let build = || {
            Walker::new(fixture.root.join("alpha"))
                .add_root(&missing)
                .expect("root")
                .add_root(fixture.root.join("gamma"))
                .expect("root")
                .error_policy(ErrorPolicy::Skip)
                .options(WalkOptions::default().sort(true).files_only(true))
        };

        for threads in [1, 4] {
            let result = build()
                .threads(threads)
                .collect()
                .expect("Skip preserves the other roots");
            assert_eq!(
                relative_paths(result.entries(), &fixture.root),
                vec![PathBuf::from("alpha/one.rs"), PathBuf::from("gamma/two.rs")]
            );
            assert_eq!(result.errors().len(), 1);
            assert_eq!(result.errors()[0].path(), missing);
        }

        let mut stream = build().stream();
        let mut entries = Vec::new();
        let mut errors = Vec::new();
        for item in &mut stream {
            match item {
                Ok(entry) => entries.push(entry),
                Err(error) => errors.push(error),
            }
        }
        entries.sort_by(|left, right| left.path().cmp(right.path()));
        assert_eq!(
            relative_paths(&entries, &fixture.root),
            vec![PathBuf::from("alpha/one.rs"), PathBuf::from("gamma/two.rs")]
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path(), missing);
    }

    /// `*.tmp/**` used to close `a/b.tmp` in both modes, because the subtree
    /// root was always read as separator-crossing. In the default mode the
    /// exclude does not reach that directory at all - `*.tmp` cannot match the
    /// component `a` - so its contents went missing from the walk without any
    /// pattern saying they should.
    /// The trap from #94, on the one host that can observe it.
    ///
    /// A pattern built by joining `PathBuf`s carries `\` separators, which this
    /// dialect reads as escapes. Every one of these used to compile without
    /// complaint and select nothing; each is now refused with a message that
    /// names the cause.
    #[cfg(windows)]
    #[test]
    fn a_windows_path_handed_over_as_a_pattern_is_refused() {
        let fixture = Fixture::new();
        fixture.write("src/main.ts");
        fixture.write("src/deep/other.ts");
        let root = fixture.root.display().to_string();

        for pattern in [
            format!(r"{root}\src\**\*.ts"),
            format!(r"{root}\src\*.ts"),
            r"src\*.ts".to_owned(),
            r"src\**\*.ts".to_owned(),
        ] {
            let refused = Walker::new(&fixture.root)
                .include(&pattern)
                .expect_err("a path spelled as a pattern is refused");
            assert!(
                refused
                    .message()
                    .starts_with("this looks like a Windows path"),
                "{pattern:?} reported {refused}"
            );
            // Excludes are read by the same rules, so they are refused too -
            // an exclude that silently matches nothing is just as invisible.
            assert!(Walker::new(&fixture.root).exclude(&pattern).is_err());
        }

        // The spelling the dialect wants keeps working, absolute or relative.
        for pattern in [
            "src/**/*.ts",
            &format!("{}/src/**/*.ts", root.replace('\\', "/")),
        ] {
            let result = Walker::new(&fixture.root)
                .include(pattern)
                .expect("valid include")
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            assert_eq!(result.entries().len(), 2, "{pattern} selects both files");
        }

        // An escaped forbidden byte inside a group is one member among
        // several, so the pattern still selects - and must still be accepted.
        // The review of #94 found the first version refusing exactly these.
        for pattern in [
            r"src/[m\*]ain.ts",
            r"src/{main,\*}.ts",
            r"src/@(main|\*).ts",
        ] {
            let result = Walker::new(&fixture.root)
                .include(pattern)
                .unwrap_or_else(|error| panic!("{pattern} can match, got {error}"))
                .options(WalkOptions::default().sort(true).files_only(true))
                .collect()
                .expect("walk succeeds");
            assert_eq!(
                relative_paths(result.entries(), &fixture.root),
                vec![PathBuf::from("src/main.ts")],
                "{pattern} selects through its group"
            );
        }
    }

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

    #[test]
    fn excludes_keep_descending_for_an_explicitly_included_descendant() {
        let fixture = Fixture::new();
        fixture.write("a/keep.txt");
        fixture.write("a/drop.txt");
        let result = Walker::new(&fixture.root)
            .include("a/keep.txt")
            .expect("valid include")
            .exclude("a")
            .expect("valid exclude")
            .options(WalkOptions::default().files_only(true).sort(true))
            .collect()
            .expect("walk succeeds");
        assert_eq!(
            relative_paths(result.entries(), &fixture.root),
            [PathBuf::from("a/keep.txt")]
        );
    }

    /// A covering exclude has only one blind spot with the default glob
    /// policy: hidden components. A broad include that cannot name one must
    /// not turn a rejected build tree back into traversal work.
    #[test]
    fn covering_excludes_prune_when_includes_cannot_reach_hidden_descendants() {
        struct PruningBackend {
            root: PathBuf,
            reads: Mutex<Vec<PathBuf>>,
        }

        impl super::DirectoryBackend for PruningBackend {
            fn read_directory(
                &self,
                path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                listing.clear();
                let relative = path
                    .strip_prefix(&self.root)
                    .expect("walk only reads descendants of its root");
                self.reads
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(relative.to_path_buf());
                if relative.as_os_str().is_empty() {
                    listing.push("target".as_ref(), true, false);
                    listing.push("src".as_ref(), true, false);
                } else if relative == Path::new("target") {
                    listing.push("visible.rs".as_ref(), false, false);
                    listing.push(".hidden".as_ref(), true, false);
                } else if relative == Path::new("target/.hidden") || relative == Path::new("src") {
                    listing.push("keep.rs".as_ref(), false, false);
                }
                Ok(())
            }
        }

        let walk = |include: &str, match_hidden: bool| {
            let fixture = Fixture::new();
            let backend = PruningBackend {
                root: fixture.root.clone(),
                reads: Mutex::new(Vec::new()),
            };
            let result = Walker::new(&fixture.root)
                .threads(1)
                .match_hidden(match_hidden)
                .include(include)
                .expect("valid include")
                .exclude("**/target/**")
                .expect("valid exclude")
                .options(WalkOptions::default().files_only(true))
                .collect_with(&backend)
                .expect("mock walk succeeds");
            let paths = relative_paths(result.entries(), &fixture.root);
            let reads = backend
                .reads
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (paths, reads)
        };

        for (include, match_hidden) in [("**/*.{rs,toml}", false), ("**/*.{rs,toml}", true)] {
            let (paths, reads) = walk(include, match_hidden);
            assert_eq!(paths, [PathBuf::from("src/keep.rs")]);
            assert_eq!(
                reads,
                [PathBuf::new(), PathBuf::from("src")],
                "{include} with match_hidden={match_hidden} must not open target"
            );
        }

        let (paths, reads) = walk("**/.hidden/keep.rs", false);
        assert_eq!(paths, [PathBuf::from("target/.hidden/keep.rs")]);
        assert!(
            reads.contains(&PathBuf::from("target/.hidden")),
            "an explicit hidden include still re-admits the excluded subtree's blind spot"
        );

        let (paths, reads) = walk("**/?(.visible).hidden/keep.rs", false);
        assert_eq!(paths, [PathBuf::from("target/.hidden/keep.rs")]);
        assert!(
            reads.contains(&PathBuf::from("target/.hidden")),
            "an extglob that explicitly permits a leading period can re-admit its zero-width branch"
        );
    }

    /// A wildcard cannot stop immediately before a leading literal period.
    /// Therefore these includes cannot reach the hidden descendants and a
    /// covering exclude may prune their parent subtree.
    #[test]
    fn covering_excludes_prune_hidden_descendants_after_wildcards() {
        let fixture = Fixture::new();
        fixture.write("build/.env");
        fixture.write("x/foo/.hidden/keep.rs");

        for (label, mode, include, exclude) in [
            (
                "zero-width star",
                WildcardMode::ComponentScoped,
                "**/*.env",
                "build/**",
            ),
            (
                "separator-crossing star",
                WildcardMode::SeparatorCrossing,
                "x/f*.hidden/keep.rs",
                "x/**",
            ),
        ] {
            let build = || {
                Walker::new(&fixture.root)
                    .wildcard_mode(mode)
                    .include(include)
                    .expect("valid include")
                    .exclude(exclude)
                    .expect("valid exclude")
                    .options(WalkOptions::default().files_only(true).sort(true))
            };
            assert_frontends_agree(label, &fixture.root, build);
            assert_eq!(
                relative_paths(
                    build()
                        .threads(1)
                        .collect()
                        .expect("walk succeeds")
                        .entries(),
                    &fixture.root,
                ),
                Vec::<PathBuf>::new(),
                "{label}: the covering exclude may prune an unreachable hidden match"
            );
        }
    }

    /// With no includes, every exclude form that rejects a directory can
    /// prune it: a literal, a directory-only pattern, or an explicit subtree.
    #[test]
    fn excluded_directories_without_includes_are_never_opened() {
        struct ReadRecordingBackend {
            root: PathBuf,
            reads: Mutex<Vec<PathBuf>>,
        }

        impl super::DirectoryBackend for ReadRecordingBackend {
            fn read_directory(
                &self,
                path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                listing.clear();
                let relative = path
                    .strip_prefix(&self.root)
                    .expect("walk only reads descendants of its root");
                self.reads
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(relative.to_path_buf());
                if relative.as_os_str().is_empty() {
                    listing.push("target".as_ref(), true, false);
                } else if relative == Path::new("target") {
                    listing.push("must-not-be-read.txt".as_ref(), false, false);
                }
                Ok(())
            }
        }

        for exclude in ["target", "target/", "target/**"] {
            let fixture = Fixture::new();
            let backend = ReadRecordingBackend {
                root: fixture.root.clone(),
                reads: Mutex::new(Vec::new()),
            };
            let result = Walker::new(&fixture.root)
                .threads(1)
                .exclude(exclude)
                .expect("valid exclude")
                .collect_with(&backend)
                .expect("mock walk succeeds");

            assert!(result.entries().is_empty(), "{exclude}");
            assert_eq!(
                backend
                    .reads
                    .into_inner()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                [PathBuf::new()],
                "{exclude} must prune the directory before opening it"
            );
        }
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
                follow_symlinks: bool,
                refuse_final_symlink: bool,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                self.reads
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(path.to_path_buf());
                super::StdBackend.read_directory(
                    path,
                    follow_symlinks,
                    refuse_final_symlink,
                    listing,
                )
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

    /// Native directory records on DT_UNKNOWN filesystems and portable
    /// `DirEntry::file_type` both use this channel. Keeping the test at the
    /// backend boundary pins the policy independently of the local filesystem
    /// (whose dirents normally already carry a type).
    #[test]
    fn deferred_entry_stat_failures_keep_siblings_and_report_permissions() {
        struct UnknownTypeBackend {
            root: PathBuf,
            failure: std::io::ErrorKind,
        }

        impl super::DirectoryBackend for UnknownTypeBackend {
            fn read_directory(
                &self,
                path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                listing.clear();
                if path == self.root {
                    listing.push("before".as_ref(), false, false);
                    super::defer_entry_stat_error(
                        listing,
                        self.root.join("unknown"),
                        std::io::Error::from(self.failure),
                    )?;
                    listing.push("after".as_ref(), false, false);
                }
                Ok(())
            }
        }

        let fixture = Fixture::new();
        let permission = UnknownTypeBackend {
            root: fixture.root.clone(),
            failure: std::io::ErrorKind::PermissionDenied,
        };
        let walker = Walker::new(&fixture.root)
            .threads(1)
            .error_policy(ErrorPolicy::Collect);
        let mut state = super::WalkState::new(&walker, &super::keep_every_entry);
        state
            .walk_directory(
                &permission,
                directory_task(&walker, &permission, fixture.root.clone()),
            )
            .expect("collect keeps the usable listing");
        assert_eq!(
            relative_paths(&state.entries, &fixture.root),
            [PathBuf::from("before"), PathBuf::from("after")]
        );
        assert_eq!(state.errors.len(), 1);
        assert_eq!(state.errors[0].operation(), "read_dir");
        assert_eq!(state.errors[0].path(), fixture.root.join("unknown"));
        assert_eq!(
            state.errors[0].source.kind(),
            std::io::ErrorKind::PermissionDenied
        );

        let parallel = UnknownTypeBackend {
            root: fixture.root.clone(),
            failure: std::io::ErrorKind::PermissionDenied,
        };
        let result = Walker::new(&fixture.root)
            .threads(4)
            .error_policy(ErrorPolicy::Collect)
            .collect_with(&parallel)
            .expect("parallel collect keeps the usable listing");
        assert_eq!(
            relative_paths(result.entries(), &fixture.root),
            [PathBuf::from("before"), PathBuf::from("after")]
        );
        assert_eq!(result.errors().len(), 1);
        assert_eq!(result.errors()[0].path(), fixture.root.join("unknown"));

        for race in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::NotADirectory,
        ] {
            let backend = UnknownTypeBackend {
                root: fixture.root.clone(),
                failure: race,
            };
            let mut state = super::WalkState::new(&walker, &super::keep_every_entry);
            state
                .walk_directory(
                    &backend,
                    directory_task(&walker, &backend, fixture.root.clone()),
                )
                .expect("a changed entry costs only that entry");
            assert_eq!(
                relative_paths(&state.entries, &fixture.root),
                [PathBuf::from("before"), PathBuf::from("after")]
            );
            assert!(state.errors.is_empty(), "{race:?} is a replacement race");
        }
    }

    #[test]
    fn stream_delivers_deferred_entry_errors_after_listing_siblings() {
        let fixture = Fixture::new();
        let mut stream = Walker::new(&fixture.root)
            .error_policy(ErrorPolicy::Collect)
            .stream();
        // Feed the stream the same completed listing native and portable
        // readers create. This isolates delivery order from a host filesystem
        // that usually supplies a file type without a fallible stat.
        stream.pending_directories.clear();
        stream.directory = fixture.root.clone();
        stream.path = fixture.root.clone();
        stream.listing.push("before".as_ref(), false, false);
        super::defer_entry_stat_error(
            &mut stream.listing,
            fixture.root.join("unknown"),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        )
        .expect("permission failure is deferred");
        stream.listing.push("after".as_ref(), false, false);

        let delivered = stream
            .map(|item| match item {
                Ok(entry) => Ok(entry
                    .path()
                    .strip_prefix(&fixture.root)
                    .expect("entry belongs to fixture")
                    .to_path_buf()),
                Err(error) => Err((
                    error.operation(),
                    error.path().to_path_buf(),
                    error.source.kind(),
                )),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            delivered,
            vec![
                Ok(PathBuf::from("before")),
                Ok(PathBuf::from("after")),
                Err((
                    "read_dir",
                    fixture.root.join("unknown"),
                    std::io::ErrorKind::PermissionDenied,
                )),
            ],
            "the stream must not let one deferred entry failure hide its siblings"
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
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
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
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
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

    /// Resolving a symlink's kind treats a missing target as an answer, but a
    /// `metadata` failure for any other reason leaves the kind genuinely
    /// unknown - and that goes to the error policy rather than being swallowed.
    ///
    /// A mock backend, because a stat that fails with anything other than
    /// `NotFound` is not something a portable fixture can arrange.
    #[test]
    fn an_unreadable_symlink_target_is_reported_rather_than_silently_dropped() {
        struct UnreadableLinkBackend {
            root: PathBuf,
            kind: std::io::ErrorKind,
        }

        impl super::DirectoryBackend for UnreadableLinkBackend {
            fn read_directory(
                &self,
                path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                listing.clear();
                if path == self.root {
                    listing.push("link".as_ref(), false, true);
                }
                Ok(())
            }

            fn metadata(&self, _path: &Path) -> std::io::Result<fs::Metadata> {
                Err(std::io::Error::from(self.kind))
            }
        }

        let fixture = Fixture::new();
        let walker = Walker::new(&fixture.root).threads(1).options(
            WalkOptions::default()
                .files_only(true)
                .resolve_symlink_kind(true),
        );

        // A target that is not there answers the question: dropped, no error.
        let backend = UnreadableLinkBackend {
            root: fixture.root.clone(),
            kind: std::io::ErrorKind::NotFound,
        };
        let mut state = super::WalkState::new(&walker, &super::keep_every_entry);
        state
            .walk_directory(
                &backend,
                directory_task(&walker, &backend, fixture.root.clone()),
            )
            .expect("a broken link does not end the walk");
        assert!(state.entries.is_empty(), "a broken link is not a file");
        assert!(state.errors.is_empty(), "a broken link is not an error");

        // A target we were not allowed to look at does not: dropped and
        // reported, so the walk cannot silently lose an entry it could not
        // classify.
        let backend = UnreadableLinkBackend {
            root: fixture.root.clone(),
            kind: std::io::ErrorKind::PermissionDenied,
        };
        let mut state = super::WalkState::new(&walker, &super::keep_every_entry);
        state
            .walk_directory(
                &backend,
                directory_task(&walker, &backend, fixture.root.clone()),
            )
            .expect("the collect policy retains the metadata error");
        assert!(state.entries.is_empty());
        assert_eq!(state.errors.len(), 1);
        assert_eq!(state.errors[0].operation(), "metadata");
        assert_eq!(state.errors[0].path(), fixture.root.join("link"));
    }

    /// The stat is not paid for entries that cannot need it: an ordinary file,
    /// and a symlink in a walk that has no kind filter to answer.
    #[test]
    fn resolving_stats_only_symlinks_that_a_kind_filter_asks_about() {
        struct CountingLinkBackend {
            root: PathBuf,
            stats: std::cell::Cell<usize>,
        }

        impl super::DirectoryBackend for CountingLinkBackend {
            fn read_directory(
                &self,
                path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                listing: &mut super::Listing,
            ) -> std::io::Result<()> {
                listing.clear();
                if path == self.root {
                    listing.push("plain.txt".as_ref(), false, false);
                    listing.push("link".as_ref(), false, true);
                }
                Ok(())
            }

            fn metadata(&self, _path: &Path) -> std::io::Result<fs::Metadata> {
                self.stats.set(self.stats.get() + 1);
                fs::metadata(&self.root)
            }
        }

        let count = |options: WalkOptions| {
            let fixture = Fixture::new();
            let walker = Walker::new(&fixture.root).threads(1).options(options);
            let backend = CountingLinkBackend {
                root: fixture.root.clone(),
                stats: std::cell::Cell::new(0),
            };
            let mut state = super::WalkState::new(&walker, &super::keep_every_entry);
            state
                .walk_directory(
                    &backend,
                    directory_task(&walker, &backend, fixture.root.clone()),
                )
                .expect("walk succeeds");
            backend.stats.get()
        };

        assert_eq!(
            count(WalkOptions::default().resolve_symlink_kind(true)),
            0,
            "no kind filter is asking, so there is nothing to resolve"
        );
        assert_eq!(
            count(WalkOptions::default().files_only(true)),
            0,
            "the option is off"
        );
        assert_eq!(
            count(
                WalkOptions::default()
                    .files_only(true)
                    .resolve_symlink_kind(true)
            ),
            1,
            "one stat, for the symlink only"
        );
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
                directory_task(
                    walker,
                    &super::StdBackend,
                    walker
                        .roots()
                        .next()
                        .expect("a walk has a root")
                        .to_path_buf(),
                ),
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
    fn a_failed_root_is_reported_under_every_error_policy() {
        let missing = std::env::temp_dir().join(format!(
            "ferralk-missing-{}",
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let collected = Walker::new(&missing)
            .error_policy(ErrorPolicy::Collect)
            .collect()
            .expect("collect policy retains the error");
        assert_eq!(collected.errors().len(), 1);
        for threads in [1, 4] {
            let skipped = Walker::new(&missing)
                .threads(threads)
                .error_policy(ErrorPolicy::Skip)
                .collect()
                .expect("Skip keeps walking after a caller-supplied root failure");
            assert_eq!(skipped.errors().len(), 1);
            assert_eq!(skipped.errors()[0].operation(), "read_dir");
            assert_eq!(skipped.errors()[0].path(), missing);
        }
        let streamed = Walker::new(&missing)
            .error_policy(ErrorPolicy::Skip)
            .stream()
            .collect::<Result<Vec<_>, _>>()
            .expect_err("stream reports a failed caller-supplied root under Skip");
        assert_eq!(streamed.operation(), "read_dir");
        assert_eq!(streamed.path(), missing);
        assert!(
            Walker::new(&missing)
                .error_policy(ErrorPolicy::Abort)
                .collect()
                .is_err()
        );
    }

    #[test]
    fn a_plain_file_root_reports_not_a_directory_without_emitting_it() {
        let fixture = Fixture::new();
        let file = fixture.root.join("not-a-directory.txt");
        fs::write(&file, b"fixture").expect("write file root");

        for threads in [1, 4] {
            let result = Walker::new(&file)
                .threads(threads)
                .error_policy(ErrorPolicy::Skip)
                .collect()
                .expect("root error is collected while the walk completes");
            assert!(result.entries().is_empty());
            assert_eq!(result.errors().len(), 1);
            assert_eq!(result.errors()[0].operation(), "read_dir");
            assert_eq!(result.errors()[0].path(), file);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_symlink_root_is_traversed_even_without_following_descendants() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("real/inside.txt");
        let link = fixture.root.join("linked-root");
        symlink("real", &link).expect("create directory symlink root");

        for threads in [1, 4] {
            for follow_symlinks in [false, true] {
                let result = Walker::new(&link)
                    .threads(threads)
                    .options(WalkOptions::default().follow_symlinks(follow_symlinks))
                    .collect()
                    .expect("directory symlink root is opened");
                assert_eq!(
                    relative_paths(result.entries(), &link),
                    vec![PathBuf::from("inside.txt")]
                );
            }
        }
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

    #[test]
    fn path_bytes_bridge_entries_to_byte_first_matchers() {
        let fixture = Fixture::new();
        fixture.write("src/main.rs");

        let result = Walker::new(&fixture.root)
            .collect()
            .expect("fixture has no I/O errors");
        let entry = result
            .entries()
            .iter()
            .find(|entry| entry.path().ends_with("src/main.rs"))
            .expect("walk reports the fixture file");
        let matcher = Pattern::compile(
            "**/main.rs",
            PatternOptions::default().recursive_double_star(true),
        )
        .expect("valid pattern");

        assert_eq!(
            entry.path_bytes(),
            entry.path().as_os_str().as_encoded_bytes()
        );
        assert!(matcher.is_match(entry.path_bytes()));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_are_entered_by_every_frontend() {
        use std::os::unix::fs::symlink;

        // Three names, one directory, but no loop: all aliases must be
        // traversed. A walk-wide visited set used to pick one winner (racy in
        // parallel); an ancestor chain sees no repeated key for any alias.
        let fixture = Fixture::new();
        fixture.write("real/inside.txt");
        symlink("real", fixture.root.join("first")).expect("create first directory symlink");
        symlink("real", fixture.root.join("second")).expect("create second directory symlink");
        let options = WalkOptions::default().follow_symlinks(true).sort(true);

        let inside_paths = |paths: Vec<PathBuf>| {
            let mut paths = paths
                .into_iter()
                .filter(|path| path.file_name().is_some_and(|name| name == "inside.txt"))
                .collect::<Vec<_>>();
            paths.sort_unstable();
            paths
        };
        let expected = vec![
            PathBuf::from("first/inside.txt"),
            PathBuf::from("real/inside.txt"),
            PathBuf::from("second/inside.txt"),
        ];

        for threads in [1, 4] {
            let result = Walker::new(&fixture.root)
                .threads(threads)
                .options(options)
                .collect()
                .expect("walk succeeds");
            assert_eq!(
                inside_paths(relative_paths(result.entries(), &fixture.root)),
                expected,
                "collect with {threads} thread(s) enters every acyclic alias"
            );
        }

        let streamed = Walker::new(&fixture.root)
            .options(options)
            .stream()
            .map(|entry| entry.expect("fixture has no I/O errors"))
            .collect::<Vec<_>>();
        assert_eq!(
            inside_paths(relative_paths(&streamed, &fixture.root)),
            expected,
            "the stream enters every acyclic alias"
        );
    }

    #[cfg(unix)]
    #[test]
    fn followed_directory_symlinks_use_the_target_kind_for_excludes_and_ignores() {
        use std::os::unix::fs::symlink;

        let excluded = Fixture::new();
        excluded.write("real/artifact.o");
        symlink("real", excluded.root.join("linked")).expect("create directory symlink");
        let follow = WalkOptions::default().follow_symlinks(true).sort(true);

        assert_frontends_agree(
            "followed symlink excluded as directory",
            &excluded.root,
            || {
                Walker::new(&excluded.root)
                    .exclude("linked/")
                    .expect("valid directory exclusion")
                    .options(follow)
            },
        );
        let result = Walker::new(&excluded.root)
            .exclude("linked/")
            .expect("valid directory exclusion")
            .options(follow)
            .collect()
            .expect("walk succeeds");
        assert_eq!(
            relative_paths(result.entries(), &excluded.root),
            vec![PathBuf::from("real"), PathBuf::from("real/artifact.o")]
        );

        let ignored = Fixture::new();
        ignored.write("real/artifact.o");
        fs::write(ignored.root.join(".gitignore"), b"build/\n").expect("write gitignore");
        symlink("real", ignored.root.join("build")).expect("create ignored directory symlink");

        assert_frontends_agree(
            "followed symlink ignored as directory",
            &ignored.root,
            || {
                Walker::new(&ignored.root)
                    .respect_git_ignore(true)
                    .options(follow)
            },
        );
        let result = Walker::new(&ignored.root)
            .respect_git_ignore(true)
            .options(follow)
            .collect()
            .expect("walk succeeds");
        assert_eq!(
            relative_paths(result.entries(), &ignored.root),
            vec![
                PathBuf::from(".gitignore"),
                PathBuf::from("real"),
                PathBuf::from("real/artifact.o"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn includes_re_admit_descendants_through_followed_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("real/keep.txt");
        symlink("real", fixture.root.join("linked")).expect("create directory symlink");
        let follow = WalkOptions::default().follow_symlinks(true).sort(true);

        let real = Walker::new(&fixture.root)
            .exclude("real")
            .expect("valid real-directory exclusion")
            .include("real/keep.txt")
            .expect("valid real-directory include")
            .options(follow)
            .collect()
            .expect("real-directory walk succeeds");
        assert_eq!(
            relative_paths(real.entries(), &fixture.root),
            vec![PathBuf::from("real/keep.txt")]
        );

        assert_frontends_agree(
            "include below a path-excluded followed directory symlink",
            &fixture.root,
            || {
                Walker::new(&fixture.root)
                    .exclude("linked")
                    .expect("valid symlink exclusion")
                    .include("linked/keep.txt")
                    .expect("valid descendant include")
                    .options(follow)
            },
        );
        let linked = Walker::new(&fixture.root)
            .exclude("linked")
            .expect("valid symlink exclusion")
            .include("linked/keep.txt")
            .expect("valid descendant include")
            .options(follow)
            .collect()
            .expect("symlink walk succeeds");
        assert_eq!(
            relative_paths(linked.entries(), &fixture.root),
            vec![PathBuf::from("linked/keep.txt")]
        );

        symlink("missing-target", fixture.root.join("dangling")).expect("create dangling symlink");
        let dangling = Walker::new(&fixture.root)
            .exclude("dangling")
            .expect("valid dangling-link exclusion")
            .include("dangling/keep.txt")
            .expect("valid descendant include")
            .options(follow)
            .error_policy(ErrorPolicy::Collect)
            .collect()
            .expect("excluded dangling link remains recoverable");
        assert!(dangling.entries().is_empty());
        assert!(dangling.errors().is_empty());
    }

    /// Resolving an excluded followed link only serves a possible descendant
    /// include. A self-loop has no reachable descendant, just like a dangling
    /// link, so ELOOP must restore the path-exclusion shortcut instead of
    /// aborting the walk.
    #[cfg(unix)]
    #[test]
    fn an_excluded_self_loop_cannot_abort_a_re_admitting_walk() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        symlink("loop", fixture.root.join("loop")).expect("create self-loop symlink");

        for threads in [1, 4] {
            let result = Walker::new(&fixture.root)
                .threads(threads)
                .exclude("loop")
                .expect("valid self-loop exclusion")
                .include("loop/keep.txt")
                .expect("valid descendant include")
                .options(WalkOptions::default().follow_symlinks(true))
                .error_policy(ErrorPolicy::Abort)
                .collect()
                .expect("an excluded self-loop has no descendant that could be re-admitted");
            assert!(result.entries().is_empty(), "threads={threads}");
            assert!(result.errors().is_empty(), "threads={threads}");
        }
    }

    /// A target path that crosses a regular file cannot contain the descendant
    /// an include was trying to re-admit. It is the same terminal resolution
    /// class as a dangling or looped target, but only after the unresolved
    /// link itself was already excluded.
    #[cfg(unix)]
    #[test]
    fn an_excluded_link_through_a_file_has_no_re_admittable_descendant() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("plain.txt");
        symlink("plain.txt/inner", fixture.root.join("notdir"))
            .expect("create link whose target crosses a file");
        let follow = WalkOptions::default().follow_symlinks(true);

        for threads in [1, 4] {
            let result = Walker::new(&fixture.root)
                .threads(threads)
                .exclude("notdir")
                .expect("valid link exclusion")
                .include("notdir/keep.txt")
                .expect("valid descendant include")
                .options(follow)
                .error_policy(ErrorPolicy::Abort)
                .collect()
                .expect("an excluded ENOTDIR link cannot reach a descendant");
            assert!(result.entries().is_empty(), "threads={threads}");
        }

        let error = Walker::new(&fixture.root)
            .options(follow)
            .error_policy(ErrorPolicy::Abort)
            .collect()
            .expect_err("an unexcluded ENOTDIR link remains a metadata error");
        assert_eq!(error.operation(), "metadata");
        assert_eq!(error.path(), fixture.root.join("notdir"));
        assert_eq!(error.source.kind(), std::io::ErrorKind::NotADirectory);
    }

    #[cfg(unix)]
    #[test]
    fn followed_symlinks_skipped_by_path_rules_do_not_need_a_target() {
        use std::os::unix::fs::symlink;

        let excluded = Fixture::new();
        symlink("missing-target", excluded.root.join("hidden-link"))
            .expect("create dangling symlink");
        let follow = WalkOptions::default().follow_symlinks(true).sort(true);

        assert_frontends_agree(
            "path-excluded dangling followed symlink",
            &excluded.root,
            || {
                Walker::new(&excluded.root)
                    .exclude("hidden-link")
                    .expect("valid path exclusion")
                    .options(follow)
            },
        );
        let result = Walker::new(&excluded.root)
            .exclude("hidden-link")
            .expect("valid path exclusion")
            .options(follow)
            .error_policy(ErrorPolicy::Collect)
            .collect()
            .expect("excluded dangling link is not an error");
        assert!(result.entries().is_empty());
        assert!(result.errors().is_empty());

        let ignored = Fixture::new();
        fs::write(ignored.root.join(".gitignore"), b"hidden-link\n").expect("write gitignore");
        symlink("missing-target", ignored.root.join("hidden-link"))
            .expect("create dangling symlink");

        assert_frontends_agree(
            "path-ignored dangling followed symlink",
            &ignored.root,
            || {
                Walker::new(&ignored.root)
                    .respect_git_ignore(true)
                    .options(follow)
            },
        );
        let result = Walker::new(&ignored.root)
            .respect_git_ignore(true)
            .options(follow)
            .error_policy(ErrorPolicy::Collect)
            .collect()
            .expect("ignored dangling link is not an error");
        assert_eq!(
            relative_paths(result.entries(), &ignored.root),
            vec![PathBuf::from(".gitignore")]
        );
        assert!(result.errors().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_policy_prevents_directory_cycles_without_pruning_aliases() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("real/inside.txt");
        // `real/back` resolves to the root, which is already an ancestor when
        // this entry is considered. It is a loop, unlike a sibling alias.
        symlink("..", fixture.root.join("real/back")).expect("create cycle symlink");

        let without_following = Walker::new(&fixture.root)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk succeeds");
        assert!(
            !relative_paths(without_following.entries(), &fixture.root)
                .contains(&PathBuf::from("real/back/real/inside.txt"))
        );
        let link = without_following
            .entries()
            .iter()
            .find(|entry| entry.basename() == Some(std::ffi::OsStr::new("back")))
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
    fn parallel_abort_returns_an_error_without_cancelling_the_caller_token() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        fixture.write("left/ok.txt");
        fixture.write("right/ok.txt");
        symlink("missing-left", fixture.root.join("left/dangling"))
            .expect("create left dangling symlink");
        symlink("missing-right", fixture.root.join("right/dangling"))
            .expect("create right dangling symlink");
        let cancellation = CancellationToken::default();
        let serial_cancellation = CancellationToken::default();

        let serial_error = Walker::new(&fixture.root)
            .threads(1)
            .options(WalkOptions::default().follow_symlinks(true))
            .error_policy(ErrorPolicy::Abort)
            .cancellation(serial_cancellation.clone())
            .collect()
            .expect_err("serial abort returns the first metadata error");

        let error = Walker::new(&fixture.root)
            .threads(4)
            .options(WalkOptions::default().follow_symlinks(true))
            .error_policy(ErrorPolicy::Abort)
            .cancellation(cancellation.clone())
            .collect()
            .expect_err("abort policy returns the first metadata error");

        assert_eq!(error.operation(), "metadata");
        assert_eq!(serial_error.operation(), error.operation());
        assert!(
            !serial_cancellation.is_cancelled(),
            "serial abort leaves the caller-owned token alone"
        );
        assert!(
            !cancellation.is_cancelled(),
            "parallel abort must match serial token ownership"
        );

        let reused = Walker::new(&fixture.root)
            .threads(4)
            .cancellation(cancellation.clone())
            .collect()
            .expect("the same token can drive a later walk");
        assert!(!reused.was_cancelled());
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
