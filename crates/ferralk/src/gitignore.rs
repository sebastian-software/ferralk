//! Gitignore evaluation, done once per directory instead of once per entry.
//!
//! Every directory the walk enters reads its own ignore files once and links
//! them onto the chain it inherited, so an entry is matched against each ignore
//! file that is actually in force exactly once, with the path relative to that
//! file's directory. Directories without ignore files add nothing to the chain,
//! which is why a tree without them costs nothing per entry.
//!
//! An ignored directory is not entered at all, which is both what Git does and
//! why nothing below it can be re-included: the ignore files inside it are
//! never read, so their negations never apply.
//!
//! The chain travels with the directory task rather than through a cache: every
//! directory is visited exactly once, so the descent already builds each node
//! exactly once, whatever the worker count. A shared cache would add
//! synchronization to a problem the task graph has already solved.

use std::{
    borrow::Cow,
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use super::glob_path_bytes;
use super::{
    BoundedFileRead, DirectoryBackend, Listing, Walker,
    ignore_rules::{
        ComponentBudgetExceeded, LineBudgets, MAX_IGNORE_FILE_COMPONENTS,
        MAX_IGNORE_FILE_RETAINED_BYTES, RuleSet, RuleSetBuilder,
    },
    read_bounded_file, read_bounded_prefix_no_follow,
};

/// Ignore files of a directory, in increasing precedence: a later file wins.
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];

/// Repository-wide excludes, which the root's own ignore files override.
const REPOSITORY_EXCLUDE_FILE: &str = "info/exclude";

/// A single ignore file may contribute at most this many rule lines. The byte
/// limit bounds I/O; this second dimension bounds compiled matchers even for a
/// file made entirely of one-byte rules.
const MAX_IGNORE_RULES: usize = 100_000;

/// One active ancestor chain of ignore files may retain at most this many
/// compiled matcher units. This bounds memory across arbitrarily nested
/// `.gitignore` and `.ignore` files without constraining realistic repositories.
const MAX_IGNORE_CHAIN_COMPONENTS: usize = 1_000_000;

/// One active ancestor chain of ignore files may retain at most this many
/// estimated matcher bytes (2x the 256 MiB single-file retained matcher limit).
/// This bounds memory across nested directories with large literal, dense-rule,
/// or wildcard ignore files.
const MAX_IGNORE_CHAIN_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy)]
struct IgnoreBudget {
    max_file_components: usize,
    max_file_bytes: usize,
    max_chain_components: usize,
    max_chain_bytes: usize,
}

impl IgnoreBudget {
    const DEFAULT: Self = Self {
        max_file_components: MAX_IGNORE_FILE_COMPONENTS,
        max_file_bytes: MAX_IGNORE_FILE_RETAINED_BYTES,
        max_chain_components: MAX_IGNORE_CHAIN_COMPONENTS,
        max_chain_bytes: MAX_IGNORE_CHAIN_BYTES,
    };
}

#[derive(Clone, Copy, Default)]
struct ChainUsage {
    compiled_units: usize,
    retained_bytes: usize,
}

#[derive(Debug)]
pub(crate) struct IgnoreReadError {
    pub(crate) path: PathBuf,
    source: super::DeferredIoError,
}

impl IgnoreReadError {
    fn new(path: PathBuf, source: std::io::Error) -> Self {
        Self {
            path,
            source: source.into(),
        }
    }

    pub(crate) fn into_parts(self) -> (PathBuf, std::io::Error) {
        (self.path, self.source.into_io_error())
    }

    pub(crate) fn into_deferred_parts(self) -> (PathBuf, super::DeferredIoError) {
        (self.path, self.source)
    }

    #[cfg(test)]
    fn kind(&self) -> std::io::ErrorKind {
        self.source.kind()
    }
}

/// The repository-local filesystem adaptations Git applies before ignore
/// matching. They are derived once per repository and carried through every
/// traversal frontend, including the deliberately non-Git nested-repository
/// traversal described in the public compatibility notes.
#[derive(Debug, Clone, Copy, Default)]
struct GitIgnoreAdaptation {
    case_insensitive: bool,
    precompose_unicode: bool,
}

/// The ignore rules in force inside one directory.
#[derive(Debug, Clone, Default)]
pub(crate) struct IgnoreScope {
    /// Innermost directory with rules. Each node links to the next ancestor
    /// that has any; directories without ignore files never appear.
    rules: Option<Arc<IgnoreNode>>,
    adaptation: GitIgnoreAdaptation,
    /// Behind an `Arc`: every queued directory carries a clone of its scope,
    /// and the two spellings inside are the same for the whole walk.
    candidate_root: Option<Arc<CandidateRoot>>,
}

/// The walk root as the caller spelled it and as repository discovery
/// resolved it, so candidates and ignore-file directories spelled below the
/// former can be matched against rules anchored below the latter.
#[derive(Debug, Clone)]
struct CandidateRoot {
    walk: Vec<u8>,
    walk_prefix: Vec<u8>,
    discovery: Vec<u8>,
    discovery_prefix: Vec<u8>,
    walk_path: PathBuf,
    discovery_path: PathBuf,
}

/// The glob-byte prefix every entry below `directory` begins with, matching
/// [`crate::push_entry_name`].
fn entry_prefix(directory: &[u8]) -> Vec<u8> {
    let mut prefix = directory.to_vec();
    if crate::needs_separator_before_name(&prefix) {
        prefix.push(b'/');
    }
    prefix
}

impl CandidateRoot {
    fn between(walk: &Path, discovery: &Path) -> Option<Self> {
        let walk_bytes = glob_path_bytes(walk).into_owned();
        let walk_prefix = entry_prefix(&walk_bytes);
        let mut discovery_bytes = glob_path_bytes(discovery).into_owned();
        while discovery_bytes.ends_with(b"//") {
            discovery_bytes.pop();
        }
        let discovery_prefix = entry_prefix(&discovery_bytes);
        (walk_prefix != discovery_prefix).then(|| Self {
            walk: walk_bytes,
            walk_prefix,
            discovery: discovery_bytes,
            discovery_prefix,
            walk_path: walk.to_path_buf(),
            discovery_path: discovery.to_path_buf(),
        })
    }

    /// The discovery spelling of a directory the walk entered.
    ///
    /// Derived lexically: the walk appended the directory's relative path to
    /// the root it was given, so the same relative path below the resolved
    /// root names it for discovery. Resolving each directory through the
    /// filesystem instead would follow a symlink at the directory itself and
    /// anchor its ignore rules somewhere the candidates below it are never
    /// spelled.
    fn map_directory(&self, directory: &Path) -> PathBuf {
        match directory.strip_prefix(&self.walk_path) {
            Ok(relative) => self.discovery_path.join(relative),
            Err(_) => directory.to_path_buf(),
        }
    }

    /// Spells a candidate given in the walk spelling in the discovery
    /// spelling, into `scratch`, which the caller keeps for the whole walk so
    /// that this costs one copy per entry and no allocation.
    fn spell_into<'a>(&self, candidate: &[u8], scratch: &'a mut Vec<u8>) -> &'a [u8] {
        scratch.clear();
        if let Some(relative) = candidate.strip_prefix(self.walk_prefix.as_slice()) {
            if relative.is_empty() {
                scratch.extend_from_slice(&self.discovery);
            } else {
                scratch.extend_from_slice(&self.discovery_prefix);
                scratch.extend_from_slice(relative);
            }
        } else if candidate == self.walk.as_slice() {
            scratch.extend_from_slice(&self.discovery);
        } else {
            scratch.extend_from_slice(candidate);
        }
        scratch
    }
}

impl IgnoreScope {
    /// What the walk root inherits: repository-wide excludes and the ignore
    /// files from the repository root through its parent. The root's own
    /// ignore files join like every other directory's, when the walk enters
    /// it, and being deeper they override these.
    pub(crate) fn for_root<B: DirectoryBackend + ?Sized>(
        walker: &Walker,
        backend: &B,
        root: &Path,
    ) -> (Self, Vec<IgnoreReadError>) {
        if !walker.respect_git_ignore {
            return (Self::default(), Vec::new());
        }
        // Repository discovery needs a complete ancestor chain even when the
        // public walk root is spelled as `.` or as one relative component.
        // Keep the original spelling in the walker itself: entry paths and
        // explicit-pattern anchoring are intentionally relative to that root.
        let discovery_root = discovery_path(root);
        let repository = repository_layout(&discovery_root);
        let adaptation =
            GitIgnoreAdaptation::effective(walker, repository.as_ref().map(|(_, layout)| layout));
        let mut scope = Self {
            rules: None,
            adaptation,
            candidate_root: CandidateRoot::between(root, &discovery_root).map(Arc::new),
        };
        let Some((repository_root, layout)) = repository else {
            return (scope, Vec::new());
        };
        let (rules, mut errors) = read_repository_rules(
            backend,
            &repository_root,
            Some(&layout),
            adaptation,
            scope.chain_usage(),
            IgnoreBudget::DEFAULT,
        );
        scope = scope.link(rules);
        for directory in ancestor_directories(&repository_root, &discovery_root) {
            let (rules, read_errors) = read_rules(
                backend,
                &directory,
                &directory,
                &IGNORE_FILES,
                adaptation,
                scope.chain_usage(),
                IgnoreBudget::DEFAULT,
            );
            scope = scope.link(rules);
            errors.extend(read_errors);
        }
        (scope, errors)
    }

    /// Adds `directory`'s own ignore files to the chain. Called once, when the
    /// walk enters the directory, with the listing it just read: a directory
    /// without ignore files is then recognized by name comparison instead of a
    /// failed open per candidate file.
    pub(crate) fn enter<B: DirectoryBackend + ?Sized>(
        self,
        walker: &Walker,
        backend: &B,
        directory: &Path,
        listing: &Listing,
    ) -> (Self, Vec<IgnoreReadError>) {
        if !walker.respect_git_ignore {
            return (self, Vec::new());
        }
        let present = IGNORE_FILES
            .into_iter()
            .filter(|file| listing.contains_git_ignore_name(file))
            .collect::<Vec<_>>();
        if present.is_empty() {
            return (self, Vec::new());
        }
        let match_directory = self.candidate_root.as_ref().map_or_else(
            || Cow::Borrowed(directory),
            |root| Cow::Owned(root.map_directory(directory)),
        );
        let (rules, errors) = read_rules(
            backend,
            directory,
            match_directory.as_ref(),
            &present,
            self.adaptation,
            self.chain_usage(),
            IgnoreBudget::DEFAULT,
        );
        (self.link(rules), errors)
    }

    /// Verdict for one entry of the directory this scope describes.
    ///
    /// The deepest ignore file with an opinion decides, which is Git's
    /// precedence. An entry below an ignored directory never reaches this: the
    /// walk does not enter such a directory.
    ///
    /// `scratch` is the walk's buffer for the candidate in the discovery
    /// spelling; see [`CandidateRoot::spell_into`].
    #[cfg(not(windows))]
    pub(crate) fn is_ignored(&self, path: &Path, is_dir: bool, scratch: &mut Vec<u8>) -> bool {
        let Some(node) = self.rules.as_ref() else {
            return false;
        };
        // Converted once for the whole chain: every set below slices its own
        // prefix off these bytes.
        // Repository discovery and ignore-file roots use lexical path
        // components, while a caller-supplied walk root can retain an inner
        // `.` in every emitted path. Align the spelling once for the whole
        // rule chain without resolving `..` or following the filesystem.
        let candidate = glob_path_bytes(path);
        match self.candidate_root.as_ref() {
            Some(root) => node.verdict(root.spell_into(&candidate, scratch), is_dir),
            None => node.verdict(&candidate, is_dir),
        }
        .unwrap_or(false)
    }

    #[cfg(windows)]
    pub(crate) fn is_ignored_bytes(
        &self,
        candidate: &[u8],
        is_dir: bool,
        scratch: &mut Vec<u8>,
    ) -> bool {
        let Some(node) = self.rules.as_ref() else {
            return false;
        };
        match self.candidate_root.as_ref() {
            Some(root) => node.verdict(root.spell_into(candidate, scratch), is_dir),
            None => node.verdict(candidate, is_dir),
        }
        .unwrap_or(false)
    }

    fn chain_usage(&self) -> ChainUsage {
        self.rules
            .as_ref()
            .map_or(ChainUsage::default(), |node| ChainUsage {
                compiled_units: node.cumulative_units,
                retained_bytes: node.cumulative_bytes,
            })
    }

    /// Puts `rules` on the chain, unless they are empty: an empty matcher can
    /// never have an opinion, so keeping it would only lengthen the walk of
    /// every entry below it.
    fn link(self, rules: RuleSet) -> Self {
        if rules.is_empty() {
            return self;
        }
        let inherited = self.chain_usage();
        let cumulative_units = inherited
            .compiled_units
            .saturating_add(rules.compiled_units());
        let cumulative_bytes = inherited
            .retained_bytes
            .saturating_add(rules.retained_bytes());
        Self {
            rules: Some(Arc::new(IgnoreNode {
                rules,
                cumulative_units,
                cumulative_bytes,
                parent: self.rules,
            })),
            adaptation: self.adaptation,
            candidate_root: self.candidate_root,
        }
    }
}

/// Resolves the walk root to the one spelling repository discovery and
/// ignore anchoring use: absolute, with `.` and `..` resolved and every
/// symlink followed, which is the directory Git reports as its working
/// directory there. One policy for every spelling: a root reached through a
/// symlink belongs to the repository that physically contains it, whether
/// the caller wrote `link`, `./link` or `work/../link`. A root that cannot
/// be canonicalized, because it does not exist, is normalized lexically so
/// the walk can still report its own error.
///
/// Called once per walk. The walker deliberately retains the caller's
/// spelling for filesystem operations and returned [`crate::WalkEntry`]
/// paths, and directories below the root derive their discovery spelling
/// from this one lexically, see [`CandidateRoot::map_directory`].
fn discovery_path(path: &Path) -> PathBuf {
    let Ok(absolute) = std::path::absolute(path) else {
        return path.to_path_buf();
    };
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return canonical;
    }
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

/// The directories whose in-tree rules a subtree walk inherits, from the
/// repository root down to (but excluding) its own root.
fn ancestor_directories(repository_root: &Path, root: &Path) -> Vec<PathBuf> {
    if root == repository_root {
        return Vec::new();
    }
    let mut directories = Vec::new();
    let mut directory = root.parent();
    while let Some(current) = directory {
        directories.push(current.to_path_buf());
        if current == repository_root {
            break;
        }
        directory = current.parent();
    }
    directories.reverse();
    directories
}

#[derive(Debug)]
struct IgnoreNode {
    rules: RuleSet,
    cumulative_units: usize,
    cumulative_bytes: usize,
    parent: Option<Arc<IgnoreNode>>,
}

impl Drop for IgnoreNode {
    fn drop(&mut self) {
        let mut current = self.parent.take();
        while let Some(mut inner) = current.and_then(Arc::into_inner) {
            current = inner.parent.take();
        }
    }
}

impl IgnoreNode {
    /// The verdict of the deepest node that matches, or `None` when no node in
    /// the chain matches. Each node sees the path relative to its own
    /// directory, which the matcher strips, so this costs one match per ignore
    /// file in force rather than one per ancestor directory.
    fn verdict(&self, candidate: &[u8], is_dir: bool) -> Option<bool> {
        let mut current = Some(self);
        while let Some(node) = current {
            if let Some(verdict) = node.rules.matched(candidate, is_dir) {
                return Some(verdict);
            }
            current = node.parent.as_deref();
        }
        None
    }
}

/// Reads the given ignore files of one directory into a single matcher.
///
/// A file that cannot be read is skipped: the read reports a missing file the
/// same way a separate existence check would, one syscall instead of two.
fn read_rules<B: DirectoryBackend + ?Sized>(
    backend: &B,
    directory: &Path,
    match_directory: &Path,
    files: &[&str],
    adaptation: GitIgnoreAdaptation,
    inherited: ChainUsage,
    budget: IgnoreBudget,
) -> (RuleSet, Vec<IgnoreReadError>) {
    let mut builder = RuleSetBuilder::new(
        match_directory,
        adaptation.case_insensitive,
        adaptation.precompose_unicode,
    );
    let mut errors = Vec::new();
    for file in files {
        let path = directory.join(file);
        match backend.read_ignore_file(&path) {
            Ok(BoundedFileRead::Contents(contents)) => {
                let chain_units_remaining = budget.max_chain_components.saturating_sub(
                    inherited
                        .compiled_units
                        .saturating_add(builder.compiled_units()),
                );
                let chain_bytes_remaining = budget.max_chain_bytes.saturating_sub(
                    inherited
                        .retained_bytes
                        .saturating_add(builder.retained_bytes()),
                );
                if let Err(source) = validate_rule_count(&contents).and_then(|()| {
                    add_rules_within_budget(
                        &mut builder,
                        &contents,
                        budget.max_file_components,
                        budget.max_file_bytes,
                        chain_units_remaining,
                        chain_bytes_remaining,
                    )
                }) {
                    errors.push(IgnoreReadError::new(path, source));
                }
            }
            Ok(BoundedFileRead::Missing | BoundedFileRead::NotRegular) => {}
            Err(source) => errors.push(IgnoreReadError::new(path, source)),
        }
    }
    (builder.build(), errors)
}

/// Reads the repository-wide exclude file for `root`.
///
/// A normal checkout has a `.git` directory. Linked worktrees and submodules
/// instead put a `gitdir: ...` pointer in `.git`; a linked worktree's private
/// git directory can in turn point at the common repository directory through
/// `commondir`. Git reads `info/exclude` from that common directory. Every
/// malformed or unreadable metadata file behaves like an unreadable exclude:
/// it contributes no rules.
fn read_repository_rules<B: DirectoryBackend + ?Sized>(
    backend: &B,
    root: &Path,
    layout: Option<&RepositoryLayout>,
    adaptation: GitIgnoreAdaptation,
    inherited: ChainUsage,
    budget: IgnoreBudget,
) -> (RuleSet, Vec<IgnoreReadError>) {
    let mut builder = RuleSetBuilder::new(
        root,
        adaptation.case_insensitive,
        adaptation.precompose_unicode,
    );
    let Some(layout) = layout else {
        return (builder.build(), Vec::new());
    };
    let path = layout.common_directory.join(REPOSITORY_EXCLUDE_FILE);
    let mut errors = Vec::new();
    match backend.read_repository_file(&path) {
        Ok(BoundedFileRead::Contents(contents)) => {
            let chain_units_remaining = budget.max_chain_components.saturating_sub(
                inherited
                    .compiled_units
                    .saturating_add(builder.compiled_units()),
            );
            let chain_bytes_remaining = budget.max_chain_bytes.saturating_sub(
                inherited
                    .retained_bytes
                    .saturating_add(builder.retained_bytes()),
            );
            if let Err(source) = validate_rule_count(&contents).and_then(|()| {
                add_rules_within_budget(
                    &mut builder,
                    &contents,
                    budget.max_file_components,
                    budget.max_file_bytes,
                    chain_units_remaining,
                    chain_bytes_remaining,
                )
            }) {
                errors.push(IgnoreReadError::new(path, source));
            }
        }
        Ok(BoundedFileRead::Missing | BoundedFileRead::NotRegular) => {}
        Err(source) => errors.push(IgnoreReadError::new(path, source)),
    }
    (builder.build(), errors)
}

fn validate_rule_count(contents: &[u8]) -> std::io::Result<()> {
    let terminated_lines = contents
        .iter()
        .filter(|&&byte| byte == b'\n')
        .take(MAX_IGNORE_RULES + 1)
        .count();
    let line_count =
        terminated_lines + usize::from(!contents.is_empty() && contents.last() != Some(&b'\n'));
    if line_count > MAX_IGNORE_RULES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ignore file exceeds the 100,000-rule safety limit",
        ));
    }
    Ok(())
}

/// Git's `setup.c::validate_headref` reads at most 255 bytes (`char buffer[256]`)
/// from `HEAD` to check whether it starts with a valid symbolic ref or OID.
const MAX_GIT_HEAD_PROBE_BYTES: u64 = 255;

fn has_git_head(directory: &Path) -> bool {
    let head = directory.join("HEAD");
    let Ok(metadata) = fs::symlink_metadata(&head) else {
        return false;
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        // With `core.preferSymlinkRefs=true`, Git writes `HEAD` as a symlink
        // targeting `refs/heads/<branch>`, which dangles on an unborn branch.
        // Any `HEAD` symlink whose target does not start with `refs/` is
        // rejected by Git's `validate_headref` without following it.
        return fs::read_link(&head).is_ok_and(|target| {
            let bytes = target.as_os_str().as_encoded_bytes();
            bytes.starts_with(b"refs/") || (cfg!(windows) && bytes.starts_with(b"refs\\"))
        });
    }
    if !file_type.is_file() {
        return false;
    }
    matches!(
        read_bounded_prefix_no_follow(&head, MAX_GIT_HEAD_PROBE_BYTES),
        Ok(BoundedFileRead::Contents(contents)) if is_valid_git_head_contents(&contents)
    )
}

/// Validates `HEAD` contents the way Git's `validate_headref` does: either
/// `ref:` followed by Git's `isspace` (`' '`, `'\t'`, `'\n'`, `'\r'`) and
/// `refs/`, or a detached hex object ID prefix (40 hex digits for SHA-1, which
/// also accepts 64-digit SHA-256 object IDs and trailing text).
fn is_valid_git_head_contents(contents: &[u8]) -> bool {
    if let Some(mut rest) = contents.strip_prefix(b"ref:") {
        while let Some((first, tail)) = rest.split_first() {
            if matches!(*first, b' ' | b'\t' | b'\n' | b'\r') {
                rest = tail;
            } else {
                break;
            }
        }
        return rest.starts_with(b"refs/");
    }
    contents.len() >= 40 && contents[..40].iter().all(u8::is_ascii_hexdigit)
}

fn is_searchable_directory(path: &Path) -> bool {
    // Appending `.` asks the OS to traverse `path` itself: on Unix this checks
    // search (`X_OK`) permission without unsafe `access(2)` calls, matching
    // Git's `access(path, X_OK)` check on `objects/` and `refs/`.
    fs::metadata(path.join(".")).is_ok_and(|metadata| metadata.is_dir())
}

fn is_git_private_directory(path: &Path) -> bool {
    is_searchable_directory(path) && has_git_head(path)
}

fn is_git_common_directory(path: &Path) -> bool {
    is_searchable_directory(path)
        && is_searchable_directory(&path.join("objects"))
        && is_searchable_directory(&path.join("refs"))
}

/// The private git directory and its common directory. They are the same for
/// ordinary checkouts; linked worktrees use a private directory below
/// `worktrees/` and a shared common directory.
#[derive(Debug)]
struct RepositoryLayout {
    private_directory: PathBuf,
    common_directory: PathBuf,
}

/// Resolves the Git directories relevant to repository-local config and the
/// repository-wide exclude file.
///
/// Only the exact `gitdir: ` pointer format Git writes is accepted. The path
/// has one line, may be absolute or relative to the pointer file, and may use
/// a single `commondir` indirection relative to the resulting git directory.
/// Pointer targets are verified to carry Git's directory structure (`HEAD` in
/// the private directory, plus `objects/` and `refs/` in the common directory)
/// before any metadata is read from them.
fn repository_layout(root: &Path) -> Option<(PathBuf, RepositoryLayout)> {
    let mut directory = Some(root);
    while let Some(candidate) = directory {
        let dot_git = candidate.join(".git");
        let metadata = match fs::metadata(&dot_git) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                directory = candidate.parent();
                continue;
            }
            Err(_) => return None,
        };
        return repository_layout_at(candidate, dot_git, metadata)
            .map(|layout| (candidate.to_path_buf(), layout));
    }
    None
}

fn repository_layout_at(
    root: &Path,
    dot_git: PathBuf,
    metadata: fs::Metadata,
) -> Option<RepositoryLayout> {
    if metadata.is_dir() {
        return Some(RepositoryLayout {
            private_directory: dot_git.clone(),
            common_directory: dot_git,
        });
    }
    if !metadata.is_file() {
        return None;
    }

    let Ok(BoundedFileRead::Contents(pointer_contents)) = read_bounded_file(&dot_git) else {
        return None;
    };
    let private_directory = resolve_metadata_path(root, parse_gitdir_pointer(&pointer_contents)?)?;
    if !is_git_private_directory(&private_directory) {
        return None;
    }
    let common_dir = private_directory.join("commondir");
    let common_directory = match read_bounded_file(&common_dir) {
        Ok(BoundedFileRead::Contents(contents)) => {
            resolve_metadata_path(&private_directory, parse_metadata_path(&contents)?)?
        }
        Ok(BoundedFileRead::Missing) => private_directory.clone(),
        Ok(BoundedFileRead::NotRegular) | Err(_) => return None,
    };
    if !is_git_common_directory(&common_directory) {
        return None;
    }
    Some(RepositoryLayout {
        private_directory,
        common_directory,
    })
}

#[derive(Default)]
struct GitConfig {
    ignore_case: Option<bool>,
    precompose_unicode: Option<bool>,
    worktree_config: Option<bool>,
}

#[derive(Clone, Copy)]
enum GitConfigSection {
    Core,
    Extensions,
}

impl GitIgnoreAdaptation {
    /// Repository-local config is deliberately the whole implicit surface.
    /// Git also reads system/global files, includes and environment
    /// overrides; a library cannot safely inherit those process-wide settings,
    /// so callers may provide the final effective values on `Walker`.
    fn effective(walker: &Walker, layout: Option<&RepositoryLayout>) -> Self {
        let config = layout.map(read_repository_config).unwrap_or_default();
        Self {
            case_insensitive: walker
                .git_ignore_case
                .or(config.ignore_case)
                .unwrap_or(false),
            // Git implements this adaptation only on macOS. Keeping that gate
            // here means `git_precompose_unicode(true)` expresses Git's
            // effective configuration rather than asking another platform to
            // invent a filesystem transformation Git itself would not make.
            precompose_unicode: cfg!(target_os = "macos")
                && walker
                    .git_precompose_unicode
                    .or(config.precompose_unicode)
                    .unwrap_or(false),
        }
    }
}

fn read_repository_config(layout: &RepositoryLayout) -> GitConfig {
    let mut config = GitConfig::default();
    if let Ok(BoundedFileRead::Contents(contents)) =
        read_bounded_file(&layout.common_directory.join("config"))
    {
        apply_config(&mut config, &contents);
    }
    if config.worktree_config == Some(true)
        && let Ok(BoundedFileRead::Contents(contents)) =
            read_bounded_file(&layout.private_directory.join("config.worktree"))
    {
        // Worktree config is read after the common config, so its last value
        // wins just as `git config --worktree` does.
        apply_config(&mut config, &contents);
    }
    config
}

/// Reads the tiny, stable config surface this adapter needs. Git config names
/// are case-insensitive; values follow Git's boolean grammar, including empty
/// and base-zero integer values. Quoted subsections deliberately remain
/// distinct from their top-level section, and backslash-newline continuations
/// are joined before comments, quoting, escapes, or boolean decoding. Unsupported
/// config forms leave the repository-local default intact rather than guessing.
fn apply_config(config: &mut GitConfig, contents: &[u8]) {
    let mut section = None;
    for_each_logical_config_line(contents, |line| {
        let mut line = trim_ascii(strip_config_comment(line));
        if line.is_empty() {
            return;
        }
        if line.starts_with(b"[") {
            let Some((parsed_section, remainder)) = parse_config_header(line) else {
                section = None;
                return;
            };
            section = parsed_section;
            line = trim_ascii(remainder);
            if line.is_empty() {
                return;
            }
        }
        let Some(section) = section else {
            return;
        };
        let (key, value) =
            line.iter()
                .position(|byte| *byte == b'=')
                .map_or((trim_ascii(line), None), |index| {
                    (
                        trim_ascii(&line[..index]),
                        Some(trim_ascii(&line[index + 1..])),
                    )
                });
        let target = match section {
            GitConfigSection::Core if key.eq_ignore_ascii_case(b"ignorecase") => {
                &mut config.ignore_case
            }
            GitConfigSection::Core if key.eq_ignore_ascii_case(b"precomposeunicode") => {
                &mut config.precompose_unicode
            }
            GitConfigSection::Extensions if key.eq_ignore_ascii_case(b"worktreeconfig") => {
                &mut config.worktree_config
            }
            _ => return,
        };
        if let Some(value) = parse_git_bool(value.unwrap_or(b"true")) {
            *target = Some(value);
        }
    });
}

/// Splits a section header from a variable written on the same physical line.
/// Git resumes its character-based parser immediately after the closing `]`,
/// so both `[core] key = value` and `[core]key = value` leave `core` active.
fn parse_config_header(line: &[u8]) -> Option<(Option<GitConfigSection>, &[u8])> {
    let end = line.iter().position(|byte| *byte == b']')?;
    Some((parse_top_level_section(&line[..=end]), &line[end + 1..]))
}

/// Streams the logical config lines that Git's value parser sees. A terminal
/// backslash in an assignment value consumes the following physical newline,
/// then parsing resumes with the next line's bytes (including indentation).
/// It works inside and outside quotes, but a backslash in a comment is ignored.
/// A terminal backslash at EOF is consumed with Git's synthetic final newline.
/// Every other quote and escape is retained for `parse_git_bool`.
///
/// Uncontinued lines are passed straight to `f` as borrowed slices into
/// `contents` without allocating. Continued lines reuse a single scratch buffer
/// cleared between logical lines.
fn for_each_logical_config_line(contents: &[u8], mut f: impl FnMut(&[u8])) {
    let mut physical = contents.split_inclusive(|byte| *byte == b'\n').map(|line| {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        line.strip_suffix(b"\r").unwrap_or(line)
    });
    let mut buffer = Vec::new();
    while let Some(line) = physical.next() {
        let Some(mut state) = config_value_continuation(line) else {
            f(line);
            continue;
        };
        let prefix = line.strip_suffix(b"\\").unwrap_or(line);
        let Some(mut next) = physical.next() else {
            f(prefix);
            continue;
        };
        buffer.clear();
        buffer.extend_from_slice(prefix);
        loop {
            buffer.extend_from_slice(next);
            let Some(next_state) = config_value_suffix_continuation(next, state) else {
                break;
            };
            buffer.pop();
            let Some(following) = physical.next() else {
                break;
            };
            state = next_state;
            next = following;
        }
        f(&buffer);
    }
}

/// Quote state at the end of a continued config-value fragment.
#[derive(Clone, Copy)]
struct ConfigValueState {
    quoted: bool,
}

/// Finds an initial assignment value and carries its quote state over a
/// backslash continuation. Subsequent physical lines are scanned only once.
fn config_value_continuation(line: &[u8]) -> Option<ConfigValueState> {
    let line = trim_ascii_start(line);
    let line = if line.starts_with(b"[") {
        trim_ascii_start(parse_config_header(line)?.1)
    } else {
        line
    };
    let first = line.first()?;
    if matches!(first, b'#' | b';') || !first.is_ascii_alphabetic() {
        return None;
    }
    let key_end = line
        .iter()
        .position(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'-'))
        .unwrap_or(line.len());
    let value = trim_ascii_start(&line[key_end..]).strip_prefix(b"=")?;
    config_value_suffix_continuation(value, ConfigValueState { quoted: false })
}

/// Scans one physical suffix using the quote state left by its predecessor.
/// A terminal unescaped backslash is removed by the caller before the next
/// suffix is appended, so no escape state itself needs to cross a line break.
fn config_value_suffix_continuation(
    value: &[u8],
    mut state: ConfigValueState,
) -> Option<ConfigValueState> {
    let mut bytes = value.iter();
    while let Some(byte) = bytes.next() {
        if *byte == b'\\' {
            if bytes.next().is_none() {
                return Some(state);
            }
        } else if *byte == b'"' {
            state.quoted = !state.quoted;
        } else if !state.quoted && matches!(*byte, b'#' | b';') {
            return None;
        }
    }
    None
}

/// Returns only the top-level sections whose variables this adapter consumes.
/// Git gives a quoted subsection a dotted key prefix, so it must never alias
/// that subsection with its parent section.
fn parse_top_level_section(line: &[u8]) -> Option<GitConfigSection> {
    let name = line.strip_prefix(b"[")?.strip_suffix(b"]")?;
    if name.eq_ignore_ascii_case(b"core") {
        Some(GitConfigSection::Core)
    } else if name.eq_ignore_ascii_case(b"extensions") {
        Some(GitConfigSection::Extensions)
    } else {
        None
    }
}

fn strip_config_comment(line: &[u8]) -> &[u8] {
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in line.iter().enumerate() {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if *byte == b'"' {
            quoted = !quoted;
        } else if !quoted && matches!(*byte, b'#' | b';') {
            return &line[..index];
        }
    }
    line
}

fn parse_git_bool(value: &[u8]) -> Option<bool> {
    let value = std::str::from_utf8(value).ok()?;
    let value = decode_git_config_value(value.trim())?;
    if value.is_empty() {
        Some(false)
    } else if value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("yes")
        || value.eq_ignore_ascii_case("on")
    {
        Some(true)
    } else if value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("no")
        || value.eq_ignore_ascii_case("off")
    {
        Some(false)
    } else {
        parse_git_config_int(&value).map(|value| value != 0)
    }
}

fn trim_ascii_start(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    &bytes[start..]
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let bytes = trim_ascii_start(bytes);
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(0, |index| index + 1);
    &bytes[..end]
}

/// Decodes the quoted escapes that Git's config parser resolves before it
/// hands a value to its boolean parser. Whitespace outside quotes is already
/// trimmed by the caller; whitespace inside quotes deliberately remains part
/// of the value and therefore makes a boolean invalid, as it does in Git.
fn decode_git_config_value(value: &str) -> Option<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut quoted = false;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            decoded.push(match character {
                't' => '\t',
                'b' => '\u{8}',
                'n' => '\n',
                '\\' | '"' => character,
                _ => return None,
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else {
            decoded.push(character);
        }
    }
    (!quoted && !escaped).then_some(decoded)
}

/// Git parses a numeric boolean through its signed `int` config grammar:
/// optional sign, C base-zero notation, and K/M/G binary scaling. Values that
/// overflow that `int` are invalid rather than silently changing the setting.
fn parse_git_config_int(value: &str) -> Option<i32> {
    let (negative, value) = match value.as_bytes().first() {
        Some(b'+') => (false, &value[1..]),
        Some(b'-') => (true, &value[1..]),
        _ => (false, value),
    };
    let (digits, scale) = match value.as_bytes().last() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1024_i64),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1024_i64.pow(2)),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1024_i64.pow(3)),
        _ => (value, 1),
    };
    let (radix, digits) = if let Some(digits) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, digits)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8, &digits[1..])
    } else {
        (10, digits)
    };
    let magnitude = i64::from_str_radix(digits, radix)
        .ok()?
        .checked_mul(scale)?;
    let value = if negative {
        magnitude.checked_neg()?
    } else {
        magnitude
    };
    i32::try_from(value).ok()
}

fn parse_gitdir_pointer(contents: &[u8]) -> Option<PathBuf> {
    parse_metadata_path(contents.strip_prefix(b"gitdir: ")?)
}

/// Parses the one native path Git writes into a `gitdir` or `commondir` file.
/// Rejecting additional lines and NUL keeps a malformed metadata file from
/// being interpreted as a path by accident.
fn parse_metadata_path(contents: &[u8]) -> Option<PathBuf> {
    let contents = contents.strip_suffix(b"\n").unwrap_or(contents);
    let contents = contents.strip_suffix(b"\r").unwrap_or(contents);
    if contents.is_empty()
        || contents
            .iter()
            .any(|byte| matches!(*byte, b'\0' | b'\n' | b'\r'))
    {
        return None;
    }
    path_from_git_bytes(contents)
}

#[cfg(unix)]
fn path_from_git_bytes(contents: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    Some(PathBuf::from(std::ffi::OsString::from_vec(
        contents.to_vec(),
    )))
}

#[cfg(not(unix))]
fn path_from_git_bytes(contents: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(contents).ok().map(PathBuf::from)
}

fn resolve_metadata_path(base: &Path, path: PathBuf) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path)
    } else if path.as_os_str().is_empty() {
        None
    } else {
        Some(base.join(path))
    }
}

/// Feeds one ignore file's lines to the builder within both the per-file and
/// remaining chain component budgets.
///
/// Ignore files are byte streams. Git strips one BOM at the file start, treats
/// NUL as the end of a rule line, and otherwise keeps parsing after invalid
/// UTF-8 bytes. When any line breaches a budget, every rule contributed by this
/// file is rolled back so an offending file is never partially applied.
fn add_rules_within_budget(
    builder: &mut RuleSetBuilder,
    contents: &[u8],
    max_file_components: usize,
    max_file_bytes: usize,
    max_chain_remaining: usize,
    max_chain_bytes_remaining: usize,
) -> std::io::Result<()> {
    let checkpoint = builder.checkpoint();
    let contents = contents.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(contents);
    let mut file_parsed_parts = 0_usize;
    let mut file_compiled_units = 0_usize;
    let mut file_retained_bytes = 0_usize;
    for line in contents.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let line = line.split(|byte| *byte == b'\0').next().unwrap_or(line);
        let budgets = LineBudgets {
            file_parsed_remaining: max_file_components.saturating_sub(file_parsed_parts),
            file_compiled_remaining: max_file_components.saturating_sub(file_compiled_units),
            file_bytes_remaining: max_file_bytes.saturating_sub(file_retained_bytes),
            chain_compiled_remaining: max_chain_remaining.saturating_sub(file_compiled_units),
            chain_bytes_remaining: max_chain_bytes_remaining.saturating_sub(file_retained_bytes),
        };
        match builder.add_line_within_budget(line, budgets) {
            Ok(usage) => {
                file_parsed_parts += usage.parsed_parts;
                file_compiled_units += usage.compiled_units;
                file_retained_bytes += usage.retained_bytes;
            }
            Err(exceeded) => {
                builder.restore(checkpoint);
                let message = match exceeded {
                    ComponentBudgetExceeded::File => {
                        "ignore file exceeds the 250,000-component safety limit"
                    }
                    ComponentBudgetExceeded::FileBytes => {
                        "ignore file exceeds the 256 MiB retained matcher safety limit"
                    }
                    ComponentBudgetExceeded::Chain => {
                        "ignore rule chain exceeds the 1,000,000-component safety limit"
                    }
                    ComponentBudgetExceeded::ChainBytes => {
                        "ignore rule chain exceeds the 512 MiB retained matcher safety limit"
                    }
                };
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    message,
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn add_rules(builder: &mut RuleSetBuilder, contents: &[u8]) {
    add_rules_within_budget(
        builder,
        contents,
        MAX_IGNORE_FILE_COMPONENTS,
        MAX_IGNORE_FILE_RETAINED_BYTES,
        MAX_IGNORE_CHAIN_COMPONENTS,
        MAX_IGNORE_CHAIN_BYTES,
    )
    .expect("test ignore rules fit within safety budget");
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        BoundedFileRead, CandidateRoot, GitConfig, GitIgnoreAdaptation, IGNORE_FILES, IgnoreBudget,
        IgnoreScope, MAX_IGNORE_FILE_COMPONENTS, MAX_IGNORE_RULES, RuleSetBuilder, add_rules,
        add_rules_within_budget, apply_config, for_each_logical_config_line,
        is_valid_git_head_contents, parse_git_bool, read_rules, validate_rule_count,
    };

    #[test]
    fn git_head_contents_validation_matches_git_headref_forms() {
        assert!(is_valid_git_head_contents(b"ref: refs/heads/main\n"));
        assert!(is_valid_git_head_contents(b"ref:\trefs/heads/main\r\n"));
        assert!(is_valid_git_head_contents(b"ref:\nrefs/heads/main\n"));
        assert!(is_valid_git_head_contents(b"ref:\r\nrefs/heads/main\n"));
        assert!(is_valid_git_head_contents(b"ref: refs/heads/.invalid\n"));
        assert!(is_valid_git_head_contents(
            b"0123456789abcdef0123456789abcdef01234567\n"
        ));
        assert!(is_valid_git_head_contents(
            b"0123456789abcdef0123456789abcdef01234567 trailing\n"
        ));
        assert!(is_valid_git_head_contents(
            b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\r\n"
        ));

        assert!(!is_valid_git_head_contents(b""));
        assert!(!is_valid_git_head_contents(b"\n"));
        assert!(!is_valid_git_head_contents(b"not a git head\n"));
        assert!(!is_valid_git_head_contents(b"ref: ../outside\n"));
        assert!(!is_valid_git_head_contents(b"ref:\x0brefs/heads/main\n"));
        assert!(!is_valid_git_head_contents(b"ref:\x0crefs/heads/main\n"));
        assert!(!is_valid_git_head_contents(
            b"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n"
        ));
        assert!(!is_valid_git_head_contents(
            b"0123456789abcdef0123456789abcdef0123456\n"
        ));
    }

    #[test]
    fn candidate_root_maps_slash_roots_and_repeated_trailing_slashes() {
        let mut scratch = Vec::new();

        // Windows drive-root walk spelled `/` or `\` against `C:/` or `//?/C:/`.
        for discovery in [Path::new("C:/"), Path::new("//?/C:/")] {
            let root = CandidateRoot::between(Path::new("/"), discovery)
                .expect("drive-relative root requires spelling translation");
            let expected_file = format!("{}foo.txt", discovery.display());
            let expected_nested = format!("{}sub/bar.txt", discovery.display());
            assert_eq!(
                root.spell_into(b"/foo.txt", &mut scratch),
                expected_file.as_bytes()
            );
            assert_eq!(
                root.spell_into(b"/sub/bar.txt", &mut scratch),
                expected_nested.as_bytes()
            );
            assert_eq!(
                root.spell_into(b"/", &mut scratch),
                discovery.as_os_str().as_encoded_bytes()
            );

            let mut builder = RuleSetBuilder::new(discovery, false, false);
            builder.add_line("/foo.txt");
            builder.add_line("sub/bar.txt");
            let rules = builder.build();
            assert_eq!(
                rules.matched(root.spell_into(b"/foo.txt", &mut scratch), false),
                Some(true)
            );
            assert_eq!(
                rules.matched(root.spell_into(b"/sub/bar.txt", &mut scratch), false),
                Some(true)
            );
            assert_eq!(
                rules.matched(root.spell_into(b"/deep/foo.txt", &mut scratch), false),
                None
            );
        }

        // Unix `.//`, `.//.`, `/repo//`, `repo//`, `./repo//`, `//`, `/repo/./sub`.
        for (walk, discovery, entry, expected) in [
            (
                ".//",
                "/repo",
                b".//foo.txt".as_slice(),
                b"/repo/foo.txt".as_slice(),
            ),
            (".//", "/repo", b".//sub/bar.txt", b"/repo/sub/bar.txt"),
            (".//.", "/repo", b".//./foo.txt", b"/repo/foo.txt"),
            (".", "/repo", b"./foo.txt", b"/repo/foo.txt"),
            ("./", "/repo", b"./foo.txt", b"/repo/foo.txt"),
            ("/repo//", "/repo", b"/repo//foo.txt", b"/repo/foo.txt"),
            (
                "/repo//",
                "/repo",
                b"/repo//sub/bar.txt",
                b"/repo/sub/bar.txt",
            ),
            ("/repo//.", "/repo", b"/repo//./foo.txt", b"/repo/foo.txt"),
            (
                "repo//",
                "/tmp/repo",
                b"repo//foo.txt",
                b"/tmp/repo/foo.txt",
            ),
            (
                "./repo//",
                "/tmp/repo",
                b"./repo//foo.txt",
                b"/tmp/repo/foo.txt",
            ),
            ("//", "/", b"//foo.txt", b"/foo.txt"),
            ("//", "/", b"//sub/bar.txt", b"/sub/bar.txt"),
            (
                "/repo/./sub",
                "/repo/sub",
                b"/repo/./sub/foo.txt",
                b"/repo/sub/foo.txt",
            ),
        ] {
            let root = CandidateRoot::between(Path::new(walk), Path::new(discovery))
                .unwrap_or_else(|| panic!("expected CandidateRoot for {walk:?} -> {discovery:?}"));
            assert_eq!(
                root.spell_into(entry, &mut scratch),
                expected,
                "{walk:?} -> {discovery:?}"
            );
        }

        // Fast-path identity spellings need no per-entry translation.
        for (walk, discovery) in [
            ("/repo", "/repo"),
            ("/repo/", "/repo"),
            ("/", "/"),
            ("C:/", "C:/"),
            ("//server/share/", "//server/share/"),
        ] {
            assert!(
                CandidateRoot::between(Path::new(walk), Path::new(discovery)).is_none(),
                "expected no CandidateRoot for {walk:?} -> {discovery:?}"
            );
        }

        #[cfg(windows)]
        {
            for (walk, discovery, entry, expected) in [
                (
                    r"\",
                    r"C:\",
                    b"/foo.txt".as_slice(),
                    b"C:/foo.txt".as_slice(),
                ),
                (r"C:", r"C:\repo", b"C:foo.txt", b"C:/repo/foo.txt"),
                (r"C:.", r"C:\repo", b"C:./foo.txt", b"C:/repo/foo.txt"),
                (r"C:\\", r"C:\", b"C://foo.txt", b"C:/foo.txt"),
                (
                    r"C:\repo\\",
                    r"C:\repo",
                    b"C:/repo//foo.txt",
                    b"C:/repo/foo.txt",
                ),
                (
                    r"\\server\share\\",
                    r"\\server\share\",
                    b"//server/share//foo.txt",
                    b"//server/share/foo.txt",
                ),
            ] {
                let root = CandidateRoot::between(Path::new(walk), Path::new(discovery))
                    .unwrap_or_else(|| {
                        panic!("expected Windows CandidateRoot for {walk:?} -> {discovery:?}")
                    });
                assert_eq!(
                    root.spell_into(entry, &mut scratch),
                    expected,
                    "{walk:?} -> {discovery:?}"
                );
            }
        }
    }

    #[test]
    fn ignore_rule_count_has_a_hard_limit() {
        let at_limit = vec![b'\n'; MAX_IGNORE_RULES];
        assert!(validate_rule_count(&at_limit).is_ok());

        let over_limit = vec![b'\n'; MAX_IGNORE_RULES + 1];
        let error = validate_rule_count(&over_limit).expect_err("one extra rule is rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn byte_lines_continue_after_invalid_utf8_and_match_byte_patterns() {
        let root = Path::new("/fixture");
        let mut builder = RuleSetBuilder::new(root, false, false);
        add_rules(&mut builder, b"first.txt\n\xE9latin1.txt\nsecond.txt\n");
        let rules = builder.build();

        assert_eq!(rules.matched(b"/fixture/second.txt", false), Some(true));
        assert_eq!(rules.matched(b"/fixture/\xE9latin1.txt", false), Some(true));
    }

    #[test]
    fn nul_ends_a_rule_and_one_initial_bom_is_stripped() {
        let root = Path::new("/fixture");
        let mut builder = RuleSetBuilder::new(root, false, false);
        add_rules(
            &mut builder,
            b"\xEF\xBB\xBF\xEF\xBB\xBFdouble.txt\r\nsec\0ret.txt\r\n",
        );
        let rules = builder.build();

        assert_eq!(
            rules.matched(b"/fixture/\xEF\xBB\xBFdouble.txt", false),
            Some(true)
        );
        assert_eq!(rules.matched(b"/fixture/double.txt", false), None);
        assert_eq!(rules.matched(b"/fixture/sec", false), Some(true));
        assert_eq!(rules.matched(b"/fixture/secret.txt", false), None);
    }

    #[test]
    fn local_config_is_case_insensitive_and_uses_the_last_boolean_value() {
        let mut config = GitConfig::default();
        apply_config(
            &mut config,
            b"[CoRe]\nignoreCase = on\nprecomposeUnicode = 0 # comment\n\
              ignorecase = false\n[ExTeNsIoNs]\nworktreeConfig\n",
        );
        assert_eq!(config.ignore_case, Some(false));
        assert_eq!(config.precompose_unicode, Some(false));
        assert_eq!(config.worktree_config, Some(true));
    }

    #[test]
    fn local_config_boolean_values_follow_git_grammar() {
        for (value, expected) in [
            ("", Some(false)),
            ("\"\"", Some(false)),
            ("true", Some(true)),
            ("No", Some(false)),
            ("+0", Some(false)),
            ("-0", Some(false)),
            ("+2", Some(true)),
            ("-7", Some(true)),
            ("0x0", Some(false)),
            ("0x2", Some(true)),
            ("010", Some(true)),
            ("1G", Some(true)),
            ("2G", None),
            ("09", None),
            ("1.0", None),
            ("invalid", None),
        ] {
            assert_eq!(parse_git_bool(value.as_bytes()), expected, "{value:?}");
        }
    }

    #[test]
    fn invalid_later_boolean_does_not_override_a_valid_value() {
        let mut config = GitConfig::default();
        apply_config(
            &mut config,
            b"[core]\nignorecase = false\nignorecase = not-a-bool\n",
        );
        assert_eq!(config.ignore_case, Some(false));
    }

    #[test]
    fn config_subsections_and_continuations_follow_git_value_boundaries() {
        let mut config = GitConfig::default();
        apply_config(
            &mut config,
            b"[core \"unrelated\"]\nignorecase = false\nprecomposeunicode = false\n\
              [extensions \"unrelated\"]\nworktreeconfig = false\n  [CoRe] # top-level comment\nignorecase = f\\\nalse\nprecomposeunicode = \"tr\\\nue\" # comment\n\
              [extensions]\nworktreeconfig = f\\\nalse\nworktreeconfig = t\\\nr\\\nue\n",
        );

        assert_eq!(config.ignore_case, Some(false));
        assert_eq!(config.precompose_unicode, Some(true));
        assert_eq!(config.worktree_config, Some(true));

        apply_config(
            &mut config,
            b"[core]\nignorecase = true\nignorecase = f\\\n  alse\n\
              precomposeunicode = true\nprecomposeunicode = tr\\\n# comment\n\
              [extensions]\nworktreeconfig = true\nworktreeconfig = tr\\",
        );

        // Continuation indentation is not trimmed, a continued comment leaves
        // the preceding invalid fragment, and an EOF backslash is consumed.
        // None is a valid boolean, so all earlier values remain in force.
        assert_eq!(config.ignore_case, Some(true));
        assert_eq!(config.precompose_unicode, Some(true));
        assert_eq!(config.worktree_config, Some(true));

        let mut continued_header = GitConfig::default();
        apply_config(
            &mut continued_header,
            b"[core]\nignorecase = true\nignorecase = f\\\n[extensions]\nworktreeconfig = true\n",
        );
        // The continued value consumes the section-looking line, so its invalid
        // boolean cannot replace `true` and the following key remains in core.
        assert_eq!(continued_header.ignore_case, Some(true));
        assert_eq!(continued_header.worktree_config, None);
    }

    #[test]
    fn config_header_assignments_keep_the_section_active() {
        let mut config = GitConfig::default();
        apply_config(
            &mut config,
            b"[core] ignorecase = true\nprecomposeunicode = true\n\
              [extensions] worktreeconfig = true\n\
              [core]ignorecase = false\nprecomposeunicode = false\n",
        );

        assert_eq!(config.ignore_case, Some(false));
        assert_eq!(config.precompose_unicode, Some(false));
        assert_eq!(config.worktree_config, Some(true));

        let mut continued = GitConfig::default();
        apply_config(&mut continued, b"[core] ignorecase = tru\\\ne\n");
        assert_eq!(continued.ignore_case, Some(true));
    }

    #[test]
    fn long_config_continuations_are_joined_without_rescanning_the_prefix() {
        const CONTINUATION_LINES: usize = 1 << 18;

        let mut contents = Vec::with_capacity(8 + CONTINUATION_LINES * 4);
        contents.extend_from_slice(b"[core]\nk=");
        for _ in 0..CONTINUATION_LINES {
            contents.extend_from_slice(b"aa\\\n");
        }
        contents.extend_from_slice(b"true\n");

        let mut count = 0;
        let mut second_len = 0;
        let mut second_ends_with_true = false;
        for_each_logical_config_line(&contents, |line| {
            if count == 1 {
                second_len = line.len();
                second_ends_with_true = line.ends_with(b"true");
            }
            count += 1;
        });
        assert_eq!(count, 2);
        assert!(second_ends_with_true);
        assert_eq!(second_len, 2 + CONTINUATION_LINES * 2 + 4);
    }

    #[test]
    fn config_parser_keeps_core_values_across_non_utf8_irrelevant_lines() {
        let mut config = GitConfig::default();
        apply_config(
            &mut config,
            b"# comment before \xff\n[user]\nname = Jos\xe9\nemail = user\\\n\xff@example.test\n\
              [core]\nignorecase = true\nprecomposeunicode = false\n\
              [remote]\nurl = ssh://\xff@example.test\n# comment after \xff\n",
        );

        assert_eq!(config.ignore_case, Some(true));
        assert_eq!(config.precompose_unicode, Some(false));
    }

    #[test]
    fn config_parser_rejects_non_utf8_relevant_boolean_values() {
        let mut config = GitConfig::default();
        apply_config(
            &mut config,
            b"[core]\nignorecase = false\nignorecase = tr\xffue\n\
              precomposeunicode = true\nprecomposeunicode = fa\xfflse\n",
        );

        assert_eq!(config.ignore_case, Some(false));
        assert_eq!(config.precompose_unicode, Some(true));
    }

    #[test]
    fn deep_ignore_node_chain_evaluates_and_drops_iteratively() {
        use std::sync::Arc;

        const DEPTH: usize = 25_000;

        let handle = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let root = Path::new("/fixture");
                let build_rules = |contents: &[u8]| {
                    let mut builder = RuleSetBuilder::new(root, false, false);
                    if !contents.is_empty() {
                        add_rules(&mut builder, contents);
                    }
                    builder.build()
                };

                let root_rules = build_rules(b"*.log\n");
                let mut cumulative_units = root_rules.compiled_units();
                let mut cumulative_bytes = root_rules.retained_bytes();
                let shared_root = Arc::new(super::IgnoreNode {
                    rules: root_rules,
                    cumulative_units,
                    cumulative_bytes,
                    parent: None,
                });

                let mut head = Arc::clone(&shared_root);
                for depth in 1..=DEPTH {
                    let rules = if depth == DEPTH / 2 {
                        build_rules(b"!keep.log\n")
                    } else if depth == DEPTH {
                        build_rules(b"other.tmp\n")
                    } else {
                        build_rules(b"")
                    };
                    cumulative_units += rules.compiled_units();
                    cumulative_bytes += rules.retained_bytes();
                    head = Arc::new(super::IgnoreNode {
                        rules,
                        cumulative_units,
                        cumulative_bytes,
                        parent: Some(head),
                    });
                }

                // Deepest matching node wins, and full-chain traversal does not
                // overflow the 256 KiB thread stack.
                assert_eq!(head.verdict(b"/fixture/other.tmp", false), Some(true));
                assert_eq!(head.verdict(b"/fixture/keep.log", false), Some(false));
                assert_eq!(head.verdict(b"/fixture/app.log", false), Some(true));
                assert_eq!(head.verdict(b"/fixture/unmatched.txt", false), None);

                // Dropping the tail unwinds all 25,000 uniquely-owned nodes
                // iteratively and leaves the shared root intact.
                drop(head);
                assert_eq!(shared_root.verdict(b"/fixture/keep.log", false), Some(true));
                drop(shared_root);
            })
            .expect("spawn small-stack thread");

        handle
            .join()
            .expect("deep IgnoreNode verdict and drop succeed on a 256 KiB stack");
    }

    #[test]
    fn ignore_file_component_budget_rolls_back_offending_file_atomically() {
        let root = Path::new("/fixture");
        let mut builder = RuleSetBuilder::new(root, false, false);

        // First file adds valid rules (`*.log` = `AnyDirs` + `*.log` = 2 components).
        add_rules_within_budget(&mut builder, b"*.log\n", 10, usize::MAX, 100, usize::MAX)
            .expect("small valid ignore file succeeds");
        assert_eq!(builder.compiled_units(), 2);

        // Second file adds many valid rules (growing `rules`'s backing buffer)
        // before breaching the per-file component budget on its final line.
        // Rollback must drop those rules AND release the excess vector capacity
        // while preserving the first file's rules.
        let mut offending = String::new();
        for index in 0..32 {
            offending.push_str(&format!("keep-{index}.txt\n"));
        }
        offending.push_str("a/b/c/d/e/f/g/h/i\n");
        let error = add_rules_within_budget(
            &mut builder,
            offending.as_bytes(),
            70,
            usize::MAX,
            200,
            usize::MAX,
        )
        .expect_err("over-budget file is rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(builder.compiled_units(), 2);
        assert_eq!(
            builder.rules_capacity(),
            1,
            "rollback must release excess backing capacity from rejected rules"
        );

        let rules = builder.build();
        assert_eq!(rules.matched(b"/fixture/app.log", false), Some(true));
        assert_eq!(rules.matched(b"/fixture/keep-0.txt", false), None);

        // Parsed-part and compiled-unit totals are tracked independently:
        // 5 pairs of `**/**/**/x` (4 parsed, 2 compiled) + `y` (1 parsed, 2
        // compiled) total 25 parsed parts and 20 compiled units — both within
        // a 25-unit file budget even though summing per-line maxima would be
        // 5 * (4 + 2) = 30 > 25.
        let mut paired = RuleSetBuilder::new(root, false, false);
        let paired_contents = "**/**/**/x\ny\n".repeat(5);
        add_rules_within_budget(
            &mut paired,
            paired_contents.as_bytes(),
            25,
            usize::MAX,
            100,
            usize::MAX,
        )
        .expect("independent parsed and compiled totals stay within budget");
        assert_eq!(paired.compiled_units(), 20);
        let paired_rules = paired.build();
        assert_eq!(paired_rules.matched(b"/fixture/a/b/x", false), Some(true));
        assert_eq!(paired_rules.matched(b"/fixture/y", false), Some(true));

        // With only 1 compiled slot remaining, an unanchored malformed rule
        // (`[[:x][:][]`, 1 parsed part) is discarded cleanly without rolling
        // back earlier valid rules.
        let mut tight_compiled = RuleSetBuilder::new(root, false, false);
        add_rules_within_budget(
            &mut tight_compiled,
            b"*.log\n[[:x][:][]\n",
            10,
            usize::MAX,
            3,
            usize::MAX,
        )
        .expect("malformed rule at tight compiled budget is discarded cleanly");
        assert_eq!(tight_compiled.compiled_units(), 2);
        assert_eq!(
            tight_compiled.build().matched(b"/fixture/app.log", false),
            Some(true)
        );

        // Repeated malformed multi-component lines charge `parsed_parts` so they
        // cannot bypass the per-file budget, while malformed lines within the
        // budget remain ignored without dropping subsequent valid rules.
        let mut under_budget = RuleSetBuilder::new(root, false, false);
        add_rules_within_budget(
            &mut under_budget,
            b"a/b/c/[\n*.log\n",
            10,
            usize::MAX,
            10,
            usize::MAX,
        )
        .expect("malformed line plus valid rule under budget succeeds");
        assert_eq!(under_budget.compiled_units(), 2);
        assert_eq!(
            under_budget.build().matched(b"/fixture/app.log", false),
            Some(true)
        );

        let mut malformed_flood = RuleSetBuilder::new(root, false, false);
        let error = add_rules_within_budget(
            &mut malformed_flood,
            b"a/b/c/[\na/b/c/[\na/b/c/[\n",
            10,
            usize::MAX,
            100,
            usize::MAX,
        )
        .expect_err("repeated malformed multi-component lines hit file budget");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(malformed_flood.compiled_units(), 0);
        assert_eq!(malformed_flood.rules_capacity(), 0);

        // Repeated 60,000-`?` lines breach the 256 MiB retained-byte ceiling
        // and roll back atomically.
        let mut wildcard_flood = RuleSetBuilder::new(root, false, false);
        let qline = format!("{}\n", "?".repeat(60_000));
        let mut qfile = String::from("keep.txt\n");
        for _ in 0..40 {
            qfile.push_str(&qline);
        }
        let error = add_rules_within_budget(
            &mut wildcard_flood,
            qfile.as_bytes(),
            MAX_IGNORE_FILE_COMPONENTS,
            super::MAX_IGNORE_FILE_RETAINED_BYTES,
            super::MAX_IGNORE_CHAIN_COMPONENTS,
            super::MAX_IGNORE_CHAIN_BYTES,
        )
        .expect_err("repeated wildcard-heavy lines exceed per-file retained-byte budget");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("256 MiB"),
            "expected FileBytes diagnostic, got: {error}"
        );
        assert_eq!(wildcard_flood.compiled_units(), 0);
        assert_eq!(wildcard_flood.rules_capacity(), 0);

        // Ordinary large literal ignore files (e.g. 40,000 simple literal
        // rules = 80,000 components) and realistic large glob ignore files
        // (e.g. 12,000 `*.generated-00001.json` rules = 24,000 components)
        // remain accepted under the default 100,000-line / 250,000-component /
        // 256 MiB safety ceilings.
        let mut ordinary_large = RuleSetBuilder::new(root, false, false);
        let forty_k_literals = "a\n".repeat(40_000);
        add_rules_within_budget(
            &mut ordinary_large,
            forty_k_literals.as_bytes(),
            MAX_IGNORE_FILE_COMPONENTS,
            super::MAX_IGNORE_FILE_RETAINED_BYTES,
            super::MAX_IGNORE_CHAIN_COMPONENTS,
            super::MAX_IGNORE_CHAIN_BYTES,
        )
        .expect("40,000 simple literal rules remain accepted under default budgets");
        assert_eq!(ordinary_large.compiled_units(), 80_000);
        let ordinary_rules = ordinary_large.build();
        assert_eq!(ordinary_rules.matched(b"/fixture/a", false), Some(true));

        let mut twelve_k_globs = RuleSetBuilder::new(root, false, false);
        let mut generated_globs = String::with_capacity(12_000 * 24);
        for index in 0..12_000 {
            use std::fmt::Write as _;
            writeln!(generated_globs, "*.generated-{index:05}.json").expect("write glob line");
        }
        add_rules_within_budget(
            &mut twelve_k_globs,
            generated_globs.as_bytes(),
            MAX_IGNORE_FILE_COMPONENTS,
            super::MAX_IGNORE_FILE_RETAINED_BYTES,
            super::MAX_IGNORE_CHAIN_COMPONENTS,
            super::MAX_IGNORE_CHAIN_BYTES,
        )
        .expect("12,000 realistic glob suffix rules remain accepted under default budgets");
        assert_eq!(twelve_k_globs.compiled_units(), 24_000);
        let glob_rules = twelve_k_globs.build();
        assert_eq!(
            glob_rules.matched(b"/fixture/pkg/app.generated-11999.json", false),
            Some(true)
        );

        // A single rule that exceeds `ferralk-glob`'s per-pattern IR budget
        // skips only that uncompiled rule without rolling back the rest of the
        // file — regardless of line order, even when its estimated retained
        // bytes exceed the remaining file byte budget (`100 MiB < ~134 MiB`),
        // AND even when only 1 compiled component slot remains.
        let mut ir_after_valid = Vec::with_capacity(1_048_577 + 32);
        ir_after_valid.extend_from_slice(b"*.log\nkeep.tmp\n");
        ir_after_valid.resize(ir_after_valid.len() + 1_048_577, b'?');
        ir_after_valid.push(b'\n');

        let mut ir_before_valid = Vec::with_capacity(1_048_577 + 32);
        ir_before_valid.resize(1_048_577, b'?');
        ir_before_valid.extend_from_slice(b"\n*.log\nkeep.tmp\n");

        for (label, contents) in [
            ("valid-before-ir-limit", ir_after_valid),
            ("ir-limit-before-valid", ir_before_valid),
        ] {
            for (max_comp, file_byte_budget) in [
                (
                    MAX_IGNORE_FILE_COMPONENTS,
                    super::MAX_IGNORE_FILE_RETAINED_BYTES,
                ),
                (MAX_IGNORE_FILE_COMPONENTS, 100 * 1024 * 1024),
                // `*.log` (2) + `keep.tmp` (2) = 4 compiled components; budget
                // of 5 leaves only 1 compiled component slot for the unanchored
                // IR-limit line (which wants 2), exercising the compiled-budget
                // overflow discard path as well.
                (5, super::MAX_IGNORE_FILE_RETAINED_BYTES),
            ] {
                let mut ir_limit_builder = RuleSetBuilder::new(root, false, false);
                add_rules_within_budget(
                    &mut ir_limit_builder,
                    &contents,
                    max_comp,
                    file_byte_budget,
                    max_comp,
                    super::MAX_IGNORE_CHAIN_BYTES,
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "{label} under ({max_comp} components, {file_byte_budget} bytes) must skip only the IR-limit rule, got: {error}"
                    )
                });
                assert_eq!(ir_limit_builder.compiled_units(), 4, "{label}");
                let ir_rules = ir_limit_builder.build();
                assert_eq!(ir_rules.matched(b"/fixture/app.log", false), Some(true));
                assert_eq!(ir_rules.matched(b"/fixture/keep.tmp", false), Some(true));
            }
        }

        // Pathological multi-star rules (`/*a*\n` repeated 80,000 times =
        // 80,000 components, strictly below `MAX_IGNORE_FILE_COMPONENTS = 250,000`)
        // hit the 256 MiB per-file retained-byte ceiling FIRST and roll back
        // atomically.
        let mut dense_wide_builder = RuleSetBuilder::new(root, false, false);
        let dense_wide_rules = "/*a*\n".repeat(80_000);
        let error = add_rules_within_budget(
            &mut dense_wide_builder,
            dense_wide_rules.as_bytes(),
            MAX_IGNORE_FILE_COMPONENTS,
            super::MAX_IGNORE_FILE_RETAINED_BYTES,
            super::MAX_IGNORE_CHAIN_COMPONENTS,
            super::MAX_IGNORE_CHAIN_BYTES,
        )
        .expect_err(
            "80,000 multi-star rules exceed 256 MiB retained matcher safety limit before component limit",
        );
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("256 MiB"),
            "expected FileBytes diagnostic, got: {error}"
        );
        assert_eq!(dense_wide_builder.compiled_units(), 0);
        assert_eq!(dense_wide_builder.rules_capacity(), 0);

        // Multi-star `WideSweepEngine` lines (`*` + 40 KiB + `*`) breach the
        // 256 MiB retained-byte budget and roll back atomically.
        let mut wide_sweep_flood = RuleSetBuilder::new(root, false, false);
        let wide_line = format!("*{}*\n", "a".repeat(40_000));
        let mut wide_file = String::from("keep.txt\n");
        for _ in 0..60 {
            wide_file.push_str(&wide_line);
        }
        let error = add_rules_within_budget(
            &mut wide_sweep_flood,
            wide_file.as_bytes(),
            MAX_IGNORE_FILE_COMPONENTS,
            super::MAX_IGNORE_FILE_RETAINED_BYTES,
            super::MAX_IGNORE_CHAIN_COMPONENTS,
            super::MAX_IGNORE_CHAIN_BYTES,
        )
        .expect_err("multi-star WideSweepEngine lines exceed per-file safety limit");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("256 MiB"),
            "expected FileBytes diagnostic, got: {error}"
        );
        assert_eq!(wide_sweep_flood.compiled_units(), 0);
        assert_eq!(wide_sweep_flood.rules_capacity(), 0);

        // A single 500,000-component line fails fast against the real ceiling.
        let mut huge_builder = RuleSetBuilder::new(root, false, false);
        let huge_line = format!("first.txt\n{}", "a/".repeat(MAX_IGNORE_FILE_COMPONENTS + 1));
        let error = add_rules_within_budget(
            &mut huge_builder,
            huge_line.as_bytes(),
            MAX_IGNORE_FILE_COMPONENTS,
            super::MAX_IGNORE_FILE_RETAINED_BYTES,
            super::MAX_IGNORE_CHAIN_COMPONENTS,
            super::MAX_IGNORE_CHAIN_BYTES,
        )
        .expect_err("single-line component explosion is rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(huge_builder.compiled_units(), 0);
        assert_eq!(huge_builder.rules_capacity(), 0);
        assert!(huge_builder.build().is_empty());
    }

    #[test]
    fn ignore_chain_cumulative_budget_preserves_ancestors_and_siblings() {
        use std::collections::HashMap;
        use std::path::PathBuf;

        struct MapBackend {
            files: HashMap<PathBuf, Vec<u8>>,
        }

        impl crate::DirectoryBackend for MapBackend {
            fn read_directory(
                &self,
                _path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                _listing: &mut crate::Listing,
            ) -> std::io::Result<()> {
                unreachable!("only ignore-file reads are exercised")
            }

            fn read_ignore_file(&self, path: &Path) -> std::io::Result<BoundedFileRead> {
                Ok(self
                    .files
                    .get(path)
                    .cloned()
                    .map_or(BoundedFileRead::Missing, BoundedFileRead::Contents))
            }
        }

        let budget = IgnoreBudget {
            max_file_components: 10,
            max_file_bytes: 16 * 1024,
            max_chain_components: 12,
            max_chain_bytes: 32 * 1024,
        };
        let adaptation = GitIgnoreAdaptation::default();
        let backend = MapBackend {
            files: HashMap::from([
                // 6 units (3 unanchored literal rules, 2 units each)
                (
                    PathBuf::from("/repo/.gitignore"),
                    b"app.log\napp.tmp\napp.bak\n".to_vec(),
                ),
                // 4 units -> cumulative 10
                (
                    PathBuf::from("/repo/sub/.gitignore"),
                    b"main.o\nmain.a\n".to_vec(),
                ),
                // 4 units -> would reach 14 > 12; rejected atomically while
                // `/repo/sub/.gitignore` remains applied.
                (
                    PathBuf::from("/repo/sub/.ignore"),
                    b"keep.txt\nsecret.txt\n".to_vec(),
                ),
                // Empty/comment-only file still succeeds at 10/12.
                (
                    PathBuf::from("/repo/sub/empty/.gitignore"),
                    b"# just a comment\n\n".to_vec(),
                ),
                // 3 parsed parts collapsing to 2 compiled units -> reaches
                // exact ceiling 12/12 despite only 2 chain slots remaining.
                (
                    PathBuf::from("/repo/sub/exact/.gitignore"),
                    b"**/**/exact.txt\n".to_vec(),
                ),
                // 2 more units after 12/12 -> rejected while ancestors stay.
                (
                    PathBuf::from("/repo/sub/exact/deep/.gitignore"),
                    b"deep.txt\n".to_vec(),
                ),
            ]),
        };

        let mut scope = IgnoreScope::default();
        let (root_rules, root_errors) = read_rules(
            &backend,
            Path::new("/repo"),
            Path::new("/repo"),
            &IGNORE_FILES,
            adaptation,
            scope.chain_usage(),
            budget,
        );
        assert!(root_errors.is_empty());
        assert_eq!(root_rules.compiled_units(), 6);
        scope = scope.link(root_rules);
        assert_eq!(scope.chain_usage().compiled_units, 6);

        let (sub_rules, sub_errors) = read_rules(
            &backend,
            Path::new("/repo/sub"),
            Path::new("/repo/sub"),
            &IGNORE_FILES,
            adaptation,
            scope.chain_usage(),
            budget,
        );
        assert_eq!(sub_errors.len(), 1);
        assert_eq!(sub_errors[0].path, PathBuf::from("/repo/sub/.ignore"));
        assert_eq!(sub_errors[0].kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(sub_rules.compiled_units(), 4);
        scope = scope.link(sub_rules);
        assert_eq!(scope.chain_usage().compiled_units, 10);

        let (empty_rules, empty_errors) = read_rules(
            &backend,
            Path::new("/repo/sub/empty"),
            Path::new("/repo/sub/empty"),
            &IGNORE_FILES,
            adaptation,
            scope.chain_usage(),
            budget,
        );
        assert!(empty_errors.is_empty());
        assert!(empty_rules.is_empty());

        let (exact_rules, exact_errors) = read_rules(
            &backend,
            Path::new("/repo/sub/exact"),
            Path::new("/repo/sub/exact"),
            &IGNORE_FILES,
            adaptation,
            scope.chain_usage(),
            budget,
        );
        assert!(exact_errors.is_empty());
        scope = scope.link(exact_rules);
        assert_eq!(scope.chain_usage().compiled_units, 12);

        let (deep_rules, deep_errors) = read_rules(
            &backend,
            Path::new("/repo/sub/exact/deep"),
            Path::new("/repo/sub/exact/deep"),
            &IGNORE_FILES,
            adaptation,
            scope.chain_usage(),
            budget,
        );
        assert_eq!(deep_errors.len(), 1);
        assert_eq!(
            deep_errors[0].path,
            PathBuf::from("/repo/sub/exact/deep/.gitignore")
        );
        assert_eq!(deep_errors[0].kind(), std::io::ErrorKind::InvalidData);
        assert!(deep_rules.is_empty());
        scope = scope.link(deep_rules);
        assert_eq!(scope.chain_usage().compiled_units, 12);

        // Ancestor rules remain in force, while rolled-back files do not apply.
        let node = scope.rules.as_ref().expect("chain has ancestor rules");
        assert_eq!(
            node.verdict(b"/repo/sub/exact/deep/app.log", false),
            Some(true)
        );
        assert_eq!(
            node.verdict(b"/repo/sub/exact/deep/main.o", false),
            Some(true)
        );
        assert_eq!(
            node.verdict(b"/repo/sub/exact/deep/exact.txt", false),
            Some(true)
        );
        assert_eq!(node.verdict(b"/repo/sub/exact/deep/keep.txt", false), None);
        assert_eq!(node.verdict(b"/repo/sub/exact/deep/deep.txt", false), None);

        // Active-chain retained-byte cap rejects large literal chains cleanly
        // while preserving ancestors and still allowing comment-only files.
        // Compute exact budgets from the production `RuleSetBuilder` estimator
        // so this boundary stays exact on every target architecture.
        let line_a = format!("{}\n", "a".repeat(40)).into_bytes();
        let line_b = format!("{}\n", "b".repeat(24)).into_bytes();
        let expected_a_bytes = {
            let mut b = RuleSetBuilder::new(Path::new("/bytes"), false, false);
            b.add_line_within_budget(
                &line_a[..line_a.len() - 1],
                crate::ignore_rules::LineBudgets::UNBOUNDED,
            )
            .expect("measure rule a")
            .retained_bytes
        };
        let expected_b_bytes = {
            let mut b = RuleSetBuilder::new(Path::new("/bytes/sub"), false, false);
            b.add_line_within_budget(
                &line_b[..line_b.len() - 1],
                crate::ignore_rules::LineBudgets::UNBOUNDED,
            )
            .expect("measure rule b")
            .retained_bytes
        };
        let expected_total_bytes = expected_a_bytes + expected_b_bytes;
        let byte_budget = IgnoreBudget {
            max_file_components: 100,
            max_file_bytes: expected_total_bytes,
            max_chain_components: 1_000,
            max_chain_bytes: expected_total_bytes,
        };
        let byte_backend = MapBackend {
            files: HashMap::from([
                (PathBuf::from("/bytes/.gitignore"), line_a),
                (PathBuf::from("/bytes/sub/.gitignore"), line_b),
                (
                    PathBuf::from("/bytes/sub/comment/.gitignore"),
                    b"# comments retain zero bytes\n".to_vec(),
                ),
                (PathBuf::from("/bytes/sub/over/.gitignore"), b"c\n".to_vec()),
            ]),
        };

        let mut byte_scope = IgnoreScope::default();
        let (r1, e1) = read_rules(
            &byte_backend,
            Path::new("/bytes"),
            Path::new("/bytes"),
            &IGNORE_FILES,
            adaptation,
            byte_scope.chain_usage(),
            byte_budget,
        );
        assert!(e1.is_empty());
        byte_scope = byte_scope.link(r1);
        assert_eq!(byte_scope.chain_usage().retained_bytes, expected_a_bytes);

        let (r2, e2) = read_rules(
            &byte_backend,
            Path::new("/bytes/sub"),
            Path::new("/bytes/sub"),
            &IGNORE_FILES,
            adaptation,
            byte_scope.chain_usage(),
            byte_budget,
        );
        assert!(e2.is_empty());
        byte_scope = byte_scope.link(r2);
        assert_eq!(
            byte_scope.chain_usage().retained_bytes,
            expected_total_bytes
        );

        let (r_comment, e_comment) = read_rules(
            &byte_backend,
            Path::new("/bytes/sub/comment"),
            Path::new("/bytes/sub/comment"),
            &IGNORE_FILES,
            adaptation,
            byte_scope.chain_usage(),
            byte_budget,
        );
        assert!(e_comment.is_empty());
        assert!(r_comment.is_empty());

        let (r_over, e_over) = read_rules(
            &byte_backend,
            Path::new("/bytes/sub/over"),
            Path::new("/bytes/sub/over"),
            &IGNORE_FILES,
            adaptation,
            byte_scope.chain_usage(),
            byte_budget,
        );
        assert_eq!(e_over.len(), 1);
        assert_eq!(e_over[0].kind(), std::io::ErrorKind::InvalidData);
        assert!(r_over.is_empty());
        let byte_node = byte_scope.rules.as_ref().expect("ancestor rules kept");
        let a_path = format!("/bytes/sub/over/{}", "a".repeat(40));
        assert_eq!(byte_node.verdict(a_path.as_bytes(), false), Some(true));
        assert_eq!(byte_node.verdict(b"/bytes/sub/over/c", false), None);
    }

    #[test]
    fn streaming_config_lines_preserve_crlf_escapes_and_reuse_scratch() {
        let mut config = GitConfig::default();
        apply_config(
            &mut config,
            b"[core]\r\n\
              # comment \\\r\n\
              ignorecase = tru\\\r\n\
              e\r\n\
              other = foo\\\\\r\n\
              precomposeunicode = \"fa\\\r\n\
              lse\" # trailing \\\r\n\
              [extensions]\r\n\
              worktreeconfig = \\\r\n\
              \\\r\n\
              yes\r\n",
        );
        assert_eq!(config.ignore_case, Some(true));
        assert_eq!(config.precompose_unicode, Some(false));
        assert_eq!(config.worktree_config, Some(true));

        // Uncontinued lines borrow directly from `contents` with no copy.
        let plain = b"[core]\nignorecase = true\nprecomposeunicode = false\n";
        let range = plain.as_ptr_range();
        let mut seen = 0;
        for_each_logical_config_line(plain, |line| {
            assert!(range.contains(&line.as_ptr()));
            seen += 1;
        });
        assert_eq!(seen, 3);

        // Trailing EOF backslash on an uncontinued line also borrows directly.
        let eof_slash = b"k = v\\";
        let eof_range = eof_slash.as_ptr_range();
        let mut yielded = Vec::new();
        for_each_logical_config_line(eof_slash, |line| {
            assert!(eof_range.contains(&line.as_ptr()));
            yielded.push(line.to_vec());
        });
        assert_eq!(yielded, vec![b"k = v".to_vec()]);
    }
}
