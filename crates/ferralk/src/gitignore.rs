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

use std::{path::Path, sync::Arc};

use super::{
    BackendEntry, DirectoryBackend, Walker, glob_path_bytes,
    ignore_rules::{RuleSet, RuleSetBuilder},
};

/// Ignore files of a directory, in increasing precedence: a later file wins.
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];

/// Repository-wide excludes, which the root's own ignore files override. Git
/// reads this file for the repository the walk root belongs to; ferralk applies
/// ignore rules from the walk root downwards, so it reads the one there.
const REPOSITORY_EXCLUDE_FILE: &str = ".git/info/exclude";

/// The ignore rules in force inside one directory.
#[derive(Debug, Clone, Default)]
pub(crate) struct IgnoreScope {
    /// Innermost directory with rules. Each node links to the next ancestor
    /// that has any; directories without ignore files never appear.
    rules: Option<Arc<IgnoreNode>>,
}

impl IgnoreScope {
    /// What the walk root inherits: the repository-wide excludes. The root's
    /// own ignore files join like every other directory's, when the walk
    /// enters it, and being deeper they override these.
    pub(crate) fn root<B: DirectoryBackend + ?Sized>(walker: &Walker, backend: &B) -> Self {
        if !walker.respect_git_ignore {
            return Self::default();
        }
        Self::default().link(read_rules(
            backend,
            &walker.root,
            &[REPOSITORY_EXCLUDE_FILE],
        ))
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
        entries: &[BackendEntry],
    ) -> Self {
        if !walker.respect_git_ignore {
            return self;
        }
        let present = IGNORE_FILES
            .into_iter()
            .filter(|file| {
                entries
                    .iter()
                    .any(|entry| entry.path.file_name().is_some_and(|name| name == *file))
            })
            .collect::<Vec<_>>();
        if present.is_empty() {
            return self;
        }
        let rules = read_rules(backend, directory, &present);
        self.link(rules)
    }

    /// Verdict for one entry of the directory this scope describes.
    ///
    /// The deepest ignore file with an opinion decides, which is Git's
    /// precedence. An entry below an ignored directory never reaches this: the
    /// walk does not enter such a directory.
    pub(crate) fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let Some(node) = self.rules.as_ref() else {
            return false;
        };
        // Converted once for the whole chain: every set below slices its own
        // prefix off these bytes.
        let candidate = glob_path_bytes(path);
        node.verdict(&candidate, is_dir).unwrap_or(false)
    }

    /// Puts `rules` on the chain, unless they are empty: an empty matcher can
    /// never have an opinion, so keeping it would only lengthen the walk of
    /// every entry below it.
    fn link(self, rules: RuleSet) -> Self {
        if rules.is_empty() {
            return self;
        }
        Self {
            rules: Some(Arc::new(IgnoreNode {
                rules,
                parent: self.rules,
            })),
        }
    }
}

#[derive(Debug)]
struct IgnoreNode {
    rules: RuleSet,
    parent: Option<Arc<IgnoreNode>>,
}

impl IgnoreNode {
    /// The verdict of the deepest node that matches, or `None` when no node in
    /// the chain matches. Each node sees the path relative to its own
    /// directory, which the matcher strips, so this costs one match per ignore
    /// file in force rather than one per ancestor directory.
    fn verdict(&self, candidate: &[u8], is_dir: bool) -> Option<bool> {
        self.rules.matched(candidate, is_dir).or_else(|| {
            self.parent
                .as_ref()
                .and_then(|parent| parent.verdict(candidate, is_dir))
        })
    }
}

/// Reads the given ignore files of one directory into a single matcher.
///
/// A file that cannot be read is skipped: the read reports a missing file the
/// same way a separate existence check would, one syscall instead of two.
fn read_rules<B: DirectoryBackend + ?Sized>(
    backend: &B,
    directory: &Path,
    files: &[&str],
) -> RuleSet {
    let mut builder = RuleSetBuilder::new(directory);
    for file in files {
        let path = directory.join(file);
        if let Ok(contents) = backend.read_ignore_file(&path) {
            add_rules(&mut builder, &contents);
        }
    }
    builder.build()
}

/// Feeds one ignore file's lines to the builder.
///
/// A leading byte-order mark is dropped the way Git drops it, and a line that
/// is not UTF-8 ends the file, keeping the lines before it.
fn add_rules(builder: &mut RuleSetBuilder, contents: &[u8]) {
    let text = match std::str::from_utf8(contents) {
        Ok(text) => text,
        Err(error) => {
            let valid = &contents[..error.valid_up_to()];
            let complete = valid
                .iter()
                .rposition(|&byte| byte == b'\n')
                .map_or(0, |index| index + 1);
            std::str::from_utf8(&valid[..complete]).unwrap_or_default()
        }
    };
    for (index, line) in text.lines().enumerate() {
        let line = if index == 0 {
            line.trim_start_matches('\u{feff}')
        } else {
            line
        };
        builder.add_line(line);
    }
}
