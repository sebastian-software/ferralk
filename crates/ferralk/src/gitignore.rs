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
    DirectoryBackend, Listing, Walker, glob_path_bytes,
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
    pub(crate) fn for_root<B: DirectoryBackend + ?Sized>(
        walker: &Walker,
        backend: &B,
        root: &Path,
    ) -> Self {
        if !walker.respect_git_ignore {
            return Self::default();
        }
        Self::default().link(read_rules(backend, root, &[REPOSITORY_EXCLUDE_FILE]))
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
    ) -> Self {
        if !walker.respect_git_ignore {
            return self;
        }
        let present = IGNORE_FILES
            .into_iter()
            .filter(|file| listing.contains(file))
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
/// Ignore files are byte streams. Git strips one BOM at the file start, treats
/// NUL as the end of a rule line, and otherwise keeps parsing after invalid
/// UTF-8 bytes.
fn add_rules(builder: &mut RuleSetBuilder, contents: &[u8]) {
    let contents = contents.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(contents);
    for line in contents.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let line = line.split(|byte| *byte == b'\0').next().unwrap_or(line);
        builder.add_line(line);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{RuleSetBuilder, add_rules};

    #[test]
    fn byte_lines_continue_after_invalid_utf8_and_match_byte_patterns() {
        let root = Path::new("/fixture");
        let mut builder = RuleSetBuilder::new(root);
        add_rules(&mut builder, b"first.txt\n\xE9latin1.txt\nsecond.txt\n");
        let rules = builder.build();

        assert_eq!(rules.matched(b"/fixture/second.txt", false), Some(true));
        assert_eq!(rules.matched(b"/fixture/\xE9latin1.txt", false), Some(true));
    }

    #[test]
    fn nul_ends_a_rule_and_one_initial_bom_is_stripped() {
        let root = Path::new("/fixture");
        let mut builder = RuleSetBuilder::new(root);
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
}
