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
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(not(windows))]
use super::glob_path_bytes;
use super::{
    DirectoryBackend, Listing, Walker,
    ignore_rules::{RuleSet, RuleSetBuilder},
    read_bounded_file,
};

/// Ignore files of a directory, in increasing precedence: a later file wins.
const IGNORE_FILES: [&str; 2] = [".gitignore", ".ignore"];

/// Repository-wide excludes, which the root's own ignore files override.
const REPOSITORY_EXCLUDE_FILE: &str = "info/exclude";

/// A single ignore file may contribute at most this many rule lines. The byte
/// limit bounds I/O; this second dimension bounds compiled matchers even for a
/// file made entirely of one-byte rules.
const MAX_IGNORE_RULES: usize = 100_000;

#[derive(Debug)]
pub(crate) struct IgnoreReadError {
    pub(crate) path: PathBuf,
    kind: std::io::ErrorKind,
    message: String,
}

impl IgnoreReadError {
    fn new(path: PathBuf, source: std::io::Error) -> Self {
        Self {
            path,
            kind: source.kind(),
            message: source.to_string(),
        }
    }

    pub(crate) fn into_parts(self) -> (PathBuf, std::io::Error) {
        (self.path, std::io::Error::new(self.kind, self.message))
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
        let repository = repository_layout(root);
        let adaptation =
            GitIgnoreAdaptation::effective(walker, repository.as_ref().map(|(_, layout)| layout));
        let mut scope = Self {
            rules: None,
            adaptation,
        };
        let Some((repository_root, layout)) = repository else {
            return (scope, Vec::new());
        };
        let (rules, mut errors) =
            read_repository_rules(backend, &repository_root, Some(&layout), adaptation);
        scope = scope.link(rules);
        for directory in ancestor_directories(&repository_root, root) {
            let (rules, read_errors) = read_rules(backend, &directory, &IGNORE_FILES, adaptation);
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
        let (rules, errors) = read_rules(backend, directory, &present, self.adaptation);
        (self.link(rules), errors)
    }

    /// Verdict for one entry of the directory this scope describes.
    ///
    /// The deepest ignore file with an opinion decides, which is Git's
    /// precedence. An entry below an ignored directory never reaches this: the
    /// walk does not enter such a directory.
    #[cfg(not(windows))]
    pub(crate) fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let Some(node) = self.rules.as_ref() else {
            return false;
        };
        // Converted once for the whole chain: every set below slices its own
        // prefix off these bytes.
        let candidate = glob_path_bytes(path);
        node.verdict(&candidate, is_dir).unwrap_or(false)
    }

    #[cfg(windows)]
    pub(crate) fn is_ignored_bytes(&self, candidate: &[u8], is_dir: bool) -> bool {
        self.rules
            .as_ref()
            .and_then(|node| node.verdict(candidate, is_dir))
            .unwrap_or(false)
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
            adaptation: self.adaptation,
        }
    }
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
    adaptation: GitIgnoreAdaptation,
) -> (RuleSet, Vec<IgnoreReadError>) {
    let mut builder = RuleSetBuilder::new(
        directory,
        adaptation.case_insensitive,
        adaptation.precompose_unicode,
    );
    let mut errors = Vec::new();
    for file in files {
        let path = directory.join(file);
        match backend.read_ignore_file(&path) {
            Ok(contents) => match validate_rule_count(&contents) {
                Ok(()) => add_rules(&mut builder, &contents),
                Err(source) => errors.push(IgnoreReadError::new(path, source)),
            },
            Err(source) if ignored_in_tree_read_error(&source) => {}
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
        Ok(contents) => match validate_rule_count(&contents) {
            Ok(()) => add_rules(&mut builder, &contents),
            Err(source) => errors.push(IgnoreReadError::new(path, source)),
        },
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
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

fn ignored_in_tree_read_error(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(not(unix))]
    if error.kind() == std::io::ErrorKind::InvalidInput {
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ELOOP) {
        return true;
    }
    false
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

    let private_directory = resolve_metadata_path(
        root,
        parse_gitdir_pointer(&read_bounded_file(&dot_git).ok()?)?,
    )?;
    let common_dir = private_directory.join("commondir");
    match read_bounded_file(&common_dir) {
        Ok(contents) => Some(RepositoryLayout {
            common_directory: resolve_metadata_path(
                &private_directory,
                parse_metadata_path(&contents)?,
            )?,
            private_directory,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(RepositoryLayout {
            common_directory: private_directory.clone(),
            private_directory,
        }),
        Err(_) => None,
    }
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
    if let Ok(contents) = read_bounded_file(&layout.common_directory.join("config")) {
        apply_config(&mut config, &contents);
    }
    if config.worktree_config == Some(true)
        && let Ok(contents) = read_bounded_file(&layout.private_directory.join("config.worktree"))
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
    for line in logical_config_lines(contents) {
        let mut line = trim_ascii(strip_config_comment(&line));
        if line.is_empty() {
            continue;
        }
        if line.starts_with(b"[") {
            let Some((parsed_section, remainder)) = parse_config_header(line) else {
                section = None;
                continue;
            };
            section = parsed_section;
            line = trim_ascii(remainder);
            if line.is_empty() {
                continue;
            }
        }
        let Some(section) = section else {
            continue;
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
        let Some(value) = parse_git_bool(value.unwrap_or(b"true")) else {
            continue;
        };
        match section {
            GitConfigSection::Core if key.eq_ignore_ascii_case(b"ignorecase") => {
                config.ignore_case = Some(value);
            }
            GitConfigSection::Core if key.eq_ignore_ascii_case(b"precomposeunicode") => {
                config.precompose_unicode = Some(value);
            }
            GitConfigSection::Extensions if key.eq_ignore_ascii_case(b"worktreeconfig") => {
                config.worktree_config = Some(value);
            }
            _ => {}
        }
    }
}

/// Splits a section header from a variable written on the same physical line.
/// Git resumes its character-based parser immediately after the closing `]`,
/// so both `[core] key = value` and `[core]key = value` leave `core` active.
fn parse_config_header(line: &[u8]) -> Option<(Option<GitConfigSection>, &[u8])> {
    let end = line.iter().position(|byte| *byte == b']')?;
    Some((parse_top_level_section(&line[..=end]), &line[end + 1..]))
}

/// Produces the logical config lines that Git's value parser sees. A terminal
/// backslash in an assignment value consumes the following physical newline,
/// then parsing resumes with the next line's bytes (including indentation).
/// It works inside and outside quotes, but a backslash in a comment is ignored.
/// A terminal backslash at EOF is consumed with Git's synthetic final newline.
/// Every other quote and escape is retained for `parse_git_bool`.
fn logical_config_lines(contents: &[u8]) -> Vec<Vec<u8>> {
    let mut physical = contents.split_inclusive(|byte| *byte == b'\n').map(|line| {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        line.strip_suffix(b"\r").unwrap_or(line)
    });
    let mut logical = Vec::new();
    while let Some(line) = physical.next() {
        let mut line = line.to_vec();
        let mut continuation = config_value_continuation(&line);
        while let Some(state) = continuation {
            line.pop();
            let Some(next) = physical.next() else {
                break;
            };
            line.extend_from_slice(next);
            continuation = config_value_suffix_continuation(next, state);
        }
        logical.push(line);
    }
    logical
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

    use super::{
        GitConfig, MAX_IGNORE_RULES, RuleSetBuilder, add_rules, apply_config, logical_config_lines,
        parse_git_bool, validate_rule_count,
    };

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

        let lines = logical_config_lines(&contents);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with(b"true"));
        assert_eq!(lines[1].len(), 2 + CONTINUATION_LINES * 2 + 4);
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
}
