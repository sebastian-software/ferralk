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

use super::{
    DirectoryBackend, ListedEntry, WalkEntry, Walker, gitignore::IgnoreScope, glob_bytes,
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
    pub(crate) metadata: Option<fs::Metadata>,
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
    /// Which of the walk's roots this directory sits under. Carried down the
    /// tree rather than rediscovered, because it selects the patterns and the
    /// root-relative offset that apply here.
    pub(crate) root: usize,
    /// Components between the walk root and this directory. The walk counts
    /// them once, on the way down, instead of recounting the components of
    /// every entry's path.
    pub(crate) depth: usize,
    pub(crate) ignores: IgnoreScope,
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
fn should_emit(
    walker: &Walker,
    root: usize,
    is_dir: bool,
    bytes: &[u8],
    git_ignored: bool,
) -> bool {
    if git_ignored {
        return false;
    }
    if walker.options.directories_only && !is_dir {
        return false;
    }
    if walker.options.files_only && is_dir {
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
    root: usize,
) -> EntryAction {
    let plan = &walker.roots[root];
    let mut is_dir = entry.is_dir();
    let path_bytes = path.as_os_str().as_encoded_bytes();
    // Every walked path is its root with names pushed onto it, so the
    // root-relative part is a suffix at a fixed offset rather than something
    // `strip_prefix` has to rediscover component by component.
    let relative = &path_bytes[plan.relative_start.min(path_bytes.len())..];
    let depth = directory_depth + 1;
    if walker
        .options
        .max_depth
        .is_some_and(|max_depth| depth > max_depth)
    {
        return EntryAction::Skip;
    }
    let bytes = glob_bytes(relative);
    if walker.options.skip_hidden && has_hidden_component(bytes.as_ref()) {
        return EntryAction::Skip;
    }
    if should_skip_git_directory(walker, entry.name()) {
        return EntryAction::Skip;
    }
    if plan
        .excludes
        .iter()
        .any(|pattern| pattern.matches(bytes.as_ref(), is_dir, walker.wildcard_mode))
    {
        return EntryAction::Skip;
    }
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
    if !is_dir && !walker.may_include_file(root, bytes.as_ref()) {
        return EntryAction::Skip;
    }

    // An ignored directory is not entered, the way Git does not enter one:
    // its contents are ignored whatever the ignore files inside it say.
    let descend = is_dir
        && !git_ignored
        && !plan
            .excludes
            .iter()
            .any(|pattern| pattern.covers_subtree(bytes.as_ref(), walker.wildcard_mode))
        && walker.may_descend_at(root, depth, bytes.as_ref());
    let emit = should_emit(walker, root, is_dir, bytes.as_ref(), git_ignored);

    // The rules a subtree inherits travel with it, so the frontends never
    // re-derive them. A queued directory outlives the scratch buffer, so this
    // is one of the few places that has to own a path.
    let task = || DirectoryTask {
        path: path.to_path_buf(),
        depth,
        root,
        ignores: ignores.clone(),
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
            Ok(metadata) => Some(metadata),
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
