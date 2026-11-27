#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Compiled, byte-first glob patterns with explicit behavior-changing options.
//!
//! A [`Pattern`] is compiled once from any `AsRef<[u8]>` and matched many
//! times against bytes, so filenames never pass through a lossy UTF-8
//! conversion. Syntax that changes meaning is opt-in through
//! [`PatternOptions`]: recursive `**`, braces, extglobs, hidden-name matching,
//! and ASCII case folding are off in [`PatternOptions::default`] until asked
//! for. [`PatternOptions::walker`] asks for the first three at once, which is
//! the dialect the `ferralk` walker reads its patterns in.
//!
//! ```
//! use ferralk_glob::{Pattern, PatternOptions};
//!
//! let pattern = Pattern::compile("src/**/*.{rs,toml}", PatternOptions::walker())?;
//!
//! assert!(pattern.is_match_glob_path("src/lib.rs"));
//! assert!(pattern.is_match_glob_path("src/parser/Cargo.toml"));
//! assert!(!pattern.is_match_glob_path("src/lib.rs.bak"));
//! # Ok::<(), ferralk_glob::PatternError>(())
//! ```
//!
//! Three entry points decide how far a wildcard may reach:
//!
//! - [`Pattern::is_match_glob_path`] keeps every ordinary wildcard inside one
//!   path component, as a shell glob does. Use it for filesystem paths.
//! - [`Pattern::is_match`] compares a whole byte sequence in which `/` is not
//!   special.
//! - [`Pattern::is_match_path`] and the [`Pattern::filter_paths`] family keep
//!   zlob's list-filter rule for callers porting from it.
//!
//! The same pattern, `*.rs`, asked through each of them:
//!
//! | Candidate | [`is_match`](Pattern::is_match) | [`is_match_path`](Pattern::is_match_path) | [`is_match_glob_path`](Pattern::is_match_glob_path) |
//! | --- | --- | --- | --- |
//! | `lib.rs` | yes | yes | yes |
//! | `src/lib.rs` | yes: `*` crosses `/` | yes: a wildcard in the first component crosses | no: `*` stays in one component |
//! | `./lib.rs` | no: `./` is compared literally | yes: one leading `./` is ignored | yes: one leading `./` is ignored |
//!
//! The matcher covers literals, `*`, `?`, `**`, character
//! classes, escapes, leading-period handling, ASCII case folding, nested brace
//! expansion, and Bash-style extglobs. The
//! [usage guide](https://github.com/sebastian-software/ferralk/blob/main/docs/usage.md)
//! documents each option with its default.
//!
//! # Recipes
//!
//! Every recipe below is a tested example. Finding files on disk is the
//! `ferralk` crate's job; its
//! [recipes](https://docs.rs/ferralk/latest/ferralk/#recipes) cover
//! `.gitignore`, include and exclude lists, early termination, errors, and
//! async use.
//!
//! ## Match a path against several configured globs
//!
//! A [`PatternSet`] is the counterpart of `globset::GlobSet`: it compiles a
//! list once and answers whether any glob matches, or which ones do, by
//! their position in the list. It asks only the globs whose literal
//! extension, component, prefix or suffix the path has, so a long list costs
//! little more per path than a short one.
//!
//! ```
//! use ferralk_glob::{PatternOptions, PatternSet, PatternSetError};
//!
//! let globs = ["src/**/*.rs", "tests/**/*.rs", "*.toml"];
//! let set = PatternSet::new(globs, PatternOptions::walker())?;
//!
//! assert!(set.is_match_glob_path("src/parser/lexer.rs"));
//! assert!(set.is_match_glob_path("Cargo.toml"));
//! // `*` stays in its component, so `*.toml` names the top level only.
//! assert!(!set.is_match_glob_path("crates/cli/Cargo.toml"));
//!
//! let mut which = Vec::new();
//! set.matches_glob_path_into("tests/cli.rs", &mut which);
//! assert_eq!(which, [1]);
//! # Ok::<(), PatternSetError>(())
//! ```
//!
//! [`PatternSetError::index`] names the glob that failed to compile. For
//! globs with different options, collect compiled [`Pattern`]s into the set
//! instead; [`PatternSet`] shows both.
//!
//! ## Port a fast-glob or globby list with `!` negations
//!
//! A leading `!` is not negation here: the matcher reads it as a literal `!`,
//! so `!**/*.test.ts` compiles and matches nothing. Split such a list into
//! includes and excludes first. fast-glob itself keeps `!(…)` as a negated
//! extglob, and so does the split below.
//!
//! ```
//! use ferralk_glob::{Pattern, PatternError, PatternOptions, PatternSet};
//!
//! fn compile_list(globs: &[&str]) -> Result<(PatternSet, PatternSet), PatternError> {
//!     let options = PatternOptions::walker();
//!     let (mut include, mut exclude) = (Vec::new(), Vec::new());
//!     for glob in globs {
//!         match glob.strip_prefix('!').filter(|rest| !rest.starts_with('(')) {
//!             Some(negated) => exclude.push(Pattern::compile(negated, options)?),
//!             None => include.push(Pattern::compile(glob, options)?),
//!         }
//!     }
//!     Ok((include.into_iter().collect(), exclude.into_iter().collect()))
//! }
//!
//! let (include, exclude) =
//!     compile_list(&["src/**/*.ts", "!src/**/*.test.ts", "!**/generated/**"])?;
//! let is_selected =
//!     |path: &str| include.is_match_glob_path(path) && !exclude.is_match_glob_path(path);
//! assert!(is_selected("src/app/main.ts"));
//! assert!(!is_selected("src/app/main.test.ts"));
//! assert!(!is_selected("src/generated/client.ts"));
//! # Ok::<(), PatternError>(())
//! ```
//!
//! fast-glob's `dot: true` is [`PatternOptions::match_hidden`]`(true)`, and
//! its `caseSensitiveMatch: false` is
//! [`PatternOptions::case_insensitive`]`(true)`. A walker takes the same
//! list through `include` and `exclude`, and rejects a leading `!` instead of
//! reading it literally.
//!
//! ## Match a `Path`
//!
//! Patterns take bytes, and [`path_bytes`] gives them for a
//! [`Path`](std::path::Path) or an [`OsStr`] without a lossy
//! conversion. Match the part of the path the pattern is written for: a
//! pattern is relative, so strip the directory it is relative to first.
//!
//! ```
//! use std::path::Path;
//!
//! use ferralk_glob::{Pattern, PatternOptions, path_bytes};
//!
//! let pattern = Pattern::compile("src/**/*.rs", PatternOptions::walker())?;
//! let root = Path::new("/work/project");
//! let path = root.join("src").join("bin").join("main.rs");
//!
//! let relative = path.strip_prefix(root).expect("path is below the root");
//! assert!(pattern.is_match_glob_path(path_bytes(relative)));
//! // The unstripped path starts with `/work`, which `src/**` does not.
//! assert!(!pattern.is_match_glob_path(path_bytes(&path)));
//! # Ok::<(), ferralk_glob::PatternError>(())
//! ```
//!
//! A path that `ferralk` walked already carries its root-relative part as
//! `WalkEntry::relative_path`, the spelling the walker's own patterns match.
//!
//! ## Put literal text into a pattern
//!
//! A directory name, a path, or user input can contain bytes that are syntax.
//! [`escape`] and [`escape_str`] make them literal under every option
//! combination with escaping enabled, so the text can be joined with glob
//! syntax of the caller's own.
//!
//! ```
//! use ferralk_glob::{Pattern, PatternOptions, escape_str};
//!
//! let directory = "reports {2024} [final]";
//! let glob = format!("{}/**/*.csv", escape_str(directory));
//! let pattern = Pattern::compile(&glob, PatternOptions::walker())?;
//!
//! assert!(pattern.is_match_glob_path("reports {2024} [final]/q1/sales.csv"));
//! assert!(!pattern.is_match_glob_path("reports 2024 f/q1/sales.csv"));
//!
//! // Unescaped, the braces are an alternation and the brackets a class.
//! let unescaped = Pattern::compile(format!("{directory}/**/*.csv"), PatternOptions::walker())?;
//! assert!(unescaped.is_match_glob_path("reports 2024 f/q1/sales.csv"));
//! assert!(!unescaped.is_match_glob_path("reports {2024} [final]/q1/sales.csv"));
//! # Ok::<(), ferralk_glob::PatternError>(())
//! ```
//!
//! ## Filter a list of paths
//!
//! [`Pattern::filter_paths`] keeps zlob's list rule, in which a wildcard in
//! the first component crosses separators. For shell semantics, filter with
//! [`Pattern::is_match_glob_path`] instead; both are shown on
//! [`Pattern::filter_paths`].
//!
//! ## Report an invalid pattern
//!
//! [`PatternError::offset`] is a byte offset into the pattern, so a caller
//! can point at the problem; see [`PatternError`].
//!
//! Provenance: semantics are ported and differentially checked against zlob
//! v1.6.3, source commit 4bc4da2cbc823d3911b4a1436448687c398977dd, primarily
//! `zig-src/fnmatch.zig`, `zig-src/pattern_context.zig`, and
//! `test/test_fnmatch.zig`. Deliberate differences live in
//! the checked-in corpus and compatibility matrix.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
    error::Error,
    ffi::OsStr,
    fmt,
    hash::{Hash, Hasher},
};

use memchr::{memchr, memchr2, memchr3, memmem, memrchr};

mod set;
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
mod suffix_word;
mod sweep;

pub use set::{PatternSet, PatternSetError};
use sweep::{SweepEngine, SweepState};

/// A compiled glob pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    alternatives: Vec<CompiledAlternative>,
    alternative_fast_path: Option<Box<AlternativeFastPath>>,
    /// The list-filter routes of [`Pattern::is_match_path`], or `None` when
    /// every alternative answers it exactly like [`Pattern::is_match`].
    path_filter: Option<Box<PathFilter>>,
    walker_path_viability: WalkerPathViability,
    walker_path_problem_offset: Option<usize>,
    options: PatternOptions,
}

/// How the alternatives answer the list-filter entry point
/// [`Pattern::is_match_path`].
///
/// Every alternative has exactly one route, chosen from its own syntax and
/// never from a sibling's, so a brace arm's verdict does not depend on the
/// arms beside it.
#[derive(Debug, Clone)]
struct PathFilter {
    /// Second compiles for the component policy, with one leading `./`
    /// removed and a fast path chosen for component-local wildcards. Once
    /// one plain alternative needs such a copy every plain alternative gets
    /// one, so they are all matched in one contiguous pass; the copy of an
    /// alternative without a component-local position answers exactly as
    /// the original does. An extglob alternative gets a copy only for its
    /// leading `./`.
    compiled: Vec<CompiledAlternative>,
    /// Indices of extglob alternatives answered from their shared compile. A
    /// program reads the component policy at match time, so a group directly
    /// behind a separator is component-local exactly like a plain wildcard
    /// there.
    programs: Vec<usize>,
    /// Indices of plain alternatives answered like [`Pattern::is_match`],
    /// used only while no plain alternative needs a component copy.
    whole: Vec<usize>,
}

impl PathFilter {
    fn is_program(alternative: &CompiledAlternative, options: PatternOptions) -> bool {
        options.extglob && alternative.extglob.is_some() && !alternative.raw.starts_with(b"./")
    }

    fn compile(
        alternatives: &[CompiledAlternative],
        options: PatternOptions,
        budget: &mut IrBudget,
        defer_sweeps: bool,
    ) -> Result<Option<Self>, PatternError> {
        let needs_copy = |alternative: &CompiledAlternative| {
            alternative.raw.starts_with(b"./")
                || alternative.tokens.windows(2).any(|tokens| {
                    matches!(
                        tokens,
                        [Token::Separator, Token::Any | Token::Star | Token::Class(_)]
                    )
                })
        };
        let plain_needs_copy = alternatives
            .iter()
            .any(|alternative| !Self::is_program(alternative, options) && needs_copy(alternative));
        let programs = alternatives
            .iter()
            .filter(|item| Self::is_program(item, options))
            .count();
        let plain = alternatives.len() - programs;
        // Sized up front: the filter lives as long as the pattern.
        let (copies, whole) = if plain_needs_copy {
            (plain, 0)
        } else {
            (0, plain)
        };
        let mut filter = Self {
            compiled: Vec::with_capacity(copies),
            programs: Vec::with_capacity(programs),
            whole: Vec::with_capacity(whole),
        };
        for (index, alternative) in alternatives.iter().enumerate() {
            if Self::is_program(alternative, options) {
                filter.programs.push(index);
            } else if plain_needs_copy {
                // A second compiled copy of the alternative, so it costs the
                // budget a second time.
                filter
                    .compiled
                    .push(Pattern::compile_path_filter_alternative(
                        alternative,
                        options,
                        budget,
                        defer_sweeps,
                    )?);
            } else {
                filter.whole.push(index);
            }
        }
        Ok((!filter.compiled.is_empty() || !filter.programs.is_empty()).then_some(filter))
    }

    /// Answers `path` with `options` on the component routes and
    /// `whole_options` on the alternatives read like `is_match`.
    fn is_match(
        &self,
        alternatives: &[CompiledAlternative],
        options: PatternOptions,
        whole_options: PatternOptions,
        path: &[u8],
    ) -> bool {
        let shared = |indices: &[usize], options: PatternOptions| {
            indices.iter().any(|&index| {
                Pattern::match_alternatives(
                    std::slice::from_ref(&alternatives[index]),
                    options,
                    path,
                )
            })
        };
        Pattern::match_alternatives(&self.compiled, options, path)
            || shared(&self.programs, options)
            || shared(&self.whole, whole_options)
    }
}

/// Explicit switches that affect glob interpretation.
///
/// Two starting points have names. [`PatternOptions::default`] is the
/// conservative reading: `**`, braces and extglobs are ordinary text until a
/// switch enables them. [`PatternOptions::walker`] is the dialect the `ferralk`
/// walker compiles its include and exclude patterns in. Either one can be
/// adjusted further with the builder methods.
///
/// ```
/// use ferralk_glob::{Pattern, PatternOptions};
///
/// // `walker()` is `default()` with three switches turned on.
/// assert_eq!(
///     PatternOptions::walker(),
///     PatternOptions::default()
///         .recursive_double_star(true)
///         .braces(true)
///         .extglob(true),
/// );
///
/// // The same text is a different pattern under each of them.
/// let walker = Pattern::compile("*.{rs,toml}", PatternOptions::walker())?;
/// let conservative = Pattern::compile("*.{rs,toml}", PatternOptions::default())?;
/// assert!(walker.is_match_glob_path("Cargo.toml"));
/// assert!(!conservative.is_match_glob_path("Cargo.toml"));
///
/// // Further switches chain onto either one.
/// let loose = PatternOptions::walker().match_hidden(true).case_insensitive(true);
/// assert!(Pattern::compile("*.RS", loose)?.is_match_glob_path(".build.rs"));
/// # Ok::<(), ferralk_glob::PatternError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternOptions {
    braces: bool,
    recursive_double_star: bool,
    extglob: bool,
    match_hidden: bool,
    case_insensitive: bool,
    escape: bool,
    component_wildcards: bool,
    root_component_wildcards: bool,
    /// Whether the first wildcard of a compiled subpattern occupies a
    /// component-local position in its enclosing pattern.
    candidate_root_component_wildcard: bool,
    /// Whether byte zero of this candidate starts a path component.
    ///
    /// Extglob alternatives borrow a range from their enclosing candidate.
    /// The range can start in the middle of a component, where a leading dot
    /// is an ordinary byte rather than a hidden-name marker.
    candidate_starts_component: bool,
}

impl Default for PatternOptions {
    fn default() -> Self {
        Self::CONSERVATIVE
    }
}

impl PatternOptions {
    /// The value [`Default::default`] returns, spelled as a constant so that
    /// [`Self::walker`] can build on it in a `const fn`.
    const CONSERVATIVE: Self = Self {
        braces: false,
        recursive_double_star: false,
        extglob: false,
        match_hidden: false,
        case_insensitive: false,
        escape: true,
        component_wildcards: false,
        root_component_wildcards: false,
        candidate_root_component_wildcard: false,
        candidate_starts_component: true,
    };

    /// The dialect `ferralk::Walker` compiles its include and exclude patterns
    /// in: [`default`](Self::default) with recursive `**`, braces and extglobs
    /// enabled.
    ///
    /// Compile a walker pattern with these options to validate it up front or
    /// to match it again with the walker's semantics, for instance inside a
    /// `visit` predicate or over paths that never touched a filesystem. The
    /// walker builds its own options from this constructor, so the two cannot
    /// drift apart.
    ///
    /// Every other switch keeps its default. In particular a wildcard does not
    /// cover a leading period, which matches the walker's default for
    /// includes; chain [`match_hidden(true)`](Self::match_hidden) to mirror
    /// `Walker::match_hidden(true)`, or to match an exclude, which the walker
    /// always lets cover a leading period. Case folding and escaping are not
    /// walker settings and stay at their defaults there too.
    ///
    /// Options select syntax, and the entry point selects how far a wildcard
    /// reaches. For a root-relative path, [`Pattern::is_match_glob_path`]
    /// answers as the walker does under its default
    /// `WildcardMode::ComponentScoped`, and [`Pattern::is_match`] as it does
    /// under `WildcardMode::SeparatorCrossing`. A few walker rules sit outside
    /// the pattern dialect: the walker reads a trailing `/` as "directories
    /// only", rewrites an absolute pattern against its root, and refuses a
    /// pattern no walk can match, such as one with a `..` component, which
    /// [`Pattern::compile`] accepts as matcher text.
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let walker = PatternOptions::walker();
    /// let conservative = PatternOptions::default();
    ///
    /// // `**` crosses directories only in the walker dialect; by default it
    /// // is two ordinary stars that stay inside one component.
    /// let recursive = Pattern::compile("src/**/*.rs", walker)?;
    /// assert!(recursive.is_match_glob_path("src/lib.rs"));
    /// assert!(recursive.is_match_glob_path("src/parser/token.rs"));
    /// let plain = Pattern::compile("src/**/*.rs", conservative)?;
    /// assert!(!plain.is_match_glob_path("src/lib.rs"));
    ///
    /// // Braces are alternatives in the walker dialect and literal text by
    /// // default.
    /// let either = Pattern::compile("*.{rs,toml}", walker)?;
    /// assert!(either.is_match_glob_path("lib.rs"));
    /// assert!(either.is_match_glob_path("Cargo.toml"));
    /// let literal = Pattern::compile("*.{rs,toml}", conservative)?;
    /// assert!(!literal.is_match_glob_path("lib.rs"));
    /// assert!(literal.is_match_glob_path("odd.{rs,toml}"));
    ///
    /// // Like the walker, the preset leaves hidden names to an explicit opt-in.
    /// assert!(!recursive.is_match_glob_path("src/.cache/lib.rs"));
    /// let hidden = Pattern::compile("src/**/*.rs", walker.match_hidden(true))?;
    /// assert!(hidden.is_match_glob_path("src/.cache/lib.rs"));
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    #[must_use]
    pub const fn walker() -> Self {
        Self::CONSERVATIVE
            .recursive_double_star(true)
            .braces(true)
            .extglob(true)
    }

    /// Enables nested brace alternatives.
    #[must_use]
    pub const fn braces(mut self, enabled: bool) -> Self {
        self.braces = enabled;
        self
    }

    /// Gives a `**` that forms a whole path component recursive,
    /// separator-crossing semantics, as gitignore, Bash `globstar`, `globset`,
    /// and `fast-glob` do.
    ///
    /// A star run is a whole component when an unescaped `/` or an end of the
    /// pattern bounds it on each side: `**`, `**/x`, `x/**`, `x/**/y`. Braces
    /// are expanded first, so every alternative is judged as the pattern it
    /// expands to, and an extglob alternative takes its group's position. A
    /// `**/` matches zero or more whole components and hands over only at a
    /// component start, so `**/x` matches `x` and `a/x` but not `sx`, and
    /// `a/**/b` matches `a/b` but not `a/xb`. A trailing `/**` also accepts the
    /// path without it: `x/**` matches `x`. An extglob group reads like its
    /// alternatives written in its place, so both rules hold around groups
    /// too: `@(**)/y` matches `y`, and `@(x)/**` and `x/@(**)` match `x`.
    ///
    /// Any other run, such as `a**`, `**b`, or `**.ts`, stays ordinary stars:
    /// component-local under [`Pattern::is_match_glob_path`], the position
    /// rule under [`Pattern::is_match_path`], and separator-crossing under
    /// [`Pattern::is_match`]. When disabled, every run reads that way.
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let options = PatternOptions::default().recursive_double_star(true);
    /// let whole = Pattern::compile("**/x", options)?;
    /// assert!(whole.is_match_glob_path("x"));
    /// assert!(whole.is_match_glob_path("a/b/x"));
    /// assert!(!whole.is_match_glob_path("a/sx"));
    ///
    /// let attached = Pattern::compile("a**/x", options)?;
    /// assert!(attached.is_match_glob_path("abc/x"));
    /// assert!(!attached.is_match_glob_path("a/b/x"));
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    #[must_use]
    pub const fn recursive_double_star(mut self, enabled: bool) -> Self {
        self.recursive_double_star = enabled;
        self
    }

    /// Enables Bash-style extglobs.
    #[must_use]
    pub const fn extglob(mut self, enabled: bool) -> Self {
        self.extglob = enabled;
        self
    }

    /// Allows wildcard tokens to match a leading period in a path component.
    #[must_use]
    pub const fn match_hidden(mut self, enabled: bool) -> Self {
        self.match_hidden = enabled;
        self
    }

    /// Performs ASCII-only case-insensitive matching.
    #[must_use]
    pub const fn case_insensitive(mut self, enabled: bool) -> Self {
        self.case_insensitive = enabled;
        self
    }

    /// Enables backslash escapes in patterns.
    #[must_use]
    pub const fn escape(mut self, enabled: bool) -> Self {
        self.escape = enabled;
        self
    }
}

/// An error returned when a pattern is not syntactically valid.
///
/// [`offset`](Self::offset) is a zero-based byte offset into the pattern, so
/// a caller can point at the construct that failed. `Display` reads
/// `<message> at byte <offset>`; the message is diagnostic text, while the
/// offset is the stable part.
///
/// ```
/// use ferralk_glob::{Pattern, PatternOptions};
///
/// let source = "src/[a-z/*.rs";
/// let error = Pattern::compile(source, PatternOptions::walker()).unwrap_err();
/// assert_eq!(error.offset(), 4); // the `[` that is never closed
///
/// // A byte offset is a column only for ASCII; count the characters before it
/// // to place a marker under any pattern.
/// let column = source[..error.offset()].chars().count();
/// let report = format!("{error}\n{source}\n{:>width$}", "^", width = column + 1);
/// assert!(report.ends_with("src/[a-z/*.rs\n    ^"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError {
    offset: usize,
    message: &'static str,
}

impl PatternError {
    /// Builds an error for a pattern a layer above the matcher rejected.
    ///
    /// Compilation produces these on its own; this exists for callers that
    /// impose rules the pattern language cannot express, such as a walker
    /// deciding that an absolute pattern cannot be rewritten for its root. Such
    /// a caller already returns `PatternError`, and a second error type in the
    /// same position would say nothing extra.
    ///
    /// `offset` is a byte offset into the pattern the caller was given, and
    /// `message` describes the construct rather than the caller's own state, so
    /// that [`Display`](fmt::Display) reads the same as a compilation error.
    #[must_use]
    pub const fn new(offset: usize, message: &'static str) -> Self {
        Self { offset, message }
    }

    /// Zero-based byte offset of the invalid construct.
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Description of the invalid construct.
    ///
    /// This text is intended for diagnostics and is not a stable interface;
    /// use [`Self::offset`] for programmatic handling.
    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for PatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at byte {}", self.message, self.offset)
    }
}

impl Error for PatternError {}

/// Whether a compiled pattern has an arm a walker can spell as a candidate.
///
/// This is compiler metadata for path-consuming embeddings. It is calculated
/// after brace expansion from the compiled token and extglob branch objects,
/// rather than by parsing the source expression again.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WalkerPathViability {
    /// At least one compiled arm avoids root-only and `.` components, and no
    /// brace-expanded arm contains a literal `..` component.
    Viable,
    /// A compiled walker arm contains a literal `..` component.
    ParentComponent,
    /// Every compiled arm names only the walk root.
    Root,
    /// Every compiled arm has an empty leading or interior component: a
    /// repeated separator, a leading `/`, or an empty alternative.
    EmptyComponent,
    /// Every compiled arm ends in a literal `.` component.
    TrailingDot,
    /// Every compiled arm contains a non-final literal `.` component.
    DotComponent,
}

impl Pattern {
    /// Reports whether the enabled syntax options make a pattern non-literal.
    ///
    /// This is a byte-first preflight helper. Like zlob's `hasWildcards`, it
    /// deliberately reports syntax markers even when an escape option would
    /// later make a particular marker literal.
    #[must_use]
    pub fn has_wildcards(pattern: impl AsRef<[u8]>, options: PatternOptions) -> bool {
        let pattern = pattern.as_ref();
        if memchr3(b'*', b'?', b'[', pattern).is_some() {
            return true;
        }
        if options.braces && memchr(b'{', pattern).is_some() {
            return true;
        }
        if !options.extglob {
            return false;
        }
        let mut offset = 0;
        while let Some(found) = memchr(b'(', &pattern[offset..]) {
            let index = offset + found;
            if index > 0 && matches!(pattern[index - 1], b'?' | b'*' | b'+' | b'@' | b'!') {
                return true;
            }
            offset = index + 1;
        }
        false
    }

    /// Compiles a pattern once for repeated matching against raw path bytes.
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let source_file = Pattern::compile("src/**/*.{rs,toml}", PatternOptions::walker())?;
    ///
    /// assert!(source_file.is_match_glob_path("src/lib.rs"));
    /// assert!(!source_file.is_match_glob_path("src/generated/lib.rs.bak"));
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a [`PatternError`] when the pattern is not valid under
    /// `options`: an unclosed class or extglob group, an invalid range, or a
    /// pattern beyond the compile budget. [`PatternError::offset`] locates the
    /// problem in the caller's pattern bytes.
    pub fn compile(
        pattern: impl AsRef<[u8]>,
        options: PatternOptions,
    ) -> Result<Self, PatternError> {
        let pattern = pattern.as_ref();
        // Direct source is an implicit identity range. Brace expansion only
        // materializes the few source spans whose byte positions actually
        // change; a literal never pays one `usize` per input byte.
        let source_provenance = SourceProvenance::Contiguous { source_start: 0 };
        let mut budget = IrBudget::new();
        let mut provenance_budget = ProvenanceBudget::new();
        let mut compiled = Self::compile_within(
            pattern,
            options,
            &mut budget,
            &mut provenance_budget,
            Some(&source_provenance),
            pattern.starts_with(b"./"),
            ComponentBounds::WHOLE,
        )?;
        let (viability, offset) = walker_path_analysis(&compiled.alternatives, &mut budget)?;
        compiled.walker_path_viability = viability;
        compiled.walker_path_problem_offset = offset;
        if compiled.alternatives.len() > 1 {
            compiled.populate_deferred_sweeps(&mut budget)?;
        }
        compiled.release_compile_state();
        Ok(compiled)
    }

    /// Drops what only the compile reads.
    ///
    /// The walker path shapes and the extglob source provenance exist for
    /// [`walker_path_analysis`], whose verdict is stored on the pattern.
    /// Nothing a match reads changes.
    fn release_compile_state(&mut self) {
        let copies = self
            .path_filter
            .iter_mut()
            .flat_map(|filter| filter.compiled.iter_mut());
        for alternative in self.alternatives.iter_mut().chain(copies) {
            alternative.release_compile_state();
        }
    }

    /// Summarizes whether a walker can represent a match from this pattern.
    ///
    /// The summary is derived from the actual alternatives and extglob arms
    /// [`Pattern::compile`] created. It intentionally exposes semantics rather
    /// than source offsets, which can no longer identify one branch after
    /// brace expansion.
    #[doc(hidden)]
    #[must_use]
    pub const fn walker_path_viability(&self) -> WalkerPathViability {
        self.walker_path_viability
    }

    /// Byte offset of the determinate component behind walker viability.
    ///
    /// The compiler retains original source provenance through brace expansion
    /// whenever an offending byte survives in an expanded arm. It returns
    /// `None` only when no one source location is available and an embedding
    /// must use its conventional fallback location.
    #[doc(hidden)]
    #[must_use]
    pub const fn walker_path_problem_offset(&self) -> Option<usize> {
        self.walker_path_problem_offset
    }

    /// The compile every alternative shares, carrying the budget that bounds
    /// their total.
    ///
    /// Brace expansion recurses through here once per alternative, and an
    /// extglob group's alternatives recurse again, so one budget threaded down
    /// is what makes the total observable at all: each call on its own looks
    /// small.
    #[allow(clippy::too_many_arguments)]
    fn compile_within(
        pattern: &[u8],
        options: PatternOptions,
        budget: &mut IrBudget,
        provenance_budget: &mut ProvenanceBudget,
        walker_source_provenance: Option<&SourceProvenance>,
        leading_dot_is_normalized: bool,
        bounds: ComponentBounds,
    ) -> Result<Self, PatternError> {
        if options.braces {
            let parse_options = PatternOptions {
                braces: false,
                ..options
            };
            ensure_source_brace_compiled_ir_lower_bound(
                pattern,
                parse_options,
                budget,
                provenance_budget,
            )?;
            let expanded = expand_brace_alternatives_with_provenance(
                pattern,
                walker_source_provenance,
                options.escape,
                provenance_budget,
            )?;
            ensure_brace_compiled_ir_lower_bound(&expanded, parse_options, budget)?;
            // Multi-alternative brace expansions defer optional `SweepEngine`
            // tables until mandatory tokens, `PathFilter` copies, and
            // `walker_path_analysis` have been charged, so optional sweeps
            // cannot starve mandatory IR and can fall back to the token
            // matcher once the remaining `IrBudget` is exhausted.
            let defer_sweeps = expanded.len() > 1;
            // Every expanded alternative compiles to exactly one, so the
            // pattern keeps no spare capacity.
            let mut alternatives = Vec::with_capacity(expanded.len());
            for alternative in expanded {
                // The path entry points ignore one leading `./` on every
                // expanded alternative, not only at the start of the source,
                // so the walker analysis reads each alternative's prefix the
                // same way (#395).
                let leading_dot_is_normalized =
                    leading_dot_is_normalized || alternative.bytes.starts_with(b"./");
                alternatives.push(Self::compile_single_alternative(
                    &alternative.bytes,
                    parse_options,
                    budget,
                    provenance_budget,
                    alternative.source_provenance.as_ref(),
                    leading_dot_is_normalized,
                    bounds,
                    defer_sweeps,
                )?);
            }
            return Self::from_alternatives(alternatives, options, budget, defer_sweeps);
        }
        let alternative = Self::compile_single_alternative(
            pattern,
            options,
            budget,
            provenance_budget,
            walker_source_provenance,
            leading_dot_is_normalized,
            bounds,
            false,
        )?;
        Self::from_alternatives(vec![alternative], options, budget, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn compile_single_alternative(
        pattern: &[u8],
        options: PatternOptions,
        budget: &mut IrBudget,
        provenance_budget: &mut ProvenanceBudget,
        walker_source_provenance: Option<&SourceProvenance>,
        leading_dot_is_normalized: bool,
        bounds: ComponentBounds,
        defer_sweeps: bool,
    ) -> Result<CompiledAlternative, PatternError> {
        let mut tokens = Vec::new();
        let mut literals = Vec::new();
        // Keep the walker-only path shape beside the token build. In
        // particular an escaped dot and an ordinary dot both become a literal
        // matcher token, but only the latter is a path operation a walker
        // must reject.
        let mut walker_path = WalkerPathShapeBuilder::new(leading_dot_is_normalized);
        let mut index = 0;
        // Charged as the tokens appear rather than per byte: a literal run is
        // one token however long it is, so billing bytes would reject patterns
        // that compile to almost nothing.
        let mut charged = 0;
        // The star run `index` is in, classified once at its first star: only
        // a whole-component run may emit the recursive tokens (#419).
        let mut star_run_end = 0;
        let mut star_run_is_recursive = false;

        while index < pattern.len() {
            budget.charge(tokens.len() - charged, 0)?;
            charged = tokens.len();
            if pattern[index] == b'*' && index >= star_run_end {
                star_run_end = index
                    + pattern[index..]
                        .iter()
                        .take_while(|&&byte| byte == b'*')
                        .count();
                star_run_is_recursive = options.recursive_double_star
                    && star_run_is_whole_component(
                        pattern,
                        index,
                        star_run_end,
                        options.escape,
                        bounds,
                    );
            }
            match pattern[index] {
                b'/' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Separator);
                    walker_path.separator(
                        walker_source_provenance.and_then(|provenance| provenance.offset_at(index)),
                    );
                    index += 1;
                }
                b'*' if star_run_is_recursive && pattern.get(index + 1) == Some(&b'*') => {
                    flush_literals(&mut tokens, &mut literals);
                    if pattern.get(index + 2) == Some(&b'/') {
                        tokens.push(Token::RecursivePrefix);
                        walker_path.wildcard();
                        walker_path.separator(
                            walker_source_provenance
                                .and_then(|provenance| provenance.offset_at(index + 2)),
                        );
                        index += 3;
                    } else {
                        tokens.push(Token::RecursiveStar);
                        walker_path.wildcard();
                        index += 2;
                    }
                }
                b'*' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Star);
                    walker_path.wildcard();
                    index += 1;
                }
                b'?' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Any);
                    walker_path.wildcard();
                    index += 1;
                }
                b'[' => {
                    flush_literals(&mut tokens, &mut literals);
                    let (class, next) =
                        parse_class(pattern, index, options.escape).map_err(|mut error| {
                            if let Some(source_offset) = walker_source_provenance
                                .and_then(|provenance| provenance.offset_at(error.offset))
                            {
                                error.offset = source_offset;
                            }
                            error
                        })?;
                    // A class token owns its member list, so it costs more than
                    // the one unit the loop charges for the token itself.
                    budget.charge(class.members.len(), 0)?;
                    tokens.push(Token::Class(class));
                    walker_path.wildcard();
                    index = next;
                }
                b'\\' if options.escape => {
                    if let Some(&escaped) = pattern.get(index + 1) {
                        literals.push(escaped);
                        walker_path.escaped();
                        index += 2;
                    } else {
                        // zlob's fnmatch core treats a trailing backslash as
                        // a literal backslash instead of rejecting the pattern.
                        literals.push(b'\\');
                        walker_path.escaped();
                        index += 1;
                    }
                }
                byte => {
                    literals.push(byte);
                    walker_path.literal(
                        byte,
                        walker_source_provenance.and_then(|provenance| provenance.offset_at(index)),
                    );
                    index += 1;
                }
            }
        }
        flush_literals(&mut tokens, &mut literals);
        budget.charge(tokens.len() - charged, 0)?;

        let extglob = compile_extglob(
            pattern,
            options,
            budget,
            provenance_budget,
            walker_source_provenance,
            leading_dot_is_normalized,
        )?;
        if defer_sweeps {
            ensure_retained_alternative_bytes(pattern.len(), &tokens, budget)?;
        }
        let fast_path = FastPath::compile(&tokens, options);
        let prefilter = Prefilter::compile(&tokens);
        if defer_sweeps {
            charge_retained_alternative_bytes(
                pattern.len(),
                &tokens,
                &fast_path,
                &prefilter,
                budget,
            )?;
        }
        let sweep = if defer_sweeps {
            None
        } else {
            compile_sweep(
                &tokens,
                &fast_path,
                extglob.as_deref(),
                options,
                budget,
                None,
                false,
                false,
            )?
        };
        let walker_path_shape = walker_path.finish();
        Ok(CompiledAlternative {
            extglob,
            raw: pattern.to_vec(),
            fast_path,
            prefilter,
            sweep,
            tokens,
            walker_path_shape,
        })
    }

    /// Reports whether a pattern is syntactically valid without retaining it.
    ///
    /// # Errors
    ///
    /// Returns the same [`PatternError`] that [`Pattern::compile`] would.
    pub fn validate(
        pattern: impl AsRef<[u8]>,
        options: PatternOptions,
    ) -> Result<(), PatternError> {
        Self::compile(pattern, options).map(|_| ())
    }

    /// Matches the entire candidate byte sequence. Ordinary wildcards may
    /// cross separators, and a leading `./` is compared literally.
    ///
    /// Use it for text in which `/` is not special. For a filesystem path,
    /// [`Self::is_match_glob_path`] is usually the entry point you want; the
    /// same three candidates are shown on each of the three.
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let pattern = Pattern::compile("*.rs", PatternOptions::walker())?;
    ///
    /// assert!(pattern.is_match("lib.rs"));
    /// // `*` crosses the separator.
    /// assert!(pattern.is_match("src/lib.rs"));
    /// // `./` is compared literally, and `*` does not cover its leading period.
    /// assert!(!pattern.is_match("./lib.rs"));
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    ///
    /// Extglob patterns that fall back to the retained interpreter can still
    /// spend quadratic time on one adversarially long component. For
    /// untrusted path-shaped input, prefer a component-scoped entry point
    /// such as [`Self::is_match_path`] where that matches the caller's
    /// semantics.
    #[must_use]
    pub fn is_match(&self, path: impl AsRef<[u8]>) -> bool {
        let path = path.as_ref();
        if let [alternative] = self.alternatives.as_slice()
            && (!self.options.extglob || alternative.extglob.is_none())
            && let Some(fast_path) = &alternative.fast_path
        {
            return fast_path.is_match(&alternative.tokens, path, self.options);
        }
        if let Some(fast_path) = &self.alternative_fast_path {
            return fast_path.is_match(path, self.options, &self.alternatives);
        }
        Self::match_alternatives(&self.alternatives, self.options, path)
    }

    /// Reports whether every match engine agrees on `path`.
    ///
    /// Hidden from documentation: it exists for the differential fuzz
    /// harness, which cannot strip the per-alternative engines from outside
    /// the crate. Each entry point is asked as compiled, with the fast paths
    /// stripped so the sweep engine answers, and with the sweep stripped too
    /// so the memoized matcher answers.
    #[doc(hidden)]
    #[cfg(any(test, feature = "unstable-test-hooks"))]
    #[must_use]
    pub fn engines_agree(&self, path: impl AsRef<[u8]>) -> bool {
        let path = path.as_ref();
        let answers = |pattern: &Self| {
            (
                pattern.is_match(path),
                pattern.is_match_path(path),
                pattern.is_match_glob_path(path),
            )
        };
        let mut sweep_only = self.clone();
        sweep_only.strip_engines(true, false, false);
        let mut memoized = self.clone();
        memoized.strip_engines(true, true, true);
        let compiled = answers(self);
        compiled == answers(&sweep_only) && compiled == answers(&memoized)
    }

    /// Removes accelerated engines so a differential run can pin one engine.
    ///
    /// `prefilters` neutralizes the fixed-ends prefilter as well, which turns
    /// the stripped pattern into the bare memoized engine — the oracle the
    /// prefilter and the sweep are both held against.
    #[cfg(any(test, feature = "unstable-test-hooks"))]
    fn strip_engines(&mut self, fast_paths: bool, sweeps: bool, prefilters: bool) {
        if fast_paths {
            self.alternative_fast_path = None;
        }
        let options = self.options;
        for alternative in self.compiled_alternatives_mut() {
            alternative.strip_engines(options, fast_paths, sweeps, prefilters);
        }
    }

    /// Every compiled alternative, including the list filter's component
    /// copies, so a differential run can rewrite all of them.
    #[cfg(any(test, feature = "unstable-test-hooks"))]
    fn compiled_alternatives_mut(&mut self) -> impl Iterator<Item = &mut CompiledAlternative> {
        self.alternatives.iter_mut().chain(
            self.path_filter
                .iter_mut()
                .flat_map(|filter| filter.compiled.iter_mut()),
        )
    }

    /// Matches `path` against any of `alternatives` under `options`.
    ///
    /// Free of `self` so a compiled extglob alternative can be run through the
    /// same routing without owning a `Pattern`.
    fn match_alternatives(
        alternatives: &[CompiledAlternative],
        options: PatternOptions,
        path: &[u8],
    ) -> bool {
        alternatives.iter().any(|alternative| {
            if options.extglob
                && let Some(program) = &alternative.extglob
            {
                match_extglob_program(program, path, options)
            } else if let Some(fast_path) = &alternative.fast_path
                && (!options.component_wildcards || fast_path.supports_component_wildcards())
            {
                fast_path.is_match(&alternative.tokens, path, options)
            } else {
                // Neither engine consults the pattern's fixed ends before it
                // walks the candidate, so a candidate that cannot possibly
                // match is rejected here rather than after the walk: the sweep
                // pays per byte and the memoized engine per visited state.
                !alternative
                    .prefilter
                    .rejects(path, options.case_insensitive)
                    && if let Some(sweep) = &alternative.sweep {
                        sweep.is_match(path, options)
                    } else {
                        Self::matches_general(&alternative.tokens, path, options)
                    }
            }
        })
    }

    /// Runs the general matcher on the thread's reusable scratch buffers.
    ///
    /// The visited matrix and the work list are sized by the candidate, so
    /// allocating them per call dominated a walker filter: `**/*.ts` leaves the
    /// inline matrix budget for any relative path longer than about 40 bytes.
    /// Borrowing them from a thread-local keeps repeated calls allocation-free
    /// without putting state on [`Pattern`], which stays `Send + Sync`.
    fn matches_general(tokens: &[Token], path: &[u8], options: PatternOptions) -> bool {
        SCRATCH.with(|cell| match cell.try_borrow_mut() {
            Ok(mut scratch) => {
                let matched = Self::matches_with_scratch(tokens, path, options, &mut scratch);
                // One oversized candidate must not pin a large matrix on a
                // worker thread for the rest of the process.
                if scratch.visited.len() > RETAINED_SCRATCH_WORDS {
                    scratch.visited.truncate(RETAINED_SCRATCH_WORDS);
                    scratch.visited.shrink_to_fit();
                }
                matched
            }
            // A nested match on this thread cannot share the borrowed buffers.
            Err(_) => Self::matches_with_scratch(tokens, path, options, &mut Scratch::default()),
        })
    }

    fn matches_with_scratch(
        tokens: &[Token],
        path: &[u8],
        options: PatternOptions,
        scratch: &mut Scratch,
    ) -> bool {
        // Generation 0 marks a stale entry, so a live match never uses it.
        if scratch.generation == u64::MAX {
            scratch.scans.clear();
            scratch.generation = 0;
        }
        scratch.generation += 1;
        let Scratch {
            visited,
            deferred,
            scans,
            generation,
        } = scratch;
        deferred.clear();
        if scans.len() < tokens.len() {
            scans.resize(tokens.len(), StarScans::default());
        }
        let mut failed = FailedStates::new(tokens, path, visited);
        let (skip_token, skip_literal) =
            skipping_star(tokens, options).unwrap_or((usize::MAX, &[]));
        let mut work = StarWork {
            scans,
            generation: *generation,
            skip_token,
            skip_literal,
        };
        // Two instantiations: patterns that cannot skip keep a matcher loop
        // with none of the skipping machinery in it.
        if skip_token == usize::MAX {
            Self::matches_from::<false>(tokens, path, options, &mut failed, deferred, &mut work)
        } else {
            Self::matches_from::<true>(tokens, path, options, &mut failed, deferred, &mut work)
        }
    }

    /// Returns the input paths accepted by this compiled pattern, in input
    /// order. Wildcards after an explicit separator stay within that path
    /// component; recursive `**` is the separator-crossing form. The returned
    /// references borrow the caller-owned path list.
    ///
    /// Each path is asked through [`Self::is_match_path`], zlob's list rule,
    /// under which a wildcard in the first component crosses separators. For
    /// shell semantics, filter with [`Self::is_match_glob_path`] yourself:
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let paths = ["lib.rs", "src/lib.rs", "src/bin/main.rs", "README.md"];
    ///
    /// // A wildcard directly behind `/` stays in its component...
    /// let direct = Pattern::compile("src/*.rs", PatternOptions::walker())?;
    /// assert_eq!(direct.filter_paths(&paths), [&"src/lib.rs"]);
    ///
    /// // ...while one in the first component crosses separators.
    /// let top = Pattern::compile("*.rs", PatternOptions::walker())?;
    /// assert_eq!(
    ///     top.filter_paths(&paths),
    ///     [&"lib.rs", &"src/lib.rs", &"src/bin/main.rs"],
    /// );
    ///
    /// // Shell semantics: `*.rs` names the top level only.
    /// let shell: Vec<_> = paths
    ///     .iter()
    ///     .filter(|path| top.is_match_glob_path(path))
    ///     .collect();
    /// assert_eq!(shell, [&"lib.rs"]);
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    #[must_use]
    pub fn filter_paths<'a, T>(&self, paths: impl IntoIterator<Item = &'a T>) -> Vec<&'a T>
    where
        T: AsRef<[u8]> + ?Sized + 'a,
    {
        paths
            .into_iter()
            .filter(|path| self.is_match_path(path.as_ref()))
            .collect()
    }

    /// Matches one root-relative path with the zlob list-filter policy used by
    /// [`Pattern::filter_paths`]. A root wildcard may cross separators, while
    /// wildcards after an explicit separator are component-local. One leading
    /// `./` is ignored on both the pattern and candidate.
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let pattern = Pattern::compile("*.rs", PatternOptions::walker())?;
    ///
    /// assert!(pattern.is_match_path("lib.rs"));
    /// // A wildcard in the first component crosses the separator.
    /// assert!(pattern.is_match_path("src/lib.rs"));
    /// // One leading `./` is ignored.
    /// assert!(pattern.is_match_path("./lib.rs"));
    ///
    /// // A wildcard directly behind `/` stays in its component.
    /// let nested = Pattern::compile("src/*.rs", PatternOptions::walker())?;
    /// assert!(!nested.is_match_path("src/bin/main.rs"));
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    #[must_use]
    pub fn is_match_path(&self, path: impl AsRef<[u8]>) -> bool {
        self.matches_path_filter(path.as_ref())
    }

    /// Matches one root-relative filesystem-glob path. Every ordinary
    /// wildcard stays within its path component; recursive `**` remains the
    /// separator-crossing form. This is stricter than [`Pattern::is_match_path`]
    /// at the root component and is suitable for traversal filters. One
    /// leading `./` is ignored on both the pattern and candidate.
    ///
    /// This is how a shell and the `ferralk` walker read a pattern, and the
    /// entry point to use for a path relative to where the pattern is
    /// anchored. Pass a [`Path`](std::path::Path) as [`path_bytes`]`(path)`.
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let pattern = Pattern::compile("*.rs", PatternOptions::walker())?;
    ///
    /// assert!(pattern.is_match_glob_path("lib.rs"));
    /// // `*` stays in one component; write `**/*.rs` for every depth.
    /// assert!(!pattern.is_match_glob_path("src/lib.rs"));
    /// // One leading `./` is ignored.
    /// assert!(pattern.is_match_glob_path("./lib.rs"));
    ///
    /// let recursive = Pattern::compile("**/*.rs", PatternOptions::walker())?;
    /// assert!(recursive.is_match_glob_path("lib.rs"));
    /// assert!(recursive.is_match_glob_path("src/lib.rs"));
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    #[must_use]
    pub fn is_match_glob_path(&self, path: impl AsRef<[u8]>) -> bool {
        let path = without_leading_dot_slash(path.as_ref());
        let options = PatternOptions {
            component_wildcards: true,
            root_component_wildcards: true,
            ..self.options
        };
        if self
            .alternatives
            .iter()
            .any(|alternative| alternative.raw.starts_with(b"./"))
            && let Some(filter) = &self.path_filter
        {
            // The `./` alternatives need their stripped copies; every other
            // route answers under this policy exactly as below.
            return filter.is_match(&self.alternatives, options, options, path);
        }
        if let [alternative] = self.alternatives.as_slice()
            && (!options.extglob || alternative.extglob.is_none())
            && let Some(fast_path) = &alternative.fast_path
            && fast_path.supports_component_wildcards()
        {
            return fast_path.is_match(&alternative.tokens, path, options);
        }
        if let Some(fast_path) = &self.alternative_fast_path {
            return fast_path.is_match(path, options, &self.alternatives);
        }
        Self::match_alternatives(&self.alternatives, options, path)
    }

    /// Matches one root-relative path with the separator-crossing wildcards of
    /// [`Pattern::is_match`], ignoring one leading `./` on the candidate and on
    /// every brace-expanded alternative exactly as [`Pattern::is_match_path`]
    /// and [`Pattern::is_match_glob_path`] do.
    ///
    /// Hidden: this is the separator-crossing walk's reading of an include or
    /// exclude, which needs the path entry points' `./` rule without their
    /// component-local wildcards.
    #[doc(hidden)]
    #[must_use]
    pub fn is_match_crossing_path(&self, path: impl AsRef<[u8]>) -> bool {
        let path = without_leading_dot_slash(path.as_ref());
        // The copies exist whenever an alternative has the prefix, so asking
        // for them first spares most patterns the scan over alternatives.
        if let Some(filter) = &self.path_filter
            && self
                .alternatives
                .iter()
                .any(|alternative| alternative.raw.starts_with(b"./"))
        {
            // The path-filter copies differ from the alternatives only by the
            // removed prefix. Their fast paths are the literal and
            // fixed-width shapes, which read the same under either wildcard
            // reach, and every other engine takes the reach from `options`.
            // Every `./` alternative has a copy; the others answer from the
            // shared compile.
            return filter.is_match(&self.alternatives, self.options, self.options, path);
        }
        self.is_match(path)
    }

    /// Returns the input paths accepted relative to `base_path`, preserving
    /// the original full paths and caller order. Candidates outside the base
    /// directory are ignored.
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let pattern = Pattern::compile("src/*.rs", PatternOptions::walker())?;
    /// let paths = [
    ///     "crates/app/src/lib.rs",
    ///     "crates/app/README.md",
    ///     "crates/cli/src/main.rs",
    ///     "crates/application/src/lib.rs",
    /// ];
    ///
    /// // Each path is matched below the base and returned in full; a path
    /// // outside it, `crates/application` included, is left out.
    /// assert_eq!(
    ///     pattern.filter_paths_at("crates/app", &paths),
    ///     [&"crates/app/src/lib.rs"],
    /// );
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    #[must_use]
    pub fn filter_paths_at<'a, T>(
        &self,
        base_path: impl AsRef<[u8]>,
        paths: impl IntoIterator<Item = &'a T>,
    ) -> Vec<&'a T>
    where
        T: AsRef<[u8]> + ?Sized + 'a,
    {
        let base_path = base_path.as_ref();
        paths
            .into_iter()
            .filter(|path| {
                path_after_base(base_path, path.as_ref())
                    .is_some_and(|relative| self.is_match_path(relative))
            })
            .collect()
    }

    /// Returns the indices of input paths accepted by this compiled pattern,
    /// in their original input order.
    ///
    /// Useful when the paths live beside other data, such as a list of
    /// records, and the caller wants positions rather than borrowed paths.
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let pattern = Pattern::compile("src/**/*.rs", PatternOptions::walker())?;
    /// let files = ["README.md", "src/lib.rs", "src/bin/main.rs"].map(String::from);
    ///
    /// assert_eq!(pattern.filter_path_indices(&files), [1, 2]);
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    #[must_use]
    pub fn filter_path_indices<'a, T>(&self, paths: impl IntoIterator<Item = &'a T>) -> Vec<usize>
    where
        T: AsRef<[u8]> + ?Sized + 'a,
    {
        paths
            .into_iter()
            .enumerate()
            .filter_map(|(index, path)| self.is_match_path(path.as_ref()).then_some(index))
            .collect()
    }

    /// Returns the indices of full input paths accepted relative to
    /// `base_path`, in their original input order.
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions};
    ///
    /// let pattern = Pattern::compile("*.toml", PatternOptions::walker())?;
    /// let paths = ["crates/app/Cargo.toml", "Cargo.toml", "crates/app/src/x.toml"];
    ///
    /// // `Cargo.toml` is outside the base. `src/x.toml` is selected because a
    /// // wildcard in the first component crosses separators under the list
    /// // rule of `filter_paths`.
    /// assert_eq!(pattern.filter_path_indices_at("crates/app", &paths), [0, 2]);
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    #[must_use]
    pub fn filter_path_indices_at<'a, T>(
        &self,
        base_path: impl AsRef<[u8]>,
        paths: impl IntoIterator<Item = &'a T>,
    ) -> Vec<usize>
    where
        T: AsRef<[u8]> + ?Sized + 'a,
    {
        let base_path = base_path.as_ref();
        paths
            .into_iter()
            .enumerate()
            .filter_map(|(index, path)| {
                path_after_base(base_path, path.as_ref())
                    .is_some_and(|relative| self.is_match_path(relative))
                    .then_some(index)
            })
            .collect()
    }

    fn matches_path_filter(&self, path: &[u8]) -> bool {
        let path = without_leading_dot_slash(path);
        let Some(filter) = &self.path_filter else {
            return self.is_match(path);
        };
        let component = PatternOptions {
            component_wildcards: true,
            ..self.options
        };
        filter.is_match(&self.alternatives, component, self.options, path)
    }

    fn from_alternatives(
        alternatives: Vec<CompiledAlternative>,
        options: PatternOptions,
        budget: &mut IrBudget,
        defer_sweeps: bool,
    ) -> Result<Self, PatternError> {
        let alternative_fast_path =
            AlternativeFastPath::compile(&alternatives, options, budget)?.map(Box::new);
        let path_filter =
            PathFilter::compile(&alternatives, options, budget, defer_sweeps)?.map(Box::new);
        Ok(Self {
            alternatives,
            alternative_fast_path,
            path_filter,
            walker_path_viability: WalkerPathViability::Viable,
            walker_path_problem_offset: None,
            options,
        })
    }

    fn populate_deferred_sweeps(&mut self, budget: &mut IrBudget) -> Result<(), PatternError> {
        let parse_options = PatternOptions {
            braces: false,
            ..self.options
        };
        // Pass 1: build sweeps for top-level alternatives first, skipping
        // `FastPath` arms that already answer every entry point without
        // deducting their unused tables from `budget` (#495).
        for alternative in &mut self.alternatives {
            alternative.sweep = compile_sweep(
                &alternative.tokens,
                &alternative.fast_path,
                alternative.extglob.as_deref(),
                parse_options,
                budget,
                None,
                false,
                true,
            )?;
        }
        // Pass 2: populate `PathFilter` copies, sharing `Arc`-backed tables
        // for unchanged tokens at zero budget cost before spending any
        // remaining budget on copies that need their own engine.
        let Some(filter) = self.path_filter.as_deref_mut() else {
            return Ok(());
        };
        if filter.compiled.is_empty() {
            return Ok(());
        }
        let mut originals = self
            .alternatives
            .iter()
            .filter(|alternative| !PathFilter::is_program(alternative, self.options));
        for copy in &mut filter.compiled {
            let original = originals
                .next()
                .expect("every path-filter copy corresponds to one non-program alternative");
            let reuses_original_tokens = !has_leading_dot_slash_prefix(original);
            let built = original.sweep.as_ref().filter(|_| reuses_original_tokens);
            copy.sweep = if let Some(built) = built {
                Some(built.clone())
            } else if copy
                .fast_path
                .as_ref()
                .is_some_and(FastPath::supports_component_wildcards)
                || (reuses_original_tokens && !sweep_is_deferred(original))
            {
                // Either `copy` already answers every entry point with its
                // complete `FastPath` (never allocating a sweep), or `original`
                // has the same tokens and already needs no sweep / fell back.
                None
            } else {
                compile_sweep(
                    &copy.tokens,
                    &copy.fast_path,
                    copy.extglob.as_deref(),
                    parse_options,
                    budget,
                    None,
                    false,
                    true,
                )?
            };
        }
        debug_assert!(originals.next().is_none());
        Ok(())
    }

    fn compile_path_filter_alternative(
        alternative: &CompiledAlternative,
        options: PatternOptions,
        budget: &mut IrBudget,
        defer_sweeps: bool,
    ) -> Result<CompiledAlternative, PatternError> {
        let leading_dot_slash = has_leading_dot_slash_prefix(alternative);
        let source_raw = if leading_dot_slash {
            &alternative.raw[2..]
        } else {
            alternative.raw.as_slice()
        };
        let source_tokens = if leading_dot_slash {
            &alternative.tokens[2..]
        } else {
            &alternative.tokens
        };
        budget.charge(source_tokens.len(), 0)?;
        if defer_sweeps {
            ensure_retained_alternative_bytes(source_raw.len(), source_tokens, budget)?;
        }
        let raw = source_raw.to_vec();
        let tokens = source_tokens.to_vec();
        let fast_path = if !leading_dot_slash {
            alternative
                .fast_path
                .as_ref()
                .filter(|fast_path| fast_path.supports_component_wildcards())
                .cloned()
        } else {
            FastPath::compile(
                &tokens,
                PatternOptions {
                    component_wildcards: false,
                    ..options
                },
            )
            .filter(FastPath::supports_component_wildcards)
        };
        let prefilter = Prefilter::compile(&tokens);
        if defer_sweeps {
            charge_retained_alternative_bytes(raw.len(), &tokens, &fast_path, &prefilter, budget)?;
        }
        let mut provenance_budget = ProvenanceBudget::new();
        let extglob = compile_extglob(&raw, options, budget, &mut provenance_budget, None, false)?;
        let sweep = if defer_sweeps {
            None
        } else {
            // Without a removed `./` the copy has its original's tokens, and
            // the same tokens under the same options build the same engine.
            let built = alternative.sweep.as_ref().filter(|_| !leading_dot_slash);
            compile_sweep(
                &tokens,
                &fast_path,
                extglob.as_deref(),
                options,
                budget,
                built,
                !leading_dot_slash,
                false,
            )?
        };
        Ok(CompiledAlternative {
            extglob,
            raw,
            fast_path,
            prefilter,
            sweep,
            tokens,
            walker_path_shape: alternative.walker_path_shape.clone(),
        })
    }

    /// Explores the token/path state graph without native recursion.
    ///
    /// A star is the only token with two successors, and it is the one that
    /// used to recurse once per consumed path byte, so a long candidate
    /// overflowed the native stack. Its repetition branch is now deferred to an
    /// explicit work list while the "stop consuming here" branch continues in
    /// place. Depth-first order is unchanged, because a deferred entry is
    /// popped exactly when the branch that was taken instead is exhausted.
    ///
    /// The work list stays small: a star's repetition branch keeps its token
    /// index while its other successor advances it, so the deferred entries
    /// hold strictly increasing token indices and never exceed one per token.
    /// `FailedStates` still bounds the explored state space to
    /// `tokens × path` visits.
    fn matches_from<const SKIP: bool>(
        tokens: &[Token],
        path: &[u8],
        options: PatternOptions,
        failed: &mut FailedStates<'_>,
        deferred: &mut Vec<(usize, usize)>,
        work: &mut StarWork<'_>,
    ) -> bool {
        let mut state = (0_usize, 0_usize);

        loop {
            let (token_index, path_index) = state;
            let advanced = if token_index == tokens.len() {
                if path_index == path.len() {
                    return true;
                }
                None
            } else if !failed.insert(token_index, path_index) {
                None
            } else {
                match &tokens[token_index] {
                    Token::Literal(literal) => {
                        Self::advance_literal(token_index, path_index, path, options, literal)
                    }
                    Token::Separator => {
                        if path.get(path_index).is_some_and(|byte| is_separator(*byte)) {
                            Some((token_index + 1, path_index + 1))
                        } else if path_index == path.len()
                            && token_index + 2 == tokens.len()
                            && matches!(tokens.get(token_index + 1), Some(Token::RecursiveStar))
                        {
                            return true;
                        } else {
                            None
                        }
                    }
                    Token::Any => {
                        Self::advance_one(tokens, token_index, path_index, path, options, |_| true)
                    }
                    Token::Class(class) => {
                        Self::advance_one(tokens, token_index, path_index, path, options, |byte| {
                            class.matches(byte, options.case_insensitive)
                        })
                    }
                    Token::Star => Self::advance_star::<SKIP>(
                        token_index,
                        path_index,
                        path,
                        options,
                        StarSemantics::ordinary(!Self::component_wildcard(
                            tokens,
                            token_index,
                            options,
                        ))
                        .handing_over_to(tokens.get(token_index + 1)),
                        deferred,
                        work,
                    ),
                    Token::RecursiveStar => Self::advance_star::<SKIP>(
                        token_index,
                        path_index,
                        path,
                        options,
                        StarSemantics::ordinary(true).handing_over_to(tokens.get(token_index + 1)),
                        deferred,
                        work,
                    ),
                    Token::RecursivePrefix => Self::advance_star::<SKIP>(
                        token_index,
                        path_index,
                        path,
                        options,
                        StarSemantics::RECURSIVE_PREFIX,
                        deferred,
                        work,
                    ),
                }
            };

            debug_assert!(
                deferred.len() <= tokens.len(),
                "the work list holds at most one deferred repetition per token"
            );
            match advanced.or_else(|| deferred.pop()) {
                Some(next) => state = next,
                None => return false,
            }
        }
    }

    fn component_wildcard(tokens: &[Token], token_index: usize, options: PatternOptions) -> bool {
        options.component_wildcards
            && (options.root_component_wildcards
                || token_index == 0 && options.candidate_root_component_wildcard
                || token_index > 0 && matches!(tokens[token_index - 1], Token::Separator))
    }

    /// Returns the state after a literal token, or `None` if it does not match.
    fn advance_literal(
        token_index: usize,
        path_index: usize,
        path: &[u8],
        options: PatternOptions,
        literal: &[u8],
    ) -> Option<(usize, usize)> {
        let candidate = path.get(path_index..path_index + literal.len())?;
        literal
            .iter()
            .zip(candidate)
            .all(|(&expected, &actual)| bytes_equal(expected, actual, options.case_insensitive))
            .then_some((token_index + 1, path_index + literal.len()))
    }

    /// Returns the state after a single-byte token, or `None` if the candidate
    /// byte is rejected by the token, the component policy, or the leading-dot
    /// policy.
    fn advance_one(
        tokens: &[Token],
        token_index: usize,
        path_index: usize,
        path: &[u8],
        options: PatternOptions,
        accepts: impl FnOnce(u8) -> bool,
    ) -> Option<(usize, usize)> {
        let &byte = path.get(path_index)?;
        (accepts(byte)
            && (!Self::component_wildcard(tokens, token_index, options) || !is_separator(byte))
            && (options.match_hidden
                || byte != b'.'
                || !at_component_start(path, path_index, options)))
        .then_some((token_index + 1, path_index + 1))
    }

    /// Queues the star's repetition branch and returns the branch that stops
    /// consuming here, which the caller explores first. An ordinary star may
    /// not stop immediately before a component-leading period: doing so would
    /// let a following literal opt into a hidden name implicitly. The
    /// syntactic `**/` prefix is exempt because it explicitly advances to the
    /// next component without consuming one; in exchange it may stop only at
    /// a component start.
    fn advance_star<const SKIP: bool>(
        token_index: usize,
        path_index: usize,
        path: &[u8],
        options: PatternOptions,
        semantics: StarSemantics,
        deferred: &mut Vec<(usize, usize)>,
        work: &mut StarWork<'_>,
    ) -> Option<(usize, usize)> {
        if let Some(next) = Self::next_star_position::<SKIP>(
            token_index,
            path_index,
            path,
            options,
            semantics.recursive,
            work,
        ) {
            deferred.push((token_index, next));
        }
        let hidden_stop_allowed = !semantics.blocks_hidden_stop
            || options.match_hidden
            || path.get(path_index) != Some(&b'.')
            || !at_component_start(path, path_index, options);
        let component_stop_allowed =
            !semantics.stops_at_component_start || at_component_start(path, path_index, options);
        (hidden_stop_allowed && component_stop_allowed).then_some((token_index + 1, path_index))
    }

    /// Where the star should resume consuming, or `None` when it cannot.
    ///
    /// A star followed by a literal only has to stop where that literal could
    /// begin, so the repetition jumps straight there instead of walking one
    /// byte at a time (ADR-0008). Positions in between can only fail: their
    /// sole other successor is the literal, which does not start there.
    ///
    /// The jump never crosses a byte the star may not consume, because it is
    /// clamped to [`star_barrier`]. Both the barrier and the literal search are
    /// monotone in `path_index` and cached as such, so a star re-entered at
    /// many positions still scans each byte a bounded number of times.
    ///
    /// Case-insensitive matching keeps the byte-wise walk: `memmem` has no
    /// folded form, and folding the candidate would mean allocating.
    #[inline]
    fn next_star_position<const SKIP: bool>(
        token_index: usize,
        path_index: usize,
        path: &[u8],
        options: PatternOptions,
        recursive: bool,
        work: &mut StarWork<'_>,
    ) -> Option<usize> {
        if SKIP && work.skip_token == token_index {
            return Self::skip_to_literal(token_index, path_index, path, options, recursive, work);
        }

        let &byte = path.get(path_index)?;
        star_consumes_byte(path, path_index, byte, options, recursive).then_some(path_index + 1)
    }

    /// The skipping half of [`Self::next_star_position`], kept out of line so
    /// the byte-wise walk stays a handful of instructions in the matcher loop.
    #[inline(never)]
    fn skip_to_literal(
        token_index: usize,
        path_index: usize,
        path: &[u8],
        options: PatternOptions,
        recursive: bool,
        work: &mut StarWork<'_>,
    ) -> Option<usize> {
        let literal = work.skip_literal;
        let scans = work.scans(token_index);
        if scans.stalled >= STALLED_SKIPS {
            let &byte = path.get(path_index)?;
            return star_consumes_byte(path, path_index, byte, options, recursive)
                .then_some(path_index + 1);
        }
        if let Some(cached) = scans.skip.get(path_index) {
            return cached;
        }
        let barrier = star_barrier(path, path_index, options, recursive, scans);
        let start = path_index + 1;
        if start > barrier {
            return scans.skip.record(path_index, path_index, None);
        }
        match next_literal_start(path, start, literal, &mut scans.literal) {
            // Nothing left to aim for, whatever the star consumes.
            None => scans.skip.record(path_index, usize::MAX, None),
            // The occurrence is out of reach, and stays out of reach for every
            // position up to the barrier.
            Some(found) if found > barrier => scans.skip.record(path_index, barrier, None),
            Some(found) => {
                scans.stalled = if found == start {
                    scans.stalled.saturating_add(1)
                } else {
                    0
                };
                scans.skip.record(path_index, found - 1, Some(found))
            }
        }
    }
}

/// Visited token/path pairs for one matcher invocation.
///
/// The state space is dense by construction: token and path indices are both
/// bounded by the input slices. Small matrices live directly in a matcher
/// frame; larger inputs retain the flat heap matrix. Both avoid hashing while
/// keeping the original backtracking semantics.
///
/// A visited pair is never cleared. The recursive matcher used to clear one on
/// the way out of a successful frame, which was unobservable: a match returned
/// straight through every caller, and each invocation builds its own matrix.
enum FailedStates<'scratch> {
    Inline {
        width: usize,
        states: u128,
    },
    Heap {
        width: usize,
        states: &'scratch mut [u64],
    },
}

impl<'scratch> FailedStates<'scratch> {
    fn new(tokens: &[Token], path: &[u8], scratch: &'scratch mut Vec<u64>) -> Self {
        let width = path.len() + 1;
        let state_count = tokens.len().saturating_mul(width);
        if state_count <= u128::BITS as usize {
            return Self::Inline { width, states: 0 };
        }
        let words = state_count.div_ceil(u64::BITS as usize);
        if scratch.len() < words {
            scratch.resize(words, 0);
        }
        let states = &mut scratch[..words];
        states.fill(0);
        Self::Heap { width, states }
    }

    fn insert(&mut self, token_index: usize, path_index: usize) -> bool {
        match self {
            Self::Inline { width, states } => {
                let mask = 1_u128 << (token_index * *width + path_index);
                if *states & mask != 0 {
                    false
                } else {
                    *states |= mask;
                    true
                }
            }
            Self::Heap { width, states } => {
                let state = token_index * *width + path_index;
                let word = &mut states[state / u64::BITS as usize];
                let mask = 1_u64 << (state % u64::BITS as usize);
                if *word & mask != 0 {
                    false
                } else {
                    *word |= mask;
                    true
                }
            }
        }
    }
}

/// Reusable matcher buffers for one thread.
///
/// Every field is sized by the candidate or the pattern, so building them per
/// call was the dominant cost of a walker filter. They are cleared, never
/// reallocated, once a thread has seen its first long candidate.
#[derive(Default)]
struct Scratch {
    visited: Vec<u64>,
    deferred: Vec<(usize, usize)>,
    scans: Vec<StarScans>,
    generation: u64,
}

/// The literal-skipping bookkeeping for one match, borrowed out of [`Scratch`].
struct StarWork<'scratch> {
    /// Cached scans, one entry per token. Entries from an earlier match are
    /// recognized by their generation and reset when first used, so starting a
    /// match costs nothing per token.
    scans: &'scratch mut [StarScans],
    generation: u64,
    /// Token index of the one star that skips, or `usize::MAX` for none.
    /// Resolved once per match so the matcher loop only loads and compares a
    /// single word per star state. See [`skipping_star`].
    skip_token: usize,
    /// The literal that star aims at.
    skip_literal: &'scratch [u8],
}

#[derive(Clone, Copy)]
struct StarSemantics {
    recursive: bool,
    blocks_hidden_stop: bool,
    /// The star may stop only where a path component starts. A `**/` prefix
    /// consumes whole components with their separators, so handing over in
    /// the middle of one would let `**/x` match `sx` (#419).
    stops_at_component_start: bool,
}

impl StarSemantics {
    const RECURSIVE_PREFIX: Self = Self {
        recursive: true,
        blocks_hidden_stop: false,
        stops_at_component_start: true,
    };

    const fn ordinary(recursive: bool) -> Self {
        Self {
            recursive,
            blocks_hidden_stop: true,
            stops_at_component_start: false,
        }
    }

    /// The same star in front of `next`, the token it hands over to.
    ///
    /// A zero-width stop before a leading period is harmful only when a
    /// byte-matching token takes the period next. A star behind this one
    /// cannot consume the period and applies this rule to its own stop, so
    /// only the last star of a run decides. That makes a run that ends in a
    /// `**/` prefix exempt, as the prefix is: `****/.h` matches `.h` as
    /// `**/.h` does. The sweep engine's `dot_stop_block` blocks exactly the
    /// boundary behind each ordinary star, which is the same rule (#442).
    const fn handing_over_to(mut self, next: Option<&Token>) -> Self {
        if matches!(
            next,
            Some(Token::Star | Token::RecursiveStar | Token::RecursivePrefix)
        ) {
            self.blocks_hidden_stop = false;
        }
        self
    }
}

impl StarWork<'_> {
    fn scans(&mut self, token_index: usize) -> &mut StarScans {
        let scans = &mut self.scans[token_index];
        if scans.generation != self.generation {
            *scans = StarScans {
                generation: self.generation,
                ..StarScans::default()
            };
        }
        scans
    }
}

/// Visited-matrix words a thread keeps between calls. 8 Ki words is 64 KiB and
/// holds 512 Ki token/path states — past any realistic path — while stopping
/// one huge candidate from pinning memory on a worker thread for good.
/// Maximum retained wide extglob repetition sweep slots kept across calls on
/// one thread so a single huge wide-arm repetition never leaves thousands of
/// heap-backed wide states behind.
const RETAINED_WIDE_SWEEP_SLOTS: usize = 64;

const RETAINED_SCRATCH_WORDS: usize = 8 * 1024;

thread_local! {
    static SCRATCH: RefCell<Scratch> = const {
        RefCell::new(Scratch {
            visited: Vec::new(),
            deferred: Vec::new(),
            scans: Vec::new(),
            generation: 0,
        })
    };
}

/// Capacities the calling thread's scratch currently holds.
///
/// Used by the steady-state test to show that repeated matches reuse the
/// buffers instead of reallocating them.
#[cfg(test)]
fn scratch_capacities() -> (usize, usize, usize) {
    SCRATCH.with(|cell| {
        let scratch = cell.borrow();
        (
            scratch.visited.capacity(),
            scratch.deferred.capacity(),
            scratch.scans.capacity(),
        )
    })
}

/// The forward scans one star token needs, each cached on its own.
///
/// Every entry is private to a token, so the spans a token rescans are
/// disjoint and its total scanning stays linear in the candidate however often
/// an enclosing `**` re-enters it. `skip` caches the combined answer, which is
/// what an enclosing `**` hits on almost every position.
///
/// `generation` is the match that filled the entry; anything older is stale.
#[derive(Clone, Copy, Default)]
struct StarScans {
    generation: u64,
    skip: SkipDecision,
    separator: FirstAtOrAfter,
    component_dot: FirstAtOrAfter,
    literal: FirstAtOrAfter,
    /// Consecutive jumps that landed on the very next byte; see
    /// [`STALLED_SKIPS`].
    stalled: u8,
}

/// How many single-byte jumps in a row turn skipping off for a token.
///
/// A candidate dense in the literal — `a*a*a*b` over a run of `a`s — has an
/// occurrence at every position, so the search finds only the next byte and the
/// scan is pure overhead on top of the step it replaces. Falling back to the
/// byte-wise walk there costs nothing in correctness: both answers name the
/// same state, and the walk is what the visited matrix shares anyway.
const STALLED_SKIPS: u8 = 4;

/// Cached resume decision for one star token, valid for `from..=upto`.
///
/// Both inputs to the decision — the consumption barrier and the next literal
/// start — are monotone in the path index, so one answer covers the whole span
/// up to the position that changes it.
#[derive(Clone, Copy)]
struct SkipDecision {
    from: usize,
    upto: usize,
    /// Where the repetition resumes, or `usize::MAX` for "it cannot".
    jump: usize,
}

impl SkipDecision {
    fn get(self, index: usize) -> Option<Option<usize>> {
        (self.from <= index && index <= self.upto)
            .then(|| (self.jump != usize::MAX).then_some(self.jump))
    }

    fn record(&mut self, from: usize, upto: usize, jump: Option<usize>) -> Option<usize> {
        *self = Self {
            from,
            upto,
            jump: jump.unwrap_or(usize::MAX),
        };
        jump
    }
}

impl Default for SkipDecision {
    fn default() -> Self {
        Self {
            from: usize::MAX,
            upto: 0,
            jump: usize::MAX,
        }
    }
}

/// Cached answer of a monotone "first index at or after `from`" question.
///
/// The answer for `from` is also the answer for every index up to it, so one
/// entry serves the whole span it covers and a later miss starts beyond it.
/// Successive misses therefore scan disjoint parts of the candidate.
#[derive(Clone, Copy)]
struct FirstAtOrAfter {
    from: usize,
    answer: usize,
}

impl FirstAtOrAfter {
    /// A cache that answers nothing.
    const EMPTY: Self = Self {
        from: usize::MAX,
        answer: 0,
    };

    fn get(self, index: usize) -> Option<usize> {
        (self.from <= index && index <= self.answer).then_some(self.answer)
    }

    fn record(&mut self, index: usize, answer: usize) -> usize {
        *self = Self {
            from: index,
            answer,
        };
        answer
    }
}

impl Default for FirstAtOrAfter {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// The star whose repetition is worth skipping, and the literal it aims at.
///
/// A star is entered once per position its prefix can end at. Only the first
/// star has a fixed-width prefix; every later one is re-entered at as many
/// positions as the earlier stars can reach, and there the byte-wise steps are
/// already shared through the visited matrix — a scan per entry would cost more
/// than the state visits it removes. So skipping is applied to the first star
/// only, where one scan replaces a walk over the whole candidate.
///
/// Case folding keeps the byte-wise walk everywhere: `memmem` has no folded
/// form, and folding the candidate would mean allocating.
fn skipping_star(tokens: &[Token], options: PatternOptions) -> Option<(usize, &[u8])> {
    if options.case_insensitive {
        return None;
    }
    let index = tokens.iter().position(|token| {
        matches!(
            token,
            Token::Star | Token::RecursiveStar | Token::RecursivePrefix
        )
    })?;
    match tokens.get(index + 1) {
        Some(Token::Literal(literal)) if !literal.is_empty() => Some((index, literal)),
        _ => None,
    }
}

/// Whether a star may consume the byte at `path_index`.
fn star_consumes_byte(
    path: &[u8],
    path_index: usize,
    byte: u8,
    options: PatternOptions,
    recursive: bool,
) -> bool {
    (recursive || !options.component_wildcards || !is_separator(byte))
        && (options.match_hidden || byte != b'.' || !at_component_start(path, path_index, options))
}

/// First index at or after `path_index` that a star may not consume.
///
/// A component-local star stops at the next separator; unless hidden entries
/// match, every star stops at the leading dot of a component. This is the span
/// form of [`star_consumes_byte`], answered with `memchr`/`memmem` so a skipped
/// run costs one vectorised pass instead of one state visit per byte.
fn star_barrier(
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    recursive: bool,
    scans: &mut StarScans,
) -> usize {
    let mut barrier = path.len();
    if !recursive && options.component_wildcards {
        barrier = barrier.min(next_separator_from(path, path_index, &mut scans.separator));
    }
    if !options.match_hidden {
        barrier = barrier.min(next_component_dot(
            path,
            path_index,
            options.candidate_starts_component,
            &mut scans.component_dot,
        ));
    }
    barrier
}

/// First index at or after `index` where `literal` starts.
fn next_literal_start(
    path: &[u8],
    index: usize,
    literal: &[u8],
    cache: &mut FirstAtOrAfter,
) -> Option<usize> {
    let found = match cache.get(index) {
        Some(found) => found,
        None => {
            let found = path
                .get(index..)
                .and_then(|tail| find_literal(tail, literal))
                .map_or(usize::MAX, |offset| index + offset);
            cache.record(index, found)
        }
    };
    (found != usize::MAX).then_some(found)
}

/// Offset of `needle` in `haystack`.
///
/// `memmem` builds a searcher per call, which costs more than the whole scan
/// for the short candidates a walker filter sees. Below the floor a plain
/// `memchr` on the first byte plus a tail comparison is setup-free; above it
/// the vectorised two-byte search wins.
fn find_literal(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    const MEMMEM_HAYSTACK_FLOOR: usize = 256;
    const SCALAR_HAYSTACK_CEILING: usize = 32;
    let (&first, rest) = needle.split_first()?;
    if haystack.len() >= MEMMEM_HAYSTACK_FLOOR {
        return memmem::find(haystack, needle);
    }
    if haystack.len() <= SCALAR_HAYSTACK_CEILING {
        // A path component is shorter than one SIMD block, so even `memchr`'s
        // entry sequence outweighs the comparisons it saves.
        return (0..haystack.len())
            .find(|&start| haystack[start] == first && haystack[start + 1..].starts_with(rest));
    }
    let mut offset = 0;
    while let Some(hit) = memchr(first, &haystack[offset..]) {
        let start = offset + hit;
        if haystack[start + 1..].starts_with(rest) {
            return Some(start);
        }
        offset = start + 1;
    }
    None
}

/// First separator at or after `index`, or the candidate's length.
fn next_separator_from(path: &[u8], index: usize, cache: &mut FirstAtOrAfter) -> usize {
    if let Some(found) = cache.get(index) {
        return found;
    }
    let found = path
        .get(index..)
        .and_then(next_separator)
        .map_or(path.len(), |offset| index + offset);
    cache.record(index, found)
}

/// Offset of the first path separator in `bytes`, using the platform's set.
fn next_separator(bytes: &[u8]) -> Option<usize> {
    if cfg!(windows) {
        memchr2(b'/', b'\\', bytes)
    } else {
        memchr(b'/', bytes)
    }
}

/// First index at or after `index` that starts a component with a dot, or the
/// candidate's length.
///
/// Past index 0 a component starts exactly after a separator, so the blocked
/// positions are the dots that directly follow one.
fn next_component_dot(
    path: &[u8],
    index: usize,
    candidate_starts_component: bool,
    cache: &mut FirstAtOrAfter,
) -> usize {
    if let Some(found) = cache.get(index) {
        return found;
    }
    if index == 0 && candidate_starts_component && path.first() == Some(&b'.') {
        return cache.record(0, 0);
    }
    let start = index.saturating_sub(1);
    let found = match path.get(start..) {
        Some(window) => {
            let mut offset = 0;
            loop {
                let Some(hit) = next_separator(&window[offset..]) else {
                    break path.len();
                };
                let at = offset + hit;
                if window.get(at + 1) == Some(&b'.') {
                    break start + at + 1;
                }
                offset = at + 1;
            }
        }
        None => path.len(),
    };
    cache.record(index, found)
}

/// Crate version exposed for build and integration diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(Vec<u8>),
    Separator,
    Any,
    Star,
    /// A whole-component `**` at the end of its pattern (see
    /// [`star_run_is_whole_component`]); a star that also crosses separators.
    RecursiveStar,
    /// A whole-component `**/`: zero or more whole components, each with its
    /// separator. It may only hand over at a component start.
    RecursivePrefix,
    Class(Class),
}

/// Whether the two ends of a compiled pattern sit on path-component
/// boundaries of the pattern it came from.
///
/// A top-level pattern — and each brace alternative, which is a whole pattern
/// once expanded — is bounded on both sides. An extglob alternative inherits
/// its group's position: its start is a component start only when the group
/// operator is, and its end a component end only when the group's closing
/// parenthesis is. That is what makes `@(**)/y` recursive and `x@(**)/y` an
/// ordinary star run, exactly as `**/y` and `x**/y` are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ComponentBounds {
    starts: bool,
    ends: bool,
}

impl ComponentBounds {
    const WHOLE: Self = Self {
        starts: true,
        ends: true,
    };
}

/// Whether the star run `pattern[start..end]` forms a whole path component
/// (#419, ADR-0020).
///
/// Only such a run may be recursive: on each side it must meet an unescaped
/// `/` or an end of the pattern that `bounds` marks as a component boundary.
/// An escaped `/` is a literal byte, not a pattern separator, so it bounds
/// nothing. Every other run is an ordinary star run, read exactly as it is
/// with `recursive_double_star` disabled.
fn star_run_is_whole_component(
    pattern: &[u8],
    start: usize,
    end: usize,
    escapes: bool,
    bounds: ComponentBounds,
) -> bool {
    starts_component_at(pattern, start, escapes, bounds) && ends_component_at(pattern, end, bounds)
}

/// Whether offset `start` of `pattern` begins a path component: it follows an
/// unescaped `/`, or it is the pattern's start and `bounds` says that is one.
fn starts_component_at(
    pattern: &[u8],
    start: usize,
    escapes: bool,
    bounds: ComponentBounds,
) -> bool {
    let Some(before) = start.checked_sub(1) else {
        return bounds.starts;
    };
    let escaped = escapes
        && pattern[..before]
            .iter()
            .rev()
            .take_while(|&&byte| byte == b'\\')
            .count()
            % 2
            == 1;
    pattern[before] == b'/' && !escaped
}

/// Whether offset `end` of `pattern` ends a path component: a `/` follows, or
/// it is the pattern's end and `bounds` says that is one.
fn ends_component_at(pattern: &[u8], end: usize, bounds: ComponentBounds) -> bool {
    pattern.get(end).map_or(bounds.ends, |&byte| byte == b'/')
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Immutable compiled representation of one syntax alternative.
///
/// It is constructed only during [`Pattern::compile`]. Matching borrows it;
/// per-call memoization remains outside this IR in [`FailedStates`].
struct CompiledAlternative {
    raw: Vec<u8>,
    fast_path: Option<FastPath>,
    tokens: Vec<Token>,
    /// The compiled extglob program, present exactly when this alternative
    /// carries extglob syntax and the option is on. Matching borrows it; the
    /// interpreter used to re-derive all of it from `raw` on every call.
    extglob: Option<Box<CompiledExtglob>>,
    /// Necessary conditions on a candidate, read off `tokens` at compile time
    /// and checked before the general engine runs. See [`Prefilter`].
    prefilter: Prefilter,
    /// Walker-only path shape recorded while this alternative's tokens were
    /// compiled. It retains distinctions (such as escaped dots) that matcher
    /// tokens intentionally erase, and can be appended to an extglob's outer
    /// component without reparsing source syntax.
    walker_path_shape: WalkerPathShape,
    /// The bit-parallel engine for the general path, present when
    /// [`compile_sweep`] found the tokens suitable. It answers exactly like
    /// the memoized matcher and replaces it in the dispatch, never a fast
    /// path and never an extglob program.
    sweep: Option<SweepEngine>,
}

impl CompiledAlternative {
    /// See [`Pattern::release_compile_state`]. Descends into extglob arms,
    /// whose shapes the analysis read as well.
    fn release_compile_state(&mut self) {
        self.walker_path_shape = WalkerPathShape::default();
        if let Some(program) = &mut self.extglob {
            program.walker_source_provenance = None;
            for group in &mut program.groups {
                for alternative in group
                    .alternatives
                    .iter_mut()
                    .chain(group.through_separator.iter_mut())
                {
                    for nested in &mut alternative.compiled {
                        nested.release_compile_state();
                    }
                }
            }
        }
    }

    /// Removes accelerated engines, descending into extglob alternatives so a
    /// differential run pins one engine for the sub-matches too.
    #[cfg(any(test, feature = "unstable-test-hooks"))]
    fn strip_engines(
        &mut self,
        options: PatternOptions,
        fast_paths: bool,
        sweeps: bool,
        prefilters: bool,
    ) {
        if fast_paths {
            // With its fast path gone the engine the compile deferred is what
            // answers, as it did when every such alternative carried one.
            if !sweeps && sweep_is_deferred(self) {
                self.sweep = SweepEngine::build(&self.tokens, options).ok();
            }
            self.fast_path = None;
        }
        if sweeps {
            self.sweep = None;
        }
        if prefilters {
            self.prefilter = Prefilter::default();
        }
        if let Some(program) = &mut self.extglob {
            for group in &mut program.groups {
                for alternative in group
                    .alternatives
                    .iter_mut()
                    .chain(group.through_separator.iter_mut())
                {
                    for nested in &mut alternative.compiled {
                        nested.strip_engines(options, fast_paths, sweeps, prefilters);
                    }
                }
            }
        }
    }
}

/// Owned heap bytes held by a `Vec<Token>`, including the `Vec` backing
/// array and any owned literal or character-class member buffers.
fn cloned_tokens_retained_bytes(tokens: &[Token]) -> usize {
    tokens
        .len()
        .saturating_mul(size_of::<Token>())
        .saturating_add(
            tokens
                .iter()
                .map(|token| match token {
                    Token::Literal(literal) => literal.len(),
                    Token::Class(class) => {
                        class.members.len().saturating_mul(size_of::<ClassMember>())
                    }
                    _ => 0,
                })
                .sum::<usize>(),
        )
}

fn ensure_retained_alternative_bytes(
    raw_len: usize,
    tokens: &[Token],
    budget: &IrBudget,
) -> Result<(), PatternError> {
    let bytes = size_of::<CompiledAlternative>()
        .saturating_add(raw_len)
        .saturating_add(cloned_tokens_retained_bytes(tokens));
    budget.ensure_retained_bytes(bytes, 0)
}

/// Charges owned heap storage retained by one multi-alternative brace arm
/// (`CompiledAlternative`, `raw`, `tokens`, `fast_path`, and `prefilter`)
/// against the dedicated [`MAX_RETAINED_PATTERN_BYTES`] (40 MiB) ceiling so
/// retained memory is strictly bounded without stealing [`IrBudget`] units
/// from sweep-backed alternatives (#495).
fn charge_retained_alternative_bytes(
    raw_len: usize,
    tokens: &[Token],
    fast_path: &Option<FastPath>,
    prefilter: &Prefilter,
    budget: &mut IrBudget,
) -> Result<(), PatternError> {
    let bytes = size_of::<CompiledAlternative>()
        .saturating_add(raw_len)
        .saturating_add(cloned_tokens_retained_bytes(tokens))
        .saturating_add(fast_path.as_ref().map_or(0, FastPath::retained_bytes))
        .saturating_add(prefilter.retained_bytes());
    budget.charge_retained_bytes(bytes, 0)
}

fn has_leading_dot_slash_prefix(alternative: &CompiledAlternative) -> bool {
    alternative.raw.starts_with(b"./")
        && matches!(alternative.tokens.as_slice(), [Token::Literal(dot), Token::Separator, ..] if dot == b".")
}

/// Compiles the Shift-And engine where the alternative would use it.
///
/// An extglob program keeps its own interpreter, and the literal and
/// deterministic fast paths win the dispatch under every option profile, so
/// building an engine behind either would spend budget on dead tables.
///
/// A starred fast path that also answers under the component policy wins the
/// dispatch of every entry point as well, so a top-level alternative never
/// runs the engine behind it either. The engine is charged but not built: an
/// extglob arm's prefix scan does run it, and
/// [`compile_extglob_alternative`] builds it there, and the differential
/// hooks build it on demand as their oracle. Every other shape may reach the
/// general path under some option profile and gets its engine here.
///
/// `built` is an engine already compiled from these very tokens under these
/// options (`reuses_original_tokens`). Its `Arc`-backed tables are shared
/// instead of allocating a second copy, so the shared tables are not charged to
/// `budget` a second time. Likewise, when `reuses_original_tokens` is true and
/// the original alternative already paid for a deferred sweep on the same
/// tokens, the path-filter copy does not pay for those tables a second time.
///
/// When `allow_sweep_fallback` is true (multi-alternative brace expansions
/// populated after mandatory IR), a complete fast path neither builds a
/// `SweepEngine` nor deducts unused sweep tables from the shared `budget`
/// (still rejecting any single arm whose own sweep exceeds
/// `MAX_COMPILED_IR_UNITS`), so fast-path arms cannot starve sweep-backed
/// sibling arms regardless of brace order (#495).
#[allow(clippy::too_many_arguments)]
fn compile_sweep(
    tokens: &[Token],
    fast_path: &Option<FastPath>,
    extglob: Option<&CompiledExtglob>,
    options: PatternOptions,
    budget: &mut IrBudget,
    built: Option<&SweepEngine>,
    reuses_original_tokens: bool,
    allow_sweep_fallback: bool,
) -> Result<Option<SweepEngine>, PatternError> {
    if extglob.is_some()
        || matches!(
            fast_path,
            Some(FastPath::LiteralTokens | FastPath::DeterministicTokens)
        )
    {
        return Ok(None);
    }
    if fast_path
        .as_ref()
        .is_some_and(FastPath::supports_component_wildcards)
    {
        if reuses_original_tokens {
            return Ok(None);
        }
        if allow_sweep_fallback {
            // Fast path answers every entry point and never allocates a
            // `SweepEngine`; validate the single-arm ceiling without deducting
            // from the shared budget so sibling arms keep their sweeps.
            SweepEngine::charge(tokens, &mut IrBudget::new())?;
            return Ok(None);
        }
        SweepEngine::charge(tokens, budget)?;
        return Ok(None);
    }
    if let Some(built) = built {
        return Ok(Some(built.clone()));
    }
    if allow_sweep_fallback {
        let sweep_bytes = SweepEngine::retained_bytes_for(tokens)?;
        if sweep_bytes > budget.remaining_retained_bytes {
            SweepEngine::charge(tokens, &mut IrBudget::new())?;
            return Ok(None);
        }
        let saved_units = budget.remaining;
        return match SweepEngine::compile(tokens, options, budget) {
            Ok(Some(sweep)) => {
                budget.remaining_retained_bytes -= sweep_bytes;
                Ok(Some(sweep))
            }
            Ok(None) => Ok(None),
            Err(error)
                if error.message() == TOO_MUCH_COMPILED_IR
                    && SweepEngine::charge(tokens, &mut IrBudget::new()).is_ok() =>
            {
                budget.remaining = saved_units;
                Ok(None)
            }
            Err(error) => Err(error),
        };
    }
    SweepEngine::compile(tokens, options, budget)
}

/// Whether `alternative` carries an engine [`compile_sweep`] paid for but did
/// not build, because its fast path answers every top-level entry point.
fn sweep_is_deferred(alternative: &CompiledAlternative) -> bool {
    alternative.sweep.is_none()
        && alternative.extglob.is_none()
        && alternative.fast_path.as_ref().is_some_and(|fast_path| {
            fast_path.supports_component_wildcards()
                && !matches!(
                    fast_path,
                    FastPath::LiteralTokens | FastPath::DeterministicTokens
                )
        })
}

/// Conditions every candidate the general engine accepts must already satisfy.
///
/// The engine is a memoized depth-first walk over (token, path position) pairs.
/// Nothing in it consults the pattern's trailing literal before exploring, so
/// `a*a*a*a*b` against a run of `a`s visits the whole state space and only then
/// fails on the final `b`. These three facts are cheap to check up front and
/// cannot turn a non-match into a match: each one is implied by a successful
/// match rather than being a second opinion about one.
///
/// Soundness rests on the engine accepting only when it has consumed the entire
/// candidate — `token_index == tokens.len() && path_index == path.len()`. The
/// leading run of `Literal`/`Separator` tokens therefore has to match at
/// position 0, the trailing run has to match at the end, and every token
/// consumes at least the byte count counted in `min_length`.
///
/// The one token whose contribution is not fixed is the `Separator` of a
/// trailing `.../**`: the engine lets it consume nothing and end the match
/// there, which is how `dir/**` accepts `dir`. It can only ever be the
/// second-to-last token, with a `RecursiveStar` behind it, so it can never sit
/// inside a *trailing* run — a run that reaches the end of the token list would
/// have to contain that `RecursiveStar`, which is neither a literal nor a
/// separator. It can sit at the end of a *leading* run, and `min_length`
/// counts it, so both of those subtract it explicitly.
///
/// A fourth fact covers the fixed runs in between: `**/node_modules/**` has no
/// fixed end, yet every match contains `node_modules`. See [`InteriorLiteral`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Prefilter {
    /// Bytes every match starts with, one per byte of the leading
    /// `Literal`/`Separator` run.
    prefix: Vec<u8>,
    /// Bytes every match ends with, one per byte of the trailing run.
    suffix: Vec<u8>,
    /// Bytes the pattern consumes even when every star consumes nothing.
    min_length: usize,
    /// The longest fixed run strictly between the leading and trailing runs,
    /// which every match contains somewhere. Boxed because a searcher is much
    /// larger than the pointer, and most alternatives have none.
    interior: Option<Box<InteriorLiteral>>,
}

/// A byte string every match contains, with its searcher built once.
///
/// Between two wildcards a run of `Literal`/`Separator` tokens is consumed as
/// a unit: a literal token only ever advances over exactly its own bytes and a
/// separator token over exactly one separator byte, so every accepted
/// candidate holds the run's bytes contiguously somewhere. Where it sits is up
/// to the wildcards around it, which is why this is a substring search rather
/// than an anchored comparison.
///
/// What the run may contain is narrower than what the fixed ends may:
///
/// - The separator before a terminal `**` may consume nothing, so it ends a
///   run instead of joining it. The `/` of a `**/` belongs to that wildcard
///   token, which is free to consume nothing, so it never joins either.
/// - On Windows a separator token also accepts `\`, which one needle byte
///   cannot express, so a separator ends a run there as well. An escaped `/`
///   inside a literal token is compared byte for byte and may stay.
/// - Case-insensitive matching compares folded bytes, which `memmem` cannot,
///   so [`Prefilter::rejects`] skips the search when folding is on. The needle
///   is derived without options, so the check has to be made per call.
///
/// Every other option changes what wildcards accept, never what a literal or
/// separator token consumes, and the prefilter is consulted on exactly the
/// candidate the engine then walks. That holds for every entry point: the
/// list-filter copies strip a leading `./` from their own tokens and from the
/// candidate before either reaches here, and an extglob sub-match asks about
/// the slice it is matching.
#[derive(Debug, Clone)]
struct InteriorLiteral(memmem::Finder<'static>);

impl PartialEq for InteriorLiteral {
    fn eq(&self, other: &Self) -> bool {
        self.0.needle() == other.0.needle()
    }
}

impl Eq for InteriorLiteral {}

impl Prefilter {
    fn retained_bytes(&self) -> usize {
        self.prefix
            .len()
            .saturating_add(self.suffix.len())
            .saturating_add(self.interior.as_ref().map_or(0, |interior| {
                size_of::<InteriorLiteral>().saturating_add(interior.0.needle().len())
            }))
    }

    fn compile(tokens: &[Token]) -> Self {
        // `dir/**` matches `dir`, so the separator before a terminal `**` is
        // the one token that may consume nothing.
        let elidable_separator = tokens.len() >= 2
            && matches!(
                tokens[tokens.len() - 2..],
                [Token::Separator, Token::RecursiveStar]
            );
        let fixed = |token: &&Token| matches!(token, Token::Literal(_) | Token::Separator);
        let mut leading = tokens.iter().take_while(fixed).count();
        if elidable_separator && leading + 1 == tokens.len() {
            leading -= 1;
        }
        let trailing = tokens.iter().rev().take_while(fixed).count();
        let min_length =
            tokens.iter().map(token_min_length).sum::<usize>() - usize::from(elidable_separator);
        let prefilter = Self {
            prefix: run_bytes(&tokens[..leading]),
            suffix: run_bytes(&tokens[tokens.len() - trailing..]),
            min_length,
            interior: interior_literal(tokens, leading, trailing, elidable_separator)
                .map(|needle| Box::new(InteriorLiteral(memmem::Finder::new(&needle).into_owned()))),
        };
        debug_assert!(
            prefilter.min_length >= prefilter.prefix.len()
                && prefilter.min_length >= prefilter.suffix.len(),
            "the fixed runs are part of what the minimum length counts"
        );
        prefilter
    }

    /// Whether `path` fails a condition every match satisfies. A `true` here is
    /// a rejection the engine would have reached the slow way.
    fn rejects(&self, path: &[u8], case_insensitive: bool) -> bool {
        if path.len() < self.min_length {
            return true;
        }
        let tail = path.len() - self.suffix.len();
        !run_matches(&self.prefix, &path[..self.prefix.len()], case_insensitive)
            || !run_matches(&self.suffix, &path[tail..], case_insensitive)
            || !case_insensitive
                && self
                    .interior
                    .as_ref()
                    .is_some_and(|interior| interior.0.find(path).is_none())
    }
}

/// The longest run of bytes every match contains that the fixed ends do not
/// already check, if one holds a byte other than a separator.
///
/// `leading` and `trailing` are the token counts of the prefix and suffix
/// runs; their bytes are compared in place and would add nothing here. A lone
/// separator is left out because nearly every path contains one. Ties go to
/// the earlier run. See [`InteriorLiteral`] for what may join a run.
fn interior_literal(
    tokens: &[Token],
    leading: usize,
    trailing: usize,
    elidable_separator: bool,
) -> Option<Vec<u8>> {
    let interior_end = tokens.len() - trailing;
    let mut best: Option<Vec<u8>> = None;
    let mut run = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let joins = index >= leading && index < interior_end;
        match token {
            Token::Literal(literal) if joins => run.extend_from_slice(literal),
            Token::Separator
                if joins
                    && !cfg!(windows)
                    && !(elidable_separator && index + 2 == tokens.len()) =>
            {
                run.push(b'/');
            }
            _ => {
                keep_longer_run(&mut best, &mut run);
            }
        }
    }
    keep_longer_run(&mut best, &mut run);
    best
}

/// Moves `run` into `best` when it is strictly longer and not separators only,
/// and leaves `run` empty for the next one.
fn keep_longer_run(best: &mut Option<Vec<u8>>, run: &mut Vec<u8>) {
    if run.len() > best.as_ref().map_or(0, Vec::len) && run.iter().any(|&byte| !is_separator(byte))
    {
        *best = Some(std::mem::take(run));
    } else {
        run.clear();
    }
}

/// Bytes a run of `Literal`/`Separator` tokens spells out, with a separator
/// written as `/`. Any other token in `run` would be a caller bug and is
/// counted as nothing, which keeps the run shorter and the filter weaker.
fn run_bytes(run: &[Token]) -> Vec<u8> {
    fn bytes(token: &Token) -> &[u8] {
        match token {
            Token::Literal(literal) => literal,
            _ => b"/",
        }
    }
    let mut collected = Vec::with_capacity(run.iter().map(|token| bytes(token).len()).sum());
    for token in run {
        collected.extend_from_slice(bytes(token));
    }
    collected
}

fn token_min_length(token: &Token) -> usize {
    match token {
        Token::Literal(literal) => literal.len(),
        Token::Separator | Token::Any | Token::Class(_) => 1,
        Token::Star | Token::RecursiveStar | Token::RecursivePrefix => 0,
    }
}

fn run_matches(expected: &[u8], actual: &[u8], case_insensitive: bool) -> bool {
    expected
        .iter()
        .zip(actual)
        .all(|(&expected, &actual)| run_byte_matches(expected, actual, case_insensitive))
}

/// Whether one byte of a fixed run is satisfied by the candidate's byte.
///
/// A `Separator` token contributes `/` and accepts whatever the platform calls
/// a separator, which is what the engine's separator arm does. A literal `/` --
/// only reachable through an escape -- is then accepted a little more widely
/// than the engine accepts it, on Windows only. Being more permissive is the
/// safe direction for a filter that may only reject.
fn run_byte_matches(expected: u8, actual: u8, case_insensitive: bool) -> bool {
    if is_separator(expected) {
        is_separator(actual)
    } else {
        bytes_equal(expected, actual, case_insensitive)
    }
}

/// The suffix representation selected once while compiling a fast path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LiteralSuffix {
    Plain(Vec<u8>),
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    Packed16(Box<suffix_word::PreparedSuffix16>),
}

impl LiteralSuffix {
    fn retained_bytes(&self) -> usize {
        match self {
            Self::Plain(bytes) => bytes.len(),
            #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
            Self::Packed16(_) => size_of::<suffix_word::PreparedSuffix16>(),
        }
    }

    fn new(suffix: Vec<u8>, case_insensitive: bool) -> Self {
        #[cfg(not(all(target_arch = "aarch64", target_os = "macos")))]
        let _ = case_insensitive;

        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        if !case_insensitive && let Some(prepared) = suffix_word::PreparedSuffix16::new(&suffix) {
            return Self::Packed16(Box::new(prepared));
        }

        Self::Plain(suffix)
    }

    fn strip_from<'a>(&self, path: &'a [u8], case_insensitive: bool) -> Option<&'a [u8]> {
        match self {
            Self::Plain(suffix) => strip_literal_suffix(path, suffix, case_insensitive),
            #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
            Self::Packed16(suffix) => {
                if !case_insensitive && let Some(matches) = suffix.matches(path) {
                    return matches.then_some(&path[..path.len() - suffix.len()]);
                }
                let bytes = suffix.bytes();
                strip_literal_suffix(path, &bytes[16 - suffix.len()..], case_insensitive)
            }
        }
    }
}

/// One matcher for a brace-expanded set that differs only in its literal
/// suffix. The trie reads a candidate from the end once, so the cost does not
/// depend on where a matching extension appeared in the source brace list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AlternativeFastPath {
    SuffixSet(SuffixSet),
    ScopedSuffixSet(ScopedSuffixSet),
}

impl AlternativeFastPath {
    fn compile(
        alternatives: &[CompiledAlternative],
        options: PatternOptions,
        budget: &mut IrBudget,
    ) -> Result<Option<Self>, PatternError> {
        if alternatives.len() < 2 || alternatives.iter().any(|item| item.extglob.is_some()) {
            return Ok(None);
        }
        if let Some(set) = SuffixSet::compile(alternatives, options.case_insensitive, budget)? {
            return Ok(Some(Self::SuffixSet(set)));
        }
        Ok(
            ScopedSuffixSet::compile(alternatives, options.case_insensitive, budget)?
                .map(Self::ScopedSuffixSet),
        )
    }

    fn is_match(
        &self,
        path: &[u8],
        options: PatternOptions,
        alternatives: &[CompiledAlternative],
    ) -> bool {
        match self {
            Self::SuffixSet(set) => set.is_match(path, options),
            Self::ScopedSuffixSet(set) => {
                alternatives.first().is_some_and(|alternative| {
                    alternative.fast_path.as_ref().is_some_and(|fast_path| {
                        fast_path.is_match(&alternative.tokens, path, options)
                    })
                }) || set.is_match(path, options)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuffixSetKind {
    Star,
    Recursive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuffixSet {
    kind: SuffixSetKind,
    trie: AffixTrie,
}

/// Two independent tries are sound only for a complete prefix/suffix cross
/// product. Compilation proves that shape before this matcher is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopedSuffixSet {
    prefixes: AffixTrie,
    suffixes: AffixTrie,
    has_nested_prefixes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AffixTrie {
    nodes: Vec<AffixNode>,
    edges: Vec<AffixEdge>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AffixNode {
    first_edge: usize,
    edge_count: usize,
    terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AffixEdge {
    byte: u8,
    next: usize,
}

#[derive(Default)]
struct BuildingAffixNode {
    edges: Vec<(u8, usize)>,
    terminal: bool,
}

impl SuffixSet {
    fn compile(
        alternatives: &[CompiledAlternative],
        case_insensitive: bool,
        budget: &mut IrBudget,
    ) -> Result<Option<Self>, PatternError> {
        let mut suffixes = Vec::with_capacity(alternatives.len());
        let mut common_kind = None;
        for alternative in alternatives {
            let Some(fast_path) = alternative.fast_path.as_ref() else {
                return Ok(None);
            };
            let (kind, suffix) = match (fast_path, alternative.tokens.as_slice()) {
                (FastPath::StarSuffix { .. }, [Token::Star, Token::Literal(suffix)]) => {
                    (SuffixSetKind::Star, suffix)
                }
                (
                    FastPath::RecursiveSuffix { .. },
                    [Token::RecursivePrefix, Token::Star, Token::Literal(suffix)],
                ) => (SuffixSetKind::Recursive, suffix),
                _ => return Ok(None),
            };
            if common_kind.is_some_and(|common| common != kind) {
                return Ok(None);
            }
            suffixes.push(suffix.as_slice());
            common_kind = Some(kind);
        }
        Ok(Some(Self {
            kind: common_kind.expect("two suffix alternatives establish a kind"),
            trie: AffixTrie::new(&suffixes, true, case_insensitive, budget)?,
        }))
    }

    fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
        let mut node = 0;
        for (offset, &byte) in path.iter().rev().enumerate() {
            let Some(next) = self
                .trie
                .next(node, fold_ascii(byte, options.case_insensitive))
            else {
                return false;
            };
            node = next;
            if !self.trie.nodes[node].terminal {
                continue;
            }

            let variable_end = path.len() - offset - 1;
            if self.kind == SuffixSetKind::Star
                && options.component_wildcards
                && next_separator(&path[..variable_end]).is_some()
            {
                continue;
            }
            if star_stops_before_hidden_component(path, variable_end, options) {
                continue;
            }
            if options.match_hidden
                || !contains_hidden_component_in(
                    path,
                    0,
                    variable_end,
                    options.candidate_starts_component,
                )
            {
                return true;
            }
        }
        false
    }
}

impl ScopedSuffixSet {
    fn compile(
        alternatives: &[CompiledAlternative],
        case_insensitive: bool,
        budget: &mut IrBudget,
    ) -> Result<Option<Self>, PatternError> {
        let mut prefixes = Vec::new();
        let mut suffixes = Vec::new();
        let mut pairs = Vec::with_capacity(alternatives.len());
        for alternative in alternatives {
            let Some(fast_path) = alternative.fast_path.as_ref() else {
                return Ok(None);
            };
            let (
                FastPath::RecursivePrefixSuffix { .. },
                [
                    Token::Literal(prefix),
                    Token::Separator,
                    Token::RecursivePrefix,
                    Token::Star,
                    Token::Literal(suffix),
                ],
            ) = (fast_path, alternative.tokens.as_slice())
            else {
                return Ok(None);
            };
            let prefix = literal_index(&mut prefixes, prefix, case_insensitive);
            let suffix = literal_index(&mut suffixes, suffix, case_insensitive);
            if !pairs.contains(&(prefix, suffix)) {
                pairs.push((prefix, suffix));
            }
        }

        // Independent tries would otherwise accept a combination the source
        // never named, such as `lib/**/*.ts` from
        // `{src/**/*.ts,lib/**/*.js}`.
        if prefixes
            .len()
            .checked_mul(suffixes.len())
            .is_none_or(|product| pairs.len() != product)
        {
            return Ok(None);
        }
        let prefix_trie = AffixTrie::new(&prefixes, false, case_insensitive, budget)?;
        let has_nested_prefixes = prefix_trie
            .nodes
            .iter()
            .any(|node| node.terminal && node.edge_count > 0);
        Ok(Some(Self {
            prefixes: prefix_trie,
            suffixes: AffixTrie::new(&suffixes, true, case_insensitive, budget)?,
            has_nested_prefixes,
        }))
    }

    fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
        #[cfg(test)]
        SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| stats.set((0, 0, 0)));

        if !self.has_nested_prefixes {
            return self.is_match_disjoint(path, options);
        }
        self.is_match_nested(path, options)
    }

    fn is_match_disjoint(&self, path: &[u8], options: PatternOptions) -> bool {
        let mut prefix_node = 0;
        let mut variable_start = None;
        for (offset, &byte) in path.iter().enumerate() {
            #[cfg(test)]
            SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                stats.set((prefix_steps + 1, suffix_steps, hidden_bytes));
            });
            let Some(next) = self
                .prefixes
                .next(prefix_node, fold_ascii(byte, options.case_insensitive))
            else {
                break;
            };
            prefix_node = next;
            if self.prefixes.nodes[prefix_node].terminal
                && path.get(offset + 1).is_some_and(|byte| is_separator(*byte))
            {
                variable_start = Some(offset + 2);
                break;
            }
        }
        let Some(variable_start) = variable_start else {
            return false;
        };
        matches!(
            self.matches_suffixes_single_start(path, variable_start, options),
            SuffixCheckOutcome::Matched
        )
    }

    #[cold]
    fn is_match_nested(&self, path: &[u8], options: PatternOptions) -> bool {
        let mut prefix_node = 0;
        let mut min_hidden: Option<usize> = None;
        let mut inline_starts = [0_usize; SCOPED_SUFFIX_INLINE_PREFIXES];
        let mut inline_len = 0_usize;
        for (offset, &byte) in path.iter().enumerate() {
            #[cfg(test)]
            SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                stats.set((prefix_steps + 1, suffix_steps, hidden_bytes));
            });
            let Some(next) = self
                .prefixes
                .next(prefix_node, fold_ascii(byte, options.case_insensitive))
            else {
                break;
            };
            prefix_node = next;
            if self.prefixes.nodes[prefix_node].terminal
                && path.get(offset + 1).is_some_and(|byte| is_separator(*byte))
            {
                let variable_start = offset + 2;
                let Some(required_hidden) = min_hidden else {
                    // Check the shortest matching prefix before walking longer
                    // prefixes: a match wins immediately, and a suffix miss
                    // rules out every longer prefix too.
                    match self.matches_suffixes(path, &[variable_start], options) {
                        SuffixCheckOutcome::Matched => return true,
                        SuffixCheckOutcome::NoSuffixMatch => return false,
                        SuffixCheckOutcome::BlockedByHidden { min_hidden: hidden } => {
                            if options.match_hidden || self.prefixes.is_leaf(prefix_node) {
                                return false;
                            }
                            min_hidden = Some(hidden);
                            continue;
                        }
                    }
                };
                if variable_start <= required_hidden {
                    continue;
                }
                if inline_len < inline_starts.len() {
                    inline_starts[inline_len] = variable_start;
                    inline_len += 1;
                } else {
                    return self.is_match_with_spill(
                        path,
                        &inline_starts,
                        prefix_node,
                        offset + 1,
                        variable_start,
                        required_hidden,
                        options,
                    );
                }
            }
        }
        if inline_len == 0 {
            return false;
        }
        matches!(
            self.matches_suffixes(path, &inline_starts[..inline_len], options),
            SuffixCheckOutcome::Matched
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn is_match_with_spill(
        &self,
        path: &[u8],
        inline_starts: &[usize],
        prefix_node: usize,
        resume_offset: usize,
        overflow_start: usize,
        required_hidden: usize,
        options: PatternOptions,
    ) -> bool {
        let collect_and_match = |starts: &mut Vec<usize>| {
            starts.clear();
            starts.extend_from_slice(inline_starts);
            starts.push(overflow_start);
            let mut prefix_node = prefix_node;
            for (offset, &byte) in path.iter().enumerate().skip(resume_offset) {
                #[cfg(test)]
                SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                    let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                    stats.set((prefix_steps + 1, suffix_steps, hidden_bytes));
                });
                let Some(next) = self
                    .prefixes
                    .next(prefix_node, fold_ascii(byte, options.case_insensitive))
                else {
                    break;
                };
                prefix_node = next;
                if self.prefixes.nodes[prefix_node].terminal
                    && path.get(offset + 1).is_some_and(|byte| is_separator(*byte))
                {
                    let variable_start = offset + 2;
                    if variable_start > required_hidden {
                        starts.push(variable_start);
                    }
                }
            }
            matches!(
                self.matches_suffixes(path, starts, options),
                SuffixCheckOutcome::Matched
            )
        };
        SCOPED_SUFFIX_SET_PREFIX_STARTS.with(|cell| match cell.try_borrow_mut() {
            Ok(mut starts) => {
                let matched = collect_and_match(&mut starts);
                starts.clear();
                if starts.capacity() > RETAINED_SCRATCH_WORDS {
                    starts.shrink_to(RETAINED_SCRATCH_WORDS);
                }
                matched
            }
            Err(_) => {
                let mut starts = Vec::with_capacity(inline_starts.len() + 1);
                collect_and_match(&mut starts)
            }
        })
    }

    /// Evaluates every matching suffix in a single right-to-left pass against
    /// strictly increasing matched prefix `starts`.
    ///
    /// For a single start `[variable_start]`, the longest eligible matching
    /// suffix (`min_variable_end`) strictly dominates every shorter suffix
    /// because `[variable_start..min_variable_end)` is a subset of every
    /// `[variable_start..larger_variable_end)`, so one forward hidden scan
    /// rejects an early hidden component immediately.
    ///
    /// For multiple starts, for any `variable_end`, the largest
    /// `variable_start <= variable_end` minimizes the variable span and
    /// keeping a monotonic hidden-dot cursor ensures each byte of `path` is
    /// scanned at most once across the entire call.
    fn matches_suffixes(
        &self,
        path: &[u8],
        starts: &[usize],
        options: PatternOptions,
    ) -> SuffixCheckOutcome {
        debug_assert!(!starts.is_empty());
        if let &[variable_start] = starts {
            return self.matches_suffixes_single_start(path, variable_start, options);
        }
        let min_start = starts[0];
        let mut prefix_idx = starts.len() - 1;
        // Rightmost hidden-component dot found by an earlier suffix split.
        // Because `variable_end` only decreases and every previous reverse
        // scan stopped at `hidden`, no byte below `hidden` has been scanned
        // yet and every split with `hidden < variable_end` fails in O(1).
        let mut known_hidden: Option<usize> = None;
        let mut min_hidden: Option<usize> = None;
        let outcome = |min_hidden: Option<usize>| match min_hidden {
            Some(min_hidden) => SuffixCheckOutcome::BlockedByHidden { min_hidden },
            None => SuffixCheckOutcome::NoSuffixMatch,
        };

        let mut suffix_node = 0;
        for (offset, &byte) in path.iter().rev().enumerate() {
            #[cfg(test)]
            SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                stats.set((prefix_steps, suffix_steps + 1, hidden_bytes));
            });
            let Some(next) = self
                .suffixes
                .next(suffix_node, fold_ascii(byte, options.case_insensitive))
            else {
                return outcome(min_hidden);
            };
            suffix_node = next;
            if !self.suffixes.nodes[suffix_node].terminal {
                continue;
            }
            let variable_end = path.len() - offset - 1;
            if variable_end < min_start {
                return outcome(min_hidden);
            }
            while starts[prefix_idx] > variable_end {
                prefix_idx -= 1;
            }
            if star_stops_before_hidden_component(path, variable_end, options) {
                continue;
            }
            if options.match_hidden {
                return SuffixCheckOutcome::Matched;
            }

            let variable_start = starts[prefix_idx];
            if let Some(hidden) = known_hidden
                && hidden < variable_end
            {
                if hidden >= variable_start {
                    min_hidden = Some(min_hidden.map_or(hidden, |prev| prev.min(hidden)));
                    continue;
                }
                return SuffixCheckOutcome::Matched;
            }
            let no_longer_suffix = self.suffixes.is_leaf(suffix_node)
                || variable_end <= min_start
                || self
                    .suffixes
                    .next(
                        suffix_node,
                        fold_ascii(path[variable_end - 1], options.case_insensitive),
                    )
                    .is_none();
            if no_longer_suffix {
                // No longer suffix can match from this node on `path`, so scan
                // forward and stop at the earliest hidden component.
                return match first_hidden_component_in(
                    path,
                    variable_start,
                    variable_end,
                    options.candidate_starts_component,
                ) {
                    Some(hidden) => {
                        #[cfg(test)]
                        SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                            let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                            stats.set((
                                prefix_steps,
                                suffix_steps,
                                hidden_bytes + (hidden - variable_start + 1),
                            ));
                        });
                        SuffixCheckOutcome::BlockedByHidden {
                            min_hidden: min_hidden.map_or(hidden, |prev| prev.min(hidden)),
                        }
                    }
                    None => {
                        #[cfg(test)]
                        SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                            let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                            stats.set((
                                prefix_steps,
                                suffix_steps,
                                hidden_bytes + (variable_end - variable_start),
                            ));
                        });
                        SuffixCheckOutcome::Matched
                    }
                };
            }

            match rightmost_hidden_component_in(
                path,
                variable_start,
                variable_end,
                options.candidate_starts_component,
            ) {
                Some(hidden) => {
                    #[cfg(test)]
                    SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                        let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                        stats.set((
                            prefix_steps,
                            suffix_steps,
                            hidden_bytes + (variable_end - hidden),
                        ));
                    });
                    known_hidden = Some(hidden);
                    min_hidden = Some(min_hidden.map_or(hidden, |prev| prev.min(hidden)));
                }
                None => {
                    #[cfg(test)]
                    SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                        let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                        stats.set((
                            prefix_steps,
                            suffix_steps,
                            hidden_bytes + (variable_end - variable_start),
                        ));
                    });
                    return SuffixCheckOutcome::Matched;
                }
            }
        }
        outcome(min_hidden)
    }

    /// Single-start fast path: walks the suffix trie from the end of `path`,
    /// running at most one forward hidden scan on the first eligible terminal
    /// suffix so an unblocked short suffix returns immediately without walking
    /// deeper trie branches, while any longer suffix ending at or before the
    /// cached first hidden period still wins in O(1).
    fn matches_suffixes_single_start(
        &self,
        path: &[u8],
        variable_start: usize,
        options: PatternOptions,
    ) -> SuffixCheckOutcome {
        let mut first_hidden: Option<usize> = None;
        let mut suffix_node = 0;
        for (offset, &byte) in path.iter().rev().enumerate() {
            #[cfg(test)]
            SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                stats.set((prefix_steps, suffix_steps + 1, hidden_bytes));
            });
            let Some(next) = self
                .suffixes
                .next(suffix_node, fold_ascii(byte, options.case_insensitive))
            else {
                break;
            };
            suffix_node = next;
            if !self.suffixes.nodes[suffix_node].terminal {
                continue;
            }
            let variable_end = path.len() - offset - 1;
            if variable_end < variable_start {
                break;
            }
            if star_stops_before_hidden_component(path, variable_end, options) {
                continue;
            }
            if options.match_hidden {
                return SuffixCheckOutcome::Matched;
            }
            match first_hidden {
                None => match first_hidden_component_in(
                    path,
                    variable_start,
                    variable_end,
                    options.candidate_starts_component,
                ) {
                    Some(hidden) => {
                        #[cfg(test)]
                        SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                            let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                            stats.set((
                                prefix_steps,
                                suffix_steps,
                                hidden_bytes + (hidden - variable_start + 1),
                            ));
                        });
                        first_hidden = Some(hidden);
                    }
                    None => {
                        #[cfg(test)]
                        SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| {
                            let (prefix_steps, suffix_steps, hidden_bytes) = stats.get();
                            stats.set((
                                prefix_steps,
                                suffix_steps,
                                hidden_bytes + (variable_end - variable_start),
                            ));
                        });
                        return SuffixCheckOutcome::Matched;
                    }
                },
                Some(hidden) => {
                    // `hidden` is the leftmost component-leading `.` in
                    // `[variable_start..first_variable_end)`. Since every
                    // subsequent suffix has a strictly smaller `variable_end`,
                    // `[variable_start..variable_end)` is hidden-free iff
                    // `variable_end <= hidden`.
                    if variable_end <= hidden {
                        return SuffixCheckOutcome::Matched;
                    }
                }
            }
        }
        match first_hidden {
            Some(min_hidden) => SuffixCheckOutcome::BlockedByHidden { min_hidden },
            None => SuffixCheckOutcome::NoSuffixMatch,
        }
    }
}

enum SuffixCheckOutcome {
    Matched,
    NoSuffixMatch,
    BlockedByHidden { min_hidden: usize },
}

const SCOPED_SUFFIX_INLINE_PREFIXES: usize = 16;

thread_local! {
    static SCOPED_SUFFIX_SET_PREFIX_STARTS: RefCell<Vec<usize>> =
        const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
thread_local! {
    /// `(prefix_trie_steps, suffix_trie_steps, hidden_scan_bytes)` recorded by
    /// the most recent `ScopedSuffixSet::is_match` call.
    static SCOPED_SUFFIX_SET_SCAN_STATS: std::cell::Cell<(usize, usize, usize)> =
        const { std::cell::Cell::new((0, 0, 0)) };
}

#[cfg(test)]
fn reset_scoped_suffix_set_scan_stats() {
    SCOPED_SUFFIX_SET_SCAN_STATS.with(|stats| stats.set((usize::MAX, usize::MAX, usize::MAX)));
}

#[cfg(test)]
fn scoped_suffix_set_scan_stats() -> (usize, usize, usize) {
    SCOPED_SUFFIX_SET_SCAN_STATS.with(std::cell::Cell::get)
}

fn literal_index<'a>(
    literals: &mut Vec<&'a [u8]>,
    literal: &'a [u8],
    case_insensitive: bool,
) -> usize {
    if let Some(index) = literals.iter().position(|existing| {
        existing.len() == literal.len()
            && existing
                .iter()
                .zip(literal)
                .all(|(&left, &right)| bytes_equal(left, right, case_insensitive))
    }) {
        return index;
    }
    literals.push(literal);
    literals.len() - 1
}

impl AffixTrie {
    fn new(
        words: &[&[u8]],
        reverse: bool,
        case_insensitive: bool,
        budget: &mut IrBudget,
    ) -> Result<Self, PatternError> {
        // The root exists in the temporary builder and the flattened trie at
        // the same time while the latter is assembled.
        budget.charge(2, 0)?;
        let mut building = vec![BuildingAffixNode::default()];
        for word in words {
            if reverse {
                Self::insert(
                    &mut building,
                    word.iter()
                        .rev()
                        .map(|&byte| fold_ascii(byte, case_insensitive)),
                    budget,
                )?;
            } else {
                Self::insert(
                    &mut building,
                    word.iter().map(|&byte| fold_ascii(byte, case_insensitive)),
                    budget,
                )?;
            }
        }

        let edge_count = building.iter().map(|node| node.edges.len()).sum();
        let mut nodes = Vec::with_capacity(building.len());
        let mut edges = Vec::with_capacity(edge_count);
        for node in building {
            let first_edge = edges.len();
            edges.extend(
                node.edges
                    .into_iter()
                    .map(|(byte, next)| AffixEdge { byte, next }),
            );
            nodes.push(AffixNode {
                first_edge,
                edge_count: edges.len() - first_edge,
                terminal: node.terminal,
            });
        }
        Ok(Self { nodes, edges })
    }

    fn insert(
        building: &mut Vec<BuildingAffixNode>,
        bytes: impl Iterator<Item = u8>,
        budget: &mut IrBudget,
    ) -> Result<(), PatternError> {
        let mut node = 0;
        for byte in bytes {
            let existing = building[node]
                .edges
                .iter()
                .find_map(|&(edge, next)| (edge == byte).then_some(next));
            node = if let Some(existing) = existing {
                existing
            } else {
                // Construction temporarily owns the builder node and edge as
                // well as their flattened counterparts. Three token-sized IR
                // units conservatively cover that peak, not merely the final
                // node/edge pair.
                budget.charge(3, 0)?;
                let next = building.len();
                building.push(BuildingAffixNode::default());
                building[node].edges.push((byte, next));
                next
            };
        }
        building[node].terminal = true;
        Ok(())
    }

    fn is_leaf(&self, node: usize) -> bool {
        self.nodes[node].edge_count == 0
    }

    fn next(&self, node: usize, byte: u8) -> Option<usize> {
        let node = self.nodes[node];
        self.edges[node.first_edge..node.first_edge + node.edge_count]
            .iter()
            .find_map(|edge| (edge.byte == byte).then_some(edge.next))
    }
}

/// An allocation-free matcher for a common recursive-prefix/suffix shape.
///
/// It is deliberately narrower than the full token matcher. Keeping it as a
/// compiled variant makes the optimization opt-in by syntax and leaves every
/// other pattern on the corpus-tested general path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FastPath {
    LiteralTokens,
    DeterministicTokens,
    Star,
    PrefixStar {
        prefix: Vec<u8>,
    },
    StarSuffix {
        suffix: LiteralSuffix,
    },
    InfixStar {
        prefix: Vec<u8>,
        suffix: Vec<u8>,
    },
    StaticStar {
        star_index: usize,
    },
    RecursiveTerminalPrefix {
        prefix: Vec<u8>,
    },
    RecursiveSuffix {
        suffix: LiteralSuffix,
        suffix_last: u8,
    },
    RecursivePrefixSuffix {
        prefix: Vec<u8>,
        suffix: LiteralSuffix,
        suffix_last: u8,
    },
}

impl FastPath {
    fn compile(tokens: &[Token], options: PatternOptions) -> Option<Self> {
        if tokens
            .iter()
            .all(|token| matches!(token, Token::Literal(_) | Token::Separator))
        {
            return Some(Self::LiteralTokens);
        }
        if tokens.iter().all(|token| {
            matches!(
                token,
                Token::Literal(_) | Token::Separator | Token::Any | Token::Class(_)
            )
        }) {
            return Some(Self::DeterministicTokens);
        }
        if options.component_wildcards {
            return None;
        }
        match tokens {
            [Token::Star] => return Some(Self::Star),
            [Token::Literal(prefix), Token::Star] => {
                return Some(Self::PrefixStar {
                    prefix: prefix.clone(),
                });
            }
            [Token::Star, Token::Literal(suffix)] => {
                return Some(Self::StarSuffix {
                    suffix: LiteralSuffix::new(suffix.clone(), options.case_insensitive),
                });
            }
            [Token::Literal(prefix), Token::Star, Token::Literal(suffix)] => {
                return Some(Self::InfixStar {
                    prefix: prefix.clone(),
                    suffix: suffix.clone(),
                });
            }
            _ => {}
        }
        if let Some(star_index) = tokens.iter().position(|token| matches!(token, Token::Star))
            && tokens.len() > 1
            && tokens.iter().enumerate().all(|(index, token)| {
                index == star_index || matches!(token, Token::Literal(_) | Token::Separator)
            })
        {
            return Some(Self::StaticStar { star_index });
        }
        if let [
            Token::Literal(prefix),
            Token::Separator,
            Token::RecursiveStar,
        ] = tokens
        {
            return Some(Self::RecursiveTerminalPrefix {
                prefix: prefix.clone(),
            });
        }
        if let [Token::RecursivePrefix, Token::Star, Token::Literal(suffix)] = tokens {
            return Some(Self::RecursiveSuffix {
                suffix: LiteralSuffix::new(suffix.clone(), options.case_insensitive),
                suffix_last: *suffix.last().expect("literal token is non-empty"),
            });
        }
        let [
            Token::Literal(prefix),
            Token::Separator,
            Token::RecursivePrefix,
            Token::Star,
            Token::Literal(suffix),
        ] = tokens
        else {
            return None;
        };
        Some(Self::RecursivePrefixSuffix {
            prefix: prefix.clone(),
            suffix: LiteralSuffix::new(suffix.clone(), options.case_insensitive),
            suffix_last: *suffix.last().expect("literal token is non-empty"),
        })
    }

    fn supports_component_wildcards(&self) -> bool {
        matches!(
            self,
            Self::LiteralTokens
                | Self::DeterministicTokens
                | Self::StarSuffix { .. }
                | Self::RecursiveSuffix { .. }
                | Self::RecursivePrefixSuffix { .. }
        )
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::LiteralTokens
            | Self::DeterministicTokens
            | Self::Star
            | Self::StaticStar { .. } => 0,
            Self::PrefixStar { prefix } | Self::RecursiveTerminalPrefix { prefix } => prefix.len(),
            Self::StarSuffix { suffix } | Self::RecursiveSuffix { suffix, .. } => {
                suffix.retained_bytes()
            }
            Self::InfixStar { prefix, suffix } => prefix.len().saturating_add(suffix.len()),
            Self::RecursivePrefixSuffix { prefix, suffix, .. } => {
                prefix.len().saturating_add(suffix.retained_bytes())
            }
        }
    }

    fn is_match(&self, tokens: &[Token], path: &[u8], options: PatternOptions) -> bool {
        match self {
            Self::LiteralTokens => {
                let mut path_index = 0;
                for token in tokens {
                    match token {
                        Token::Literal(literal) => {
                            let Some(candidate) = path.get(path_index..path_index + literal.len())
                            else {
                                return false;
                            };
                            if !literal.iter().zip(candidate).all(|(&expected, &actual)| {
                                bytes_equal(expected, actual, options.case_insensitive)
                            }) {
                                return false;
                            }
                            path_index += literal.len();
                        }
                        Token::Separator => {
                            if !path.get(path_index).is_some_and(|byte| is_separator(*byte)) {
                                return false;
                            }
                            path_index += 1;
                        }
                        _ => unreachable!("literal fast path only stores literal tokens"),
                    }
                }
                path_index == path.len()
            }
            Self::DeterministicTokens => {
                let mut path_index = 0;
                for (token_index, token) in tokens.iter().enumerate() {
                    match token {
                        Token::Literal(literal) => {
                            let Some(candidate) = path.get(path_index..path_index + literal.len())
                            else {
                                return false;
                            };
                            if !literal.iter().zip(candidate).all(|(&expected, &actual)| {
                                bytes_equal(expected, actual, options.case_insensitive)
                            }) {
                                return false;
                            }
                            path_index += literal.len();
                        }
                        Token::Separator => {
                            if !path.get(path_index).is_some_and(|byte| is_separator(*byte)) {
                                return false;
                            }
                            path_index += 1;
                        }
                        Token::Any => {
                            let Some(&byte) = path.get(path_index) else {
                                return false;
                            };
                            if Pattern::component_wildcard(tokens, token_index, options)
                                && is_separator(byte)
                            {
                                return false;
                            }
                            if !options.match_hidden
                                && byte == b'.'
                                && at_component_start(path, path_index, options)
                            {
                                return false;
                            }
                            path_index += 1;
                        }
                        Token::Class(class) => {
                            let Some(&byte) = path.get(path_index) else {
                                return false;
                            };
                            if (Pattern::component_wildcard(tokens, token_index, options)
                                && is_separator(byte))
                                || !class.matches(byte, options.case_insensitive)
                                || (!options.match_hidden
                                    && byte == b'.'
                                    && at_component_start(path, path_index, options))
                            {
                                return false;
                            }
                            path_index += 1;
                        }
                        Token::Star | Token::RecursiveStar | Token::RecursivePrefix => {
                            return false;
                        }
                    }
                }
                path_index == path.len()
            }
            Self::Star => {
                options.match_hidden
                    || !contains_hidden_component_in(
                        path,
                        0,
                        path.len(),
                        options.candidate_starts_component,
                    )
            }
            Self::PrefixStar { prefix } => {
                let Some(variable) = strip_literal_prefix(path, prefix, options.case_insensitive)
                else {
                    return false;
                };
                options.match_hidden
                    || !contains_hidden_component_in(
                        path,
                        path.len() - variable.len(),
                        path.len(),
                        options.candidate_starts_component,
                    )
            }
            Self::StarSuffix { suffix } => {
                let Some(variable) = suffix.strip_from(path, options.case_insensitive) else {
                    return false;
                };
                // The leading star is component-local only in a
                // component-local root position, exactly as
                // `Pattern::component_wildcard` answers for token zero.
                if options.component_wildcards
                    && (options.root_component_wildcards
                        || options.candidate_root_component_wildcard)
                    && next_separator(variable).is_some()
                {
                    return false;
                }
                if star_stops_before_hidden_component(path, variable.len(), options) {
                    return false;
                }
                options.match_hidden
                    || !contains_hidden_component_in(
                        path,
                        0,
                        variable.len(),
                        options.candidate_starts_component,
                    )
            }
            Self::InfixStar { prefix, suffix } => {
                let Some(remainder) = strip_literal_prefix(path, prefix, options.case_insensitive)
                else {
                    return false;
                };
                let Some(variable) =
                    strip_literal_suffix(remainder, suffix, options.case_insensitive)
                else {
                    return false;
                };
                let variable_start = path.len() - remainder.len();
                let variable_end = variable_start + variable.len();
                if star_stops_before_hidden_component(path, variable_end, options) {
                    return false;
                }
                options.match_hidden
                    || !contains_hidden_component_in(
                        path,
                        variable_start,
                        variable_end,
                        options.candidate_starts_component,
                    )
            }
            Self::StaticStar { star_index } => {
                let prefix = &tokens[..*star_index];
                let suffix = &tokens[*star_index + 1..];
                let Some(variable_start) = match_static_prefix(prefix, path, options) else {
                    return false;
                };
                let Some(variable_end) = match_static_suffix(suffix, path, options) else {
                    return false;
                };
                if variable_start > variable_end {
                    return false;
                }
                if star_stops_before_hidden_component(path, variable_end, options) {
                    return false;
                }
                options.match_hidden
                    || !contains_hidden_component_in(
                        path,
                        variable_start,
                        variable_end,
                        options.candidate_starts_component,
                    )
            }
            Self::RecursiveTerminalPrefix { prefix } => {
                let Some(remainder) = strip_literal_prefix(path, prefix, options.case_insensitive)
                else {
                    return false;
                };
                remainder.is_empty()
                    || remainder.first().is_some_and(|byte| is_separator(*byte))
                        && (options.match_hidden
                            || !contains_hidden_component_in(
                                path,
                                path.len() - remainder.len() + 1,
                                path.len(),
                                options.candidate_starts_component,
                            ))
            }
            Self::RecursiveSuffix {
                suffix,
                suffix_last,
            } => {
                let Some(&path_last) = path.last() else {
                    return false;
                };
                if !bytes_equal(*suffix_last, path_last, options.case_insensitive) {
                    return false;
                }
                let Some(variable) = suffix.strip_from(path, options.case_insensitive) else {
                    return false;
                };
                if star_stops_before_hidden_component(path, variable.len(), options) {
                    return false;
                }
                options.match_hidden
                    || !contains_hidden_component_in(
                        path,
                        0,
                        variable.len(),
                        options.candidate_starts_component,
                    )
            }
            Self::RecursivePrefixSuffix {
                prefix,
                suffix,
                suffix_last,
            } => {
                let Some(&path_last) = path.last() else {
                    return false;
                };
                if !bytes_equal(*suffix_last, path_last, options.case_insensitive) {
                    return false;
                }
                let Some(prefix_and_variable) = suffix.strip_from(path, options.case_insensitive)
                else {
                    return false;
                };
                let Some(remainder) =
                    strip_literal_prefix(prefix_and_variable, prefix, options.case_insensitive)
                else {
                    return false;
                };
                let Some((&separator, variable)) = remainder.split_first() else {
                    return false;
                };
                if !is_separator(separator) {
                    return false;
                }
                let variable_start = prefix.len() + 1;
                let variable_end = variable_start + variable.len();
                if star_stops_before_hidden_component(path, variable_end, options) {
                    return false;
                }
                options.match_hidden
                    || !contains_hidden_component_in(
                        path,
                        variable_start,
                        variable_end,
                        options.candidate_starts_component,
                    )
            }
        }
    }
}

fn match_static_prefix(tokens: &[Token], path: &[u8], options: PatternOptions) -> Option<usize> {
    let mut index = 0;
    for token in tokens {
        match token {
            Token::Literal(literal) => {
                let candidate = path.get(index..index + literal.len())?;
                if !literal.iter().zip(candidate).all(|(&expected, &actual)| {
                    bytes_equal(expected, actual, options.case_insensitive)
                }) {
                    return None;
                }
                index += literal.len();
            }
            Token::Separator => {
                if !path.get(index).is_some_and(|byte| is_separator(*byte)) {
                    return None;
                }
                index += 1;
            }
            _ => return None,
        }
    }
    Some(index)
}

fn match_static_suffix(tokens: &[Token], path: &[u8], options: PatternOptions) -> Option<usize> {
    let mut index = path.len();
    for token in tokens.iter().rev() {
        match token {
            Token::Literal(literal) => {
                let start = index.checked_sub(literal.len())?;
                if !literal
                    .iter()
                    .zip(&path[start..index])
                    .all(|(&expected, &actual)| {
                        bytes_equal(expected, actual, options.case_insensitive)
                    })
                {
                    return None;
                }
                index = start;
            }
            Token::Separator => {
                let start = index.checked_sub(1)?;
                if !is_separator(path[start]) {
                    return None;
                }
                index = start;
            }
            _ => return None,
        }
    }
    Some(index)
}

fn strip_literal_prefix<'a>(
    path: &'a [u8],
    literal: &[u8],
    case_insensitive: bool,
) -> Option<&'a [u8]> {
    let candidate = path.get(..literal.len())?;
    literal
        .iter()
        .zip(candidate)
        .all(|(&expected, &actual)| bytes_equal(expected, actual, case_insensitive))
        .then_some(&path[literal.len()..])
}

fn strip_literal_suffix<'a>(
    path: &'a [u8],
    literal: &[u8],
    case_insensitive: bool,
) -> Option<&'a [u8]> {
    let start = path.len().checked_sub(literal.len())?;
    literal
        .iter()
        .zip(&path[start..])
        .all(|(&expected, &actual)| bytes_equal(expected, actual, case_insensitive))
        .then_some(&path[..start])
}

fn first_hidden_component_in(
    path: &[u8],
    start: usize,
    end: usize,
    candidate_starts_component: bool,
) -> Option<usize> {
    let segment = path.get(start..end)?;
    let mut offset = start;
    while let Some(found) = memchr(b'.', &segment[offset - start..]) {
        let index = offset + found;
        if (index == 0 && candidate_starts_component)
            || (index > 0 && is_separator(path[index - 1]))
        {
            return Some(index);
        }
        offset = index + 1;
    }
    None
}

fn contains_hidden_component_in(
    path: &[u8],
    start: usize,
    end: usize,
    candidate_starts_component: bool,
) -> bool {
    first_hidden_component_in(path, start, end, candidate_starts_component).is_some()
}

/// Returns the rightmost component-leading period index in `path[start..end]`.
fn rightmost_hidden_component_in(
    path: &[u8],
    start: usize,
    end: usize,
    candidate_starts_component: bool,
) -> Option<usize> {
    let mut search_end = end.min(path.len());
    while search_end > start {
        let found = memrchr(b'.', &path[start..search_end])?;
        let index = start + found;
        if (index == 0 && candidate_starts_component)
            || (index > 0 && is_separator(path[index - 1]))
        {
            return Some(index);
        }
        search_end = index;
    }
    None
}

/// Whether an ordinary star would hand a component-leading period to the
/// literal suffix without consuming it.
fn star_stops_before_hidden_component(path: &[u8], index: usize, options: PatternOptions) -> bool {
    !options.match_hidden
        && path.get(index) == Some(&b'.')
        && at_component_start(path, index, options)
}

/// Removes the one conventional current-directory prefix accepted by path APIs.
fn without_leading_dot_slash(path: &[u8]) -> &[u8] {
    path.strip_prefix(b"./").unwrap_or(path)
}

fn path_after_base<'a>(base_path: &[u8], path: &'a [u8]) -> Option<&'a [u8]> {
    if base_path.is_empty() {
        return Some(path);
    }
    let base_path = base_path
        .iter()
        .rposition(|byte| !is_separator(*byte))
        .map_or(&base_path[..1], |last| &base_path[..=last]);
    if base_path.len() == 1 && is_separator(base_path[0]) {
        return path.strip_prefix(base_path);
    }
    let suffix = path.strip_prefix(base_path)?;
    suffix
        .strip_prefix(b"/")
        .or_else(|| cfg!(windows).then(|| suffix.strip_prefix(b"\\")).flatten())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Class {
    negated: bool,
    members: Vec<ClassMember>,
}

impl Class {
    fn matches(&self, byte: u8, case_insensitive: bool) -> bool {
        let included = self.members.iter().any(|member| match member {
            ClassMember::Byte(expected) => bytes_equal(*expected, byte, case_insensitive),
            ClassMember::Range(start, end) => {
                let byte = fold_ascii(byte, case_insensitive);
                let start = fold_ascii(*start, case_insensitive);
                let end = fold_ascii(*end, case_insensitive);
                start <= byte && byte <= end
            }
            ClassMember::Posix(class) => class.matches(byte, case_insensitive),
        });
        included != self.negated
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassMember {
    Byte(u8),
    Range(u8, u8),
    Posix(PosixClass),
}

/// One byte collected inside `[...]`, tagged with how it was written.
///
/// Bash and glibc `fnmatch` only treat an *unescaped* `-` as the range
/// operator, so the escape has to be remembered until ranges are grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClassValue {
    byte: u8,
    escaped: bool,
}

impl ClassValue {
    const fn literal(byte: u8) -> Self {
        Self {
            byte,
            escaped: false,
        }
    }

    const fn escaped(byte: u8) -> Self {
        Self {
            byte,
            escaped: true,
        }
    }

    const fn is_range_separator(self) -> bool {
        self.byte == b'-' && !self.escaped
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PosixClass {
    Alnum,
    Alpha,
    Ascii,
    Blank,
    Cntrl,
    Digit,
    Graph,
    Lower,
    Print,
    Punct,
    Space,
    Upper,
    Word,
    Xdigit,
}

const MAX_POSIX_CLASS_NAME_LEN: usize = 6;

impl PosixClass {
    fn matches(self, byte: u8, case_insensitive: bool) -> bool {
        match self {
            Self::Alnum => byte.is_ascii_alphanumeric(),
            Self::Alpha => byte.is_ascii_alphabetic(),
            Self::Ascii => byte.is_ascii(),
            Self::Blank => matches!(byte, b' ' | b'\t'),
            Self::Cntrl => byte.is_ascii_control(),
            Self::Digit => byte.is_ascii_digit(),
            Self::Graph => byte.is_ascii_graphic(),
            Self::Lower | Self::Upper if case_insensitive => byte.is_ascii_alphabetic(),
            Self::Lower => byte.is_ascii_lowercase(),
            Self::Print => byte.is_ascii_graphic() || byte == b' ',
            Self::Punct => byte.is_ascii_punctuation(),
            Self::Space => byte.is_ascii_whitespace(),
            Self::Upper => byte.is_ascii_uppercase(),
            Self::Word => byte.is_ascii_alphanumeric() || byte == b'_',
            Self::Xdigit => byte.is_ascii_hexdigit(),
        }
    }
}

fn parse_class(
    pattern: &[u8],
    start: usize,
    escapes: bool,
) -> Result<(Class, usize), PatternError> {
    let mut index = start + 1;
    let mut negated = false;
    if pattern
        .get(index)
        .is_some_and(|byte| matches!(byte, b'!' | b'^'))
    {
        negated = true;
        index += 1;
    }

    // Each entry keeps the byte together with whether it reached the class
    // through a backslash escape. Only an unescaped `-` separates a range, so
    // the flag has to survive until `class_members` groups the entries.
    let mut values: Vec<ClassValue> = Vec::new();
    let mut members = Vec::new();
    if pattern.get(index) == Some(&b']') {
        values.push(ClassValue::literal(b']'));
        index += 1;
    }
    while let Some(&byte) = pattern.get(index) {
        if byte == b']' {
            if values.is_empty() && members.is_empty() {
                return Err(PatternError {
                    offset: start,
                    message: "empty character class",
                });
            }
            return Ok((
                Class {
                    negated,
                    members: {
                        members.extend(class_members(values));
                        members
                    },
                },
                index + 1,
            ));
        }
        if byte == b'[' && pattern.get(index + 1) == Some(&b':') {
            let name_start = index + 2;
            // POSIX class names are a closed, short set. Bounding the search
            // keeps every malformed opener constant-time while preserving the
            // old rule that only the first `:]` can close this opener.
            let search_end = pattern.len().min(name_start + MAX_POSIX_CLASS_NAME_LEN + 2);
            if let Some(posix_end) = memmem::find(&pattern[name_start..search_end], b":]")
                && let Some(class) = parse_posix_class(&pattern[name_start..name_start + posix_end])
            {
                let name_end = name_start + posix_end;
                members.extend(class_members(std::mem::take(&mut values)));
                members.push(ClassMember::Posix(class));
                index = name_end + 2;
                continue;
            }
        }
        if byte == b'\\' && escapes {
            let Some(&escaped) = pattern.get(index + 1) else {
                return Err(PatternError {
                    offset: index,
                    message: "trailing escape in character class",
                });
            };
            values.push(ClassValue::escaped(escaped));
            index += 2;
        } else {
            values.push(ClassValue::literal(byte));
            index += 1;
        }
    }

    Err(PatternError {
        offset: start,
        message: "unclosed character class",
    })
}

/// One component's only path-relevant literal shapes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
enum WalkerComponentKind {
    #[default]
    Empty,
    Dot,
    Parent,
    Other,
}

/// A source location kept for a diagnostic only.
///
/// It never distinguishes two walker states: the extglob analysis
/// deduplicates states by what they can still match, and letting a location
/// split them would multiply the state set, and the IR budget it is charged
/// against, by the number of separators the arms of a group end in. Of states
/// that differ only here, the first one reached keeps its location.
#[derive(Debug, Clone, Copy, Default)]
struct DiagnosticOffset(Option<usize>);

impl PartialEq for DiagnosticOffset {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for DiagnosticOffset {}

impl Hash for DiagnosticOffset {
    fn hash<H: Hasher>(&self, _: &mut H) {}
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
struct WalkerComponent {
    kind: WalkerComponentKind,
    /// The first literal dot in this component, when it still has a
    /// determinate location in the caller's unexpanded source bytes.
    offset: Option<usize>,
    /// Where an empty component is. It has no byte of its own, so it is
    /// located at a separator next to it: the one that closes it (the repeated
    /// separator in `src//x`), or for a trailing one the separator that opens
    /// it. Meaningful only while `kind` is `Empty`.
    separator: DiagnosticOffset,
}

impl WalkerComponent {
    /// The component that follows a separator at `offset`.
    const fn opened_at(offset: Option<usize>) -> Self {
        Self {
            kind: WalkerComponentKind::Empty,
            offset: None,
            separator: DiagnosticOffset(offset),
        }
    }

    /// Records the separator at `offset` that ends this component. Only an
    /// empty component takes its location from it.
    fn close_at(&mut self, offset: Option<usize>) {
        if self.kind == WalkerComponentKind::Empty && offset.is_some() {
            self.separator = DiagnosticOffset(offset);
        }
    }

    /// Where this component is, if it is empty and that is known.
    const fn empty_location(self) -> Option<usize> {
        match self.kind {
            WalkerComponentKind::Empty => self.separator.0,
            _ => None,
        }
    }

    fn push_literal(&mut self, byte: u8, offset: Option<usize>) {
        match (self.kind, byte) {
            (WalkerComponentKind::Empty, b'.') => {
                self.kind = WalkerComponentKind::Dot;
                self.offset = offset;
            }
            (WalkerComponentKind::Dot, b'.') => self.kind = WalkerComponentKind::Parent,
            (
                WalkerComponentKind::Empty | WalkerComponentKind::Dot | WalkerComponentKind::Parent,
                _,
            ) => {
                self.kind = WalkerComponentKind::Other;
                self.offset = None;
            }
            (WalkerComponentKind::Other, _) => {}
        }
    }

    fn wildcard(&mut self) {
        self.kind = WalkerComponentKind::Other;
        self.offset = None;
    }

    fn append(&mut self, other: Self) {
        match other.kind {
            WalkerComponentKind::Empty => {
                if self.kind == WalkerComponentKind::Empty && other.separator.0.is_some() {
                    self.separator = other.separator;
                }
            }
            WalkerComponentKind::Dot => self.push_literal(b'.', other.offset),
            WalkerComponentKind::Parent => {
                self.push_literal(b'.', other.offset);
                self.push_literal(b'.', None);
            }
            WalkerComponentKind::Other => self.wildcard(),
        }
    }
}

/// Path-shape events emitted alongside the ordinary token compiler.
///
/// This is deliberately not a second parser: each method is called from the
/// same match arm that creates a token. It keeps only whether a component is a
/// real literal dot shape, a wildcard-bearing shape, or a separator.
#[derive(Default)]
struct WalkerPathShapeBuilder {
    leading_dot_is_normalized: bool,
    components: Vec<WalkerComponent>,
    current: WalkerComponent,
}

impl WalkerPathShapeBuilder {
    fn new(leading_dot_is_normalized: bool) -> Self {
        Self {
            leading_dot_is_normalized,
            components: Vec::new(),
            current: WalkerComponent::default(),
        }
    }
    fn literal(&mut self, byte: u8, offset: Option<usize>) {
        self.current.push_literal(byte, offset);
    }

    fn escaped(&mut self) {
        self.current.wildcard();
    }

    fn wildcard(&mut self) {
        self.current.wildcard();
    }

    fn separator(&mut self, offset: Option<usize>) {
        let mut component =
            std::mem::replace(&mut self.current, WalkerComponent::opened_at(offset));
        component.close_at(offset);
        self.components.push(component);
    }

    fn finish(mut self) -> WalkerPathShape {
        self.components.push(self.current);
        WalkerPathShape {
            leading_dot_is_normalized: self.leading_dot_is_normalized,
            components: self.components,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WalkerPathState {
    leading_dot_is_normalized: bool,
    /// The number of components already closed by a separator, saturated once
    /// the first-component distinction no longer matters.
    completed_components: u8,
    all_empty_or_dot: bool,
    first_parent: Option<WalkerComponent>,
    first_dot: Option<WalkerComponent>,
    last_nonempty: Option<WalkerComponent>,
    /// The first empty component a separator closed. Such a leading or
    /// interior component is not a spelling the walker can select, even when
    /// later matcher text makes the final component nonempty.
    first_empty: Option<WalkerComponent>,
    current: WalkerComponent,
}

impl WalkerPathState {
    fn new(leading_dot_is_normalized: bool) -> Self {
        Self {
            leading_dot_is_normalized,
            completed_components: 0,
            all_empty_or_dot: true,
            first_parent: None,
            first_dot: None,
            last_nonempty: None,
            first_empty: None,
            current: WalkerComponent::default(),
        }
    }
    fn literal(&mut self, byte: u8, offset: Option<usize>) {
        self.current.push_literal(byte, offset);
    }

    fn escaped(&mut self) {
        // An escape is matcher text, including for `\.` and `\/`. It may
        // match a punctuation byte but does not request filesystem path
        // normalization.
        self.current.wildcard();
    }

    fn wildcard(&mut self) {
        self.current.wildcard();
    }

    fn separator(&mut self, offset: Option<usize>) {
        let mut component =
            std::mem::replace(&mut self.current, WalkerComponent::opened_at(offset));
        component.close_at(offset);
        if component.kind == WalkerComponentKind::Empty && self.first_empty.is_none() {
            self.first_empty = Some(component);
        }
        self.record_component(component);
    }

    fn append_shape(&mut self, shape: &WalkerPathShape) {
        for (index, component) in shape.components.iter().copied().enumerate() {
            self.current.append(component);
            if index + 1 != shape.components.len() {
                // The shape's empty components already carry their separator
                // locations, which `append` has just carried over.
                self.separator(None);
            }
        }
    }

    fn record_component(&mut self, component: WalkerComponent) {
        let first = self.completed_components == 0;
        if !matches!(
            component.kind,
            WalkerComponentKind::Empty | WalkerComponentKind::Dot
        ) {
            self.all_empty_or_dot = false;
        }
        if component.kind == WalkerComponentKind::Parent && self.first_parent.is_none() {
            self.first_parent = Some(component);
        }
        if component.kind == WalkerComponentKind::Dot
            && (!first || !self.leading_dot_is_normalized)
            && self.first_dot.is_none()
        {
            self.first_dot = Some(component);
        }
        if component.kind != WalkerComponentKind::Empty {
            self.last_nonempty = Some(component);
        }
        self.completed_components = self.completed_components.saturating_add(1).min(2);
    }

    fn finish(mut self) -> WalkerPathEvaluation {
        let component = std::mem::take(&mut self.current);
        let empty_component = self
            .first_empty
            .or((component.kind == WalkerComponentKind::Empty).then_some(component));
        self.record_component(component);
        WalkerPathEvaluation {
            selects_candidate: empty_component.is_none(),
            empty_component_offset: empty_component.and_then(WalkerComponent::empty_location),
            problem: WalkerPathSummary {
                all_empty_or_dot: self.all_empty_or_dot,
                first_parent: self.first_parent,
                first_dot: self.first_dot,
                last_nonempty: self.last_nonempty,
            }
            .problem(),
        }
    }
}

#[derive(Clone, Copy)]
struct WalkerPathEvaluation {
    /// A walker candidate ends in a nonempty component and contains no empty
    /// leading or interior component. Nullable groups must not use an empty
    /// arm as a viable escape from their real invalid arms.
    selects_candidate: bool,
    /// Where the empty component that makes this path unselectable is.
    empty_component_offset: Option<usize>,
    problem: Option<WalkerPathProblem>,
}

impl WalkerPathEvaluation {
    fn complete_problem(self) -> Option<WalkerPathProblem> {
        self.problem.or_else(|| {
            (!self.selects_candidate).then_some(WalkerPathProblem {
                viability: WalkerPathViability::EmptyComponent,
                offset: self.empty_component_offset,
            })
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct WalkerPathSummary {
    all_empty_or_dot: bool,
    first_parent: Option<WalkerComponent>,
    first_dot: Option<WalkerComponent>,
    last_nonempty: Option<WalkerComponent>,
}

impl WalkerPathSummary {
    fn problem(self) -> Option<WalkerPathProblem> {
        if self.all_empty_or_dot {
            return Some(WalkerPathProblem {
                viability: WalkerPathViability::Root,
                offset: self.last_nonempty.and_then(|component| component.offset),
            });
        }
        if let Some(component) = self.first_parent {
            return Some(WalkerPathProblem {
                viability: WalkerPathViability::ParentComponent,
                offset: component.offset,
            });
        }
        if self
            .last_nonempty
            .is_some_and(|component| component.kind == WalkerComponentKind::Dot)
        {
            return Some(WalkerPathProblem {
                viability: WalkerPathViability::TrailingDot,
                offset: self.last_nonempty.and_then(|component| component.offset),
            });
        }
        self.first_dot.map(|component| WalkerPathProblem {
            viability: WalkerPathViability::DotComponent,
            offset: component.offset,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct WalkerPathShape {
    leading_dot_is_normalized: bool,
    components: Vec<WalkerComponent>,
}

impl WalkerPathShape {
    fn has_separator(&self) -> bool {
        self.components.len() > 1
    }

    fn is_empty(&self) -> bool {
        matches!(
            self.components.as_slice(),
            [WalkerComponent {
                kind: WalkerComponentKind::Empty,
                ..
            }]
        )
    }

    fn problem(&self) -> Option<WalkerPathProblem> {
        let mut state = WalkerPathState::new(self.leading_dot_is_normalized);
        state.append_shape(self);
        state.finish().complete_problem()
    }
}

#[derive(Debug, Clone, Copy)]
struct WalkerPathProblem {
    viability: WalkerPathViability,
    offset: Option<usize>,
}

/// The semantic summary of the alternatives the compiler actually built.
///
/// Brace expansion happens before this sees an alternative, while extglob
/// alternatives arrive through their compiled branch objects. The walker
/// policy therefore never needs a second source grammar.
fn walker_path_analysis(
    alternatives: &[CompiledAlternative],
    budget: &mut IrBudget,
) -> Result<(WalkerPathViability, Option<usize>), PatternError> {
    let mut first_problem = None;
    let mut saw_viable = false;
    for alternative in alternatives {
        match alternative.walker_path_problem(budget)? {
            None => saw_viable = true,
            Some(problem) if problem.viability == WalkerPathViability::ParentComponent => {
                return Ok((problem.viability, problem.offset));
            }
            Some(problem) => {
                first_problem.get_or_insert(problem);
            }
        };
    }
    if saw_viable {
        return Ok((WalkerPathViability::Viable, None));
    }
    match first_problem {
        Some(problem) => Ok((problem.viability, problem.offset)),
        None => Ok((WalkerPathViability::Viable, None)),
    }
}

impl CompiledAlternative {
    fn walker_path_problem(
        &self,
        budget: &mut IrBudget,
    ) -> Result<Option<WalkerPathProblem>, PatternError> {
        match &self.extglob {
            Some(program) => program.walker_path_problem(budget),
            None => Ok(self.walker_path_shape.problem()),
        }
    }
}

impl CompiledExtglob {
    fn walker_path_problem(
        &self,
        budget: &mut IrBudget,
    ) -> Result<Option<WalkerPathProblem>, PatternError> {
        let mut states = vec![WalkerPathState::new(self.leading_dot_is_normalized)];
        let mut index = 0;
        while let Some(step) = self.steps.get(index) {
            match step {
                ExtglobStep::Separator { .. } => {
                    let offset = self
                        .walker_source_provenance
                        .as_ref()
                        .and_then(|provenance| provenance.offset_at(index));
                    for state in &mut states {
                        state.separator(offset);
                    }
                    index += 1;
                }
                ExtglobStep::Byte(byte) => {
                    for state in &mut states {
                        state.literal(
                            *byte,
                            self.walker_source_provenance
                                .as_ref()
                                .and_then(|provenance| provenance.offset_at(index)),
                        );
                    }
                    index += 1;
                }
                ExtglobStep::Escape { .. } => {
                    for state in &mut states {
                        state.escaped();
                    }
                    index += 2;
                }
                ExtglobStep::Group(group) => {
                    states = self.groups[*group].apply_to(states, budget)?;
                    index = self.groups[*group].rest;
                }
                ExtglobStep::Star { next, .. } | ExtglobStep::Class { next, .. } => {
                    for state in &mut states {
                        state.wildcard();
                    }
                    index = *next;
                }
                ExtglobStep::Any | ExtglobStep::UnclosedGroup { .. } => {
                    for state in &mut states {
                        state.wildcard();
                    }
                    index += 1;
                }
                // A group interior is skipped by its group's `rest` jump.
                // Outside a group, a no-match step still occupies matcher
                // text, so keep the compiled program's following top-level
                // components visible to the walker analysis.
                ExtglobStep::NoMatch => {
                    for state in &mut states {
                        state.wildcard();
                    }
                    index += 1;
                }
            }
        }
        let mut first_problem = None;
        let mut first_unselectable = None;
        for state in states {
            let evaluation = state.finish();
            if !evaluation.selects_candidate {
                first_unselectable.get_or_insert(evaluation.empty_component_offset);
                continue;
            }
            let Some(problem) = evaluation.problem else {
                return Ok(None);
            };
            first_problem.get_or_insert(problem);
        }
        Ok(first_problem.or_else(|| {
            first_unselectable.map(|offset| WalkerPathProblem {
                viability: WalkerPathViability::EmptyComponent,
                offset,
            })
        }))
    }
}

impl ExtglobGroup {
    fn apply_to(
        &self,
        states: Vec<WalkerPathState>,
        budget: &mut IrBudget,
    ) -> Result<Vec<WalkerPathState>, PatternError> {
        match self.kind {
            ExtglobKind::Negated => {
                let mut states = states;
                for state in &mut states {
                    // A negated arm can generate arbitrary matcher text. A
                    // later outer `/.` or `/..` remains visible after this
                    // placeholder.
                    state.wildcard();
                }
                Ok(states)
            }
            ExtglobKind::Optional => {
                let mut output = states.clone();
                output.extend(self.exact_states(&states, budget)?);
                Ok(deduplicate_walker_states(output))
            }
            ExtglobKind::ZeroOrMore => self.fixed_point(states, false, budget),
            ExtglobKind::OneOrMore => self.fixed_point(states, true, budget),
            ExtglobKind::ExactlyOne => self.exact_states(&states, budget),
        }
    }

    fn fixed_point(
        &self,
        states: Vec<WalkerPathState>,
        require_one: bool,
        budget: &mut IrBudget,
    ) -> Result<Vec<WalkerPathState>, PatternError> {
        let mut seen = HashSet::new();
        let mut all = Vec::new();
        let mut frontier = if require_one {
            self.exact_states(&states, budget)?
        } else {
            states
        };
        frontier.retain(|state| seen.insert(state.clone()));
        all.extend(frontier.iter().cloned());
        while !frontier.is_empty() {
            let next = self.exact_states(&frontier, budget)?;
            frontier = next
                .into_iter()
                .filter(|state| seen.insert(state.clone()))
                .collect();
            all.extend(frontier.iter().cloned());
        }
        Ok(all)
    }

    fn exact_states(
        &self,
        states: &[WalkerPathState],
        budget: &mut IrBudget,
    ) -> Result<Vec<WalkerPathState>, PatternError> {
        let mut seen = HashSet::new();
        let mut output = Vec::new();
        for alternative in &self.alternatives {
            for arm in &alternative.compiled {
                for state in states {
                    // The state product is compiler-owned semantic IR. Charge
                    // every attempted transition before it can allocate, so
                    // an adversarial sequence of distinct groups is bounded
                    // by the same limit as the matcher IR.
                    budget.charge(1, self.start)?;
                    let mut state = state.clone();
                    if arm.walker_path_shape.has_separator() {
                        state.append_shape(&arm.walker_path_shape);
                    } else if arm.walker_path_shape.is_empty() {
                        // A syntactically empty arm (`?()`/`*()`) matches no
                        // matcher text and therefore leaves this canonical
                        // path state unchanged.
                    } else {
                        // A group that remains inside its containing component
                        // is matcher text (`@(..)`), not a path operation.
                        state.wildcard();
                    }
                    if seen.insert(state.clone()) {
                        output.push(state);
                    }
                }
            }
        }
        Ok(output)
    }
}

fn deduplicate_walker_states(states: Vec<WalkerPathState>) -> Vec<WalkerPathState> {
    let mut seen = HashSet::new();
    states
        .into_iter()
        .filter(|state| seen.insert(state.clone()))
        .collect()
}

fn parse_posix_class(name: &[u8]) -> Option<PosixClass> {
    match name {
        b"alnum" => Some(PosixClass::Alnum),
        b"alpha" => Some(PosixClass::Alpha),
        b"ascii" => Some(PosixClass::Ascii),
        b"blank" => Some(PosixClass::Blank),
        b"cntrl" => Some(PosixClass::Cntrl),
        b"digit" => Some(PosixClass::Digit),
        b"graph" => Some(PosixClass::Graph),
        b"lower" => Some(PosixClass::Lower),
        b"print" => Some(PosixClass::Print),
        b"punct" => Some(PosixClass::Punct),
        b"space" => Some(PosixClass::Space),
        b"upper" => Some(PosixClass::Upper),
        b"word" => Some(PosixClass::Word),
        b"xdigit" => Some(PosixClass::Xdigit),
        _ => None,
    }
}

fn class_members(values: Vec<ClassValue>) -> Vec<ClassMember> {
    let mut members = Vec::new();
    let mut index = 0;
    while index < values.len() {
        // A range needs an unescaped `-` with an endpoint before the closing
        // bracket. `[a\-z]`, `[a-]` and `[-a]` therefore stay literal, while an
        // escaped byte may still bound a range as in `[\--0]` or `[a-\-]`.
        if index + 2 < values.len() && values[index + 1].is_range_separator() {
            members.push(ClassMember::Range(
                values[index].byte,
                values[index + 2].byte,
            ));
            index += 3;
        } else {
            members.push(ClassMember::Byte(values[index].byte));
            index += 1;
        }
    }
    members
}

/// Ends the current literal run. The token gets an exact copy and the run's
/// buffer is kept for the next one, so a literal retains no spare capacity.
fn flush_literals(tokens: &mut Vec<Token>, literals: &mut Vec<u8>) {
    if !literals.is_empty() {
        tokens.push(Token::Literal(literals.clone()));
        literals.clear();
    }
}

/// Alternatives one pattern may expand to before it is rejected.
///
/// Brace groups multiply, so expansion is exponential in the number of groups:
/// ten nine-way groups fit in 100 bytes and ask for 3.5 billion alternatives.
/// Neither zlob 1.6.3 nor glibc `GLOB_BRACE` bounds this, so a pattern that
/// small takes them minutes or kills them; ferralk rejects it instead, which is
/// a deliberate difference recorded in the compatibility guide.
///
/// The limit is the fuzz harness's long-standing cap. It is far past any real
/// pattern — a language's extension list is a handful of alternatives — and
/// every alternative is a token program [`Pattern::is_match`] tries in turn, so
/// a pattern needing more is a matching problem even where it fits in memory.
const MAX_BRACE_ALTERNATIVES: usize = 1 << 12;

/// Bytes brace expansion may write before a pattern is rejected.
///
/// The alternative count alone does not bound the work. Expansion rewrites the
/// whole pattern once per group it resolves, so a pattern of many one-way
/// groups costs the square of its length while expanding to a single
/// alternative: `{a}` repeated 200,000 times is 600 KB and took 11.8 s. And a
/// pattern inside the alternative budget still materializes that budget times
/// its own length, so 4096 alternatives of a 100 KB pattern is 400 MB and a
/// second, however few groups produced them.
///
/// Counting the bytes written bounds both, because both are the same quantity:
/// what expansion copies. 64 MiB is roughly 10 ms of copying, and orders of
/// magnitude past any real pattern — a language's extension list against a path
/// glob is a few hundred bytes, and even the largest expansion the alternative
/// budget admits is under a megabyte at a realistic pattern length.
const MAX_BRACE_EXPANSION_BYTES: usize = 1 << 26;

/// Compiled units one pattern may build before it is rejected.
///
/// The brace budgets bound the expanded pattern *text*: how many alternatives
/// and how many bytes they add up to. Neither sees what compiling that text
/// costs, and the compiled form is far larger than its source — a token per
/// wildcard byte, and for an extglob a program step per byte offset of the
/// alternative. Total compiled size is therefore
/// *(units per alternative) x (alternatives)*, a third dimension the byte
/// budget cannot express: 20 MB of expanded text became 1.9 GB of compiled
/// program, from a 5 KB pattern that sat inside both other budgets.
///
/// A unit is one token, one program step, one class member, or one attempted
/// compiler-derived walker-state transition — a class token owns its member
/// list, so it is charged for both. Affix tries charge their temporary builder
/// and flattened node/edge representation together at the construction peak.
/// The transition charge bounds the product of exact extglob arms without
/// adding a separate viability limit. [`Token`] is 32 bytes and
/// [`ExtglobStep`] is 40, pinned by a test so this arithmetic cannot go stale,
/// which puts the ceiling around 40 MB of compiled program and tens of
/// milliseconds of work. That is orders of magnitude past any real pattern:
/// a language's extension list against a path glob is a few hundred units.
const MAX_COMPILED_IR_UNITS: usize = 1 << 20;

/// Tracks compiled units across every alternative of one [`Pattern::compile`].
///
/// Charged before each allocation rather than after, so a pattern that would
/// pass the ceiling stops there instead of building its way past it first.
/// The one message the compiled-size ceiling reports.
const TOO_MUCH_COMPILED_IR: &str = "pattern compiles to too much";

/// Retained heap-byte ceiling for one multi-alternative compiled pattern
/// (40 MiB, matching the documented compiled-size ceiling in
/// `docs/compatibility-guide.md`). Enforced alongside [`MAX_COMPILED_IR_UNITS`]
/// so retained literal/prefilter/token storage is bounded without stealing IR
/// unit budget from sweep-backed alternatives (#495).
const MAX_RETAINED_PATTERN_BYTES: usize = 40 * 1024 * 1024;

struct IrBudget {
    remaining: usize,
    remaining_retained_bytes: usize,
}

impl IrBudget {
    const fn new() -> Self {
        Self {
            remaining: MAX_COMPILED_IR_UNITS,
            remaining_retained_bytes: MAX_RETAINED_PATTERN_BYTES,
        }
    }

    /// Charges `units`, reporting the pattern as too large if they run out.
    ///
    /// `offset` is where to point the error; every caller here compiles one
    /// alternative of a pattern the caller never wrote, so it points at the
    /// start of what they did write.
    fn charge(&mut self, units: usize, offset: usize) -> Result<(), PatternError> {
        match self.remaining.checked_sub(units) {
            Some(remaining) => {
                self.remaining = remaining;
                Ok(())
            }
            None => Err(PatternError {
                offset,
                message: TOO_MUCH_COMPILED_IR,
            }),
        }
    }

    /// Rejects a proven lower bound without consuming it.
    ///
    /// The ordinary compiler still performs the exact charge as structures
    /// are built. This look-ahead only avoids starting work that cannot fit.
    fn ensure(&self, units: usize, offset: usize) -> Result<(), PatternError> {
        if units <= self.remaining {
            Ok(())
        } else {
            Err(PatternError {
                offset,
                message: TOO_MUCH_COMPILED_IR,
            })
        }
    }

    fn ensure_retained_bytes(&self, bytes: usize, offset: usize) -> Result<(), PatternError> {
        if bytes <= self.remaining_retained_bytes {
            Ok(())
        } else {
            Err(PatternError {
                offset,
                message: TOO_MUCH_COMPILED_IR,
            })
        }
    }

    fn charge_retained_bytes(&mut self, bytes: usize, offset: usize) -> Result<(), PatternError> {
        match self.remaining_retained_bytes.checked_sub(bytes) {
            Some(remaining) => {
                self.remaining_retained_bytes = remaining;
                Ok(())
            }
            None => Err(PatternError {
                offset,
                message: TOO_MUCH_COMPILED_IR,
            }),
        }
    }
}

/// Bounds source-span metadata independently from matcher IR.
///
/// `SourceSpan` is three machine words. Capping the count at the established
/// brace-copy byte ceiling keeps provenance below that ceiling while charging
/// every materialized span before its vector allocation.
struct ProvenanceBudget {
    remaining: usize,
}

impl ProvenanceBudget {
    const fn new() -> Self {
        Self {
            remaining: MAX_BRACE_EXPANSION_BYTES / std::mem::size_of::<SourceSpan>(),
        }
    }

    fn charge(&mut self, spans: usize, offset: usize) -> Result<(), PatternError> {
        match self.remaining.checked_sub(spans) {
            Some(remaining) => {
                self.remaining = remaining;
                Ok(())
            }
            None => Err(PatternError {
                offset,
                message: "brace provenance is too large",
            }),
        }
    }
}

/// The bytes a [`Pattern`] matches for a path: [`OsStr::as_encoded_bytes`] of
/// anything that is `AsRef<OsStr>`.
///
/// Every matching entry point takes `impl AsRef<[u8]>`, which a [`&str`](str)
/// already is and a [`Path`](std::path::Path) is not. This is the one
/// conversion for the rest, so `path.as_os_str().as_encoded_bytes()` need not
/// be spelled out at every call site. It borrows and never converts: raw
/// filesystem bytes on Unix and lossless WTF-8 on Windows (ADR-0005), where
/// the matcher reads both `/` and `\` as a separator. It works with
/// [`Pattern::is_match`], [`Pattern::is_match_path`],
/// [`Pattern::is_match_glob_path`], and, mapped over a list, with
/// [`Pattern::filter_path_indices`].
///
/// A pattern is relative to wherever it is anchored, so pass the part of the
/// path below that directory; a path that `ferralk` walked has it as
/// `WalkEntry::relative_path`.
///
/// ```
/// use std::path::{Path, PathBuf};
///
/// use ferralk_glob::{Pattern, PatternOptions, path_bytes};
///
/// let pattern = Pattern::compile("src/**/*.rs", PatternOptions::walker())?;
///
/// let path: PathBuf = ["src", "bin", "main.rs"].iter().collect();
/// assert!(pattern.is_match_glob_path(path_bytes(&path)));
///
/// let root = Path::new("/work/project");
/// let joined = root.join(&path);
/// let relative = joined.strip_prefix(root).expect("joined below the root");
/// assert!(pattern.is_match_glob_path(path_bytes(relative)));
/// // The joined path starts with `/work`, which `src/**` does not.
/// assert!(!pattern.is_match_glob_path(path_bytes(&joined)));
///
/// // A list of paths, answered by index into it.
/// let paths = [Path::new("src/lib.rs"), Path::new("README.md")];
/// let selected = pattern.filter_path_indices(paths.iter().map(path_bytes));
/// assert_eq!(selected, [0]);
/// # Ok::<(), ferralk_glob::PatternError>(())
/// ```
///
/// [`OsStr::as_encoded_bytes`]: std::ffi::OsStr::as_encoded_bytes
#[must_use]
pub fn path_bytes(path: &(impl AsRef<OsStr> + ?Sized)) -> &[u8] {
    path.as_ref().as_encoded_bytes()
}

/// Whether `byte` is pattern syntax under some [`PatternOptions`]: a wildcard,
/// a class bracket, a brace or its separator, an extglob operator, parenthesis
/// or separator, or the escape character itself.
const fn is_syntax_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'\\'
            | b'*'
            | b'?'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'('
            | b')'
            | b','
            | b'|'
            | b'!'
            | b'@'
            | b'+'
    )
}

/// Escapes `text` so that it compiles to a pattern matching exactly `text`.
///
/// This is the counterpart of `globset::escape`, for user input or a path that
/// has to become part of a pattern without its bytes turning into syntax. Every
/// byte that is syntax under any [`PatternOptions`] gets a `\` in front of it:
/// `\ * ? [ ] { } ( ) , | ! @ +`. The result is therefore literal whichever
/// options it is compiled with, as long as
/// [`escape`](PatternOptions::escape) stays enabled, which it is in both
/// [`PatternOptions::default`] and [`PatternOptions::walker`].
///
/// Only syntax is escaped, not path structure. `/` still separates
/// components, and a leading `./` is still the one the path entry points
/// ignore, so the escaped text matches the way the unescaped path would under
/// that entry point. Case folding, when enabled, applies to the escaped bytes
/// as to any literal. Because `,` and `|` are escaped too, the result stays
/// literal inside a brace or extglob alternative, and a leading `!` never
/// reads as a walker negation.
///
/// ```
/// use ferralk_glob::{Pattern, PatternOptions, escape};
///
/// let name = "notes [draft] {v2}.md";
/// let escaped = escape(name);
/// assert_eq!(escaped, br"notes \[draft\] \{v2\}.md");
///
/// let pattern = Pattern::compile(&escaped, PatternOptions::walker())?;
/// assert!(pattern.is_match_glob_path(name));
/// assert!(!pattern.is_match_glob_path("notes d {v2}.md"));
///
/// // Escaped text composes with pattern syntax around it.
/// let mut below = escape("/work/app[1]");
/// below.extend_from_slice(b"/**/*.{ts,tsx}");
/// let pattern = Pattern::compile(&below, PatternOptions::walker())?;
/// assert!(pattern.is_match_glob_path("/work/app[1]/src/main.tsx"));
/// assert!(!pattern.is_match_glob_path("/work/app1/src/main.tsx"));
/// # Ok::<(), ferralk_glob::PatternError>(())
/// ```
///
/// [`escape_str`] does the same for a `&str` and returns a `String`.
#[must_use]
pub fn escape(text: impl AsRef<[u8]>) -> Vec<u8> {
    let text = text.as_ref();
    let mut escaped = Vec::with_capacity(text.len());
    for &byte in text {
        if is_syntax_byte(byte) {
            escaped.push(b'\\');
        }
        escaped.push(byte);
    }
    escaped
}

/// [`escape`] for UTF-8 text, returning a `String`.
///
/// Every escaped byte is ASCII, so the result is valid UTF-8 whenever the
/// input is, and it can be formatted into a larger pattern directly.
///
/// ```
/// use ferralk_glob::{Pattern, PatternOptions, escape_str};
///
/// let root = "/work/brace{project}";
/// let pattern = format!("{}/**/*.ts", escape_str(root));
/// assert_eq!(pattern, r"/work/brace\{project\}/**/*.ts");
///
/// let pattern = Pattern::compile(&pattern, PatternOptions::walker())?;
/// assert!(pattern.is_match_glob_path("/work/brace{project}/src/main.ts"));
/// assert!(!pattern.is_match_glob_path("/work/braceproject/src/main.ts"));
/// # Ok::<(), ferralk_glob::PatternError>(())
/// ```
#[must_use]
pub fn escape_str(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if u8::try_from(character).is_ok_and(is_syntax_byte) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Expands brace alternatives into the plain patterns a pattern stands for.
///
/// [`Pattern::compile`] expands braces before it compiles anything; this
/// exposes the same expansion so callers can reason about the alternatives
/// themselves. Deriving a prefilter is the motivating case: every alternative
/// of `**/*.{ts,tsx}` ends in a literal extension, so a caller can prefilter on
/// `ts` and `tsx` without giving up on brace patterns.
///
/// Without [`PatternOptions::braces`] a pattern stands for itself and the
/// result is the input unchanged. Alternatives come back in the order the
/// expansion produces them, and a pattern always expands to at least one.
///
/// ```
/// use ferralk_glob::{PatternOptions, expand_braces};
///
/// let options = PatternOptions::default().braces(true);
/// let alternatives = expand_braces("**/*.{ts,tsx}", options)?;
/// assert_eq!(alternatives, [b"**/*.ts".to_vec(), b"**/*.tsx".to_vec()]);
/// # Ok::<(), ferralk_glob::PatternError>(())
/// ```
///
/// # Errors
///
/// Reports a pattern that exceeds the documented brace-expansion limits, so a
/// caller never has to assume expansion succeeds. Glob syntax is not checked
/// here: an unclosed brace is ordinary text, the way [`Pattern::compile`]
/// treats it, and anything else malformed is reported when the alternative it
/// belongs to is compiled.
pub fn expand_braces(
    pattern: impl AsRef<[u8]>,
    options: PatternOptions,
) -> Result<Vec<Vec<u8>>, PatternError> {
    let pattern = pattern.as_ref();
    if !options.braces {
        return Ok(vec![pattern.to_vec()]);
    }
    let mut provenance_budget = ProvenanceBudget::new();
    Ok(expand_brace_alternatives_with_provenance(
        pattern,
        None,
        options.escape,
        &mut provenance_budget,
    )?
    .into_iter()
    .map(|alternative| alternative.bytes)
    .collect())
}

/// One brace alternative and the source offset of its first byte.
type AlternativeWithFirstOffset = (Vec<u8>, Option<usize>);

/// [`expand_braces`], with each alternative paired with the source offset of
/// its first byte, or `None` for an empty alternative.
///
/// This is provenance for path-consuming embeddings: a rule about how an
/// alternative begins can then be reported at the byte the caller wrote
/// instead of at an offset into an expansion the caller never saw.
#[doc(hidden)]
pub fn expand_braces_with_first_offsets(
    pattern: impl AsRef<[u8]>,
    options: PatternOptions,
) -> Result<Vec<AlternativeWithFirstOffset>, PatternError> {
    let pattern = pattern.as_ref();
    let first = (!pattern.is_empty()).then_some(0);
    if !options.braces {
        return Ok(vec![(pattern.to_vec(), first)]);
    }
    let source = SourceProvenance::Contiguous { source_start: 0 };
    let mut provenance_budget = ProvenanceBudget::new();
    Ok(expand_brace_alternatives_with_provenance(
        pattern,
        Some(&source),
        options.escape,
        &mut provenance_budget,
    )?
    .into_iter()
    .map(|alternative| {
        let first = if alternative.bytes.is_empty() {
            None
        } else {
            alternative
                .source_provenance
                .as_ref()
                .and_then(|provenance| provenance.offset_at(0))
        };
        (alternative.bytes, first)
    })
    .collect())
}

/// A contiguous output range that came from a contiguous source range.
///
/// Brace expansion removes delimiters and alternatives but never manufactures
/// matcher bytes, so a handful of these spans preserves exact offsets without
/// an `usize` allocation per source byte.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceSpan {
    output_start: usize,
    source_start: usize,
    len: usize,
}

/// Original byte provenance for a compiled byte sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SourceProvenance {
    Contiguous { source_start: usize },
    Spans(Vec<SourceSpan>),
}

impl SourceProvenance {
    fn offset_at(&self, index: usize) -> Option<usize> {
        match self {
            Self::Contiguous { source_start } => Some(source_start + index),
            Self::Spans(spans) => spans.iter().find_map(|span| {
                (index >= span.output_start && index - span.output_start < span.len)
                    .then_some(span.source_start + index - span.output_start)
            }),
        }
    }

    fn span_count_in(&self, start: usize, end: usize) -> usize {
        match self {
            Self::Contiguous { .. } => usize::from(start < end),
            Self::Spans(spans) => spans
                .iter()
                .filter(|span| span.output_start < end && span.output_start + span.len > start)
                .count(),
        }
    }

    fn append_range(
        &self,
        start: usize,
        end: usize,
        output_start: usize,
        target: &mut Vec<SourceSpan>,
    ) {
        if start == end {
            return;
        }
        match self {
            Self::Contiguous { source_start } => target.push(SourceSpan {
                output_start,
                source_start: source_start + start,
                len: end - start,
            }),
            Self::Spans(spans) => {
                for span in spans {
                    let span_end = span.output_start + span.len;
                    let overlap_start = span.output_start.max(start);
                    let overlap_end = span_end.min(end);
                    if overlap_start < overlap_end {
                        target.push(SourceSpan {
                            output_start: output_start + overlap_start - start,
                            source_start: span.source_start + overlap_start - span.output_start,
                            len: overlap_end - overlap_start,
                        });
                    }
                }
            }
        }
    }

    fn slice(
        &self,
        start: usize,
        end: usize,
        provenance_budget: &mut ProvenanceBudget,
        error_offset: usize,
    ) -> Result<Self, PatternError> {
        if let Self::Contiguous { source_start } = self {
            return Ok(Self::Contiguous {
                source_start: source_start + start,
            });
        }
        let span_count = self.span_count_in(start, end);
        provenance_budget.charge(span_count, error_offset)?;
        let mut spans = Vec::with_capacity(span_count);
        self.append_range(start, end, 0, &mut spans);
        Ok(Self::Spans(spans))
    }
}

/// One brace-expanded byte sequence and its compact source provenance.
struct BraceExpansion {
    bytes: Vec<u8>,
    source_provenance: Option<SourceProvenance>,
}

#[derive(Clone, Copy)]
struct BraceExpansionSummary {
    source_length: usize,
    alternatives: usize,
    minimum_length: usize,
    final_length_sum: usize,
    nodes: usize,
    written: usize,
}

impl BraceExpansionSummary {
    const fn literal(length: usize) -> Self {
        Self {
            source_length: length,
            alternatives: 1,
            minimum_length: length,
            final_length_sum: length,
            nodes: 1,
            written: length,
        }
    }

    /// Symbolically expands `self` before `suffix`, matching the work-list's
    /// left-to-right order without materializing either expansion tree.
    fn concat(self, suffix: Self) -> Self {
        let suffix_descendants = suffix.nodes.saturating_sub(1);
        Self {
            source_length: self.source_length.saturating_add(suffix.source_length),
            alternatives: self.alternatives.saturating_mul(suffix.alternatives),
            minimum_length: self.minimum_length.saturating_add(suffix.minimum_length),
            final_length_sum: suffix
                .alternatives
                .saturating_mul(self.final_length_sum)
                .saturating_add(self.alternatives.saturating_mul(suffix.final_length_sum)),
            nodes: self
                .nodes
                .saturating_add(self.alternatives.saturating_mul(suffix_descendants)),
            written: self
                .written
                .saturating_add(self.nodes.saturating_mul(suffix.source_length))
                .saturating_add(suffix_descendants.saturating_mul(self.final_length_sum))
                .saturating_add(
                    self.alternatives
                        .saturating_mul(suffix.written.saturating_sub(suffix.source_length)),
                ),
        }
    }
}

/// Refuses a source whose syntax proves that every brace expansion together
/// cannot fit the compiled-IR budget.
///
/// The summary computes only the number of final alternatives and the shortest
/// final byte length. Syntax outside every brace arm survives in every result,
/// so an unconditional extglob opener proves one step table of at least that
/// length per alternative, while unconditional question marks each prove one
/// ordinary token. Ambiguous, very deep, or over-the-brace-count-limit sources
/// simply defer to the existing expansion and exact compiler budgets.
fn ensure_source_brace_compiled_ir_lower_bound(
    pattern: &[u8],
    options: PatternOptions,
    budget: &IrBudget,
    provenance_budget: &ProvenanceBudget,
) -> Result<(), PatternError> {
    // Class parsing is the only fallible syntax pass after brace expansion.
    // Only look ahead when every class is valid and brace expansion cannot
    // split or rewrite one, preserving parser-error precedence and offsets.
    if !source_classes_are_stable_and_valid(pattern, options.escape) {
        return Ok(());
    }
    let Some(summary) = brace_expansion_summary(pattern, options.escape, 0) else {
        return Ok(());
    };
    if summary.alternatives > MAX_BRACE_ALTERNATIVES {
        return Ok(());
    }
    // Preserve the earlier expansion-byte error whenever both budgets would
    // fail. `written` models that exact work-list counter symbolically.
    if summary.written > MAX_BRACE_EXPANSION_BYTES {
        return Ok(());
    }
    // Selecting one brace arm can split at most two existing provenance
    // spans. Bound every generated node by the deepest possible source path;
    // if that cannot fit, preserve the provenance-budget error as well.
    let brace_openers = unescaped_brace_openers(pattern, options.escape);
    let spans_per_node = 1_usize.saturating_add(brace_openers.saturating_mul(2));
    let provenance_upper = summary
        .nodes
        .saturating_sub(1)
        .saturating_mul(spans_per_node);
    if provenance_upper > provenance_budget.remaining {
        return Ok(());
    }
    let Some((question_marks, has_extglob)) = unconditional_compiled_units(pattern, options) else {
        return Ok(());
    };
    let per_alternative = question_marks.saturating_add(if has_extglob {
        summary.minimum_length
    } else {
        0
    });
    budget.ensure(summary.alternatives.saturating_mul(per_alternative), 0)
}

fn unescaped_brace_openers(pattern: &[u8], escapes: bool) -> usize {
    let mut openers = 0_usize;
    let mut index = 0;
    while index < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += usize::from(index + 1 < pattern.len()) + 1;
        } else {
            openers += usize::from(pattern[index] == b'{');
            index += 1;
        }
    }
    openers
}

fn source_classes_are_stable_and_valid(pattern: &[u8], escapes: bool) -> bool {
    let mut brace_depth = 0_usize;
    let mut index = 0;
    while index < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += usize::from(index + 1 < pattern.len()) + 1;
            continue;
        }
        match pattern[index] {
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b'[' => {
                let Ok((_, next)) = parse_class(pattern, index, escapes) else {
                    return false;
                };
                let mut class_index = index + 1;
                while class_index < next {
                    if escapes && pattern[class_index] == b'\\' {
                        class_index += usize::from(class_index + 1 < next) + 1;
                        continue;
                    }
                    if matches!(pattern[class_index], b'{' | b'}')
                        || (brace_depth > 0 && pattern[class_index] == b',')
                    {
                        return false;
                    }
                    class_index += 1;
                }
                index = next;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    true
}

fn brace_expansion_summary(
    pattern: &[u8],
    escapes: bool,
    depth: usize,
) -> Option<BraceExpansionSummary> {
    const MAX_SUMMARY_DEPTH: usize = 128;
    if depth > MAX_SUMMARY_DEPTH {
        return None;
    }
    let Some(open) = first_unescaped_brace(pattern, escapes) else {
        return Some(BraceExpansionSummary::literal(pattern.len()));
    };
    let Some(close) = matching_brace(pattern, open, escapes) else {
        return Some(BraceExpansionSummary::literal(pattern.len()));
    };

    let content = &pattern[open + 1..close];
    let mut group_alternatives = 0_usize;
    let mut group_minimum = usize::MAX;
    let mut group_final_length_sum = 0_usize;
    let mut group_nodes = 1_usize;
    let mut group_written = close - open + 1;
    for range in split_brace_alternatives(content, escapes) {
        let arm = brace_expansion_summary(&content[range], escapes, depth + 1)?;
        group_alternatives = group_alternatives.saturating_add(arm.alternatives);
        group_minimum = group_minimum.min(arm.minimum_length);
        group_final_length_sum = group_final_length_sum.saturating_add(arm.final_length_sum);
        group_nodes = group_nodes.saturating_add(arm.nodes);
        group_written = group_written.saturating_add(arm.written);
    }
    let prefix = BraceExpansionSummary::literal(open);
    let group = BraceExpansionSummary {
        source_length: close - open + 1,
        alternatives: group_alternatives,
        minimum_length: group_minimum,
        final_length_sum: group_final_length_sum,
        nodes: group_nodes,
        written: group_written,
    };
    let suffix = brace_expansion_summary(&pattern[close + 1..], escapes, depth + 1)?;
    Some(prefix.concat(group).concat(suffix))
}

/// Maximum live brace nesting tracked by [`unconditional_compiled_units`].
///
/// Deeper nesting while more than this many closing braces remain defers to the
/// exact expansion and compiler budgets so the preflight stays within a fixed
/// scratch cap.
const MAX_UNCONDITIONAL_BRACE_DEPTH: usize = 1_024;

#[cfg(test)]
thread_local! {
    static UNCONDITIONAL_BRACE_PEAK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static UNCONDITIONAL_BRACE_SCANNED_BYTES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn unconditional_brace_peak() -> usize {
    UNCONDITIONAL_BRACE_PEAK.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_unconditional_brace_scanned_bytes() {
    UNCONDITIONAL_BRACE_SCANNED_BYTES.with(|cell| cell.set(0));
}

#[cfg(test)]
fn unconditional_brace_scanned_bytes() -> usize {
    UNCONDITIONAL_BRACE_SCANNED_BYTES.with(std::cell::Cell::get)
}

/// Counts compiler units whose syntax is outside every matched brace group.
///
/// Keeps scratch memory bounded by `min(remaining_closing_braces,
/// MAX_UNCONDITIONAL_BRACE_DEPTH)`: oldest open braces beyond the remaining
/// closing-brace count can never match and are discarded, and live nesting
/// beyond the cap returns `None` so the caller defers to the exact compiler.
fn unconditional_compiled_units(pattern: &[u8], options: PatternOptions) -> Option<(usize, bool)> {
    let mut remaining_closing_braces = memchr::memchr_iter(b'}', pattern).count();
    let mut question_marks = 0_usize;
    let mut has_extglob = false;
    let mut open_braces = std::collections::VecDeque::new();
    let mut saw_unclosed_class = false;
    let mut skip_class_until = 0_usize;
    #[cfg(test)]
    let mut peak = 0_usize;
    #[cfg(test)]
    let mut scanned = 0_usize;
    let mut index = 0;
    while index < pattern.len() {
        #[cfg(test)]
        {
            scanned += 1;
        }
        if options.escape && pattern[index] == b'\\' {
            if pattern.get(index + 1) == Some(&b'}') {
                remaining_closing_braces -= 1;
                if open_braces.len() > remaining_closing_braces {
                    open_braces.pop_front();
                }
            }
            index += usize::from(index + 1 < pattern.len()) + 1;
            continue;
        }
        if !saw_unclosed_class && index >= skip_class_until && pattern[index] == b'[' {
            if let Some((next, has_unescaped_brace, skipped_closing_braces)) =
                scan_class_end(pattern, index, options.escape)
            {
                if !has_unescaped_brace {
                    remaining_closing_braces -= skipped_closing_braces;
                    while open_braces.len() > remaining_closing_braces {
                        open_braces.pop_front();
                    }
                    index = next;
                    continue;
                }
                // Braces expand before character classes are parsed, so a class
                // holding an unescaped `{` or `}` must still be scanned for
                // brace nesting, while suppressing nested `[` rescans until its
                // closing `]` keeps worst-case scanning strictly O(N).
                skip_class_until = next;
            } else {
                saw_unclosed_class = true;
            }
        }
        if remaining_closing_braces > 0 {
            match pattern[index] {
                b'{' => {
                    while open_braces.len() >= remaining_closing_braces {
                        open_braces.pop_front();
                    }
                    if open_braces.len() == MAX_UNCONDITIONAL_BRACE_DEPTH {
                        #[cfg(test)]
                        {
                            UNCONDITIONAL_BRACE_PEAK.with(|cell| cell.set(peak));
                            UNCONDITIONAL_BRACE_SCANNED_BYTES
                                .with(|cell| cell.set(cell.get() + scanned));
                        }
                        return None;
                    }
                    open_braces.push_back((question_marks, has_extglob));
                    #[cfg(test)]
                    {
                        peak = peak.max(open_braces.len());
                    }
                    index += 1;
                    continue;
                }
                b'}' => {
                    remaining_closing_braces -= 1;
                    if let Some((saved_question_marks, saved_has_extglob)) = open_braces.pop_back()
                    {
                        question_marks = saved_question_marks;
                        has_extglob = saved_has_extglob;
                    }
                    index += 1;
                    continue;
                }
                _ => {}
            }
        }
        question_marks += usize::from(pattern[index] == b'?');
        has_extglob |= options.extglob && detect_extglob_at(pattern, index).is_some();
        index += 1;
    }
    #[cfg(test)]
    {
        UNCONDITIONAL_BRACE_PEAK.with(|cell| cell.set(peak));
        UNCONDITIONAL_BRACE_SCANNED_BYTES.with(|cell| cell.set(cell.get() + scanned));
    }
    Some((question_marks, has_extglob))
}

/// Rejects brace expansions whose unavoidable compiler structures exceed the
/// remaining IR budget before building any alternative's matcher program.
///
/// This deliberately remains a lower bound. Every valid class contributes its
/// token and owned members, every ordinary `?` contributes a token, and an
/// extglob-enabled alternative with extglob syntax always allocates one step
/// slot per byte. Literal runs and the derived fast paths are left uncounted,
/// so passing this guard never promises that the exact compiler will fit.
/// Returning `None` for an invalid class preserves the parser error that the
/// ordinary alternative-by-alternative compiler reports.
fn ensure_brace_compiled_ir_lower_bound(
    alternatives: &[BraceExpansion],
    options: PatternOptions,
    budget: &IrBudget,
) -> Result<(), PatternError> {
    let mut total = 0_usize;
    for alternative in alternatives {
        let Some(units) = compiled_ir_lower_bound(&alternative.bytes, options) else {
            return Ok(());
        };
        total = total.saturating_add(units);
        if total > budget.remaining {
            return budget.ensure(total, 0);
        }
    }
    budget.ensure(total, 0)
}

fn compiled_ir_lower_bound(pattern: &[u8], options: PatternOptions) -> Option<usize> {
    let mut units = if options.extglob && contains_extglob(pattern, options.escape) {
        pattern.len()
    } else {
        0
    };
    let mut index = 0;
    while index < pattern.len() {
        match pattern[index] {
            b'\\' if options.escape => index += usize::from(index + 1 < pattern.len()) + 1,
            b'[' => {
                let (class, next) = parse_class(pattern, index, options.escape).ok()?;
                units = units.saturating_add(1 + class.members.len());
                index = next;
            }
            b'?' => {
                units = units.saturating_add(1);
                index += 1;
            }
            _ => index += 1,
        }
    }
    Some(units)
}

/// Expands brace groups into one pattern per combination.
///
/// The work list replaces recursion, which used to be one frame per brace group
/// and overflowed the stack on a pattern of many small groups. Popping from the
/// back with the alternatives pushed in reverse keeps the original order: every
/// combination of the first group is emitted before the second group's.
///
/// `expanded.len() + pending.len()` only ever grows and every pending entry
/// yields at least one alternative, so it is a running lower bound on the final
/// count. Checking it before each push therefore rejects exactly the patterns
/// whose expansion would exceed the documented alternative limit, and bounds
/// memory rather than only the result.
///
/// The bytes written are counted against the documented expansion-byte limit
/// the same way, and that is what bounds the time: rewriting the whole pattern
/// per group is quadratic in its length even where it expands to one
/// alternative. The running total only grows too, so stopping at the first
/// write that exceeds the limit rejects exactly the patterns whose finished
/// expansion would.
fn expand_brace_alternatives_with_provenance(
    pattern: &[u8],
    source_provenance: Option<&SourceProvenance>,
    escapes: bool,
    provenance_budget: &mut ProvenanceBudget,
) -> Result<Vec<BraceExpansion>, PatternError> {
    let Some(first_open) = first_unescaped_brace(pattern, escapes) else {
        return Ok(vec![BraceExpansion {
            bytes: pattern.to_vec(),
            source_provenance: source_provenance.cloned(),
        }]);
    };

    let mut expanded = Vec::new();
    let mut pending = vec![BraceExpansion {
        bytes: pattern.to_vec(),
        source_provenance: source_provenance.cloned(),
    }];
    let mut written = pattern.len();
    while let Some(current) = pending.pop() {
        let Some(open) = first_unescaped_brace(&current.bytes, escapes) else {
            expanded.push(current);
            continue;
        };
        let Some(close) = matching_brace(&current.bytes, open, escapes) else {
            // zlob treats an unmatched brace as ordinary text.
            expanded.push(current);
            continue;
        };

        let alternatives = split_brace_alternatives(&current.bytes[open + 1..close], escapes);
        for range in alternatives.iter().rev() {
            let alternative = &current.bytes[open + 1 + range.start..open + 1 + range.end];
            if expanded.len() + pending.len() >= MAX_BRACE_ALTERNATIVES {
                return Err(PatternError {
                    // Offsets into a partly expanded pattern would not point
                    // into the caller's, so report where its expansion starts.
                    offset: first_open,
                    message: "too many brace alternatives",
                });
            }
            let length = open + alternative.len() + current.bytes.len() - close - 1;
            written = written.saturating_add(length);
            if written > MAX_BRACE_EXPANSION_BYTES {
                return Err(PatternError {
                    offset: first_open,
                    message: "brace expansion is too large",
                });
            }
            let mut bytes = Vec::with_capacity(length);
            bytes.extend_from_slice(&current.bytes[..open]);
            bytes.extend_from_slice(alternative);
            bytes.extend_from_slice(&current.bytes[close + 1..]);
            let source_provenance = current
                .source_provenance
                .as_ref()
                .map(|provenance| {
                    let alternative_start = open + 1 + range.start;
                    let span_count = provenance.span_count_in(0, open)
                        + provenance.span_count_in(
                            alternative_start,
                            alternative_start + alternative.len(),
                        )
                        + provenance.span_count_in(close + 1, current.bytes.len());
                    // This charges compact provenance before it is materialized.
                    // Its bounded metadata budget prevents expansion from turning
                    // a small matcher into an unbounded offset side allocation.
                    provenance_budget.charge(span_count, first_open)?;
                    let mut spans = Vec::with_capacity(span_count);
                    provenance.append_range(0, open, 0, &mut spans);
                    provenance.append_range(
                        alternative_start,
                        alternative_start + alternative.len(),
                        open,
                        &mut spans,
                    );
                    provenance.append_range(
                        close + 1,
                        current.bytes.len(),
                        open + alternative.len(),
                        &mut spans,
                    );
                    Ok::<_, PatternError>(SourceProvenance::Spans(spans))
                })
                .transpose()?;
            pending.push(BraceExpansion {
                bytes,
                source_provenance,
            });
        }
    }
    Ok(expanded)
}

#[cfg(test)]
fn expand_brace_alternatives(pattern: &[u8], escapes: bool) -> Result<Vec<Vec<u8>>, PatternError> {
    let mut provenance_budget = ProvenanceBudget::new();
    Ok(
        expand_brace_alternatives_with_provenance(pattern, None, escapes, &mut provenance_budget)?
            .into_iter()
            .map(|alternative| alternative.bytes)
            .collect(),
    )
}

fn first_unescaped_brace(pattern: &[u8], escapes: bool) -> Option<usize> {
    if !escapes {
        return memchr(b'{', pattern);
    }
    let mut index = 0;
    while index < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += 2;
        } else if pattern[index] == b'{' {
            return Some(index);
        } else {
            index += 1;
        }
    }
    None
}

fn matching_brace(pattern: &[u8], open: usize, escapes: bool) -> Option<usize> {
    let mut depth = 0_usize;
    let mut index = open;
    while index < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += 2;
            continue;
        }
        match pattern[index] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn split_brace_alternatives(content: &[u8], escapes: bool) -> Vec<std::ops::Range<usize>> {
    let mut alternatives = Vec::new();
    let mut start = 0;
    let mut depth = 0_usize;
    let mut index = 0;
    while index < content.len() {
        if escapes && content[index] == b'\\' {
            index += 2;
            continue;
        }
        match content[index] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b',' if depth == 0 => {
                alternatives.push(start..index);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    alternatives.push(start..content.len());
    alternatives
}

fn is_separator(byte: u8) -> bool {
    byte == b'/' || (cfg!(windows) && byte == b'\\')
}

fn at_component_start(path: &[u8], index: usize, options: PatternOptions) -> bool {
    (index == 0 && options.candidate_starts_component)
        || (index > 0 && path.get(index - 1).is_some_and(|byte| is_separator(*byte)))
}

fn bytes_equal(expected: u8, actual: u8, case_insensitive: bool) -> bool {
    fold_ascii(expected, case_insensitive) == fold_ascii(actual, case_insensitive)
}

fn fold_ascii(byte: u8, case_insensitive: bool) -> u8 {
    if case_insensitive {
        byte.to_ascii_lowercase()
    } else {
        byte
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtglobKind {
    Optional,
    ZeroOrMore,
    OneOrMore,
    ExactlyOne,
    Negated,
}

/// A compiled extglob pattern.
///
/// One step per byte offset of the raw pattern. The interpreter this replaces
/// jumped by byte offset — into a group's rest, back to the last star, past a
/// class — so keeping offsets as the addressing scheme reproduces its control
/// flow exactly, while every parse it redid on each call happens once here.
///
/// Steps are filled only for offsets a walk can reach: a group jumps past its
/// own content, whose alternatives are compiled separately. An unreachable
/// offset keeps [`ExtglobStep::NoMatch`], so nothing depends on the interior
/// of a group being classifiable on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledExtglob {
    /// The ordinary bytes the program starts with. Every engine consumes
    /// them one for one, so a candidate that does not start with them is
    /// rejected before either engine runs, as the plain prefilter does.
    literal_prefix: Vec<u8>,
    steps: Vec<ExtglobStep>,
    groups: Vec<ExtglobGroup>,
    /// Linear-space Thompson machine for eligible positive extglobs.
    /// Negation needs substring-complement semantics, while plain outer stars
    /// retain legacy backtracking quirks; those cases stay on the compatible
    /// explicit worklist below.
    positive_nfa: Option<PositiveExtglobNfa>,
    /// Dense memo state for every interpreter offset that participates in the
    /// recurrence: the entry point, each group start and continuation, plus
    /// the zero-directory continuation after a recursive `**/` prefix.
    /// The byte-indexed step table remains the interpreter's address space.
    memo_state_indices: Vec<usize>,
    /// Memo rows needed on the fast path; fallback retry rows for retryable
    /// outer-star segments are assigned strictly above this count and allocated
    /// lazily only when a deferred retry fires.
    fast_memo_state_count: usize,
    memo_state_count: usize,
    /// Whether any outer-star segment can hit a retryable hidden-dot or
    /// separator barrier behind an overwritten earlier star.
    has_retryable_star_segment: bool,
    /// The step offset of a separator that a candidate ending right in front
    /// of it may leave out, because only a trailing whole-component `**`
    /// follows it; see [`trailing_globstar_separator`].
    trailing_globstar_separator: Option<usize>,
    /// Original-source byte offsets for `steps`, retained after brace
    /// expansion so walker diagnostics preserve `PatternError` provenance.
    walker_source_provenance: Option<SourceProvenance>,
    leading_dot_is_normalized: bool,
}

/// What the walk does at one byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExtglobStep {
    /// Nothing matches here; the walk falls back to its last star. Also the
    /// filler for an offset no walk reaches.
    NoMatch,
    /// A run of `*`, resuming at the offset after it.
    Star {
        next: usize,
        /// Ordinary stars cannot stop immediately before a leading period.
        /// A whole-component star run ending in a recursive `**/` prefix is
        /// exempt, including longer even runs such as `****/`.
        blocks_leading_period: bool,
        /// Whether this star precedes a retryable barrier in its outer-star
        /// segment and must keep expanding in the memoized fallback rather
        /// than handing off to the linear latest-star fast path.
        memoize_tail: bool,
    },
    /// `?`.
    Any,
    /// A bracket class, resuming at the offset after it.
    Class { class: Class, next: usize },
    /// A backslash with a byte to escape. Only the escaped byte matches, and
    /// the walk skips both offsets: the backslash is never read as a literal
    /// with the escaped byte as syntax, exactly as in the plain engines and
    /// in Bash.
    Escape { escaped: u8 },
    /// An unescaped `/` path separator, matching `/` everywhere and `\` on
    /// Windows (ADR-0005). `recursive_prefix` is true when this separator
    /// closes a whole-component recursive star run (`**/`, `****/`), matching
    /// `Token::RecursivePrefix` in the plain token engine.
    Separator { recursive_prefix: bool },
    /// An ordinary byte.
    Byte(u8),
    /// An extglob group, indexing [`CompiledExtglob::groups`].
    Group(usize),
    /// A group opener whose parenthesis never closes. It still refuses a
    /// leading period, then reads `byte` as ordinary text.
    UnclosedGroup { byte: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PositiveExtglobNfa {
    states: Vec<PositiveExtglobState>,
    start: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PositiveExtglobState {
    Epsilon {
        targets: Vec<usize>,
        blocks_leading_period: bool,
        /// A whole-component recursive prefix arm (`**/`, `****/`) inside a
        /// repeated group may only be entered at a component start (ADR-0020).
        requires_component_start: bool,
    },
    Consume {
        matcher: PositiveExtglobMatcher,
        next: usize,
    },
    Star {
        matcher: PositiveExtglobMatcher,
        next: usize,
        blocks_leading_period: bool,
        /// A `**/` prefix hands over only at a component start (#419).
        stops_at_component_start: bool,
    },
    Match,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PositiveExtglobMatcher {
    Literal(u8),
    GroupLiteral(u8),
    GroupSeparator,
    Wildcard(WildcardScope),
    Class(Class, WildcardScope),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WildcardScope {
    /// An outer-program wildcard and whether its source position follows an
    /// explicit separator.
    Extglob(bool),
    /// A group-alternative wildcard and whether its source position is the
    /// component-local root of that alternative or follows its own separator.
    Group(bool),
    /// An explicit recursive wildcard, which crosses separators under every
    /// entry point.
    Recursive,
}

struct PositiveExtglobBuilder<'budget> {
    states: Vec<PositiveExtglobState>,
    budget: &'budget mut IrBudget,
}

#[derive(Default)]
struct PositiveExtglobClosureScratch {
    seen: Vec<bool>,
    pending: Vec<usize>,
}

#[derive(Default)]
struct PositiveExtglobMatchScratch {
    current: Vec<usize>,
    next: Vec<usize>,
    closure: PositiveExtglobClosureScratch,
}

impl PositiveExtglobMatchScratch {
    fn prepare(&mut self, state_count: usize) {
        self.current.clear();
        self.next.clear();
        self.closure.seen.resize(state_count, false);
        self.closure.seen.fill(false);
        self.closure.pending.clear();
    }

    fn release(&mut self) {
        self.current.clear();
        if self.current.capacity() > RETAINED_SCRATCH_WORDS {
            self.current.shrink_to(RETAINED_SCRATCH_WORDS);
        }
        self.next.clear();
        if self.next.capacity() > RETAINED_SCRATCH_WORDS {
            self.next.shrink_to(RETAINED_SCRATCH_WORDS);
        }
        self.closure.pending.clear();
        if self.closure.pending.capacity() > RETAINED_SCRATCH_WORDS {
            self.closure.pending.shrink_to(RETAINED_SCRATCH_WORDS);
        }
        let retained_seen_bits = RETAINED_SCRATCH_WORDS * u64::BITS as usize;
        if self.closure.seen.capacity() > retained_seen_bits {
            self.closure.seen.clear();
            self.closure.seen.shrink_to(retained_seen_bits);
        }
    }
}

// A positive NFA touches the same four state buffers for every candidate, so
// keep their allocation per thread just like the general and fallback engines.
thread_local! {
    static POSITIVE_EXTGLOB_SCRATCH: RefCell<PositiveExtglobMatchScratch> =
        RefCell::new(PositiveExtglobMatchScratch::default());
}

/// One alternative of a group, compiled as a whole-candidate pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtglobAlternative {
    compiled: Vec<CompiledAlternative>,
    /// Byte width when every token consumes a fixed count, which lets the scan
    /// over candidate end offsets visit one offset instead of the component.
    width: Option<usize>,
    /// Precomputed at compile time: whether any compiled arm starts with a
    /// whole-component recursive prefix (`**/`, `****/`) and must therefore
    /// only start at a component boundary (ADR-0020, #495).
    requires_component_start: bool,
    /// Whether [`ExtglobGroup::through_separator`] holds this alternative
    /// with the separator behind the group, which the matchers use instead.
    /// The alternative itself stays for the compile-time analyses, which
    /// read the separator from the outer program.
    absorbed_by_separator: bool,
}

/// How [`match_extglob_task`] deduplicates arrivals at a group's start offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupEntryMemo {
    /// Reached only through fixed-width steps from a single deduplicated task
    /// entry (no preceding star or `through_separator` split), so the same
    /// `(group.start, path_index)` can never be visited twice.
    None,
    /// Reached only from a single task arrival after one or more ordinary stars
    /// with no crossing-to-component-local star pair and no recursive `**/`
    /// prefix, so `path_index` arrives monotonically and a single stack slot
    /// catches separator-stop retries without heap memo pages.
    Monotonic,
    /// Like [`Self::Monotonic`], except an earlier star precedes a
    /// separator-bound star that can populate `crossing_star` under
    /// `is_match_path`. Uses stack deduplication unless `crossing_star`
    /// actually activates at runtime, in which case it falls back to paged
    /// `(group.start, path_index)` memoization.
    MonotonicOrCrossingStar,
    /// Reached after a recursive `**/` prefix, a multi-arrival preceding group
    /// plus a star, or a `through_separator` split, requiring paged
    /// `(group.start, path_index)` memoization in `ExtglobFailedStates`.
    Paged,
}

/// One `?(…)`, `*(…)`, `+(…)`, `@(…)` or `!(…)`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtglobGroup {
    kind: ExtglobKind,
    alternatives: Vec<ExtglobAlternative>,
    /// Offset of this group's operator in the program.
    start: usize,
    /// Offset just past the closing parenthesis.
    rest: usize,
    /// Whether the group operator follows an explicit separator in the outer
    /// program. Its alternatives inherit that position at their root.
    component_local: bool,
    /// Whether some alternative spells a separator or a recursive star, so a
    /// match can span components even where every ordinary wildcard is
    /// component-local.
    spans_components: bool,
    /// Whether some alternative starts with an ordinary wildcard, the one
    /// token whose locality follows the group's position.
    root_wildcard: bool,
    /// Every alternative that ends in a whole-component `**` right before a
    /// separator of the outer program, compiled together with that separator
    /// and resuming behind it (#422). Written inline, the `**` and the
    /// separator are one `**/`, which may stand for no directory at all, so
    /// `@(**)/y` matches `y` as `**/y` does, and which leaves no trailing
    /// `/**` for the path to stop in front of, so `@(a/**)/**` refuses `a` as
    /// `a/**/**` does. In a repetition (`*(…)`, `+(…)`), the same compiled
    /// `**/` form also runs as a non-final iteration so `*(**|.h)/x` and
    /// `+(a/**|.h)/x` can hand off at a component boundary to a later literal
    /// dot iteration while still refusing mid-component handoff (`a.h/x`)
    /// (#495). Empty for a negated group, which consumes what its alternatives
    /// reject rather than their text.
    through_separator: Vec<ExtglobAlternative>,
    /// Precomputed at compile time: whether every matching arm is a
    /// [`Self::through_separator`] arm ending in `Token::RecursivePrefix` whose
    /// match language is closed under repetition (such as `+(**)/x`,
    /// `*(**|****)/x`, or `+(<32x a/>**)/z` when no arm can match a hidden
    /// component under `match_hidden: false`). Because any later iteration is
    /// already absorbed by the first iteration's trailing `**/`, repeating only
    /// such arms needs neither the repetition bitmap nor an NFA loop-back edge
    /// (#495).
    idempotent_through_separator: bool,
    /// Deduplication strategy for arrivals at `self.start`.
    entry_memo: GroupEntryMemo,
}

impl ExtglobGroup {
    /// Whether the first iteration of a repetition reads its root wildcard
    /// differently from a later one, which follows the previous iteration's
    /// text rather than the separator in front of the group.
    fn first_iteration_differs(&self) -> bool {
        self.component_local && self.root_wildcard
    }

    /// The alternatives the matchers run with the continuation behind the
    /// group; the rest are matched through [`Self::through_separator`].
    fn matched_alternatives(&self) -> impl Iterator<Item = &ExtglobAlternative> {
        self.alternatives
            .iter()
            .filter(|alternative| !alternative.absorbed_by_separator)
    }

    /// Every alternative that may run as an iteration of a repeated group,
    /// tagged with whether it comes from [`Self::through_separator`].
    fn repetition_alternatives(&self) -> impl Iterator<Item = (&ExtglobAlternative, bool)> {
        self.matched_alternatives()
            .map(|alternative| (alternative, false))
            .chain(
                self.through_separator
                    .iter()
                    .map(|alternative| (alternative, true)),
            )
    }

    /// Whether the interpreter matches the first iteration on its own, which
    /// it does where that difference is observable: under the list-filter
    /// policy.
    fn first_iteration_apart(&self, options: PatternOptions) -> bool {
        self.first_iteration_differs()
            && options.component_wildcards
            && !options.root_component_wildcards
    }

    /// The root position the interpreter gives an iteration after the first.
    fn later_iteration_root_local(&self, options: PatternOptions) -> bool {
        self.component_local && !self.first_iteration_apart(options)
    }

    /// The last candidate offset a match of one alternative can end at.
    ///
    /// A negated group consumes the text its alternatives reject, a
    /// wildcard-like span that stays in the component under a
    /// component-local position. A positive alternative answers the position
    /// rule itself: its root wildcard is component-local exactly when the
    /// group is, and a wildcard behind it crosses again under the list-filter
    /// rule, as in `a/b*`. Bounding it by the component end is only a
    /// shortcut, taken when every ordinary wildcard is component-local and no
    /// alternative spells a separator or a recursive star.
    fn alternative_end_limit(
        &self,
        path: &[u8],
        path_index: usize,
        options: PatternOptions,
    ) -> usize {
        if self.kind == ExtglobKind::Negated {
            return extglob_component_end(path, path_index, options, self.component_local);
        }
        if options.component_wildcards && options.root_component_wildcards && !self.spans_components
        {
            return extglob_component_end(path, path_index, options, true);
        }
        path.len()
    }
}

/// Whether `tokens` could match a path component starting with `.` under any
/// component-wildcard policy (`is_match` or `is_match_glob_path`).
fn tokens_can_match_hidden_component(tokens: &[Token], case_insensitive: bool) -> bool {
    let mut may_start_component = true;
    for token in tokens {
        match token {
            Token::Literal(literal) => {
                if (may_start_component && literal.starts_with(b"."))
                    || literal
                        .windows(2)
                        .any(|pair| is_separator(pair[0]) && pair[1] == b'.')
                {
                    return true;
                }
                may_start_component = literal.last().is_some_and(|&byte| is_separator(byte));
            }
            Token::Separator | Token::RecursivePrefix => {
                may_start_component = true;
            }
            Token::Class(class) => {
                if may_start_component && class.matches(b'.', case_insensitive) {
                    return true;
                }
                may_start_component = class.matches(b'/', case_insensitive)
                    || (cfg!(windows) && class.matches(b'\\', case_insensitive));
            }
            Token::Any | Token::Star | Token::RecursiveStar => {
                // Under `match_hidden: false` wildcards never consume a leading
                // `.`, but under `is_match` (`component_wildcards: false`) they
                // may consume a separator so the next token starts a component.
                may_start_component = true;
            }
        }
    }
    false
}

impl PositiveExtglobNfa {
    fn compile(
        steps: &[ExtglobStep],
        groups: &[ExtglobGroup],
        trailing_globstar_separator: Option<usize>,
        budget: &mut IrBudget,
    ) -> Result<Option<Self>, PatternError> {
        if groups
            .iter()
            .any(|group| group.kind == ExtglobKind::Negated)
            || steps.iter().any(|step| {
                matches!(
                    step,
                    ExtglobStep::Star { .. } | ExtglobStep::UnclosedGroup { byte: b'*' }
                )
            })
        {
            return Ok(None);
        }
        let mut builder = PositiveExtglobBuilder {
            states: Vec::new(),
            budget,
        };
        let accept = builder.push(PositiveExtglobState::Match, 0)?;
        let mut suffixes = vec![None; steps.len() + 1];
        suffixes[steps.len()] = Some(accept);

        for index in (0..steps.len()).rev() {
            let start =
                match &steps[index] {
                    ExtglobStep::NoMatch => None,
                    ExtglobStep::Separator { .. } => builder
                        .consume(PositiveExtglobMatcher::GroupSeparator, suffixes[index + 1])?,
                    ExtglobStep::Byte(byte) => builder
                        .consume(PositiveExtglobMatcher::Literal(*byte), suffixes[index + 1])?,
                    ExtglobStep::Any => builder.consume(
                        PositiveExtglobMatcher::Wildcard(WildcardScope::Extglob(
                            extglob_step_component_local(steps, index),
                        )),
                        suffixes[index + 1],
                    )?,
                    ExtglobStep::Class { class, next } => builder.consume(
                        PositiveExtglobMatcher::Class(
                            class.clone(),
                            WildcardScope::Extglob(extglob_step_component_local(steps, index)),
                        ),
                        suffixes[*next],
                    )?,
                    ExtglobStep::Star { .. } => {
                        unreachable!("outer stars keep the compatible interpreter")
                    }
                    ExtglobStep::Escape { escaped } => builder.consume(
                        PositiveExtglobMatcher::Literal(*escaped),
                        suffixes[index + 2],
                    )?,
                    ExtglobStep::Group(group) => {
                        let group = &groups[*group];
                        let behind_separator = if group.through_separator.is_empty() {
                            None
                        } else {
                            suffixes[group.rest + 1]
                        };
                        builder.group(group, suffixes[group.rest], behind_separator)?
                    }
                    ExtglobStep::UnclosedGroup { byte: b'*' } => {
                        unreachable!("unclosed outer stars keep the compatible interpreter")
                    }
                    ExtglobStep::UnclosedGroup { byte: b'?' } => builder.consume(
                        PositiveExtglobMatcher::Wildcard(WildcardScope::Extglob(
                            extglob_step_component_local(steps, index),
                        )),
                        suffixes[index + 1],
                    )?,
                    ExtglobStep::UnclosedGroup { byte } => builder
                        .consume(PositiveExtglobMatcher::Literal(*byte), suffixes[index + 1])?,
                };
            // A candidate may end in front of the separator of a trailing
            // `/**`, as `x/**` accepts `x` (#422).
            let start = if trailing_globstar_separator == Some(index) {
                builder.epsilon(start.into_iter().chain([accept]).collect(), false)?
            } else {
                start
            };
            suffixes[index] = start;
        }
        Ok(suffixes[0].map(|start| Self {
            states: builder.states,
            start,
        }))
    }

    fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
        POSITIVE_EXTGLOB_SCRATCH.with(|cell| match cell.try_borrow_mut() {
            Ok(mut scratch) => {
                scratch.prepare(self.states.len());
                let matched = self.is_match_with_scratch(path, options, &mut scratch);
                scratch.release();
                matched
            }
            Err(_) => {
                let mut scratch = PositiveExtglobMatchScratch::default();
                scratch.prepare(self.states.len());
                self.is_match_with_scratch(path, options, &mut scratch)
            }
        })
    }

    fn is_match_with_scratch(
        &self,
        path: &[u8],
        options: PatternOptions,
        scratch: &mut PositiveExtglobMatchScratch,
    ) -> bool {
        let PositiveExtglobMatchScratch {
            current,
            next,
            closure,
        } = scratch;
        self.add_closure(
            self.start,
            path.first().copied(),
            options.candidate_starts_component,
            options,
            current,
            closure,
        );

        let mut at_component_start = options.candidate_starts_component;
        for (path_index, &byte) in path.iter().enumerate() {
            next.clear();
            closure.seen.fill(false);
            let next_byte = path.get(path_index + 1).copied();
            let next_starts_component = is_separator(byte);
            for &state in current.iter() {
                match &self.states[state] {
                    PositiveExtglobState::Consume {
                        matcher,
                        next: target,
                    } if matcher.matches(byte, at_component_start, options) => {
                        self.add_closure(
                            *target,
                            next_byte,
                            next_starts_component,
                            options,
                            next,
                            closure,
                        );
                    }
                    PositiveExtglobState::Star { matcher, .. }
                        if matcher.matches(byte, at_component_start, options) =>
                    {
                        self.add_closure(
                            state,
                            next_byte,
                            next_starts_component,
                            options,
                            next,
                            closure,
                        );
                    }
                    _ => {}
                }
            }
            if next.is_empty() {
                return false;
            }
            std::mem::swap(current, next);
            at_component_start = next_starts_component;
        }
        current
            .iter()
            .any(|&state| matches!(self.states[state], PositiveExtglobState::Match))
    }

    fn add_closure(
        &self,
        start: usize,
        byte: Option<u8>,
        at_component_start: bool,
        options: PatternOptions,
        active: &mut Vec<usize>,
        scratch: &mut PositiveExtglobClosureScratch,
    ) {
        scratch.pending.clear();
        scratch.pending.push(start);
        while let Some(state) = scratch.pending.pop() {
            if std::mem::replace(&mut scratch.seen[state], true) {
                continue;
            }
            match &self.states[state] {
                PositiveExtglobState::Epsilon {
                    targets,
                    blocks_leading_period,
                    requires_component_start,
                } => {
                    if (*requires_component_start && !at_component_start)
                        || (*blocks_leading_period
                            && !options.match_hidden
                            && at_component_start
                            && byte == Some(b'.'))
                    {
                        continue;
                    }
                    scratch.pending.extend(targets.iter().copied());
                }
                PositiveExtglobState::Star {
                    next,
                    blocks_leading_period,
                    stops_at_component_start,
                    ..
                } => {
                    active.push(state);
                    if !(*blocks_leading_period
                        && !options.match_hidden
                        && at_component_start
                        && byte == Some(b'.'))
                        && (!*stops_at_component_start || at_component_start)
                    {
                        scratch.pending.push(*next);
                    }
                }
                PositiveExtglobState::Consume { .. } | PositiveExtglobState::Match => {
                    active.push(state);
                }
            }
        }
    }
}

/// Scratch capacities after a positive-extglob NFA match, used to verify that
/// repeated matches reuse bounded thread-local allocations.
#[cfg(test)]
fn positive_extglob_scratch_capacities() -> (usize, usize, usize, usize) {
    POSITIVE_EXTGLOB_SCRATCH.with(|cell| {
        let scratch = cell.borrow();
        (
            scratch.current.capacity(),
            scratch.next.capacity(),
            scratch.closure.seen.capacity(),
            scratch.closure.pending.capacity(),
        )
    })
}

impl PositiveExtglobMatcher {
    fn matches(&self, byte: u8, at_component_start: bool, options: PatternOptions) -> bool {
        match self {
            Self::Literal(expected) => bytes_equal(*expected, byte, options.case_insensitive),
            Self::GroupLiteral(expected) => bytes_equal(*expected, byte, options.case_insensitive),
            Self::GroupSeparator => is_separator(byte),
            Self::Wildcard(scope) => scope.accepts(byte, at_component_start, options),
            Self::Class(class, scope) => {
                scope.accepts(byte, at_component_start, options)
                    && class.matches(byte, options.case_insensitive)
            }
        }
    }
}

impl WildcardScope {
    fn accepts(self, byte: u8, at_component_start: bool, options: PatternOptions) -> bool {
        let component_local = match self {
            Self::Extglob(position_local) | Self::Group(position_local) => {
                options.component_wildcards && (options.root_component_wildcards || position_local)
            }
            Self::Recursive => false,
        };
        (!is_separator(byte) || !component_local)
            && (options.match_hidden || byte != b'.' || !at_component_start)
    }
}

fn token_position_component_local(
    tokens: &[Token],
    token_index: usize,
    root_component_local: bool,
) -> bool {
    token_index == 0 && root_component_local
        || token_index > 0 && matches!(tokens[token_index - 1], Token::Separator)
}

fn extglob_step_component_local(steps: &[ExtglobStep], step_index: usize) -> bool {
    step_index > 0
        && matches!(
            steps[step_index - 1],
            ExtglobStep::Separator {
                recursive_prefix: false
            }
        )
}

impl PositiveExtglobBuilder<'_> {
    fn push(
        &mut self,
        state: PositiveExtglobState,
        edge_count: usize,
    ) -> Result<usize, PatternError> {
        // A state contains an enum payload and every edge is one stored index.
        // Charging two IR units for the payload plus one per edge deliberately
        // overestimates the actual allocation before it is made.
        self.budget.charge(2 + edge_count, 0)?;
        let index = self.states.len();
        self.states.push(state);
        Ok(index)
    }

    fn consume(
        &mut self,
        matcher: PositiveExtglobMatcher,
        next: Option<usize>,
    ) -> Result<Option<usize>, PatternError> {
        next.map(|next| self.push(PositiveExtglobState::Consume { matcher, next }, 1))
            .transpose()
    }

    fn star(
        &mut self,
        matcher: PositiveExtglobMatcher,
        next: Option<usize>,
        semantics: StarSemantics,
    ) -> Result<Option<usize>, PatternError> {
        next.map(|next| {
            self.push(
                PositiveExtglobState::Star {
                    matcher,
                    next,
                    blocks_leading_period: semantics.blocks_hidden_stop,
                    stops_at_component_start: semantics.stops_at_component_start,
                },
                1,
            )
        })
        .transpose()
    }

    fn epsilon(
        &mut self,
        mut targets: Vec<usize>,
        blocks_leading_period: bool,
    ) -> Result<Option<usize>, PatternError> {
        targets.sort_unstable();
        targets.dedup();
        if targets.is_empty() {
            return Ok(None);
        }
        if targets.len() == 1 && !blocks_leading_period {
            return Ok(targets.first().copied());
        }
        let edge_count = targets.len();
        self.push(
            PositiveExtglobState::Epsilon {
                targets,
                blocks_leading_period,
                requires_component_start: false,
            },
            edge_count,
        )
        .map(Some)
    }

    fn epsilon_at_component_start(&mut self, target: usize) -> Result<usize, PatternError> {
        self.push(
            PositiveExtglobState::Epsilon {
                targets: vec![target],
                blocks_leading_period: false,
                requires_component_start: true,
            },
            1,
        )
    }

    /// Compiles `group` in front of `rest`. `behind_separator` is the state
    /// behind the separator that follows the group, where the alternatives
    /// that take that separator along resume (#422).
    fn group(
        &mut self,
        group: &ExtglobGroup,
        rest: Option<usize>,
        behind_separator: Option<usize>,
    ) -> Result<Option<usize>, PatternError> {
        // The separator in front of `behind_separator` consumes one byte into
        // it, so it has no state exactly when `behind_separator` has none.
        let Some(rest) = rest else {
            return Ok(None);
        };
        let blocks_leading_period = !extglob_group_allows_literal_leading_period(group);
        match group.kind {
            ExtglobKind::ExactlyOne => {
                let mut alternatives = self.alternatives(group, rest, group.component_local)?;
                if let Some(behind_separator) = behind_separator {
                    alternatives.extend(self.through_separator(
                        group,
                        behind_separator,
                        group.component_local,
                    )?);
                }
                self.epsilon(alternatives, blocks_leading_period)
            }
            ExtglobKind::Optional => {
                let mut alternatives = self.alternatives(group, rest, group.component_local)?;
                alternatives.push(rest);
                if let Some(behind_separator) = behind_separator {
                    alternatives.extend(self.through_separator(
                        group,
                        behind_separator,
                        group.component_local,
                    )?);
                }
                self.epsilon(alternatives, blocks_leading_period)
            }
            ExtglobKind::ZeroOrMore | ExtglobKind::OneOrMore => {
                if group.idempotent_through_separator {
                    let mut entry = if let Some(behind_separator) = behind_separator {
                        self.through_separator(group, behind_separator, group.component_local)?
                    } else {
                        Vec::new()
                    };
                    if group.kind == ExtglobKind::ZeroOrMore {
                        entry.push(rest);
                    }
                    return self.epsilon(entry, blocks_leading_period);
                }
                let hub = self.push(
                    PositiveExtglobState::Epsilon {
                        targets: Vec::new(),
                        blocks_leading_period: false,
                        requires_component_start: false,
                    },
                    0,
                )?;
                // A `through_separator` alternative may either leave the loop
                // behind the outer separator (final iteration, #422) or return
                // to `hub` for a further iteration such as a literal `.h` arm
                // (#495).
                let after_through = behind_separator
                    .map(|behind| self.epsilon(vec![hub, behind], false))
                    .transpose()?
                    .flatten();
                // Only the first iteration stands where the group does; a
                // later one follows the previous iteration's text, so its
                // root wildcard gets a copy that is not component-local.
                let mut later = self.alternatives(group, hub, false)?;
                if let Some(after_through) = after_through {
                    later.extend(self.through_separator(group, after_through, false)?);
                }
                let first = if group.first_iteration_differs() {
                    let mut first = self.alternatives(group, hub, true)?;
                    if let Some(after_through) = after_through {
                        first.extend(self.through_separator(group, after_through, true)?);
                    }
                    first
                } else {
                    later.clone()
                };
                let mut loop_targets = later;
                loop_targets.push(rest);
                let edge_count = loop_targets.len();
                self.budget.charge(edge_count, 0)?;
                self.states[hub] = PositiveExtglobState::Epsilon {
                    targets: loop_targets,
                    blocks_leading_period: false,
                    requires_component_start: false,
                };
                if group.kind == ExtglobKind::ZeroOrMore {
                    let mut entry = first;
                    entry.push(rest);
                    self.epsilon(entry, blocks_leading_period)
                } else {
                    self.epsilon(first, blocks_leading_period)
                }
            }
            ExtglobKind::Negated => unreachable!("negated groups do not compile to this NFA"),
        }
    }

    fn alternatives(
        &mut self,
        group: &ExtglobGroup,
        next: usize,
        root_component_local: bool,
    ) -> Result<Vec<usize>, PatternError> {
        self.alternative_list(group.matched_alternatives(), next, root_component_local)
    }

    /// The alternatives that take the separator behind the group along,
    /// resuming at `behind_separator`.
    fn through_separator(
        &mut self,
        group: &ExtglobGroup,
        behind_separator: usize,
        root_component_local: bool,
    ) -> Result<Vec<usize>, PatternError> {
        self.alternative_list(
            &group.through_separator,
            behind_separator,
            root_component_local,
        )
    }

    fn alternative_list<'a>(
        &mut self,
        alternatives: impl IntoIterator<Item = &'a ExtglobAlternative>,
        next: usize,
        root_component_local: bool,
    ) -> Result<Vec<usize>, PatternError> {
        let mut starts = Vec::new();
        for alternative in alternatives {
            for compiled in &alternative.compiled {
                if let Some(mut start) =
                    self.tokens(&compiled.tokens, next, root_component_local)?
                {
                    if alternative_starts_with_recursive_prefix(&compiled.tokens) {
                        start = self.epsilon_at_component_start(start)?;
                    }
                    starts.push(start);
                }
            }
        }
        Ok(starts)
    }

    fn tokens(
        &mut self,
        tokens: &[Token],
        next: usize,
        root_component_local: bool,
    ) -> Result<Option<usize>, PatternError> {
        let mut start = Some(next);
        for (token_index, token) in tokens.iter().enumerate().rev() {
            start = match token {
                Token::Literal(literal) => {
                    let mut literal_start = start;
                    for &byte in literal.iter().rev() {
                        literal_start = self
                            .consume(PositiveExtglobMatcher::GroupLiteral(byte), literal_start)?;
                    }
                    literal_start
                }
                Token::Separator => self.consume(PositiveExtglobMatcher::GroupSeparator, start)?,
                Token::Any => self.consume(
                    PositiveExtglobMatcher::Wildcard(WildcardScope::Group(
                        token_position_component_local(tokens, token_index, root_component_local),
                    )),
                    start,
                )?,
                Token::Class(class) => self.consume(
                    PositiveExtglobMatcher::Class(
                        class.clone(),
                        WildcardScope::Group(token_position_component_local(
                            tokens,
                            token_index,
                            root_component_local,
                        )),
                    ),
                    start,
                )?,
                Token::Star => self.star(
                    PositiveExtglobMatcher::Wildcard(WildcardScope::Group(
                        token_position_component_local(tokens, token_index, root_component_local),
                    )),
                    start,
                    StarSemantics::ordinary(false).handing_over_to(tokens.get(token_index + 1)),
                )?,
                Token::RecursiveStar => self.star(
                    PositiveExtglobMatcher::Wildcard(WildcardScope::Recursive),
                    start,
                    StarSemantics::ordinary(true).handing_over_to(tokens.get(token_index + 1)),
                )?,
                Token::RecursivePrefix => self.star(
                    PositiveExtglobMatcher::Wildcard(WildcardScope::Recursive),
                    start,
                    StarSemantics::RECURSIVE_PREFIX,
                )?,
            };
            if token_index + 2 == tokens.len()
                && matches!(token, Token::Separator)
                && matches!(tokens.last(), Some(Token::RecursiveStar))
                && let Some(normal) = start
            {
                start = self.epsilon(vec![normal, next], false)?;
            }
        }
        Ok(start)
    }
}

/// Compiles the extglob program for `pattern`, or `None` when it has no group.
///
/// This subsumes the scan `is_match` used to repeat on every call.
fn compile_extglob(
    pattern: &[u8],
    options: PatternOptions,
    budget: &mut IrBudget,
    provenance_budget: &mut ProvenanceBudget,
    walker_source_provenance: Option<&SourceProvenance>,
    leading_dot_is_normalized: bool,
) -> Result<Option<Box<CompiledExtglob>>, PatternError> {
    if !options.extglob || !contains_extglob(pattern, options.escape) {
        return Ok(None);
    }
    // The step table is one entry per byte offset whatever the walk reaches, so
    // it is charged in full before it is allocated.
    budget.charge(pattern.len(), 0)?;
    #[cfg(test)]
    EXTGLOB_CLOSING_TABLE_LEN.with(|cell| cell.set(0));
    let mut closing_parens = ExtglobClosingParens::Direct;
    let mut steps = vec![ExtglobStep::NoMatch; pattern.len()];
    let mut compiled = vec![false; pattern.len()];
    let mut groups = Vec::new();
    let mut pending = vec![0_usize];
    let mut single_arrival = true;
    let mut segment_has_star = false;
    let mut segment_has_crossing_star = false;
    let mut segment_needs_paged_memo = false;
    let mut prev_was_separator = false;
    let mut prev_was_any_separator = false;
    let mut prev_was_recursive_star = false;
    while let Some(index) = pending.pop() {
        if index >= pattern.len() || compiled[index] {
            continue;
        }
        compiled[index] = true;
        let step = compile_extglob_step(
            pattern,
            index,
            prev_was_separator,
            prev_was_any_separator,
            prev_was_recursive_star,
            options,
            &mut closing_parens,
            &mut groups,
            budget,
            provenance_budget,
            walker_source_provenance,
        )?;
        match &step {
            ExtglobStep::Group(group) => {
                let group = &mut groups[*group];
                group.entry_memo = if !segment_has_star && !segment_needs_paged_memo {
                    GroupEntryMemo::None
                } else if single_arrival && !segment_needs_paged_memo {
                    if segment_has_crossing_star {
                        GroupEntryMemo::MonotonicOrCrossingStar
                    } else {
                        GroupEntryMemo::Monotonic
                    }
                } else {
                    GroupEntryMemo::Paged
                };
                let uniform_fixed_width = group
                    .alternatives
                    .first()
                    .and_then(|alternative| alternative.width)
                    .is_some_and(|width| {
                        group
                            .alternatives
                            .iter()
                            .all(|alternative| alternative.width == Some(width))
                    });
                single_arrival = single_arrival
                    && !segment_has_star
                    && !segment_needs_paged_memo
                    && group.kind == ExtglobKind::ExactlyOne
                    && group.through_separator.is_empty()
                    && uniform_fixed_width;
                segment_has_star = false;
                segment_has_crossing_star = false;
                segment_needs_paged_memo = !group.through_separator.is_empty();
                prev_was_separator = false;
                prev_was_any_separator = false;
                prev_was_recursive_star = false;
                pending.push(group.rest);
            }
            ExtglobStep::Star {
                next,
                blocks_leading_period,
                ..
            } => {
                if !*blocks_leading_period {
                    segment_needs_paged_memo = true;
                } else if segment_has_star && prev_was_separator {
                    segment_has_crossing_star = true;
                }
                segment_has_star = true;
                prev_was_separator = false;
                prev_was_any_separator = false;
                prev_was_recursive_star = !*blocks_leading_period;
                pending.push(*next);
            }
            ExtglobStep::Class { next, .. } => {
                prev_was_separator = false;
                prev_was_any_separator = false;
                prev_was_recursive_star = false;
                pending.push(*next);
            }
            // The escaped byte is consumed together with its backslash; the
            // offset between them is never where a walk stands.
            ExtglobStep::Escape { .. } => {
                prev_was_separator = false;
                prev_was_any_separator = false;
                prev_was_recursive_star = false;
                pending.push(index + 2);
            }
            ExtglobStep::UnclosedGroup { byte: b'*' } => {
                if segment_has_star && prev_was_separator {
                    segment_has_crossing_star = true;
                }
                segment_has_star = true;
                prev_was_separator = false;
                prev_was_any_separator = false;
                prev_was_recursive_star = false;
                pending.push(index + 1);
            }
            ExtglobStep::Separator { recursive_prefix } => {
                prev_was_separator = !*recursive_prefix;
                prev_was_any_separator = true;
                prev_was_recursive_star = false;
                pending.push(index + 1);
            }
            ExtglobStep::Byte(_) => {
                prev_was_separator = false;
                prev_was_any_separator = false;
                prev_was_recursive_star = false;
                pending.push(index + 1);
            }
            ExtglobStep::Any | ExtglobStep::UnclosedGroup { .. } => {
                prev_was_separator = false;
                prev_was_any_separator = false;
                prev_was_recursive_star = false;
                pending.push(index + 1);
            }
            // The matcher still stops at this state, but compiling the suffix
            // records the same outer path structure for embeddings that need
            // to classify every generated branch.
            ExtglobStep::NoMatch => {
                prev_was_separator = false;
                prev_was_any_separator = false;
                prev_was_recursive_star = false;
                pending.push(index + 1);
            }
        }
        steps[index] = step;
    }
    let mut memo_state_indices = vec![usize::MAX; pattern.len() + 1];
    let mut memo_state_count = 0;
    let fast_memo_state_count = {
        let mut add_memo_state = |offset| {
            if memo_state_indices[offset] == usize::MAX {
                memo_state_indices[offset] = memo_state_count;
                memo_state_count += 1;
            }
        };
        add_memo_state(0);
        for group in &groups {
            if matches!(
                group.entry_memo,
                GroupEntryMemo::MonotonicOrCrossingStar | GroupEntryMemo::Paged
            ) {
                add_memo_state(group.start);
            }
            add_memo_state(group.rest);
            if !group.through_separator.is_empty() {
                // Alternatives that take the separator along resume behind it.
                add_memo_state(group.rest + 1);
            }
        }
        for step in &steps {
            if let ExtglobStep::Star {
                next,
                blocks_leading_period: false,
                ..
            } = step
                && steps
                    .get(*next)
                    .is_some_and(|step| matches!(step, ExtglobStep::Separator { .. }))
            {
                add_memo_state(*next + 1);
            }
        }
        memo_state_count
    };
    // Fallback memo indices are assigned strictly above `fast_memo_state_count`
    // and only for outer-star segments that can encounter a retryable hidden-
    // dot or separator barrier behind an overwritten star.
    let mut add_memo_state = |offset| {
        if memo_state_indices[offset] == usize::MAX {
            memo_state_indices[offset] = memo_state_count;
            memo_state_count += 1;
        }
    };
    let mut has_retryable_star_segment = false;
    let mut segment_cursor = 0;
    while segment_cursor < steps.len() {
        let mut first_star = None;
        let mut cursor = segment_cursor;
        let segment_end = loop {
            match steps.get(cursor) {
                None | Some(ExtglobStep::NoMatch) => break steps.len(),
                Some(ExtglobStep::Group(group)) => break groups[*group].rest,
                Some(ExtglobStep::Star { next, .. }) => {
                    first_star.get_or_insert(cursor);
                    cursor = *next;
                }
                Some(ExtglobStep::UnclosedGroup { byte: b'*' }) => {
                    first_star.get_or_insert(cursor);
                    cursor += 1;
                }
                Some(ExtglobStep::Class { next, .. }) => cursor = *next,
                Some(ExtglobStep::Escape { .. }) => cursor += 2,
                Some(
                    ExtglobStep::Any
                    | ExtglobStep::Separator { .. }
                    | ExtglobStep::Byte(_)
                    | ExtglobStep::UnclosedGroup { .. },
                ) => cursor += 1,
            }
        };
        if let Some(star_start) = first_star {
            let retry = extglob_retryable_star_segment(&steps, star_start, steps.len(), options);
            if retry.any() {
                has_retryable_star_segment = true;
                let memo_cutoff = retry.memo_cutoff;
                let mut cursor = star_start;
                while cursor < segment_end {
                    match steps.get_mut(cursor) {
                        None | Some(ExtglobStep::NoMatch) => break,
                        Some(ExtglobStep::Group(group)) => {
                            add_memo_state(cursor);
                            cursor = groups[*group].rest;
                        }
                        Some(ExtglobStep::Star {
                            next, memoize_tail, ..
                        }) => {
                            add_memo_state(cursor);
                            if cursor < memo_cutoff {
                                *memoize_tail = true;
                            }
                            cursor = *next;
                        }
                        Some(ExtglobStep::UnclosedGroup { byte: b'*' }) => {
                            add_memo_state(cursor);
                            cursor += 1;
                        }
                        Some(ExtglobStep::Class { next, .. }) => cursor = *next,
                        Some(ExtglobStep::Escape { .. }) => cursor += 2,
                        Some(
                            ExtglobStep::Any
                            | ExtglobStep::Separator { .. }
                            | ExtglobStep::Byte(_)
                            | ExtglobStep::UnclosedGroup { .. },
                        ) => cursor += 1,
                    }
                }
            }
        }
        if segment_end <= segment_cursor {
            break;
        }
        segment_cursor = segment_end;
    }
    let trailing_globstar_separator =
        trailing_globstar_separator(pattern, &steps, &groups, options);
    let positive_nfa =
        PositiveExtglobNfa::compile(&steps, &groups, trailing_globstar_separator, budget)?;
    // A candidate may stop in front of a trailing `/**`, so that separator
    // is not a byte every candidate starts with.
    let literal_prefix = steps[..trailing_globstar_separator.unwrap_or(steps.len())]
        .iter()
        .map_while(|step| match step {
            ExtglobStep::Byte(byte) => Some(*byte),
            ExtglobStep::Separator { .. } => Some(b'/'),
            _ => None,
        })
        .collect();
    Ok(Some(Box::new(CompiledExtglob {
        literal_prefix,
        steps,
        groups,
        positive_nfa,
        memo_state_indices,
        fast_memo_state_count,
        memo_state_count,
        has_retryable_star_segment,
        trailing_globstar_separator,
        walker_source_provenance: walker_source_provenance.cloned(),
        leading_dot_is_normalized,
    })))
}

/// Which barrier types in an outer-star segment can require retrying an
/// overwritten earlier star via the memoized fallback.
#[derive(Clone, Copy, Default)]
struct RetryableStarSegment {
    /// An explicit leading-`.` step appears strictly between two stars
    /// (`star_start < dot < later_star <= max_star_step`).
    explicit_dot: bool,
    /// Under `is_match_path` (or at compile time), a separator-crossing
    /// wildcard (`?` or separator-matching `[...]`) separates an earlier
    /// component-local star from a later component-local star.
    crossing_wildcard_before_local_star: bool,
    /// Strict upper bound on stars in this segment that precede a retryable
    /// barrier; the first star at or past this offset can hand off to the
    /// linear latest-star fast path.
    memo_cutoff: usize,
}

impl RetryableStarSegment {
    const fn any(self) -> bool {
        self.explicit_dot || self.crossing_wildcard_before_local_star
    }
}

/// Classifies whether an outer-star segment starting at `star_start` can
/// require retrying an overwritten star after hitting a hidden-dot or
/// separator barrier.
fn extglob_retryable_star_segment(
    steps: &[ExtglobStep],
    star_start: usize,
    max_star_step: usize,
    options: PatternOptions,
) -> RetryableStarSegment {
    let mut result = RetryableStarSegment::default();
    let allow_path_crossing_wildcards =
        !options.component_wildcards || !options.root_component_wildcards;
    let path_options = PatternOptions {
        component_wildcards: true,
        root_component_wildcards: false,
        ..options
    };
    let mut index = star_start;
    let mut saw_crossing_star = false;
    let mut saw_local_star = false;
    let mut saw_crossing_wildcard_after_local_star = false;
    let mut prev_can_end_at_component_start = false;
    let mut prev_from_crossing_wildcard = false;
    let mut has_explicit_dot = false;
    let mut last_dot_trigger = 0;
    while index <= max_star_step && index < steps.len() {
        match &steps[index] {
            ExtglobStep::Group(_) | ExtglobStep::NoMatch => break,
            ExtglobStep::Star {
                next,
                blocks_leading_period,
                ..
            } => {
                if has_explicit_dot {
                    result.explicit_dot = true;
                    result.memo_cutoff = result.memo_cutoff.max(last_dot_trigger);
                }
                if allow_path_crossing_wildcards
                    && extglob_star_component_local(steps, index, path_options)
                {
                    if saw_crossing_wildcard_after_local_star {
                        result.crossing_wildcard_before_local_star = true;
                        result.memo_cutoff = result.memo_cutoff.max(index);
                    }
                    saw_local_star = true;
                } else {
                    saw_local_star = false;
                    saw_crossing_wildcard_after_local_star = false;
                }
                if !extglob_star_component_local(steps, index, options) {
                    saw_crossing_star = true;
                }
                prev_can_end_at_component_start = !*blocks_leading_period;
                prev_from_crossing_wildcard = false;
                index = *next;
            }
            ExtglobStep::UnclosedGroup { byte: b'*' } => {
                if has_explicit_dot {
                    result.explicit_dot = true;
                    result.memo_cutoff = result.memo_cutoff.max(last_dot_trigger);
                }
                if allow_path_crossing_wildcards
                    && extglob_wildcard_component_local(steps, index, path_options)
                {
                    if saw_crossing_wildcard_after_local_star {
                        result.crossing_wildcard_before_local_star = true;
                        result.memo_cutoff = result.memo_cutoff.max(index);
                    }
                    saw_local_star = true;
                } else {
                    saw_local_star = false;
                    saw_crossing_wildcard_after_local_star = false;
                }
                if !extglob_wildcard_component_local(steps, index, options) {
                    saw_crossing_star = true;
                }
                prev_can_end_at_component_start = false;
                prev_from_crossing_wildcard = false;
                index += 1;
            }
            ExtglobStep::Byte(b'.') => {
                if (saw_crossing_star && prev_can_end_at_component_start)
                    || prev_from_crossing_wildcard
                {
                    has_explicit_dot = true;
                    last_dot_trigger = index + 1;
                }
                prev_can_end_at_component_start = false;
                prev_from_crossing_wildcard = false;
                index += 1;
            }
            ExtglobStep::Escape { escaped: b'.' } => {
                if (saw_crossing_star && prev_can_end_at_component_start)
                    || prev_from_crossing_wildcard
                {
                    has_explicit_dot = true;
                    last_dot_trigger = index + 2;
                }
                prev_can_end_at_component_start = false;
                prev_from_crossing_wildcard = false;
                index += 2;
            }
            ExtglobStep::Separator { .. } => {
                prev_can_end_at_component_start = true;
                prev_from_crossing_wildcard = false;
                index += 1;
            }
            ExtglobStep::Any | ExtglobStep::UnclosedGroup { byte: b'?' } => {
                let can_cross = !extglob_wildcard_component_local(steps, index, options);
                prev_can_end_at_component_start = can_cross;
                prev_from_crossing_wildcard = can_cross;
                if saw_local_star
                    && allow_path_crossing_wildcards
                    && !extglob_wildcard_component_local(steps, index, path_options)
                {
                    saw_crossing_wildcard_after_local_star = true;
                }
                index += 1;
            }
            ExtglobStep::Class { class, next } => {
                let matches_sep = class.matches(b'/', options.case_insensitive)
                    || (cfg!(windows) && class.matches(b'\\', options.case_insensitive));
                let can_cross =
                    !extglob_wildcard_component_local(steps, index, options) && matches_sep;
                prev_can_end_at_component_start = can_cross;
                prev_from_crossing_wildcard = can_cross;
                if saw_local_star
                    && allow_path_crossing_wildcards
                    && !extglob_wildcard_component_local(steps, index, path_options)
                    && matches_sep
                {
                    saw_crossing_wildcard_after_local_star = true;
                }
                index = *next;
            }
            ExtglobStep::Byte(byte) | ExtglobStep::UnclosedGroup { byte } => {
                prev_can_end_at_component_start = is_separator(*byte);
                prev_from_crossing_wildcard = false;
                index += 1;
            }
            ExtglobStep::Escape { escaped } => {
                prev_can_end_at_component_start = is_separator(*escaped);
                prev_from_crossing_wildcard = false;
                index += 2;
            }
        }
    }
    result
}

/// Classifies one byte offset the way the interpreter classified it.
#[allow(clippy::too_many_arguments)]
fn compile_extglob_step(
    pattern: &[u8],
    index: usize,
    prev_was_separator: bool,
    prev_was_any_separator: bool,
    prev_was_recursive_star: bool,
    options: PatternOptions,
    closing_parens: &mut ExtglobClosingParens,
    groups: &mut Vec<ExtglobGroup>,
    budget: &mut IrBudget,
    provenance_budget: &mut ProvenanceBudget,
    walker_source_provenance: Option<&SourceProvenance>,
) -> Result<ExtglobStep, PatternError> {
    if let Some(kind) = detect_extglob_at(pattern, index) {
        let open = index + 1;
        let Some(close) = closing_parens.closing(pattern, open, options.escape) else {
            return Ok(ExtglobStep::UnclosedGroup {
                byte: pattern[index],
            });
        };
        // An alternative stands where its group stands, so a `**` at its edge
        // is a whole component only when the group is (#419).
        let bounds = ComponentBounds {
            starts: starts_component_at(pattern, index, options.escape, ComponentBounds::WHOLE),
            ends: ends_component_at(pattern, close + 1, ComponentBounds::WHOLE),
        };
        // A separator right behind the group is where an alternative's
        // trailing `**` would read as `**/`, zero directories included (#422).
        let separator_follows = pattern.get(close + 1) == Some(&b'/');
        let mut alternatives = Vec::new();
        let mut through_separator = Vec::new();
        for range in split_extglob_alternatives(&pattern[open + 1..close], options.escape) {
            let start = open + 1 + range.start;
            let end = open + 1 + range.end;
            let alternative_provenance = match walker_source_provenance {
                Some(provenance) => Some(provenance.slice(start, end, provenance_budget, index)?),
                None => None,
            };
            let mut alternative = compile_extglob_alternative(
                &pattern[start..end],
                options,
                budget,
                provenance_budget,
                alternative_provenance.as_ref(),
                false,
                bounds,
            )?;
            if kind != ExtglobKind::Negated
                && separator_follows
                && alternative_ends_in_recursive_star(&alternative)
            {
                // Written inline, the trailing `**` and the separator are one
                // `**/`, so the alternative is compiled once more with that
                // separator, exactly as the inlined text tokenizes, and the
                // copy replaces it for matching.
                let mut with_separator = pattern[start..end].to_vec();
                with_separator.push(b'/');
                let absorbing = compile_extglob_alternative(
                    &with_separator,
                    options,
                    budget,
                    provenance_budget,
                    None,
                    false,
                    bounds,
                )?;
                debug_assert!(
                    absorbing.compiled.iter().all(|compiled| matches!(
                        compiled.tokens.last(),
                        Some(Token::RecursivePrefix)
                    )),
                    "the separator turns the trailing `**` into a `**/`"
                );
                through_separator.push(absorbing);
                alternative.absorbed_by_separator = true;
            }
            alternatives.push(alternative);
        }
        let spans_components = alternatives.iter().any(|alternative| {
            alternative.compiled.iter().any(|compiled| {
                compiled.tokens.iter().any(|token| match token {
                    Token::Separator | Token::RecursiveStar | Token::RecursivePrefix => true,
                    Token::Literal(literal) => literal.iter().any(|byte| is_separator(*byte)),
                    Token::Any | Token::Star | Token::Class(_) => false,
                })
            })
        });
        let root_wildcard = alternatives.iter().any(|alternative| {
            alternative.compiled.iter().any(|compiled| {
                matches!(
                    compiled.tokens.first(),
                    Some(Token::Any | Token::Class(_) | Token::Star)
                )
            })
        });
        let idempotent_through_separator = !through_separator.is_empty()
            && alternatives
                .iter()
                .all(|alternative| alternative.absorbed_by_separator)
            && through_separator.iter().all(|alternative| {
                alternative.compiled.iter().all(|compiled| {
                    matches!(compiled.tokens.last(), Some(Token::RecursivePrefix))
                        && (options.match_hidden
                            || !tokens_can_match_hidden_component(
                                &compiled.tokens,
                                options.case_insensitive,
                            ))
                })
            });
        groups.push(ExtglobGroup {
            kind,
            alternatives,
            start: index,
            rest: close + 1,
            // Negated groups behind `**/` stay component-local (#393), while
            // positive groups follow the plain token rule (`Token::RecursivePrefix`
            // does not make a following wildcard component-local under `is_match_path`).
            component_local: if kind == ExtglobKind::Negated {
                prev_was_any_separator
            } else {
                prev_was_separator
            },
            spans_components,
            root_wildcard,
            through_separator,
            idempotent_through_separator,
            entry_memo: GroupEntryMemo::None,
        });
        return Ok(ExtglobStep::Group(groups.len() - 1));
    }
    Ok(match pattern[index] {
        b'/' => ExtglobStep::Separator {
            recursive_prefix: prev_was_recursive_star,
        },
        b'*' => {
            let mut next = index + 1;
            // Shell extglob grammar gives the final star in a run to a closed
            // `*(` group. Keep the preceding stars as one ordinary wildcard,
            // but leave that last offset for the group compiler instead of
            // greedily swallowing it into the run. An unclosed opener keeps
            // the compatible literal-suffix fallback and remains part of the
            // ordinary star run.
            while pattern.get(next) == Some(&b'*') {
                let closed_group_starts_here = detect_extglob_at(pattern, next).is_some()
                    && closing_parens
                        .closing(pattern, next + 1, options.escape)
                        .is_some();
                if closed_group_starts_here {
                    break;
                }
                next += 1;
            }
            ExtglobStep::Star {
                next,
                blocks_leading_period: !(options.recursive_double_star
                    && next - index >= 2
                    && (next - index).is_multiple_of(2)
                    && pattern.get(next) == Some(&b'/')
                    && starts_component_at(pattern, index, options.escape, ComponentBounds::WHOLE)),
                memoize_tail: false,
            }
        }
        b'?' => ExtglobStep::Any,
        // An unparseable class never matched and was never read as a literal
        // bracket either: its arm had no guard to fall through.
        b'[' => match parse_class(pattern, index, options.escape) {
            Ok((class, next)) => ExtglobStep::Class { class, next },
            Err(_) => ExtglobStep::NoMatch,
        },
        b'\\' if options.escape && index + 1 < pattern.len() => ExtglobStep::Escape {
            escaped: pattern[index + 1],
        },
        byte => ExtglobStep::Byte(byte),
    })
}

/// Compiles one alternative as a whole-candidate pattern.
///
/// Braces and extglobs stay off, exactly as the per-match compile had them, so
/// a nested group inside an alternative stays ordinary text. The component
/// policy is left off and supplied by the caller, which is what lets one
/// compiled alternative serve `is_match` and `is_match_glob_path` alike.
fn compile_extglob_alternative(
    alternative: &[u8],
    options: PatternOptions,
    budget: &mut IrBudget,
    provenance_budget: &mut ProvenanceBudget,
    walker_source_provenance: Option<&SourceProvenance>,
    leading_dot_is_normalized: bool,
    bounds: ComponentBounds,
) -> Result<ExtglobAlternative, PatternError> {
    let options = PatternOptions {
        braces: false,
        extglob: false,
        component_wildcards: false,
        root_component_wildcards: false,
        ..options
    };
    let mut compiled = vec![Pattern::compile_single_alternative(
        alternative,
        options,
        budget,
        provenance_budget,
        walker_source_provenance,
        leading_dot_is_normalized,
        bounds,
        false,
    )?];
    // An arm's prefix scan runs the engine whatever fast path the arm has,
    // so the engine deferred for a top-level alternative is built here. The
    // compile already charged it.
    for compiled in &mut compiled {
        if sweep_is_deferred(compiled) {
            compiled.sweep = Some(SweepEngine::build(&compiled.tokens, options)?);
        }
    }
    let width = fixed_token_width(&compiled);
    let requires_component_start = compiled
        .iter()
        .any(|compiled| alternative_starts_with_recursive_prefix(&compiled.tokens));
    Ok(ExtglobAlternative {
        compiled,
        width,
        requires_component_start,
        absorbed_by_separator: false,
    })
}

/// Whether the alternative ends in a whole-component `**` that is not the
/// start of a `**/` (see [`Token::RecursiveStar`]).
fn alternative_ends_in_recursive_star(alternative: &ExtglobAlternative) -> bool {
    alternative
        .compiled
        .iter()
        .any(|compiled| matches!(compiled.tokens.last(), Some(Token::RecursiveStar)))
}

/// Whether the alternative is exactly one whole-component `**`, which at the
/// end of a pattern also accepts the path without the separator in front of
/// it, as `x/**` accepts `x`.
fn alternative_is_globstar(alternative: &ExtglobAlternative) -> bool {
    alternative
        .compiled
        .iter()
        .any(|compiled| matches!(compiled.tokens.as_slice(), [Token::RecursiveStar]))
}

/// The step offset of the separator in front of a trailing whole-component
/// `**`, which a candidate that ends right before it does not need (#422).
///
/// The token engine lets a trailing `/**` accept the path without it, so
/// `x/**` matches `x`. The program keeps that rule where the `**` is an outer
/// star run behind a group, as in `@(x)/**`, and where it is the only
/// alternative of a trailing positive group, as in `x/@(**)`.
fn trailing_globstar_separator(
    pattern: &[u8],
    steps: &[ExtglobStep],
    groups: &[ExtglobGroup],
    options: PatternOptions,
) -> Option<usize> {
    if !options.recursive_double_star {
        return None;
    }
    // An outer run of exactly two stars that ends the program.
    let outer_star = steps.len().checked_sub(2).filter(
        |&star| matches!(steps[star], ExtglobStep::Star { next, .. } if next == steps.len()),
    );
    // Otherwise a trailing positive group with a `**` alternative. Its
    // interior is `NoMatch` filler, so it is found by where it ends.
    let globstar = match outer_star {
        Some(star) => star,
        None => {
            groups
                .iter()
                .find(|group| {
                    group.rest == steps.len()
                        && group.kind != ExtglobKind::Negated
                        && matches!(steps.get(group.start), Some(ExtglobStep::Group(_)))
                        && group.alternatives.iter().any(alternative_is_globstar)
                })?
                .start
        }
    };
    // Either one is a whole component only behind an unescaped separator.
    let separator = globstar.checked_sub(1)?;
    (matches!(steps[separator], ExtglobStep::Separator { .. })
        && starts_component_at(
            pattern,
            separator + 1,
            options.escape,
            ComponentBounds::WHOLE,
        ))
    .then_some(separator)
}

/// Total bytes the alternative consumes, when that is the same for every
/// candidate it accepts.
///
/// Braces are off while compiling one, so there is exactly one token list to
/// measure; a star of any kind makes the width vary and gives up.
fn fixed_token_width(alternatives: &[CompiledAlternative]) -> Option<usize> {
    let [alternative] = alternatives else {
        return None;
    };
    let mut width = 0_usize;
    for token in &alternative.tokens {
        width += match token {
            Token::Literal(literal) => literal.len(),
            Token::Separator | Token::Any | Token::Class(_) => 1,
            Token::Star | Token::RecursiveStar | Token::RecursivePrefix => {
                return None;
            }
        };
    }
    Some(width)
}

/// Scans the character class starting at `start`, mirroring [`parse_class`]
/// closing rules without allocating. Returns
/// `(next, has_unescaped_brace, raw_closing_braces)` when closed, or `None`
/// when the class is unclosed or ends in a trailing escape.
fn scan_class_end(pattern: &[u8], start: usize, escapes: bool) -> Option<(usize, bool, usize)> {
    let mut index = start + 1;
    let mut has_unescaped_brace = false;
    let mut raw_closing_braces = 0_usize;
    if pattern
        .get(index)
        .is_some_and(|byte| matches!(byte, b'!' | b'^'))
    {
        index += 1;
    }
    if pattern.get(index) == Some(&b']') {
        index += 1;
    }
    while let Some(&byte) = pattern.get(index) {
        if byte == b']' {
            return Some((index + 1, has_unescaped_brace, raw_closing_braces));
        }
        if byte == b'[' && pattern.get(index + 1) == Some(&b':') {
            let name_start = index + 2;
            let search_end = pattern.len().min(name_start + MAX_POSIX_CLASS_NAME_LEN + 2);
            if let Some(posix_end) = memmem::find(&pattern[name_start..search_end], b":]")
                && parse_posix_class(&pattern[name_start..name_start + posix_end]).is_some()
            {
                index = name_start + posix_end + 2;
                continue;
            }
        }
        if byte == b'\\' && escapes {
            let &escaped = pattern.get(index + 1)?;
            raw_closing_braces += usize::from(escaped == b'}');
            index += 2;
        } else {
            has_unescaped_brace |= matches!(byte, b'{' | b'}');
            raw_closing_braces += usize::from(byte == b'}');
            index += 1;
        }
    }
    None
}

fn contains_extglob(pattern: &[u8], escapes: bool) -> bool {
    let mut index = 0;
    let mut saw_unclosed_class = false;
    while index + 1 < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += 2;
            continue;
        }
        if !saw_unclosed_class && pattern[index] == b'[' {
            if let Some((next, _, _)) = scan_class_end(pattern, index, escapes) {
                index = next;
                continue;
            }
            // Once an unclosed `[` has scanned to the end without a matching
            // `]`, no later `[` can close over an extglob opener either (only a
            // lowercase POSIX `[:name:]` class could close, which cannot hold
            // an extglob opener). Stopping retries keeps malformed inputs O(N).
            saw_unclosed_class = true;
        }
        if detect_extglob_at(pattern, index).is_some() {
            return true;
        }
        index += 1;
    }
    false
}

fn detect_extglob_at(pattern: &[u8], index: usize) -> Option<ExtglobKind> {
    if pattern.get(index + 1) != Some(&b'(') {
        return None;
    }
    match pattern[index] {
        b'?' => Some(ExtglobKind::Optional),
        b'*' => Some(ExtglobKind::ZeroOrMore),
        b'+' => Some(ExtglobKind::OneOrMore),
        b'@' => Some(ExtglobKind::ExactlyOne),
        b'!' => Some(ExtglobKind::Negated),
        _ => None,
    }
}

#[cfg(test)]
thread_local! {
    static EXTGLOB_CLOSING_TABLE_LEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static EXTGLOB_CLOSING_SCANNED_BYTES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn extglob_closing_table_len() -> usize {
    EXTGLOB_CLOSING_TABLE_LEN.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_extglob_closing_scanned_bytes() {
    EXTGLOB_CLOSING_SCANNED_BYTES.with(|cell| cell.set(0));
}

#[cfg(test)]
fn extglob_closing_scanned_bytes() -> usize {
    EXTGLOB_CLOSING_SCANNED_BYTES.with(std::cell::Cell::get)
}

/// Result of one forward scan for an extglob opener's matching `)`.
enum ClosingParenScan {
    Found(usize),
    Unclosed {
        last_close: Option<usize>,
        next_extglob_open: Option<usize>,
    },
}

/// Lazily resolves matching `)` offsets for extglob `(` openers.
///
/// Closed groups use a direct forward scan with zero allocation. Unclosed
/// openers collect `last_close` and `next_extglob_open` during the same forward
/// scan (never scanning the prefix behind `open`). Only after a third unclosed
/// opener still has another extglob opener strictly before `last_close` does it
/// build one lookup table over `next_extglob_open..=last_close`.
enum ExtglobClosingParens {
    Direct,
    NoCloseAtOrAfter(usize),
    PendingDirect { last_close: usize },
    ReadyToIndex { last_close: usize },
    Table { base: usize, closes: Vec<usize> },
}

impl ExtglobClosingParens {
    fn closing(&mut self, pattern: &[u8], open: usize, escapes: bool) -> Option<usize> {
        match self {
            Self::Table { base, closes } if open >= *base => closes
                .get(open - *base)
                .copied()
                .filter(|&close| close != usize::MAX)
                .map(|rel| *base + rel),
            Self::Table { .. } => closing_extglob_parenthesis(pattern, open, escapes),
            Self::NoCloseAtOrAfter(bound) if open >= *bound => None,
            Self::ReadyToIndex { last_close } => {
                if open >= *last_close {
                    return None;
                }
                match scan_closing_extglob_parenthesis(pattern, open, escapes) {
                    ClosingParenScan::Found(close) => Some(close),
                    ClosingParenScan::Unclosed {
                        last_close: Some(last),
                        next_extglob_open: Some(next_open),
                    } => {
                        let closes = precompute_extglob_closing_parentheses(
                            &pattern[next_open..=last],
                            escapes,
                        );
                        #[cfg(test)]
                        EXTGLOB_CLOSING_TABLE_LEN.with(|cell| cell.set(closes.len()));
                        *self = Self::Table {
                            base: next_open,
                            closes,
                        };
                        None
                    }
                    ClosingParenScan::Unclosed { last_close, .. } => {
                        *self = Self::NoCloseAtOrAfter(last_close.unwrap_or(open));
                        None
                    }
                }
            }
            Self::PendingDirect { last_close } => {
                if open >= *last_close {
                    return None;
                }
                match scan_closing_extglob_parenthesis(pattern, open, escapes) {
                    ClosingParenScan::Found(close) => Some(close),
                    ClosingParenScan::Unclosed {
                        last_close: Some(last),
                        next_extglob_open: Some(_),
                    } => {
                        *self = Self::ReadyToIndex { last_close: last };
                        None
                    }
                    ClosingParenScan::Unclosed { last_close, .. } => {
                        *self = Self::NoCloseAtOrAfter(last_close.unwrap_or(open));
                        None
                    }
                }
            }
            Self::Direct | Self::NoCloseAtOrAfter(_) => {
                match scan_closing_extglob_parenthesis(pattern, open, escapes) {
                    ClosingParenScan::Found(close) => Some(close),
                    ClosingParenScan::Unclosed {
                        last_close: Some(last),
                        next_extglob_open: Some(_),
                    } => {
                        *self = Self::PendingDirect { last_close: last };
                        None
                    }
                    ClosingParenScan::Unclosed { last_close, .. } => {
                        *self = Self::NoCloseAtOrAfter(last_close.unwrap_or(open));
                        None
                    }
                }
            }
        }
    }
}

/// Precomputes the matching `)` offset for every unescaped `(` in `slice`.
fn precompute_extglob_closing_parentheses(slice: &[u8], escapes: bool) -> Vec<usize> {
    let mut closing = vec![usize::MAX; slice.len()];
    let mut open_stack = Vec::new();
    #[cfg(test)]
    let mut scanned = 0_usize;
    let mut index = 0;
    while index < slice.len() {
        #[cfg(test)]
        {
            scanned += 1;
        }
        if escapes && slice[index] == b'\\' {
            index += 2;
            continue;
        }
        match slice[index] {
            b'(' => open_stack.push(index),
            b')' => {
                if let Some(open) = open_stack.pop() {
                    closing[open] = index;
                }
            }
            _ => {}
        }
        index += 1;
    }
    #[cfg(test)]
    EXTGLOB_CLOSING_SCANNED_BYTES.with(|cell| cell.set(cell.get() + scanned));
    closing
}

fn scan_closing_extglob_parenthesis(
    pattern: &[u8],
    open: usize,
    escapes: bool,
) -> ClosingParenScan {
    if pattern.get(open) != Some(&b'(') {
        return ClosingParenScan::Unclosed {
            last_close: None,
            next_extglob_open: None,
        };
    }
    let mut depth = 1_usize;
    let mut last_close = None;
    let mut first_extglob_open = None;
    let mut prev_was_unescaped_extglob_op = false;
    #[cfg(test)]
    let mut scanned = 0_usize;
    let mut index = open + 1;
    while index < pattern.len() {
        #[cfg(test)]
        {
            scanned += 1;
        }
        if escapes && pattern[index] == b'\\' {
            prev_was_unescaped_extglob_op = false;
            index += 2;
            continue;
        }
        let byte = pattern[index];
        match byte {
            b'(' => {
                if first_extglob_open.is_none() && prev_was_unescaped_extglob_op {
                    first_extglob_open = Some(index);
                }
                depth += 1;
                prev_was_unescaped_extglob_op = false;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    #[cfg(test)]
                    EXTGLOB_CLOSING_SCANNED_BYTES.with(|cell| cell.set(cell.get() + scanned));
                    return ClosingParenScan::Found(index);
                }
                last_close = Some(index);
                prev_was_unescaped_extglob_op = false;
            }
            _ => {
                prev_was_unescaped_extglob_op = matches!(byte, b'?' | b'*' | b'+' | b'@' | b'!');
            }
        }
        index += 1;
    }
    #[cfg(test)]
    EXTGLOB_CLOSING_SCANNED_BYTES.with(|cell| cell.set(cell.get() + scanned));
    let next_extglob_open =
        first_extglob_open.filter(|&pos| last_close.is_some_and(|last| pos < last));
    ClosingParenScan::Unclosed {
        last_close,
        next_extglob_open,
    }
}

fn closing_extglob_parenthesis(pattern: &[u8], open: usize, escapes: bool) -> Option<usize> {
    match scan_closing_extglob_parenthesis(pattern, open, escapes) {
        ClosingParenScan::Found(close) => Some(close),
        ClosingParenScan::Unclosed { .. } => None,
    }
}

fn split_extglob_alternatives(content: &[u8], escapes: bool) -> Vec<std::ops::Range<usize>> {
    let mut alternatives = Vec::new();
    let mut start = 0;
    let mut depth = 0_usize;
    let mut index = 0;
    while index < content.len() {
        if escapes && content[index] == b'\\' {
            index += 2;
            continue;
        }
        match content[index] {
            b'(' => depth += 1,
            b')' if depth > 0 => depth -= 1,
            b'|' if depth == 0 => {
                alternatives.push(start..index);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    alternatives.push(start..content.len());
    alternatives
}

/// Reusable state for one extglob match on a thread.
#[derive(Default)]
struct ExtglobScratch {
    /// One visited-position frame per repetition in flight.
    visited: Vec<u64>,
    /// Dense rows for extglob-recursive interpreter offsets. Each row lazily
    /// allocates fixed-size candidate pages as states are explored.
    failed: Vec<ExtglobFailedRow>,
    #[cfg(test)]
    /// The previous match's materialized failure pages, retained only as
    /// deterministic test instrumentation.
    failed_page_count: usize,
    #[cfg(test)]
    /// The previous match's distinct failed states, retained only as
    /// deterministic test instrumentation.
    failed_state_count: usize,
    #[cfg(test)]
    /// Largest number of deferred continuations live during the previous
    /// match. This proves duplicate states never inflate the worklist.
    pending_peak: usize,
    #[cfg(test)]
    /// Number of group expansions executed during the previous match. This
    /// proves star backtracking never re-expands the same `(group, path_index)`.
    group_expansions: usize,
    /// Deferred `(program, candidate)` continuations. Extglob groups append
    /// work here instead of recursing through the native stack.
    pending: Vec<(usize, usize)>,
    /// Non-retryable tail stars handed off from `match_extglob_task_memoized`
    /// to the linear latest-star matcher with shared star-state deduplication.
    handoff_pending: Vec<(usize, usize)>,
    /// Memoized outer-star segment continuations `(program, candidate)` used
    /// only when a retryable outer-star segment hits a hidden-dot or separator
    /// barrier behind an overwritten earlier star.
    memo_pending: Vec<(usize, usize)>,
    /// Deferred memoized outer-star retries queued after fast-path failure.
    deferred_retries: Vec<DeferredMemoRetry>,
    /// Reused match ends for ordinary and repeated extglob groups.
    ends: Vec<usize>,
    /// Temporary ends while one repetition alternative falls back to the
    /// general matcher.
    candidate_ends: Vec<usize>,
    /// Negated groups mark rejected offsets here before queuing the rest.
    excluded: Vec<bool>,
    /// Reusable Shift-And registers for narrow repetition alternatives.
    narrow_sweeps: Vec<u64>,
    /// Reusable heap-backed Shift-And states for wide repetition alternatives,
    /// kept separate from `narrow_sweeps` so mixed narrow/wide groups never
    /// evict retained wide buffers (#495).
    wide_sweeps: Vec<SweepState>,
    /// The two word rows used while a wide ordinary-group alternative reports
    /// all matching prefix ends. Narrow alternatives need no retained state.
    prefix_sweep_state: Option<SweepState>,
}

/// A deferred memoized outer-star retry queued when the latest-star fast path
/// hits a hidden-component or separator barrier behind an overwritten star.
struct DeferredMemoRetry {
    star_start: usize,
    star_start_path: usize,
    already_marked: bool,
    monotonic_group: Option<(usize, (usize, usize))>,
}

/// Borrows the extglob scratch buffers for one match.
struct ExtglobMatchState<'scratch> {
    visited: &'scratch mut Vec<u64>,
    failed: ExtglobFailedStates<'scratch>,
    pending: &'scratch mut Vec<(usize, usize)>,
    handoff_pending: &'scratch mut Vec<(usize, usize)>,
    memo_pending: &'scratch mut Vec<(usize, usize)>,
    deferred_retries: &'scratch mut Vec<DeferredMemoRetry>,
    #[cfg(test)]
    pending_peak: &'scratch mut usize,
    #[cfg(test)]
    group_expansions: &'scratch mut usize,
    ends: &'scratch mut Vec<usize>,
    candidate_ends: &'scratch mut Vec<usize>,
    excluded: &'scratch mut Vec<bool>,
    narrow_sweeps: &'scratch mut Vec<u64>,
    wide_sweeps: &'scratch mut Vec<SweepState>,
    max_narrow_sweeps: usize,
    max_wide_sweeps: usize,
    prefix_sweep_state: &'scratch mut Option<SweepState>,
}

thread_local! {
    static EXTGLOB_SCRATCH: RefCell<ExtglobScratch> = RefCell::new(ExtglobScratch::default());
    #[cfg(test)]
    static EXTGLOB_MEMOIZED_STAR_FALLBACKS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    #[cfg(test)]
    static EXTGLOB_MEMOIZED_STAR_VISITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    #[cfg(test)]
    static EXTGLOB_REPETITION_SWEEP_STEPS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    #[cfg(test)]
    static EXTGLOB_SWEEP_CLEANUP_SCANS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_extglob_memoized_star_fallbacks() {
    EXTGLOB_MEMOIZED_STAR_FALLBACKS.with(|count| count.set(0));
    EXTGLOB_MEMOIZED_STAR_VISITS.with(|count| count.set(0));
}

#[cfg(test)]
fn reset_extglob_repetition_sweep_steps() {
    EXTGLOB_REPETITION_SWEEP_STEPS.with(|count| count.set(0));
}

#[cfg(test)]
fn extglob_repetition_sweep_steps() -> usize {
    EXTGLOB_REPETITION_SWEEP_STEPS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_extglob_sweep_cleanup_scans() {
    EXTGLOB_SWEEP_CLEANUP_SCANS.with(|count| count.set(0));
}

#[cfg(test)]
fn extglob_sweep_cleanup_scans() -> usize {
    EXTGLOB_SWEEP_CLEANUP_SCANS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn extglob_memoized_star_fallbacks() -> usize {
    EXTGLOB_MEMOIZED_STAR_FALLBACKS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn extglob_memoized_star_visits() -> usize {
    EXTGLOB_MEMOIZED_STAR_VISITS.with(std::cell::Cell::get)
}

/// Runs a compiled extglob program on the thread's reusable visited buffer.
///
/// That buffer is separate from the general matcher's scratch on purpose: an
/// alternative is matched through [`Pattern::match_alternatives`], which takes
/// the general scratch, so holding both at once would push every sub-match
/// onto the re-entrancy fallback and allocate there.
fn match_extglob_program(program: &CompiledExtglob, path: &[u8], options: PatternOptions) -> bool {
    let prefix = &program.literal_prefix;
    if path.len() < prefix.len()
        || !prefix.iter().zip(path).all(|(&expected, &actual)| {
            if expected == b'/' {
                is_separator(actual)
            } else {
                bytes_equal(expected, actual, options.case_insensitive)
            }
        })
    {
        return false;
    }
    if let Some(nfa) = &program.positive_nfa {
        return nfa.is_match(path, options);
    }
    EXTGLOB_SCRATCH.with(|cell| match cell.try_borrow_mut() {
        Ok(mut scratch) => {
            let (matched, max_narrow_sweeps, max_wide_sweeps) = {
                let ExtglobScratch {
                    visited,
                    failed,
                    #[cfg(test)]
                    failed_page_count,
                    #[cfg(test)]
                    failed_state_count,
                    #[cfg(test)]
                    pending_peak,
                    #[cfg(test)]
                    group_expansions,
                    pending,
                    handoff_pending,
                    memo_pending,
                    deferred_retries,
                    ends,
                    candidate_ends,
                    excluded,
                    narrow_sweeps,
                    wide_sweeps,
                    prefix_sweep_state,
                } = &mut *scratch;
                let use_retries = program.has_retryable_star_segment
                    && (!options.match_hidden || options.component_wildcards);
                visited.clear();
                failed.clear();
                pending.clear();
                if use_retries {
                    handoff_pending.clear();
                    memo_pending.clear();
                    deferred_retries.clear();
                }
                ends.clear();
                candidate_ends.clear();
                excluded.clear();
                #[cfg(test)]
                {
                    *failed_page_count = 0;
                    *failed_state_count = 0;
                    *pending_peak = 0;
                    *group_expansions = 0;
                }
                let mut state = ExtglobMatchState {
                    visited,
                    failed: ExtglobFailedStates::new(
                        program,
                        failed,
                        #[cfg(test)]
                        failed_page_count,
                        #[cfg(test)]
                        failed_state_count,
                    ),
                    pending,
                    handoff_pending,
                    memo_pending,
                    deferred_retries,
                    #[cfg(test)]
                    pending_peak,
                    #[cfg(test)]
                    group_expansions,
                    ends,
                    candidate_ends,
                    excluded,
                    narrow_sweeps,
                    wide_sweeps,
                    max_narrow_sweeps: 0,
                    max_wide_sweeps: 0,
                    prefix_sweep_state,
                };
                let matched = match_extglob_from(program, path, 0, 0, options, &mut state);
                (matched, state.max_narrow_sweeps, state.max_wide_sweeps)
            };
            scratch.visited.clear();
            if scratch.visited.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.visited.shrink_to(RETAINED_SCRATCH_WORDS);
            }
            scratch.failed.clear();
            let retained_rows = RETAINED_SCRATCH_WORDS * std::mem::size_of::<u64>()
                / std::mem::size_of::<ExtglobFailedRow>();
            if scratch.failed.capacity() > retained_rows {
                scratch.failed.shrink_to(retained_rows);
            }
            scratch.pending.clear();
            if scratch.pending.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.pending.shrink_to(RETAINED_SCRATCH_WORDS);
            }
            if program.has_retryable_star_segment
                && (!options.match_hidden || options.component_wildcards)
            {
                scratch.handoff_pending.clear();
                if scratch.handoff_pending.capacity() > RETAINED_SCRATCH_WORDS {
                    scratch.handoff_pending.shrink_to(RETAINED_SCRATCH_WORDS);
                }
                scratch.memo_pending.clear();
                if scratch.memo_pending.capacity() > RETAINED_SCRATCH_WORDS {
                    scratch.memo_pending.shrink_to(RETAINED_SCRATCH_WORDS);
                }
                scratch.deferred_retries.clear();
                if scratch.deferred_retries.capacity() > RETAINED_SCRATCH_WORDS {
                    scratch.deferred_retries.shrink_to(RETAINED_SCRATCH_WORDS);
                }
            }
            scratch.ends.clear();
            if scratch.ends.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.ends.shrink_to(RETAINED_SCRATCH_WORDS);
            }
            scratch.candidate_ends.clear();
            if scratch.candidate_ends.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.candidate_ends.shrink_to(RETAINED_SCRATCH_WORDS);
            }
            scratch.excluded.clear();
            if scratch.excluded.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.excluded.shrink_to(RETAINED_SCRATCH_WORDS);
            }
            // Keep narrow and wide repetition sweep buffers retained separately
            // across calls so mixed narrow/wide groups never evict wide buffers,
            // while matches without repetitions skip cleanup completely (#495).
            if max_narrow_sweeps > 0 && scratch.narrow_sweeps.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.narrow_sweeps.truncate(RETAINED_SCRATCH_WORDS);
                scratch.narrow_sweeps.shrink_to(RETAINED_SCRATCH_WORDS);
            }
            if max_wide_sweeps > 0 {
                if scratch.wide_sweeps.len() > RETAINED_WIDE_SWEEP_SLOTS {
                    scratch.wide_sweeps.truncate(RETAINED_WIDE_SWEEP_SLOTS);
                }
                if scratch.wide_sweeps.capacity() > RETAINED_WIDE_SWEEP_SLOTS {
                    scratch.wide_sweeps.shrink_to(RETAINED_WIDE_SWEEP_SLOTS);
                }
                #[cfg(test)]
                EXTGLOB_SWEEP_CLEANUP_SCANS
                    .with(|count| count.set(count.get() + scratch.wide_sweeps.len()));
                // Enforce a single combined word budget across every retained
                // wide sweep slot so distinct wide patterns cannot accumulate
                // megabytes of pinned buffers on one thread (#495).
                let mut retained_words = 0_usize;
                for state in &mut scratch.wide_sweeps {
                    let words = SweepEngine::retained_state_capacity(state);
                    if retained_words.saturating_add(words) > RETAINED_SCRATCH_WORDS {
                        *state = SweepState::Narrow(0);
                    } else {
                        retained_words += words;
                    }
                }
            }
            if scratch.prefix_sweep_state.as_ref().is_some_and(|state| {
                SweepEngine::state_exceeds_retained_words(state, RETAINED_SCRATCH_WORDS)
            }) {
                scratch.prefix_sweep_state = None;
            }
            matched
        }
        Err(_) => {
            let mut scratch = ExtglobScratch::default();
            let ExtglobScratch {
                visited,
                failed,
                #[cfg(test)]
                failed_page_count,
                #[cfg(test)]
                failed_state_count,
                #[cfg(test)]
                pending_peak,
                #[cfg(test)]
                group_expansions,
                pending,
                handoff_pending,
                memo_pending,
                deferred_retries,
                ends,
                candidate_ends,
                excluded,
                narrow_sweeps,
                wide_sweeps,
                prefix_sweep_state,
            } = &mut scratch;
            let mut state = ExtglobMatchState {
                visited,
                failed: ExtglobFailedStates::new(
                    program,
                    failed,
                    #[cfg(test)]
                    failed_page_count,
                    #[cfg(test)]
                    failed_state_count,
                ),
                pending,
                handoff_pending,
                memo_pending,
                deferred_retries,
                #[cfg(test)]
                pending_peak,
                #[cfg(test)]
                group_expansions,
                ends,
                candidate_ends,
                excluded,
                narrow_sweeps,
                wide_sweeps,
                max_narrow_sweeps: 0,
                max_wide_sweeps: 0,
                prefix_sweep_state,
            };
            match_extglob_from(program, path, 0, 0, options, &mut state)
        }
    })
}

/// Scratch capacities after an extglob match, used to keep the retained
/// thread-local allocation bounded without relying on process RSS.
#[cfg(test)]
fn extglob_scratch_capacities() -> (usize, usize, usize, usize, usize, usize, usize, usize) {
    EXTGLOB_SCRATCH.with(|cell| {
        let scratch = cell.borrow();
        (
            scratch.visited.capacity(),
            scratch.failed.capacity() * std::mem::size_of::<ExtglobFailedRow>()
                / std::mem::size_of::<u64>(),
            scratch.pending.capacity(),
            scratch.ends.capacity(),
            scratch.candidate_ends.capacity(),
            scratch.excluded.capacity(),
            scratch.narrow_sweeps.capacity() + scratch.wide_sweeps.capacity(),
            scratch
                .prefix_sweep_state
                .as_ref()
                .map_or(0, SweepEngine::retained_state_capacity),
        )
    })
}

#[cfg(test)]
fn extglob_failed_len() -> usize {
    EXTGLOB_SCRATCH.with(|cell| cell.borrow().failed.len())
}

#[cfg(test)]
fn extglob_failed_stats() -> (usize, usize) {
    EXTGLOB_SCRATCH.with(|cell| {
        let scratch = cell.borrow();
        (scratch.failed_page_count, scratch.failed_state_count)
    })
}

#[cfg(test)]
fn extglob_pending_peak() -> usize {
    EXTGLOB_SCRATCH.with(|cell| cell.borrow().pending_peak)
}

#[cfg(test)]
fn extglob_group_expansions() -> usize {
    EXTGLOB_SCRATCH.with(|cell| cell.borrow().group_expansions)
}

/// Failure memoization for a single dense interpreter row.
#[derive(Default)]
struct ExtglobFailedRow {
    pages: BTreeMap<usize, Box<[u64; EXTGLOB_MEMO_PAGE_WORDS]>>,
}

/// A page holds 4,096 candidate offsets. This keeps a sparse failed-state
/// search compact without allocating the full state-by-candidate product.
const EXTGLOB_MEMO_PAGE_WORDS: usize = 64;
const EXTGLOB_MEMO_PAGE_BITS: usize = EXTGLOB_MEMO_PAGE_WORDS * u64::BITS as usize;

/// A sparse paged bitset over dense extglob interpreter rows and candidate
/// offsets. Its boolean semantics are identical to a flat matrix, while only
/// recurrence states actually reached by the match materialize storage.
struct ExtglobFailedStates<'scratch> {
    rows: &'scratch mut Vec<ExtglobFailedRow>,
    #[cfg(test)]
    page_count: &'scratch mut usize,
    #[cfg(test)]
    state_count: &'scratch mut usize,
}

impl<'scratch> ExtglobFailedStates<'scratch> {
    fn new(
        program: &CompiledExtglob,
        scratch: &'scratch mut Vec<ExtglobFailedRow>,
        #[cfg(test)] page_count: &'scratch mut usize,
        #[cfg(test)] state_count: &'scratch mut usize,
    ) -> Self {
        scratch.resize_with(program.fast_memo_state_count, ExtglobFailedRow::default);
        Self {
            rows: scratch,
            #[cfg(test)]
            page_count,
            #[cfg(test)]
            state_count,
        }
    }

    fn grow_to(&mut self, state_count: usize) {
        if self.rows.len() < state_count {
            self.rows
                .resize_with(state_count, ExtglobFailedRow::default);
        }
    }

    fn insert(&mut self, state_index: usize, path_index: usize) -> bool {
        let candidate_page = path_index / EXTGLOB_MEMO_PAGE_BITS;
        let page_bit = path_index % EXTGLOB_MEMO_PAGE_BITS;
        let row = &mut self.rows[state_index];
        let words = match row.pages.entry(candidate_page) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let words = entry.insert(Box::new([0; EXTGLOB_MEMO_PAGE_WORDS]));
                #[cfg(test)]
                {
                    *self.page_count += 1;
                }
                words
            }
        };
        let word = &mut words[page_bit / u64::BITS as usize];
        let mask = 1_u64 << (page_bit % u64::BITS as usize);
        if *word & mask != 0 {
            false
        } else {
            *word |= mask;
            #[cfg(test)]
            {
                *self.state_count += 1;
            }
            true
        }
    }
}

/// Reserves a zeroed frame of `positions` bits and returns its first word.
fn push_visited(visited: &mut Vec<u64>, positions: usize) -> usize {
    let base = visited.len();
    let words = positions.div_ceil(u64::BITS as usize);
    visited.resize(base + words, 0);
    base
}

/// Records `position` in the frame at `base`, reporting whether it is new.
fn visit(visited: &mut [u64], base: usize, position: usize) -> bool {
    let word = &mut visited[base + position / u64::BITS as usize];
    let mask = 1_u64 << (position % u64::BITS as usize);
    if *word & mask != 0 {
        false
    } else {
        *word |= mask;
        true
    }
}

fn visited(visited: &[u64], base: usize, position: usize) -> bool {
    visited[base + position / u64::BITS as usize] & (1_u64 << (position % u64::BITS as usize)) != 0
}

fn match_extglob_from(
    program: &CompiledExtglob,
    path: &[u8],
    start: usize,
    start_path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) -> bool {
    let use_retries = program.has_retryable_star_segment
        && (!options.match_hidden || options.component_wildcards);
    if !use_retries {
        match_extglob_from_inner::<false>(program, path, start, start_path_index, options, state)
    } else {
        match_extglob_from_retryable(program, path, start, start_path_index, options, state)
    }
}

#[cold]
#[inline(never)]
fn match_extglob_from_retryable(
    program: &CompiledExtglob,
    path: &[u8],
    start: usize,
    start_path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) -> bool {
    match_extglob_from_inner::<true>(program, path, start, start_path_index, options, state)
}

#[inline(always)]
fn match_extglob_from_inner<const TRACK_RETRIES: bool>(
    program: &CompiledExtglob,
    path: &[u8],
    start: usize,
    start_path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) -> bool {
    let pending_base = state.pending.len();
    let handoff_pending_base = if TRACK_RETRIES {
        state.handoff_pending.len()
    } else {
        0
    };
    let memo_pending_base = if TRACK_RETRIES {
        state.memo_pending.len()
    } else {
        0
    };
    let deferred_retries_base = if TRACK_RETRIES {
        state.deferred_retries.len()
    } else {
        0
    };
    queue_extglob_continuation(program, state, start, start_path_index);
    let mut matched = false;
    let mut active_monotonic_group: Option<(usize, (usize, usize))> = None;
    loop {
        if state.pending.len() > pending_base {
            let (task_start, task_path) = state
                .pending
                .pop()
                .expect("the extglob continuation slice is non-empty");
            if match_extglob_task::<TRACK_RETRIES>(
                program, path, task_start, task_path, false, options, state,
            ) {
                matched = true;
                break;
            }
            continue;
        }
        if TRACK_RETRIES && state.handoff_pending.len() > handoff_pending_base {
            let (task_start, task_path) = state
                .handoff_pending
                .pop()
                .expect("the handed-off tail worklist is non-empty");
            if match_extglob_task::<true>(
                program, path, task_start, task_path, true, options, state,
            ) {
                matched = true;
                break;
            }
            continue;
        }
        if TRACK_RETRIES && state.memo_pending.len() > memo_pending_base {
            let (task_start, task_path) = state
                .memo_pending
                .pop()
                .expect("the memoized extglob worklist is non-empty");
            if match_extglob_task_memoized(
                program,
                path,
                task_start,
                task_path,
                active_monotonic_group,
                options,
                state,
            ) {
                matched = true;
                break;
            }
            continue;
        }
        if TRACK_RETRIES && state.deferred_retries.len() > deferred_retries_base {
            let retry = state
                .deferred_retries
                .pop()
                .expect("the deferred extglob retry stack is non-empty");
            state.failed.grow_to(program.memo_state_count);
            let retry_memo = program.memo_state_index(retry.star_start);
            if retry.already_marked || state.failed.insert(retry_memo, retry.star_start_path) {
                #[cfg(test)]
                EXTGLOB_MEMOIZED_STAR_FALLBACKS.with(|count| count.set(count.get() + 1));
                active_monotonic_group = retry.monotonic_group;
                state
                    .memo_pending
                    .push((retry.star_start, retry.star_start_path));
                #[cfg(test)]
                {
                    *state.pending_peak = (*state.pending_peak).max(
                        state.pending.len()
                            + state.handoff_pending.len()
                            + state.memo_pending.len(),
                    );
                }
            }
            continue;
        }
        break;
    }
    if TRACK_RETRIES {
        state.deferred_retries.truncate(deferred_retries_base);
        state.memo_pending.truncate(memo_pending_base);
        state.handoff_pending.truncate(handoff_pending_base);
    }
    state.pending.truncate(pending_base);
    matched
}

/// Saved backtrack state for a separator-crossing outer star that a later
/// component-local star superseded.
#[derive(Clone, Copy)]
struct CrossingStar {
    resume: usize,
    consumed: usize,
    memo_index: usize,
    blocks_leading_period: bool,
}

/// Runs one deterministic outer-program branch. Groups append their possible
/// continuations to the shared work list, so sequential groups consume heap
/// work rather than native call frames. A preceding plain star keeps its local
/// two-pointer backtrack state and queues the group for each reachable split.
///
/// If an overwritten earlier star in a retryable outer-star segment hits a
/// hidden-dot or separator barrier, the task queues an iterative memoized retry
/// from that segment's first star.
#[allow(clippy::too_many_arguments)]
fn match_extglob_task<const TRACK_RETRIES: bool>(
    program: &CompiledExtglob,
    path: &[u8],
    start: usize,
    start_path_index: usize,
    dedup_stars: bool,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) -> bool {
    let steps = &program.steps;
    let mut pattern_index = start;
    let mut path_index = start_path_index;
    let mut star_pattern_index = 0_usize;
    let mut star_path_index = 0_usize;
    let mut star_memo_index = 0_usize;
    let mut has_star = false;
    let mut star_component_local = false;
    let mut star_blocks_leading_period = false;
    let mut monotonic_range: Option<(usize, usize)> = None;
    let mut monotonic_group_index = 0_usize;
    let mut rewound_crossing_star = false;
    // The last separator-crossing star that a later component-local star
    // replaced as the backtrack point. Only the latest star is normally kept,
    // because it can absorb whatever an earlier one would have: a
    // component-local star cannot absorb a separator, so when it reaches one,
    // the crossing star before it has to take the next byte instead. Without
    // this, `**/*.@(x)` refused `a/b/c.x` under the component policies
    // although `**/*.x` accepts it.
    let mut crossing_star: Option<CrossingStar> = None;
    let mut first_retryable_star: Option<(usize, usize)> = None;
    let mut max_star_step = 0_usize;
    let mut earlier_retryable_star_overwritten = false;
    let mut hit_hidden_barrier = false;
    let mut hit_separator_barrier = false;

    while path_index < path.len() || pattern_index < steps.len() {
        if pattern_index < steps.len() {
            let leading_period_blocked = !options.match_hidden
                && path.get(path_index) == Some(&b'.')
                && at_component_start(path, path_index, options)
                && match &steps[pattern_index] {
                    ExtglobStep::Group(group) => {
                        !extglob_group_allows_literal_leading_period(&program.groups[*group])
                    }
                    ExtglobStep::UnclosedGroup { .. } => true,
                    _ => false,
                };
            if leading_period_blocked {
                if TRACK_RETRIES {
                    hit_hidden_barrier = true;
                }
            } else {
                match &steps[pattern_index] {
                    ExtglobStep::Group(group) => {
                        let group = &program.groups[*group];
                        let should_expand = match group.entry_memo {
                            GroupEntryMemo::None => true,
                            GroupEntryMemo::Monotonic => {
                                if monotonic_range.is_some_and(|(_, hi)| path_index == hi) {
                                    false
                                } else {
                                    monotonic_range = Some((
                                        monotonic_range.map_or(path_index, |(lo, _)| lo),
                                        path_index,
                                    ));
                                    if TRACK_RETRIES {
                                        monotonic_group_index = pattern_index;
                                    }
                                    true
                                }
                            }
                            GroupEntryMemo::MonotonicOrCrossingStar if !rewound_crossing_star => {
                                if monotonic_range.is_some_and(|(_, hi)| path_index == hi) {
                                    false
                                } else {
                                    monotonic_range = Some((
                                        monotonic_range.map_or(path_index, |(lo, _)| lo),
                                        path_index,
                                    ));
                                    if TRACK_RETRIES {
                                        monotonic_group_index = pattern_index;
                                    }
                                    true
                                }
                            }
                            GroupEntryMemo::MonotonicOrCrossingStar => {
                                !monotonic_range
                                    .is_some_and(|(lo, hi)| (lo..=hi).contains(&path_index))
                                    && ((pattern_index == start && path_index == start_path_index)
                                        || state.failed.insert(
                                            program.memo_state_index(pattern_index),
                                            path_index,
                                        ))
                            }
                            GroupEntryMemo::Paged => {
                                (pattern_index == start && path_index == start_path_index)
                                    || state
                                        .failed
                                        .insert(program.memo_state_index(pattern_index), path_index)
                            }
                        };
                        if should_expand {
                            queue_extglob_group(program, group, path, path_index, options, state);
                        }
                        // The group's continuations are queued; this branch
                        // then backtracks through the preceding star like any
                        // failed step, below.
                    }
                    ExtglobStep::Star {
                        next,
                        blocks_leading_period,
                        ..
                    } => {
                        if TRACK_RETRIES {
                            max_star_step = max_star_step.max(pattern_index);
                        }
                        let hidden_blocked = *blocks_leading_period
                            && star_stops_before_hidden_component(path, path_index, options);
                        let dedup_blocked = if !hidden_blocked && TRACK_RETRIES && dedup_stars {
                            let memo_index = program.memo_state_index(pattern_index);
                            let blocked = !(pattern_index == start
                                && path_index == start_path_index)
                                && !state.failed.insert(memo_index, path_index);
                            #[cfg(test)]
                            if !blocked {
                                EXTGLOB_MEMOIZED_STAR_VISITS
                                    .with(|count| count.set(count.get() + 1));
                            }
                            blocked
                        } else {
                            false
                        };
                        if hidden_blocked {
                            if TRACK_RETRIES {
                                hit_hidden_barrier = true;
                            }
                        } else if !dedup_blocked {
                            if !*blocks_leading_period
                                && steps.get(*next).is_some_and(|step| {
                                    matches!(step, ExtglobStep::Separator { .. })
                                })
                            {
                                queue_extglob_continuation(program, state, *next + 1, path_index);
                            }
                            let component_local =
                                extglob_star_component_local(steps, pattern_index, options);
                            if TRACK_RETRIES {
                                if let Some((retry_start, _)) = first_retryable_star {
                                    if pattern_index != retry_start {
                                        earlier_retryable_star_overwritten = true;
                                    }
                                } else if !component_local || !options.root_component_wildcards {
                                    first_retryable_star = Some((pattern_index, path_index));
                                }
                            }
                            remember_crossing_star(
                                &mut crossing_star,
                                has_star.then_some((
                                    CrossingStar {
                                        resume: star_pattern_index,
                                        consumed: star_path_index,
                                        memo_index: star_memo_index,
                                        blocks_leading_period: star_blocks_leading_period,
                                    },
                                    star_component_local,
                                )),
                                component_local,
                            );
                            star_pattern_index = *next;
                            star_path_index = path_index;
                            if TRACK_RETRIES && dedup_stars {
                                star_memo_index = program.memo_state_index(pattern_index);
                            }
                            star_component_local = component_local;
                            star_blocks_leading_period = *blocks_leading_period;
                            has_star = true;
                            pattern_index = *next;
                            continue;
                        }
                    }
                    ExtglobStep::UnclosedGroup { byte: b'*' } => {
                        if TRACK_RETRIES {
                            max_star_step = max_star_step.max(pattern_index);
                        }
                        let dedup_blocked = if TRACK_RETRIES && dedup_stars {
                            let memo_index = program.memo_state_index(pattern_index);
                            let blocked = !(pattern_index == start
                                && path_index == start_path_index)
                                && !state.failed.insert(memo_index, path_index);
                            #[cfg(test)]
                            if !blocked {
                                EXTGLOB_MEMOIZED_STAR_VISITS
                                    .with(|count| count.set(count.get() + 1));
                            }
                            blocked
                        } else {
                            false
                        };
                        if !dedup_blocked {
                            let component_local =
                                extglob_wildcard_component_local(steps, pattern_index, options);
                            if TRACK_RETRIES {
                                if let Some((retry_start, _)) = first_retryable_star {
                                    if pattern_index != retry_start {
                                        earlier_retryable_star_overwritten = true;
                                    }
                                } else if !component_local || !options.root_component_wildcards {
                                    first_retryable_star = Some((pattern_index, path_index));
                                }
                            }
                            remember_crossing_star(
                                &mut crossing_star,
                                has_star.then_some((
                                    CrossingStar {
                                        resume: star_pattern_index,
                                        consumed: star_path_index,
                                        memo_index: star_memo_index,
                                        blocks_leading_period: star_blocks_leading_period,
                                    },
                                    star_component_local,
                                )),
                                component_local,
                            );
                            star_pattern_index = pattern_index + 1;
                            star_path_index = path_index;
                            if TRACK_RETRIES && dedup_stars {
                                star_memo_index = program.memo_state_index(pattern_index);
                            }
                            star_component_local = component_local;
                            star_blocks_leading_period = true;
                            has_star = true;
                            pattern_index += 1;
                            continue;
                        }
                    }
                    ExtglobStep::Any | ExtglobStep::UnclosedGroup { byte: b'?' } => {
                        if path.get(path_index).is_some_and(|&byte| {
                            (!extglob_wildcard_component_local(steps, pattern_index, options)
                                || !is_separator(byte))
                                && (options.match_hidden
                                    || byte != b'.'
                                    || !at_component_start(path, path_index, options))
                        }) {
                            pattern_index += 1;
                            path_index += 1;
                            continue;
                        }
                        // The wildcard arm falling through leaves `?` readable as
                        // an ordinary byte, which only a literal `?` can match.
                        if path.get(path_index).is_some_and(|&actual| {
                            bytes_equal(b'?', actual, options.case_insensitive)
                        }) {
                            pattern_index += 1;
                            path_index += 1;
                            continue;
                        }
                    }
                    ExtglobStep::Class { class, next } => {
                        if path.get(path_index).is_some_and(|&byte| {
                            (!extglob_wildcard_component_local(steps, pattern_index, options)
                                || !is_separator(byte))
                                && (options.match_hidden
                                    || byte != b'.'
                                    || !at_component_start(path, path_index, options))
                                && class.matches(byte, options.case_insensitive)
                        }) {
                            pattern_index = *next;
                            path_index += 1;
                            continue;
                        }
                    }
                    ExtglobStep::Escape { escaped } => {
                        if path.get(path_index).is_some_and(|&byte| {
                            bytes_equal(*escaped, byte, options.case_insensitive)
                        }) {
                            pattern_index += 2;
                            path_index += 1;
                            continue;
                        }
                    }
                    ExtglobStep::Separator { .. } => {
                        if path
                            .get(path_index)
                            .is_some_and(|&actual| is_separator(actual))
                        {
                            pattern_index += 1;
                            path_index += 1;
                            continue;
                        }
                        // A candidate may end in front of the separator of a
                        // trailing `/**`, as `x/**` accepts `x` (#422).
                        if path_index == path.len()
                            && program.trailing_globstar_separator == Some(pattern_index)
                        {
                            return true;
                        }
                    }
                    ExtglobStep::Byte(expected) | ExtglobStep::UnclosedGroup { byte: expected } => {
                        if path.get(path_index).is_some_and(|&actual| {
                            bytes_equal(*expected, actual, options.case_insensitive)
                        }) {
                            pattern_index += 1;
                            path_index += 1;
                            continue;
                        }
                    }
                    ExtglobStep::NoMatch => {}
                }
            }
        }

        if has_star && star_path_index < path.len() {
            let mut can_consume =
                extglob_star_can_consume(path, star_path_index, options, star_component_local);
            let mut dedup_stopped = false;
            if can_consume && TRACK_RETRIES && dedup_stars {
                if !state.failed.insert(star_memo_index, star_path_index + 1) {
                    can_consume = false;
                    dedup_stopped = true;
                } else {
                    #[cfg(test)]
                    EXTGLOB_MEMOIZED_STAR_VISITS.with(|count| count.set(count.get() + 1));
                }
            }
            if !can_consume {
                if TRACK_RETRIES && !dedup_stopped {
                    if star_stops_before_hidden_component(path, star_path_index, options) {
                        hit_hidden_barrier = true;
                    }
                    if star_component_local && is_separator(path[star_path_index]) {
                        hit_separator_barrier = true;
                    }
                }
                if let Some(previous) = crossing_star.take() {
                    star_pattern_index = previous.resume;
                    star_path_index = previous.consumed;
                    star_memo_index = previous.memo_index;
                    star_component_local = false;
                    star_blocks_leading_period = previous.blocks_leading_period;
                    rewound_crossing_star = true;
                    dedup_stopped = false;
                    can_consume = star_path_index < path.len()
                        && extglob_star_can_consume(path, star_path_index, options, false);
                    if can_consume && TRACK_RETRIES && dedup_stars {
                        if !state.failed.insert(star_memo_index, star_path_index + 1) {
                            can_consume = false;
                            dedup_stopped = true;
                        } else {
                            #[cfg(test)]
                            EXTGLOB_MEMOIZED_STAR_VISITS.with(|count| count.set(count.get() + 1));
                        }
                    }
                }
            }
            if !can_consume {
                if dedup_stopped {
                    break;
                }
                if star_stops_before_hidden_component(path, star_path_index, options) {
                    if TRACK_RETRIES {
                        hit_hidden_barrier = true;
                    }
                    if star_blocks_leading_period {
                        break;
                    }
                }
                if TRACK_RETRIES && star_component_local && is_separator(path[star_path_index]) {
                    hit_separator_barrier = true;
                }
                pattern_index = star_pattern_index;
                path_index = star_path_index;
                has_star = false;
                continue;
            }
            let consumed = path[star_path_index];
            pattern_index = star_pattern_index;
            star_path_index += 1;
            if !star_component_local
                && star_blocks_leading_period
                && !options.match_hidden
                && is_separator(consumed)
                && path.get(star_path_index) == Some(&b'.')
            {
                if TRACK_RETRIES {
                    hit_hidden_barrier = true;
                }
                break;
            }
            path_index = star_path_index;
            continue;
        }
        break;
    }

    if path_index == path.len() && pattern_index == steps.len() {
        return true;
    }
    if TRACK_RETRIES
        && !dedup_stars
        && earlier_retryable_star_overwritten
        && (hit_hidden_barrier || hit_separator_barrier)
        && let Some((star_start, star_start_path)) = first_retryable_star
        && {
            let retry = extglob_retryable_star_segment(steps, star_start, max_star_step, options);
            (hit_hidden_barrier && retry.explicit_dot)
                || (hit_separator_barrier && retry.crossing_wildcard_before_local_star)
        }
    {
        state.deferred_retries.push(DeferredMemoRetry {
            star_start,
            star_start_path,
            already_marked: star_start == start && star_start_path == start_path_index,
            monotonic_group: monotonic_range.map(|range| (monotonic_group_index, range)),
        });
    }
    false
}

/// Runs one memoized outer-star segment task after the latest-star fast path
/// hit a hidden-component or separator barrier behind an overwritten star.
#[allow(clippy::too_many_arguments)]
fn match_extglob_task_memoized(
    program: &CompiledExtglob,
    path: &[u8],
    start: usize,
    start_path_index: usize,
    monotonic_group: Option<(usize, (usize, usize))>,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) -> bool {
    let steps = &program.steps;
    let mut pattern_index = start;
    let mut path_index = start_path_index;

    while path_index < path.len() || pattern_index < steps.len() {
        if pattern_index >= steps.len() {
            return false;
        }
        let leading_period_blocked = !options.match_hidden
            && path.get(path_index) == Some(&b'.')
            && at_component_start(path, path_index, options)
            && match &steps[pattern_index] {
                ExtglobStep::Group(group) => {
                    !extglob_group_allows_literal_leading_period(&program.groups[*group])
                }
                ExtglobStep::UnclosedGroup { .. } => true,
                _ => false,
            };
        if leading_period_blocked {
            return false;
        }
        match &steps[pattern_index] {
            ExtglobStep::Group(group) => {
                let group = &program.groups[*group];
                let already_expanded = monotonic_group.is_some_and(|(g, (lo, hi))| {
                    g == pattern_index && (lo..=hi).contains(&path_index)
                });
                let should_expand = !already_expanded
                    && ((pattern_index == start && path_index == start_path_index)
                        || state
                            .failed
                            .insert(program.memo_state_index(pattern_index), path_index));
                if should_expand {
                    queue_extglob_group(program, group, path, path_index, options, state);
                }
                return false;
            }
            ExtglobStep::Star {
                next,
                blocks_leading_period,
                memoize_tail,
            } => {
                let memo_index = program.memo_state_index(pattern_index);
                if !(pattern_index == start && path_index == start_path_index)
                    && !state.failed.insert(memo_index, path_index)
                {
                    return false;
                }
                #[cfg(test)]
                EXTGLOB_MEMOIZED_STAR_VISITS.with(|count| count.set(count.get() + 1));
                if !*memoize_tail {
                    debug_assert!(
                        pattern_index != start || path_index != start_path_index,
                        "handoff star must not be the retry entry star"
                    );
                    state.handoff_pending.push((pattern_index, path_index));
                    #[cfg(test)]
                    {
                        *state.pending_peak = (*state.pending_peak).max(
                            state.pending.len()
                                + state.handoff_pending.len()
                                + state.memo_pending.len(),
                        );
                    }
                    return false;
                }
                if *blocks_leading_period
                    && star_stops_before_hidden_component(path, path_index, options)
                {
                    return false;
                }
                if !*blocks_leading_period
                    && (path_index == 0 || is_separator(path[path_index - 1]))
                    && steps
                        .get(*next)
                        .is_some_and(|step| matches!(step, ExtglobStep::Separator { .. }))
                {
                    queue_extglob_continuation(program, state, *next + 1, path_index);
                }
                let component_local = extglob_star_component_local(steps, pattern_index, options);
                if path_index < path.len()
                    && extglob_star_can_consume(path, path_index, options, component_local)
                {
                    let next_path = path_index + 1;
                    if !(*blocks_leading_period
                        && star_stops_before_hidden_component(path, next_path, options))
                    {
                        queue_extglob_memo_continuation(program, state, pattern_index, next_path);
                    }
                }
                pattern_index = *next;
                continue;
            }
            ExtglobStep::UnclosedGroup { byte: b'*' } => {
                let memo_index = program.memo_state_index(pattern_index);
                if !(pattern_index == start && path_index == start_path_index)
                    && !state.failed.insert(memo_index, path_index)
                {
                    return false;
                }
                #[cfg(test)]
                EXTGLOB_MEMOIZED_STAR_VISITS.with(|count| count.set(count.get() + 1));
                let component_local =
                    extglob_wildcard_component_local(steps, pattern_index, options);
                if path_index < path.len()
                    && extglob_star_can_consume(path, path_index, options, component_local)
                {
                    let next_path = path_index + 1;
                    if !star_stops_before_hidden_component(path, next_path, options) {
                        queue_extglob_memo_continuation(program, state, pattern_index, next_path);
                    }
                }
                pattern_index += 1;
                continue;
            }
            ExtglobStep::Any | ExtglobStep::UnclosedGroup { byte: b'?' } => {
                if path.get(path_index).is_some_and(|&byte| {
                    (!extglob_wildcard_component_local(steps, pattern_index, options)
                        || !is_separator(byte))
                        && (options.match_hidden
                            || byte != b'.'
                            || !at_component_start(path, path_index, options))
                }) || path
                    .get(path_index)
                    .is_some_and(|&actual| bytes_equal(b'?', actual, options.case_insensitive))
                {
                    pattern_index += 1;
                    path_index += 1;
                    continue;
                }
                return false;
            }
            ExtglobStep::Class { class, next } => {
                if path.get(path_index).is_some_and(|&byte| {
                    (!extglob_wildcard_component_local(steps, pattern_index, options)
                        || !is_separator(byte))
                        && (options.match_hidden
                            || byte != b'.'
                            || !at_component_start(path, path_index, options))
                        && class.matches(byte, options.case_insensitive)
                }) {
                    pattern_index = *next;
                    path_index += 1;
                    continue;
                }
                return false;
            }
            ExtglobStep::Escape { escaped } => {
                if path
                    .get(path_index)
                    .is_some_and(|&byte| bytes_equal(*escaped, byte, options.case_insensitive))
                {
                    pattern_index += 2;
                    path_index += 1;
                    continue;
                }
                return false;
            }
            ExtglobStep::Separator { .. } => {
                if path
                    .get(path_index)
                    .is_some_and(|&actual| is_separator(actual))
                {
                    pattern_index += 1;
                    path_index += 1;
                    continue;
                }
                return path_index == path.len()
                    && program.trailing_globstar_separator == Some(pattern_index);
            }
            ExtglobStep::Byte(expected) | ExtglobStep::UnclosedGroup { byte: expected } => {
                if path
                    .get(path_index)
                    .is_some_and(|&actual| bytes_equal(*expected, actual, options.case_insensitive))
                {
                    pattern_index += 1;
                    path_index += 1;
                    continue;
                }
                return false;
            }
            ExtglobStep::NoMatch => return false,
        }
    }
    true
}

fn queue_extglob_memo_continuation(
    program: &CompiledExtglob,
    state: &mut ExtglobMatchState<'_>,
    start: usize,
    path_index: usize,
) {
    if state
        .failed
        .insert(program.memo_state_index(start), path_index)
    {
        state.memo_pending.push((start, path_index));
        #[cfg(test)]
        {
            *state.pending_peak =
                (*state.pending_peak).max(state.pending.len() + state.memo_pending.len());
        }
    }
}

/// Updates the crossing-star backtrack point when a new star replaces
/// `previous`.
///
/// A new crossing star supersedes every earlier star. A new component-local
/// star keeps the latest crossing star as the fallback for the separator it
/// cannot consume.
fn remember_crossing_star(
    crossing_star: &mut Option<CrossingStar>,
    previous: Option<(CrossingStar, bool)>,
    component_local: bool,
) {
    if !component_local {
        *crossing_star = None;
    } else if let Some((previous, false)) = previous {
        *crossing_star = Some(previous);
    }
}

fn extglob_wildcard_component_local(
    steps: &[ExtglobStep],
    step_index: usize,
    options: PatternOptions,
) -> bool {
    options.component_wildcards
        && (options.root_component_wildcards || extglob_step_component_local(steps, step_index))
}

/// Whether the outer star run at `step_index` stays inside one component,
/// answered exactly as the token engine reads the same run.
///
/// A single star follows the position rule. A longer run that is a whole path
/// component starts with a recursive star under `recursive_double_star`,
/// which crosses under every policy (#419, ADR-0020). Any other longer run is
/// a sequence of ordinary stars: the second one stands behind the first
/// rather than behind a separator, so the run crosses unless every ordinary
/// wildcard is component-local.
fn extglob_star_component_local(
    steps: &[ExtglobStep],
    step_index: usize,
    options: PatternOptions,
) -> bool {
    match steps.get(step_index) {
        Some(ExtglobStep::Star { next, .. }) if *next - step_index >= 2 => {
            !(options.recursive_double_star
                && extglob_star_run_is_whole_component(steps, step_index, *next))
                && options.component_wildcards
                && options.root_component_wildcards
        }
        _ => extglob_wildcard_component_local(steps, step_index, options),
    }
}

/// Whether the outer star run `start..end` is a whole path component, the
/// step-table reading of [`star_run_is_whole_component`]: it follows the
/// program start or a separator and ends at the program end or before one. An
/// escaped separator compiles to an escape, never to a separator byte, so it
/// bounds nothing, exactly as in the token engine.
fn extglob_star_run_is_whole_component(steps: &[ExtglobStep], start: usize, end: usize) -> bool {
    let is_separator_step =
        |index: usize| matches!(steps.get(index), Some(ExtglobStep::Separator { .. }));
    (start == 0 || is_separator_step(start - 1)) && (end == steps.len() || is_separator_step(end))
}

fn extglob_star_can_consume(
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    component_local: bool,
) -> bool {
    let byte = path[path_index];
    (!component_local || !is_separator(byte))
        && (options.match_hidden || byte != b'.' || !at_component_start(path, path_index, options))
}

/// Whether a group can explicitly consume the leading period of a component.
///
/// Wildcard-led alternatives still observe `match_hidden`; only a literal dot
/// is an opt-in to matching a hidden path.
fn extglob_group_allows_literal_leading_period(group: &ExtglobGroup) -> bool {
    // Negation consumes whatever its alternatives reject, not a literal from
    // one of them, so it remains wildcard-like at a component boundary.
    if group.kind == ExtglobKind::Negated {
        return false;
    }
    group.alternatives.iter().any(|alternative| {
        alternative.compiled.iter().any(|alternative| {
            matches!(
                alternative.tokens.first(),
                Some(Token::Literal(literal)) if literal.first() == Some(&b'.')
            ) || alternative_starts_with_recursive_prefix(&alternative.tokens)
        })
    }) || group.through_separator.iter().any(|alternative| {
        // A trailing `**` compiled together with the outer separator can
        // leave a leading period to what follows, as `**/.h` does (#422).
        alternative
            .compiled
            .iter()
            .any(|alternative| alternative_starts_with_recursive_prefix(&alternative.tokens))
    })
}

/// A leading even run of recursive stars may hand over to its final `**/`
/// without consuming a hidden component. Odd runs end in an ordinary star
/// and still block the leading period (#442, #445).
fn alternative_starts_with_recursive_prefix(tokens: &[Token]) -> bool {
    matches!(
        tokens
            .iter()
            .find(|token| !matches!(token, Token::RecursiveStar)),
        Some(Token::RecursivePrefix)
    )
}

fn queue_extglob_group(
    program: &CompiledExtglob,
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) {
    #[cfg(test)]
    {
        *state.group_expansions += 1;
    }
    match group.kind {
        ExtglobKind::ExactlyOne => {
            matching_extglob_group_ends(
                group,
                path,
                path_index,
                options,
                state.prefix_sweep_state,
                state.ends,
            );
            queue_extglob_continuations(program, state, group.rest);
            queue_extglob_through_separator(program, group, path, path_index, options, state);
        }
        ExtglobKind::Optional => {
            queue_extglob_continuation(program, state, group.rest, path_index);
            matching_extglob_group_ends(
                group,
                path,
                path_index,
                options,
                state.prefix_sweep_state,
                state.ends,
            );
            queue_extglob_continuations(program, state, group.rest);
            queue_extglob_through_separator(program, group, path, path_index, options, state);
        }
        ExtglobKind::ZeroOrMore => {
            queue_extglob_continuation(program, state, group.rest, path_index);
            if group.idempotent_through_separator {
                queue_extglob_through_separator(program, group, path, path_index, options, state);
            } else {
                matching_extglob_repetition_ends(group, path, path_index, options, state);
                queue_extglob_continuations(program, state, group.rest);
                if !group.through_separator.is_empty() {
                    queue_extglob_through_separator_ends(program, state, group.rest + 1);
                }
            }
        }
        ExtglobKind::OneOrMore => {
            if group.idempotent_through_separator {
                queue_extglob_through_separator(program, group, path, path_index, options, state);
            } else {
                matching_extglob_repetition_ends(group, path, path_index, options, state);
                queue_extglob_continuations(program, state, group.rest);
                if !group.through_separator.is_empty() {
                    queue_extglob_through_separator_ends(program, state, group.rest + 1);
                }
            }
        }
        ExtglobKind::Negated => {
            let stop_limit = negated_extglob_stop_limit(
                path,
                path_index,
                extglob_component_end(path, path_index, options, group.component_local),
                options,
            );
            state.excluded.resize(stop_limit - path_index, false);
            state.excluded.fill(false);
            matching_extglob_group_ends(
                group,
                path,
                path_index,
                options,
                state.prefix_sweep_state,
                state.ends,
            );
            // An alternative may match past the last permitted stop; the
            // negation cannot stop there either way, so nothing is excluded.
            for &end in state.ends.iter() {
                if let Some(excluded) = state.excluded.get_mut(end - path_index) {
                    *excluded = true;
                }
            }
            for offset in 0..state.excluded.len() {
                if !state.excluded[offset] {
                    queue_extglob_continuation(program, state, group.rest, path_index + offset);
                }
            }
        }
    }
}

fn queue_extglob_continuations(
    program: &CompiledExtglob,
    state: &mut ExtglobMatchState<'_>,
    rest: usize,
) {
    for index in 0..state.ends.len() {
        let end = state.ends[index];
        queue_extglob_continuation(program, state, rest, end);
    }
}

/// Queues the continuations of the alternatives in
/// [`ExtglobGroup::through_separator`] for single-iteration groups (`@()`,
/// `?()`), resuming behind the separator after the group (#422).
///
/// Repeated groups (`*()`, `+()`) compute both regular and `through_separator`
/// ends in one shared pass inside [`matching_extglob_repetition_ends`] and
/// queue the latter through [`queue_extglob_through_separator_ends`] so a
/// repetition with many reachable offsets never restarts the `through_separator`
/// sweep quadratically (#495).
fn queue_extglob_through_separator(
    program: &CompiledExtglob,
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) {
    if group.through_separator.is_empty() {
        return;
    }
    let span = AlternativeSpan {
        path,
        start: path_index,
        end_limit: group.alternative_end_limit(path, path_index, options),
        root_component_local: group.component_local,
    };
    state.candidate_ends.clear();
    for alternative in &group.through_separator {
        matching_extglob_alternative_ends(
            alternative,
            span,
            options,
            state.prefix_sweep_state,
            state.candidate_ends,
        );
    }
    queue_extglob_through_separator_ends(program, state, group.rest + 1);
}

fn queue_extglob_through_separator_ends(
    program: &CompiledExtglob,
    state: &mut ExtglobMatchState<'_>,
    resume: usize,
) {
    for index in 0..state.candidate_ends.len() {
        let end = state.candidate_ends[index];
        queue_extglob_continuation(program, state, resume, end);
    }
}

/// Queues a continuation once. Marking at enqueue time bounds the live
/// worklist by the memo-state × candidate space instead of all duplicate
/// paths that happen to discover that state before it is popped.
fn queue_extglob_continuation(
    program: &CompiledExtglob,
    state: &mut ExtglobMatchState<'_>,
    start: usize,
    path_index: usize,
) {
    if state
        .failed
        .insert(program.memo_state_index(start), path_index)
    {
        state.pending.push((start, path_index));
        #[cfg(test)]
        {
            *state.pending_peak = (*state.pending_peak).max(state.pending.len());
        }
    }
}

fn matching_extglob_group_ends(
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    prefix_sweep_state: &mut Option<SweepState>,
    output: &mut Vec<usize>,
) {
    output.clear();
    let span = AlternativeSpan {
        path,
        start: path_index,
        end_limit: group.alternative_end_limit(path, path_index, options),
        root_component_local: group.component_local,
    };
    for alternative in group.matched_alternatives() {
        matching_extglob_alternative_ends(alternative, span, options, prefix_sweep_state, output);
    }
    output.sort_unstable();
    output.dedup();
}

/// Whether entering `alternative` inside a repetition requires the current
/// offset to start a path component.
///
/// All offsets reachable after one or more repetitions of `group`.
///
/// Variable-width alternatives run as streaming sweeps — including any
/// [`ExtglobGroup::through_separator`] alternatives, whose end offsets are
/// returned in `state.candidate_ends` so callers can queue them behind the
/// outer separator without restarting a separate sweep per reached offset
/// (#495). Whenever a prior match reaches the current offset, their initial
/// boundary is injected into the live state. This evaluates every possible
/// partition in one candidate pass instead of rematching every suffix from
/// every reached offset.
fn matching_extglob_repetition_ends(
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) {
    let component_end = group.alternative_end_limit(path, path_index, options);
    let position_count = component_end - path_index + 1;
    // Only the first iteration stands where the group does. A later one
    // follows the previous iteration's text, not the separator, so its first
    // wildcard is not component-local: `src/+(*.ts)` reads like
    // `src/*.ts*.ts`. The difference is only observable under the list-filter
    // policy; there the first iteration is matched on its own, because the
    // streaming sweeps share one policy across every live iteration.
    let first_iteration_apart = group.first_iteration_apart(options);
    let later_root_local = group.later_iteration_root_local(options);
    state.visited.clear();
    let reachable_base = push_visited(state.visited, position_count);
    let matched_base = push_visited(state.visited, position_count);
    let through_sep_base =
        (!group.through_separator.is_empty()).then(|| push_visited(state.visited, position_count));
    #[cfg(test)]
    let mut sweep_steps = 0_usize;
    visit(state.visited, reachable_base, 0);
    let mut max_reachable = 0_usize;
    let (narrow_count, wide_count) =
        prepare_extglob_sweeps(group, state.narrow_sweeps, state.wide_sweeps);
    state.max_narrow_sweeps = state.max_narrow_sweeps.max(narrow_count);
    state.max_wide_sweeps = state.max_wide_sweeps.max(wide_count);
    let mut starts_component = at_component_start(path, path_index, options);
    let mut later_options = PatternOptions {
        braces: false,
        extglob: false,
        candidate_starts_component: starts_component,
        candidate_root_component_wildcard: later_root_local,
        ..options
    };
    for offset in 0..position_count {
        let absolute = path_index + offset;
        let root_component_local = if offset == 0 {
            group.component_local
        } else {
            later_root_local
        };
        if visited(state.visited, reachable_base, offset) {
            let before_hidden =
                !options.match_hidden && starts_component && path.get(absolute) == Some(&b'.');
            let mut narrow_index = 0;
            let mut wide_index = 0;
            for (alternative, is_through_separator) in group.repetition_alternatives() {
                if let Some(width) = alternative.width {
                    let Some(end) = absolute.checked_add(width) else {
                        continue;
                    };
                    if end <= component_end
                        && match_extglob_alternative_exact(
                            alternative,
                            &path[absolute..end],
                            starts_component,
                            options,
                            root_component_local,
                        )
                    {
                        let reached = end - path_index;
                        visit(state.visited, reachable_base, reached);
                        max_reachable = max_reachable.max(reached);
                        visit(state.visited, matched_base, reached);
                    }
                    continue;
                }
                let blocked_mid_component =
                    !starts_component && alternative.requires_component_start;
                let enumerate_apart = offset == 0 && first_iteration_apart;
                let mut has_fallback = enumerate_apart && !blocked_mid_component;
                for compiled in &alternative.compiled {
                    if let Some(sweep) = &compiled.sweep {
                        let accepted = if sweep.is_wide() {
                            let sweep_state = &mut state.wide_sweeps[wide_index];
                            wide_index += 1;
                            if enumerate_apart || blocked_mid_component {
                                continue;
                            }
                            sweep.inject_start(sweep_state, starts_component);
                            sweep.accepts_at(sweep_state, before_hidden)
                        } else {
                            let sweep_state = &mut state.narrow_sweeps[narrow_index];
                            narrow_index += 1;
                            if enumerate_apart || blocked_mid_component {
                                continue;
                            }
                            sweep.inject_start_narrow(sweep_state, starts_component);
                            sweep.accepts_at_narrow(*sweep_state, before_hidden)
                        };
                        if accepted {
                            if let Some(through_sep_base) =
                                through_sep_base.filter(|_| is_through_separator)
                            {
                                visit(state.visited, through_sep_base, offset);
                            } else {
                                visit(state.visited, matched_base, offset);
                            }
                        }
                    } else if !blocked_mid_component {
                        has_fallback = true;
                    }
                }
                if has_fallback {
                    state.candidate_ends.clear();
                    matching_extglob_alternative_ends(
                        alternative,
                        AlternativeSpan {
                            path,
                            start: absolute,
                            end_limit: component_end,
                            root_component_local,
                        },
                        options,
                        state.prefix_sweep_state,
                        state.candidate_ends,
                    );
                    #[cfg(test)]
                    {
                        sweep_steps += component_end - absolute;
                    }
                    for &end in state.candidate_ends.iter() {
                        let reached = end - path_index;
                        visit(state.visited, reachable_base, reached);
                        max_reachable = max_reachable.max(reached);
                        if let Some(through_sep_base) =
                            through_sep_base.filter(|_| is_through_separator)
                        {
                            visit(state.visited, through_sep_base, reached);
                        } else {
                            visit(state.visited, matched_base, reached);
                        }
                    }
                }
            }
            debug_assert_eq!(
                (narrow_index, wide_index),
                (narrow_count, wide_count),
                "prepared repetition sweeps must align with injected variable-width alternatives"
            );
            // If the component-local first iteration matched zero bytes (e.g.
            // an empty arm, `*`, or `**/` at offset 0), a later non-local
            // iteration can also start right at offset 0.
            let matched_zero_at_start = visited(state.visited, matched_base, 0)
                || through_sep_base.is_some_and(|base| visited(state.visited, base, 0));
            if offset == 0 && first_iteration_apart && matched_zero_at_start {
                let mut narrow_index = 0;
                let mut wide_index = 0;
                for (alternative, is_through_separator) in group.repetition_alternatives() {
                    if let Some(width) = alternative.width {
                        if width == 0 {
                            continue;
                        }
                        let Some(end) = absolute.checked_add(width) else {
                            continue;
                        };
                        if end <= component_end
                            && match_extglob_alternative_exact(
                                alternative,
                                &path[absolute..end],
                                starts_component,
                                options,
                                later_root_local,
                            )
                        {
                            let reached = end - path_index;
                            visit(state.visited, reachable_base, reached);
                            max_reachable = max_reachable.max(reached);
                            visit(state.visited, matched_base, reached);
                        }
                        continue;
                    }
                    let blocked_mid_component =
                        !starts_component && alternative.requires_component_start;
                    let mut has_fallback = false;
                    for compiled in &alternative.compiled {
                        if let Some(sweep) = &compiled.sweep {
                            let accepted = if sweep.is_wide() {
                                let sweep_state = &mut state.wide_sweeps[wide_index];
                                wide_index += 1;
                                if blocked_mid_component {
                                    continue;
                                }
                                sweep.inject_start(sweep_state, starts_component);
                                sweep.accepts_at(sweep_state, before_hidden)
                            } else {
                                let sweep_state = &mut state.narrow_sweeps[narrow_index];
                                narrow_index += 1;
                                if blocked_mid_component {
                                    continue;
                                }
                                sweep.inject_start_narrow(sweep_state, starts_component);
                                sweep.accepts_at_narrow(*sweep_state, before_hidden)
                            };
                            if accepted {
                                if let Some(through_sep_base) =
                                    through_sep_base.filter(|_| is_through_separator)
                                {
                                    visit(state.visited, through_sep_base, 0);
                                } else {
                                    visit(state.visited, matched_base, 0);
                                }
                            }
                        } else if !blocked_mid_component {
                            has_fallback = true;
                        }
                    }
                    if has_fallback {
                        state.candidate_ends.clear();
                        matching_extglob_alternative_ends(
                            alternative,
                            AlternativeSpan {
                                path,
                                start: absolute,
                                end_limit: component_end,
                                root_component_local: later_root_local,
                            },
                            options,
                            state.prefix_sweep_state,
                            state.candidate_ends,
                        );
                        #[cfg(test)]
                        {
                            sweep_steps += component_end - absolute;
                        }
                        for &end in state.candidate_ends.iter() {
                            let reached = end - path_index;
                            visit(state.visited, reachable_base, reached);
                            max_reachable = max_reachable.max(reached);
                            if let Some(through_sep_base) =
                                through_sep_base.filter(|_| is_through_separator)
                            {
                                visit(state.visited, through_sep_base, reached);
                            } else {
                                visit(state.visited, matched_base, reached);
                            }
                        }
                    }
                }
                debug_assert_eq!(
                    (narrow_index, wide_index),
                    (narrow_count, wide_count),
                    "prepared repetition sweeps must align with injected variable-width alternatives"
                );
            }
        }

        if absolute == component_end {
            break;
        }
        let byte = path[absolute];
        let next_starts_component = is_separator(byte);
        let before_hidden_next =
            !options.match_hidden && next_starts_component && path.get(absolute + 1) == Some(&b'.');
        later_options.candidate_starts_component = starts_component;
        let mut narrow_index = 0;
        let mut wide_index = 0;
        let mut any_sweep_active = false;
        for (alternative, is_through_separator) in group.repetition_alternatives() {
            for compiled in &alternative.compiled {
                let Some(sweep) = &compiled.sweep else {
                    continue;
                };
                let accepted = if sweep.is_wide() {
                    let sweep_state = &mut state.wide_sweeps[wide_index];
                    wide_index += 1;
                    if sweep_state.is_empty() {
                        continue;
                    }
                    #[cfg(test)]
                    {
                        sweep_steps += 1;
                    }
                    if !sweep.advance(sweep_state, byte, starts_component, later_options) {
                        continue;
                    }
                    any_sweep_active = true;
                    sweep.accepts_at(sweep_state, before_hidden_next)
                } else {
                    let sweep_state = &mut state.narrow_sweeps[narrow_index];
                    narrow_index += 1;
                    if *sweep_state == 0 {
                        continue;
                    }
                    #[cfg(test)]
                    {
                        sweep_steps += 1;
                    }
                    if !sweep.advance_narrow(sweep_state, byte, starts_component, later_options) {
                        continue;
                    }
                    any_sweep_active = true;
                    sweep.accepts_at_narrow(*sweep_state, before_hidden_next)
                };
                if accepted {
                    let reached = offset + 1;
                    visit(state.visited, reachable_base, reached);
                    max_reachable = max_reachable.max(reached);
                    // `through_separator` arms already absorbed the outer `/`
                    // (and their trailing `RecursivePrefix` consumes any
                    // further separator directly), so they only queue behind
                    // the separator (`through_sep_base`), avoiding a dead
                    // `group.rest` memo row (#495).
                    if let Some(through_sep_base) =
                        through_sep_base.filter(|_| is_through_separator)
                    {
                        visit(state.visited, through_sep_base, reached);
                    } else {
                        visit(state.visited, matched_base, reached);
                    }
                }
            }
        }
        debug_assert_eq!(
            (narrow_index, wide_index),
            (narrow_count, wide_count),
            "prepared repetition sweep count must match the advance loop"
        );
        if !any_sweep_active && max_reachable <= offset {
            break;
        }
        starts_component = next_starts_component;
    }
    #[cfg(test)]
    EXTGLOB_REPETITION_SWEEP_STEPS.with(|count| count.set(count.get() + sweep_steps));
    state.ends.clear();
    state.ends.extend(
        (0..=max_reachable)
            .filter(|&offset| visited(state.visited, matched_base, offset))
            .map(|offset| path_index + offset),
    );
    state.candidate_ends.clear();
    if let Some(through_sep_base) = through_sep_base {
        state.candidate_ends.extend(
            (0..=max_reachable)
                .filter(|&offset| visited(state.visited, through_sep_base, offset))
                .map(|offset| path_index + offset),
        );
    }
}

fn prepare_extglob_sweeps(
    group: &ExtglobGroup,
    narrow: &mut Vec<u64>,
    wide: &mut Vec<SweepState>,
) -> (usize, usize) {
    let mut narrow_count = 0;
    let mut wide_count = 0;
    for (alternative, _) in group.repetition_alternatives() {
        for compiled in &alternative.compiled {
            let Some(sweep) = &compiled.sweep else {
                continue;
            };
            if sweep.is_wide() {
                if let Some(state) = wide.get_mut(wide_count) {
                    sweep.reset_state(state);
                } else {
                    wide.push(sweep.empty_state());
                }
                wide_count += 1;
            } else {
                if let Some(slot) = narrow.get_mut(narrow_count) {
                    *slot = 0;
                } else {
                    narrow.push(0);
                }
                narrow_count += 1;
            }
        }
    }
    (narrow_count, wide_count)
}

/// Where one match of a group alternative starts and how far it may reach.
#[derive(Clone, Copy)]
struct AlternativeSpan<'a> {
    path: &'a [u8],
    start: usize,
    /// See [`ExtglobGroup::alternative_end_limit`].
    end_limit: usize,
    /// Whether the alternative's first wildcard is in a component-local
    /// position: the group's own position for its first iteration.
    root_component_local: bool,
}

fn matching_extglob_alternative_ends(
    alternative: &ExtglobAlternative,
    span: AlternativeSpan<'_>,
    options: PatternOptions,
    prefix_sweep_state: &mut Option<SweepState>,
    output: &mut Vec<usize>,
) {
    let AlternativeSpan {
        path,
        start: path_index,
        end_limit: component_end,
        root_component_local,
    } = span;
    if let Some(width) = alternative.width {
        let Some(end) = path_index.checked_add(width) else {
            return;
        };
        if end <= component_end
            && match_extglob_alternative_exact(
                alternative,
                &path[path_index..end],
                at_component_start(path, path_index, options),
                options,
                root_component_local,
            )
        {
            output.push(end);
        }
        return;
    }

    let alternative_options =
        extglob_alternative_options(path, path_index, options, root_component_local);
    let suffix = &path[path_index..component_end];
    for compiled in &alternative.compiled {
        if let Some(sweep) = &compiled.sweep {
            sweep.matching_prefix_ends(
                suffix,
                alternative_options,
                path_index,
                prefix_sweep_state,
                output,
            );
        } else {
            let elided_tokens = match compiled.tokens.as_slice() {
                [prefix @ .., Token::Separator, Token::RecursiveStar] => Some(prefix),
                _ => None,
            };
            let blocks_hidden_end = matches!(
                compiled.tokens.last(),
                Some(Token::Star | Token::RecursiveStar)
            );
            let elided_blocks_hidden_end = elided_tokens.is_some_and(|prefix| {
                matches!(prefix.last(), Some(Token::Star | Token::RecursiveStar))
            });
            for end in path_index..=component_end {
                if blocks_hidden_end && star_stops_before_hidden_component(path, end, options) {
                    if let Some(prefix) = elided_tokens
                        && !elided_blocks_hidden_end
                        && Pattern::matches_general(
                            prefix,
                            &path[path_index..end],
                            alternative_options,
                        )
                    {
                        output.push(end);
                    }
                    continue;
                }
                if Pattern::match_alternatives(
                    std::slice::from_ref(compiled),
                    alternative_options,
                    &path[path_index..end],
                ) {
                    output.push(end);
                }
            }
        }
    }
}

fn extglob_alternative_options(
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    root_component_local: bool,
) -> PatternOptions {
    PatternOptions {
        braces: false,
        extglob: false,
        candidate_starts_component: at_component_start(path, path_index, options),
        candidate_root_component_wildcard: root_component_local,
        ..options
    }
}

/// Matches one compiled alternative against the whole of `path`.
///
/// The component policy comes from the caller, reproducing the entry point the
/// per-match compile picked: `is_match_glob_path` once the root is
/// component-local, `is_match` otherwise.
fn match_extglob_alternative_exact(
    alternative: &ExtglobAlternative,
    path: &[u8],
    candidate_starts_component: bool,
    options: PatternOptions,
    root_component_local: bool,
) -> bool {
    if alternative.width.is_some_and(|width| width != path.len()) {
        return false;
    }
    let mut options = extglob_alternative_options(path, 0, options, root_component_local);
    options.candidate_starts_component = candidate_starts_component;
    Pattern::match_alternatives(&alternative.compiled, options, path)
}

/// One past the last offset at which a negated group's span may stop, given
/// the `component_end` its locality allows.
///
/// A negation consumes matcher text like an ordinary `*`, so it observes the
/// same leading-period rule (ADR-0011): without `match_hidden` it neither
/// consumes the period that starts a component nor stops immediately before
/// it, which would let a following literal opt into the hidden name
/// implicitly. The group-start check already refuses a hidden component at
/// `path_index` itself, so this bounds a span that would otherwise cross a
/// separator into a hidden component.
fn negated_extglob_stop_limit(
    path: &[u8],
    path_index: usize,
    component_end: usize,
    options: PatternOptions,
) -> usize {
    (path_index..component_end)
        .find(|&index| star_stops_before_hidden_component(path, index, options))
        .unwrap_or(component_end + 1)
}

fn extglob_component_end(
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    position_component_local: bool,
) -> usize {
    if options.component_wildcards && (options.root_component_wildcards || position_component_local)
    {
        path_index
            + path[path_index..]
                .iter()
                .position(|byte| is_separator(*byte))
                .unwrap_or(path.len() - path_index)
    } else {
        path.len()
    }
}

impl CompiledExtglob {
    fn memo_state_index(&self, offset: usize) -> usize {
        let state_index = self.memo_state_indices[offset];
        debug_assert_ne!(
            state_index,
            usize::MAX,
            "only the entry point and group transitions participate in extglob recursion"
        );
        state_index
    }
}

#[cfg(test)]
mod tests {
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    use super::LiteralSuffix;
    use super::{
        AlternativeFastPath, ComponentBounds, ExtglobStep, FailedStates, FastPath, IrBudget,
        Pattern, PatternOptions, PositiveExtglobMatcher, PositiveExtglobState, Prefilter,
        ProvenanceBudget, SweepEngine, Token, WalkerPathViability, extglob_closing_scanned_bytes,
        extglob_closing_table_len, extglob_failed_len, extglob_failed_stats,
        extglob_group_expansions, extglob_memoized_star_fallbacks, extglob_memoized_star_visits,
        extglob_pending_peak, extglob_scratch_capacities, positive_extglob_scratch_capacities,
        reset_extglob_closing_scanned_bytes, reset_extglob_memoized_star_fallbacks,
        reset_scoped_suffix_set_scan_stats, reset_unconditional_brace_scanned_bytes,
        scoped_suffix_set_scan_stats, scratch_capacities, sweep_is_deferred,
        unconditional_brace_peak, unconditional_brace_scanned_bytes,
    };

    fn compile(pattern: &str) -> Pattern {
        Pattern::compile(pattern, PatternOptions::default()).unwrap()
    }

    #[test]
    fn compiled_patterns_stay_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Pattern>();
        assert_send_sync::<PatternOptions>();
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    #[test]
    fn apple_silicon_only_packs_short_case_sensitive_suffixes() {
        assert!(matches!(
            LiteralSuffix::new(b".ts".to_vec(), false),
            LiteralSuffix::Packed16(_)
        ));
        assert!(matches!(
            LiteralSuffix::new(b".ts".to_vec(), true),
            LiteralSuffix::Plain(_)
        ));
        assert!(matches!(
            LiteralSuffix::new(vec![b'x'; 17], false),
            LiteralSuffix::Plain(_)
        ));
    }

    #[test]
    fn repeated_general_matches_reuse_the_thread_local_scratch() {
        // libtest gives each test its own thread, so the scratch observed here
        // is private to this test.
        let mut pattern = Pattern::compile(
            "**/*.ts",
            PatternOptions::default().recursive_double_star(true),
        )
        .unwrap();
        // The scratch belongs to the memoized matcher; pin it, since the
        // sweep engine would otherwise answer for this shape without any
        // buffers at all.
        pattern.strip_engines(true, true, false);
        let matching = "src/deep/nested/module/component/widget/main.ts";
        let other = "src/deep/nested/module/component/widget/main.tsx";

        for _ in 0..8 {
            assert!(pattern.is_match_glob_path(matching));
            assert!(!pattern.is_match_glob_path(other));
        }
        let warm = scratch_capacities();
        assert!(
            warm.0 > 0 && warm.1 > 0 && warm.2 > 0,
            "this shape must leave the inline budget and use every buffer: {warm:?}"
        );

        for _ in 0..1_000 {
            assert!(pattern.is_match_glob_path(matching));
            assert!(!pattern.is_match_glob_path(other));
        }
        assert_eq!(
            scratch_capacities(),
            warm,
            "steady-state matching must not grow a scratch buffer"
        );

        // One oversized candidate grows the matrix, but must not keep it.
        let long = "x".repeat(300_000);
        let mut oversized = Pattern::compile("*a*y", PatternOptions::default()).unwrap();
        oversized.strip_engines(false, true, true);
        assert!(!oversized.is_match(&long));
        assert!(
            scratch_capacities().0 <= super::RETAINED_SCRATCH_WORDS,
            "an oversized candidate must release the matrix it needed"
        );
    }

    #[test]
    fn literal_skipping_honours_component_and_leading_dot_policies() {
        // A skip must never carry a star across a byte it may not consume.
        let component = Pattern::compile(
            "*b.ts",
            PatternOptions {
                component_wildcards: true,
                root_component_wildcards: true,
                ..PatternOptions::default()
            },
        )
        .unwrap();
        assert!(component.is_match("ab.ts"));
        assert!(!component.is_match("a/b.ts"));

        // The dot that starts a component blocks the jump behind it.
        assert!(!compile("*b.ts").is_match("a/.b.ts"));
        assert!(
            Pattern::compile("*b.ts", PatternOptions::default().match_hidden(true))
                .unwrap()
                .is_match("a/.b.ts")
        );
        assert!(!compile("*.ts").is_match(".hidden.ts"));

        // A jump target that only appears past the barrier is not reachable,
        // but an earlier one still is.
        assert!(compile("*b.ts").is_match("bb.ts"));
        assert!(!compile("*x.ts").is_match(".a/x.ts"));
    }

    /// The walker preset is the conservative default with exactly the three
    /// syntax switches a filesystem glob needs; hidden names, case folding and
    /// escaping keep their defaults.
    #[test]
    fn walker_preset_enables_exactly_the_walker_syntax() {
        assert_eq!(
            PatternOptions::walker(),
            PatternOptions::default()
                .recursive_double_star(true)
                .braces(true)
                .extglob(true)
        );
    }

    #[test]
    fn matches_literals_and_component_wildcards() {
        assert!(compile("src/*.rs").is_match("src/lib.rs"));
        assert!(compile("src/?.rs").is_match("src/a.rs"));
        assert!(compile("src/*.rs").is_match("src/bin/main.rs"));
        assert!(compile("*.rs").is_match("lib.rs"));

        let star = compile("*");
        assert!(star.is_match("d/e/a.c"));
        assert!(star.is_match_path("d/e/a.c"));
        assert!(!star.is_match_glob_path("d/e/a.c"));
        assert!(compile("?").is_match("/"));
    }

    #[test]
    fn leading_period_requires_an_explicit_option_or_literal() {
        assert!(!compile("*").is_match(".gitignore"));
        assert!(!compile("*.rs").is_match(".rs"));
        assert!(compile(".*").is_match(".gitignore"));
        assert!(
            Pattern::compile("*", PatternOptions::default().match_hidden(true))
                .unwrap()
                .is_match(".gitignore")
        );
        assert!(
            Pattern::compile("*.rs", PatternOptions::default().match_hidden(true))
                .unwrap()
                .is_match(".rs")
        );
    }

    /// An escape has only its escaped reading in both extglob engines: a
    /// literal backslash followed by the escaped byte read as syntax is not
    /// a second branch. Bash 5.2 with `extglob` gives every verdict below.
    #[test]
    fn extglob_escapes_match_only_the_escaped_byte() {
        let options = PatternOptions::default().extglob(true);
        // `Some(true)` is the positive NFA, `Some(false)` the retained
        // interpreter, `None` a pattern whose only group opener is escaped
        // and which therefore stays with the plain engines.
        for (pattern, candidate, expected, positive_nfa) in [
            ("\\*@(a)", "*a", true, Some(true)),
            ("\\*@(a)", "\\xa", false, Some(true)),
            ("\\*@(a)", "xa", false, Some(true)),
            ("*(a)\\*", "aa*", true, Some(true)),
            ("*(a)\\*", "*", true, Some(true)),
            ("*(a)\\*", "\\x)\\", false, Some(true)),
            ("@(a)\\/b", "a/b", true, Some(true)),
            ("@(a)\\/b", "a\\/b", false, Some(true)),
            // An outer star or a negated group keeps the retained
            // interpreter.
            ("*\\*@(a)", "x*a", true, Some(false)),
            ("*\\*@(a)", "*a", true, Some(false)),
            ("*\\*@(a)", "x\\xa", false, Some(false)),
            ("!(b)\\*", "a*", true, Some(false)),
            ("!(b)\\*", "\\x", false, Some(false)),
            ("!(b)\\*", "a\\x", false, Some(false)),
            ("\\*(a)", "*(a)", true, None),
            ("\\*(a)", "\\a", false, None),
        ] {
            let compiled = Pattern::compile(pattern, options).expect("pattern compiles");
            assert_eq!(
                compiled.alternatives[0]
                    .extglob
                    .as_ref()
                    .map(|program| program.positive_nfa.is_some()),
                positive_nfa,
                "{pattern}: engine selection"
            );
            assert_eq!(
                compiled.is_match(candidate),
                expected,
                "{pattern} against {candidate}"
            );
            assert!(
                compiled.engines_agree(candidate),
                "{pattern} against {candidate}: engines disagree"
            );
        }
    }

    /// zlob's list filter makes a wildcard component-local only when it
    /// stands directly behind an explicit separator. A later wildcard in the
    /// same component, the star of `?*`, a star behind a class or a literal,
    /// or the second star of a non-recursive `**`, crosses separators again;
    /// the zlob oracle lane replays these verdicts from the corpus. `is_match`
    /// crosses throughout and `is_match_glob_path` is component-local
    /// throughout, so the three entry points stay distinguishable.
    #[test]
    fn only_a_wildcard_directly_behind_a_separator_is_component_local() {
        let options = PatternOptions::default();
        let local = Pattern::compile("a/*/z", options).unwrap();
        assert!(local.is_match_path("a/b/z"));
        assert!(
            !local.is_match_path("a/b/c/z"),
            "a/*/z stays inside one component"
        );
        for pattern in ["a/**/z", "a/?*/z", "a/*[a-z]*/z", "a/b*/z"] {
            let compiled = Pattern::compile(pattern, options).unwrap();
            assert!(compiled.is_match("a/b/c/z"), "{pattern}: is_match crosses");
            assert!(compiled.is_match_path("a/b/z"), "{pattern}: one component");
            assert!(
                compiled.is_match_path("a/b/c/z"),
                "{pattern}: the wildcard behind the first one crosses under is_match_path, as in zlob's matchPaths"
            );
            assert!(
                !compiled.is_match_glob_path("a/b/c/z"),
                "{pattern}: the glob-path reading is component-local throughout"
            );
            for candidate in ["a/b/z", "a/b/c/z"] {
                assert!(
                    compiled.engines_agree(candidate),
                    "{pattern}: engines disagree on {candidate}"
                );
            }
        }
    }

    /// A zero-width extglob branch still observes the leading-period rule;
    /// only a branch that spells the period opts a hidden component in.
    #[test]
    fn zero_width_extglob_branches_keep_the_leading_period_rule() {
        let walker_options = PatternOptions::default()
            .braces(true)
            .recursive_double_star(true)
            .extglob(true);
        let zero_width = Pattern::compile("**/?(visible).hidden/keep.txt", walker_options)
            .expect("pattern compiles");
        assert!(!zero_width.is_match("target/.hidden/keep.txt"));
        let explicitly_hidden = Pattern::compile("**/?(.visible).hidden/keep.txt", walker_options)
            .expect("pattern compiles");
        assert!(explicitly_hidden.is_match("target/.hidden/keep.txt"));
    }

    #[test]
    fn recursive_double_star_is_explicit_and_component_aware() {
        let options = PatternOptions::default().recursive_double_star(true);
        let pattern = Pattern::compile("**/*.rs", options).unwrap();
        assert!(pattern.is_match("lib.rs"));
        assert!(pattern.is_match("src/bin/main.rs"));
        assert!(!pattern.is_match("src/.private.rs"));
        assert!(Pattern::compile("src/**", options).unwrap().is_match("src"));
        assert!(compile("**/*.rs").is_match("src/main.rs"));
        assert!(compile("**/*.rs").is_match("src/bin/main.rs"));
    }

    #[test]
    fn long_candidates_do_not_overflow_the_native_stack() {
        // Star repetition used to recurse once per consumed path byte, so a
        // few thousand candidate bytes aborted the process. 256 KiB is well
        // below the 2 MiB a worker thread gets by default, which pins the
        // bound: the matcher must not use the native stack per path byte.
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let filler = "x".repeat(300_000);
                let pattern = compile("*a*y");
                assert!(!pattern.is_match(&filler));

                let mut matching = String::with_capacity(filler.len() + 2);
                matching.push_str(&filler[..150_000]);
                matching.push('a');
                matching.push_str(&filler[150_000..]);
                matching.push('y');
                assert!(pattern.is_match(&matching));

                // The walker routes every starred pattern through the general
                // matcher once wildcards are component-local.
                assert!(pattern.is_match_glob_path(&matching));
                assert!(!pattern.is_match_glob_path(&filler));

                let recursive = Pattern::compile(
                    "**/*a*.rs",
                    PatternOptions::default().recursive_double_star(true),
                )
                .unwrap();
                let mut deep = "dir/".repeat(20_000);
                deep.push_str("zzz.rs");
                assert!(!recursive.is_match_glob_path(&deep));
                deep.truncate(deep.len() - "zzz.rs".len());
                deep.push_str("zaz.rs");
                assert!(recursive.is_match_glob_path(&deep));
            })
            .expect("spawn a small-stack matcher worker")
            .join()
            .expect("the matcher must not overflow a 256 KiB stack");
    }

    #[test]
    fn character_classes_support_ranges_and_negation() {
        assert!(compile("file[0-9].rs").is_match("file7.rs"));
        assert!(compile("file[!0-9].rs").is_match("filex.rs"));
        assert!(compile("file[^0-9].rs").is_match("filex.rs"));
        assert!(!compile("file[!0-9].rs").is_match("file7.rs"));
    }

    #[test]
    fn escaped_dash_in_a_class_is_a_literal_member() {
        let pattern = compile(r"[a\-z]");
        assert!(pattern.is_match("a"));
        assert!(pattern.is_match("-"));
        assert!(pattern.is_match("z"));
        assert!(!pattern.is_match("b"));
        assert!(!pattern.is_match("y"));
        assert!(!pattern.is_match("\\"));
    }

    #[test]
    fn escaped_dash_stays_literal_without_escape_processing() {
        let options = PatternOptions::default().escape(false);
        let pattern = Pattern::compile(r"[a\-z]", options).unwrap();
        assert!(pattern.is_match("b"));
        assert!(pattern.is_match("\\"));
        assert!(!pattern.is_match("-"));
    }

    #[test]
    fn escaped_class_members_still_bound_ranges() {
        let low = compile(r"[\--0]");
        assert!(low.is_match("-"));
        assert!(low.is_match("0"));
        assert!(!low.is_match("a"));
        assert!(!low.is_match("\\"));

        let reversed = compile(r"[a-\-]");
        assert!(!reversed.is_match("a"));
        assert!(!reversed.is_match("-"));
        assert!(!reversed.is_match("\\"));

        let escaped_start = compile(r"[\a-z]");
        assert!(escaped_start.is_match("a"));
        assert!(escaped_start.is_match("b"));
        assert!(escaped_start.is_match("z"));
        assert!(!escaped_start.is_match("-"));
    }

    #[test]
    fn unescaped_edge_dashes_stay_literal() {
        let trailing = compile("[a-]");
        assert!(trailing.is_match("a"));
        assert!(trailing.is_match("-"));
        assert!(!trailing.is_match("b"));

        let leading = compile("[-a]");
        assert!(leading.is_match("a"));
        assert!(leading.is_match("-"));
        assert!(!leading.is_match("b"));

        let both = compile("[-a-c-]");
        assert!(both.is_match("-"));
        assert!(both.is_match("b"));
        assert!(!both.is_match("d"));
    }

    #[test]
    fn negated_classes_honour_escaped_dashes() {
        let pattern = compile(r"[!a\-z]");
        assert!(!pattern.is_match("a"));
        assert!(!pattern.is_match("-"));
        assert!(!pattern.is_match("z"));
        assert!(pattern.is_match("b"));
    }

    #[test]
    fn character_classes_support_posix_named_sets() {
        assert!(compile("[[:alpha:]]").is_match("a"));
        assert!(!compile("[[:alpha:]]").is_match("7"));
        assert!(compile("[[:digit:]]").is_match("7"));
        assert!(compile("[[:word:]]").is_match("_"));
        assert!(compile("[![:space:]]").is_match("x"));
    }

    #[test]
    fn case_folding_makes_upper_and_lower_classes_ascii_alphabetic() {
        let folded = PatternOptions::default().case_insensitive(true);
        for class in ["lower", "upper"] {
            let pattern = Pattern::compile(format!("[[:{class}:]]"), folded).unwrap();
            assert!(pattern.is_match("a"));
            assert!(pattern.is_match("A"));
            assert!(!pattern.is_match("1"));
        }

        let not_upper = Pattern::compile("[![:upper:]]", folded).unwrap();
        assert!(!not_upper.is_match("A"));
        assert!(not_upper.is_match("1"));

        let path = Pattern::compile("src/[[:upper:]]*.rs", folded).unwrap();
        assert!(path.is_match("src/Main.rs"));

        assert!(compile("[[:upper:]]").is_match("A"));
        assert!(!compile("[[:upper:]]").is_match("a"));
        assert!(compile("[[:lower:]]").is_match("a"));
        assert!(!compile("[[:lower:]]").is_match("A"));
    }

    /// An empty component has no byte of its own; it is located at the
    /// separator that closes it, or for a trailing one the separator that opens
    /// it, through brace expansion and extglob arms alike (#400).
    #[test]
    fn walker_empty_component_is_located_at_its_separator() {
        let options = PatternOptions::default()
            .braces(true)
            .recursive_double_star(true)
            .extglob(true);
        for (source, offset) in [
            ("/bar", 0),
            ("src//*.ts", 4),
            ("a///b", 2),
            ("**//x", 3),
            ("src/", 3),
            ("src/{}/bar", 6),
            ("{a,b}//x", 6),
            ("src/{,./a.rs}/bar", 13),
            ("src/?()/bar", 7),
            ("src/@(a/)/bar", 9),
            ("src/@(a/|b/)", 7),
        ] {
            let compiled = Pattern::compile(source, options).expect("pattern compiles");
            assert_eq!(
                compiled.walker_path_viability(),
                WalkerPathViability::EmptyComponent,
                "{source}"
            );
            assert_eq!(
                compiled.walker_path_problem_offset(),
                Some(offset),
                "{source}"
            );
            assert_eq!(
                source.as_bytes()[offset],
                b'/',
                "{source} points at a separator"
            );
        }

        // The location is diagnostic only. Arms that end in distinct
        // separators still leave one walker state, so a wide group is charged
        // as before instead of once per pair of arms and running out of IR.
        let arms = (0..1000)
            .map(|index| format!("a{index}/"))
            .collect::<Vec<_>>()
            .join("|");
        let empty_arms = (0..1000)
            .map(|index| format!("a{index}//"))
            .collect::<Vec<_>>()
            .join("|");
        for source in [
            format!("*({arms})x"),
            format!("@({arms})@({arms})x"),
            format!("*({empty_arms})x"),
        ] {
            assert_eq!(
                Pattern::compile(&source, options)
                    .expect("a wide group stays within the IR budget")
                    .walker_path_viability(),
                WalkerPathViability::Viable
            );
        }
    }

    #[test]
    fn brace_alternatives_carry_the_source_offset_of_their_first_byte() {
        let braces = PatternOptions::default().braces(true);
        let expand = |source: &str, options| {
            super::expand_braces_with_first_offsets(source, options)
                .expect("review pattern expands")
                .into_iter()
                .map(|(bytes, first)| (String::from_utf8(bytes).unwrap(), first))
                .collect::<Vec<_>>()
        };

        assert_eq!(expand("!src/**", braces), [("!src/**".into(), Some(0))]);
        assert_eq!(
            expand("{!a,b}/**", braces),
            [("!a/**".into(), Some(1)), ("b/**".into(), Some(4))]
        );
        assert_eq!(
            expand("{a,}!b", braces),
            [("a!b".into(), Some(1)), ("!b".into(), Some(4))]
        );
        assert_eq!(
            expand("x{{!a,b},c}", braces),
            [
                ("x!a".into(), Some(0)),
                ("xb".into(), Some(0)),
                ("xc".into(), Some(0))
            ]
        );
        assert_eq!(
            expand("{{!a,b},c}", braces),
            [
                ("!a".into(), Some(2)),
                ("b".into(), Some(5)),
                ("c".into(), Some(8))
            ]
        );
        assert_eq!(
            expand("{,}", braces),
            [(String::new(), None), (String::new(), None)]
        );
        assert_eq!(
            expand("{!a,b}", PatternOptions::default()),
            [("{!a,b}".into(), Some(0))]
        );
        assert_eq!(
            super::expand_braces_with_first_offsets("{!a,b}/**", braces)
                .unwrap()
                .into_iter()
                .map(|(bytes, _)| bytes)
                .collect::<Vec<_>>(),
            super::expand_braces("{!a,b}/**", braces).unwrap(),
            "the alternatives are the public expansion's"
        );
    }

    #[test]
    fn walker_path_viability_follows_compiled_alternatives() {
        let options = PatternOptions::default().braces(true).extglob(true);
        fn viability(source: &str, options: PatternOptions) -> WalkerPathViability {
            Pattern::compile(source, options)
                .expect("review pattern compiles")
                .walker_path_viability()
        }

        let posix = Pattern::compile("src/[[:alpha:]/../].rs", options).unwrap();
        assert!(posix.is_match("src/a.rs"));
        assert_eq!(
            viability("src/[[:alpha:]/../].rs", options),
            WalkerPathViability::Viable
        );

        let leading_bracket = Pattern::compile("src/[]/../].rs", options).unwrap();
        assert!(leading_bracket.is_match("src/].rs"));
        assert_eq!(
            viability("src/[]/../].rs", options),
            WalkerPathViability::Viable
        );

        // A literal parent component is invalid walker input even when a
        // sibling brace arm can name a candidate. Silently dropping that arm
        // makes a caller typo depend on expansion order.
        assert_eq!(
            viability("{dead/../branch,src/main.rs}", options),
            WalkerPathViability::ParentComponent
        );
        assert_eq!(
            viability("src/{a,..}", options),
            WalkerPathViability::ParentComponent
        );

        // Extglob alternatives remain matcher branches rather than brace-
        // expanded path inputs.
        assert_eq!(
            viability("@(dead/../branch|src/main.rs)", options),
            WalkerPathViability::Viable
        );
        assert_eq!(
            viability("{dead/{nested/../branch},src/main.rs}", options),
            WalkerPathViability::ParentComponent
        );
        assert_eq!(
            viability("@(dead/@(nested/../branch)|src/main.rs)", options),
            WalkerPathViability::Viable
        );
        assert_eq!(
            viability(r"dead\/../branch", options),
            WalkerPathViability::Viable
        );

        let brace_class = Pattern::compile("src/[{],a}/../].rs", options).unwrap();
        assert!(brace_class.is_match("src/a.rs"));
        assert!(brace_class.is_match("src/].rs"));
        assert_eq!(
            brace_class.walker_path_viability(),
            WalkerPathViability::Viable
        );

        let brace_extglob =
            Pattern::compile("@(dead/{),x}/../branch|src/main.rs)", options).unwrap();
        assert!(brace_extglob.is_match("src/main.rs"));
        assert_eq!(
            brace_extglob.walker_path_viability(),
            WalkerPathViability::Viable
        );

        // A brace or extglob delimiter is processed by its compiler phase
        // before the later class parse. These compile into an outer `..`.
        for source in [
            "{dead/[}]]/../x,src/main.rs}",
            "{dead/../branch}",
            "@(dead/../branch)",
            "@(dead|src)/../main.rs",
        ] {
            assert_eq!(
                viability(source, options),
                WalkerPathViability::ParentComponent,
                "all compiled arms remain unwalkable: {source}"
            );
        }
    }

    /// #395: the path entry points ignore one leading `./` on each expanded
    /// alternative, so the walker summary and the separator-crossing walk
    /// reading must too.
    #[test]
    fn a_leading_dot_slash_is_read_per_brace_alternative() {
        let options = PatternOptions::default()
            .braces(true)
            .extglob(true)
            .recursive_double_star(true);
        let compile = |source: &str| Pattern::compile(source, options).expect("valid pattern");

        for source in [
            "{./src/*.rs,./lib/*.rs}",
            "{./src/*.rs,lib/*.rs}",
            "{./a.rs}",
            "{x,}./src/*.rs",
            "{{./src,lib}/*.rs,x}",
            "./{src,lib}/*.rs",
        ] {
            assert_eq!(
                compile(source).walker_path_viability(),
                WalkerPathViability::Viable,
                "{source}"
            );
        }
        // Only one prefix per alternative, and only at its start.
        for (source, offset) in [
            ("{././a,././b}", 3),
            ("src/{./a,./b}", 5),
            ("{./src/./a,./lib/./b}", 7),
        ] {
            let pattern = compile(source);
            assert_eq!(
                pattern.walker_path_viability(),
                WalkerPathViability::DotComponent,
                "{source}"
            );
            assert_eq!(
                pattern.walker_path_problem_offset(),
                Some(offset),
                "{source}"
            );
        }

        for (source, path, expected) in [
            ("{./src/*.rs,lib/*.rs}", "src/m.rs", true),
            ("{./src/*.rs,lib/*.rs}", "lib/n.rs", true),
            ("{./src/*.rs,./lib/*.rs}", "lib/n.rs", true),
            ("{./src/*.rs,lib/*.rs}", "src/a/m.rs", true),
            ("{./@(src|lib)/*.rs,x}", "lib/a/n.rs", true),
            ("{x,}./src/*", "src/m.rs", true),
            ("{./src,lib}", "src", true),
            ("./{./src/*.rs,lib/*.rs}", "src/m.rs", false),
            ("./{./src/*.rs,lib/*.rs}", "lib/n.rs", true),
            ("src/{./a,b}/*.rs", "src/a/m.rs", false),
            ("{./src/*.rs,lib/*.rs}", "docs/d.md", false),
        ] {
            let pattern = compile(source);
            assert_eq!(
                pattern.is_match_crossing_path(path),
                expected,
                "{source} vs {path}"
            );
            // The crossing reading differs from the component-local one only
            // in how far an ordinary wildcard reaches.
            if !path.contains("/a/") {
                assert_eq!(
                    pattern.is_match_glob_path(path),
                    expected,
                    "{source} vs {path}"
                );
            }
        }
        // An extglob branch is not a brace alternative: its `./` stays a real
        // `.` component under every path reading.
        let extglob = compile("@(./a.rs)");
        assert!(!extglob.is_match_glob_path("a.rs"));
        assert!(!extglob.is_match_crossing_path("a.rs"));
        // Without a `./` alternative the crossing reading is `is_match`.
        let plain = compile("src/*.rs");
        assert!(plain.is_match_crossing_path("src/a/m.rs"));
        assert!(!plain.is_match_glob_path("src/a/m.rs"));
    }

    #[test]
    fn walker_path_viability_composes_extglob_quantifiers_without_state_products() {
        let options = PatternOptions::default().braces(true).extglob(true);
        let mandatory =
            Pattern::compile("+(dead/../branch)", options).expect("mandatory extglob compiles");
        assert!(mandatory.is_match("dead/../branch"));
        assert_eq!(
            mandatory.walker_path_viability(),
            WalkerPathViability::ParentComponent,
            "one required repetition preserves its real parent component"
        );
        assert_eq!(
            Pattern::compile("prefix+(../bar)", options)
                .expect("prefixed repetition compiles")
                .walker_path_viability(),
            WalkerPathViability::Viable,
            "the compiler composes an arm with its outer component"
        );

        for source in [
            "?(dead/../branch)",
            "*(dead/../branch)",
            "+(dead/../branch)",
            "@(dead/../branch)",
        ] {
            assert_ne!(
                Pattern::compile(source, options)
                    .expect("quantified extglob compiles")
                    .walker_path_viability(),
                WalkerPathViability::Viable,
                "every matching repetition of {source} remains unwalkable"
            );
        }
        assert_eq!(
            Pattern::compile("!(dead/../branch)", options)
                .expect("negated extglob compiles")
                .walker_path_viability(),
            WalkerPathViability::Viable
        );

        // Only a `./` that starts the source or a brace-expanded alternative
        // is normalized, as the path entry points normalize it (`{./a.rs}` is
        // viable, #395). An extglob-produced leading component is still a real
        // unmatchable dot component. Nor may a nullable arm use an empty
        // leading or interior component to escape that invalid positive arm.
        for source in [
            "@(./a.rs)",
            "src/?(./a.rs)",
            "src/*(./a.rs)",
            "src/?(./a.rs)/bar",
            "src/*(./a.rs)/bar",
        ] {
            assert_eq!(
                Pattern::compile(source, options)
                    .expect("edge-normalization regression compiles")
                    .walker_path_viability(),
                WalkerPathViability::DotComponent,
                "{source} has no selectable walker candidate"
            );
        }
        for source in ["src/?()/bar", "src/*()/bar", "?()/bar", "*()/bar"] {
            assert_eq!(
                Pattern::compile(source, options)
                    .expect("empty extglob arm compiles")
                    .walker_path_viability(),
                WalkerPathViability::EmptyComponent,
                "{source} has only an empty leading or interior component"
            );
        }
        for source in ["src//bar", "/bar", "src/{}/bar", "src/{,./a.rs}/bar"] {
            assert_eq!(
                Pattern::compile(source, options)
                    .expect("empty plain or brace arm compiles")
                    .walker_path_viability(),
                WalkerPathViability::EmptyComponent,
                "{source} has no selectable complete walker alternative"
            );
        }
        // Only a pattern made of nothing but empty and `.` components names
        // the root itself.
        for source in ["", ".", "./", "//"] {
            assert_eq!(
                Pattern::compile(source, options)
                    .expect("root spelling compiles")
                    .walker_path_viability(),
                WalkerPathViability::Root,
                "{source} names only the walk root"
            );
        }
        for source in ["src/?(x)/bar", "src/?()bar", "?(x)/bar"] {
            assert_eq!(
                Pattern::compile(source, options)
                    .expect("selectable nullable control compiles")
                    .walker_path_viability(),
                WalkerPathViability::Viable,
                "{source} retains a selectable compiler arm"
            );
        }
        assert_eq!(
            Pattern::compile("./a.rs", options)
                .expect("raw normalized spelling compiles")
                .walker_path_viability(),
            WalkerPathViability::Viable
        );

        // Each group has two compiler arms, but their walker summaries are
        // equal. This used to construct 2^32 duplicate state vectors; the
        // canonical state set stays one state per group transition.
        let sequential = "@(a|b)".repeat(32);
        let compiled = Pattern::compile(&sequential, options)
            .expect("sequential equivalent extglobs stay within the IR budget");
        assert_eq!(
            compiled.walker_path_viability(),
            WalkerPathViability::Viable
        );

        // Nesting must take the same bounded compiler path rather than falling
        // back to a second source-level grammar.
        let nested = "@(@(a|b)|@(c|d))".repeat(16);
        Pattern::compile(&nested, options)
            .expect("nested equivalent extglobs stay within the IR budget");
    }

    #[test]
    fn braces_expand_nested_and_empty_alternatives() {
        let options = PatternOptions::default().braces(true);
        let pattern = Pattern::compile("{src,{lib,bin}}/*.{rs,toml}", options).unwrap();
        assert!(pattern.is_match("src/main.rs"));
        assert!(pattern.is_match("lib/Cargo.toml"));
        assert!(pattern.is_match("bin/main.rs"));
        assert!(!pattern.is_match("tests/main.rs"));
        assert!(
            Pattern::compile("test{,_suffix}.txt", options)
                .unwrap()
                .is_match("test_suffix.txt")
        );
    }

    #[test]
    fn brace_expansion_keeps_its_order() {
        // The work list must emit combinations in the order the recursive
        // expansion did: every choice of the first group before the second's.
        assert_eq!(
            super::expand_brace_alternatives(b"{a,b}{c,d}", true).unwrap(),
            vec![
                b"ac".to_vec(),
                b"ad".to_vec(),
                b"bc".to_vec(),
                b"bd".to_vec()
            ]
        );
        assert_eq!(
            super::expand_brace_alternatives(b"{x,{y,z}}!", true).unwrap(),
            vec![b"x!".to_vec(), b"y!".to_vec(), b"z!".to_vec()]
        );
    }

    #[test]
    fn brace_provenance_stays_span_bounded_for_large_and_branching_sources() {
        let mut provenance_budget = super::ProvenanceBudget::new();
        let literal = vec![b'x'; 1 << 20];
        let expanded = super::expand_brace_alternatives_with_provenance(
            &literal,
            Some(&super::SourceProvenance::Contiguous { source_start: 0 }),
            true,
            &mut provenance_budget,
        )
        .expect("large literal has no provenance allocation per byte");
        assert!(matches!(
            expanded[0].source_provenance,
            Some(super::SourceProvenance::Contiguous { source_start: 0 })
        ));

        let branching = "{a,b}".repeat(12);
        let expanded = super::expand_brace_alternatives_with_provenance(
            branching.as_bytes(),
            Some(&super::SourceProvenance::Contiguous { source_start: 0 }),
            true,
            &mut provenance_budget,
        )
        .expect("bounded brace expansion compiles");
        assert_eq!(expanded.len(), 1 << 12);
        let span_count = expanded
            .iter()
            .map(|alternative| match &alternative.source_provenance {
                Some(super::SourceProvenance::Spans(spans)) => spans.len(),
                Some(super::SourceProvenance::Contiguous { .. }) | None => 0,
            })
            .sum::<usize>();
        assert!(
            span_count <= 12 * (1 << 12),
            "one compact source span per selected brace arm, never one usize per output byte"
        );

        let mut capped = super::ProvenanceBudget::new();
        assert!(
            capped.charge(super::MAX_BRACE_EXPANSION_BYTES, 0).is_err(),
            "provenance rejects before a span vector can exceed its byte bound"
        );
    }

    #[test]
    fn brace_expansion_stops_at_the_alternative_budget() {
        let options = PatternOptions::default().braces(true);
        // Two-way groups make the boundary exact: 2^12 is the budget.
        let within = "{a,b}".repeat(12);
        assert_eq!(
            super::expand_brace_alternatives(within.as_bytes(), true)
                .unwrap()
                .len(),
            super::MAX_BRACE_ALTERNATIVES
        );
        assert!(Pattern::compile(&within, options).is_ok());

        let beyond = "{a,b}".repeat(13);
        let error = Pattern::compile(&beyond, options).unwrap_err();
        assert_eq!(error.message(), "too many brace alternatives");
        assert_eq!(error.offset(), 0);

        // The reproductions from the issue: 9^5 and 9^10 alternatives.
        for groups in [5, 10] {
            let pattern = "{,,,,,,,,}".repeat(groups);
            let error = Pattern::compile(&pattern, options).unwrap_err();
            assert_eq!(error.message(), "too many brace alternatives");
        }

        // The offset locates the expansion, not the start of the pattern.
        let error = Pattern::compile(format!("src/{beyond}"), options).unwrap_err();
        assert_eq!(error.offset(), 4);

        // Without brace expansion the same pattern is ordinary text.
        assert!(Pattern::compile(&beyond, PatternOptions::default()).is_ok());
    }

    #[test]
    fn brace_expansion_survives_many_small_groups() {
        // One alternative per group keeps the count at 1 however deep it goes,
        // so only the expansion's own recursion used to fail here. The work
        // budget now stops such a pattern long before the depth would matter,
        // so this stays inside it.
        let options = PatternOptions::default().braces(true);
        let pattern = "{a}".repeat(2_000);
        let compiled = Pattern::compile(&pattern, options).unwrap();
        assert!(compiled.is_match("a".repeat(2_000)));
    }

    #[test]
    fn brace_expansion_summary_matches_the_materialization_tree() {
        for (pattern, expected) in [
            ("{a,b}", (5, 2, 1, 2, 3, 7)),
            ("{a,b}{c,d}", (10, 4, 2, 8, 7, 30)),
            ("x{a,b}y", (7, 2, 3, 6, 3, 13)),
            ("{a,{b,c}}", (9, 3, 1, 3, 5, 17)),
            ("x{a", (3, 1, 3, 3, 1, 3)),
        ] {
            let summary = super::brace_expansion_summary(pattern.as_bytes(), true, 0)
                .expect("small expansion has a summary");
            assert_eq!(
                (
                    summary.source_length,
                    summary.alternatives,
                    summary.minimum_length,
                    summary.final_length_sum,
                    summary.nodes,
                    summary.written,
                ),
                expected,
                "{pattern}"
            );
        }
    }

    #[test]
    fn brace_expansion_stops_at_the_work_budget() {
        let options = PatternOptions::default().braces(true);

        // One alternative per group: the count budget never fires, because the
        // expansion is a single alternative however many groups produced it.
        // Rewriting the whole pattern per group is what costs, and it is
        // quadratic in the length: `{a}` x 200,000 took 11.8 s.
        let degenerate = "{a}".repeat(200_000);
        let error = Pattern::compile(&degenerate, options).unwrap_err();
        assert_eq!(error.message(), "brace expansion is too large");
        assert_eq!(error.offset(), 0);

        // Inside the alternative budget but not inside the work budget: 4096
        // alternatives of a 10 KB pattern is 80 MB of rewriting.
        let wide = format!("{}{}", "x".repeat(10_000), "{a,b}".repeat(12));
        let error = Pattern::compile(&wide, options).unwrap_err();
        assert_eq!(error.message(), "brace expansion is too large");
        assert_eq!(error.offset(), 10_000);

        // The IR lower bound is also over budget, but expansion has always
        // been the first failing phase and keeps its established error.
        let overlapping = format!("{}{}", "?".repeat(10_000), "{a,b}".repeat(12));
        let error = Pattern::compile(&overlapping, options).unwrap_err();
        assert_eq!(error.message(), "brace expansion is too large");
        assert_eq!(error.offset(), 10_000);

        // The same shape at a realistic length still compiles, and so does the
        // largest alternative count the other budget admits.
        let narrow = format!("{}{}", "x".repeat(1_000), "{a,b}".repeat(12));
        assert!(Pattern::compile(&narrow, options).is_ok());
        assert!(Pattern::compile("{a,b}".repeat(12), options).is_ok());

        // Without brace expansion the same bytes are ordinary text.
        assert!(Pattern::compile(&degenerate, PatternOptions::default()).is_ok());
    }

    #[test]
    fn compiled_size_is_budgeted_across_every_alternative() {
        let options = PatternOptions::default().braces(true).extglob(true);

        // 4096 alternatives hundreds of bytes long: inside the alternative
        // and expansion-byte budgets, but millions of compiled units.
        let extglob = format!("@(a|b){}{}", "x".repeat(300), "{a,b}".repeat(12));
        let error = Pattern::compile(&extglob, options).unwrap_err();
        assert_eq!(error.message(), "pattern compiles to too much");

        // The same dimension without extglob: one token per wildcard byte.
        let wildcards = format!("{}{}", "?".repeat(300), "{a,b}".repeat(12));
        let error =
            Pattern::compile(&wildcards, PatternOptions::default().braces(true)).unwrap_err();
        assert_eq!(error.message(), "pattern compiles to too much");
        assert!(
            extglob.len() < 384 && wildcards.len() < 384,
            "the budget rejection must remain reachable below the matcher fuzz ceiling"
        );

        // A literal run is one token however long, so the same alternative
        // count over the same number of bytes compiles.
        let literals = format!("{}{}", "x".repeat(1_000), "{a,b}".repeat(12));
        assert!(Pattern::compile(&literals, PatternOptions::default().braces(true)).is_ok());

        // The early lower bound must not hide a parser error that exact
        // alternative-by-alternative compilation would report first.
        let invalid_class = format!("{}[{}", "?".repeat(300), "{a,b}".repeat(12));
        assert_eq!(
            Pattern::compile(&invalid_class, PatternOptions::default().braces(true))
                .unwrap_err()
                .message(),
            "unclosed character class"
        );
        let stable_class = format!("{}[a-z]{}", "?".repeat(300), "{a,b}".repeat(12));
        assert_eq!(
            Pattern::compile(&stable_class, PatternOptions::default().braces(true))
                .unwrap_err()
                .message(),
            "pattern compiles to too much"
        );

        // The budget is a compile-time ceiling, not a syntax rule.
        assert!(Pattern::compile(&wildcards, PatternOptions::default()).is_ok());

        // A separator before a wildcard makes the pattern carry a second
        // compiled copy for the path-filter reading, which costs the budget a
        // second time (512 × 1,101 = 563,712 fits; 2 × 512 × 1,102 = 1,128,448
        // exceeds `MAX_COMPILED_IR_UNITS`). The same pattern without one
        // compiles.
        let braces = PatternOptions::default().braces(true);
        let single = format!("{}{}", "{a,b}".repeat(9), "?".repeat(1_100));
        let doubled = format!("{}/{}", "{a,b}".repeat(9), "?".repeat(1_100));
        assert!(Pattern::compile(&single, braces).is_ok());
        assert_eq!(
            Pattern::compile(&doubled, braces).unwrap_err().message(),
            "pattern compiles to too much"
        );
    }

    #[test]
    fn compiled_size_budget_leaves_realistic_patterns_alone() {
        let options = PatternOptions::default()
            .braces(true)
            .extglob(true)
            .recursive_double_star(true);
        for pattern in [
            "src/**/+(main|lib).{rs,toml}",
            "src/**/*.{js,jsx,ts,tsx,mjs,cjs}",
            "src/{a,b}/{c,d}/*.{e,f}",
            "**/@(foo|bar)/**/*.ts",
        ] {
            assert!(
                Pattern::compile(pattern, options).is_ok(),
                "{pattern} must stay inside the compiled-size ceiling"
            );
        }
    }

    #[test]
    fn wide_sweep_patterns_at_one_mebibyte_are_bounded() {
        // Wide sweeps charge their bit rows to the same compiled-program
        // budget as the narrow engine. This is a resource boundary rather
        // than a syntax restriction: 0.8.x accepted this shape, but it could
        // construct an unboundedly large machine.
        let pattern = format!("{}*b", "?".repeat(1 << 20));
        let error = Pattern::compile(&pattern, PatternOptions::default()).unwrap_err();
        assert_eq!(error.message(), "pattern compiles to too much");
    }

    #[test]
    fn the_three_compile_budgets_are_reported_apart() {
        // Each answers a different question — how many alternatives, how much
        // text they add up to, and how much program that text becomes — so a
        // caller can tell which ceiling it hit.
        let options = PatternOptions::default().braces(true);
        assert_eq!(
            Pattern::compile("{a,b}".repeat(13), options)
                .unwrap_err()
                .message(),
            "too many brace alternatives"
        );
        assert_eq!(
            Pattern::compile("{a}".repeat(200_000), options)
                .unwrap_err()
                .message(),
            "brace expansion is too large"
        );
        assert_eq!(
            Pattern::compile(
                format!("{}{}", "?".repeat(1_000), "{a,b}".repeat(12)),
                options
            )
            .unwrap_err()
            .message(),
            "pattern compiles to too much"
        );
    }

    #[test]
    fn compiled_unit_sizes_match_what_the_budget_documents() {
        // The ceiling is expressed in units, and its documentation converts
        // that to memory. If a unit grows, the documented ceiling silently
        // becomes a larger one.
        assert_eq!(
            size_of::<Token>(),
            32,
            "Token grew; the budget doc is stale"
        );
        assert_eq!(
            size_of::<super::ExtglobStep>(),
            40,
            "ExtglobStep grew; the budget doc is stale"
        );
        assert!(
            size_of::<super::BuildingAffixNode>()
                + size_of::<(u8, usize)>()
                + size_of::<super::AffixNode>()
                + size_of::<super::AffixEdge>()
                <= 3 * size_of::<Token>(),
            "affix-trie construction outgrew its three-unit charge"
        );
    }

    #[test]
    fn brace_expansion_budgets_are_reported_apart() {
        // The two limits answer different questions and must stay
        // distinguishable: how many alternatives, and how much copying.
        let options = PatternOptions::default().braces(true);
        let many = Pattern::compile("{a,b}".repeat(13), options).unwrap_err();
        assert_eq!(many.message(), "too many brace alternatives");
        let large = Pattern::compile("{a}".repeat(200_000), options).unwrap_err();
        assert_eq!(large.message(), "brace expansion is too large");
    }

    #[test]
    fn escapes_and_case_folding_are_opt_in_and_byte_oriented() {
        assert!(compile("\\*.rs").is_match("*.rs"));
        assert!(!compile("\\*.rs").is_match("main.rs"));
        assert!(
            Pattern::compile(
                "README.md",
                PatternOptions::default().case_insensitive(true)
            )
            .unwrap()
            .is_match("readme.MD")
        );
    }

    #[test]
    fn extglobs_support_alternation_repetition_and_negation() {
        let options = PatternOptions::default().extglob(true);
        assert!(
            Pattern::compile("@(foo|bar)", options)
                .unwrap()
                .is_match("foo")
        );
        assert!(
            Pattern::compile("file?(.bak).txt", options)
                .unwrap()
                .is_match("file.txt")
        );
        assert!(
            Pattern::compile("a*(X)b", options)
                .unwrap()
                .is_match("aXXXb")
        );
        assert!(
            Pattern::compile("+(a|b)", options)
                .unwrap()
                .is_match("abba")
        );
        assert!(
            Pattern::compile("*.!(js)", options)
                .unwrap()
                .is_match("file.txt")
        );
        assert!(
            !Pattern::compile("*.!(js)", options)
                .unwrap()
                .is_match("file.js")
        );
        assert!(
            !Pattern::compile("@(foo|bar)", PatternOptions::default())
                .unwrap()
                .is_match("foo")
        );
    }

    #[test]
    fn sequential_extglob_repetitions_share_failed_suffixes() {
        let pattern = Pattern::compile("+(a)+(a)+(a)+(a)", PatternOptions::default().extglob(true))
            .expect("repeating extglobs compile");

        // Every partition of this run reaches one of the same group/path
        // suffixes. The trailing byte makes all of them fail, exercising the
        // compiled NFA's shared frontier without a wall-clock assertion.
        let mut non_matching = "a".repeat(400);
        non_matching.push('x');
        assert!(!pattern.is_match(&non_matching));

        // Adjacent `+()` groups remain independently non-empty.
        assert!(pattern.is_match("aaaa"));
        assert!(!pattern.is_match("aaa"));

        // A long single repetition stays within one explicit work frame.
        let single = Pattern::compile("+(a)", PatternOptions::default().extglob(true))
            .expect("single repeating extglob compiles");
        assert!(single.is_match("a".repeat(5_000)));
    }

    #[test]
    fn positive_extglob_nfa_reuses_thread_local_scratch() {
        let pattern = Pattern::compile(
            "@(src|tests)/lib.rs",
            PatternOptions::default().extglob(true),
        )
        .expect("positive extglob compiles");
        assert!(pattern.is_match("src/lib.rs"));
        assert!(!pattern.is_match("vendor/lib.rs"));

        let warmed = positive_extglob_scratch_capacities();
        assert!(
            warmed.0 > 0 && warmed.1 > 0 && warmed.2 > 0 && warmed.3 > 0,
            "the NFA populated every reusable buffer: {warmed:?}"
        );
        for _ in 0..1_000 {
            assert!(pattern.is_match("tests/lib.rs"));
            assert!(!pattern.is_match("vendor/lib.rs"));
        }
        assert_eq!(
            positive_extglob_scratch_capacities(),
            warmed,
            "steady-state matches must not grow the reusable buffers"
        );
    }

    #[test]
    fn positive_extglob_nfa_releases_oversized_scratch() {
        let mut scratch = super::PositiveExtglobMatchScratch::default();
        scratch.current.reserve(super::RETAINED_SCRATCH_WORDS + 1);
        scratch.next.reserve(super::RETAINED_SCRATCH_WORDS + 1);
        scratch
            .closure
            .pending
            .reserve(super::RETAINED_SCRATCH_WORDS + 1);
        let retained_seen_bits = super::RETAINED_SCRATCH_WORDS * u64::BITS as usize;
        scratch.closure.seen.reserve(retained_seen_bits + 1);

        scratch.release();

        assert!(scratch.current.capacity() <= super::RETAINED_SCRATCH_WORDS);
        assert!(scratch.next.capacity() <= super::RETAINED_SCRATCH_WORDS);
        assert!(scratch.closure.pending.capacity() <= super::RETAINED_SCRATCH_WORDS);
        assert!(scratch.closure.seen.capacity() <= retained_seen_bits);
    }

    #[test]
    fn positive_extglob_nfa_has_a_reentrant_scratch_fallback() {
        let pattern = Pattern::compile("@(src|tests)", PatternOptions::default().extglob(true))
            .expect("positive extglob compiles");

        super::POSITIVE_EXTGLOB_SCRATCH.with(|cell| {
            let _borrow = cell.borrow_mut();
            assert!(pattern.is_match("src"));
            assert!(!pattern.is_match("vendor"));
        });
    }

    #[test]
    fn negated_extglob_interpreter_keeps_sparse_memo_and_releases_it() {
        let pattern = Pattern::compile(
            "!(z)",
            PatternOptions::default().extglob(true).match_hidden(true),
        )
        .expect("negated extglob compiles");
        let program = pattern.alternatives[0]
            .extglob
            .as_ref()
            .expect("the pattern carries an extglob program");
        assert!(program.positive_nfa.is_none());
        assert!(pattern.is_match("x"));
        let (pages, states) = extglob_failed_stats();
        assert!(
            pages > 0 && states > 0,
            "the fallback memo records reached states"
        );

        let capacities = extglob_scratch_capacities();
        assert!(
            capacities.1 <= super::RETAINED_SCRATCH_WORDS,
            "the failed-state bitset must stay within its retained cap: {capacities:?}"
        );
        assert_eq!(
            extglob_failed_len(),
            0,
            "the retained memo is logically empty"
        );
    }

    #[test]
    fn outer_star_deduplicates_negated_group_continuations_before_queueing() {
        let pattern = Pattern::compile(
            "*!(a)b",
            PatternOptions::default().extglob(true).match_hidden(true),
        )
        .expect("extglob compiles");
        assert!(
            pattern.alternatives[0]
                .extglob
                .as_ref()
                .is_some_and(|program| program.positive_nfa.is_none()),
            "the regression exercises the retained interpreter"
        );
        let candidate = vec![b'a'; 4_096];
        assert!(!pattern.is_match(&candidate));
        assert!(
            extglob_pending_peak() <= candidate.len() + 1,
            "duplicate retries must not make the worklist quadratic"
        );
    }

    #[test]
    fn extglob_group_entry_is_memoized_across_star_backtracking() {
        let candidate = vec![b'a'; 256];
        let mut matching_candidate = candidate.clone();
        matching_candidate.push(b'y');

        for match_hidden in [false, true] {
            let options = PatternOptions::default()
                .extglob(true)
                .recursive_double_star(true)
                .match_hidden(match_hidden);

            for source in [
                "!(z)*!(z)y",
                "*+(a)*+(a)y",
                "*a*!(z)y",
                "**/!(z)y",
                "**/a!(z)y",
            ] {
                let pattern = Pattern::compile(source, options).expect("extglob compiles");
                let program = pattern.alternatives[0]
                    .extglob
                    .as_ref()
                    .expect("extglob program is present");
                assert!(
                    program.positive_nfa.is_none(),
                    "{source} exercises the retained interpreter"
                );
                let max_group_expansions = program.groups.len() * (candidate.len() + 1);

                for entry_point in ["is_match", "is_match_path", "is_match_glob_path"] {
                    let matched = match entry_point {
                        "is_match" => pattern.is_match(&candidate),
                        "is_match_path" => pattern.is_match_path(&candidate),
                        "is_match_glob_path" => pattern.is_match_glob_path(&candidate),
                        _ => unreachable!(),
                    };
                    assert!(
                        !matched,
                        "{source} ({entry_point}) should reject non-matching candidate"
                    );
                    let expansions = extglob_group_expansions();
                    assert!(
                        expansions <= max_group_expansions,
                        "{source} ({entry_point}) expanded groups {expansions} times for {} bytes (bound {max_group_expansions})",
                        candidate.len()
                    );
                }
                assert!(
                    pattern.engines_agree(&candidate),
                    "{source} engines agree on non-matching candidate (match_hidden={match_hidden})"
                );
                assert!(pattern.is_match(&matching_candidate));
                assert!(
                    pattern.engines_agree(&matching_candidate),
                    "{source} engines agree on matching candidate (match_hidden={match_hidden})"
                );
            }

            // Also test `**/!(z)y` and `**/a!(z)y` on a deep multi-component
            // non-matching path so the recursive prefix queues and crosses
            // separators into the group.
            let deep_non_matching = "a/".repeat(64);
            for source in ["**/!(z)y", "**/a!(z)y"] {
                let pattern = Pattern::compile(source, options).expect("extglob compiles");
                let program = pattern.alternatives[0]
                    .extglob
                    .as_ref()
                    .expect("extglob program is present");
                let max_group_expansions = program.groups.len() * (deep_non_matching.len() + 1);
                assert!(!pattern.is_match(&deep_non_matching));
                let expansions = extglob_group_expansions();
                assert!(
                    expansions <= max_group_expansions,
                    "{source} expanded groups {expansions} times on deep path (bound {max_group_expansions})"
                );
                assert!(pattern.engines_agree(&deep_non_matching));
            }

            // Verify concrete verdicts and engine agreement when `group.start`
            // is queued directly via adjacent groups, globstar skips, or
            // `through_separator` resumes.
            for (source, sample, expected) in [
                ("!(z)!(z)y", "abcy", true),
                ("!(z)!(z)y", "x", false),
                ("!(z)*!(z)y", "zy", true),
                ("!(z)*!(z)y", "z", false),
                ("*+(a)*+(a)y", "xaay", true),
                ("*+(a)*+(a)y", "xay", false),
                ("file-+(a)*+(a)y", "file-aay", true),
                ("file-+(a)*+(a)y", "file-ay", false),
                ("**/!(z)y", "xy", true),
                ("**/!(z)y", "a/xy", true),
                ("**/!(z)y", "zy", false),
                ("**/a!(z)y", "axy", true),
                ("**/a!(z)y", "b/axy", true),
                ("**/a!(z)y", "azy", false),
                ("@(a/**)/!(z)y", "a/xy", true),
                ("@(a/**)/!(z)y", "a/zy", false),
            ] {
                let pattern = Pattern::compile(source, options).expect("extglob compiles");
                assert_eq!(
                    pattern.is_match(sample),
                    expected,
                    "{source} verdict on {sample:?} (match_hidden={match_hidden})"
                );
                assert!(
                    pattern.engines_agree(sample),
                    "{source} engines agree on {sample:?} (match_hidden={match_hidden})"
                );
            }
        }

        // Mixed crossing + separator-bound component-local stars can activate
        // `crossing_star` under `is_match_path`, so they select
        // `MonotonicOrCrossingStar`: zero group-entry memo pages when
        // `crossing_star` never activates (such as `is_match`), and paged
        // deduplication whenever `crossing_star` does activate.
        let hidden_crossing_positive = Pattern::compile(
            "*/.hidden/*@(jpg|png)",
            PatternOptions::default().extglob(true),
        )
        .expect("crossing-star positive extglob compiles");
        assert_eq!(
            hidden_crossing_positive.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program is present")
                .groups[0]
                .entry_memo,
            super::GroupEntryMemo::MonotonicOrCrossingStar
        );
        let long_hidden_nonmatch = format!("a/.hidden/{}", "x".repeat(65_536));
        assert!(!hidden_crossing_positive.is_match(&long_hidden_nonmatch));
        assert_eq!(
            extglob_failed_stats(),
            (1, 1),
            "crossing-star pattern under is_match must stay on monotonic dedup with 1 memo page"
        );

        let hidden_crossing_negated =
            Pattern::compile("*/.hidden/*!(z)y", PatternOptions::default().extglob(true))
                .expect("crossing-star negated extglob compiles");
        assert!(!hidden_crossing_negated.is_match("x".repeat(65_536)));
        assert_eq!(extglob_failed_stats(), (1, 1));

        // Even under `is_match_path` where `crossing_star` is saved, staying
        // inside one component without triggering `crossing_star.take()` must
        // keep monotonic stack deduplication with 1 memo page / 1 memo state.
        let foo_crossing_positive =
            Pattern::compile("*/foo/*@(jpg|png)", PatternOptions::default().extglob(true))
                .expect("crossing-star foo extglob compiles");
        assert_eq!(
            foo_crossing_positive.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program is present")
                .groups[0]
                .entry_memo,
            super::GroupEntryMemo::MonotonicOrCrossingStar
        );
        let foo_single_component_nonmatch = format!("a/foo/{}", "x".repeat(8_192));
        assert!(!foo_crossing_positive.is_match_path(&foo_single_component_nonmatch));
        assert_eq!(
            extglob_failed_stats(),
            (1, 1),
            "saving crossing_star without rewinding must not allocate group-entry memo pages"
        );

        for (source, match_hidden, nonmatch, match_sample) in [
            (
                "*/.hidden/*!(z)y",
                false,
                "a/b/.hidden/c/d",
                "a/b/.hidden/xy",
            ),
            (
                "*/.hidden/*!(z)y",
                true,
                "a/.hidden/b/.hidden/c",
                "a/.hidden/b/.hidden/xy",
            ),
            ("*a/*!(z)y", false, "aa/a/aa/a/x", "aa/a/aa/a/xy"),
        ] {
            let pattern = Pattern::compile(
                source,
                PatternOptions::default()
                    .extglob(true)
                    .match_hidden(match_hidden),
            )
            .expect("crossing-star extglob compiles");
            let program = pattern.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program is present");
            assert_eq!(
                program.groups[0].entry_memo,
                super::GroupEntryMemo::MonotonicOrCrossingStar,
                "{source} must use MonotonicOrCrossingStar when crossing_star can activate"
            );
            assert!(!pattern.is_match_path(nonmatch));
            let expansions = extglob_group_expansions();
            assert!(
                expansions <= program.groups.len() * (nonmatch.len() + 1),
                "{source} expanded groups {expansions} times on {nonmatch:?}"
            );
            assert!(pattern.is_match_path(match_sample));
            assert!(pattern.engines_agree(nonmatch));
            assert!(pattern.engines_agree(match_sample));
        }

        // `*/file*@(jpg|png)` has a second star behind `e` (not `/`), so both
        // stars share locality in every mode and stay on pure `Monotonic` with
        // 1 memo page / 1 memo state across a >4 KiB (cross-page) nonmatch in
        // all 3 modes.
        let same_locality_stars =
            Pattern::compile("*/file*@(jpg|png)", PatternOptions::default().extglob(true))
                .expect("same-locality multi-star extglob compiles");
        assert_eq!(
            same_locality_stars.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program is present")
                .groups[0]
                .entry_memo,
            super::GroupEntryMemo::Monotonic
        );
        let dir_file_long = format!("dir/file{}", "x".repeat(4_097));
        assert!(!same_locality_stars.is_match(&dir_file_long));
        assert_eq!(extglob_failed_stats(), (1, 1));
        assert!(!same_locality_stars.is_match_path(&dir_file_long));
        assert_eq!(extglob_failed_stats(), (1, 1));
        assert!(!same_locality_stars.is_match_glob_path(&dir_file_long));
        assert_eq!(extglob_failed_stats(), (1, 1));
        assert!(same_locality_stars.is_match("dir/file1.jpg"));
        assert!(same_locality_stars.is_match_path("dir/file1.jpg"));
        assert!(same_locality_stars.is_match_glob_path("dir/file1.jpg"));

        // One or more ordinary leading stars before the first group (or after a
        // single-arrival uniform fixed-width `ExactlyOne` group) deduplicate via
        // a stack slot rather than allocating paged memo tables across long
        // candidates.
        for (source, nonmatch, match_sample, expected_pages, expected_states) in [
            ("*@(jpg|png)", "a".repeat(64 * 1024), "photo.jpg", 1, 1),
            (
                "*foo*@(jpg|png)",
                format!("foo{}", "x".repeat(8_192)),
                "afoob.png",
                1,
                1,
            ),
            (
                "@(src|lib)*@(jpg|png)",
                format!("src{}", "x".repeat(8_192)),
                "lib/a.jpg",
                2,
                2,
            ),
        ] {
            let pattern = Pattern::compile(source, PatternOptions::default().extglob(true))
                .expect("monotonic-star extglob compiles");
            let program = pattern.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program is present");
            assert_eq!(
                program
                    .groups
                    .last()
                    .expect("at least one group")
                    .entry_memo,
                super::GroupEntryMemo::Monotonic,
                "{source} should use monotonic stack deduplication on its trailing group"
            );
            assert!(!pattern.is_match(&nonmatch));
            let (pages, states) = extglob_failed_stats();
            assert_eq!(
                (pages, states),
                (expected_pages, expected_states),
                "{source} must not allocate group-entry memo pages on long nonmatch"
            );
            assert!(pattern.is_match(match_sample));
        }

        // A component-local `*` that clears `has_star` at `/` must keep group
        // deduplication active on the retry so the group still expands once,
        // without allocating any group-entry memo page.
        let star_slash = Pattern::compile("*/!(z)y", PatternOptions::default().extglob(true))
            .expect("component-local star before negated group compiles");
        assert_eq!(
            star_slash.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program is present")
                .groups[0]
                .entry_memo,
            super::GroupEntryMemo::Monotonic
        );
        assert!(!star_slash.is_match_glob_path("a/x"));
        assert_eq!(
            extglob_group_expansions(),
            1,
            "clearing `has_star` at `/` must not re-expand the following group"
        );
        let (pages, _states) = extglob_failed_stats();
        assert_eq!(
            pages, 2,
            "monotonic star retry should only materialize entry and rest memo pages"
        );
        assert!(star_slash.is_match_glob_path("a/xy"));
        assert!(star_slash.engines_agree("a/x"));
        assert!(star_slash.engines_agree("a/xy"));

        // Verify post-group star, recursive-prefix, and `through_separator`
        // groups select `GroupEntryMemo::Paged`.
        let paged_options = PatternOptions::default()
            .extglob(true)
            .recursive_double_star(true);
        for source in ["!(z)*!(z)y", "*+(a)*+(a)y", "**/!(z)y", "@(a/**)/!(z)y"] {
            let pattern = Pattern::compile(source, paged_options).expect("paged extglob compiles");
            let program = pattern.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program is present");
            assert_eq!(
                program
                    .groups
                    .last()
                    .expect("at least one group")
                    .entry_memo,
                super::GroupEntryMemo::Paged,
                "{source} should require paged entry memoization on its trailing group"
            );
        }

        // Deterministic fixed-width segments before or between groups have only
        // one way to reach each `group.start`, so matching them must not
        // materialize extra group-entry memo pages.
        for (source, matching, non_matching, expected_pages) in [
            ("file-!(draft).txt", "file-abc.txt", "file-draft.txt", 2),
            (
                "file-@(foo|bar)-!(draft).txt",
                "file-foo-main.txt",
                "file-foo-draft.txt",
                3,
            ),
            (
                "@(foo)-@(bar)-@(xyz)-!(draft)",
                "foo-bar-xyz-main",
                "foo-bar-xyz-draft",
                5,
            ),
        ] {
            let pattern = Pattern::compile(source, PatternOptions::default().extglob(true))
                .expect("deterministic-segment extglob compiles");
            let program = pattern.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program is present");
            assert!(
                program
                    .groups
                    .iter()
                    .all(|group| group.entry_memo == super::GroupEntryMemo::None),
                "{source} should not mark deterministic groups for entry memoization"
            );
            assert!(pattern.is_match(matching));
            let (pages, _states) = extglob_failed_stats();
            assert_eq!(
                pages, expected_pages,
                "{source} should only materialize entry and group-rest memo pages"
            );
            assert!(!pattern.is_match(non_matching));
            assert!(pattern.engines_agree(matching));
            assert!(pattern.engines_agree(non_matching));
        }
    }

    #[test]
    fn retained_extglob_interpreter_reuses_group_scratch() {
        let options = PatternOptions::default().extglob(true).match_hidden(true);
        let outer_star = Pattern::compile("*@(a)b", options).expect("extglob compiles");
        let negated = Pattern::compile("!(z)y", options).expect("extglob compiles");
        let repeated = Pattern::compile("*+(a*|b)c", options).expect("extglob compiles");
        let wide_literal = "a".repeat(70);
        let wide_negated = Pattern::compile(format!("*!({wide_literal}*)y"), options)
            .expect("wide extglob compiles");

        for pattern in [&outer_star, &negated, &repeated, &wide_negated] {
            assert!(
                pattern.alternatives[0]
                    .extglob
                    .as_ref()
                    .is_some_and(|program| program.positive_nfa.is_none()),
                "the regression exercises the retained interpreter"
            );
        }
        assert!(outer_star.is_match("aab"));
        assert!(negated.is_match("xy"));
        assert!(repeated.is_match("aaabc"));
        let wide_candidate = format!("{}y", "x".repeat(128));
        assert!(wide_negated.is_match(&wide_candidate));

        let warmed = extglob_scratch_capacities();
        assert!(
            warmed.3 > 0 && warmed.5 > 0 && warmed.6 > 0 && warmed.7 > 0,
            "group matching populated its retained scratch: {warmed:?}"
        );
        for _ in 0..1_000 {
            assert!(outer_star.is_match("aab"));
            assert!(negated.is_match("xy"));
            assert!(repeated.is_match("aaabc"));
            assert!(wide_negated.is_match(&wide_candidate));
        }
        assert_eq!(
            extglob_scratch_capacities(),
            warmed,
            "steady-state retained extglob matches must not grow their scratch"
        );
    }

    #[test]
    fn extglob_alternatives_keep_the_enclosing_component_context() {
        let options = PatternOptions::default().extglob(true);
        let star = Pattern::compile("a@(*)", options).expect("extglob compiles");
        let question = Pattern::compile("a@(?b)", options).expect("extglob compiles");
        assert!(star.is_match("a.b"));
        assert!(question.is_match("a.b"));

        let path_group = Pattern::compile("dir/a@(*)", options).expect("extglob compiles");
        assert!(path_group.is_match_path("dir/a.b"));
        assert!(path_group.is_match_glob_path("dir/a.b"));

        let at_start = Pattern::compile("@(*)", options).expect("extglob compiles");
        assert!(!at_start.is_match(".b"));
        assert!(!at_start.is_match_path(".b"));
        assert!(!at_start.is_match_glob_path(".b"));

        let after_separator = Pattern::compile("dir/@(*)", options).expect("extglob compiles");
        assert!(!after_separator.is_match_path("dir/.b"));
        assert!(!after_separator.is_match_glob_path("dir/.b"));
    }

    #[test]
    fn extglob_zero_width_forms_honor_leading_period_policy() {
        let options = PatternOptions::default().extglob(true);
        let optional = Pattern::compile("?(a|b).c", options).expect("optional extglob compiles");
        assert!(!optional.is_match(".c"));
        assert!(optional.is_match("a.c"));

        let repeating = Pattern::compile("*(ab).c", options).expect("repeating extglob compiles");
        assert!(!repeating.is_match(".c"));
        assert!(repeating.is_match("ab.c"));

        let outer_star = Pattern::compile("*.c@(x)", options).expect("outer star compiles");
        assert!(!outer_star.is_match(".cx"));

        let group_star = Pattern::compile("x@(*.c)", options).expect("group star compiles");
        assert!(!group_star.is_match("xvisible/.c"));

        let hidden = Pattern::compile("?(a|b).c", options.match_hidden(true))
            .expect("period-enabled optional extglob compiles");
        assert!(hidden.is_match(".c"));
        assert!(
            Pattern::compile("*.c@(x)", options.match_hidden(true))
                .expect("period-enabled outer star compiles")
                .is_match(".cx")
        );
        assert!(
            Pattern::compile("x@(*.c)", options.match_hidden(true))
                .expect("period-enabled group star compiles")
                .is_match("xvisible/.c")
        );
    }

    #[test]
    fn extglob_recursive_prefix_exemption_follows_even_star_runs() {
        let options = PatternOptions::default()
            .extglob(true)
            .recursive_double_star(true);

        let recursive = Pattern::compile("**/@(foo)", options).expect("extglob compiles");
        assert!(matches!(
            recursive.alternatives[0]
                .extglob
                .as_ref()
                .expect("the pattern carries an extglob program")
                .steps[0],
            ExtglobStep::Star {
                next: 2,
                blocks_leading_period: false,
                ..
            }
        ));

        let ordinary = Pattern::compile("***/@(foo)", options).expect("extglob compiles");
        assert!(matches!(
            ordinary.alternatives[0]
                .extglob
                .as_ref()
                .expect("the pattern carries an extglob program")
                .steps[0],
            ExtglobStep::Star {
                next: 3,
                blocks_leading_period: true,
                ..
            }
        ));
        assert!(!ordinary.is_match(".hidden/foo"));

        let longer = Pattern::compile("****/@(.h)", options).expect("extglob compiles");
        assert!(matches!(
            longer.alternatives[0]
                .extglob
                .as_ref()
                .expect("the pattern carries an extglob program")
                .steps[0],
            ExtglobStep::Star {
                next: 4,
                blocks_leading_period: false,
                ..
            }
        ));
        assert!(longer.is_match(".h"));
    }

    #[test]
    fn extglob_literal_dot_alternatives_honor_leading_period_policy() {
        let options = PatternOptions::default().extglob(true);
        let matcher =
            Pattern::compile("@(.gitignore|.npmignore)", options).expect("extglob compiles");
        assert!(matcher.is_match(".gitignore"));
        assert!(matcher.is_match(".npmignore"));
        assert!(!matcher.is_match(".env"));

        // The exemption is for explicit dots only: a wildcard alternative
        // still cannot select a hidden path with the default options.
        let wildcard = Pattern::compile("@(*|visible)", options).expect("extglob compiles");
        assert!(!wildcard.is_match(".hidden"));

        let mixed = Pattern::compile("@(.gitignore|*)", options).expect("extglob compiles");
        assert!(mixed.is_match(".gitignore"));
        assert!(!mixed.is_match(".hidden"));

        // A negated group is itself wildcard-like: naming one hidden path as
        // the exception must not opt every other hidden path into matching.
        let negated = Pattern::compile("!(.gitignore)", options).expect("extglob compiles");
        assert!(!negated.is_match(".hidden"));
        let hidden = Pattern::compile("!(.gitignore)", options.match_hidden(true))
            .expect("period-enabled extglob compiles");
        assert!(hidden.is_match(".hidden"));
        assert!(!hidden.is_match(".gitignore"));
    }

    #[test]
    fn negated_extglob_never_consumes_a_later_leading_period() {
        // Issue #394: a negation spans matcher text like `*`, so after
        // crossing a separator it must neither consume the period that starts
        // the next component nor stop right before it, under any entry point
        // or engine.
        let options = PatternOptions::default().extglob(true);
        let refused = [
            ("!(x)", "a/.env"),
            ("!(x)", ".env"),
            ("a!(x)", "a/.env"),
            ("src/!(x)", "src/a/.env"),
            ("!(.env)", "a/.git"),
            ("!(x)/b", "a/.c/b"),
            // Like `*.env`, the negation may not stop immediately before the
            // hidden component either, so the literal period cannot opt the
            // hidden name in implicitly.
            ("!(x).env", "a/.env"),
        ];
        for (pattern, path) in refused {
            let matcher = Pattern::compile(pattern, options).expect("extglob compiles");
            assert!(matcher.engines_agree(path), "{pattern} against {path}");
            assert!(!matcher.is_match(path), "{pattern} against {path}");
            assert!(!matcher.is_match_path(path), "{pattern} against {path}");
            assert!(
                !matcher.is_match_glob_path(path),
                "{pattern} against {path}"
            );

            let hidden = Pattern::compile(pattern, options.match_hidden(true))
                .expect("period-enabled extglob compiles");
            assert!(hidden.engines_agree(path), "{pattern} against {path}");
            assert!(
                hidden.is_match(path),
                "{pattern} against {path} with match_hidden"
            );
        }
        let star = Pattern::compile("*.env", options).expect("star compiles");
        assert!(!star.is_match("a/.env"));

        // A period the pattern writes at the start of its own component still
        // names the hidden component explicitly.
        let explicit = Pattern::compile("!(x)/.env", options).expect("extglob compiles");
        assert!(explicit.is_match("a/.env"));
        assert!(explicit.is_match_path("a/.env"));
        assert!(explicit.engines_agree("a/.env"));
        // A period inside a component is an ordinary byte.
        let inner = Pattern::compile("!(x)", options).expect("extglob compiles");
        assert!(inner.is_match("a/b.env"));
        assert!(inner.is_match_path("a/b.env"));
    }

    #[test]
    fn a_star_run_ending_in_a_recursive_prefix_leaves_a_leading_period_to_the_literal() {
        // Issue #442: a whole-component run of four stars compiles to a
        // recursive star in front of a `**/` prefix. The prefix is exempt from
        // the leading-period stop rule, so the run as a whole must be too, in
        // every engine: `****/.h` matches `.h` as `**/.h` does.
        let options = PatternOptions::default().recursive_double_star(true);
        let accepted = [
            ("****/.", "."),
            ("****/.h", ".h"),
            ("****/..", ".."),
            ("******/.h", ".h"),
            ("x/****/.h", "x/.h"),
            ("****/.h", "a/.h"),
        ];
        for (pattern, path) in accepted {
            let compiled = Pattern::compile(pattern, options).expect("pattern compiles");
            assert!(compiled.engines_agree(path), "{pattern} against {path}");
            assert!(compiled.is_match(path), "{pattern} against {path}");
            assert!(compiled.is_match_path(path), "{pattern} against {path}");
            assert!(
                compiled.is_match_glob_path(path),
                "{pattern} against {path}"
            );
        }
        // The exemption ends at the prefix: a wildcard behind it still may not
        // take the period, and an odd run keeps its ordinary last star.
        let refused = [("****/*", ".h"), ("****/?h", ".h"), ("*****/.h", ".h")];
        for (pattern, path) in refused {
            let compiled = Pattern::compile(pattern, options).expect("pattern compiles");
            assert!(compiled.engines_agree(path), "{pattern} against {path}");
            assert!(!compiled.is_match(path), "{pattern} against {path}");
            assert!(!compiled.is_match_path(path), "{pattern} against {path}");
            assert!(
                !compiled.is_match_glob_path(path),
                "{pattern} against {path}"
            );
        }
    }

    #[test]
    fn extglob_programs_are_compiled_only_where_the_syntax_is_present() {
        let options = PatternOptions::default().extglob(true);
        let with_group = Pattern::compile("@(a|b)c", options).expect("group compiles");
        assert!(with_group.alternatives[0].extglob.is_some());

        // The walker enables extglob for every traversal pattern, so a pattern
        // without the syntax must not carry a program — nor pay for a scan.
        let without_group = Pattern::compile("src/**/*.ts", options).expect("plain compiles");
        assert!(without_group.alternatives[0].extglob.is_none());

        // The option gates it: the same bytes are ordinary text without it.
        let disabled =
            Pattern::compile("@(a|b)c", PatternOptions::default()).expect("plain compiles");
        assert!(disabled.alternatives[0].extglob.is_none());
        assert!(disabled.is_match("@(a|b)c"));
    }

    #[test]
    fn extglob_alternatives_carry_a_fixed_width_only_when_they_have_one() {
        let options = PatternOptions::default().extglob(true);
        let pattern = Pattern::compile("@(foo|b?|c*)", options).expect("group compiles");
        let program = pattern.alternatives[0]
            .extglob
            .as_ref()
            .expect("the pattern carries a group");
        let widths: Vec<Option<usize>> = program.groups[0]
            .alternatives
            .iter()
            .map(|alternative| alternative.width)
            .collect();
        // `foo` is three bytes, `b?` is two, and a star makes `c*` vary.
        assert_eq!(widths, vec![Some(3), Some(2), None]);
    }

    #[test]
    fn extglobs_avoid_native_stack_recursion() {
        let options = PatternOptions::default().extglob(true);
        let pattern = Pattern::compile("+(a*)b", options).expect("repetition compiles");
        let program = pattern.alternatives[0]
            .extglob
            .as_ref()
            .expect("the pattern carries an extglob program");
        assert!(program.positive_nfa.is_some());

        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                assert!(!pattern.is_match("a".repeat(50_000)));
                let sequential = Pattern::compile("@(a)".repeat(2_000), options)
                    .expect("sequential groups compile");
                assert!(sequential.is_match("a".repeat(2_000)));

                // An outer star deliberately retains the compatible
                // interpreter. Its continuation work must be heap-backed too.
                let compatible = Pattern::compile(["*", &"@(a)".repeat(800)].concat(), options)
                    .expect("compatible extglob compiles");
                assert!(compatible.is_match("a".repeat(800)));
            })
            .expect("spawn a small-stack matcher worker")
            .join()
            .expect("positive extglobs must not recurse through the native stack");
    }

    #[test]
    fn positive_extglob_nfa_agrees_with_the_fallback_interpreter() {
        let candidates = byte_words(b"ab./", 5);
        for source in [
            "@(a|ab)b",
            "?(a|b)c",
            "*(a|bc)d",
            "+(a*|b?)c",
            "@(|a)b",
            "+(|a)b",
            "a@(b|[a-c])/?(.x|y)",
            r"x\@(a|b)@(c)",
            // A group's alternatives inherit its component position (#419); an
            // attached group keeps its stars ordinary.
            "x@(**)a",
        ] {
            for match_hidden in [false, true] {
                for case_insensitive in [false, true] {
                    let options = PatternOptions::default()
                        .extglob(true)
                        .recursive_double_star(true)
                        .match_hidden(match_hidden)
                        .case_insensitive(case_insensitive);
                    let compiled = Pattern::compile(source, options)
                        .expect("positive differential pattern compiles");
                    let mut interpreted = compiled.clone();
                    for alternative in interpreted.compiled_alternatives_mut() {
                        alternative
                            .extglob
                            .as_mut()
                            .expect("every differential pattern carries an extglob")
                            .positive_nfa = None;
                    }
                    for candidate in &candidates {
                        assert_eq!(
                            compiled.is_match(candidate),
                            interpreted.is_match(candidate),
                            "is_match diverges for {source:?} against {candidate:?} under {options:?}"
                        );
                        assert_eq!(
                            compiled.is_match_path(candidate),
                            interpreted.is_match_path(candidate),
                            "is_match_path diverges for {source:?} against {candidate:?} under {options:?}"
                        );
                        assert_eq!(
                            compiled.is_match_glob_path(candidate),
                            interpreted.is_match_glob_path(candidate),
                            "is_match_glob_path diverges for {source:?} against {candidate:?} under {options:?}"
                        );
                    }
                }
            }
        }
    }

    /// The two extglob engines agree on recursive stars inside a group,
    /// including the component-start rule of a `**/` alternative and the
    /// ordinary reading of an attached group's `**` (#419).
    #[test]
    fn positive_extglob_nfa_agrees_on_recursive_group_alternatives() {
        let candidates = byte_words(b"ab./", 5);
        for source in [
            "@(**/a|b)",
            "a/@(**|b)/a",
            "@(**)/a",
            "+(**/a)",
            "@(a/**)",
            "x@(**/a)",
            "@(**)x",
        ] {
            for match_hidden in [false, true] {
                let options = PatternOptions::default()
                    .extglob(true)
                    .recursive_double_star(true)
                    .match_hidden(match_hidden);
                let compiled = Pattern::compile(source, options).expect("pattern compiles");
                assert!(
                    compiled.alternatives[0]
                        .extglob
                        .as_ref()
                        .is_some_and(|program| program.positive_nfa.is_some()),
                    "{source} compiles to the positive NFA"
                );
                let mut interpreted = compiled.clone();
                for alternative in interpreted.compiled_alternatives_mut() {
                    if let Some(program) = alternative.extglob.as_mut() {
                        program.positive_nfa = None;
                    }
                }
                for candidate in &candidates {
                    let answers = |pattern: &Pattern| {
                        (
                            pattern.is_match(candidate),
                            pattern.is_match_path(candidate),
                            pattern.is_match_glob_path(candidate),
                        )
                    };
                    assert_eq!(
                        answers(&compiled),
                        answers(&interpreted),
                        "the engines diverge for {source:?} against {:?} under {options:?}",
                        String::from_utf8_lossy(candidate)
                    );
                }
            }
        }
    }

    #[test]
    fn outer_stars_honor_the_selected_path_component_policy() {
        let options = PatternOptions::default()
            .extglob(true)
            .recursive_double_star(true);
        let star_before_group = Pattern::compile("*@(a)b", options).expect("extglob compiles");
        assert!(
            star_before_group.alternatives[0]
                .extglob
                .as_ref()
                .is_some_and(|program| program.positive_nfa.is_none())
        );
        assert!(star_before_group.is_match("/ab"));
        assert!(star_before_group.is_match_path("/ab"));
        assert!(!star_before_group.is_match_glob_path("/ab"));

        let retained = Pattern::compile("a@(b|[a-c])*/?(.x|y)", options)
            .expect("backtracking extglob compiles");
        assert!(retained.is_match_path("aa//"));
        assert!(!retained.is_match("aa/./"));
    }

    #[test]
    fn last_star_in_a_run_opens_an_extglob_group() {
        let options = PatternOptions::default().extglob(true).match_hidden(true);

        for pattern in ["**(a)", "***(a)", "a**(b)"] {
            let compiled = Pattern::compile(pattern, options).expect("extglob compiles");
            assert!(
                compiled.alternatives[0]
                    .extglob
                    .as_ref()
                    .is_some_and(|program| !program.groups.is_empty()),
                "{pattern:?} must retain its final `*(` as an extglob opener"
            );
        }

        let leading = Pattern::compile("**(a)", options).expect("extglob compiles");
        for candidate in ["", "ab", "x", "(a)", "x(a)"] {
            assert!(
                leading.is_match(candidate),
                "the outer star may consume {candidate:?} before the zero-width group"
            );
        }

        let embedded = Pattern::compile("a**(b)", options).expect("extglob compiles");
        for candidate in ["a", "ab", "ax"] {
            assert!(
                embedded.is_match(candidate),
                "expected {candidate:?} to match"
            );
        }

        let disabled = Pattern::compile("**(a)", PatternOptions::default())
            .expect("literal parenthesized suffix compiles");
        assert!(disabled.is_match("x(a)"));
        assert!(!disabled.is_match("x"));
    }

    #[test]
    fn unclosed_extglob_after_a_star_run_keeps_its_literal_suffix() {
        let options = PatternOptions::default().extglob(true).match_hidden(true);

        let leading = Pattern::compile("**(a", options).expect("fallback pattern compiles");
        let leading_program = leading.alternatives[0]
            .extglob
            .as_ref()
            .expect("extglob interpreter is retained");
        assert!(matches!(
            leading_program.steps[0],
            ExtglobStep::Star { next: 2, .. }
        ));
        assert!(leading.is_match("x(a"));
        assert!(!leading.is_match("x"));

        let embedded = Pattern::compile("a**(b", options).expect("fallback pattern compiles");
        assert!(embedded.is_match("ax(b"));
        assert!(!embedded.is_match("ax"));
    }

    #[test]
    fn extglob_alternation_agrees_with_braces_over_exhaustive_byte_words() {
        // `@(x|y)` and `{x,y}` are two spellings of the same alternation, so
        // they must accept the same words. The extglob form is the one that
        // changed representation.
        let extglob = Pattern::compile(
            "@(a|bc)d",
            PatternOptions::default().extglob(true).match_hidden(true),
        )
        .expect("extglob alternation compiles");
        let braces = Pattern::compile(
            "{a,bc}d",
            PatternOptions::default().braces(true).match_hidden(true),
        )
        .expect("brace alternation compiles");
        for candidate in byte_words(b"abcd", 4) {
            assert_eq!(
                extglob.is_match(&candidate),
                braces.is_match(&candidate),
                "spellings disagree on {candidate:?}"
            );
        }
    }

    #[test]
    fn extglob_reads_an_escaped_group_opener_as_text() {
        // A backslash before a group opener is the corner where the walk can
        // land on the escaped byte itself, which is a group start again.
        let options = PatternOptions::default().extglob(true);
        let pattern = Pattern::compile("x\\@(a|b)@(c)", options).expect("pattern compiles");
        assert!(pattern.is_match("x@(a|b)c"));
        assert!(!pattern.is_match("xac"));

        let no_escapes = Pattern::compile("x\\@(a|b)@(c)", options.escape(false))
            .expect("pattern compiles without escapes");
        assert!(no_escapes.is_match("x\\ac"));
    }

    #[test]
    fn literal_only_patterns_match_exactly_over_exhaustive_byte_words() {
        let words = byte_words(b"abc", 3);
        for pattern in &words {
            let matcher = Pattern::compile(pattern, PatternOptions::default())
                .expect("literal byte patterns compile");
            for candidate in &words {
                assert_eq!(
                    matcher.is_match(candidate),
                    pattern == candidate,
                    "literal pattern {pattern:?} against {candidate:?}"
                );
            }
        }
    }

    #[test]
    fn wildcard_subset_invariants_hold_over_exhaustive_byte_words() {
        let options = PatternOptions::default().match_hidden(true);
        let any = Pattern::compile("?", options).expect("single wildcard compiles");
        let star = Pattern::compile("*", options).expect("star wildcard compiles");
        let prefixed = Pattern::compile("a*", options).expect("prefixed star compiles");
        let words = byte_words(b"ab.", 4);
        for candidate in &words {
            assert!(
                !any.is_match(candidate) || star.is_match(candidate),
                "question-mark matches a path the star rejects: {candidate:?}"
            );
            assert!(
                !prefixed.is_match(candidate) || star.is_match(candidate),
                "prefixed star matches a path the unrestricted star rejects: {candidate:?}"
            );
        }
    }

    #[test]
    fn failed_state_memos_use_inline_and_heap_storage_without_changing_membership() {
        let mut words = Vec::new();
        let tokens = vec![Token::Star];
        let mut inline = FailedStates::new(&tokens, b"abc", &mut words);
        assert!(matches!(&inline, FailedStates::Inline { .. }));
        assert!(inline.insert(0, 1));
        assert!(!inline.insert(0, 1));
        assert!(inline.insert(0, 2));
        assert!(!inline.insert(0, 2));
        assert!(words.is_empty(), "an inline matrix must not touch the heap");

        let tokens = vec![Token::Star; 2];
        let mut heap = FailedStates::new(&tokens, &[b'a'; 64], &mut words);
        assert!(matches!(&heap, FailedStates::Heap { .. }));
        assert!(heap.insert(1, 63));
        assert!(!heap.insert(1, 63));
        assert!(heap.insert(0, 63));
        assert!(!heap.insert(0, 63));
        // 2 tokens x 65 path positions is 130 bits, which is three words.
        assert_eq!(words.len(), 3);

        // Reusing the buffer must present a cleared matrix, and the words the
        // previous call dirtied must not leak into the next one.
        let mut reused = FailedStates::new(&tokens, &[b'a'; 64], &mut words);
        assert!(reused.insert(1, 63));
        assert!(reused.insert(0, 63));
    }

    #[test]
    fn recursive_prefix_suffix_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default()
            .recursive_double_star(true)
            .case_insensitive(true);
        let fast = Pattern::compile("Src/**/*.RS", options).expect("pattern compiles");
        assert!(matches!(
            fast.alternatives[0].fast_path,
            Some(FastPath::RecursivePrefixSuffix { .. })
        ));
        let mut general = fast.clone();
        general.alternatives[0].fast_path = None;

        let mut candidates = vec![
            b"".to_vec(),
            b"src".to_vec(),
            b"src/".to_vec(),
            b"src/.r".to_vec(),
            b"src/rs".to_vec(),
            b"src/.rs".to_vec(),
            b"src/.hidden.rs".to_vec(),
            b"src/visible.rs".to_vec(),
            b"src/nested/.hidden.rs".to_vec(),
            b"src/nested/visible.rs".to_vec(),
            b"SRC/NESTED/VISIBLE.RS".to_vec(),
            b"SRC\\NESTED\\VISIBLE.RS".to_vec(),
            b"other/visible.rs".to_vec(),
        ];
        candidates.extend(
            byte_words(b"ab./rs", 4)
                .into_iter()
                .map(|suffix| [b"src/".as_slice(), suffix.as_slice()].concat()),
        );
        for candidate in candidates {
            assert_eq!(
                fast.is_match(&candidate),
                general.is_match(&candidate),
                "fast path differs for {candidate:?}"
            );
            assert_eq!(
                fast.is_match_glob_path(&candidate),
                general.is_match_glob_path(&candidate),
                "component fast path differs for {candidate:?}"
            );
        }
        #[cfg(windows)]
        assert!(fast.is_match_glob_path(br"SRC\NESTED\VISIBLE.RS"));
    }

    #[test]
    fn recursive_suffix_fast_path_matches_the_general_matcher() {
        for options in [
            PatternOptions::default().recursive_double_star(true),
            PatternOptions::default()
                .recursive_double_star(true)
                .case_insensitive(true),
            PatternOptions::default()
                .recursive_double_star(true)
                .match_hidden(true),
        ] {
            let fast = Pattern::compile("**/*.ts", options).expect("pattern compiles");
            assert!(matches!(
                fast.alternatives[0].fast_path,
                Some(FastPath::RecursiveSuffix { .. })
            ));
            let mut general = fast.clone();
            general.alternatives[0].fast_path = None;

            let mut candidates = vec![
                b"".to_vec(),
                b".ts".to_vec(),
                b"index.ts".to_vec(),
                b"INDEX.TS".to_vec(),
                b"index.tsx".to_vec(),
                b".hidden.ts".to_vec(),
                b"src/.ts".to_vec(),
                b"src/index.ts".to_vec(),
                b"src/.hidden.ts".to_vec(),
                b".hidden/index.ts".to_vec(),
                b"src/deep/index.ts".to_vec(),
            ];
            candidates.extend(byte_words(b"ab./tsTS", 4));
            for candidate in candidates {
                assert_eq!(
                    fast.is_match(&candidate),
                    general.is_match(&candidate),
                    "fast path differs for {options:?} against {candidate:?}"
                );
                assert_eq!(
                    fast.is_match_glob_path(&candidate),
                    general.is_match_glob_path(&candidate),
                    "component fast path differs for {options:?} against {candidate:?}"
                );
            }
        }
    }

    #[test]
    fn suffix_set_fast_path_matches_linear_alternatives() {
        for options in [
            PatternOptions::default()
                .braces(true)
                .recursive_double_star(true),
            PatternOptions::default()
                .braces(true)
                .recursive_double_star(true)
                .case_insensitive(true),
            PatternOptions::default()
                .braces(true)
                .recursive_double_star(true)
                .match_hidden(true),
        ] {
            for source in ["*.{ts,tsx,js,jsx,mjs,cjs}", "**/*.{ts,tsx,js,jsx,mjs,cjs}"] {
                let fast = Pattern::compile(source, options).expect("suffix set compiles");
                assert!(matches!(
                    fast.alternative_fast_path.as_deref(),
                    Some(AlternativeFastPath::SuffixSet(_))
                ));
                let mut linear = fast.clone();
                linear.alternative_fast_path = None;

                let mut candidates = vec![
                    b"".to_vec(),
                    b".ts".to_vec(),
                    b"index.ts".to_vec(),
                    b"INDEX.TSX".to_vec(),
                    b"index.cjs".to_vec(),
                    b"index.vue".to_vec(),
                    b"src/index.ts".to_vec(),
                    b"src/.hidden.ts".to_vec(),
                    b".hidden/index.jsx".to_vec(),
                    b"src/deep/index.mjs".to_vec(),
                ];
                candidates.extend(byte_words(b"ab./tTsSxjc", 4));
                for candidate in candidates {
                    assert_eq!(
                        fast.is_match(&candidate),
                        linear.is_match(&candidate),
                        "crossing match differs for {source:?}, {options:?}, {candidate:?}"
                    );
                    assert_eq!(
                        fast.is_match_glob_path(&candidate),
                        linear.is_match_glob_path(&candidate),
                        "component match differs for {source:?}, {options:?}, {candidate:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn scoped_suffix_set_fast_path_matches_only_complete_cross_products() {
        for options in [
            PatternOptions::default()
                .braces(true)
                .recursive_double_star(true),
            PatternOptions::default()
                .braces(true)
                .recursive_double_star(true)
                .case_insensitive(true),
            PatternOptions::default()
                .braces(true)
                .recursive_double_star(true)
                .match_hidden(true),
        ] {
            let fast = Pattern::compile("{src,packages}/**/*.{ts,tsx,js,jsx,mjs,cjs}", options)
                .expect("scoped suffix set compiles");
            assert!(matches!(
                fast.alternative_fast_path.as_deref(),
                Some(AlternativeFastPath::ScopedSuffixSet(_))
            ));
            let mut linear = fast.clone();
            linear.alternative_fast_path = None;

            let mut candidates = vec![
                b"".to_vec(),
                b"src/.ts".to_vec(),
                b"src/index.ts".to_vec(),
                b"SRC/INDEX.TSX".to_vec(),
                b"packages/.js".to_vec(),
                b"packages/index.cjs".to_vec(),
                b"packages/deep/index.vue".to_vec(),
                b"vendor/index.ts".to_vec(),
                b"src/.hidden/index.ts".to_vec(),
                b"src/deep/.hidden.mjs".to_vec(),
                b"packages/deep/index.jsx".to_vec(),
            ];
            candidates.extend(
                byte_words(b"ab./tTsSxjc", 4)
                    .into_iter()
                    .map(|suffix| [b"src/".as_slice(), suffix.as_slice()].concat()),
            );
            // The recursive prefix may consume no component, but the ordinary
            // star cannot hand a leading period to its suffix unless hidden
            // matching is explicit. Use a non-first prefix/suffix pair so the
            // aggregate matcher cannot conceal a boundary error.
            assert_eq!(fast.is_match("packages/.js"), options.match_hidden);
            assert_eq!(
                fast.is_match_glob_path("packages/.js"),
                options.match_hidden
            );
            for candidate in candidates {
                assert_eq!(
                    fast.is_match(&candidate),
                    linear.is_match(&candidate),
                    "crossing match differs for {options:?}, {candidate:?}"
                );
                assert_eq!(
                    fast.is_match_glob_path(&candidate),
                    linear.is_match_glob_path(&candidate),
                    "component match differs for {options:?}, {candidate:?}"
                );
            }
        }

        let coupled = Pattern::compile(
            "{src/**/*.ts,lib/**/*.js}",
            PatternOptions::default()
                .braces(true)
                .recursive_double_star(true),
        )
        .expect("coupled alternatives compile");
        assert!(coupled.alternative_fast_path.is_none());
        assert!(!coupled.is_match_glob_path("src/index.js"));
        assert!(!coupled.is_match_glob_path("lib/index.ts"));
    }

    #[test]
    fn recursive_terminal_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default()
            .recursive_double_star(true)
            .case_insensitive(true);
        let fast = Pattern::compile("Src/**", options).expect("pattern compiles");
        assert!(matches!(
            fast.alternatives[0].fast_path,
            Some(FastPath::RecursiveTerminalPrefix { .. })
        ));
        let mut general = fast.clone();
        general.alternatives[0].fast_path = None;

        let mut candidates = vec![
            b"src".to_vec(),
            b"SRC/".to_vec(),
            b"src/file.rs".to_vec(),
            b"src/nested/file.rs".to_vec(),
            b"src/.hidden".to_vec(),
            b"src/.hidden/file.rs".to_vec(),
            b"source/file.rs".to_vec(),
        ];
        candidates.extend(
            byte_words(b"src./", 5)
                .into_iter()
                .map(|suffix| [b"src".as_slice(), suffix.as_slice()].concat()),
        );
        for candidate in candidates {
            assert_eq!(
                fast.is_match(&candidate),
                general.is_match(&candidate),
                "fast path differs for {candidate:?}"
            );
        }
    }

    #[test]
    fn literal_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default().case_insensitive(true);
        let fast = Pattern::compile("Src/Readme", options).expect("pattern compiles");
        assert!(matches!(
            fast.alternatives[0].fast_path,
            Some(FastPath::LiteralTokens)
        ));
        let mut general = fast.clone();
        general.alternatives[0].fast_path = None;

        for candidate in [
            b"src/readme".as_slice(),
            b"SRC/README".as_slice(),
            b"src/readme.txt".as_slice(),
            b"src/readme/".as_slice(),
            b"other/readme".as_slice(),
        ] {
            assert_eq!(
                fast.is_match(candidate),
                general.is_match(candidate),
                "fast path differs for {candidate:?}"
            );
        }
    }

    #[test]
    fn deterministic_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default().case_insensitive(true);
        let fast = Pattern::compile("src/[ab]?.[Rr][Ss]", options).expect("pattern compiles");
        assert!(matches!(
            fast.alternatives[0].fast_path,
            Some(FastPath::DeterministicTokens)
        ));
        let mut general = fast.clone();
        general.alternatives[0].fast_path = None;

        let mut candidates = vec![
            b"src/ab.rs".to_vec(),
            b"src/aX.RS".to_vec(),
            b"src/.a.rs".to_vec(),
            b"src/ab.txt".to_vec(),
            b"lib/ab.rs".to_vec(),
        ];
        candidates.extend(
            byte_words(b"ab./rsRS", 4)
                .into_iter()
                .map(|suffix| [b"src/".as_slice(), suffix.as_slice()].concat()),
        );
        for candidate in candidates {
            assert_eq!(
                fast.is_match(&candidate),
                general.is_match(&candidate),
                "fast path differs for {candidate:?}"
            );
        }
    }

    #[test]
    fn infix_star_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default().case_insensitive(true);
        let fast = Pattern::compile("Src*.rs", options).expect("pattern compiles");
        assert!(matches!(
            fast.alternatives[0].fast_path,
            Some(FastPath::InfixStar { .. })
        ));
        let mut general = fast.clone();
        general.alternatives[0].fast_path = None;

        let mut candidates = vec![
            b"src.rs".to_vec(),
            b"srcMain.RS".to_vec(),
            b"src/nested/main.rs".to_vec(),
            b"src/.hidden.rs".to_vec(),
            b"other.rs".to_vec(),
            b"src.txt".to_vec(),
        ];
        candidates.extend(
            byte_words(b"src./RS", 5)
                .into_iter()
                .map(|middle| [b"src".as_slice(), middle.as_slice(), b".rs"].concat()),
        );
        for candidate in candidates {
            assert_eq!(
                fast.is_match(&candidate),
                general.is_match(&candidate),
                "fast path differs for {candidate:?}"
            );
        }
    }

    #[test]
    fn static_star_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default().case_insensitive(true);
        let fast = Pattern::compile("Src/Lib/*.RS", options).expect("pattern compiles");
        assert!(matches!(
            fast.alternatives[0].fast_path,
            Some(FastPath::StaticStar { .. })
        ));
        let mut general = fast.clone();
        general.alternatives[0].fast_path = None;

        let mut candidates = vec![
            b"src/lib/.rs".to_vec(),
            b"src/lib/main.rs".to_vec(),
            b"SRC/LIB/main.RS".to_vec(),
            b"src/lib/nested/main.rs".to_vec(),
            b"src/lib/.hidden.rs".to_vec(),
            b"src/other/main.rs".to_vec(),
            b"src/lib/main.txt".to_vec(),
        ];
        candidates.extend(
            byte_words(b"ab./rsRS", 4)
                .into_iter()
                .map(|middle| [b"src/lib/".as_slice(), middle.as_slice(), b".rs"].concat()),
        );
        for candidate in candidates {
            assert_eq!(
                fast.is_match(&candidate),
                general.is_match(&candidate),
                "fast path differs for {candidate:?}"
            );
        }
    }

    #[test]
    fn static_star_edge_fast_paths_match_the_general_matcher() {
        let options = PatternOptions::default().case_insensitive(true);
        for (pattern, candidates) in [
            (
                "Src/Lib/*",
                vec![
                    b"src/lib/".as_slice(),
                    b"src/lib/main.rs".as_slice(),
                    b"SRC/LIB/MAIN.RS".as_slice(),
                    b"src/lib/.hidden".as_slice(),
                    b"src/lib/nested/main.rs".as_slice(),
                    b"src/other/main.rs".as_slice(),
                ],
            ),
            (
                "*/Main.RS",
                vec![
                    b"main.rs".as_slice(),
                    b"src/main.rs".as_slice(),
                    b"SRC/DEEP/MAIN.RS".as_slice(),
                    b".hidden/main.rs".as_slice(),
                    b"src/main.txt".as_slice(),
                ],
            ),
        ] {
            let fast = Pattern::compile(pattern, options).expect("pattern compiles");
            assert!(matches!(
                fast.alternatives[0].fast_path,
                Some(FastPath::StaticStar { .. })
            ));
            let mut general = fast.clone();
            general.alternatives[0].fast_path = None;
            for candidate in candidates {
                assert_eq!(
                    fast.is_match(candidate),
                    general.is_match(candidate),
                    "fast path differs for {pattern:?} against {candidate:?}"
                );
            }
        }
    }

    #[test]
    fn single_star_fast_paths_match_the_general_matcher() {
        let mut candidates = byte_words(b"ab./rs", 4);
        candidates.extend([b"src..rs".to_vec(), b"src/.hidden".to_vec()]);
        for pattern in ["*", "src*", "*.rs"] {
            let fast =
                Pattern::compile(pattern, PatternOptions::default()).expect("pattern compiles");
            assert!(fast.alternatives[0].fast_path.is_some());
            let mut general = fast.clone();
            general.alternatives[0].fast_path = None;
            for candidate in &candidates {
                assert_eq!(
                    fast.is_match(candidate),
                    general.is_match(candidate),
                    "fast path differs for {pattern:?} against {candidate:?}"
                );
                assert_eq!(
                    fast.is_match_glob_path(candidate),
                    general.is_match_glob_path(candidate),
                    "component fast path differs for {pattern:?} against {candidate:?}"
                );
            }
        }
    }

    #[test]
    fn component_suffix_fast_paths_preserve_separator_and_hidden_rules() {
        let star = Pattern::compile("*.ts", PatternOptions::default()).unwrap();
        assert!(star.is_match_glob_path("index.ts"));
        assert!(!star.is_match_glob_path(".ts"));
        assert!(!star.is_match_glob_path("src/index.ts"));
        assert!(!star.is_match_glob_path(".hidden.ts"));

        let recursive = Pattern::compile(
            "**/*.ts",
            PatternOptions::default().recursive_double_star(true),
        )
        .unwrap();
        assert!(recursive.is_match_glob_path("index.ts"));
        assert!(recursive.is_match_glob_path("src/deep/index.ts"));
        assert!(!recursive.is_match_glob_path("src/.ts"));
        assert!(!recursive.is_match_glob_path("src/.hidden.ts"));
        assert!(!recursive.is_match_glob_path(".hidden/index.ts"));

        let hidden = Pattern::compile(
            "**/*.ts",
            PatternOptions::default()
                .recursive_double_star(true)
                .match_hidden(true),
        )
        .unwrap();
        assert!(hidden.is_match_glob_path("src/.ts"));
        assert!(hidden.is_match_glob_path("src/.hidden.ts"));
        assert!(hidden.is_match_glob_path(".hidden/index.ts"));
    }

    #[test]
    fn filter_paths_preserves_input_order_and_borrows_inputs() {
        let pattern = Pattern::compile("*.txt", PatternOptions::default()).unwrap();
        let paths = ["first.txt", "skip.rs", "second.txt"];
        assert_eq!(
            pattern.filter_paths(&paths),
            vec![&"first.txt", &"second.txt"]
        );
    }

    #[test]
    fn filter_paths_keeps_wildcards_after_a_separator_in_one_component() {
        let pattern = Pattern::compile(
            "**/lua/*.lua",
            PatternOptions::default().recursive_double_star(true),
        )
        .unwrap();
        let paths = [
            "lua/init.lua",
            "nvim/lua/setup.lua",
            "nvim/lua/sub/nested.lua",
        ];
        assert_eq!(
            pattern.filter_paths(&paths),
            vec![&"lua/init.lua", &"nvim/lua/setup.lua"]
        );
        assert!(pattern.is_match("nvim/lua/sub/nested.lua"));
        assert!(pattern.is_match_path("nvim/lua/setup.lua"));
        assert!(!pattern.is_match_path("nvim/lua/sub/nested.lua"));

        let root_pattern = Pattern::compile("*.rs", PatternOptions::default()).unwrap();
        assert!(root_pattern.is_match_path("src/nested.rs"));
        assert!(!root_pattern.is_match_glob_path("src/nested.rs"));
        assert!(root_pattern.is_match_glob_path("nested.rs"));

        let extglob = Pattern::compile("@(a*)", PatternOptions::default().extglob(true)).unwrap();
        assert!(extglob.is_match_glob_path("aaa"));
        assert!(!extglob.is_match_glob_path("aaa/nested"));
        let nested_extglob =
            Pattern::compile("@(foo)/*/bar", PatternOptions::default().extglob(true)).unwrap();
        assert!(nested_extglob.is_match_glob_path("foo/a/bar"));
        assert!(!nested_extglob.is_match_glob_path("foo/a/deep/bar"));
    }

    #[test]
    fn extglobs_share_each_entry_points_component_policy() {
        let options = PatternOptions::default().extglob(true);
        let mixed = Pattern::compile("*/x/b*@(y)", options).unwrap();
        assert!(mixed.is_match("a/b/x/bzy"));
        assert!(mixed.is_match_path("a/b/x/bzy"));
        assert!(!mixed.is_match_glob_path("a/b/x/bzy"));

        let root = Pattern::compile("*@(y)", options).unwrap();
        assert!(root.is_match("a/b/zy"));
        assert!(root.is_match_path("a/b/zy"));
        assert!(!root.is_match_glob_path("a/b/zy"));

        for pattern in [&mixed, &root] {
            assert!(pattern.engines_agree("a/b/x/bzy"));
            assert!(pattern.engines_agree("a/b/zy"));
        }
    }

    #[test]
    fn recursive_prefix_before_extglob_matches_at_depth_zero() {
        let options = PatternOptions::default()
            .extglob(true)
            .recursive_double_star(true);
        let pattern = Pattern::compile("**/@(x)", options).unwrap();
        for candidate in ["x", "a/x"] {
            assert!(pattern.is_match(candidate), "is_match: {candidate}");
            assert!(
                pattern.is_match_path(candidate),
                "is_match_path: {candidate}"
            );
            assert!(
                pattern.is_match_glob_path(candidate),
                "is_match_glob_path: {candidate}"
            );
            assert!(pattern.engines_agree(candidate), "engines: {candidate}");
        }

        let negated_suffix = Pattern::compile("**/*.!(js)", options).unwrap();
        assert!(negated_suffix.is_match_path("src/app.ts"));
        assert!(!negated_suffix.is_match_path("src/app.js"));
    }

    #[test]
    fn filter_paths_precompiles_its_component_sensitive_ir() {
        let component_pattern = Pattern::compile("src/*.rs", PatternOptions::default())
            .expect("component pattern compiles");
        assert!(
            component_pattern
                .path_filter
                .as_ref()
                .is_some_and(|filter| {
                    filter.compiled.len() == 1
                        && filter.compiled[0].fast_path.is_none()
                        && filter.programs.is_empty()
                        && filter.whole.is_empty()
                })
        );

        let root_pattern =
            Pattern::compile("*.rs", PatternOptions::default()).expect("root pattern compiles");
        assert!(root_pattern.path_filter.is_none());
    }

    #[test]
    fn filter_paths_routes_each_brace_arm_on_its_own() {
        let options = PatternOptions::default().braces(true).extglob(true);
        // The extglob arm is answered from its shared program; the plain
        // arms share one pass over their component copies.
        let mixed = Pattern::compile("{zz,lib/*,src/@(*.ts)}", options).expect("pattern compiles");
        let filter = mixed
            .path_filter
            .as_ref()
            .expect("component arms need the list-filter routes");
        assert_eq!(filter.compiled.len(), 2);
        assert_eq!(filter.programs, [2]);
        assert!(filter.whole.is_empty());

        // Without a plain arm that needs a copy, plain arms keep the
        // whole-sequence route and nothing is compiled twice.
        let lean = Pattern::compile("{zz,src/@(*.ts)}", options).expect("pattern compiles");
        let filter = lean
            .path_filter
            .as_ref()
            .expect("the extglob arm is routed");
        assert!(filter.compiled.is_empty());
        assert_eq!(filter.programs, [1]);
        assert_eq!(filter.whole, [0]);

        // Issue #393: an extglob group directly behind a separator is
        // component-local, and a sibling arm cannot change that verdict.
        for pattern in [
            "src/@(*.ts)",
            "src/+(*.ts)",
            "src/?(*.ts)",
            "src/*(*.ts)",
            "src/!(x)",
            "src/@(x|*.ts)",
            "{src/@(*.ts),zz}",
            "{src/@(*.ts),lib/*}",
            "{zz,src/@(*.ts)}",
            "./src/@(*.ts)",
        ] {
            let pattern = Pattern::compile(pattern, options).expect("pattern compiles");
            assert!(pattern.is_match_path("src/b.ts"));
            assert!(!pattern.is_match_path("src/a/b.ts"));
            assert!(pattern.is_match("src/a/b.ts") || pattern.is_match("./src/a/b.ts"));
            assert!(!pattern.is_match_glob_path("src/a/b.ts"));
        }

        // Only a repetition's first iteration stands behind the separator; a
        // later one follows the previous iteration's text, so `src/+(*.ts)`
        // reads like `src/*.ts*.ts` there. Both engines agree on it.
        let repeated_like = Pattern::compile("src/*.ts*.ts", options).expect("pattern compiles");
        for pattern in ["src/+(*.ts)", "src/*(*.ts)"] {
            let pattern = Pattern::compile(pattern, options).expect("pattern compiles");
            for candidate in ["src/a.tsb/c.ts", "src/a.ts/c.ts", "src/a/b.ts"] {
                assert_eq!(
                    pattern.is_match_path(candidate),
                    repeated_like.is_match_path(candidate),
                    "{candidate}"
                );
                assert!(pattern.engines_agree(candidate), "{candidate}");
            }
            let mut interpreted = pattern.clone();
            for alternative in interpreted.compiled_alternatives_mut() {
                if let Some(program) = alternative.extglob.as_mut() {
                    program.positive_nfa = None;
                }
            }
            for candidate in ["src/a.tsb/c.ts", "src/a/b.ts", "src/a.ts"] {
                assert_eq!(
                    pattern.is_match_path(candidate),
                    interpreted.is_match_path(candidate),
                    "{candidate}"
                );
            }
        }
    }

    /// A one-alternative group answers both path entry points exactly like its
    /// alternative written inline: the group's position is the position of
    /// its first wildcard, a later wildcard in the alternative keeps its own
    /// position, and the outer program backtracks through the group as it does
    /// through plain syntax.
    #[test]
    fn extglob_groups_follow_the_plain_position_rule() {
        let pairs = [
            // Directly behind a separator: component-local (issue #393).
            ("src/@(*.ts)", "src/*.ts"),
            ("a/@(*)/z", "a/*/z"),
            ("x/@(*[z])*", "x/*[z]*"),
            // Behind a literal or at the root: crossing, as in `a/b*`.
            ("src/a@(*.ts)", "src/a*.ts"),
            ("@(*.ts)", "*.ts"),
            ("*/a@(*.ts)", "*/a*.ts"),
            ("a/@(b*)/z", "a/b*/z"),
            ("*/@(b*)/z", "*/b*/z"),
            ("a/@(?*)/z", "a/?*/z"),
            // A crossing star before a component-local one keeps its
            // backtrack point.
            ("*/*.@(ts)", "*/*.ts"),
            ("a*/*.@(ts)", "a*/*.ts"),
            ("**/*.@(ts)", "**/*.ts"),
            ("**/lua/@(*.lua)", "**/lua/*.lua"),
            // A spelled separator inside the group is ordinary syntax.
            ("*/@(a/b)", "*/a/b"),
            // A star run of two or more reads as the token engine reads it:
            // recursive under `recursive_double_star`, otherwise ordinary
            // stars of which only the first stands behind the separator.
            ("x@(y)/***/z", "xy/***/z"),
            ("src/@(*.ts)/***", "src/*.ts/***"),
            ("src/**/*.@(ts)", "src/**/*.ts"),
            // A trailing `/**` also accepts the path without it, and a
            // whole-component `**` in a group may stand for no directory at
            // all, exactly as written inline (#422).
            ("@(x)/**", "x/**"),
            ("x/@(**)", "x/**"),
            ("@(**)/z", "**/z"),
            ("a/@(**)/z", "a/**/z"),
            ("@(x/**)/z", "x/**/z"),
            ("@(**/**)/z", "**/**/z"),
            ("@(**)/*", "**/*"),
        ];
        let candidates = [
            "xy/b/c/z",
            "src/a.ts/b/c",
            "src/a/b/c.ts",
            "a/b/c/d.ts",
            "b.ts",
            "a/b.ts",
            "src/b.ts",
            "src/ab.ts",
            "src/a/b.ts",
            "src/ax/b.ts",
            "x/c.ts",
            "x/a/b/c.ts",
            "ax/b/c.ts",
            "q/a/b.ts",
            "q/ab.ts",
            "a/b/z",
            "a/bx/z",
            "a/b/c/z",
            "x/z",
            "x/zq",
            "x/a/z",
            "x/a/b",
            "lua/a.lua",
            "x/lua/a.lua",
            "x/lua/a/b.lua",
            "x/y/lua/a.lua",
            "x",
            "z",
            "a/z",
            ".z",
            "x/.z",
            "sz",
        ];
        for recursive in [false, true] {
            let options = PatternOptions::default()
                .extglob(true)
                .recursive_double_star(recursive);
            for (extglob, plain) in pairs {
                let extglob = Pattern::compile(extglob, options).expect("pattern compiles");
                let plain = Pattern::compile(plain, options).expect("pattern compiles");
                for candidate in candidates {
                    assert_eq!(
                        (
                            extglob.is_match(candidate),
                            extglob.is_match_path(candidate),
                            extglob.is_match_glob_path(candidate)
                        ),
                        (
                            plain.is_match(candidate),
                            plain.is_match_path(candidate),
                            plain.is_match_glob_path(candidate)
                        ),
                        "{extglob:?} against {candidate} (recursive {recursive})"
                    );
                    assert!(extglob.engines_agree(candidate), "{candidate}");
                }
            }
        }

        // A recursive star inside a group crosses separators under both path
        // entry points; the group is not cut at the component end.
        let options = PatternOptions::default()
            .extglob(true)
            .recursive_double_star(true);
        for (pattern, candidate) in [("a/@(x|**)/*.ts", "a/b/c/d.ts"), ("*/@(**)/z", "a/b/c/z")] {
            let pattern = Pattern::compile(pattern, options).expect("pattern compiles");
            assert!(pattern.is_match_path(candidate), "{candidate}");
            assert!(pattern.is_match_glob_path(candidate), "{candidate}");
            assert!(pattern.engines_agree(candidate), "{candidate}");
        }
    }

    /// A group next to a whole-component `**` answers every entry point like
    /// the union of its alternatives written inline (#422): before a trailing
    /// `/**` the path without that suffix matches, and a `**` that ends an
    /// alternative in front of a separator may stand for no directory. Both
    /// extglob engines agree on it.
    #[test]
    fn extglob_groups_around_a_globstar_read_like_their_inlined_alternatives() {
        let inlined: &[(&str, &[&str])] = &[
            ("@(a|b)/**", &["a/**", "b/**"]),
            ("?(a)/**", &["a/**", "/**"]),
            ("a/@(**)", &["a/**"]),
            ("a/?(**|b)", &["a/**", "a/b", "a/"]),
            ("@(**)/a", &["**/a"]),
            ("@(**|b)/a", &["**/a", "b/a"]),
            ("?(**)/a", &["**/a", "/a"]),
            ("@(**)/.a", &["**/.a"]),
            ("@(**/.a)", &["**/.a"]),
            ("@(****)/.a", &["****/.a"]),
            ("****/@(.a)", &["****/.a"]),
            (r"@(x\\|****/.a)", &["x\\", "****/.a"]),
            ("@(**)/*", &["**/*"]),
            ("@(**)/?", &["**/?"]),
            ("a/@(**)/b", &["a/**/b"]),
            ("@(a/**)/b", &["a/**/b"]),
            ("@(**/**)/a", &["**/**/a"]),
            ("@(a/**/**)/b", &["a/**/**/b"]),
            ("@(**/a|**)/b", &["**/a/b", "**/b"]),
            ("@(**)//a", &["**//a"]),
            ("/@(**)/", &["/**/"]),
            ("@(****)/a", &["****/a"]),
            ("@(***)/a", &["***/a"]),
        ];
        let candidates = byte_words(b"ab./", 4);
        for match_hidden in [false, true] {
            let options = PatternOptions::default()
                .extglob(true)
                .recursive_double_star(true)
                .match_hidden(match_hidden);
            for &(source, alternatives) in inlined {
                let group = Pattern::compile(source, options).expect("pattern compiles");
                let mut interpreted = group.clone();
                for alternative in interpreted.compiled_alternatives_mut() {
                    if let Some(program) = alternative.extglob.as_mut() {
                        program.positive_nfa = None;
                    }
                }
                let alternatives = alternatives
                    .iter()
                    .map(|alternative| Pattern::compile(alternative, options).unwrap())
                    .collect::<Vec<_>>();
                let answers = |pattern: &Pattern, candidate: &[u8]| {
                    [
                        pattern.is_match(candidate),
                        pattern.is_match_path(candidate),
                        pattern.is_match_glob_path(candidate),
                    ]
                };
                for candidate in &candidates {
                    // The path entry points drop one leading `./`, which an
                    // inlined alternative that starts with a separator would
                    // read differently.
                    if candidate.starts_with(b"./") {
                        continue;
                    }
                    let mut expected = alternatives.iter().fold([false; 3], |any, alternative| {
                        let answers = answers(alternative, candidate);
                        [0, 1, 2].map(|entry| any[entry] || answers[entry])
                    });
                    let mut actual = answers(&group, candidate);
                    let mut actual_interpreted = answers(&interpreted, candidate);
                    // Under the list filter the outer program reads a
                    // wildcard behind the separator of `**/` as behind a
                    // separator, where the token engine does not. That only
                    // shows where the wildcard would have to match the
                    // separator of an empty component, as `**/?@(x)` and
                    // `**/?x` already differ on `a//x`, so those candidates
                    // are left out of that entry point here.
                    if candidate.first() == Some(&b'/')
                        || candidate.windows(2).any(|pair| pair == b"//")
                    {
                        expected[1] = false;
                        actual[1] = false;
                        actual_interpreted[1] = false;
                    }
                    let lossy = String::from_utf8_lossy(candidate);
                    assert_eq!(
                        actual, expected,
                        "{source:?} against {lossy:?} (is_match, is_match_path, \
                         is_match_glob_path), match_hidden {match_hidden}"
                    );
                    assert_eq!(
                        actual_interpreted, expected,
                        "interpreter: {source:?} against {lossy:?}, match_hidden {match_hidden}"
                    );
                    assert!(
                        group.engines_agree(candidate),
                        "{source:?} against {lossy:?}"
                    );
                }
            }
        }

        // Shapes without an inline spelling: the outer trailing `/**` still
        // accepts the path in front of it, and a repeated `**` alternative has
        // the same zero-directory reading as a single one.
        let options = PatternOptions::default()
            .extglob(true)
            .recursive_double_star(true);
        for (source, candidate, expected) in [
            ("!(y)/**", "x", true),
            ("!(y)/**", "y", false),
            ("!(y)/**", "x/a", true),
            ("*(a)/**", "a", true),
            ("*(a)/**", "aa", true),
            ("*(a)/**", "b", false),
            ("+(a)/**", "a", true),
            ("+(**)/a", "a", true),
            ("*(**)/a", "a", true),
            ("*(**)/a", "b/a", true),
            ("+(b|**)/a", "b/a", true),
            ("x/*(**)", "x", true),
            ("x/!(**)", "x", false),
            ("!(**)/a", "a", false),
        ] {
            let pattern = Pattern::compile(source, options).expect("pattern compiles");
            let mut interpreted = pattern.clone();
            for alternative in interpreted.compiled_alternatives_mut() {
                if let Some(program) = alternative.extglob.as_mut() {
                    program.positive_nfa = None;
                }
            }
            for matcher in [&pattern, &interpreted] {
                assert_eq!(
                    [
                        matcher.is_match(candidate),
                        matcher.is_match_path(candidate),
                        matcher.is_match_glob_path(candidate),
                    ],
                    [expected; 3],
                    "{source} against {candidate}"
                );
            }
            assert!(
                pattern.engines_agree(candidate),
                "{source} against {candidate}"
            );
        }
    }

    #[test]
    fn extglob_recursive_prefix_hidden_literal_matches_inlined_form() {
        let options = PatternOptions::default()
            .extglob(true)
            .recursive_double_star(true);
        for (group, inline) in [
            ("@(**)/.h", "**/.h"),
            ("**/@(.h)", "**/.h"),
            ("@(**/.h)", "**/.h"),
            ("@(****)/.h", "****/.h"),
            ("****/@(.h)", "****/.h"),
            (r"@(x\\|****/.h)", "****/.h"),
        ] {
            let grouped = Pattern::compile(group, options).expect("group compiles");
            let inlined = Pattern::compile(inline, options).expect("inline compiles");
            for path in [".h", "a/.h", "a/b/.h"] {
                let answers = |pattern: &Pattern| {
                    [
                        pattern.is_match(path),
                        pattern.is_match_path(path),
                        pattern.is_match_glob_path(path),
                    ]
                };
                assert_eq!(
                    answers(&grouped),
                    answers(&inlined),
                    "{group} against {path}"
                );
                assert_eq!(answers(&grouped), [true; 3], "{group} against {path}");
                assert!(grouped.engines_agree(path), "{group} against {path}");
            }
        }
        for pattern in ["@(****)/*", "****/@(*h)", "@(*****)/.h"] {
            let compiled = Pattern::compile(pattern, options).expect("pattern compiles");
            assert!(!compiled.is_match_glob_path(".h"), "{pattern}");
            assert!(compiled.engines_agree(".h"), "{pattern}");
        }
    }

    #[test]
    fn component_deterministic_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default().case_insensitive(true);
        let fast = Pattern::compile("src/[ab]?.[Rr][Ss]", options).expect("pattern compiles");
        assert!(fast.path_filter.as_ref().is_some_and(|filter| {
            matches!(
                filter.compiled[0].fast_path,
                Some(FastPath::DeterministicTokens)
            )
        }));
        let mut general = fast.clone();
        for alternative in general.compiled_alternatives_mut() {
            alternative.fast_path = None;
        }

        let mut candidates = vec![
            b"src/a1.rs".as_slice(),
            b"src/Bx.RS".as_slice(),
            b"src/c1.rs".as_slice(),
            b"src/a/.rs".as_slice(),
            b"src/.1.rs".as_slice(),
            b"src/a1.rs/extra".as_slice(),
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
        candidates.extend(
            byte_words(b"ab./rRsS", 4)
                .into_iter()
                .map(|suffix| [b"src/".as_slice(), suffix.as_slice()].concat()),
        );
        for candidate in candidates {
            assert_eq!(
                fast.is_match_path(&candidate),
                general.is_match_path(&candidate),
                "path-list candidate: {}",
                String::from_utf8_lossy(&candidate)
            );
            assert_eq!(
                fast.is_match_glob_path(&candidate),
                general.is_match_glob_path(&candidate),
                "glob-path candidate: {}",
                String::from_utf8_lossy(&candidate)
            );
        }
    }

    #[test]
    fn filter_paths_at_matches_relative_to_a_component_boundary() {
        let recursive = Pattern::compile(
            "**/*.c",
            PatternOptions::default().recursive_double_star(true),
        )
        .expect("recursive pattern compiles");
        let paths = [
            "/home/user/project/src/main.c",
            "/home/user/project/src/test/unit.c",
            "/home/user/project/lib/utils.c",
            "/home/user/project/docs/readme.md",
            "/home/user/project-other/ignored.c",
            "/short",
        ];
        let expected = vec![
            &"/home/user/project/src/main.c",
            &"/home/user/project/src/test/unit.c",
            &"/home/user/project/lib/utils.c",
        ];
        assert_eq!(
            recursive.filter_paths_at("/home/user/project/", &paths),
            expected
        );
        assert_eq!(
            recursive.filter_paths_at("/home/user/project", &paths),
            expected
        );

        let literal = Pattern::compile("config.json", PatternOptions::default())
            .expect("literal pattern compiles");
        assert_eq!(
            literal.filter_paths_at(
                "/srv/data",
                &["/srv/data/config.json", "/srv/data/readme.md"],
            ),
            vec![&"/srv/data/config.json"]
        );

        let dotted = Pattern::compile(
            "./**/*.c",
            PatternOptions::default().recursive_double_star(true),
        )
        .expect("dot-slash pattern compiles");
        assert_eq!(
            dotted.filter_paths_at(
                "/home/user/project",
                &[
                    "/home/user/project/src/main.c",
                    "/home/user/project/lib/utils.c"
                ],
            ),
            vec![
                &"/home/user/project/src/main.c",
                &"/home/user/project/lib/utils.c"
            ]
        );
    }

    #[test]
    fn filter_path_indices_preserve_input_order_and_support_a_base() {
        let suffix =
            Pattern::compile("*.rs", PatternOptions::default()).expect("suffix pattern compiles");
        assert_eq!(
            suffix.filter_path_indices(&["foo.rs", "bar.txt", "baz.rs", "qux.md"]),
            vec![0, 2]
        );
        assert_eq!(
            suffix.filter_path_indices(&["z.rs", "a.rs", "m.rs"]),
            vec![0, 1, 2]
        );

        let recursive = Pattern::compile(
            "src/**/*.rs",
            PatternOptions::default().recursive_double_star(true),
        )
        .expect("recursive pattern compiles");
        assert_eq!(
            recursive.filter_path_indices_at(
                "/home/me/proj",
                &[
                    "/home/me/proj/src/main.rs",
                    "/home/me/proj/src/lib/util.rs",
                    "/home/me/proj/tests/test.rs",
                    "/home/me/proj/Cargo.toml",
                ],
            ),
            vec![0, 1]
        );
    }

    #[test]
    fn non_recursive_double_star_matches_one_star_per_entry_point() {
        let single = Pattern::compile("*/*.c", PatternOptions::default()).unwrap();
        let doubled = Pattern::compile("**/*.c", PatternOptions::default()).unwrap();
        for candidate in ["a.c", "d/a.c", "d/e/a.c", "d/e/a.rs"] {
            assert_eq!(
                doubled.is_match(candidate),
                single.is_match(candidate),
                "is_match differs for {candidate:?}"
            );
            assert_eq!(
                doubled.is_match_path(candidate),
                single.is_match_path(candidate),
                "is_match_path differs for {candidate:?}"
            );
            assert_eq!(
                doubled.is_match_glob_path(candidate),
                single.is_match_glob_path(candidate),
                "is_match_glob_path differs for {candidate:?}"
            );
        }
    }

    #[test]
    fn filter_paths_normalizes_a_leading_dot_slash() {
        let options = PatternOptions::default().recursive_double_star(true);
        let bare = Pattern::compile("**/*.lua", options).unwrap();
        let dotted = Pattern::compile("./**/*.lua", options).unwrap();
        let paths = [
            "init.lua",
            "lua/setup.lua",
            "nested/deep/plugin.lua",
            "src/main.zig",
        ];
        assert_eq!(dotted.filter_paths(&paths), bare.filter_paths(&paths));
        assert!(!dotted.is_match("nested/deep/plugin.lua"));

        for pattern in [&bare, &dotted] {
            assert!(pattern.is_match_path("lua/setup.lua"));
            assert!(pattern.is_match_path("./lua/setup.lua"));
            assert!(pattern.is_match_glob_path("lua/setup.lua"));
            assert!(pattern.is_match_glob_path("./lua/setup.lua"));
        }

        let current_directory = compile("./");
        assert!(current_directory.is_match_path(""));
        assert!(current_directory.is_match_path("./"));
        assert!(current_directory.is_match_glob_path(""));
        assert!(current_directory.is_match_glob_path("./"));
        assert!(!current_directory.is_match(""));
    }

    #[test]
    fn invalid_syntax_has_a_location() {
        let error = Pattern::compile("[abc", PatternOptions::default()).unwrap_err();
        assert_eq!(error.offset(), 0);
        assert_eq!(error.message(), "unclosed character class");
        assert!(compile("foo\\").is_match("foo\\"));
    }

    #[test]
    fn invalid_classes_in_extglob_alternatives_are_compile_errors() {
        let options = PatternOptions::default().extglob(true);
        for (source, offset) in [
            ("@([)]", 2),
            ("@(a|[)]", 4),
            ("@(dead/[)]]/../x|src/main.rs)", 7),
        ] {
            let error = Pattern::compile(source, options)
                .expect_err("an invalid extglob alternative is rejected");
            assert_eq!(error.message(), "unclosed character class");
            assert_eq!(error.offset(), offset);
        }
    }

    #[test]
    fn brace_expansion_errors_keep_source_offsets() {
        let options = PatternOptions::default().braces(true);
        for (pattern, offset) in [("abc{d,[}", 6), ("{a,b}[", 5), ("x{a,b}{c,[d}", 9)] {
            let error = Pattern::compile(pattern, options)
                .expect_err("the expanded alternative contains an unclosed class");
            assert_eq!(error.message(), "unclosed character class");
            assert_eq!(error.offset(), offset);
            assert_eq!(
                pattern.as_bytes().get(error.offset()),
                Some(&b'['),
                "{pattern} points into the caller's source"
            );
        }
    }

    #[test]
    fn deeply_nested_unclosed_posix_openers_are_rejected() {
        let mut pattern = String::from("[");
        pattern.push_str("[:".repeat(32_768).as_str());
        let error = Pattern::compile(&pattern, PatternOptions::default()).unwrap_err();
        assert_eq!(error.offset(), 0);
        assert_eq!(error.message(), "unclosed character class");
    }

    #[test]
    fn deeply_nested_posix_openers_with_a_final_bracket_compile_linearly() {
        let mut pattern = String::from("[");
        pattern.push_str("[:".repeat(32_768).as_str());
        pattern.push(']');
        assert!(Pattern::compile(&pattern, PatternOptions::default()).is_ok());
    }

    #[test]
    fn unclosed_braces_compile_linearly() {
        let options = PatternOptions::default().braces(true);
        let unclosed = "{".repeat(100_000);
        reset_unconditional_brace_scanned_bytes();
        let compiled = Pattern::compile(&unclosed, options).expect("unclosed braces compile");
        assert_eq!(unconditional_brace_peak(), 0);
        assert!(
            unconditional_brace_scanned_bytes() <= unclosed.len(),
            "brace preflight scanned {} bytes for {} input bytes",
            unconditional_brace_scanned_bytes(),
            unclosed.len()
        );
        assert!(compiled.is_match(&unclosed));
        assert!(!compiled.is_match("x"));

        // A single trailing `}` must keep preflight scratch memory O(1) even
        // behind 100,000 unclosed `{` openers, and brace expansion stays O(N).
        let with_tail = format!("{unclosed}{{a,b}}?");
        let expanded = super::expand_braces(&with_tail, options)
            .expect("unclosed braces with tail expand linearly");
        assert_eq!(expanded, [with_tail.as_bytes()]);
        reset_unconditional_brace_scanned_bytes();
        let compiled_tail =
            Pattern::compile(&with_tail, options).expect("unclosed braces with tail compile");
        assert_eq!(unconditional_brace_peak(), 1);
        assert!(
            unconditional_brace_scanned_bytes() <= with_tail.len(),
            "brace preflight scanned {} bytes for {} input bytes",
            unconditional_brace_scanned_bytes(),
            with_tail.len()
        );
        assert!(compiled_tail.is_match(format!("{unclosed}{{a,b}}x")));
        assert!(!compiled_tail.is_match(format!("{unclosed}ax")));

        let with_prefix = format!("{{a,b}}{unclosed}");
        reset_unconditional_brace_scanned_bytes();
        let compiled_prefix = Pattern::compile(&with_prefix, options)
            .expect("prefix brace with unclosed tail compiles");
        assert_eq!(unconditional_brace_peak(), 1);
        assert!(
            unconditional_brace_scanned_bytes() <= with_prefix.len(),
            "brace preflight scanned {} bytes for {} input bytes",
            unconditional_brace_scanned_bytes(),
            with_prefix.len()
        );
        assert!(compiled_prefix.is_match(format!("a{unclosed}")));
        assert!(compiled_prefix.is_match(format!("b{unclosed}")));
        assert!(!compiled_prefix.is_match(format!("c{unclosed}")));

        // Live nesting deeper than the cap with as many closing braces bails
        // out of the preflight with bounded scratch instead of growing with N.
        let deeply_nested = format!("{}{}", "{".repeat(2_000), "}".repeat(2_000));
        reset_unconditional_brace_scanned_bytes();
        assert_eq!(
            super::unconditional_compiled_units(deeply_nested.as_bytes(), options),
            None
        );
        assert!(unconditional_brace_peak() <= super::MAX_UNCONDITIONAL_BRACE_DEPTH);
        assert!(unconditional_brace_scanned_bytes() <= deeply_nested.len());
    }

    #[test]
    fn unclosed_extglob_openers_compile_linearly() {
        let options = PatternOptions::default().extglob(true);

        // Well-formed short groups followed by a large literal tail must not
        // allocate a delimiter table.
        for closed_with_tail in [
            format!("@(a){}", "x".repeat(200_000)),
            format!("!(a){}", "x".repeat(400_000)),
        ] {
            reset_extglob_closing_scanned_bytes();
            let compiled_closed = Pattern::compile(&closed_with_tail, options)
                .expect("closed group with large literal tail compiles");
            assert_eq!(extglob_closing_table_len(), 0);
            assert!(
                extglob_closing_scanned_bytes() <= 16,
                "short closed group scanned {} bytes",
                extglob_closing_scanned_bytes()
            );
            assert!(!compiled_closed.is_match("x"));
        }

        // Unclosed openers without a 4th extglob opener before `last_close`
        // (including `@(@(@(a...a)` where the 3rd opener closes directly, or
        // unclosed openers followed only by escaped `\)`) must not allocate a
        // delimiter table.
        for no_table_pattern in [
            format!("@({}()", "x".repeat(64_000)),
            format!("@(@(a){}()", "x".repeat(8_192)),
            format!("@(@({}()", "x".repeat(64_000)),
            format!("@(@(@({})", "a".repeat(64_000)),
            format!("@(@(@({}))", "a".repeat(64_000)),
            format!(r"@(@(@({}\)", "x".repeat(64_000)),
        ] {
            reset_extglob_closing_scanned_bytes();
            let compiled = Pattern::compile(&no_table_pattern, options)
                .expect("unclosed opener with balanced or escaped tail compiles");
            assert_eq!(extglob_closing_table_len(), 0);
            assert!(
                extglob_closing_scanned_bytes() <= 6 * no_table_pattern.len(),
                "delimiter scan visited {} bytes for {} input bytes",
                extglob_closing_scanned_bytes(),
                no_table_pattern.len()
            );
            assert!(!compiled.is_match("x"));
        }

        // An unclosed opener after a long prefix of escaped `\)` must never
        // scan backward over the prefix.
        let escaped_close_prefix = format!("{}@(", r"\)".repeat(32_000));
        reset_extglob_closing_scanned_bytes();
        let compiled_escaped_prefix = Pattern::compile(&escaped_close_prefix, options)
            .expect("escaped close prefix before unclosed opener compiles");
        assert_eq!(extglob_closing_table_len(), 0);
        assert!(
            extglob_closing_scanned_bytes() <= 8,
            "prefix of escaped `\\)` must not be rescanned, scanned {} bytes",
            extglob_closing_scanned_bytes()
        );
        assert!(!compiled_escaped_prefix.is_match("x"));

        // Control case: an even number of backslashes (`\\)`) leaves `)`
        // unescaped, so after 3 unclosed openers the 4th closes against it via
        // the table.
        let unescaped_after_double_backslash = format!(r"@(@(@(@({}\\)", "x".repeat(64_000));
        reset_extglob_closing_scanned_bytes();
        let compiled_double_backslash =
            Pattern::compile(&unescaped_after_double_backslash, options)
                .expect("escaped backslash before closing paren compiles");
        assert!(extglob_closing_table_len() > 0);
        assert!(
            extglob_closing_scanned_bytes() <= 8 * unescaped_after_double_backslash.len(),
            "escaped backslash tail scanned {} bytes for {} input bytes",
            extglob_closing_scanned_bytes(),
            unescaped_after_double_backslash.len()
        );
        assert!(!compiled_double_backslash.is_match("x"));
        let small_double_backslash =
            Pattern::compile(r"@(@(@(a\\)", options).expect("small double backslash compiles");
        assert!(small_double_backslash.is_match(r"@(@(a\"));
        assert!(!small_double_backslash.is_match(r"@(@(@(a\)"));

        let repeated_at = "@(".repeat(50_000);
        reset_extglob_closing_scanned_bytes();
        let compiled_at =
            Pattern::compile(&repeated_at, options).expect("unclosed @() openers compile");
        assert_eq!(extglob_closing_table_len(), 0);
        assert!(
            extglob_closing_scanned_bytes() <= 4 * repeated_at.len(),
            "repeated @(: scanned {} bytes for {} input bytes",
            extglob_closing_scanned_bytes(),
            repeated_at.len()
        );
        assert!(!compiled_at.is_match("x"));

        let repeated_at_closed_tail = format!("{repeated_at})");
        reset_extglob_closing_scanned_bytes();
        let compiled_at_tail = Pattern::compile(&repeated_at_closed_tail, options)
            .expect("unclosed @() openers with one closed tail compile");
        assert_eq!(extglob_closing_table_len(), repeated_at.len() - 6);
        assert!(
            extglob_closing_scanned_bytes() <= 8 * repeated_at_closed_tail.len(),
            "repeated @( with tail: scanned {} bytes for {} input bytes",
            extglob_closing_scanned_bytes(),
            repeated_at_closed_tail.len()
        );
        assert!(!compiled_at_tail.is_match("x"));

        // When a literal suffix follows the last `)`, the lazy delimiter table
        // covers only `fourth_open..=last_close` rather than the trailing literals.
        let repeated_at_middle_close = format!("{repeated_at}){}", "x".repeat(100_000));
        reset_extglob_closing_scanned_bytes();
        let compiled_at_middle = Pattern::compile(&repeated_at_middle_close, options)
            .expect("unclosed @() openers with mid-pattern close compile");
        assert_eq!(extglob_closing_table_len(), repeated_at.len() - 6);
        assert!(
            extglob_closing_scanned_bytes() <= 8 * repeated_at_middle_close.len(),
            "repeated @( with mid-close: scanned {} bytes for {} input bytes",
            extglob_closing_scanned_bytes(),
            repeated_at_middle_close.len()
        );
        assert!(!compiled_at_middle.is_match("x"));

        // When a long literal prefix precedes 3 unclosed openers and a 4th
        // opener before `)`, the lazy table starts at the 4th opener rather
        // than byte 0.
        let prefix_before_unclosed = format!("{}@(@(@(@()", "x".repeat(200_000));
        reset_extglob_closing_scanned_bytes();
        let compiled_prefix_tail = Pattern::compile(&prefix_before_unclosed, options)
            .expect("long literal prefix before unclosed openers compiles");
        assert!(
            extglob_closing_table_len() > 0 && extglob_closing_table_len() <= 8,
            "table must start at the fourth opener, got len {}",
            extglob_closing_table_len()
        );
        assert!(
            extglob_closing_scanned_bytes() <= 64,
            "long prefix before unclosed tail must not be scanned, got {} bytes",
            extglob_closing_scanned_bytes()
        );
        assert!(!compiled_prefix_tail.is_match("x"));

        // Verify matching semantics on small inputs with the same structure.
        let small_at = Pattern::compile("@(@(@(", options).expect("small unclosed @() compiles");
        assert!(small_at.is_match("@(@(@("));
        assert!(!small_at.is_match("@(@("));
        let small_at_tail =
            Pattern::compile("@(@(@()", options).expect("small unclosed @() with tail compiles");
        assert!(small_at_tail.is_match("@(@("));
        assert!(!small_at_tail.is_match("@(@(@()"));

        let repeated_double_star = "**(".repeat(30_000);
        reset_extglob_closing_scanned_bytes();
        let compiled_stars = Pattern::compile(&repeated_double_star, options)
            .expect("unclosed **() openers compile");
        assert_eq!(extglob_closing_table_len(), 0);
        assert!(
            extglob_closing_scanned_bytes() <= 4 * repeated_double_star.len(),
            "repeated **(: scanned {} bytes for {} input bytes",
            extglob_closing_scanned_bytes(),
            repeated_double_star.len()
        );
        assert!(!compiled_stars.is_match("x"));

        let repeated_double_star_tail = format!("{repeated_double_star})");
        reset_extglob_closing_scanned_bytes();
        let compiled_stars_tail = Pattern::compile(&repeated_double_star_tail, options)
            .expect("unclosed **() openers with one closed tail compile");
        assert_eq!(
            extglob_closing_table_len(),
            repeated_double_star_tail.len() - 11
        );
        assert!(
            extglob_closing_scanned_bytes() <= 8 * repeated_double_star_tail.len(),
            "repeated **( with tail: scanned {} bytes for {} input bytes",
            extglob_closing_scanned_bytes(),
            repeated_double_star_tail.len()
        );
        assert!(!compiled_stars_tail.is_match("x"));

        let small_stars =
            Pattern::compile("**(**(", options).expect("small unclosed **() compiles");
        assert!(small_stars.is_match("a(b("));
        assert!(!small_stars.is_match("a("));
        let small_stars_tail =
            Pattern::compile("**(**()", options).expect("small unclosed **() with tail compiles");
        assert!(small_stars_tail.is_match("a("));
        assert!(!small_stars_tail.is_match("a"));
    }

    #[test]
    fn unconditional_compiled_units_matches_direct_brace_scan() {
        fn reference(pattern: &[u8], options: PatternOptions) -> (usize, bool) {
            let mut question_marks = 0_usize;
            let mut has_extglob = false;
            let mut saw_unclosed_class = false;
            let mut skip_class_until = 0_usize;
            let mut index = 0;
            while index < pattern.len() {
                if options.escape && pattern[index] == b'\\' {
                    index += usize::from(index + 1 < pattern.len()) + 1;
                    continue;
                }
                if !saw_unclosed_class && index >= skip_class_until && pattern[index] == b'[' {
                    if let Some((next, has_unescaped_brace, _)) =
                        super::scan_class_end(pattern, index, options.escape)
                    {
                        if !has_unescaped_brace {
                            index = next;
                            continue;
                        }
                        skip_class_until = next;
                    } else {
                        saw_unclosed_class = true;
                    }
                }
                if pattern[index] == b'{'
                    && let Some(close) = super::matching_brace(pattern, index, options.escape)
                {
                    index = close + 1;
                    continue;
                }
                question_marks += usize::from(pattern[index] == b'?');
                has_extglob |= super::detect_extglob_at(pattern, index).is_some();
                index += 1;
            }
            (question_marks, options.extglob && has_extglob)
        }

        let cases = [
            "",
            "?",
            "{?}",
            "{{?}",
            "{{{?a}?}",
            "{@(a)}",
            "{{@(a}",
            "{@(a{b}",
            "{@({a})",
            "@({a})",
            "{a@}(b)",
            "{?}(a)",
            "a?({b})",
            r"a\{?}",
            r"{?a\",
            r"{a\\}?",
            r"{\\}}?",
            r"{{{\}\}?{?}",
            r"{?{?\}",
            r"}\\{?",
            "{{{??@(a){?}?",
            "[@(]",
            "[?@(]",
            "{a,b}[@(]",
            "a[@(b)",
            "{a,b}[}]",
        ];
        for escape in [false, true] {
            for extglob in [false, true] {
                let options = PatternOptions::default().escape(escape).extglob(extglob);
                for case in cases {
                    assert_eq!(
                        super::unconditional_compiled_units(case.as_bytes(), options),
                        Some(reference(case.as_bytes(), options)),
                        "mismatch for {case:?} under escape={escape}, extglob={extglob}"
                    );
                }
            }
        }

        // Twelve `{a,b}` groups + 260 literals + `[@(]` must not trip the
        // extglob IR lower-bound guard because `@(` lives inside a closed
        // bracket class (#495).
        let braced_class = format!("{}{}[@(]", "{a,b}".repeat(12), "x".repeat(260));
        let compiled = Pattern::compile(&braced_class, PatternOptions::walker())
            .expect("closed bracket class with `@(` must not charge extglob IR");
        let accept = format!("{}{}@", "a".repeat(12), "x".repeat(260));
        let reject = format!("{}{}z", "a".repeat(12), "x".repeat(260));
        assert!(compiled.is_match(&accept));
        assert!(!compiled.is_match(&reject));

        // Classes containing thousands of literal `[` bytes plus escaped or
        // unescaped braces must scan linearly in `unconditional_compiled_units`.
        let many_open_escaped_brace = format!("[{}\\{{]", "[".repeat(16_000));
        reset_unconditional_brace_scanned_bytes();
        assert_eq!(
            super::unconditional_compiled_units(
                many_open_escaped_brace.as_bytes(),
                PatternOptions::walker()
            ),
            Some((0, false))
        );
        assert!(
            unconditional_brace_scanned_bytes() <= 16,
            "escaped brace inside class must be skipped in one jump: {}",
            unconditional_brace_scanned_bytes()
        );
        let compiled_escaped = Pattern::compile(&many_open_escaped_brace, PatternOptions::walker())
            .expect("large class with escaped brace compiles");
        assert!(compiled_escaped.is_match("{"));
        assert!(compiled_escaped.is_match("["));

        let many_open_unescaped_brace = format!("[{}{{a,b}}]", "[".repeat(16_000));
        reset_unconditional_brace_scanned_bytes();
        assert_eq!(
            super::unconditional_compiled_units(
                many_open_unescaped_brace.as_bytes(),
                PatternOptions::walker()
            ),
            Some((0, false))
        );
        assert!(
            unconditional_brace_scanned_bytes() <= many_open_unescaped_brace.len(),
            "unescaped brace inside class must not trigger nested `[` rescans"
        );
        let compiled_unescaped =
            Pattern::compile(&many_open_unescaped_brace, PatternOptions::walker())
                .expect("large class with unescaped brace compiles");
        assert!(compiled_unescaped.is_match("a"));
        assert!(compiled_unescaped.is_match("b"));
    }

    #[test]
    fn precomputed_extglob_closing_parentheses_match_direct_scan() {
        let cases = [
            "",
            "@(",
            "@()",
            "@((a)",
            "@((a))",
            r"@(a\))",
            r"@(a\)",
            r"@(a\\))",
            r"\@(a)",
            r"@(a)\)",
            "[(]@(a)",
            "[)]@(a)",
            "[(]@(a))",
            "@([)])",
            "**(a)**(b",
            "**(a)**(b)",
            r"@(a\",
            "((())((",
            "@(@(@(a)",
            "@(@(@(@(a)",
            r"@(@(@(@(a\))",
        ];
        for escapes in [false, true] {
            for case in cases {
                let bytes = case.as_bytes();
                let precomputed = super::precompute_extglob_closing_parentheses(bytes, escapes);
                super::EXTGLOB_CLOSING_TABLE_LEN.with(|cell| cell.set(0));
                let mut resolver = super::ExtglobClosingParens::Direct;
                let mut index = 0;
                while index < bytes.len() {
                    if escapes && bytes[index] == b'\\' {
                        index += 2;
                        continue;
                    }
                    if bytes[index] == b'(' {
                        let expected = super::closing_extglob_parenthesis(bytes, index, escapes);
                        assert_eq!(
                            precomputed
                                .get(index)
                                .copied()
                                .filter(|&close| close != usize::MAX),
                            expected,
                            "precomputed mismatch at offset {index} for {case:?} (escapes={escapes})"
                        );
                        assert_eq!(
                            resolver.closing(bytes, index, escapes),
                            expected,
                            "resolver mismatch at offset {index} for {case:?} (escapes={escapes})"
                        );
                    }
                    index += 1;
                }
                if case == "@(@(@(@(a)" {
                    assert!(
                        extglob_closing_table_len() > 0,
                        "3 unclosed openers followed by a 4th before `)` must build the indexed table"
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_posix_opener_does_not_hide_a_later_valid_class() {
        let pattern = Pattern::compile("[[:[:alpha:]]", PatternOptions::default())
            .expect("character class compiles");
        assert!(pattern.is_match("x"));
        assert!(!pattern.is_match("1"));
    }

    #[test]
    fn has_wildcards_respects_brace_and_extglob_options() {
        let plain = PatternOptions::default();
        assert!(!Pattern::has_wildcards("literal", plain));
        assert!(Pattern::has_wildcards("file?.rs", plain));
        assert!(Pattern::has_wildcards("[[:alpha:]]", plain));
        assert!(!Pattern::has_wildcards("{src,lib}", plain));
        assert!(Pattern::has_wildcards(
            "{src,lib}",
            PatternOptions::default().braces(true)
        ));
        assert!(!Pattern::has_wildcards("@(src|lib)", plain));
        assert!(Pattern::has_wildcards(
            "@(src|lib)",
            PatternOptions::default().extglob(true)
        ));
        assert!(!Pattern::has_wildcards(
            "literal(",
            PatternOptions::default().extglob(true)
        ));
    }

    /// Strips every route around the general engine, leaving the prefilter as
    /// the only thing between a candidate and the state walk.
    fn only_the_general_engine(pattern: &mut Pattern) {
        pattern.alternative_fast_path = None;
        for alternative in pattern.compiled_alternatives_mut() {
            alternative.fast_path = None;
        }
    }

    /// The same pattern with the prefilter neutralized, so the engine decides
    /// on its own.
    fn without_the_prefilter(pattern: &Pattern) -> Pattern {
        let mut unfiltered = pattern.clone();
        for alternative in unfiltered.compiled_alternatives_mut() {
            alternative.prefilter = Prefilter::default();
        }
        unfiltered
    }

    /// The prefilter may only reject what the general engine rejects.
    ///
    /// Every candidate is decided twice through the same engine — once with the
    /// compiled prefilter in front of it, once with the prefilter neutralized —
    /// across the three entry points that reach it, because each supplies its
    /// own `PatternOptions` and the prefilter is derived without them. The fast
    /// paths are removed from both copies so nothing routes around the engine
    /// and turns the comparison into a tautology.
    #[test]
    fn the_prefilter_rejects_only_what_the_general_engine_rejects() {
        let plain = PatternOptions::default();
        let recursive = PatternOptions::default().recursive_double_star(true);
        let folded = recursive.case_insensitive(true);
        let escaped = plain.escape(true);
        let cases: [(&str, PatternOptions); 24] = [
            // The bench case: nothing in the engine consults the trailing `b`.
            ("a*a*a*a*b", plain),
            ("*a*a*", plain),
            ("a*", plain),
            ("*a", plain),
            ("*", plain),
            ("?a*b?", plain),
            ("[ab]*b", plain),
            ("a/*/b", plain),
            ("a/**", recursive),
            ("**/a", recursive),
            ("**/*.b", recursive),
            ("**", recursive),
            ("a/**/b*b", recursive),
            ("A/**/*.B", folded),
            ("A*A*B", folded),
            ("./a/**/b", recursive),
            // Interior runs: fixed bytes with a wildcard on both sides.
            ("**/ab/**", recursive),
            ("**/a/*.b", recursive),
            ("*/ab/*", plain),
            ("*ab*", plain),
            ("*a\\/b*", escaped),
            ("./**/b./**", recursive),
            ("**/AB/**", folded),
            ("*A.B*", folded),
        ];
        let mut candidates = byte_words(b"ab./", 4);
        candidates.push(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec());
        candidates.push(b"a/b/a/b/a/b.b".to_vec());
        candidates.push(b"A/B/A.B".to_vec());
        candidates.push(b"a/AB/b".to_vec());
        candidates.push(b"./a/b./b".to_vec());

        for (source, options) in cases {
            let mut filtered = Pattern::compile(source, options).expect("case pattern compiles");
            only_the_general_engine(&mut filtered);
            let unfiltered = without_the_prefilter(&filtered);
            for candidate in &candidates {
                for (entry, filtered, unfiltered) in [
                    (
                        "is_match",
                        filtered.is_match(candidate),
                        unfiltered.is_match(candidate),
                    ),
                    (
                        "is_match_path",
                        filtered.is_match_path(candidate),
                        unfiltered.is_match_path(candidate),
                    ),
                    (
                        "is_match_glob_path",
                        filtered.is_match_glob_path(candidate),
                        unfiltered.is_match_glob_path(candidate),
                    ),
                ] {
                    assert_eq!(
                        filtered, unfiltered,
                        "{source} via {entry} disagrees with the unfiltered engine on {candidate:?}"
                    );
                }
            }
        }
    }

    /// What the three facts are, on the shapes whose reading is not obvious.
    ///
    /// The separator before a terminal `**` is the one token that may consume
    /// nothing, so it belongs to neither the leading run nor the minimum
    /// length; `a/**` accepting `a` is what that buys.
    #[test]
    fn the_prefilter_leaves_out_the_separator_a_terminal_double_star_may_elide() {
        let recursive = PatternOptions::default().recursive_double_star(true);
        let terminal = Pattern::compile("ab/**", recursive).expect("terminal pattern compiles");
        let prefilter = &terminal.alternatives[0].prefilter;
        assert_eq!(
            prefilter.prefix, b"ab",
            "the elidable separator is left off"
        );
        assert_eq!(prefilter.suffix, b"", "a trailing run cannot hold a `**`");
        assert_eq!(prefilter.min_length, 2, "the separator is not counted");
        assert!(!prefilter.rejects(b"ab", false), "`ab/**` accepts `ab`");

        // A separator anywhere else is an ordinary byte of the fixed run.
        let inner = Pattern::compile("ab/**/cd", recursive).expect("inner pattern compiles");
        let prefilter = &inner.alternatives[0].prefilter;
        assert_eq!(prefilter.prefix, b"ab/");
        assert_eq!(prefilter.suffix, b"cd");
        assert_eq!(prefilter.min_length, 5);

        // Stars are worth nothing, every other token its own bytes.
        let mixed = Pattern::compile("a?[bc]*d", PatternOptions::default())
            .expect("mixed pattern compiles");
        let prefilter = &mixed.alternatives[0].prefilter;
        assert_eq!(prefilter.prefix, b"a");
        assert_eq!(prefilter.suffix, b"d");
        assert_eq!(prefilter.min_length, 4);
    }

    /// The needle an interior run leaves, on the shapes that decide it.
    #[test]
    fn the_prefilter_searches_for_the_longest_interior_run() {
        let recursive = PatternOptions::default().recursive_double_star(true);
        let needle = |source: &str, options: PatternOptions| {
            let pattern = Pattern::compile(source, options).expect("needle pattern compiles");
            pattern.alternatives[0]
                .prefilter
                .interior
                .as_ref()
                .map(|interior| interior.0.needle().to_vec())
        };
        // Neither separator joins: `**/` may consume nothing at the start and
        // the terminal separator may consume nothing at the end.
        assert_eq!(
            needle("**/node_modules/**", recursive).as_deref(),
            Some(b"node_modules".as_slice())
        );
        // A separator between the run and a wildcard is consumed as a byte,
        // except on Windows, where it also accepts `\\`.
        let lib = if cfg!(windows) { "lib" } else { "lib/" };
        assert_eq!(
            needle("**/lib/*.js", recursive).as_deref(),
            Some(lib.as_bytes())
        );
        assert_eq!(
            needle("src/**/test/*.rs", recursive).as_deref(),
            Some(if cfg!(windows) {
                b"test".as_slice()
            } else {
                b"test/"
            })
        );
        // The longest run wins, the earlier one on a tie.
        assert_eq!(
            needle("*a*bcd*ef*", PatternOptions::default()).as_deref(),
            Some(b"bcd".as_slice())
        );
        assert_eq!(
            needle("*ab*cd*", PatternOptions::default()).as_deref(),
            Some(b"ab".as_slice())
        );
        // An escaped separator is a literal byte, compared exactly everywhere.
        assert_eq!(
            needle("*a\\/b*", PatternOptions::default().escape(true)).as_deref(),
            Some(b"a/b".as_slice())
        );
        // Fixed ends are compared in place, and a lone separator or the
        // elidable one carries nothing worth a search.
        assert_eq!(needle("ab/**", recursive), None);
        assert_eq!(needle("ab*cd", PatternOptions::default()), None);
        assert_eq!(needle("src/main.rs", PatternOptions::default()), None);
        assert_eq!(needle("*/*", PatternOptions::default()), None);
        assert_eq!(needle("**/*", recursive), None);

        let excluded = Pattern::compile("**/node_modules/**", recursive).unwrap();
        let prefilter = &excluded.alternatives[0].prefilter;
        assert!(prefilter.rejects(b"src/lib/index.js", false));
        assert!(!prefilter.rejects(b"a/node_modules/b", false));
        assert!(!prefilter.rejects(b"node_modules", false));
        // Folding has no `memmem` form, so the search stands aside.
        assert!(!prefilter.rejects(b"a/NODE_MODULES/b", true));
    }

    /// The prefilter may only reject what the engines behind it reject, over
    /// generated patterns and paths.
    ///
    /// The fragments lean on multi-byte literals next to every wildcard,
    /// separator and group form, so interior runs start and end in every
    /// position, and the path pieces reuse the same literals so a good share
    /// of candidates contain the needle and reach the engine. Both copies keep
    /// the sweep and every extglob program and only differ in their
    /// prefilters, nested extglob alternatives included; the fast paths are
    /// stripped from both so nothing answers before the prefilter is asked.
    #[test]
    fn the_prefilter_agrees_with_the_unfiltered_engines_over_randomized_patterns() {
        let fragments: &[&[u8]] = &[
            b"ab",
            b"nm",
            b"a",
            b"B",
            b".",
            b"/",
            b"./",
            b"*",
            b"?",
            b"**",
            b"**/",
            b"/**",
            b"[ab]",
            b"[!a]",
            b"{ab,B}",
            b"{a/b,nm}",
            b"\\/",
            b"\\*",
            b"@(ab|B)",
            b"!(nm)",
            b"+(a)",
            b"*(ab/)",
        ];
        let path_pieces: &[&[u8]] = &[
            b"a", b"b", b"B", b"ab", b"AB", b"nm", b"NM", b".", b"/", b"./", b"a/b",
        ];
        // The same reproducible generator as the sweep's randomized test.
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move |bound: usize| {
            seed = seed.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(1);
            (usize::try_from(seed >> 33).expect("31 bits fit a usize")) % bound
        };
        let mut searched = 0_usize;
        for _ in 0..3_000 {
            let mut pattern = Vec::new();
            for _ in 0..1 + next(7) {
                pattern.extend_from_slice(fragments[next(fragments.len())]);
            }
            let options = PatternOptions::default()
                .braces(next(2) == 0)
                .recursive_double_star(next(4) != 0)
                .extglob(next(3) == 0)
                .match_hidden(next(2) == 0)
                .case_insensitive(next(4) == 0)
                .escape(next(2) == 0);
            let Ok(mut filtered) = Pattern::compile(&pattern, options) else {
                continue;
            };
            filtered.strip_engines(true, false, false);
            let mut unfiltered = filtered.clone();
            unfiltered.strip_engines(false, false, true);
            searched += usize::from(
                filtered
                    .alternatives
                    .iter()
                    .any(|alternative| alternative.prefilter.interior.is_some()),
            );
            for _ in 0..32 {
                let mut path = Vec::new();
                for _ in 0..next(9) {
                    path.extend_from_slice(path_pieces[next(path_pieces.len())]);
                }
                let shown = || {
                    format!(
                        "{:?} against {:?} under {options:?}",
                        String::from_utf8_lossy(&pattern),
                        String::from_utf8_lossy(&path)
                    )
                };
                assert_eq!(
                    filtered.is_match(&path),
                    unfiltered.is_match(&path),
                    "is_match: {}",
                    shown()
                );
                assert_eq!(
                    filtered.is_match_path(&path),
                    unfiltered.is_match_path(&path),
                    "is_match_path: {}",
                    shown()
                );
                assert_eq!(
                    filtered.is_match_glob_path(&path),
                    unfiltered.is_match_glob_path(&path),
                    "is_match_glob_path: {}",
                    shown()
                );
                // The walker's separator-crossing reading asks the `./`-free
                // copies under the pattern's own options.
                assert_eq!(
                    filtered.is_match_crossing_path(&path),
                    unfiltered.is_match_crossing_path(&path),
                    "is_match_crossing_path: {}",
                    shown()
                );
            }
        }
        assert!(
            searched > 500,
            "only {searched} generated patterns carry an interior search"
        );
    }

    /// Asserts that the sweep engine and the memoized matcher agree on every
    /// candidate, under every entry point, and that the compiled dispatch
    /// answers the same.
    fn assert_sweep_agrees(pattern: &[u8], options: PatternOptions, candidates: &[Vec<u8>]) {
        let compiled = Pattern::compile(pattern, options).expect("differential pattern compiles");
        let mut sweep_only = compiled.clone();
        sweep_only.strip_engines(true, false, false);
        let mut memoized = sweep_only.clone();
        memoized.strip_engines(false, true, true);
        for candidate in candidates {
            let name = String::from_utf8_lossy(pattern);
            let shown = String::from_utf8_lossy(candidate);
            assert_eq!(
                sweep_only.is_match(candidate),
                memoized.is_match(candidate),
                "is_match diverges for {name:?} against {shown:?} under {options:?}"
            );
            assert_eq!(
                sweep_only.is_match_path(candidate),
                memoized.is_match_path(candidate),
                "is_match_path diverges for {name:?} against {shown:?} under {options:?}"
            );
            assert_eq!(
                sweep_only.is_match_glob_path(candidate),
                memoized.is_match_glob_path(candidate),
                "is_match_glob_path diverges for {name:?} against {shown:?} under {options:?}"
            );
            assert!(
                compiled.engines_agree(candidate),
                "an engine diverges for {name:?} against {shown:?} under {options:?}"
            );
        }
    }

    #[test]
    fn sweep_engine_agrees_with_the_memoized_matcher_over_exhaustive_byte_words() {
        // Both letter cases, the hidden-file dot, and the separator: the four
        // byte roles the sweep tables distinguish.
        let words = byte_words(b"aB./", 4);
        let patterns: &[&[u8]] = &[
            b"*",
            b"?",
            b"a",
            b"*a",
            b"a*",
            b"*a*",
            b"a*B",
            b"*.*",
            b"a*a*a*B",
            b"?a?",
            b"[aB]",
            b"[!a]*",
            b"[.]a",
            b"[a-b]*",
            b"[[:alpha:]]?",
            b"*/",
            b"/*",
            b"a/*",
            b"*/a",
            b"a/?",
            b"a/??",
            b"**",
            b"**/",
            b"a/**",
            b"**/a",
            b"a/**/B",
            b"**/*.a",
            b"a/?*B",
            b"./a/*",
            b"\\*a",
            b"a\\/B",
            b"\\.a",
        ];
        for &pattern in patterns {
            for recursive in [false, true] {
                for match_hidden in [false, true] {
                    for case_insensitive in [false, true] {
                        let options = PatternOptions::default()
                            .recursive_double_star(recursive)
                            .match_hidden(match_hidden)
                            .case_insensitive(case_insensitive);
                        assert_sweep_agrees(pattern, options, &words);
                    }
                }
            }
        }
    }

    #[test]
    fn sweep_engine_agrees_for_brace_alternatives_over_exhaustive_byte_words() {
        let words = byte_words(b"aB./", 4);
        for &pattern in [b"{a,B}*".as_slice(), b"{a/**,B?}", b"*.{a,B}"].iter() {
            for match_hidden in [false, true] {
                let options = PatternOptions::default()
                    .braces(true)
                    .recursive_double_star(true)
                    .match_hidden(match_hidden);
                assert_sweep_agrees(pattern, options, &words);
            }
        }
    }

    #[test]
    fn sweep_engine_agrees_over_randomized_patterns_and_paths() {
        // A multiplicative generator keeps the search reproducible without a
        // random-number crate. The fragments cover every token kind the sweep
        // encodes, and the paths lean on the bytes with special roles.
        let fragments: &[&[u8]] = &[
            b"a", b"B", b".", b"/", b"*", b"?", b"**", b"[aB]", b"[!a]", b"[.-b]", b"{a,B}", b"\\*",
        ];
        let path_bytes = b"aB./";
        let mut seed = 0x0123_4567_89AB_CDEF_u64;
        let mut next = move |bound: usize| {
            seed = seed.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(1);
            (usize::try_from(seed >> 33).expect("31 bits fit a usize")) % bound
        };
        for _ in 0..2_000 {
            let mut pattern = Vec::new();
            for _ in 0..next(8) {
                pattern.extend_from_slice(fragments[next(fragments.len())]);
            }
            let options = PatternOptions::default()
                .braces(next(2) == 0)
                .recursive_double_star(next(2) == 0)
                .match_hidden(next(2) == 0)
                .case_insensitive(next(2) == 0)
                .escape(next(2) == 0);
            let Ok(compiled) = Pattern::compile(&pattern, options) else {
                continue;
            };
            for _ in 0..24 {
                let mut path = Vec::new();
                for _ in 0..next(12) {
                    path.push(path_bytes[next(path_bytes.len())]);
                }
                assert!(
                    compiled.engines_agree(&path),
                    "engines diverge for {:?} against {:?} under {options:?}",
                    String::from_utf8_lossy(&pattern),
                    String::from_utf8_lossy(&path)
                );
            }
        }
    }

    #[test]
    fn sweep_engine_is_present_exactly_where_the_general_path_needs_it() {
        let starred = Pattern::compile("a*a*B", PatternOptions::default()).unwrap();
        assert!(starred.alternatives[0].sweep.is_some());

        // Literal and deterministic shapes never reach the general path.
        let literal = Pattern::compile("src/main.rs", PatternOptions::default()).unwrap();
        assert!(literal.alternatives[0].sweep.is_none());
        let deterministic = Pattern::compile("src/[ab]?.rs", PatternOptions::default()).unwrap();
        assert!(deterministic.alternatives[0].sweep.is_none());

        // An extglob program keeps its own interpreter.
        let extglob = Pattern::compile("@(a|b)*", PatternOptions::default().extglob(true)).unwrap();
        assert!(extglob.alternatives[0].sweep.is_none());

        // Past one machine word the wide sweep stays responsible, and the
        // retained memoized oracle still answers alike.
        let oversized_source = [b"a".repeat(70), b"*B".to_vec()].concat();
        let oversized = Pattern::compile(&oversized_source, PatternOptions::default()).unwrap();
        assert!(oversized.alternatives[0].sweep.is_some());
        let candidate = [b"a".repeat(70), b"xB".to_vec()].concat();
        assert!(oversized.is_match(&candidate));
        assert!(oversized.engines_agree(&candidate));
    }

    #[test]
    fn sweeps_behind_a_complete_fast_path_are_built_only_where_they_run() {
        // A starred fast path that also answers under the component policy
        // wins every top-level dispatch, so its engine is not built there.
        let options = PatternOptions::walker();
        for source in ["*.rs", "**/*.rs", "src/**/*.rs"] {
            let pattern = Pattern::compile(source, options).unwrap();
            let alternative = &pattern.alternatives[0];
            assert!(
                alternative
                    .fast_path
                    .as_ref()
                    .is_some_and(FastPath::supports_component_wildcards),
                "{source} keeps its complete fast path"
            );
            assert!(alternative.sweep.is_none(), "{source} builds no engine");
            assert!(sweep_is_deferred(alternative));
            // The differential oracle still gets its engine on demand.
            let mut sweep_only = pattern.clone();
            sweep_only.strip_engines(true, false, false);
            assert!(sweep_only.alternatives[0].sweep.is_some());
            for path in ["lib.rs", "src/lib.rs", "src/.x.rs", "src/lib.ts"] {
                assert!(pattern.engines_agree(path), "{source} against {path}");
            }
        }

        // An extglob arm's prefix scan runs the engine whatever its fast path
        // is, so the arm keeps one.
        let extglob = Pattern::compile("*(*.rs|x)", options).unwrap();
        let program = extglob.alternatives[0].extglob.as_ref().unwrap();
        let arm = &program.groups[0].alternatives[0].compiled[0];
        assert!(matches!(arm.fast_path, Some(FastPath::StarSuffix { .. })));
        assert!(arm.sweep.is_some());

        // Budget use does not depend on whether the engine was built: the
        // same pattern charges as much as one whose engine is.
        let mut deferred = IrBudget::new();
        Pattern::compile_within(
            b"*.rs",
            options,
            &mut deferred,
            &mut ProvenanceBudget::new(),
            None,
            false,
            ComponentBounds::WHOLE,
        )
        .unwrap();
        let mut built = IrBudget::new();
        let tokens = [Token::Star, Token::Literal(b".rs".to_vec())];
        built.charge(tokens.len(), 0).unwrap();
        SweepEngine::compile(&tokens, options, &mut built).unwrap();
        assert_eq!(deferred.remaining, built.remaining);
    }

    #[test]
    fn list_filter_copies_share_their_originals_engine() {
        // `docs/*.md` needs a component copy for `is_match_path`; with the
        // same tokens the copy reads its original's tables instead of
        // building its own.
        let pattern = Pattern::compile("docs/*.md", PatternOptions::walker()).unwrap();
        let filter = pattern.path_filter.as_ref().expect("a component copy");
        let (Some(original), Some(copy)) =
            (&pattern.alternatives[0].sweep, &filter.compiled[0].sweep)
        else {
            panic!("both run the sweep");
        };
        assert!(original.shares_tables_with(copy));
        assert!(pattern.is_match_path("docs/a.md"));
        assert!(!pattern.is_match_path("docs/a/b.md"));

        // A removed `./` changes the tokens, so that copy builds its own.
        let dotted = Pattern::compile("./docs/*.md", PatternOptions::walker()).unwrap();
        let filter = dotted.path_filter.as_ref().expect("a stripped copy");
        assert!(filter.compiled[0].sweep.is_some());
        assert!(dotted.is_match_glob_path("docs/a.md"));
    }

    #[test]
    fn wide_sweep_agrees_across_machine_word_boundaries() {
        let prefixes = [
            ["a".repeat(62), "***B".to_owned()].concat(),
            ["*a".repeat(40), "*B".to_owned()].concat(),
            ["a/".repeat(32), "**/B".to_owned()].concat(),
            ["[aB]?".repeat(33), "*".to_owned()].concat(),
        ];
        let mut candidates = Vec::new();
        for prefix in ["a".repeat(62), "a".repeat(80), "a/".repeat(32)] {
            for suffix in byte_words(b"aB./", 3) {
                candidates.push([prefix.as_bytes(), suffix.as_slice()].concat());
            }
        }
        for source in &prefixes {
            for match_hidden in [false, true] {
                let options = PatternOptions::default()
                    .recursive_double_star(true)
                    .match_hidden(match_hidden);
                assert_sweep_agrees(source.as_bytes(), options, &candidates);
            }
        }
    }

    #[test]
    fn sweep_engine_keeps_the_terminal_recursive_star_end_of_path_case() {
        let options = PatternOptions::default().recursive_double_star(true);
        // `src/**` accepts `src` itself; `src/**/` does not, because the
        // recursive-prefix form still demands the separator.
        assert_sweep_agrees(
            b"src/**",
            options,
            &[
                b"src".to_vec(),
                b"src/".to_vec(),
                b"sr".to_vec(),
                b"src/a".to_vec(),
                b"src/.a".to_vec(),
            ],
        );
        assert_sweep_agrees(
            b"src/**/",
            options,
            &[b"src".to_vec(), b"src/".to_vec(), b"src/a/".to_vec()],
        );
        let terminal = Pattern::compile("src/**", options).unwrap();
        assert!(terminal.is_match("src"));
        let prefixed = Pattern::compile("src/**/", options).unwrap();
        assert!(!prefixed.is_match("src"));
    }

    fn byte_words(alphabet: &[u8], max_length: usize) -> Vec<Vec<u8>> {
        let mut words = vec![Vec::new()];
        let mut current = vec![Vec::new()];
        for _ in 0..max_length {
            let mut next = Vec::new();
            for prefix in current {
                for &byte in alphabet {
                    let mut word = prefix.clone();
                    word.push(byte);
                    next.push(word);
                }
            }
            words.extend(next.iter().cloned());
            current = next;
        }
        words
    }

    /// Which reach an ordinary wildcard has in [`reference_glob_match`].
    #[derive(Clone, Copy, Debug)]
    enum ReferenceReach {
        /// `is_match`: every ordinary wildcard crosses separators.
        Crossing,
        /// `is_match_path`: only a wildcard directly behind a `/` stays local.
        PathFilter,
        /// `is_match_glob_path`: every ordinary wildcard stays local.
        GlobPath,
    }

    /// A deliberately naive specification of `**` as a whole path component
    /// (#419), written against the pattern bytes rather than the token IR.
    ///
    /// The generated alphabet has no braces, classes, escapes or extglobs. A
    /// run of exactly two stars bounded by `/` or a pattern end on both sides
    /// is a globstar: before a `/` it consumes zero or more whole components,
    /// each with its separator; at the end it consumes the rest of the path,
    /// and a `/**` suffix also accepts the path that stops before its `/`.
    /// Every other star is an ordinary one-byte-at-a-time wildcard. Without
    /// `match_hidden` nothing but a literal consumes the period that starts a
    /// component, and an ordinary star does not stop right before one.
    fn reference_glob_match(
        pattern: &[u8],
        path: &[u8],
        recursive: bool,
        reach: ReferenceReach,
        match_hidden: bool,
    ) -> bool {
        fn component_start(path: &[u8], index: usize) -> bool {
            index == 0 || path[index - 1] == b'/'
        }
        fn hidden_at(path: &[u8], index: usize, match_hidden: bool) -> bool {
            !match_hidden && path.get(index) == Some(&b'.') && component_start(path, index)
        }
        fn globstar_at(pattern: &[u8], index: usize) -> bool {
            pattern.get(index..index + 2) == Some(b"**")
                && pattern.get(index + 2) != Some(&b'*')
                && (index == 0 || pattern[index - 1] == b'/')
                && pattern.get(index + 2).is_none_or(|&byte| byte == b'/')
        }
        #[allow(clippy::too_many_arguments)]
        fn go(
            pattern: &[u8],
            path: &[u8],
            p: usize,
            s: usize,
            recursive: bool,
            reach: ReferenceReach,
            match_hidden: bool,
            memo: &mut std::collections::HashMap<(usize, usize), bool>,
        ) -> bool {
            if let Some(&known) = memo.get(&(p, s)) {
                return known;
            }
            let local = match reach {
                ReferenceReach::Crossing => false,
                ReferenceReach::GlobPath => true,
                // The `/` that ends a globstar prefix belongs to the prefix,
                // so the wildcard behind it is not behind an explicit
                // separator.
                ReferenceReach::PathFilter => {
                    p > 0
                        && pattern[p - 1] == b'/'
                        && !(recursive && p >= 3 && globstar_at(pattern, p - 3))
                }
            };
            let mut recurse = |p, s| go(pattern, path, p, s, recursive, reach, match_hidden, memo);
            let result = if p == pattern.len() {
                s == path.len()
            } else if recursive && globstar_at(pattern, p) {
                if p + 2 == pattern.len() {
                    // The rest of the path, through no hidden component start.
                    (s..path.len()).all(|index| !hidden_at(path, index, match_hidden))
                } else {
                    // Zero directories, or one whole visible component more.
                    recurse(p + 3, s) || {
                        let end = path[s..]
                            .iter()
                            .position(|&byte| byte == b'/')
                            .map(|offset| s + offset);
                        end.is_some_and(|end| {
                            !hidden_at(path, s, match_hidden) && recurse(p, end + 1)
                        })
                    }
                }
            } else {
                match pattern[p] {
                    b'*' => {
                        let stop = !hidden_at(path, s, match_hidden) && recurse(p + 1, s);
                        stop || (s < path.len()
                            && !(local && path[s] == b'/')
                            && !hidden_at(path, s, match_hidden)
                            && recurse(p, s + 1))
                    }
                    b'?' => {
                        s < path.len()
                            && !(local && path[s] == b'/')
                            && !hidden_at(path, s, match_hidden)
                            && recurse(p + 1, s + 1)
                    }
                    b'/' if recursive
                        && s == path.len()
                        && p + 3 == pattern.len()
                        && globstar_at(pattern, p + 1) =>
                    {
                        true
                    }
                    byte => path.get(s) == Some(&byte) && recurse(p + 1, s + 1),
                }
            };
            memo.insert((p, s), result);
            result
        }
        go(
            pattern,
            path,
            0,
            0,
            recursive,
            reach,
            match_hidden,
            &mut std::collections::HashMap::new(),
        )
    }

    /// Every engine against the whole-component specification above, over
    /// every short pattern the alphabet spells (#419).
    #[test]
    fn every_engine_reads_double_star_as_a_whole_component_over_exhaustive_patterns() {
        // Four path bytes reach two nested components (`a/a/a`); the pattern
        // alphabet spells every globstar position up to five bytes, with `?`
        // up to four to keep the debug-build run short.
        let paths = byte_words(b"a./", 4);
        let mut patterns = byte_words(b"a*/.", 5);
        patterns.extend(byte_words(b"a*?/.", 4));
        patterns.sort_unstable();
        patterns.dedup();
        let mut compared = 0_usize;
        for pattern in patterns {
            // Longer runs keep their inherited token shape; the specification
            // above does not model them, and dedicated cases pin them.
            if pattern.windows(3).any(|window| window == b"***") {
                continue;
            }
            for recursive in [false, true] {
                for match_hidden in [false, true] {
                    let options = PatternOptions::default()
                        .recursive_double_star(recursive)
                        .match_hidden(match_hidden);
                    let compiled = Pattern::compile(&pattern, options).expect("pattern compiles");
                    let mut sweep_only = compiled.clone();
                    sweep_only.strip_engines(true, false, false);
                    let mut memoized = compiled.clone();
                    memoized.strip_engines(true, true, true);
                    for path in &paths {
                        let expected = [
                            ReferenceReach::Crossing,
                            ReferenceReach::PathFilter,
                            ReferenceReach::GlobPath,
                        ]
                        .map(|reach| {
                            reference_glob_match(&pattern, path, recursive, reach, match_hidden)
                        });
                        for (engine, matcher) in [
                            ("compiled", &compiled),
                            ("sweep", &sweep_only),
                            ("memoized", &memoized),
                        ] {
                            let actual = [
                                matcher.is_match(path),
                                // The path entry points ignore a leading `./`,
                                // which the specification does not model.
                                if path.starts_with(b"./") || pattern.starts_with(b"./") {
                                    expected[1]
                                } else {
                                    matcher.is_match_path(path)
                                },
                                if path.starts_with(b"./") || pattern.starts_with(b"./") {
                                    expected[2]
                                } else {
                                    matcher.is_match_glob_path(path)
                                },
                            ];
                            assert_eq!(
                                actual,
                                expected,
                                "{engine} engine: {:?} against {:?} (is_match, is_match_path, \
                                 is_match_glob_path) under {options:?}",
                                String::from_utf8_lossy(&pattern),
                                String::from_utf8_lossy(path),
                            );
                        }
                        compared += 1;
                    }
                }
            }
        }
        assert!(
            compared > 500_000,
            "the sweep compared only {compared} pairs"
        );
    }

    #[test]
    fn only_a_whole_component_star_run_compiles_to_recursive_tokens() {
        let options = PatternOptions::default()
            .recursive_double_star(true)
            .braces(true);
        let tokens = |source: &str| {
            Pattern::compile(source, options)
                .expect("pattern compiles")
                .alternatives
                .into_iter()
                .map(|alternative| alternative.tokens)
                .collect::<Vec<_>>()
        };
        let literal = |bytes: &[u8]| Token::Literal(bytes.to_vec());
        assert_eq!(
            tokens("**/x"),
            [vec![Token::RecursivePrefix, literal(b"x")]]
        );
        assert_eq!(
            tokens("a/**"),
            [vec![literal(b"a"), Token::Separator, Token::RecursiveStar]]
        );
        assert_eq!(
            tokens("a**/x"),
            [vec![
                literal(b"a"),
                Token::Star,
                Token::Star,
                Token::Separator,
                literal(b"x"),
            ]]
        );
        assert_eq!(
            tokens("**x"),
            [vec![Token::Star, Token::Star, literal(b"x")]]
        );
        assert_eq!(
            tokens("x/**y"),
            [vec![
                literal(b"x"),
                Token::Separator,
                Token::Star,
                Token::Star,
                literal(b"y")
            ]]
        );
        // An escaped separator is a literal byte and bounds nothing.
        assert_eq!(
            tokens(r"a\/**"),
            [vec![literal(b"a/"), Token::Star, Token::Star]]
        );
        // Runs longer than two keep their inherited whole-component shape.
        assert_eq!(
            tokens("***/x"),
            [vec![
                Token::RecursiveStar,
                Token::Star,
                Token::Separator,
                literal(b"x"),
            ]]
        );
        // Each brace alternative is judged as the whole pattern it expands to.
        assert_eq!(
            tokens("{**,a**}/x"),
            [
                vec![Token::RecursivePrefix, literal(b"x")],
                vec![
                    literal(b"a"),
                    Token::Star,
                    Token::Star,
                    Token::Separator,
                    literal(b"x"),
                ],
            ]
        );
        // With the option off every run is ordinary, whatever its position.
        let ordinary = Pattern::compile("**/x", PatternOptions::default()).unwrap();
        assert_eq!(
            ordinary.alternatives[0].tokens,
            [Token::Star, Token::Star, Token::Separator, literal(b"x")]
        );
    }

    #[test]
    fn extglob_alternatives_inherit_the_component_position_of_their_group() {
        let options = PatternOptions::default()
            .recursive_double_star(true)
            .extglob(true);
        let alternative_tokens = |source: &str| {
            let compiled = Pattern::compile(source, options).expect("pattern compiles");
            let program = compiled.alternatives[0]
                .extglob
                .as_ref()
                .expect("an extglob program")
                .clone();
            program.groups[0]
                .alternatives
                .iter()
                .map(|alternative| alternative.compiled[0].tokens.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(alternative_tokens("@(**)/y")[0], [Token::RecursiveStar]);
        assert_eq!(alternative_tokens("a/@(**)")[0], [Token::RecursiveStar]);
        assert_eq!(
            alternative_tokens("x@(**)/y")[0],
            [Token::Star, Token::Star]
        );
        assert_eq!(alternative_tokens("@(**)y")[0], [Token::Star, Token::Star]);
        assert_eq!(
            alternative_tokens("x@(**/y|z)")[0],
            [
                Token::Star,
                Token::Star,
                Token::Separator,
                Token::Literal(b"y".to_vec())
            ]
        );
        assert_eq!(
            alternative_tokens("@(y/**|z)x")[0],
            [
                Token::Literal(b"y".to_vec()),
                Token::Separator,
                Token::Star,
                Token::Star
            ]
        );

        // The separator-crossing reading for groups whose alternatives cross,
        // and the filesystem-glob reading where an attached run must not.
        for (source, candidate, crossing, expected) in [
            ("@(**)/y", "a/b/y", true, true),
            ("@(**)/y", "a/b/y", false, true),
            ("@(**/x|z)", "a/b/x", false, true),
            ("x@(**)/y", "xa/b/y", false, false),
            ("x@(**)/y", "xa/y", false, true),
            ("@(**/x|z)", "sx", true, false),
            ("@(**/x|z)", "sx", false, false),
            ("@(**/x|z)", "a/b/x", true, true),
            ("+(**/x)", "a/x/b/x", true, true),
            ("+(**/x)", "a/sx", true, false),
            ("**/@(x)", "sx", true, false),
            ("**/@(x)", "a/x", false, true),
            ("a**/@(y)", "ay", true, false),
            ("a**/@(y)", "ab/y", false, true),
            ("a**/@(y)", "a/b/y", false, false),
        ] {
            let compiled = Pattern::compile(source, options).expect("pattern compiles");
            let actual = if crossing {
                compiled.is_match(candidate)
            } else {
                compiled.is_match_glob_path(candidate)
            };
            assert_eq!(actual, expected, "{source} against {candidate}");
            assert!(
                compiled.engines_agree(candidate),
                "{source} against {candidate}: engines disagree"
            );
        }
    }

    #[test]
    fn scoped_suffix_set_nested_prefixes_and_shared_suffixes() {
        let options = PatternOptions::default()
            .braces(true)
            .recursive_double_star(true);
        let pattern = Pattern::compile("{a,a\\/.b}/**/*.{rs,ts}", options).expect("compiles");
        assert!(matches!(
            pattern.alternative_fast_path.as_deref(),
            Some(AlternativeFastPath::ScopedSuffixSet(_))
        ));
        for (path, expected) in [
            ("a/.b/x.rs", true),
            ("a/.b/dir/x.ts", true),
            ("a/.c/x.rs", false),
            ("a/x.rs", true),
            ("a/.b/.c/x.rs", false),
            ("a/.b/.rs", false),
        ] {
            assert_eq!(pattern.is_match_glob_path(path), expected, "path {path:?}");
            assert!(pattern.engines_agree(path), "engines disagree on {path:?}");
        }

        // Shared suffixes where a shorter suffix leaves a hidden component in
        // the middle while a longer suffix absorbs it (`{src,lib}/**/*.{gz,tar.gz}`).
        let shared_ext = Pattern::compile("{src,lib}/**/*.{gz,tar.gz}", options).expect("compiles");
        assert!(matches!(
            shared_ext.alternative_fast_path.as_deref(),
            Some(AlternativeFastPath::ScopedSuffixSet(_))
        ));
        for (path, expected) in [
            ("src/a.gz", true),
            ("src/a.tar.gz", true),
            ("lib/sub/pkg.tar.gz", true),
            ("src/.gz", false),
            ("src/.tar.gz", false),
            ("src/.hidden/a.tar.gz", false),
            ("lib/a.zip", false),
        ] {
            assert_eq!(
                shared_ext.is_match_glob_path(path),
                expected,
                "shared_ext path {path:?}"
            );
            assert!(
                shared_ext.engines_agree(path),
                "shared_ext engines disagree on {path:?}"
            );
        }

        // Longer literal suffix absorbing a hidden component that blocked a
        // shorter suffix (`{src,lib}/**/*{z,p\/.h.z}`).
        let hidden_absorbing_suffix =
            Pattern::compile(r"{src,lib}/**/*{z,p\/.h.z}", options).expect("compiles");
        assert!(matches!(
            hidden_absorbing_suffix.alternative_fast_path.as_deref(),
            Some(AlternativeFastPath::ScopedSuffixSet(_))
        ));
        for (path, expected) in [
            ("src/p/.h.z", true),
            ("lib/sub/p/.h.z", true),
            ("src/q/.h.z", false),
            ("src/.hidden/p/.h.z", false),
        ] {
            assert_eq!(
                hidden_absorbing_suffix.is_match_glob_path(path),
                expected,
                "hidden_absorbing_suffix path {path:?}"
            );
            assert!(
                hidden_absorbing_suffix.engines_agree(path),
                "hidden_absorbing_suffix engines disagree on {path:?}"
            );
        }

        // Case-insensitive nested prefixes and shared suffixes.
        let ci = Pattern::compile("{a,a\\/.B}/**/*.{RS,ts}", options.case_insensitive(true))
            .expect("compiles");
        assert!(ci.is_match_glob_path("A/.b/x.rs"));
        assert!(!ci.is_match_glob_path("A/.c/x.rs"));
        assert!(ci.engines_agree("A/.b/x.rs"));

        // Spill path beyond SCOPED_SUFFIX_INLINE_PREFIXES (16 nested prefixes).
        let mut prefixes = Vec::new();
        let mut current = String::from("p");
        prefixes.push(current.clone());
        for i in 0..20 {
            current.push_str(&format!("\\/.h{i}"));
            prefixes.push(current.clone());
        }
        let spill_source = format!("{{{}}}/**/*.{{rs,ts}}", prefixes.join(","));
        let spill = Pattern::compile(&spill_source, options).expect("spill pattern compiles");
        assert!(matches!(
            spill.alternative_fast_path.as_deref(),
            Some(AlternativeFastPath::ScopedSuffixSet(_))
        ));
        let deep_path = format!(
            "p/{}/leaf.rs",
            (0..20)
                .map(|i| format!(".h{i}"))
                .collect::<Vec<_>>()
                .join("/")
        );
        assert!(spill.is_match_glob_path(&deep_path));
        assert!(spill.engines_agree(&deep_path));
        let deep_miss = format!("{deep_path}.nope");
        assert!(!spill.is_match_glob_path(&deep_miss));
        assert!(spill.engines_agree(&deep_miss));

        // Short-circuit scan bounds: non-matching suffix stops at shortest
        // prefix without walking deeper nested prefixes, and a clean single-
        // start match stops at the shortest matching suffix without scanning
        // deeper suffix branches.
        let shared =
            Pattern::compile("{z,a,a\\/b\\/c\\/d}/**/*.{ts,rs,foo.rs}", options).expect("compiles");
        reset_scoped_suffix_set_scan_stats();
        assert!(!shared.is_match_glob_path("a/b/c/d/file.txt"));
        let (prefix_steps, suffix_steps, hidden_bytes) = scoped_suffix_set_scan_stats();
        assert_eq!(
            prefix_steps, 1,
            "suffix miss must abort after shortest prefix"
        );
        assert!(suffix_steps <= 2, "suffix miss must fail quickly");
        assert_eq!(
            hidden_bytes, 0,
            "suffix miss must not scan for hidden components"
        );

        reset_scoped_suffix_set_scan_stats();
        let long_mid = format!("a/{}bar.foo.rs", "x/".repeat(64));
        assert!(shared.is_match_glob_path(&long_mid));
        let (prefix_steps, suffix_steps, _) = scoped_suffix_set_scan_stats();
        assert_eq!(prefix_steps, 1, "clean match must stop at shortest prefix");
        assert_eq!(
            suffix_steps, 3,
            "clean single-start match must return at `.rs` without walking `.foo.rs`"
        );
    }

    #[test]
    fn extglob_outer_star_backtrack_and_group_hidden_stop_guard() {
        let options = PatternOptions::walker();

        // Outer-star and group-star dotfile backtracking parity with plain patterns.
        for (extglob_src, plain_src, candidates) in [
            ("*.@(x)", "*.x", &["a/.x", "a/b.x", ".x", "b.x"][..]),
            ("a*.@(x)", "a*.x", &["a/.x", "a/b.x", "a..x", "ab/.x"][..]),
            (
                "@(a*).@(x)",
                "a*.x",
                &["a/.x", "a/b.x", "a..x", "ab/.x"][..],
            ),
            (
                "*?.*@(x)",
                "*?.*x",
                &["a.b/.c.x", "a.b/c.x", "a.b/.x", "a.b/.xx", "a/.x"][..],
            ),
            (
                "*?.*.@(rs|keep.rs)",
                "*?.*.{rs,keep.rs}",
                &["dir.name/.keep.rs", "dir.name/.rs", "dir.name/file.rs"][..],
            ),
            (
                "dir/*?.*.@(x)*",
                "dir/*?.*.x*",
                &["dir/a.b/.x.x", "dir/a.b/file.x", "dir/a.b/.y"][..],
            ),
            (
                "*?.*@(*).x",
                "*?.**.x",
                &["a.b/.y/.x", "a.b/y/.x", "a.b/y/z.x", "a.b/.x"][..],
            ),
            (
                "**/.a*/.a/@(.a)",
                "**/.a*/.a/.a",
                &[
                    "a.a/.a/.a",
                    ".a/.a/.a",
                    "dir/.a/.a/.a",
                    "dir/a.a/.a/.a",
                    ".h/.a/.a/.a",
                    "dir/.h/.a/.a/.a",
                ][..],
            ),
            (
                "x/**/.a*/.a/@(.a)",
                "x/**/.a*/.a/.a",
                &[
                    "x/a.a/.a/.a",
                    "x/.a/.a/.a",
                    "x/dir/.a/.a/.a",
                    "x/.h/.a/.a/.a",
                ][..],
            ),
            (r"a\/@(*)", r"a\/*", &["a/x/y", "a/x"][..]),
            (
                "**/?.*.*@(txt)",
                "**/?.*.*txt",
                &["a.//.conf.txt", "a/.conf.txt", "a/b.c.txt"][..],
            ),
            ("**/?@(txt)", "**/?txt", &["a//txt", "a/btxt", "/txt"][..]),
        ] {
            let extglob = Pattern::compile(extglob_src, options).expect("extglob compiles");
            let plain = Pattern::compile(plain_src, options).expect("plain compiles");
            for &path in candidates {
                assert_eq!(
                    extglob.is_match(path),
                    plain.is_match(path),
                    "is_match mismatch: {extglob_src} vs {plain_src} on {path:?}"
                );
                assert_eq!(
                    extglob.is_match_path(path),
                    plain.is_match_path(path),
                    "is_match_path mismatch: {extglob_src} vs {plain_src} on {path:?}"
                );
                assert_eq!(
                    extglob.is_match_glob_path(path),
                    plain.is_match_glob_path(path),
                    "is_match_glob_path mismatch: {extglob_src} vs {plain_src} on {path:?}"
                );
                assert!(
                    extglob.engines_agree(path),
                    "engines disagree for {extglob_src} on {path:?}"
                );
            }
        }

        // Memoized `**/` retries must hand off only on component boundaries.
        let globstar_retry = Pattern::compile("**/.a*/.a/@(.a)", options).expect("compiles");
        for (path, expected) in [
            ("a.a/.a/.a", false),
            (".a/.a/.a", true),
            ("dir/.a/.a/.a", true),
            ("dir/a.a/.a/.a", false),
            (".h/.a/.a/.a", false),
            ("dir/.h/.a/.a/.a", false),
        ] {
            assert_eq!(
                globstar_retry.is_match_glob_path(path),
                expected,
                "is_match_glob_path **/.a*/.a/@(.a) on {path:?}"
            );
            assert!(globstar_retry.engines_agree(path));
        }

        // Preserve exact `main` behavior for separator-absorbing literal-dot
        // arms and trailing-elided globstar repetition arms.
        let hidden_config = Pattern::compile("@(.config/**)/settings", options).expect("compiles");
        for path in [".config/settings", ".config/apps/settings"] {
            assert!(hidden_config.is_match(path), "is_match {path:?}");
            assert!(hidden_config.is_match_path(path), "is_match_path {path:?}");
            assert!(
                hidden_config.is_match_glob_path(path),
                "is_match_glob_path {path:?}"
            );
            assert!(hidden_config.engines_agree(path));
        }
        assert!(!hidden_config.is_match_glob_path(".config/.hidden/settings"));

        // `+(a/**|/).x*` has `.` right behind the group (`bounds.ends = false`),
        // so `a/**` ends in an ordinary star run and must reject `"a/.x"` just
        // like `+(a/**|/).x` and `a/**.x*`, while accepting `"a//.x"` via the
        // `/` arm. With `/` behind the group (`+(a/**|/)/.x*`), `a/**` absorbs
        // the separator into `a/**/` and accepts `"a/.x"`.
        for src in ["+(a/**|/).x", "+(a/**|/).x*"] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(!pat.is_match("a/.x"), "{src} must reject a/.x");
            assert!(pat.is_match("a//.x"), "{src} must accept a//.x");
            assert!(pat.is_match("a/b.x"), "{src} must accept a/b.x");
            assert!(pat.engines_agree("a/.x"));
            assert!(pat.engines_agree("a//.x"));
            assert!(pat.engines_agree("a/b.x"));
        }
        for src in ["+(a/**|/)/.x", "+(a/**|/)/.x*"] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(pat.is_match("a/.x"), "{src} must accept a/.x");
            assert!(pat.is_match_path("a/.x"), "{src} must accept a/.x");
            assert!(pat.is_match_glob_path("a/.x"), "{src} must accept a/.x");
            assert!(pat.engines_agree("a/.x"));
        }

        let elided_globstar = Pattern::compile("*+(dir?/**|.f)", options).expect("compiles");
        assert!(elided_globstar.is_match("dir/.f"));
        assert!(elided_globstar.engines_agree("dir/.f"));
        assert!(elided_globstar.is_match_path("dir/.f"));
        assert!(!elided_globstar.is_match_glob_path("dir/.f"));

        // Non-retryable patterns stay on the latest-star fast path (0 memoized
        // star fallbacks); retryable hidden-dot patterns invoke it cleanly.
        for (src, path) in [
            ("*.@(x)", "a/.x"),
            ("a*.@(x)", "a/.x"),
            ("dir/*/*.@(x)", "dir/a/.x"),
        ] {
            let pattern = Pattern::compile(src, options).expect("compiles");
            let program = pattern.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program");
            assert!(!program.has_retryable_star_segment);
            reset_extglob_memoized_star_fallbacks();
            assert!(!pattern.is_match(path));
            assert_eq!(
                extglob_memoized_star_fallbacks(),
                0,
                "{src} should not invoke memoized star fallback"
            );
        }
        for common_src in ["**/*.@(js|ts)", "*(ab|xy)*", "+(*/**)/y*"] {
            let pattern = Pattern::compile(common_src, options).expect("compiles");
            let program = pattern.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program");
            assert!(
                !program.has_retryable_star_segment,
                "{common_src} must stay on the non-retryable fast path"
            );
        }

        // Escaped `\/` is a literal slash, not a separator, so a group right
        // behind it is not component-local under `is_match_path`.
        let escaped_slash_group = Pattern::compile(r"a\/@(*/)*x", options).expect("compiles");
        assert!(escaped_slash_group.is_match_path("a/b/c/x"));
        assert!(escaped_slash_group.is_match_path("a/b/c/d/x"));
        assert!(!escaped_slash_group.is_match_path("a/b/c/x/y"));
        assert!(escaped_slash_group.engines_agree("a/b/c/x"));

        // Zero-width first iterations inside component-local repetitions seed
        // later non-local iterations at offset 0 in both NFA and interpreter.
        for (src, candidate, expected_path) in [
            ("src/+(|*.txt)", "src/dir/file.txt", true),
            ("src/+(|*.txt)*", "src/dir/file.txt", true),
            ("a/@(a/|?/)*(a/|*)?", "a/a//a", true),
            ("a/+(a/|?/)*(a/|*)?", "a/a//a", true),
        ] {
            let compiled = Pattern::compile(src, options).expect("compiles");
            assert_eq!(
                compiled.is_match_path(candidate),
                expected_path,
                "is_match_path mismatch for {src} on {candidate:?}"
            );
            assert!(
                compiled.engines_agree(candidate),
                "engines disagree for {src} on {candidate:?}"
            );
        }

        // `**/` matches `Token::RecursivePrefix` and does not make a following
        // wildcard component-local under `is_match_path` (while `**/!(x)` keeps
        // its #393 component-local negation rule).
        let globstar_q = Pattern::compile("**/?.*.*@(txt)", options).expect("compiles");
        assert!(globstar_q.is_match("a.//.conf.txt"));
        assert!(globstar_q.is_match_path("a.//.conf.txt"));
        assert!(!globstar_q.is_match_glob_path("a.//.conf.txt"));
        assert!(globstar_q.engines_agree("a.//.conf.txt"));

        let globstar_q_short = Pattern::compile("**/?@(txt)", options).expect("compiles");
        assert!(globstar_q_short.is_match("a//txt"));
        assert!(globstar_q_short.is_match_path("a//txt"));
        assert!(!globstar_q_short.is_match_glob_path("a//txt"));
        assert!(globstar_q_short.engines_agree("a//txt"));

        let globstar_neg = Pattern::compile("**/!(x)", options).expect("compiles");
        assert!(!globstar_neg.is_match_path("a/b/x"));
        assert!(globstar_neg.is_match_path("a/b/y"));
        assert!(globstar_neg.engines_agree("a/b/x"));

        // A literal backslash in a pattern (`escape(false)`) is never a
        // pattern-side separator, even on Windows (ADR-0005).
        let no_escape = PatternOptions::walker().escape(false);
        let lit_bs_group = Pattern::compile(r"dir\?(tmp)*.rs", no_escape).expect("compiles");
        let lit_bs_plain = Pattern::compile(r"dir\*.rs", no_escape).expect("compiles");
        assert!(
            !lit_bs_group.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program")
                .groups[0]
                .component_local,
            "literal backslash must not mark following group component-local"
        );
        assert!(lit_bs_group.is_match_path(r"dir\sub/file.rs"));
        for candidate in [r"dir\sub/file.rs", r"dir\tmp/file.rs", r"dir\file.rs"] {
            assert_eq!(
                lit_bs_group.is_match_path(candidate),
                lit_bs_plain.is_match_path(candidate),
                "escape(false) backslash parity on {candidate:?}"
            );
            assert!(lit_bs_group.engines_agree(candidate));
        }

        // Preserve ADR-0020 syntactic boundary rules: an extglob group starts
        // at a component boundary only when its operator follows `/` or pattern
        // start in source, never dynamically behind a preceding `)`.
        for (src, rejected, accepted) in [
            ("?(tmp)@(**/)file", "file", "a/file"),
            ("@(src/)@(**/)file", "src/file", "src/a/file"),
            ("@(**/)@(**/)file", "file", "a/file"),
        ] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(!pat.is_match(rejected), "{src} is_match({rejected:?})");
            assert!(
                !pat.is_match_path(rejected),
                "{src} is_match_path({rejected:?})"
            );
            assert!(
                !pat.is_match_glob_path(rejected),
                "{src} is_match_glob_path({rejected:?})"
            );
            assert!(pat.engines_agree(rejected));
            assert!(pat.is_match(accepted), "{src} is_match({accepted:?})");
            assert!(
                pat.is_match_path(accepted),
                "{src} is_match_path({accepted:?})"
            );
            assert!(
                pat.is_match_glob_path(accepted),
                "{src} is_match_glob_path({accepted:?})"
            );
            assert!(pat.engines_agree(accepted));
        }

        // Preserve the 1.0 `is_match_path` contract (`docs/compatibility-guide.md`):
        // only a wildcard or group syntactically right after `/` is
        // component-local; a wildcard after `)` may cross `/` again.
        for (src, candidate) in [
            ("src/?(tmp)*.rs", "src/sub/file.rs"),
            ("@(dir/)*?.*", "dir/a.b/c/.d"),
            ("dir/@(|tmp)*x", "dir/a/x"),
        ] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(
                pat.is_match_path(candidate),
                "{src} must match {candidate:?} under is_match_path"
            );
            assert!(pat.engines_agree(candidate));
        }

        // When `match_hidden` is enabled and `component_wildcards` is off
        // (`is_match`), neither barrier can fire so retryable star patterns
        // stay on the non-retryable fast path.
        let hidden_ok =
            Pattern::compile("*?.*.@(rs|ts)", options.match_hidden(true)).expect("compiles");
        reset_extglob_memoized_star_fallbacks();
        assert!(!hidden_ok.is_match("dir/path.rs"));
        assert!(hidden_ok.is_match("dir/a.b.rs"));
        assert!(hidden_ok.is_match("dir.name/.rs"));
        assert_eq!(extglob_memoized_star_fallbacks(), 0);

        let retryable = Pattern::compile("*?.*.@(rs|keep.rs)", options).expect("compiles");
        reset_extglob_memoized_star_fallbacks();
        assert!(retryable.is_match("dir.name/.keep.rs"));
        assert!(
            extglob_memoized_star_fallbacks() > 0,
            "retryable hidden-dot pattern must use memoized star fallback"
        );

        // Tail handoffs from `match_extglob_task_memoized` share visited star
        // states in `state.failed`, keeping repeated `.` matches strictly linear.
        let dot_repeat = Pattern::compile("*?.*@(x)", options).expect("compiles");
        for repeats in [1024_usize, 4096] {
            let candidate = format!("{}/.y", "a.".repeat(repeats));
            reset_extglob_memoized_star_fallbacks();
            assert!(!dot_repeat.is_match(&candidate));
            assert!(
                extglob_memoized_star_fallbacks() > 0,
                "dot_repeat({repeats}) must exercise memoized star fallback"
            );
            let max_linear_visits = 3 * (candidate.len() + 1);
            assert!(
                extglob_memoized_star_visits() <= max_linear_visits,
                "tail handoffs must share visited star states linearly for n={repeats}: {} > {max_linear_visits}",
                extglob_memoized_star_visits()
            );
        }

        // Zero-width star fallthroughs inside `match_extglob_task_memoized`
        // must memoize each `(star, path_index)` state once and hand off
        // non-retryable tails back to the linear latest-star fast path.
        let chain_src = format!("*?.*{}@(x)", "a*".repeat(480));
        let chain_path = format!("a.{}/.z", "a".repeat(960));
        let chain = Pattern::compile(&chain_src, options).expect("chain compiles");
        reset_extglob_memoized_star_fallbacks();
        assert!(!chain.is_match(&chain_path));
        assert!(
            extglob_memoized_star_fallbacks() > 0,
            "chain must exercise memoized star fallback"
        );
        let max_handoff_visits = 3 * (chain_path.len() + 1);
        assert!(
            extglob_memoized_star_visits() <= max_handoff_visits,
            "non-retryable tail must hand off cleanly: {} > {max_handoff_visits}",
            extglob_memoized_star_visits()
        );

        // Even when a trailing `?.*` keeps every intermediate `a*` star inside
        // the memoized prefix, each `(star, path_index)` pair is visited once.
        let bounded_src = format!("*?.*{}?.*@(x)", "a*".repeat(64));
        let bounded_path = format!("a.{}.b/.z", "a".repeat(128));
        let bounded = Pattern::compile(&bounded_src, options).expect("bounded chain compiles");
        reset_extglob_memoized_star_fallbacks();
        assert!(!bounded.is_match(&bounded_path));
        assert!(
            extglob_memoized_star_fallbacks() > 0,
            "bounded chain must exercise memoized star fallback"
        );
        let max_bounded_visits = 67 * (bounded_path.len() + 1);
        assert!(
            extglob_memoized_star_visits() <= max_bounded_visits,
            "memoized star visits must stay polynomial: {} > {max_bounded_visits}",
            extglob_memoized_star_visits()
        );

        // Lazy fallback row initialization keeps long retryable star chains
        // fast when no retry fires.
        let long_retryable_src = format!("{}@(x)", "*?.".repeat(256));
        let long_retryable = Pattern::compile(&long_retryable_src, options).expect("compiles");
        let long_prog = long_retryable.alternatives[0]
            .extglob
            .as_ref()
            .expect("extglob program");
        assert!(long_prog.has_retryable_star_segment);
        assert!(
            long_prog.fast_memo_state_count <= 4,
            "fast_memo_state_count should stay O(groups), got {}",
            long_prog.fast_memo_state_count
        );
        assert!(!long_retryable.is_match("f"));

        // Both `SweepEngine::Wide` (>63 positions) and the true non-sweep
        // fallback (`compiled.sweep.is_none()`) must block stopping an arm's
        // trailing `*` immediately before a hidden component.
        let wide_arm = format!("*?.*@({}*).x", "?".repeat(65));
        let wide_pattern = Pattern::compile(&wide_arm, options).expect("wide compiles");
        let wide_path = format!("a.b/{}/.x", "z".repeat(65));
        let wide_ok_path = format!("a.b/{}/ok.x", "z".repeat(65));
        assert!(
            wide_pattern.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program")
                .groups[0]
                .alternatives[0]
                .compiled[0]
                .sweep
                .is_some(),
            "65-`?` arm compiles to SweepEngine::Wide"
        );
        assert!(!wide_pattern.is_match(&wide_path));
        assert!(wide_pattern.is_match(&wide_ok_path));
        assert!(wide_pattern.engines_agree(&wide_path));
        assert!(wide_pattern.engines_agree(&wide_ok_path));

        let mut non_sweep_pattern = wide_pattern.clone();
        non_sweep_pattern.strip_engines(true, true, true);
        assert!(
            non_sweep_pattern.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program")
                .groups[0]
                .alternatives[0]
                .compiled[0]
                .sweep
                .is_none(),
            "stripped pattern must exercise the non-sweep arm fallback"
        );
        assert!(!non_sweep_pattern.is_match(&wide_path));
        assert!(non_sweep_pattern.is_match(&wide_ok_path));

        // 64 outer-star segments run iteratively on a 256 KiB stack without
        // overflowing native stack frames (both accept and full-exhaustion reject).
        let deep_src = format!("{}@(x)", "*?.".repeat(64));
        let deep_match = format!("{}/.x", "a.".repeat(64));
        let deep_miss = format!("{}/.y", "a.".repeat(64));
        let handle = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let pattern = Pattern::compile(&deep_src, options).expect("deep compiles");
                assert!(pattern.is_match(&deep_match));
                assert!(!pattern.is_match(&deep_miss));
            })
            .expect("spawn small-stack thread");
        handle.join().expect("small-stack thread must not overflow");
    }

    #[test]
    fn extglob_separators_keep_escaped_slashes_literal() {
        let options = PatternOptions::walker();

        let pos = Pattern::compile("src/@(foo|bar).rs", options).expect("compiles");
        let pos_prog = pos.alternatives[0]
            .extglob
            .as_ref()
            .expect("extglob program");
        assert_eq!(
            pos_prog.steps[3],
            ExtglobStep::Separator {
                recursive_prefix: false
            }
        );
        assert!(pos_prog.positive_nfa.as_ref().is_some_and(|nfa| {
            nfa.states.iter().any(|state| {
                matches!(
                    state,
                    PositiveExtglobState::Consume {
                        matcher: PositiveExtglobMatcher::GroupSeparator,
                        ..
                    }
                )
            })
        }));
        assert!(pos.is_match("src/foo.rs"));
        assert!(pos.engines_agree("src/foo.rs"));
        assert_eq!(pos.is_match(r"src\foo.rs"), cfg!(windows));
        assert!(pos.engines_agree(r"src\foo.rs"));

        let neg = Pattern::compile("src/!(foo).rs", options).expect("compiles");
        let neg_prog = neg.alternatives[0]
            .extglob
            .as_ref()
            .expect("extglob program");
        assert_eq!(
            neg_prog.steps[3],
            ExtglobStep::Separator {
                recursive_prefix: false
            }
        );
        assert!(neg.is_match("src/bar.rs"));
        assert!(!neg.is_match("src/foo.rs"));
        assert_eq!(neg.is_match(r"src\bar.rs"), cfg!(windows));
        assert!(!neg.is_match(r"src\foo.rs"));

        let escaped = Pattern::compile(r"src\/@(foo|bar).rs", options).expect("compiles");
        let escaped_prog = escaped.alternatives[0]
            .extglob
            .as_ref()
            .expect("extglob program");
        assert_eq!(escaped_prog.steps[3], ExtglobStep::Escape { escaped: b'/' });
        assert!(escaped.is_match("src/foo.rs"));
        assert!(!escaped.is_match(r"src\foo.rs"));
        assert!(escaped.engines_agree("src/foo.rs"));
        assert!(escaped.engines_agree(r"src\foo.rs"));

        // Preserve `#393` component-local negation behind `**/`.
        let neg_after_globstar = Pattern::compile("**/!(x)", options).expect("compiles");
        assert!(!neg_after_globstar.is_match_path("src/dir/x"));
        assert!(!neg_after_globstar.is_match_path("src/x"));
        assert!(neg_after_globstar.is_match_path("src/dir/y"));
    }

    #[test]
    fn extglob_escaped_separator_before_group_matches_plain_locality() {
        let escaped_opts = PatternOptions::walker();
        let raw_opts = PatternOptions::walker().escape(false);

        // POSIX escaped slash: `a\/@(*)` matches `a\/*` under all entry points.
        for (options, expected_local, candidates) in [
            (
                escaped_opts,
                false,
                &["a/x", "a/x/y", r"a\/x", r"a\/x/y"][..],
            ),
            (raw_opts, true, &["a/x", "a/x/y", r"a\/x", r"a\/x/y"][..]),
        ] {
            let group = Pattern::compile(r"a\/@(*)", options).expect("group compiles");
            let plain = Pattern::compile(r"a\/*", options).expect("plain compiles");
            let prog = group.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program");
            assert_eq!(prog.groups[0].component_local, expected_local);
            for &path in candidates {
                assert_eq!(
                    group.is_match(path),
                    plain.is_match(path),
                    "is_match {path:?} {options:?}"
                );
                assert_eq!(
                    group.is_match_path(path),
                    plain.is_match_path(path),
                    "is_match_path {path:?} {options:?}"
                );
                assert_eq!(
                    group.is_match_glob_path(path),
                    plain.is_match_glob_path(path),
                    "is_match_glob_path {path:?} {options:?}"
                );
                assert!(group.engines_agree(path), "engines_agree {path:?}");
            }
            // Specifically pin `a\/@(*)` on `"a/x/y"` under `is_match_path`.
            if options.escape {
                assert!(group.is_match_path("a/x/y"));
                assert!(!group.is_match_glob_path("a/x/y"));
            } else {
                assert!(group.is_match_path(r"a\/x"));
                assert!(!group.is_match_path(r"a\/x/y"));
            }
        }

        // Negated group behind escaped slash vs real slash.
        let neg_escaped = Pattern::compile(r"a\/!(z)", escaped_opts).expect("compiles");
        assert!(
            !neg_escaped.alternatives[0]
                .extglob
                .as_ref()
                .expect("program")
                .groups[0]
                .component_local
        );
        assert!(neg_escaped.is_match_path("a/x/y"));
        assert!(!neg_escaped.is_match_glob_path("a/x/y"));
        assert!(neg_escaped.engines_agree("a/x/y"));

        let neg_raw = Pattern::compile(r"a\/!(z)", raw_opts).expect("compiles");
        assert!(
            neg_raw.alternatives[0]
                .extglob
                .as_ref()
                .expect("program")
                .groups[0]
                .component_local
        );
        assert!(neg_raw.is_match_path(r"a\/x"));
        assert!(!neg_raw.is_match_path(r"a\/x/y"));
        assert!(neg_raw.engines_agree(r"a\/x/y"));

        // Escaped backslash followed by an unescaped slash IS a separator.
        let escaped_bs_then_slash = Pattern::compile(r"a\\/@(*)", escaped_opts).expect("compiles");
        assert!(
            escaped_bs_then_slash.alternatives[0]
                .extglob
                .as_ref()
                .expect("program")
                .groups[0]
                .component_local
        );
        assert!(escaped_bs_then_slash.is_match_path(r"a\/x"));
        assert!(!escaped_bs_then_slash.is_match_path(r"a\/x/y"));
        assert!(escaped_bs_then_slash.engines_agree(r"a\/x/y"));

        // Escaped backslash (`a\\@(*)`) vs `a\\*` on Windows-style paths: never
        // marks the following group component-local on any platform.
        for options in [escaped_opts, raw_opts] {
            let group = Pattern::compile(r"a\\@(*)", options).expect("group compiles");
            let plain = Pattern::compile(r"a\\*", options).expect("plain compiles");
            let prog = group.alternatives[0]
                .extglob
                .as_ref()
                .expect("extglob program");
            assert!(!prog.groups[0].component_local);
            for path in [r"a\x", r"a\x\y", r"a\\x", r"a\\x\y", r"a\x/y"] {
                assert_eq!(
                    group.is_match(path),
                    plain.is_match(path),
                    "is_match {path:?} {options:?}"
                );
                assert_eq!(
                    group.is_match_path(path),
                    plain.is_match_path(path),
                    "is_match_path {path:?} {options:?}"
                );
                assert_eq!(
                    group.is_match_glob_path(path),
                    plain.is_match_glob_path(path),
                    "is_match_glob_path {path:?} {options:?}"
                );
                assert!(group.engines_agree(path), "engines_agree {path:?}");
            }
            if options.escape {
                assert!(group.is_match_path(r"a\x\y"));
            } else {
                assert!(group.is_match_path(r"a\\x\y"));
            }
        }
    }

    #[test]
    fn extglob_repeated_globstar_before_separator_hands_off_to_later_iterations() {
        let options = PatternOptions::walker();
        let assert_both_engines = |src: &str, path: &str, expected: bool| {
            let pat = Pattern::compile(src, options).expect("pattern compiles");
            assert_eq!(
                [
                    pat.is_match(path),
                    pat.is_match_path(path),
                    pat.is_match_glob_path(path),
                ],
                [expected; 3],
                "{src} against {path:?}"
            );
            assert!(
                pat.engines_agree(path),
                "engines disagree: {src} on {path:?}"
            );
        };

        // `*(**|.h)/x` and `*(**|.h)/x*` (NFA and interpreter).
        for src in ["*(**|.h)/x", "*(**|.h)/x*"] {
            for (path, expected) in [
                ("x", true),
                ("/x", true),
                ("a/x", true),
                ("a/b/x", true),
                (".h/x", true),
                ("a/.h/x", true),
                ("a/b/.h/x", true),
                // `.h` ends mid-component (or before `/`), so a leading `**/`
                // arm cannot start right after `.h`.
                (".h/b/x", false),
                ("a/.h/b/x", false),
                ("a/.hx/x", false),
                (".h/.h/x", false),
                (".a/x", false),
                ("a/.b/x", false),
                (".a/.h/x", false),
                ("a/.h/.b/x", false),
                (".a.h/x", false),
                ("a/.b.h/x", false),
                // `"a.h"` is an ordinary non-hidden directory, so `**/x` accepts it.
                ("a.h/x", true),
            ] {
                assert_both_engines(src, path, expected);
            }
        }

        // `+(**|.h)/x` and `+(**|.h)/x*`: 1 iteration of `**` + `/` = `**/x`,
        // which also accepts `"x"` per #422 (`+(**)/a` against `"a"`).
        for src in ["+(**|.h)/x", "+(**|.h)/x*"] {
            for (path, expected) in [
                ("x", true),
                ("/x", true),
                ("a/x", true),
                ("a/b/x", true),
                (".h/x", true),
                ("a/.h/x", true),
                ("a/b/.h/x", true),
                (".h/b/x", false),
                ("a/.h/b/x", false),
                ("a/.hx/x", false),
                (".a/x", false),
                ("a/.b/x", false),
                (".a/.h/x", false),
                (".a.h/x", false),
                ("a/.b.h/x", false),
            ] {
                assert_both_engines(src, path, expected);
            }
        }

        // Leading-`**/` arms inside repeated groups may only start at a
        // component boundary (ADR-0020): `+(**|.git)/config` must reject
        // `"repo/.github/config"` and `"repo/.git/.git/config"`, while
        // `+(**|.git/)/config` accepts `"repo/.git/.git/config"` because
        // `.git/` ends at a separator.
        for src in ["+(**|.git)/config", "+(**|.git)/config*"] {
            assert_both_engines(src, "repo/.git/config", true);
            assert_both_engines(src, "repo/.github/config", false);
            assert_both_engines(src, "repo/.git/.git/config", false);
            assert_both_engines(src, ".git/config", true);
            assert_both_engines(src, ".github/config", false);
        }
        for src in ["+(**|.git/)/config", "+(**|.git/)/config*"] {
            assert_both_engines(src, "repo/.git/config", true);
            assert_both_engines(src, "repo/.git/.git/config", true);
            assert_both_engines(src, "repo/.git/sub/config", true);
            assert_both_engines(src, "repo/.github/config", false);
        }
        // Regular (non-`through_separator`) arms starting with `**/` must also
        // obey the component-start guard inside repetitions (`#495`).
        for src in ["+(**|.git|**/a)/config", "+(**|.git|**/a)/config*"] {
            assert_both_engines(src, "repo/.github/a/config", false);
            assert_both_engines(src, "repo/.git/a/config", false);
            assert_both_engines(src, "repo/.git/config", true);
            assert_both_engines(src, "repo/a/config", true);
        }
        for src in ["+(**|.git/|**/a)/config", "+(**|.git/|**/a)/config*"] {
            assert_both_engines(src, "repo/.git/a/config", true);
            assert_both_engines(src, "repo/.github/a/config", false);
        }
        for src in ["+(**|.h?)/config", "+(**|.h?)/config*"] {
            let pat = Pattern::compile(src, options).expect("compiles");
            // Under `is_match`, `.h?` can consume `.h/` and land at a component
            // start; under `is_match_glob_path`, `?` stays inside one component.
            assert!(pat.is_match(".h/b/config"));
            assert!(!pat.is_match_glob_path(".h/b/config"));
            assert!(!pat.is_match(".hxb/config"));
            assert!(!pat.is_match_glob_path(".hxb/config"));
            assert!(pat.engines_agree(".h/b/config"));
            assert!(pat.engines_agree(".hxb/config"));
        }

        // `+(a/**|.h)/x` and `+(a/**|.h)/x*` (and `*(a/**|.h)/x`).
        for src in ["+(a/**|.h)/x", "+(a/**|.h)/x*"] {
            for (path, expected) in [
                ("x", false),
                ("/x", false),
                ("a/x", true),
                ("a/b/x", true),
                (".h/x", true),
                ("a/.h/x", true),
                ("a/b/.h/x", true),
                ("a/b/c/.h/x", true),
                ("a/.h/b/x", false),
                ("a.h/x", false),
                ("a/.b.h/x", false),
                ("a/.b/x", false),
                (".a/.h/x", false),
            ] {
                assert_both_engines(src, path, expected);
            }
        }
        for src in ["*(a/**|.h)/x", "*(a/**|.h)/x*"] {
            assert_both_engines(src, "/x", true);
            assert_both_engines(src, "x", false);
            assert_both_engines(src, "a/.h/x", true);
            assert_both_engines(src, "a/b/.h/x", true);
            assert_both_engines(src, "a.h/x", false);
        }

        // `+(**)/a` and `+(****|.h)/x` preserve whole-component boundaries.
        for src in ["+(**)/a", "+(**)/a*"] {
            assert_both_engines(src, "a", true);
            assert_both_engines(src, "b/c/a", true);
            assert_both_engines(src, "xa", false);
            assert_both_engines(src, ".h/a", false);
        }
        for src in ["+(****|.h)/x", "+(****|.h)/x*"] {
            assert_both_engines(src, "a/.h/x", true);
            assert_both_engines(src, "a/b/.h/x", true);
            assert_both_engines(src, ".a.h/x", false);
            assert_both_engines(src, "a/.b.h/x", false);
        }

        // Trailing `/**` still refuses bare `"a"` on `+(a/**)/**` (as `a/**/**` does).
        for src in ["+(a/**)/**", "+(a/**)/**/*"] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(!pat.is_match_glob_path("a"), "{src} must reject bare a");
            assert!(pat.engines_agree("a"));
        }
        assert_both_engines("+(a/**|.h)/**", "a", false);
        assert_both_engines("+(a/**|.h)/**", "a/.h", true);
        assert_both_engines("+(a/**|.h)/**", "a/b/.h", true);
        assert_both_engines("+(a/**|.h)/**", "a/.b", false);

        // Component-local first iteration (`first_iteration_apart`) under `is_match_path`:
        // first iteration's `*` stays in one component, later iteration's `*` crosses.
        for src in ["dir/+(*.rs/**|.h)/x", "dir/+(*.rs/**|.h)/x*"] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(pat.is_match_path("dir/b.rs/.h/x"));
            assert!(!pat.is_match_path("dir/a/b.rs/.h/x"));
            assert!(pat.is_match("dir/a/b.rs/.h/x"));
            assert!(!pat.is_match_glob_path("dir/a/b.rs/.h/x"));
            assert!(pat.engines_agree("dir/b.rs/.h/x"));
            assert!(pat.engines_agree("dir/a/b.rs/.h/x"));
        }
        for src in ["dir/+(a/|*.rs/**|.h)/x", "dir/+(a/|*.rs/**|.h)/x*"] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(pat.is_match_path("dir/a/b/c.rs/.h/x"));
            assert!(!pat.is_match_glob_path("dir/a/b/c.rs/.h/x"));
            assert!(pat.engines_agree("dir/a/b/c.rs/.h/x"));
        }
        for src in ["dir/+(**|.h|*.rs)/x", "dir/+(**|.h|*.rs)/x*"] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(pat.is_match_path("dir/a/b.rs/x"));
            assert!(pat.is_match_glob_path("dir/a/.h/x"));
            assert!(pat.engines_agree("dir/a/b.rs/x"));
            assert!(pat.engines_agree("dir/a/.h/x"));
        }
    }

    #[test]
    fn extglob_repeated_through_separator_stays_linear() {
        let options = PatternOptions::walker();

        for (src, path) in [
            ("+(*/**)/y*", format!("dir/{}", "x".repeat(4_000))),
            ("+(*/**)/y*", format!("dir/{}", "x".repeat(8_000))),
            ("+(*/**)/y*", format!("dir/{}", "x/".repeat(2_000))),
            ("+(*/**|x)/y*", format!("dir/{}", "x".repeat(4_000))),
            ("dir/+(*/**|x)/y*", format!("dir/{}", "x".repeat(4_000))),
        ] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(
                pat.alternatives[0]
                    .extglob
                    .as_ref()
                    .expect("program")
                    .positive_nfa
                    .is_none(),
                "{src} must run the interpreter"
            );
            for name in ["is_match", "is_match_path", "is_match_glob_path"] {
                super::reset_extglob_repetition_sweep_steps();
                let matched = match name {
                    "is_match" => pat.is_match(&path),
                    "is_match_path" => pat.is_match_path(&path),
                    "is_match_glob_path" => pat.is_match_glob_path(&path),
                    _ => unreachable!(),
                };
                assert!(!matched, "{src} {name}");
                let steps = super::extglob_repetition_sweep_steps();
                assert!(
                    steps <= 3 * path.len(),
                    "{src} {name}: {steps} sweep steps for {} bytes (must stay linear)",
                    path.len()
                );
            }
        }

        // A dead wide sweep inside `matching_extglob_repetition_ends` must
        // stop advancing immediately instead of stepping across the rest of a
        // long nonmatching path, and its `SweepState::Wide` buffer must stay
        // retained across calls when below the retention cap. Using a leading
        // `.` keeps this `through_separator` repetition non-idempotent under
        // `match_hidden: false` so it runs the repetition sweep.
        let wide_src = format!("+(.{}/**)/z*", "a".repeat(4_095));
        let wide_pat = Pattern::compile(&wide_src, options).expect("wide compiles");
        let nonmatch = format!("b{}", "x".repeat(4_096));
        super::reset_extglob_repetition_sweep_steps();
        assert!(!wide_pat.is_match(&nonmatch));
        assert_eq!(
            super::extglob_repetition_sweep_steps(),
            1,
            "dead wide sweep must exit after the first mismatching byte"
        );
        let retained_wide = super::EXTGLOB_SCRATCH.with(|cell| {
            cell.borrow()
                .wide_sweeps
                .first()
                .map_or(0, SweepEngine::retained_state_capacity)
        });
        assert!(
            retained_wide > 0,
            "wide repetition sweep state must be retained across calls"
        );
        assert!(!wide_pat.is_match(&nonmatch));
        let retained_wide_after = super::EXTGLOB_SCRATCH.with(|cell| {
            cell.borrow()
                .wide_sweeps
                .first()
                .map_or(0, SweepEngine::retained_state_capacity)
        });
        assert_eq!(retained_wide_after, retained_wide);

        // Running a huge-arm repetition must bound retained sweep slots, and
        // subsequent non-repetition matches (`a@(b)x*`) must skip sweep cleanup
        // completely.
        let many_arms = (0..4_000)
            .map(|i| format!("a{i}*"))
            .collect::<Vec<_>>()
            .join("|");
        let huge_rep =
            Pattern::compile(format!("+({many_arms})y*"), options).expect("4000-arm rep compiles");
        assert!(!huge_rep.is_match("zzz"));
        let non_rep = Pattern::compile("a@(b)x*", options).expect("non-rep compiles");
        super::reset_extglob_sweep_cleanup_scans();
        assert!(non_rep.is_match("abx"));
        assert!(!non_rep.is_match("acy"));
        assert_eq!(
            super::extglob_sweep_cleanup_scans(),
            0,
            "matches without repetition sweeps must skip sweep cleanup"
        );

        // Exceeding `RETAINED_WIDE_SWEEP_SLOTS` (64) with actual wide arms
        // (`> 64` positions each) must truncate retained `wide_sweeps` back to
        // at most `RETAINED_WIDE_SWEEP_SLOTS`.
        let seventy_wide_arms = (0..70)
            .map(|i| format!("{}a{i}*", "b".repeat(70)))
            .collect::<Vec<_>>()
            .join("|");
        let seventy_wide = Pattern::compile(format!("+({seventy_wide_arms})y*"), options)
            .expect("70 wide arms compile");
        assert!(!seventy_wide.is_match("zzz"));
        let wide_slots_after_seventy =
            super::EXTGLOB_SCRATCH.with(|cell| cell.borrow().wide_sweeps.len());
        assert!(
            wide_slots_after_seventy <= super::RETAINED_WIDE_SWEEP_SLOTS,
            "retained wide sweep slots must stay capped after 70 wide arms: {wide_slots_after_seventy}"
        );

        // Idempotent `through_separator` repetitions (`+(**)/x*`,
        // `*(**|****)/x*`, `+(a/a/.../a/**)/z*`) bypass both the interpreter
        // repetition bitmap and the NFA loop-back edge, while arms that can
        // explicitly match a hidden component under `match_hidden: false` keep
        // the loop-back so repeated hidden prefixes still match (#495).
        let fifty_dirs = format!("a/{}x", "b/".repeat(50));
        for src in ["+(**)/x*", "*(**)/x*", "+(**|****)/x*"] {
            let pat = Pattern::compile(src, options).expect("sole globstar compiles");
            super::reset_extglob_repetition_sweep_steps();
            assert!(pat.is_match_glob_path(&fifty_dirs));
            assert_eq!(
                super::extglob_repetition_sweep_steps(),
                0,
                "{src} must bypass the multi-pass repetition bitmap"
            );
            assert!(pat.engines_agree(&fifty_dirs));
        }

        let deep_a_pat = format!("+({}**)/z*", "a/".repeat(32));
        let deep_a_path = format!("{}z", "a/".repeat(250));
        let deep_a = Pattern::compile(&deep_a_pat, options).expect("deep visible /** compiles");
        super::reset_extglob_repetition_sweep_steps();
        assert!(deep_a.is_match_glob_path(&deep_a_path));
        assert_eq!(
            super::extglob_repetition_sweep_steps(),
            0,
            "visible through_separator repetition must bypass the multi-pass repetition bitmap"
        );
        assert!(deep_a.engines_agree(&deep_a_path));
        let deep_a_hidden = format!("{}a/.h/z", "a/".repeat(32));
        assert!(!deep_a.is_match_glob_path(&deep_a_hidden));
        assert!(deep_a.engines_agree(&deep_a_hidden));

        // Hidden-component `through_separator` arms must keep the loop-back
        // when `match_hidden: false` so second iterations can match `.h/`.
        for (src, accept, reject) in [
            ("+(.h/**)/z", ".h/.h/z", ".h/.x/z"),
            ("+(a/.h/**)/z", "a/.h/a/.h/z", "a/.h/.h/z"),
            (r"+(a\/.h/**)/z", "a/.h/a/.h/z", "a/.h/.x/z"),
            ("+(?.h/**)/z", "/.h//.h/z", "/.h/.x/z"),
            ("+(a[!x].h/**)/z", "a/.h/a/.h/z", "a/.h/.x/z"),
        ] {
            let pat = Pattern::compile(src, options).expect("hidden through_separator compiles");
            assert!(
                pat.is_match(accept),
                "{src} must accept repeated hidden prefix {accept:?}"
            );
            assert!(!pat.is_match(reject), "{src} must reject {reject:?}");
            assert!(pat.engines_agree(accept), "{src} engines_agree({accept:?})");
            assert!(pat.engines_agree(reject), "{src} engines_agree({reject:?})");
        }
        // Leading `[.]` and `*.h` (which blocks stopping before a leading `.`)
        // cannot match a hidden period under `match_hidden: false`, and all
        // engines agree.
        for (src, path) in [("+([.]h/**)/z", ".h/.h/z"), ("+(*.h/**)/z", "a/.h/a/.h/z")] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(!pat.is_match(path), "{src} must reject {path:?}");
            assert!(pat.engines_agree(path), "{src} engines_agree({path:?})");
        }

        // Separate patterns populating different `wide_sweeps` slots with large
        // wide arms must stay within one combined `RETAINED_SCRATCH_WORDS`
        // budget on the thread (`#495`).
        let small_wide = format!("{}*", "a".repeat(64));
        let large_wide = format!("{}*", "b".repeat(20_000));
        for slot in 0..64 {
            let mut arms: Vec<&str> = std::iter::repeat_n(small_wide.as_str(), slot).collect();
            arms.push(large_wide.as_str());
            let pat = Pattern::compile(format!("+({})z*", arms.join("|")), options)
                .expect("wide pattern compiles");
            assert!(!pat.is_match("nope"));
        }
        let total_retained_wide_words: usize = super::EXTGLOB_SCRATCH.with(|cell| {
            cell.borrow()
                .wide_sweeps
                .iter()
                .map(SweepEngine::retained_state_capacity)
                .sum()
        });
        assert!(
            total_retained_wide_words <= super::RETAINED_SCRATCH_WORDS,
            "combined retained wide sweep words must stay within budget: {total_retained_wide_words}"
        );
    }

    #[test]
    fn contains_extglob_skips_closed_bracket_classes() {
        let walker = PatternOptions::walker();
        let no_escape = PatternOptions::walker().escape(false);

        // Closed bracket classes containing extglob-like bytes must not
        // trigger `CompiledExtglob`.
        for (src, options, matching, non_matching) in [
            ("*.[@(]", walker, "foo.@", "foo.a"),
            ("src/[@(]/foo", walker, "src/(/foo", "src/a/foo"),
            ("[!@(]", walker, "x", "@"),
            ("[^@(]", walker, "x", "("),
            ("[]@(]", walker, "]", "x"),
            ("[!]@(]", walker, "x", "]"),
            ("[!(a)]", walker, "x", "a"),
            ("src/[a-z@(]/*.rs", walker, "src/@/lib.rs", "src/A/lib.rs"),
            ("[[:alpha:]@(]", walker, "z", "9"),
            (r"[\]@(a)]", walker, "]", "x"),
            (r"\[@(a)]", no_escape, r"\@", r"\x"),
        ] {
            assert!(
                !super::contains_extglob(src.as_bytes(), options.escape),
                "{src:?} ({options:?}) should not detect extglob"
            );
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(
                pat.alternatives[0].extglob.is_none(),
                "{src:?} should not compile an extglob program"
            );
            assert!(pat.is_match(matching), "{src:?} should match {matching:?}");
            assert!(
                !pat.is_match(non_matching),
                "{src:?} should reject {non_matching:?}"
            );
            assert!(pat.engines_agree(matching));
            assert!(pat.engines_agree(non_matching));
        }

        // Unclosed `[` or real extglobs outside a class must still be detected.
        for (src, escapes) in [
            ("a[@(b)", true),
            ("a[@(b)", false),
            (r"\[@(a)", true),
            (r"\[@(a)", false),
            (r"\[@(a)]", true),
            ("[a]@(b)", true),
            ("[a-z]@(foo|bar)", true),
            ("[[:alpha:]]@(a)", true),
            (r"[\]@(a)", false),
            (r"[\]@(a)", true),
            (r"a[\\]@(b)", true),
            ("[]@(a)", true),
        ] {
            assert!(
                super::contains_extglob(src.as_bytes(), escapes),
                "{src:?} (escape={escapes}) must detect extglob"
            );
        }

        for (src, options, matching, non_matching) in [
            (r"\[@(a)", walker, "[a", "[b"),
            (r"\[@(a)]", walker, "[a]", "[b]"),
            ("[a]@(b)", walker, "ab", "ac"),
            ("[a-z]@(foo|bar)", walker, "xbar", "xbaz"),
            ("[[:alpha:]]@(a)", walker, "za", "9a"),
            (r"[\]@(a)", no_escape, r"\a", r"\b"),
            (r"a[\\]@(b)", walker, r"a\b", r"a\c"),
        ] {
            let pat = Pattern::compile(src, options).expect("compiles");
            assert!(
                pat.alternatives[0].extglob.is_some(),
                "{src:?} must compile an extglob program"
            );
            assert!(pat.is_match(matching), "{src:?} should match {matching:?}");
            assert!(
                !pat.is_match(non_matching),
                "{src:?} should reject {non_matching:?}"
            );
            assert!(pat.engines_agree(matching));
            assert!(pat.engines_agree(non_matching));
        }

        // Twelve `{a,b}` groups + `*` + 90 literals + `/[@(]` moves from
        // `CompiledExtglob` to plain `PathFilter` copies that share their
        // `Arc`-backed wide sweep tables; `compile_sweep` must not double-
        // charge those shared tables (#495).
        let braced_wide_class = format!("{}*{}/[@(]", "{a,b}".repeat(12), "x".repeat(90));
        let wide_compiled = Pattern::compile(&braced_wide_class, PatternOptions::walker())
            .expect("shared PathFilter wide sweeps must not double-charge IR budget");
        assert!(
            wide_compiled
                .alternatives
                .iter()
                .all(|alternative| alternative.extglob.is_none()),
            "braced bracket class with `@(` must not compile extglob programs"
        );
        let wide_accept = format!("{}x{}/@", "a".repeat(12), "x".repeat(90));
        let wide_reject = format!("{}x{}/z", "a".repeat(12), "x".repeat(90));
        assert!(wide_compiled.is_match(&wide_accept));
        assert!(wide_compiled.is_match_path(&wide_accept));
        assert!(wide_compiled.is_match_glob_path(&wide_accept));
        assert!(!wide_compiled.is_match(&wide_reject));
        assert!(!wide_compiled.is_match_path(&wide_reject));
        assert!(!wide_compiled.is_match_glob_path(&wide_reject));
        assert!(wide_compiled.engines_agree(&wide_accept));
        assert!(wide_compiled.engines_agree(&wide_reject));

        // Even when the optional wide sweep tables alone would exceed the
        // remaining `IrBudget` (4096 alternatives × ~205-byte wide sweeps),
        // `compile_sweep` degrades cleanly to the token matcher instead of
        // rejecting the valid pattern (#495).
        let braced_huge_wide_class = format!("{}*{}/[@(]", "{a,b}".repeat(12), "x".repeat(190));
        let huge_wide_compiled =
            Pattern::compile(&braced_huge_wide_class, PatternOptions::walker())
                .expect("optional wide sweep overflow must fall back to token matcher");
        assert!(
            huge_wide_compiled
                .alternatives
                .iter()
                .all(|alternative| alternative.extglob.is_none()),
            "braced 190-literal bracket class must not compile extglob programs"
        );
        assert!(
            huge_wide_compiled
                .alternatives
                .last()
                .expect("4096 alternatives")
                .sweep
                .is_none(),
            "tail alternatives beyond IrBudget must fall back to the token matcher"
        );
        for prefix in ["a".repeat(12), "b".repeat(12)] {
            let huge_accept = format!("{prefix}x{}/@", "x".repeat(190));
            let huge_reject = format!("{prefix}x{}/z", "x".repeat(190));
            assert!(huge_wide_compiled.is_match(&huge_accept));
            assert!(huge_wide_compiled.is_match_path(&huge_accept));
            assert!(huge_wide_compiled.is_match_glob_path(&huge_accept));
            assert!(!huge_wide_compiled.is_match(&huge_reject));
            assert!(!huge_wide_compiled.is_match_path(&huge_reject));
            assert!(!huge_wide_compiled.is_match_glob_path(&huge_reject));
            assert!(huge_wide_compiled.engines_agree(&huge_accept));
            assert!(huge_wide_compiled.engines_agree(&huge_reject));
        }

        // Complete `FastPath` arms (`StarSuffix`) in a multi-arm brace
        // expansion never allocate a `SweepEngine`, so they must not deduct
        // unused sweep budget and starve sweep-backed sibling arms regardless
        // of branch order (#495).
        let star_suffix_arm = format!("*{}{}", "{a,b}".repeat(11), "r".repeat(500));
        let sweep_arm = format!("{}[xy]*", "?*".repeat(1_000));
        let fast_first = Pattern::compile(
            format!("{{{star_suffix_arm},{sweep_arm}}}"),
            PatternOptions::walker(),
        )
        .expect("fast-path-first brace pattern compiles");
        let sweep_first = Pattern::compile(
            format!("{{{sweep_arm},{star_suffix_arm}}}"),
            PatternOptions::walker(),
        )
        .expect("sweep-first brace pattern compiles");
        assert!(
            fast_first
                .alternatives
                .last()
                .expect("sweep arm")
                .sweep
                .is_some(),
            "sweep arm must retain its SweepEngine when StarSuffix arms come first"
        );
        assert!(
            sweep_first
                .alternatives
                .first()
                .expect("sweep arm")
                .sweep
                .is_some(),
            "sweep arm must retain its SweepEngine when it comes first"
        );
        let reject_2k_a = "a".repeat(2_000);
        assert!(!fast_first.is_match(&reject_2k_a));
        assert!(!sweep_first.is_match(&reject_2k_a));
        let accept_sweep = format!("{}x", "a".repeat(1_000));
        assert!(fast_first.is_match(&accept_sweep));
        assert!(sweep_first.is_match(&accept_sweep));
        assert!(fast_first.engines_agree(&accept_sweep));
        assert!(sweep_first.engines_agree(&accept_sweep));

        // Same branch-order guarantee through `PathFilter` copies when the
        // sweep arm has a leading `./` prefix (`is_match_path`, #495).
        let dotted_sweep_arm = format!("./{sweep_arm}");
        let dotted_fast_first = Pattern::compile(
            format!("{{{star_suffix_arm},{dotted_sweep_arm}}}"),
            PatternOptions::walker(),
        )
        .expect("dotted fast-path-first brace pattern compiles");
        let dotted_sweep_first = Pattern::compile(
            format!("{{{dotted_sweep_arm},{star_suffix_arm}}}"),
            PatternOptions::walker(),
        )
        .expect("dotted sweep-first brace pattern compiles");
        assert!(
            dotted_fast_first
                .path_filter
                .as_ref()
                .expect("path_filter exists")
                .compiled
                .last()
                .expect("dotted sweep copy")
                .sweep
                .is_some(),
            "dotted sweep arm copy must retain its SweepEngine when StarSuffix arms come first"
        );
        assert!(
            dotted_sweep_first
                .path_filter
                .as_ref()
                .expect("path_filter exists")
                .compiled
                .first()
                .expect("dotted sweep copy")
                .sweep
                .is_some(),
            "dotted sweep arm copy must retain its SweepEngine when it comes first"
        );
        assert!(!dotted_fast_first.is_match_path(&reject_2k_a));
        assert!(!dotted_sweep_first.is_match_path(&reject_2k_a));
        assert!(dotted_fast_first.is_match_path(&accept_sweep));
        assert!(dotted_sweep_first.is_match_path(&accept_sweep));
        assert!(dotted_fast_first.engines_agree(&accept_sweep));
        assert!(dotted_sweep_first.engines_agree(&accept_sweep));

        // Dedicated 40 MiB retained-byte budget rejects oversized multi-arm
        // literal/prefilter expansions without stealing `IrBudget` sweep units
        // from sweep-backed arms, while zero-copy `FastPath::{LiteralTokens,
        // DeterministicTokens, StaticStar}` avoid doubling `Vec<Token>` memory
        // (#495).
        let huge_star_suffix =
            format!("{{./anchor,*{}{}}}", "{a,b}".repeat(11), "r".repeat(12_000));
        assert_eq!(
            Pattern::compile(&huge_star_suffix, PatternOptions::walker())
                .unwrap_err()
                .message(),
            super::TOO_MUCH_COMPILED_IR
        );
        let huge_plain_literal = format!(
            "{{./anchor,}}{}*a*{}[x]",
            "{a,b}".repeat(11),
            "z".repeat(6_000)
        );
        assert_eq!(
            Pattern::compile(&huge_plain_literal, PatternOptions::walker())
                .unwrap_err()
                .message(),
            super::TOO_MUCH_COMPILED_IR
        );

        // Zero-copy `DeterministicTokens` and `StaticStar` stay within 40 MiB
        // and compile cleanly.
        let det_braces = format!("{}{}[x]", "{a,b}".repeat(12), "ab?".repeat(80));
        let det_compiled = Pattern::compile(&det_braces, PatternOptions::walker())
            .expect("zero-copy DeterministicTokens brace pattern compiles within 40 MiB");
        assert!(matches!(
            det_compiled.alternatives[0].fast_path,
            Some(FastPath::DeterministicTokens)
        ));
        let det_accept = format!("{}{}x", "b".repeat(12), "abc".repeat(80));
        assert!(det_compiled.is_match(&det_accept));
        assert!(det_compiled.engines_agree(&det_accept));

        let static_star_braces = format!("{}*a{}", "{a,b}".repeat(12), "/a".repeat(110));
        let static_star_compiled = Pattern::compile(&static_star_braces, PatternOptions::walker())
            .expect("zero-copy StaticStar brace pattern compiles within 40 MiB");
        assert!(matches!(
            static_star_compiled.alternatives[0].fast_path,
            Some(FastPath::StaticStar { .. })
        ));
        let static_star_accept = format!("{}xa{}", "b".repeat(12), "/a".repeat(110));
        assert!(static_star_compiled.is_match(&static_star_accept));
        assert!(static_star_compiled.engines_agree(&static_star_accept));

        // Sweep-backed brace arms keep 100% of their `IrBudget` sweep units
        // (neither `{a,b}×12 + a*×55 + [x]` nor `{a,b}×11 * z×350 * a*×10 [xy]?`
        // loses any `SweepEngine`, #495).
        let braced_sweeps_src = format!("{}{}[x]", "{a,b}".repeat(12), "a*".repeat(55));
        let braced_sweeps = Pattern::compile(&braced_sweeps_src, PatternOptions::walker())
            .expect("4096 sweep-backed arms compile");
        assert_eq!(braced_sweeps.alternatives.len(), 4_096);
        assert!(
            braced_sweeps
                .alternatives
                .iter()
                .all(|alternative| alternative.sweep.is_some()),
            "every sweep-backed arm (including the last `bbbbbbbbbbbb` arm) must keep its SweepEngine"
        );
        let tail_reject = format!("{}{}y", "b".repeat(12), "a".repeat(10_000));
        assert!(!braced_sweeps.is_match(&tail_reject));
        let tail_accept = format!("{}{}x", "b".repeat(12), "a".repeat(60));
        assert!(braced_sweeps.is_match(&tail_accept));
        assert!(braced_sweeps.engines_agree(&tail_accept));

        let wide_sweeps_src = format!(
            "{}*{}*{}[xy]?",
            "{a,b}".repeat(11),
            "z".repeat(350),
            "a*".repeat(10)
        );
        let wide_sweeps = Pattern::compile(&wide_sweeps_src, PatternOptions::walker())
            .expect("2048 wide sweep-backed arms compile");
        assert_eq!(wide_sweeps.alternatives.len(), 2_048);
        assert!(
            wide_sweeps
                .alternatives
                .iter()
                .all(|alternative| alternative.sweep.is_some()),
            "all 2,048 wide sweep-backed arms must keep their SweepEngine"
        );
        let wide_sweeps_reject = format!(
            "{}{}{}bb",
            "b".repeat(11),
            "z".repeat(350),
            "a".repeat(1_000)
        );
        assert!(!wide_sweeps.is_match(&wide_sweeps_reject));
        let wide_sweeps_accept =
            format!("{}{}{}xb", "b".repeat(11), "z".repeat(350), "a".repeat(10));
        assert!(wide_sweeps.is_match(&wide_sweeps_accept));
        assert!(wide_sweeps.engines_agree(&wide_sweeps_accept));

        // Escaped `\}` bytes inside a skipped bracket class must be subtracted
        // from `remaining_closing_braces` so subsequent unmatched `{` bytes do
        // not overflow `MAX_UNCONDITIONAL_BRACE_DEPTH` and disable the pre-
        // expansion IR lower-bound rejection (#495).
        let escaped_close_class = format!(
            "{}[{}]{}@(x){}",
            "{a,b}".repeat(9),
            r"\}".repeat(1_026),
            "{".repeat(1_026),
            "a".repeat(50_000)
        );
        let parse_options = PatternOptions {
            braces: false,
            ..PatternOptions::walker()
        };
        assert_eq!(
            super::unconditional_compiled_units(escaped_close_class.as_bytes(), parse_options),
            Some((0, true)),
            "skipped escaped `\\}}` bytes must not leave phantom remaining closing braces"
        );
        assert_eq!(
            Pattern::compile(&escaped_close_class, PatternOptions::walker())
                .unwrap_err()
                .message(),
            super::TOO_MUCH_COMPILED_IR
        );

        // Many unclosed `[` bytes must still scan in linear time.
        let unclosed_many = "[".repeat(20_000);
        assert!(!super::contains_extglob(unclosed_many.as_bytes(), true));
        let unclosed_with_extglob = format!("{unclosed_many}@(a)");
        assert!(super::contains_extglob(
            unclosed_with_extglob.as_bytes(),
            true
        ));
    }
}

#[cfg(test)]
mod escape_tests {
    use super::{Pattern, PatternOptions, escape, escape_str};

    /// Every byte that is syntax somewhere, next to ordinary bytes that are
    /// path structure (`.`, `/`), case-foldable, class-only syntax (`-`, `^`),
    /// or not UTF-8.
    const ALPHABET: &[u8] = b"\\*?[]{}(),|!@+-^./aAbZ: \x00\xc3\xa9\xff";

    /// The reproducible generator the other randomized tests here use.
    fn generator(mut seed: u64) -> impl FnMut(usize) -> usize {
        move |bound: usize| {
            seed = seed.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(1);
            (usize::try_from(seed >> 33).expect("31 bits fit a usize")) % bound
        }
    }

    /// Every option combination with escaping on: 32 of them.
    fn escaping_options() -> Vec<PatternOptions> {
        (0..32_u8)
            .map(|bits| {
                PatternOptions::default()
                    .braces(bits & 1 != 0)
                    .recursive_double_star(bits & 2 != 0)
                    .extglob(bits & 4 != 0)
                    .match_hidden(bits & 8 != 0)
                    .case_insensitive(bits & 16 != 0)
            })
            .collect()
    }

    fn random_text(next: &mut impl FnMut(usize) -> usize, max_len: usize) -> Vec<u8> {
        (0..next(max_len + 1))
            .map(|_| ALPHABET[next(ALPHABET.len())])
            .collect()
    }

    /// The text itself and near misses: one byte inserted, removed, replaced,
    /// or case-swapped, a `./` added or removed, and unrelated text.
    fn candidates(text: &[u8], next: &mut impl FnMut(usize) -> usize) -> Vec<Vec<u8>> {
        let mut candidates = vec![text.to_vec(), [b"./", text].concat()];
        if let Some(rest) = text.strip_prefix(b"./") {
            candidates.push(rest.to_vec());
        }
        for _ in 0..12 {
            let mut candidate = text.to_vec();
            let at = next(candidate.len() + 1);
            match next(4) {
                0 => candidate.insert(at, ALPHABET[next(ALPHABET.len())]),
                1 if at < candidate.len() => {
                    candidate.remove(at);
                }
                2 if at < candidate.len() => candidate[at] = ALPHABET[next(ALPHABET.len())],
                3 if at < candidate.len() => candidate[at] ^= 0x20,
                _ => candidate = random_text(next, 6),
            }
            candidates.push(candidate);
        }
        candidates
    }

    fn folded(bytes: &[u8], options: PatternOptions) -> Vec<u8> {
        if options.case_insensitive {
            bytes.to_ascii_lowercase()
        } else {
            bytes.to_vec()
        }
    }

    /// The one leading `./` the path entry points ignore on both sides.
    fn without_dot_slash(bytes: &[u8]) -> &[u8] {
        bytes.strip_prefix(b"./").unwrap_or(bytes)
    }

    /// `escape(text)` compiles under every option combination with escaping on
    /// and matches `text` and nothing else, where "the same" is what the entry
    /// point compares: whole bytes for `is_match`, bytes after one leading
    /// `./` for the path entry points, ASCII-folded under `case_insensitive`.
    ///
    /// On Windows a candidate path may use `\` as a separator, so a text with
    /// a backslash is not a Windows path and those cases are left to POSIX.
    #[test]
    fn escaped_text_matches_exactly_itself_under_every_option() {
        let options = escaping_options();
        let mut next = generator(0x0E5C_A9ED_7E57_0001);
        for _ in 0..1_500 {
            let text = random_text(&mut next, 10);
            let escaped = escape(&text);
            if let Ok(utf8) = std::str::from_utf8(&text) {
                assert_eq!(escape_str(utf8).as_bytes(), escaped, "{utf8:?}");
            }
            let candidates = candidates(&text, &mut next);
            for &options in &options {
                let shown = || format!("{:?} under {options:?}", String::from_utf8_lossy(&text));
                let pattern = Pattern::compile(&escaped, options)
                    .unwrap_or_else(|error| panic!("{error}: {}", shown()));
                for candidate in &candidates {
                    if cfg!(windows) && (text.contains(&b'\\') || candidate.contains(&b'\\')) {
                        continue;
                    }
                    let whole = folded(candidate, options) == folded(&text, options);
                    let path = folded(without_dot_slash(candidate), options)
                        == folded(without_dot_slash(&text), options);
                    let candidate_shown = String::from_utf8_lossy(candidate);
                    assert_eq!(
                        pattern.is_match(candidate),
                        whole,
                        "is_match({candidate_shown:?}): {}",
                        shown()
                    );
                    assert_eq!(
                        pattern.is_match_path(candidate),
                        path,
                        "is_match_path({candidate_shown:?}): {}",
                        shown()
                    );
                    assert_eq!(
                        pattern.is_match_glob_path(candidate),
                        path,
                        "is_match_glob_path({candidate_shown:?}): {}",
                        shown()
                    );
                }
            }
        }
    }

    /// Escaped text stays literal as one alternative of a brace or extglob
    /// group, which is why `,` and `|` are escaped too.
    #[test]
    fn escaped_text_stays_literal_inside_a_group() {
        let mut next = generator(0x0E5C_A9ED_7E57_0002);
        for _ in 0..1_500 {
            // Group alternatives are compared within one component here.
            let left = random_text(&mut next, 6);
            let right = random_text(&mut next, 6);
            if [&left, &right]
                .iter()
                .any(|text| text.contains(&b'/') || (cfg!(windows) && text.contains(&b'\\')))
            {
                continue;
            }
            let options = PatternOptions::walker().match_hidden(next(2) == 0);
            let braces = [b"{", &escape(&left)[..], b",", &escape(&right), b"}"].concat();
            let extglob = [b"@(", &escape(&left)[..], b"|", &escape(&right), b")"].concat();
            for pattern in [braces, extglob] {
                let shown = || {
                    format!(
                        "{:?} for {:?} and {:?}",
                        String::from_utf8_lossy(&pattern),
                        String::from_utf8_lossy(&left),
                        String::from_utf8_lossy(&right)
                    )
                };
                let compiled = Pattern::compile(&pattern, options)
                    .unwrap_or_else(|error| panic!("{error}: {}", shown()));
                for candidate in candidates(&left, &mut next).iter().chain([&right]) {
                    let expected = *candidate == left || *candidate == right;
                    assert_eq!(
                        compiled.is_match(candidate),
                        expected,
                        "{:?}: {}",
                        String::from_utf8_lossy(candidate),
                        shown()
                    );
                }
            }
        }
    }

    /// The fixed points worth reading: the bytes escaped, the ones left alone,
    /// and that escaping is not idempotent - it escapes its own `\`.
    #[test]
    fn escape_marks_every_syntax_byte_and_nothing_else() {
        assert_eq!(escape(br"\*?[]{}(),|!@+"), br"\\\*\?\[\]\{\}\(\)\,\|\!\@\+");
        assert_eq!(escape(b"./a-b^c/.d:e f\xff"), b"./a-b^c/.d:e f\xff");
        assert_eq!(escape(b""), b"");
        assert_eq!(escape(escape(b"*")), br"\\\*");
        assert_eq!(escape_str("caf\u{e9} [1]"), "caf\u{e9} \\[1\\]");
    }

    /// With escaping switched off a backslash is an ordinary byte, so the
    /// escaped spelling is not literal there. This is the documented limit.
    #[test]
    fn escaped_text_needs_escaping_enabled() {
        let pattern = Pattern::compile(escape("a*"), PatternOptions::default().escape(false))
            .expect("valid pattern");
        assert!(!pattern.is_match("a*"));
        assert!(pattern.is_match(r"a\*"));
    }
}
