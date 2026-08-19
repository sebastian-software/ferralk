//! Gitignore evaluation, done once per directory instead of once per entry.
//!
//! Every directory the walk enters reads its own ignore files once and links
//! them onto the chain it inherited, so an entry is matched against each ignore
//! file that is actually in force exactly once, with the path relative to that
//! file's directory. Directories without ignore files add nothing to the chain,
//! which is why a tree without them costs nothing per entry.
//!
//! The chain travels with the directory task rather than through a cache: every
//! directory is visited exactly once, so the descent already builds each node
//! exactly once, whatever the worker count. A shared cache would add
//! synchronization to a problem the task graph has already solved.

use std::{path::Path, sync::Arc};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::{BackendEntry, DirectoryBackend, Walker};

/// Ignore files of a directory, in increasing precedence: a later file wins.
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];

/// Repository-wide excludes, which the root's own ignore files override. Git
/// reads this file for the repository the walk root belongs to; ferralk applies
/// ignore rules from the walk root downwards, so it reads the one there.
const REPOSITORY_EXCLUDE_FILE: &str = ".git/info/exclude";

/// The ignore rules in force inside one directory, plus that directory's own
/// verdict, which its entries inherit.
#[derive(Debug, Clone, Default)]
pub(crate) struct IgnoreScope {
    /// Innermost directory with rules. Each node links to the next ancestor
    /// that has any; directories without ignore files never appear.
    rules: Option<Arc<IgnoreNode>>,
    /// Whether the directory this scope describes is itself ignored.
    ignored: bool,
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

    /// The state a child directory inherits: the rules in force here, plus the
    /// child's own verdict. Its ignore files join when the walk enters it.
    pub(crate) fn inherit(&self, ignored: bool) -> Self {
        Self {
            rules: self.rules.clone(),
            ignored,
        }
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
    /// precedence; with none, the entry inherits the directory's own verdict.
    pub(crate) fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        match &self.rules {
            Some(node) => node.verdict(path, is_dir).unwrap_or(self.ignored),
            None => self.ignored,
        }
    }

    /// Puts `rules` on the chain, unless they are empty: an empty matcher can
    /// never have an opinion, so keeping it would only lengthen the walk of
    /// every entry below it.
    fn link(self, rules: Gitignore) -> Self {
        if rules.is_empty() {
            return self;
        }
        Self {
            rules: Some(Arc::new(IgnoreNode {
                rules,
                parent: self.rules,
            })),
            ignored: self.ignored,
        }
    }
}

#[derive(Debug)]
struct IgnoreNode {
    rules: Gitignore,
    parent: Option<Arc<IgnoreNode>>,
}

impl IgnoreNode {
    /// The verdict of the deepest node that matches, or `None` when no node in
    /// the chain matches. Each node sees the path relative to its own
    /// directory, which the matcher strips, so this costs one match per ignore
    /// file in force rather than one per ancestor directory.
    fn verdict(&self, path: &Path, is_dir: bool) -> Option<bool> {
        let matched = self.rules.matched(path, is_dir);
        if !matched.is_none() {
            return Some(matched.is_ignore());
        }
        self.parent
            .as_ref()
            .and_then(|parent| parent.verdict(path, is_dir))
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
) -> Gitignore {
    let mut builder = GitignoreBuilder::new(directory);
    for file in files {
        let path = directory.join(file);
        if let Ok(contents) = backend.read_ignore_file(&path) {
            add_rules(&mut builder, &path, &contents);
        }
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// Feeds one ignore file's lines to the builder.
///
/// This mirrors what the `ignore` crate does when it reads the file itself: a
/// leading byte-order mark is dropped the way Git drops it, and a line that is
/// not UTF-8 ends the file, keeping the lines before it.
fn add_rules(builder: &mut GitignoreBuilder, path: &Path, contents: &[u8]) {
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
        let _ = builder.add_line(Some(path.to_path_buf()), line);
    }
}
