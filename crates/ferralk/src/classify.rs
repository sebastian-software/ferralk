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

use std::path::PathBuf;

use super::{
    BackendEntry, DirectoryBackend, WalkEntry, Walker, gitignore::IgnoreScope, glob_path_bytes,
    has_hidden_component, should_skip_git_directory,
};

/// What a frontend has to do with one directory entry.
pub(crate) enum EntryAction {
    /// The entry is filtered away: nothing to traverse, nothing to emit.
    Skip,
    /// Traverse into this directory; the directory itself is not emitted.
    Descend(DirectoryTask),
    /// Emit this entry; there is nothing to traverse into.
    Emit(WalkEntry),
    /// Traverse into this directory and emit it as well.
    DescendAndEmit(WalkEntry, DirectoryTask),
    /// A filesystem call failed. The error policy, which each frontend applies
    /// its own way, decides what happens next. A directory that was already
    /// cleared for traversal is still reported, so a failed stat cannot
    /// silently prune a subtree.
    Failed {
        failure: EntryFailure,
        descend: Option<DirectoryTask>,
    },
}

/// A directory the walk still has to visit, carrying the ignore state it
/// inherits. Its own ignore files join the chain when the walk enters it, which
/// happens exactly once per directory and therefore exactly once per walk.
#[derive(Debug)]
pub(crate) struct DirectoryTask {
    pub(crate) path: PathBuf,
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
fn should_emit(walker: &Walker, entry: &BackendEntry, bytes: &[u8], git_ignored: bool) -> bool {
    if git_ignored {
        return false;
    }
    if walker.options.directories_only && !entry.is_dir {
        return false;
    }
    if walker.options.files_only && entry.is_dir {
        return false;
    }
    walker.includes.is_empty()
        || walker
            .includes
            .iter()
            .any(|pattern| pattern.matches(bytes, entry.is_dir))
}

/// Decides what one directory entry means for the walk.
pub(crate) fn classify_entry<B: DirectoryBackend + ?Sized>(
    walker: &Walker,
    backend: &B,
    mut entry: BackendEntry,
    ignores: &IgnoreScope,
) -> EntryAction {
    let relative = entry
        .path
        .strip_prefix(&walker.root)
        .unwrap_or(entry.path.as_path());
    // The only walk of the path components; every depth question below reuses
    // the result.
    let depth = relative.components().count();
    if walker
        .options
        .max_depth
        .is_some_and(|max_depth| depth > max_depth)
    {
        return EntryAction::Skip;
    }
    let bytes = glob_path_bytes(relative);
    if walker.options.skip_hidden && has_hidden_component(bytes.as_ref()) {
        return EntryAction::Skip;
    }
    if should_skip_git_directory(walker, &entry.path) {
        return EntryAction::Skip;
    }
    if walker
        .excludes
        .iter()
        .any(|pattern| pattern.matches(bytes.as_ref(), entry.is_dir))
    {
        return EntryAction::Skip;
    }
    let git_ignored = ignores.is_ignored(&entry.path, entry.is_dir);
    if git_ignored && !entry.is_dir {
        return EntryAction::Skip;
    }
    if entry.is_symlink && walker.options.follow_symlinks {
        // Resolving the link decides whether this entry is a directory, which
        // the remaining filters and the traversal decision both depend on.
        match backend.metadata(&entry.path) {
            Ok(metadata) => entry.is_dir = metadata.is_dir(),
            Err(source) => {
                return EntryAction::Failed {
                    failure: EntryFailure {
                        operation: "metadata",
                        path: entry.path,
                        source,
                    },
                    descend: None,
                };
            }
        }
    }
    if !entry.is_dir && !walker.may_include_file(bytes.as_ref()) {
        return EntryAction::Skip;
    }

    let descend = entry.is_dir
        && !walker
            .excludes
            .iter()
            .any(|pattern| pattern.covers_subtree(bytes.as_ref()))
        && walker.may_descend_at(depth, bytes.as_ref());
    let emit = should_emit(walker, &entry, bytes.as_ref(), git_ignored);

    // The state a subtree inherits is decided here, so the frontends never
    // re-derive it.
    let task = |path| DirectoryTask {
        path,
        ignores: ignores.inherit(git_ignored),
    };
    if !emit {
        if descend {
            return EntryAction::Descend(task(entry.path));
        }
        return EntryAction::Skip;
    }
    // Last, and only for an entry that is actually emitted.
    let metadata = if walker.options.metadata {
        match backend.symlink_metadata(&entry.path) {
            Ok(metadata) => Some(metadata),
            Err(source) => {
                return EntryAction::Failed {
                    descend: descend.then(|| task(entry.path.clone())),
                    failure: EntryFailure {
                        operation: "symlink_metadata",
                        path: entry.path,
                        source,
                    },
                };
            }
        }
    } else {
        None
    };
    let walk_entry = WalkEntry {
        path: entry.path,
        is_dir: entry.is_dir,
        is_symlink: entry.is_symlink,
        depth,
        metadata,
    };
    if descend {
        let task = task(walk_entry.path.clone());
        EntryAction::DescendAndEmit(walk_entry, task)
    } else {
        EntryAction::Emit(walk_entry)
    }
}
