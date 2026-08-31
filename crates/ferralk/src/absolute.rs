//! Rewriting absolute include and exclude patterns into root-relative ones.
//!
//! The walker matches root-relative bytes, while callers routinely hold
//! absolute patterns: a build tool that knows a project lives at `/repo` writes
//! `/repo/src/**/*.ts` rather than tracking which prefix the walk will strip.
//! Every such caller ends up writing the same prefix arithmetic, and gets the
//! edge cases wrong in the same places, so the walker does it once here.
//!
//! The rewrite is purely lexical. Nothing here touches the filesystem, so a
//! pattern compiles without a `stat`, and no symlink or `..` is resolved
//! behind the caller's back - which is also why a `..` in either the pattern or
//! the root is rejected instead of being folded away.
//!
//! [`rewrite`] takes the root as an argument rather than reading it from a
//! walker, so a future multi-root walk can rewrite one pattern once per root.
//!
//! One limitation follows from the pattern dialect rather than from this code:
//! a walk root whose own name contains `*`, `?`, `[`, `{` or `\` cannot be
//! spelled literally in front of a pattern, because those bytes are syntax
//! there. Such a root is reported as unrewritable rather than guessed at, and
//! the pattern has to be written relative to it.

use std::borrow::Cow;

use ferralk_glob::PatternError;

use crate::first_metacharacter;

/// What an absolute pattern turned into for one walk root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Rewrite {
    /// The pattern was not absolute. It is already in the walker's dialect and
    /// is used unchanged.
    Relative,
    /// The pattern was absolute and named paths inside the root. These are the
    /// equivalent root-relative pattern bytes.
    Rooted(Vec<u8>),
    /// The pattern was absolute and named paths outside the root, so no entry
    /// this walk can produce will match it.
    ///
    /// This is a verdict, not a failure: a caller filtering one pattern list
    /// across several roots expects the patterns for other roots to select
    /// nothing here rather than to be rejected.
    Outside,
}

/// Which spelling of an absolute path a rewrite is reading.
///
/// Carried as a value rather than read from `cfg!` at each use, so both
/// spellings are exercised by the tests on whichever platform runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// A leading separator makes a path absolute.
    Posix,
    /// A drive letter (`C:/`) or a UNC share (`//host/share`) does.
    Windows,
}

impl Syntax {
    /// The spelling this build's platform uses.
    pub(crate) const NATIVE: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::Posix
    };
}

/// Bytes Windows forbids in a file or directory name.
///
/// `/` is left out because it is the separator the pattern dialect uses, and a
/// backslash is in because a walker candidate never contains one: paths reach
/// the matcher through [`crate::glob_path_bytes`], which turns every separator
/// into `/`.
const FORBIDDEN_IN_A_WINDOWS_NAME: [u8; 8] = *b"\\:*?\"<>|";

/// Rejects a walker pattern that a Windows host can never match, when the
/// reason is that it was written as a path.
///
/// Backslash is the escape character in this dialect on every platform, so a
/// pattern built by joining `PathBuf`s on Windows does not fail - it quietly
/// turns each separator into "the next byte, literally" and then matches
/// nothing. That is the worst failure a pattern can have, and it cost the
/// Palamedes adoption nineteen green-looking CI runs.
///
/// The test is deliberately narrow, because escaping an ordinary byte is legal
/// syntax and means what it says. It fires only where the pattern would demand
/// a literal byte Windows cannot put in a name, so nothing that could ever have
/// matched is refused:
///
/// - a drive prefix spelled with a backslash, `X:\`, which asks for a component
///   literally named `X:`. That is a property of the first three bytes and needs
///   no reading of what follows.
/// - `\` followed by one of [`FORBIDDEN_IN_A_WINDOWS_NAME`] **in plain literal
///   text**, which asks for a literal `*`, `?`, `:` or `\` in a component - none
///   of which a Windows path can contain. `C:\repo\src\**\*.ts` and `\\?\C:\x`
///   are caught here.
///
/// The emphasis is the correction from the review of #94. Inside a character
/// class or an alternation an escaped forbidden byte is one member among
/// several: `[a\*]` still matches `a`, and `{a,\*}` still matches `a`. Refusing
/// those would refuse patterns that work, which is the one thing this check may
/// never do. The scan therefore stops at the first grouping construct and reads
/// only the plain text before it.
///
/// Two kinds of pattern go unexamined as a result, both deliberately:
///
/// - `\` before an ordinary byte, `a\b\c`, a legal pattern for a file named
///   `abc`.
/// - anything after a `[`, `{` or extglob opener, including a one-alternative
///   group like `{a\*b}` that could in fact never match. Unnoticed but correct
///   beats noticed but lossy, and the alternative is a second parser here that
///   would have to agree with the real one about every nesting case to stay
///   sound.
pub(crate) fn reject_path_shaped(pattern: &[u8], syntax: Syntax) -> Result<(), PatternError> {
    if syntax != Syntax::Windows {
        return Ok(());
    }
    if let Some(offset) = drive_prefix_with_backslash(pattern) {
        return Err(PatternError::new(offset, PATH_SHAPED));
    }
    if let Some(offset) = path_shaped_byte(pattern) {
        return Err(PatternError::new(offset, PATH_SHAPED));
    }
    Ok(())
}

/// Offset of a byte that makes a plain-text Windows pattern unmatchable.
///
/// Stops at the first byte that could open a group rather than skipping the
/// group, because skipping means deciding where it ends, and disagreeing with
/// the real parser about that would resume the scan *inside* a group - which is
/// the lossy rejection being avoided. Stopping early costs only detection.
fn path_shaped_byte(pattern: &[u8]) -> Option<usize> {
    let mut index = 0;
    while index < pattern.len() {
        if pattern[index] == b'\\' {
            // A trailing backslash is the pattern language's own error, and is
            // reported where that is decided.
            let escaped = *pattern.get(index + 1)?;
            if FORBIDDEN_IN_A_WINDOWS_NAME.contains(&escaped) {
                return Some(index);
            }
            index += 2;
            continue;
        }
        // A colon cannot occur in a Windows name. The absolute `C:/` form
        // was already rewritten before this check runs, so a plain colon here
        // is necessarily a drive-relative or otherwise unmatchable spelling.
        let absolute_drive_prefix = index == 1
            && pattern.first().is_some_and(u8::is_ascii_alphabetic)
            && pattern.get(2) == Some(&b'/');
        if pattern[index] == b':' && !absolute_drive_prefix {
            return Some(index);
        }
        if opens_a_group(pattern, index) {
            return None;
        }
        index += 1;
    }
    None
}

/// Whether the unescaped byte at `index` could begin a class, an alternation or
/// an extglob group.
///
/// Asked without looking for the closer: an opener that never closes is a
/// compilation error anyway, and treating it as a group here only ends the scan
/// early.
fn opens_a_group(pattern: &[u8], index: usize) -> bool {
    match pattern[index] {
        b'[' | b'{' => true,
        b'@' | b'+' | b'!' | b'*' | b'?' => pattern.get(index + 1) == Some(&b'('),
        _ => false,
    }
}

/// The one message, so a caller matching on it has one string to match.
const PATH_SHAPED: &str = "this looks like a Windows path rather than a pattern; write patterns with `/` separators, \
     because `\\` is the escape character on every platform";

/// Offset of a `X:\` drive prefix, if the pattern opens with one.
fn drive_prefix_with_backslash(pattern: &[u8]) -> Option<usize> {
    (pattern.len() >= 3
        && pattern[0].is_ascii_alphabetic()
        && pattern[1] == b':'
        && pattern[2] == b'\\')
        .then_some(0)
}

/// Rewrites `pattern` for a walk rooted at `root`, or reports why it cannot.
///
/// `root` is the walk root as glob bytes - separators already normalized to
/// `/`, which is what [`crate::glob_path_bytes`] produces on every platform.
/// The pattern itself is read in the walker's dialect, where `/` separates
/// components and `\` escapes, per ADR-0005.
///
/// `syntax` is passed in rather than read from `cfg!` here, so the tests and
/// the corpus can drive both spellings on whichever host runs them.
pub(crate) fn rewrite_in(
    pattern: &[u8],
    root: &[u8],
    syntax: Syntax,
) -> Result<Rewrite, PatternError> {
    let pattern = deverbatimize(pattern, syntax);
    let root = deverbatimize(root, syntax);
    let pattern = pattern.as_ref();
    let root = root.as_ref();
    let Some(pattern_prefix) = absolute_prefix(pattern, syntax) else {
        return Ok(Rewrite::Relative);
    };
    let Some(root_prefix) = absolute_prefix(root, syntax) else {
        // Relating an absolute pattern to a relative root would take the
        // current directory, which is process state the walk does not read.
        return Err(PatternError::new(
            0,
            "an absolute pattern needs an absolute walk root",
        ));
    };
    if !roots_agree(&pattern[..pattern_prefix], &root[..root_prefix]) {
        // A different drive or share. Nothing under this root can match.
        return Ok(Rewrite::Outside);
    }

    // Rejected wherever it appears, not only above the root: `/a/b/../b/x.ts`
    // is a path inside a root of `/a/b`, but the bytes left after removing the
    // root would start with `..` and match no walk candidate at all. Silently
    // selecting nothing there would be worse than saying so.
    if let Some(at) = dot_dot_component(pattern) {
        return Err(PatternError::new(
            at,
            "`..` in an absolute pattern is not resolved, because resolving it lexically would be wrong across a symlink",
        ));
    }

    // A metacharacter anywhere ends the part of the pattern that is a plain
    // path, and only a plain path can be compared against the root.
    let magic = first_metacharacter(pattern);
    let mut offset = pattern_prefix;
    let root_is_unc = syntax == Syntax::Windows && root[..root_prefix] == *b"//";
    for (root_component_index, root_component) in components(&root[root_prefix..]).enumerate() {
        if root_component == b".." {
            return Err(PatternError::new(
                0,
                "an absolute pattern needs a walk root without a `..` component",
            ));
        }
        loop {
            let Some((component, next)) = next_component(pattern, offset) else {
                // The pattern named an ancestor of the root and stopped there,
                // so it names one path and that path is not inside the walk.
                return Ok(Rewrite::Outside);
            };
            if component.is_empty() || component == b"." {
                offset = next;
                continue;
            }
            if magic.is_some_and(|at| at < offset + component.len()) {
                return Err(PatternError::new(
                    offset,
                    "a wildcard at or above the walk root cannot be made relative to it",
                ));
            }
            let component_agrees = if root_is_unc && root_component_index < 2 {
                component.eq_ignore_ascii_case(root_component)
            } else {
                component == root_component
            };
            if !component_agrees {
                return Ok(Rewrite::Outside);
            }
            offset = next;
            break;
        }
    }

    // The separator that joined the root to the rest belongs to neither, and a
    // caller whose root already ended in one leaves two behind. A pattern may
    // not start with a separator - walk candidates never do - so they come off
    // here rather than becoming a pattern that matches nothing.
    while let Some((component, next)) = next_component(pattern, offset) {
        if !component.is_empty() && component != b"." {
            break;
        }
        offset = next;
    }

    // Everything that names nothing has been skipped, so what is left is
    // either a real component or nothing at all.
    let remainder = &pattern[offset..];
    if remainder.is_empty() {
        return Err(PatternError::new(
            offset,
            "an absolute pattern that names the walk root itself selects nothing; add `/**` to select what is inside it",
        ));
    }
    Ok(Rewrite::Rooted(remainder.to_vec()))
}

/// Removes Windows' verbatim namespace prefix from a normalized path spelling.
///
/// `std::fs::canonicalize` returns `//?/C:/...` or `//?/UNC/host/share/...`
/// on Windows. Those are alternate spellings of ordinary drive and UNC paths,
/// not UNC shares whose host is `?`, so strip the namespace before deciding
/// how an absolute pattern relates to its root.
fn deverbatimize<'a>(bytes: &'a [u8], syntax: Syntax) -> Cow<'a, [u8]> {
    if syntax != Syntax::Windows {
        return Cow::Borrowed(bytes);
    }
    let Some(rest) = bytes.strip_prefix(b"//?/") else {
        return Cow::Borrowed(bytes);
    };
    if rest.len() >= 4 && rest[..4].eq_ignore_ascii_case(b"UNC/") {
        let mut ordinary = Vec::with_capacity(rest.len() - 2);
        ordinary.extend_from_slice(b"//");
        ordinary.extend_from_slice(&rest[4..]);
        Cow::Owned(ordinary)
    } else {
        Cow::Borrowed(rest)
    }
}

/// Length of the prefix that makes `bytes` absolute, if it is.
///
/// On Windows a single leading separator is deliberately not enough, matching
/// the platform: `\tmp` is relative to the current drive, not absolute, so a
/// pattern spelled that way keeps the root-relative reading it has today.
fn absolute_prefix(bytes: &[u8], syntax: Syntax) -> Option<usize> {
    match syntax {
        Syntax::Posix => bytes.starts_with(b"/").then_some(1),
        Syntax::Windows => {
            if bytes.starts_with(b"//") {
                return Some(2);
            }
            let drive = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && bytes[2] == b'/';
            drive.then_some(3)
        }
    }
}

/// Whether two absolute prefixes name the same root.
///
/// The only prefix with content of its own is a Windows drive letter, and a
/// drive letter is not a filename: `C:` and `c:` are one drive however
/// case-sensitively the names below them are matched.
fn roots_agree(pattern_prefix: &[u8], root_prefix: &[u8]) -> bool {
    pattern_prefix.eq_ignore_ascii_case(root_prefix)
}

/// Components of an already-absolute path's remainder, skipping the empties a
/// doubled or trailing separator leaves behind, and `.` which names nothing.
fn components(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split(|byte| *byte == b'/')
        .filter(|component| !component.is_empty() && *component != b".")
}

/// The component starting at `offset`, and where the next one starts.
///
/// Splits on `/` only. `\` is an escape in the pattern dialect rather than a
/// separator, and it is a metacharacter, so a pattern that spells its root with
/// backslashes is reported as unrewritable instead of being silently mis-split.
fn next_component(pattern: &[u8], offset: usize) -> Option<(&[u8], usize)> {
    if offset >= pattern.len() {
        return None;
    }
    let rest = &pattern[offset..];
    match rest.iter().position(|byte| *byte == b'/') {
        Some(separator) => Some((&rest[..separator], offset + separator + 1)),
        None => Some((rest, pattern.len())),
    }
}

/// Offset of the first `..` path component, if the pattern has one.
///
/// Components are split on `/` alone, so a `..` hidden inside a brace
/// alternative is not found. That is deliberate: this rejects the spelling that
/// looks like a path operation, not every byte sequence that could expand to
/// one.
fn dot_dot_component(pattern: &[u8]) -> Option<usize> {
    let mut offset = 0;
    while let Some((component, next)) = next_component(pattern, offset) {
        if component == b".." {
            return Some(offset);
        }
        offset = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Rewrite, Syntax, rewrite_in};

    /// Convenience for the common assertion: this rewrote to these bytes.
    fn rooted(pattern: &str, root: &str, syntax: Syntax) -> String {
        match rewrite_in(pattern.as_bytes(), root.as_bytes(), syntax) {
            Ok(Rewrite::Rooted(bytes)) => String::from_utf8(bytes).expect("ASCII fixture"),
            other => panic!("expected a rewrite, got {other:?}"),
        }
    }

    fn verdict(pattern: &str, root: &str, syntax: Syntax) -> Rewrite {
        rewrite_in(pattern.as_bytes(), root.as_bytes(), syntax).expect("no error expected")
    }

    fn message(pattern: &str, root: &str, syntax: Syntax) -> &'static str {
        rewrite_in(pattern.as_bytes(), root.as_bytes(), syntax)
            .expect_err("an error was expected")
            .message()
    }

    fn rejection(pattern: &str, syntax: Syntax) -> Option<&'static str> {
        super::reject_path_shaped(pattern.as_bytes(), syntax)
            .err()
            .map(|error| error.message())
    }

    /// The shapes a `PathBuf` join produces on Windows, which the dialect reads
    /// as escapes and which therefore match nothing.
    #[test]
    fn a_windows_path_spelled_as_a_pattern_is_refused() {
        for pattern in [
            r"C:\repo\src\**\*.ts",
            r"C:\repo\node_modules",
            r"src\*.ts",
            r"src\**\*.ts",
            r"\\server\share\src\*.ts",
            r"\\?\C:\repo\*.ts",
        ] {
            assert!(
                rejection(pattern, Syntax::Windows)
                    .is_some_and(|message| message.starts_with("this looks like a Windows path")),
                "{pattern} must be refused on Windows"
            );
            // POSIX hosts keep every one of these: there a backslash is only
            // ever an escape, and a file really can be named `src*.ts`.
            assert_eq!(
                rejection(pattern, Syntax::Posix),
                None,
                "{pattern} on POSIX"
            );
        }
    }

    /// The rule may only refuse patterns that could not have matched anyway.
    #[test]
    fn a_pattern_that_could_match_on_windows_is_kept() {
        for pattern in [
            // The spellings that work, which is most of the point.
            "src/**/*.ts",
            "C:/repo/src/**/*.ts",
            "//server/share/src/*.ts",
            "{src,lib}/**/*.ts",
            // Escaping an ordinary byte is legal and means the byte itself.
            // `a\b\c` selects a file named `abc` - odd, but it works, so it
            // is not this rule's business.
            r"a\b\c",
            // Brackets and braces are legal in a Windows filename, so escaping
            // them asks for something that can exist.
            r"notes\[1\].txt",
            r"literal\{braces\}.txt",
            // A drive prefix written the way this dialect wants it.
            "C:/repo",
        ] {
            assert_eq!(rejection(pattern, Syntax::Windows), None, "{pattern}");
            assert_eq!(rejection(pattern, Syntax::Posix), None, "{pattern}");
        }
    }

    /// An escaped forbidden byte inside a group is one member among several, so
    /// the pattern can still match and must not be refused.
    ///
    /// This is the review finding on #94: the first version scanned raw bytes
    /// and refused `[a\*]` and `{a,\*}`, both of which match `a`. A rejection
    /// that removes working patterns is worse than the silence it replaced.
    #[test]
    fn an_escape_inside_a_group_is_not_a_path_separator() {
        for pattern in [
            // A class: `*` is one member, `a` is another.
            r"[a\*]",
            r"src/[a\*].ts",
            // An alternation: `\*` is one branch, `a` is another.
            r"{a,\*}",
            r"src/{a,\*}.ts",
            // Extglob groups alternate the same way.
            r"@(a|\*)",
            r"+(a|\*)",
            r"!(a|\*)",
            r"*(a|\*)",
            r"?(a|\*)",
            // The residue, stated so it is a decision and not a surprise: a
            // one-alternative group could never match on Windows, and is still
            // accepted, because reading it would mean parsing the group.
            r"{a\*b}",
        ] {
            assert_eq!(
                rejection(pattern, Syntax::Windows),
                None,
                "{pattern} can match, so it must not be refused"
            );
        }
    }

    /// The negative half of the same pair: what is refused stays refused.
    ///
    /// Restricting the scan to plain text must not cost the shapes the check
    /// exists for, all of which reach the forbidden escape before any group.
    #[test]
    fn narrowing_the_scan_keeps_the_shapes_it_was_built_for() {
        for pattern in [
            r"C:\repo\*.ts",
            r"src\*.ts",
            r"C:\repo\src\**\*.ts",
            r"\\server\share\src\*.ts",
            "C:src/*.ts",
        ] {
            assert!(
                rejection(pattern, Syntax::Windows).is_some(),
                "{pattern} can never match on Windows and must be refused"
            );
        }
    }

    /// A trailing backslash is the pattern language's own error, reported where
    /// that is decided rather than guessed at here.
    #[test]
    fn a_trailing_backslash_is_left_to_the_compiler() {
        assert_eq!(rejection(r"src\", Syntax::Windows), None);
    }

    #[test]
    fn a_pattern_under_the_root_loses_exactly_the_root() {
        assert_eq!(rooted("/a/b/src/*.ts", "/a/b", Syntax::Posix), "src/*.ts");
        assert_eq!(rooted("/a/b/**/*.ts", "/a/b", Syntax::Posix), "**/*.ts");
        assert_eq!(rooted("/a/b/**", "/a/b", Syntax::Posix), "**");
        // A brace root from #20 survives, because the rewrite stops at the
        // first metacharacter and the prefix before it is what it removes.
        assert_eq!(
            rooted("/a/b/{src,lib}/**/*.ts", "/a/b", Syntax::Posix),
            "{src,lib}/**/*.ts"
        );
        // A trailing separator still means "directories only" afterwards.
        assert_eq!(rooted("/a/b/build/", "/a/b", Syntax::Posix), "build/");
    }

    #[test]
    fn separator_noise_on_either_side_is_ignored() {
        assert_eq!(rooted("/a//b/src/*.ts", "/a/b", Syntax::Posix), "src/*.ts");
        assert_eq!(rooted("/a/./b/src/*.ts", "/a/b", Syntax::Posix), "src/*.ts");
        assert_eq!(rooted("/a/b/src/*.ts", "/a/b/", Syntax::Posix), "src/*.ts");
        assert_eq!(
            rooted("/a/b/src/*.ts", "/a//b//", Syntax::Posix),
            "src/*.ts"
        );
        // A root that already ends in a separator leaves two at the join.
        assert_eq!(rooted("/a/b//src/*.ts", "/a/b", Syntax::Posix), "src/*.ts");
        assert_eq!(rooted("/a/b/./src/*.ts", "/a/b", Syntax::Posix), "src/*.ts");
        assert_eq!(rooted("//a/b/src/*.ts", "/a/b", Syntax::Posix), "src/*.ts");
    }

    #[test]
    fn a_pattern_that_cannot_reach_the_root_selects_nothing() {
        // A sibling of the root.
        assert_eq!(verdict("/a/c/**", "/a/b", Syntax::Posix), Rewrite::Outside);
        // An unrelated tree.
        assert_eq!(
            verdict("/other/**", "/a/b", Syntax::Posix),
            Rewrite::Outside
        );
        // An ancestor named exactly, which is one path and not inside the walk.
        assert_eq!(verdict("/a", "/a/b", Syntax::Posix), Rewrite::Outside);
        // A near miss that a byte-wise prefix test would have accepted: the
        // root is `/a/b`, and `/a/bb` is a different directory.
        assert_eq!(verdict("/a/bb/**", "/a/b", Syntax::Posix), Rewrite::Outside);
    }

    #[test]
    fn a_relative_pattern_is_left_alone() {
        assert_eq!(
            verdict("src/*.ts", "/a/b", Syntax::Posix),
            Rewrite::Relative
        );
        assert_eq!(verdict("**/*.ts", "/a/b", Syntax::Posix), Rewrite::Relative);
        // On Windows a single leading separator is drive-relative, not
        // absolute, so it keeps whatever meaning it has as a walker pattern.
        assert_eq!(
            verdict("/src/*.ts", "C:/a/b", Syntax::Windows),
            Rewrite::Relative
        );
    }

    #[test]
    fn a_wildcard_at_or_above_the_root_is_rejected() {
        // `/*/x.ts` could match inside the root or outside it; which is not
        // decidable without matching, so the walker says so instead of
        // guessing. Selecting nothing here would silently drop real matches.
        assert_eq!(
            message("/*/x.ts", "/a/b", Syntax::Posix),
            "a wildcard at or above the walk root cannot be made relative to it"
        );
        assert_eq!(
            message("/**/*.ts", "/a/b", Syntax::Posix),
            "a wildcard at or above the walk root cannot be made relative to it"
        );
        // Sharing a component with the root's own name is the same problem:
        // `b*` covers `b` and `bb` alike.
        assert_eq!(
            message("/a/b*/x.ts", "/a/b", Syntax::Posix),
            "a wildcard at or above the walk root cannot be made relative to it"
        );
        // A wildcard below the root is fine; only the part being removed has
        // to be literal.
        assert_eq!(rooted("/a/b/*/x.ts", "/a/b", Syntax::Posix), "*/x.ts");
    }

    #[test]
    fn dot_dot_is_rejected_on_both_sides() {
        assert!(message("/a/b/../b/x.ts", "/a/b", Syntax::Posix).starts_with("`..`"));
        assert_eq!(
            message("/a/b/x.ts", "/a/b/../b", Syntax::Posix),
            "an absolute pattern needs a walk root without a `..` component"
        );
        // Below the root it is rejected too. Removing `/a` would leave
        // `b/../c/*.ts`, which matches no walk candidate, so the pattern would
        // quietly select nothing instead of what it names.
        assert!(message("/a/b/../c/*.ts", "/a", Syntax::Posix).starts_with("`..`"));
    }

    #[test]
    fn naming_the_root_itself_is_rejected() {
        for pattern in ["/a/b", "/a/b/", "/a/b/.", "/a//b//"] {
            assert!(
                message(pattern, "/a/b", Syntax::Posix)
                    .starts_with("an absolute pattern that names"),
                "{pattern} names the root"
            );
        }
    }

    #[test]
    fn an_absolute_pattern_needs_an_absolute_root() {
        assert_eq!(
            message("/a/b/*.ts", "relative/dir", Syntax::Posix),
            "an absolute pattern needs an absolute walk root"
        );
        assert_eq!(
            message("C:/a/*.ts", "a/b", Syntax::Windows),
            "an absolute pattern needs an absolute walk root"
        );
    }

    #[test]
    fn windows_roots_are_read_the_way_windows_spells_them() {
        assert_eq!(
            rooted("C:/a/b/src/*.ts", "C:/a/b", Syntax::Windows),
            "src/*.ts"
        );
        // A drive letter is not a filename, so its case does not decide.
        assert_eq!(
            rooted("c:/a/b/src/*.ts", "C:/a/b", Syntax::Windows),
            "src/*.ts"
        );
        // Names below the drive are matched as written, per ADR-0005.
        assert_eq!(
            verdict("C:/a/B/src/*.ts", "C:/a/b", Syntax::Windows),
            Rewrite::Outside
        );
        // Another drive is outside this walk.
        assert_eq!(
            verdict("D:/a/b/**", "C:/a/b", Syntax::Windows),
            Rewrite::Outside
        );
        // UNC shares work the same way.
        assert_eq!(
            rooted("//host/share/a/**/*.ts", "//host/share/a", Syntax::Windows),
            "**/*.ts"
        );
        // `canonicalize` returns this verbatim namespace spelling on Windows.
        assert_eq!(
            rooted("C:/repo/src/**/*.ts", "//?/C:/repo", Syntax::Windows),
            "src/**/*.ts"
        );
        assert_eq!(
            rooted("//?/C:/repo/src/**/*.ts", "C:/repo", Syntax::Windows),
            "src/**/*.ts"
        );
        assert_eq!(
            rooted(
                "//HOST/SHARE/a/**/*.ts",
                "//?/UNC/host/share/a",
                Syntax::Windows
            ),
            "**/*.ts"
        );
        assert_eq!(
            verdict("//host/other/a/**", "//host/share/a", Syntax::Windows),
            Rewrite::Outside
        );
        // A backslash is an escape in this dialect, not a separator, so a
        // pattern spelled the way `Path::display` prints one is reported
        // rather than silently mis-split.
        assert_eq!(
            message("C:/a\\b\\src\\*.ts", "C:/a/b", Syntax::Windows),
            "a wildcard at or above the walk root cannot be made relative to it"
        );
    }
}
