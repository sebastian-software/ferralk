//! The one entry-classification pipeline behind all three walk frontends.
//!
//! Serial `collect`, `stream` and parallel `collect` differ in how they
//! schedule directories, report errors and deliver entries. What an entry
//! *means* - filtered away, traversed into, emitted - is decided here, once,
//! so the frontends cannot drift apart on it again.
//!
//! Filters run before any `stat`: an entry that no pattern will emit costs no
//! filesystem call, and therefore also produces no error for the walk to
//! report. The only stat that runs earlier is the symlink resolution, because
//! whether a link points at a directory is itself an input to the filters.
//!
//! Nothing here owns a path. The entry's path lives in the scratch buffer the
//! frontend keeps for the directory it is reading, and is copied out only
//! where something has to keep it: a queued subdirectory, a reported error, or
//! an entry that survived every filter — and, for a visited walk, the visitor.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(not(windows))]
use super::glob_bytes;
use super::{
    AncestorChain, DirectoryBackend, DirectoryOpen, ListedEntry, Listing, WalkEntry, Walker,
    gitignore::{IgnoreReadError, IgnoreScope},
    has_hidden_component, should_skip_git_directory,
};

/// What a frontend has to do with one directory entry.
pub(crate) enum EntryAction {
    /// The entry is filtered away: nothing to traverse, nothing to emit.
    Skip,
    /// Traverse into this directory; the directory itself is not emitted.
    Descend(DirectoryTask),
    /// Emit this entry; there is nothing to traverse into.
    Emit(EmittedEntry),
    /// Traverse into this directory and emit it as well.
    DescendAndEmit(EmittedEntry, DirectoryTask),
    /// A filesystem call failed. The error policy, which each frontend applies
    /// its own way, decides what happens next. A directory that was already
    /// cleared for traversal is still reported, so a failed stat cannot
    /// silently prune a subtree.
    Failed {
        failure: EntryFailure,
        descend: Option<DirectoryTask>,
    },
}

/// An entry that passed every filter, minus its path.
///
/// The path is still in the frontend's scratch buffer when this is returned.
/// Keeping it there is the point: a visited walk copies it only once the
/// visitor has said `Keep`, so a `Verdict::Skip` costs no allocation.
pub(crate) struct EmittedEntry {
    pub(crate) is_dir: bool,
    pub(crate) is_symlink: bool,
    pub(crate) depth: usize,
    /// Boxed for the same reason [`WalkEntry`] boxes it: the inline `stat`
    /// struct dominated both this type and [`EntryAction`], which is returned
    /// by value from `classify_entry` for every entry the walk classifies -
    /// including the ones it drops.
    pub(crate) metadata: Option<Box<fs::Metadata>>,
    /// The root this entry was found under, shared with every other entry from
    /// the same root rather than copied per entry.
    pub(crate) root: Arc<Path>,
}

impl EmittedEntry {
    /// Completes the entry with the path the frontend materialized for it.
    pub(crate) fn with_path(self, path: PathBuf) -> WalkEntry {
        WalkEntry {
            path,
            root: self.root,
            is_dir: self.is_dir,
            is_symlink: self.is_symlink,
            depth: self.depth,
            metadata: self.metadata,
        }
    }
}

/// A directory the walk still has to visit, carrying the ignore state it
/// inherits. Its own ignore files join the chain when the walk enters it, which
/// happens exactly once per directory and therefore exactly once per walk.
#[derive(Debug)]
pub(crate) struct DirectoryTask {
    pub(crate) path: PathBuf,
    /// Backend-specific capability for opening this directory without
    /// resolving its complete path again. Empty on backends that do not expose
    /// one.
    pub(crate) open: DirectoryOpen,
    /// Which of the walk's roots this directory sits under. Carried down the
    /// tree rather than rediscovered, because it selects the patterns and the
    /// root-relative offset that apply here.
    pub(crate) root: usize,
    /// The directories between this task's root and its parent while following
    /// symlinks. It detects loops without deduplicating sibling aliases.
    pub(crate) ancestors: AncestorChain,
    /// Components between the walk root and this directory. The walk counts
    /// them once, on the way down, instead of recounting the components of
    /// every entry's path.
    pub(crate) depth: usize,
    pub(crate) ignores: IgnoreScope,
    /// Repository-level ignore errors are discovered while the root task is
    /// built and consumed exactly once by the frontend that opens it.
    pub(crate) ignore_errors: Vec<IgnoreReadError>,
}

/// Directory-specific state carried while one of its entries is classified.
pub(crate) struct TraversalContext<'a> {
    pub(crate) root: usize,
    pub(crate) ancestors: &'a AncestorChain,
    pub(crate) listing: &'a Listing,
    /// Reusable Windows-normalized bytes for both glob filters and gitignore.
    pub(crate) glob_bytes_scratch: &'a mut Vec<u8>,
}

/// A filesystem call that failed while classifying one entry.
pub(crate) struct EntryFailure {
    pub(crate) operation: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) source: std::io::Error,
}

/// Whether an entry that survived the traversal filters is part of the result
/// set. Traversal and emission are separate questions: a directory can be
/// walked into without being emitted, and the other way round.
///
/// `kind_is_dir` is what the kind filters count this entry as: a directory, a
/// file, or - only ever for a symlink whose target is gone - neither. It is
/// what the listing observed unless
/// [`WalkOptions::resolve_symlink_kind`](crate::WalkOptions::resolve_symlink_kind)
/// asked for the target's kind instead.
fn should_emit(
    walker: &Walker,
    root: usize,
    is_dir: bool,
    kind_is_dir: Option<bool>,
    bytes: &[u8],
    git_ignored: bool,
) -> bool {
    if git_ignored {
        return false;
    }
    if walker.options.directories_only && kind_is_dir != Some(true) {
        return false;
    }
    if walker.options.files_only && kind_is_dir != Some(false) {
        return false;
    }
    let includes = &walker.roots[root].includes;
    includes.is_empty()
        || includes
            .iter()
            .any(|pattern| pattern.matches(bytes, is_dir, walker.wildcard_mode))
}

/// Decides what one directory entry means for the walk.
///
/// `path` is the frontend's scratch buffer, already holding this entry's whole
/// path. `directory_depth` is how deep the directory holding the entry sits
/// below the walk root, so the entry itself is one deeper.
pub(crate) fn classify_entry<B: DirectoryBackend + ?Sized>(
    walker: &Walker,
    backend: &B,
    path: &Path,
    entry: &ListedEntry,
    ignores: &IgnoreScope,
    directory_depth: usize,
    context: TraversalContext<'_>,
) -> EntryAction {
    let glob_bytes_scratch = context.glob_bytes_scratch;
    #[cfg(not(windows))]
    let _ = glob_bytes_scratch;
    let plan = &walker.roots[context.root];
    let mut is_dir = entry.is_dir();
    let path_bytes = path.as_os_str().as_encoded_bytes();
    // Every walked path is its root with names pushed onto it, so the
    // root-relative part is a suffix at a fixed offset rather than something
    // `strip_prefix` has to rediscover component by component.
    #[cfg(not(windows))]
    let relative = &path_bytes[plan.relative_start.min(path_bytes.len())..];
    let depth = directory_depth + 1;
    if walker
        .options
        .max_depth
        .is_some_and(|max_depth| depth > max_depth)
    {
        return EntryAction::Skip;
    }
    #[cfg(windows)]
    let git_ignored = {
        super::glob_bytes_into(path_bytes, glob_bytes_scratch);
        ignores.is_ignored_bytes(glob_bytes_scratch, is_dir)
    };
    #[cfg(not(windows))]
    let normalized_bytes = glob_bytes(relative);
    #[cfg(not(windows))]
    let bytes = normalized_bytes.as_ref();
    #[cfg(windows)]
    let bytes = &glob_bytes_scratch[plan.relative_start.min(glob_bytes_scratch.len())..];
    if walker.options.skip_hidden && has_hidden_component(bytes) {
        return EntryAction::Skip;
    }
    if should_skip_git_directory(walker, entry.name()) {
        return EntryAction::Skip;
    }
    let excluded = plan
        .excludes
        .iter()
        .any(|pattern| pattern.matches(bytes, is_dir, walker.wildcard_mode));
    // A matching directory is not emitted, but its descendants may still be
    // selected by an include. Files have no descendants to re-admit.
    if excluded && !is_dir {
        return EntryAction::Skip;
    }
    #[cfg(not(windows))]
    let git_ignored = ignores.is_ignored(path, is_dir);
    if git_ignored && !is_dir {
        return EntryAction::Skip;
    }
    if entry.is_symlink() && walker.options.follow_symlinks {
        // Resolving the link decides whether this entry is a directory, which
        // the remaining filters and the traversal decision both depend on.
        match backend.metadata(path) {
            Ok(metadata) => is_dir = metadata.is_dir(),
            Err(source) => {
                return EntryAction::Failed {
                    failure: EntryFailure {
                        operation: "metadata",
                        path: path.to_path_buf(),
                        source,
                    },
                    descend: None,
                };
            }
        }
    }
    if !is_dir && !walker.may_include_file(context.root, bytes) {
        return EntryAction::Skip;
    }

    // An ignored directory is not entered, the way Git does not enter one:
    // its contents are ignored whatever the ignore files inside it say.
    let may_include_descendant = walker.may_descend_into(context.root, bytes);
    let exclude_proves_no_re_admission = plan.excludes.iter().any(|pattern| {
        pattern.covers_subtree(bytes, walker.wildcard_mode)
            && (plan.includes.is_empty() || !may_include_descendant)
    });
    let descend = is_dir
        && !git_ignored
        && !exclude_proves_no_re_admission
        && walker.may_descend_at(context.root, depth, bytes);
    // What the kind filters count this entry as. A listing reports a symlink as
    // a symlink and nothing about its target, so left alone the filters read
    // every unfollowed symlink as a non-directory. Resolving costs one stat and
    // is therefore paid only for a symlink, only when a kind filter is on to
    // ask the question, and only when following has not already answered it.
    let mut kind_is_dir = Some(is_dir);
    if walker.options.resolve_symlink_kind
        && entry.is_symlink()
        && !walker.options.follow_symlinks
        && (walker.options.files_only || walker.options.directories_only)
    {
        match backend.metadata(path) {
            Ok(metadata) => kind_is_dir = Some(metadata.is_dir()),
            // A link with nothing at the end of it is neither a file nor a
            // directory. That is an answer, not a failure: dangling links are
            // ordinary, and reporting one per link would flood the error
            // channel and end an `Abort` walk over a build artefact.
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => kind_is_dir = None,
            // Anything else leaves the kind genuinely unknown, which the error
            // policy gets to decide about. The entry is dropped either way,
            // because neither filter can be answered for it.
            Err(source) => {
                return EntryAction::Failed {
                    failure: EntryFailure {
                        operation: "metadata",
                        path: path.to_path_buf(),
                        source,
                    },
                    // Nothing to traverse: a listing never reports a symlink as
                    // a directory, and this branch is only reached when the
                    // walk is not following symlinks, so `descend` is false.
                    descend: None,
                };
            }
        }
    }
    let emit = !excluded
        && should_emit(
            walker,
            context.root,
            is_dir,
            kind_is_dir,
            bytes,
            git_ignored,
        );

    // The rules a subtree inherits travel with it, so the frontends never
    // re-derive them. A queued directory outlives the scratch buffer, so this
    // is one of the few places that has to own a path.
    let task = || DirectoryTask {
        path: path.to_path_buf(),
        open: backend.child_directory_open(context.listing, entry.name()),
        depth,
        root: context.root,
        ancestors: context.ancestors.clone(),
        ignores: ignores.clone(),
        ignore_errors: Vec::new(),
    };
    if !emit {
        if descend {
            return EntryAction::Descend(task());
        }
        return EntryAction::Skip;
    }
    // Last, and only for an entry that is actually emitted.
    let metadata = if walker.options.metadata {
        match backend.symlink_metadata(path) {
            Ok(metadata) => Some(Box::new(metadata)),
            Err(source) => {
                return EntryAction::Failed {
                    descend: descend.then(task),
                    failure: EntryFailure {
                        operation: "symlink_metadata",
                        path: path.to_path_buf(),
                        source,
                    },
                };
            }
        }
    } else {
        None
    };
    let emitted = EmittedEntry {
        is_dir,
        is_symlink: entry.is_symlink(),
        depth,
        metadata,
        root: Arc::clone(&plan.shared_path),
    };
    if descend {
        EntryAction::DescendAndEmit(emitted, task())
    } else {
        EntryAction::Emit(emitted)
    }
}
