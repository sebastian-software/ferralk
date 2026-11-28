//! Many compiled patterns asked as one.
//!
//! A [`PatternSet`] answers exactly what asking each of its [`Pattern`]s in
//! turn would answer: [`PatternSet::is_match_glob_path`] is the OR of the
//! members' [`Pattern::is_match_glob_path`], and
//! [`PatternSet::matches_glob_path_into`] lists every member that says yes.
//! What it saves is asking the members that cannot say yes.
//!
//! # How members are skipped
//!
//! Every alternative a pattern compiled to carries fixed bytes that any match
//! must contain in a known place — the facts [`Prefilter`] already reads off
//! the tokens. The set files each alternative under one such fact, chosen in
//! this order:
//!
//! 1. **Extension.** The trailing fixed run ends in `.ext`, with no further
//!    period after it (`**/*.rs`, `src/*.d.ts`). A matching path then ends in
//!    exactly those bytes, so its own final `.`-suffix *is* the key: one hash
//!    lookup per candidate covers every such alternative.
//! 2. **Component.** A literal token stands alone between component
//!    boundaries — a separator, a `**/` or an end of the pattern — on both
//!    sides (`**/node_modules/**`, `dist/**`). A separator token consumes
//!    exactly one separator byte and `**/` hands over only at a component
//!    start, so a matching path has that literal as one whole component.
//! 3. **Suffix.** The trailing fixed run's last component, indexed in a
//!    compact reversed-byte affix trie.
//! 4. **Prefix.** The leading fixed run's first component, indexed in a
//!    compact affix trie.
//!
//! An alternative with none of these, or one answered by an extglob program
//! whose tokens are not its semantics, makes its pattern a candidate for
//! every path. A pattern is a candidate when any of its alternatives' keys is
//! found, and every candidate is then asked through its own entry point. A
//! key is only ever a necessary condition, so the answer is the members'
//! answer by construction; `set_answers_like_its_members` holds that against
//! generated pattern lists on every entry point.
//!
//! The one normalization the path entry points apply before matching — one
//! leading `./` on the candidate, and on every alternative that starts with
//! it — is applied here too: keys are looked up in the candidate the members
//! will read, and an alternative with a leading `./` is keyed by what follows
//! it. Such an alternative never files a prefix key, because
//! [`Pattern::is_match`] still compares its `./` literally.
//!
//! [`Prefilter`]: crate::Prefilter

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet, hash_map::RandomState},
    error::Error,
    fmt,
    hash::{BuildHasher, Hasher},
};

use memchr::{memchr_iter, memchr2_iter, memchr3_iter, memrchr};

use crate::{
    CompiledAlternative, Pattern, PatternError, PatternOptions, Prefilter, Token,
    has_leading_dot_slash_prefix, interior_literal, next_separator, without_leading_dot_slash,
};

/// A list of compiled [`Pattern`]s matched as one, the counterpart of
/// `globset::GlobSet`.
///
/// Every query answers exactly what asking the members one by one would: the
/// `is_match*` methods are the OR of the members' verdicts, and the
/// `matches*_into` methods list each member that matches, by its position in
/// the list the set was built from. The three pairs mirror the three
/// [`Pattern`] entry points, so a set reads its patterns the way a single
/// pattern would be read.
///
/// The set is faster than a loop because it asks only the members a path can
/// possibly satisfy. Literal extensions, whole literal components, and fixed
/// prefixes and suffixes are indexed once at construction, so a list of
/// hundreds of `**/*.ext`, `dir/**` and `**/name/**` globs costs a few hash
/// lookups per path rather than hundreds of matches. A literal that sits
/// between wildcards, such as the `test` of `**/*test*`, is indexed too, and
/// found with one scan of the path. Patterns without any literal of two or
/// more bytes, such as `*` or an extglob, are asked for every path, as they
/// would be in a loop.
///
/// ```
/// use ferralk_glob::{PatternOptions, PatternSet};
///
/// let set = PatternSet::new(
///     ["**/*.rs", "**/node_modules/**", "docs/*.md"],
///     PatternOptions::walker(),
/// )?;
///
/// assert!(set.is_match_glob_path("src/lib.rs"));
/// assert!(set.is_match_glob_path("web/node_modules/left-pad/index.js"));
/// assert!(!set.is_match_glob_path("docs/api/index.md"));
///
/// // Which members matched, by their index in the input list.
/// let mut matched = Vec::new();
/// set.matches_glob_path_into("node_modules/pkg/build.rs", &mut matched);
/// assert_eq!(matched, [0, 1]);
/// # Ok::<(), ferralk_glob::PatternSetError>(())
/// ```
///
/// Members may carry different [`PatternOptions`]: compile each [`Pattern`]
/// yourself and collect them.
///
/// ```
/// use ferralk_glob::{Pattern, PatternOptions, PatternSet};
///
/// let options = PatternOptions::walker();
/// let set: PatternSet = [
///     Pattern::compile("**/*.rs", options)?,
///     // Only this member folds ASCII case.
///     Pattern::compile("**/README.md", options.case_insensitive(true))?,
/// ]
/// .into_iter()
/// .collect();
///
/// assert!(set.is_match_glob_path("docs/readme.MD"));
/// assert!(!set.is_match_glob_path("src/LIB.RS"));
/// # Ok::<(), ferralk_glob::PatternError>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct PatternSet {
    patterns: Vec<Pattern>,
    /// Keys compared byte for byte, from case-sensitive members.
    exact: KeyIndex,
    /// Keys stored ASCII-lowercased, from case-insensitive members, and
    /// looked up in a lowercased copy of the candidate.
    folded: KeyIndex,
    /// Members with an alternative no key describes, in ascending order.
    always: Vec<usize>,
}

/// The entry point a query stands for.
#[derive(Clone, Copy)]
enum Entry {
    Whole,
    Path,
    GlobPath,
}

impl Entry {
    fn matches(self, pattern: &Pattern, path: &[u8]) -> bool {
        match self {
            Self::Whole => pattern.is_match(path),
            Self::Path => pattern.is_match_path(path),
            Self::GlobPath => pattern.is_match_glob_path(path),
        }
    }

    /// The bytes the members compare their fixed runs against. Only the path
    /// entry points drop a leading `./` before matching.
    fn view(self, path: &[u8]) -> &[u8] {
        match self {
            Self::Whole => path,
            Self::Path | Self::GlobPath => without_leading_dot_slash(path),
        }
    }
}

impl PatternSet {
    /// Compiles every glob with the same `options`, keeping their order.
    ///
    /// ```
    /// use ferralk_glob::{PatternOptions, PatternSet};
    ///
    /// let excludes = PatternSet::new(["**/target/**", "**/*.tmp"], PatternOptions::walker())?;
    /// assert!(excludes.is_match_glob_path("crates/cli/target/debug/cli"));
    /// assert!(!excludes.is_match_glob_path("crates/cli/src/main.rs"));
    /// # Ok::<(), ferralk_glob::PatternSetError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a [`PatternSetError`] for the first glob that does not compile.
    /// [`PatternSetError::index`] is that glob's position in `patterns`, and
    /// [`PatternSetError::pattern_error`] the [`PatternError`] that
    /// [`Pattern::compile`] reported for it.
    ///
    /// ```
    /// use ferralk_glob::{PatternOptions, PatternSet};
    ///
    /// let error = PatternSet::new(["src/**", "[a-", "*.md"], PatternOptions::walker())
    ///     .expect_err("the class is never closed");
    /// assert_eq!(error.index(), 1);
    /// assert_eq!(error.pattern_error().offset(), 0);
    /// ```
    pub fn new<I>(patterns: I, options: PatternOptions) -> Result<Self, PatternSetError>
    where
        I: IntoIterator,
        I::Item: AsRef<[u8]>,
    {
        let patterns = patterns
            .into_iter()
            .enumerate()
            .map(|(index, pattern)| {
                Pattern::compile(pattern, options).map_err(|error| PatternSetError { index, error })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::from_patterns(patterns))
    }

    /// Builds the index over already compiled members.
    fn from_patterns(patterns: Vec<Pattern>) -> Self {
        let hasher = KeyBuildHasher::new();
        let mut exact = KeyIndexBuilder::new(hasher);
        let mut folded = KeyIndexBuilder::new(hasher);
        let mut always = Vec::new();
        for (index, pattern) in patterns.iter().enumerate() {
            let keys = pattern
                .alternatives
                .iter()
                .map(Key::of)
                .collect::<Option<Vec<_>>>();
            let Some(keys) = keys else {
                always.push(index);
                continue;
            };
            let target = if pattern.options.case_insensitive {
                &mut folded
            } else {
                &mut exact
            };
            for key in keys {
                target.insert(key, index, pattern.options.case_insensitive);
            }
        }
        always.shrink_to_fit();
        Self {
            patterns,
            exact: exact.finish(),
            folded: folded.finish(),
            always,
        }
    }

    /// The number of member patterns.
    ///
    /// ```
    /// use ferralk_glob::{PatternOptions, PatternSet};
    ///
    /// let set = PatternSet::new(["*.rs", "*.toml"], PatternOptions::walker())?;
    /// assert_eq!(set.len(), 2);
    /// # Ok::<(), ferralk_glob::PatternSetError>(())
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Whether the set has no members. An empty set matches nothing.
    ///
    /// ```
    /// use ferralk_glob::PatternSet;
    ///
    /// let set = PatternSet::default();
    /// assert!(set.is_empty());
    /// assert!(!set.is_match_glob_path("anything"));
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Whether any member's [`Pattern::is_match`] accepts `path`: the whole
    /// byte sequence, with wildcards crossing `/`.
    ///
    /// ```
    /// use ferralk_glob::{PatternOptions, PatternSet};
    ///
    /// let set = PatternSet::new(["*.rs", "*.toml"], PatternOptions::walker())?;
    /// // `*` crosses the separator under this entry point.
    /// assert!(set.is_match("src/lib.rs"));
    /// assert!(!set.is_match("src/lib.c"));
    /// # Ok::<(), ferralk_glob::PatternSetError>(())
    /// ```
    #[must_use]
    pub fn is_match(&self, path: impl AsRef<[u8]>) -> bool {
        self.any(path.as_ref(), Entry::Whole)
    }

    /// Whether any member's [`Pattern::is_match_path`] accepts `path`, under
    /// zlob's list-filter rule.
    ///
    /// ```
    /// use ferralk_glob::{PatternOptions, PatternSet};
    ///
    /// let set = PatternSet::new(["*.rs", "docs/*.md"], PatternOptions::walker())?;
    /// // A wildcard in the first component crosses separators...
    /// assert!(set.is_match_path("./src/lib.rs"));
    /// // ...one directly behind `/` does not.
    /// assert!(!set.is_match_path("docs/api/index.md"));
    /// # Ok::<(), ferralk_glob::PatternSetError>(())
    /// ```
    #[must_use]
    pub fn is_match_path(&self, path: impl AsRef<[u8]>) -> bool {
        self.any(path.as_ref(), Entry::Path)
    }

    /// Whether any member's [`Pattern::is_match_glob_path`] accepts `path`,
    /// with every ordinary wildcard inside one component as in a shell. This
    /// is the entry point for root-relative filesystem paths.
    ///
    /// ```
    /// use ferralk_glob::{PatternOptions, PatternSet};
    ///
    /// let set = PatternSet::new(["*.toml", "src/**/*.rs"], PatternOptions::walker())?;
    /// assert!(set.is_match_glob_path("Cargo.toml"));
    /// assert!(set.is_match_glob_path("./src/bin/main.rs"));
    /// // `*` stays in its component.
    /// assert!(!set.is_match_glob_path("crates/cli/Cargo.toml"));
    /// # Ok::<(), ferralk_glob::PatternSetError>(())
    /// ```
    #[must_use]
    pub fn is_match_glob_path(&self, path: impl AsRef<[u8]>) -> bool {
        self.any(path.as_ref(), Entry::GlobPath)
    }

    /// Replaces the contents of `matches` with the index of every member
    /// whose [`Pattern::is_match`] accepts `path`, in ascending order.
    ///
    /// The vector is cleared first and reused, so a caller that asks about
    /// many paths allocates only while it grows.
    ///
    /// ```
    /// use ferralk_glob::{PatternOptions, PatternSet};
    ///
    /// let set = PatternSet::new(["*.rs", "src/*", "*.md"], PatternOptions::walker())?;
    /// let mut matches = Vec::new();
    /// set.matches_into("src/lib.rs", &mut matches);
    /// assert_eq!(matches, [0, 1]);
    /// # Ok::<(), ferralk_glob::PatternSetError>(())
    /// ```
    pub fn matches_into(&self, path: impl AsRef<[u8]>, matches: &mut Vec<usize>) {
        self.collect(path.as_ref(), Entry::Whole, matches);
    }

    /// Replaces the contents of `matches` with the index of every member
    /// whose [`Pattern::is_match_path`] accepts `path`, in ascending order.
    /// The vector is cleared first.
    ///
    /// ```
    /// use ferralk_glob::{PatternOptions, PatternSet};
    ///
    /// let set = PatternSet::new(["*.rs", "src/*.rs", "*.md"], PatternOptions::walker())?;
    /// let mut matches = Vec::new();
    /// set.matches_path_into("./src/bin/main.rs", &mut matches);
    /// // `src/*.rs` keeps its `*` inside `bin/`, which holds a separator.
    /// assert_eq!(matches, [0]);
    /// # Ok::<(), ferralk_glob::PatternSetError>(())
    /// ```
    pub fn matches_path_into(&self, path: impl AsRef<[u8]>, matches: &mut Vec<usize>) {
        self.collect(path.as_ref(), Entry::Path, matches);
    }

    /// Replaces the contents of `matches` with the index of every member
    /// whose [`Pattern::is_match_glob_path`] accepts `path`, in ascending
    /// order. The vector is cleared first.
    ///
    /// ```
    /// use ferralk_glob::{PatternOptions, PatternSet};
    ///
    /// let rules = ["**/*.ts", "**/*.test.ts", "src/generated/**"];
    /// let set = PatternSet::new(rules, PatternOptions::walker())?;
    /// let mut matches = Vec::new();
    ///
    /// set.matches_glob_path_into("src/generated/api.test.ts", &mut matches);
    /// assert_eq!(matches, [0, 1, 2]);
    ///
    /// // The last matching rule wins, as in many configuration formats.
    /// set.matches_glob_path_into("src/app.ts", &mut matches);
    /// assert_eq!(matches.last().map(|&index| rules[index]), Some("**/*.ts"));
    /// # Ok::<(), ferralk_glob::PatternSetError>(())
    /// ```
    pub fn matches_glob_path_into(&self, path: impl AsRef<[u8]>, matches: &mut Vec<usize>) {
        self.collect(path.as_ref(), Entry::GlobPath, matches);
    }

    fn any(&self, path: &[u8], entry: Entry) -> bool {
        if !self.exact.is_empty() || !self.folded.is_empty() {
            let mut dedup = CandidateDedup::new(self.patterns.len());
            let found = self.for_each_candidate(entry.view(path), |index| {
                dedup.mark_new(index) && entry.matches(&self.patterns[index], path)
            });
            drop(dedup);
            if found {
                return true;
            }
        }
        self.always
            .iter()
            .any(|&index| entry.matches(&self.patterns[index], path))
    }

    fn collect(&self, path: &[u8], entry: Entry, matches: &mut Vec<usize>) {
        matches.clear();
        let mut keyed_matched = false;
        if !self.exact.is_empty() || !self.folded.is_empty() {
            let mut dedup = CandidateDedup::new(self.patterns.len());
            self.for_each_candidate(entry.view(path), |index| {
                if dedup.mark_new(index) && entry.matches(&self.patterns[index], path) {
                    matches.push(index);
                    keyed_matched = true;
                }
                false
            });
        }
        for &index in &self.always {
            if entry.matches(&self.patterns[index], path) {
                matches.push(index);
            }
        }
        // Keyed candidates arrive in bucket/trie order rather than pattern
        // order; `always` is already ascending and disjoint from keyed members.
        if keyed_matched && matches.len() > 1 {
            matches.sort_unstable();
        }
        debug_assert!(matches.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// Calls `visit` with every keyed member whose key `view` contains, until
    /// `visit` returns `true`. A member may be visited more than once.
    fn for_each_candidate(&self, view: &[u8], mut visit: impl FnMut(usize) -> bool) -> bool {
        if self.exact.find(view, &mut visit) {
            return true;
        }
        if self.folded.is_empty() {
            return false;
        }
        if view.iter().any(u8::is_ascii_uppercase) {
            // Most candidates fit the stack copy; longer paths reuse a bounded
            // thread-local buffer so warm calls do not allocate.
            let mut buffer = [0; 256];
            if let Some(folded) = buffer.get_mut(..view.len()) {
                folded.copy_from_slice(view);
                folded.make_ascii_lowercase();
                self.folded.find(folded, &mut visit)
            } else {
                self.find_folded_long(view, &mut visit)
            }
        } else {
            self.folded.find(view, &mut visit)
        }
    }

    #[cold]
    fn find_folded_long(&self, view: &[u8], visit: &mut impl FnMut(usize) -> bool) -> bool {
        if view.len() <= MAX_RETAINED_FOLD_BYTES
            && let Ok(Some(matched)) = FOLD_SCRATCH.try_with(|cell| {
                let Ok(mut scratch) = cell.try_borrow_mut() else {
                    return None;
                };
                scratch.clear();
                if scratch.capacity() < view.len() {
                    let target = view
                        .len()
                        .checked_next_power_of_two()
                        .map_or(MAX_RETAINED_FOLD_BYTES, |n| n.min(MAX_RETAINED_FOLD_BYTES));
                    scratch.reserve_exact(target);
                }
                scratch.extend_from_slice(view);
                scratch.make_ascii_lowercase();
                Some(self.folded.find(&scratch, visit))
            })
        {
            return matched;
        }
        self.folded.find(&view.to_ascii_lowercase(), visit)
    }
}

impl FromIterator<Pattern> for PatternSet {
    /// Collects compiled patterns into a set, keeping their order and each
    /// member's own [`PatternOptions`].
    ///
    /// ```
    /// use ferralk_glob::{Pattern, PatternOptions, PatternSet};
    ///
    /// let set: PatternSet = ["*.rs", "*.toml"]
    ///     .iter()
    ///     .map(|glob| Pattern::compile(glob, PatternOptions::walker()))
    ///     .collect::<Result<_, _>>()?;
    /// assert!(set.is_match_glob_path("Cargo.toml"));
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
    fn from_iter<I: IntoIterator<Item = Pattern>>(patterns: I) -> Self {
        Self::from_patterns(patterns.into_iter().collect())
    }
}

/// The glob in a [`PatternSet::new`] list that failed to compile.
///
/// ```
/// use ferralk_glob::{PatternOptions, PatternSet};
///
/// let globs = ["**/*.rs", "src/[a-z*"];
/// let error = PatternSet::new(globs, PatternOptions::walker()).expect_err("the class is never closed");
///
/// assert_eq!(globs[error.index()], "src/[a-z*");
/// // The byte offset points into that glob, as for `Pattern::compile`.
/// assert_eq!(error.pattern_error().offset(), 4);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternSetError {
    index: usize,
    error: PatternError,
}

impl PatternSetError {
    /// The position of the failed glob in the list given to
    /// [`PatternSet::new`].
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// The compile error of that glob, with its byte offset into it.
    #[must_use]
    pub const fn pattern_error(&self) -> &PatternError {
        &self.error
    }
}

impl fmt::Display for PatternSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "pattern {}: {}", self.index, self.error)
    }
}

impl Error for PatternSetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.error)
    }
}

/// One necessary condition an alternative files itself under. The bytes never
/// contain a separator of any platform.
enum Key {
    /// The candidate ends in these bytes, which start with its final `.`.
    Extension(Vec<u8>),
    /// One whole component of the candidate equals these bytes.
    Component(Vec<u8>),
    /// The candidate ends in these bytes.
    Suffix(Vec<u8>),
    /// The candidate starts with these bytes.
    Prefix(Vec<u8>),
    /// The candidate contains these bytes somewhere. At least
    /// [`InteriorIndex::ANCHOR`] bytes long, and the only key that may hold a
    /// `/`.
    Contains(Vec<u8>),
}

impl Key {
    /// The most selective key every match of `alternative` satisfies, or
    /// `None` when the alternative must be asked about every candidate.
    fn of(alternative: &CompiledAlternative) -> Option<Self> {
        // An extglob program answers from its own compile; its tokens spell
        // the group syntax out as text.
        if alternative.extglob.is_some() {
            return None;
        }
        // The path entry points read an alternative with a leading `./` from
        // its stripped copy, so only what follows it is fixed for all three.
        let stripped = has_leading_dot_slash_prefix(alternative);
        let tokens = if stripped {
            &alternative.tokens[2..]
        } else {
            &alternative.tokens[..]
        };
        let (leading, trailing, _) = Prefilter::fixed_run_bounds(tokens);
        let suffix = last_component_of_run(&tokens[tokens.len() - trailing..]);
        // A final literal that is a whole component holds the extension and
        // more (`**/Cargo.toml`), so it selects strictly fewer paths.
        if let Some(name) = tokens
            .len()
            .checked_sub(1)
            .and_then(|last| whole_component_at(tokens, last))
        {
            return Some(Self::Component(name.to_vec()));
        }
        if let Some(dot) = memrchr(b'.', suffix) {
            return Some(Self::Extension(suffix[dot..].to_vec()));
        }
        if let Some(component) = (0..tokens.len())
            .rev()
            .filter_map(|index| whole_component_at(tokens, index))
            .max_by_key(|literal| literal.len())
        {
            return Some(Self::Component(component.to_vec()));
        }
        if !suffix.is_empty() {
            return Some(Self::Suffix(suffix.to_vec()));
        }
        // `Pattern::is_match` compares a leading `./` literally, so a stripped
        // alternative's first bytes are not where that entry point reads them.
        let prefix = first_component_of_run(&tokens[..leading]);
        if !stripped && !prefix.is_empty() {
            return Some(Self::Prefix(prefix.to_vec()));
        }
        // The prefilter's interior literal: every match holds these bytes
        // contiguously, wherever the wildcards around them put it. It is read
        // from the same tokens the path entry points match, so it holds for
        // all three, and it never holds a separator a platform spells twice.
        let (_, _, elidable_separator) = Prefilter::fixed_run_bounds(tokens);
        interior_literal(tokens, leading, trailing, elidable_separator)
            .filter(|needle| needle.len() >= InteriorIndex::ANCHOR)
            .map(Self::Contains)
    }
}

/// The bytes after the last separator-like byte of a fixed run's trailing
/// literal. Adjacent literal tokens are merged during compilation, so only the
/// final token can contribute bytes past the last separator.
fn last_component_of_run(run: &[Token]) -> &[u8] {
    match run.last() {
        Some(Token::Literal(literal)) => last_component(literal),
        _ => b"",
    }
}

/// The bytes before the first separator-like byte of a fixed run's leading
/// literal.
fn first_component_of_run(run: &[Token]) -> &[u8] {
    match run.first() {
        Some(Token::Literal(literal)) => first_component(literal),
        _ => b"",
    }
}

/// The bytes after the last separator-like byte of a fixed run. A run spells
/// a separator token as `/`, and an escaped `\` is excluded too so a key is
/// separator-free on every platform.
fn last_component(run: &[u8]) -> &[u8] {
    run.iter()
        .rposition(|&byte| matches!(byte, b'/' | b'\\'))
        .map_or(run, |separator| &run[separator + 1..])
}

/// The bytes before the first separator-like byte of a fixed run.
fn first_component(run: &[u8]) -> &[u8] {
    run.iter()
        .position(|&byte| matches!(byte, b'/' | b'\\'))
        .map_or(run, |separator| &run[..separator])
}

/// The literal token at `index` when it must match one whole candidate
/// component: it starts the tokens or follows a separator or `**/`, and it
/// ends them or precedes a separator.
fn whole_component_at(tokens: &[Token], index: usize) -> Option<&[u8]> {
    let Token::Literal(literal) = &tokens[index] else {
        return None;
    };
    let starts =
        index == 0 || matches!(tokens[index - 1], Token::Separator | Token::RecursivePrefix);
    let ends = matches!(tokens.get(index + 1), None | Some(Token::Separator));
    (starts
        && ends
        && !literal.is_empty()
        && !literal.iter().any(|&byte| matches!(byte, b'/' | b'\\')))
    .then_some(literal.as_slice())
}

const INLINE_DEDUP_CAPACITY: usize = 4;
const MAX_RETAINED_SEEN_SLOTS: usize = 16_384;
const MAX_RETAINED_FOLD_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct SeenScratch {
    stamps: Vec<u32>,
    generation: u32,
}

thread_local! {
    static SEEN_SCRATCH: RefCell<SeenScratch> = const {
        RefCell::new(SeenScratch {
            stamps: Vec::new(),
            generation: 0,
        })
    };
    static FOLD_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Candidate-proportional fallback set for large [`PatternSet`]s or thread
/// teardown. Wrapped behind an out-of-line cold drop so [`CandidateDedup`]
/// keeps a single null pointer on the stack without inflating unwind/drop
/// code size on the common inline and TLS paths.
struct SparseSeen(HashSet<usize>);

struct SparseSlot(Option<Box<SparseSeen>>);

impl Drop for SparseSlot {
    #[inline]
    fn drop(&mut self) {
        if let Some(boxed) = self.0.take() {
            drop_sparse_seen(boxed);
        }
    }
}

#[cold]
#[inline(never)]
fn drop_sparse_seen(boxed: Box<SparseSeen>) {
    drop(boxed);
}

/// Allocation-free warm candidate deduplicator for one [`PatternSet`] query.
///
/// Up to [`INLINE_DEDUP_CAPACITY`] distinct candidates are tracked on the stack
/// without touching thread-local state. For sets up to
/// [`MAX_RETAINED_SEEN_SLOTS`] patterns, larger fanouts spill into a
/// generation-stamped thread-local table (zero warm allocations). Larger sets
/// — and thread-teardown calls where TLS is unavailable — spill into a sparse
/// `HashSet` sized only by the candidates actually visited.
struct CandidateDedup {
    pattern_count: usize,
    inline: [usize; INLINE_DEDUP_CAPACITY],
    inline_len: usize,
    tls_generation: u32,
    sparse: SparseSlot,
}

impl CandidateDedup {
    #[inline]
    fn new(pattern_count: usize) -> Self {
        Self {
            pattern_count,
            inline: [0; INLINE_DEDUP_CAPACITY],
            inline_len: 0,
            tls_generation: 0,
            sparse: SparseSlot(None),
        }
    }

    #[inline]
    fn mark_new(&mut self, index: usize) -> bool {
        if self.tls_generation != 0 {
            return self.mark_spilled(index);
        }
        if self.sparse.0.is_some() {
            return self.mark_sparse(index);
        }
        if self.inline[..self.inline_len].contains(&index) {
            return false;
        }
        if self.inline_len < INLINE_DEDUP_CAPACITY {
            self.inline[self.inline_len] = index;
            self.inline_len += 1;
            return true;
        }
        self.spill_and_mark(index)
    }

    #[cold]
    #[inline(never)]
    fn mark_sparse(&mut self, index: usize) -> bool {
        self.sparse
            .0
            .as_deref_mut()
            .expect("checked sparse.is_some()")
            .0
            .insert(index)
    }

    #[cold]
    #[inline(never)]
    fn mark_spilled(&mut self, index: usize) -> bool {
        let generation = self.tls_generation;
        let result = SEEN_SCRATCH.try_with(|cell| {
            let Ok(mut scratch) = cell.try_borrow_mut() else {
                return None;
            };
            let slot = &mut scratch.stamps[index];
            if *slot == generation {
                Some(false)
            } else {
                *slot = generation;
                Some(true)
            }
        });
        match result {
            Ok(Some(is_new)) => is_new,
            Ok(None) | Err(_) => {
                // Defensive fallback if TLS became unavailable mid-query.
                self.tls_generation = 0;
                let sparse = self.sparse.0.get_or_insert_with(|| {
                    let mut set = HashSet::with_capacity(INLINE_DEDUP_CAPACITY + 4);
                    set.extend(self.inline[..self.inline_len].iter().copied());
                    Box::new(SparseSeen(set))
                });
                sparse.0.insert(index)
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn spill_and_mark(&mut self, index: usize) -> bool {
        if self.pattern_count <= MAX_RETAINED_SEEN_SLOTS {
            let spilled = SEEN_SCRATCH
                .try_with(|cell| {
                    let Ok(mut scratch) = cell.try_borrow_mut() else {
                        return false;
                    };
                    if scratch.generation == u32::MAX {
                        scratch.stamps.fill(0);
                        scratch.generation = 0;
                    }
                    scratch.generation += 1;
                    let generation = scratch.generation;
                    if scratch.stamps.len() < self.pattern_count {
                        scratch.stamps.resize(self.pattern_count, 0);
                    }
                    for &prior in &self.inline[..self.inline_len] {
                        scratch.stamps[prior] = generation;
                    }
                    scratch.stamps[index] = generation;
                    self.tls_generation = generation;
                    true
                })
                .unwrap_or(false);
            if spilled {
                return true;
            }
        }
        let mut sparse = HashSet::with_capacity(INLINE_DEDUP_CAPACITY + 4);
        sparse.extend(self.inline[..self.inline_len].iter().copied());
        sparse.insert(index);
        self.sparse.0 = Some(Box::new(SparseSeen(sparse)));
        true
    }
}

/// Keys of one case policy, each mapped to the members that filed it.
#[derive(Debug, Clone, Default)]
struct KeyIndex {
    extensions: KeyMap,
    components: KeyMap,
    /// Reversed-byte compressed affix trie for suffix keys.
    suffixes: MemberTrie,
    /// Forward-byte compressed affix trie for prefix keys.
    prefixes: MemberTrie,
    /// Interior literals, found by one scan of the candidate.
    contains: InteriorIndex,
}

struct KeyIndexBuilder {
    extensions: KeyMap,
    components: KeyMap,
    /// Sorted, so needles sharing their first bytes end up side by side.
    contains: BTreeMap<Vec<u8>, Vec<usize>>,
    /// Deduplicates `(affix, member_index)` as patterns are inserted so many
    /// brace arms sharing the same prefix/suffix retain one key copy before
    /// the flat [`MemberTrie`] is assembled.
    suffixes: KeyMap,
    prefixes: KeyMap,
}

impl KeyIndexBuilder {
    fn new(hasher: KeyBuildHasher) -> Self {
        Self {
            extensions: KeyMap::with_hasher(hasher),
            components: KeyMap::with_hasher(hasher),
            suffixes: KeyMap::with_hasher(hasher),
            prefixes: KeyMap::with_hasher(hasher),
            contains: BTreeMap::new(),
        }
    }

    fn insert(&mut self, key: Key, index: usize, fold: bool) {
        if let Key::Contains(mut needle) = key {
            if fold {
                needle.make_ascii_lowercase();
            }
            let members = self.contains.entry(needle).or_default();
            // Members are inserted in index order, so a repeat is the last entry.
            if members.last() != Some(&index) {
                members.push(index);
            }
            return;
        }
        let (target, mut bytes) = match key {
            Key::Extension(bytes) => (&mut self.extensions, bytes),
            Key::Component(bytes) => (&mut self.components, bytes),
            Key::Suffix(bytes) => (&mut self.suffixes, bytes),
            Key::Prefix(bytes) => (&mut self.prefixes, bytes),
            Key::Contains(_) => unreachable!("interior keys are filed above"),
        };
        if fold {
            bytes.make_ascii_lowercase();
        }
        target.insert(bytes, index);
    }

    fn finish(mut self) -> KeyIndex {
        self.extensions.compact();
        self.components.compact();
        KeyIndex {
            extensions: self.extensions,
            components: self.components,
            suffixes: MemberTrie::build_from_map(self.suffixes, true),
            prefixes: MemberTrie::build_from_map(self.prefixes, false),
            contains: InteriorIndex::build(self.contains),
        }
    }
}

/// Interior literals and the members that filed them.
///
/// A candidate is scanned once: every position whose two bytes start some
/// needle is looked up in a bitmap of those pairs, and only a hit compares
/// the needles that start with them. A path with no such pair costs one bit
/// test per byte, whatever the number of needles.
#[derive(Debug, Clone, Default)]
struct InteriorIndex {
    /// One bit per byte pair that starts a needle; empty without needles.
    anchors: Box<[u64]>,
    /// Needles in byte order, so the ones sharing an anchor are adjacent.
    needles: Box<[InteriorNeedle]>,
    /// The distinct first bytes of the needles when there are at most three,
    /// so the scan can jump between them with `memchr` instead of testing
    /// every position.
    first_bytes: Option<Box<[u8]>>,
}

#[derive(Debug, Clone)]
struct InteriorNeedle {
    bytes: Box<[u8]>,
    members: Box<[usize]>,
}

impl InteriorIndex {
    /// The bytes a needle is found by, and so its minimum length. One byte
    /// would select far too many candidates to be worth an index.
    const ANCHOR: usize = 2;

    fn anchor(pair: [u8; Self::ANCHOR]) -> usize {
        usize::from(u16::from_be_bytes(pair))
    }

    fn build(needles: BTreeMap<Vec<u8>, Vec<usize>>) -> Self {
        if needles.is_empty() {
            return Self::default();
        }
        let mut anchors = vec![0; (1 << 16) / 64].into_boxed_slice();
        let mut first_bytes = Vec::new();
        let needles = needles
            .into_iter()
            .map(|(bytes, members)| {
                // Sorted needles group their first bytes together.
                if first_bytes.last() != Some(&bytes[0]) {
                    first_bytes.push(bytes[0]);
                }
                let anchor = Self::anchor([bytes[0], bytes[1]]);
                anchors[anchor / 64] |= 1 << (anchor % 64);
                InteriorNeedle {
                    bytes: bytes.into_boxed_slice(),
                    members: members.into_boxed_slice(),
                }
            })
            .collect();
        Self {
            anchors,
            needles,
            first_bytes: (first_bytes.len() <= 3).then(|| first_bytes.into_boxed_slice()),
        }
    }

    fn is_empty(&self) -> bool {
        self.needles.is_empty()
    }

    fn find(&self, view: &[u8], visit: &mut impl FnMut(usize) -> bool) -> bool {
        if self.is_empty() {
            return false;
        }
        // A needle can start no later than `ANCHOR` bytes before the end.
        let Some(last_start) = view.len().checked_sub(Self::ANCHOR) else {
            return false;
        };
        let try_at = |start: usize| {
            let anchor = Self::anchor([view[start], view[start + 1]]);
            self.anchors[anchor / 64] & (1 << (anchor % 64)) != 0
                && self.visit_anchor(&view[start..], visit)
        };
        let starts = &view[..=last_start];
        match self.first_bytes.as_deref() {
            Some(&[a]) => memchr_iter(a, starts).any(try_at),
            Some(&[a, b]) => memchr2_iter(a, b, starts).any(try_at),
            Some(&[a, b, c]) => memchr3_iter(a, b, c, starts).any(try_at),
            _ => (0..=last_start).any(try_at),
        }
    }

    #[inline(never)]
    fn visit_anchor(&self, rest: &[u8], visit: &mut impl FnMut(usize) -> bool) -> bool {
        let pair = &rest[..Self::ANCHOR];
        let first = self
            .needles
            .partition_point(|needle| needle.bytes[..Self::ANCHOR] < *pair);
        self.needles[first..]
            .iter()
            .take_while(|needle| needle.bytes[..Self::ANCHOR] == *pair)
            .filter(|needle| rest.starts_with(&needle.bytes))
            .any(|needle| visit_member_slice(&needle.members, visit))
    }
}

/// Compact path-compressed radix trie mapping literal prefixes or reversed
/// suffixes to sorted member-index slices.
#[derive(Debug, Clone, Default)]
struct MemberTrie {
    nodes: Box<[MemberTrieNode]>,
    edge_bytes: Box<[u8]>,
    edge_next: Box<[u32]>,
    labels: Box<[u8]>,
    members: Box<[usize]>,
    #[cfg(test)]
    key_count: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct MemberTrieNode {
    label_start: u32,
    label_len: u32,
    first_edge: u32,
    first_member: u32,
    member_count: u32,
    edge_count: u16,
}

impl MemberTrie {
    fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.key_count
    }

    /// Builds a flat compressed trie from deduplicated `(key, members)` groups
    /// in `O(U log U + total_unique_bytes)` time without recursion.
    ///
    /// When `reverse` is `true`, keys are reversed for trie structure while
    /// edge labels are stored in forward byte order so suffix checks compile
    /// to forward slice comparisons (`ends_with`).
    fn build_from_map(map: KeyMap, reverse: bool) -> Self {
        if map.is_empty() {
            return Self::default();
        }
        let mut entries: Vec<(Vec<u8>, Vec<usize>)> = map
            .members
            .into_iter()
            .map(|(key, members)| {
                let mut key = key.into_vec();
                if reverse {
                    key.reverse();
                }
                (key, members)
            })
            .collect();
        entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));

        let mut nodes = vec![MemberTrieNode::default()];
        let mut edge_bytes = Vec::new();
        let mut edge_next = Vec::new();
        let mut labels = Vec::new();
        let mut members = Vec::new();
        #[cfg(test)]
        let mut key_count = 0_usize;

        let mut stack = vec![(0_usize, 0_usize, entries.len(), 0_usize)];
        while let Some((node_idx, mut start, end, mut depth)) = stack.pop() {
            if node_idx != 0 {
                let first = &entries[start].0[depth..];
                let last = &entries[end - 1].0[depth..];
                let lcp_len = first.iter().zip(last).take_while(|(a, b)| a == b).count();
                let label_start = u32::try_from(labels.len()).expect("trie labels fit in u32");
                if reverse {
                    labels.extend(first[..lcp_len].iter().rev().copied());
                } else {
                    labels.extend_from_slice(&first[..lcp_len]);
                }
                nodes[node_idx].label_start = label_start;
                nodes[node_idx].label_len =
                    u32::try_from(lcp_len).expect("trie label length fits in u32");
                depth += lcp_len;
            }

            let first_member = u32::try_from(members.len()).expect("trie members fit in u32");
            // Keys in `entries` are unique, so at most one entry terminates at `depth`.
            if start < end && entries[start].0.len() == depth {
                members.extend_from_slice(&entries[start].1);
                start += 1;
                #[cfg(test)]
                {
                    key_count += 1;
                }
            }
            let member_count = members.len() - first_member as usize;
            nodes[node_idx].first_member = first_member;
            nodes[node_idx].member_count =
                u32::try_from(member_count).expect("trie node members fit in u32");

            let first_edge = u32::try_from(edge_bytes.len()).expect("trie edges fit in u32");
            let mut group_start = start;
            let mut child_count = 0_usize;
            while group_start < end {
                let byte = entries[group_start].0[depth];
                let mut group_end = group_start + 1;
                while group_end < end && entries[group_end].0[depth] == byte {
                    group_end += 1;
                }
                let child_idx = nodes.len();
                nodes.push(MemberTrieNode::default());
                edge_bytes.push(byte);
                edge_next.push(u32::try_from(child_idx).expect("trie nodes fit in u32"));
                stack.push((child_idx, group_start, group_end, depth + 1));
                child_count += 1;
                group_start = group_end;
            }
            nodes[node_idx].first_edge = first_edge;
            nodes[node_idx].edge_count =
                u16::try_from(child_count).expect("byte trie node has at most 256 edges");
        }

        Self {
            nodes: nodes.into_boxed_slice(),
            edge_bytes: edge_bytes.into_boxed_slice(),
            edge_next: edge_next.into_boxed_slice(),
            labels: labels.into_boxed_slice(),
            members: members.into_boxed_slice(),
            #[cfg(test)]
            key_count,
        }
    }

    #[inline]
    fn label(&self, node: MemberTrieNode) -> &[u8] {
        let start = node.label_start as usize;
        &self.labels[start..start + node.label_len as usize]
    }

    #[inline]
    fn visit_members(&self, node: MemberTrieNode, visit: &mut impl FnMut(usize) -> bool) -> bool {
        if node.member_count == 0 {
            return false;
        }
        let start = node.first_member as usize;
        visit_member_slice(
            &self.members[start..start + node.member_count as usize],
            visit,
        )
    }

    #[inline]
    fn next_child(&self, node: MemberTrieNode, byte: u8) -> Option<usize> {
        let start = node.first_edge as usize;
        let end = start + usize::from(node.edge_count);
        let bytes = &self.edge_bytes[start..end];
        let offset = if bytes.len() <= 16 {
            bytes.iter().position(|&edge| edge == byte)?
        } else {
            bytes.binary_search(&byte).ok()?
        };
        Some(self.edge_next[start + offset] as usize)
    }

    fn find_prefix(&self, view: &[u8], visit: &mut impl FnMut(usize) -> bool) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let mut node = self.nodes[0];
        if self.visit_members(node, visit) {
            return true;
        }
        let mut pos = 0;
        while pos < view.len() {
            let Some(child_idx) = self.next_child(node, view[pos]) else {
                return false;
            };
            pos += 1;
            let child = self.nodes[child_idx];
            let label = self.label(child);
            if !view[pos..].starts_with(label) {
                return false;
            }
            pos += label.len();
            if self.visit_members(child, visit) {
                return true;
            }
            node = child;
        }
        false
    }

    fn find_suffix(&self, view: &[u8], visit: &mut impl FnMut(usize) -> bool) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let mut node = self.nodes[0];
        if self.visit_members(node, visit) {
            return true;
        }
        let mut remaining = view.len();
        while remaining > 0 {
            let Some(child_idx) = self.next_child(node, view[remaining - 1]) else {
                return false;
            };
            remaining -= 1;
            let child = self.nodes[child_idx];
            let label = self.label(child);
            if !view[..remaining].ends_with(label) {
                return false;
            }
            remaining -= label.len();
            if self.visit_members(child, visit) {
                return true;
            }
            node = child;
        }
        false
    }
}

/// Members by key, with the key lengths present kept aside so a candidate
/// slice of another length is turned away before it is hashed.
#[derive(Debug, Clone)]
struct KeyMap {
    members: HashMap<Box<[u8]>, Vec<usize>, KeyBuildHasher>,
    /// Bit `n` is set when a key of length `n` exists; bit 63 stands for
    /// every length from 63 up.
    lengths: u64,
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::with_hasher(KeyBuildHasher::new())
    }
}

impl KeyMap {
    fn with_hasher(hasher: KeyBuildHasher) -> Self {
        Self {
            members: HashMap::with_hasher(hasher),
            lengths: 0,
        }
    }

    fn length_bit(length: usize) -> u64 {
        1 << length.min(63)
    }

    fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.members.len()
    }

    fn insert(&mut self, key: Vec<u8>, index: usize) {
        self.lengths |= Self::length_bit(key.len());
        if let Some(members) = self.members.get_mut(key.as_slice()) {
            // Members are inserted in index order, so a repeat is the last entry.
            if members.last() != Some(&index) {
                members.push(index);
            }
            return;
        }
        self.members.insert(key.into_boxed_slice(), vec![index]);
    }

    fn compact(&mut self) {
        for members in self.members.values_mut() {
            members.shrink_to_fit();
        }
    }

    fn get(&self, key: &[u8]) -> Option<&[usize]> {
        if self.lengths & Self::length_bit(key.len()) == 0 {
            return None;
        }
        self.members.get(key).map(Vec::as_slice)
    }
}

/// Out-of-line member-slice dispatch keeps the caller's closure captures out
/// of the hot component/extension/trie scan loops in [`KeyIndex::find`].
#[inline(never)]
fn visit_member_slice(members: &[usize], visit: &mut impl FnMut(usize) -> bool) -> bool {
    members.iter().any(|&index| visit(index))
}

impl KeyIndex {
    fn is_empty(&self) -> bool {
        self.extensions.is_empty()
            && self.components.is_empty()
            && self.suffixes.is_empty()
            && self.prefixes.is_empty()
            && self.contains.is_empty()
    }

    /// Visits the members of every key `view` satisfies, until `visit` returns
    /// `true`.
    fn find(&self, view: &[u8], visit: &mut impl FnMut(usize) -> bool) -> bool {
        let mut hits = |map: &KeyMap, key: &[u8]| {
            map.get(key)
                .is_some_and(|members| visit_member_slice(members, visit))
        };
        if !self.extensions.is_empty()
            && let Some(dot) = memrchr(b'.', view)
            && hits(&self.extensions, &view[dot..])
        {
            return true;
        }
        if !self.components.is_empty() {
            let mut rest = view;
            loop {
                let end = next_separator(rest).unwrap_or(rest.len());
                if hits(&self.components, &rest[..end]) {
                    return true;
                }
                if end == rest.len() {
                    break;
                }
                rest = &rest[end + 1..];
            }
        }
        if self.suffixes.find_suffix(view, visit) {
            return true;
        }
        if self.prefixes.find_prefix(view, visit) {
            return true;
        }
        self.contains.find(view, visit)
    }
}

/// Per-[`PatternSet`] seeded builder for [`KeyHasher`].
///
/// Seeded once per set from [`RandomState`] so offline collision attacks
/// against [`PatternSet::new`] or lookups cannot construct worst-case bucket
/// chains, while keeping short-key hashing at a few 64x64->128 multiply-fold
/// steps.
#[derive(Debug, Clone, Copy)]
struct KeyBuildHasher {
    seed: u64,
    multiplier: u64,
}

impl KeyBuildHasher {
    fn new() -> Self {
        let state = RandomState::new();
        let seed = state.hash_one(0_u64);
        // Keep the multiplier odd so the low 64 bits remain bijective while
        // the high 64 bits fold secret-dependent carries into every output bit.
        let multiplier = state.hash_one(1_u64) | 1;
        Self { seed, multiplier }
    }
}

impl Default for KeyBuildHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl BuildHasher for KeyBuildHasher {
    type Hasher = KeyHasher;

    fn build_hasher(&self) -> Self::Hasher {
        KeyHasher {
            state: self.seed,
            multiplier: self.multiplier,
        }
    }
}

/// A seeded folded-multiply hash for short byte keys.
#[derive(Debug, Clone, Copy)]
struct KeyHasher {
    state: u64,
    multiplier: u64,
}

impl KeyHasher {
    #[inline]
    fn mix(&mut self, word: u64) {
        let product = u128::from(self.state ^ word).wrapping_mul(u128::from(self.multiplier));
        self.state = (product as u64) ^ ((product >> 64) as u64);
    }
}

impl Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for &word in words {
            self.mix(u64::from_le_bytes(word));
        }
        if !rest.is_empty() {
            let mut word = [0; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.mix(u64::from_le_bytes(word));
        }
    }

    fn write_usize(&mut self, value: usize) {
        self.mix(value as u64);
    }

    fn finish(&self) -> u64 {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every member's own verdicts on `path`, one list per entry point.
    fn member_verdicts(patterns: &[Pattern], path: &[u8]) -> [Vec<usize>; 3] {
        let indices = |entry: Entry| {
            patterns
                .iter()
                .enumerate()
                .filter_map(|(index, pattern)| entry.matches(pattern, path).then_some(index))
                .collect::<Vec<_>>()
        };
        [
            indices(Entry::Whole),
            indices(Entry::Path),
            indices(Entry::GlobPath),
        ]
    }

    fn assert_set_agrees(
        set: &PatternSet,
        patterns: &[Pattern],
        path: &[u8],
        shown: &dyn Fn() -> String,
    ) {
        let [whole, path_filter, glob_path] = member_verdicts(patterns, path);
        let mut matches = vec![usize::MAX];
        set.matches_into(path, &mut matches);
        assert_eq!(matches, whole, "matches_into: {}", shown());
        set.matches_path_into(path, &mut matches);
        assert_eq!(matches, path_filter, "matches_path_into: {}", shown());
        set.matches_glob_path_into(path, &mut matches);
        assert_eq!(matches, glob_path, "matches_glob_path_into: {}", shown());
        assert_eq!(
            set.is_match(path),
            !whole.is_empty(),
            "is_match: {}",
            shown()
        );
        assert_eq!(
            set.is_match_path(path),
            !path_filter.is_empty(),
            "is_match_path: {}",
            shown()
        );
        assert_eq!(
            set.is_match_glob_path(path),
            !glob_path.is_empty(),
            "is_match_glob_path: {}",
            shown()
        );
    }

    /// The set answers exactly like asking its members in a loop, on every
    /// entry point, over generated pattern lists with mixed per-member
    /// options and generated candidates.
    #[test]
    fn set_answers_like_its_members() {
        let fragments: &[&[u8]] = &[
            b"a",
            b"B",
            b"ab",
            b"node_modules",
            b".rs",
            b".RS",
            b".d.ts",
            b".",
            b"/",
            b"./",
            b"*",
            b"?",
            b"**",
            b"**/",
            b"/**",
            b"[ab]",
            b"[!.]",
            b"{a,b}",
            b"{./a,.rs}",
            b"{a/b,x.rs}",
            b"\\/",
            b"\\.",
            b"\\\\",
            b"\\*",
            b"@(ab|B)",
            b"!(nm)",
            b"+(a)",
        ];
        let path_pieces: &[&[u8]] = &[
            b"a",
            b"b",
            b"B",
            b"ab",
            b"AB",
            b".",
            b"/",
            b"./",
            b"a/b",
            b".rs",
            b".RS",
            b"x.d.ts",
            b"node_modules",
            b"NODE_MODULES",
            b"\\",
            b"",
        ];
        // The generator of the other randomized tests in this crate.
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move |bound: usize| {
            seed = seed.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(1);
            (usize::try_from(seed >> 33).expect("31 bits fit a usize")) % bound
        };
        let mut keyed = 0_usize;
        let mut always = 0_usize;
        for _ in 0..1_500 {
            let mut sources = Vec::new();
            let mut patterns = Vec::new();
            for _ in 0..1 + next(8) {
                let mut source = Vec::new();
                for _ in 0..1 + next(5) {
                    source.extend_from_slice(fragments[next(fragments.len())]);
                }
                let options = PatternOptions::default()
                    .braces(next(3) != 0)
                    .recursive_double_star(next(4) != 0)
                    .extglob(next(3) == 0)
                    .match_hidden(next(2) == 0)
                    .case_insensitive(next(3) == 0)
                    .escape(next(4) != 0);
                if let Ok(pattern) = Pattern::compile(&source, options) {
                    sources.push((source, options));
                    patterns.push(pattern);
                }
            }
            let set: PatternSet = patterns.iter().cloned().collect();
            keyed += set.len() - set.always.len();
            always += set.always.len();
            for _ in 0..24 {
                let mut path = Vec::new();
                for _ in 0..next(7) {
                    path.extend_from_slice(path_pieces[next(path_pieces.len())]);
                }
                let shown = || {
                    format!(
                        "{:?} against {:?}",
                        sources
                            .iter()
                            .map(|(source, options)| format!(
                                "{:?} {options:?}",
                                String::from_utf8_lossy(source)
                            ))
                            .collect::<Vec<_>>(),
                        String::from_utf8_lossy(&path)
                    )
                };
                assert_set_agrees(&set, &patterns, &path, &shown);
            }
        }
        assert!(
            keyed > 2 * always,
            "only {keyed} of {} generated members were keyed",
            keyed + always
        );
    }

    /// Realistic list shapes, each bucket kind with many members, against
    /// realistic paths.
    #[test]
    fn realistic_lists_answer_like_their_members() {
        let globs = [
            "**/*.rs",
            "**/*.d.ts",
            "src/**/*.{ts,tsx}",
            "**/node_modules/**",
            "target/**",
            "./docs/**/*.md",
            "**/Makefile",
            "**/*rc",
            "test_*",
            "**/.git/**",
            "*.lock",
            "**/*.Snap",
            "**/!(index).js",
            "src/*/mod.rs",
            "**",
            "{./a,b}/**",
        ];
        let paths = [
            "src/lib.rs",
            "./src/lib.rs",
            "src/app/view.tsx",
            "types/index.d.ts",
            "web/node_modules/pkg/index.js",
            "target/debug/build",
            "target",
            "docs/guide/intro.md",
            "./docs/intro.md",
            "tools/Makefile",
            "home/.bashrc",
            "test_parser",
            ".git/HEAD",
            "Cargo.lock",
            "snapshots/view.snap",
            "lib/index.js",
            "lib/main.js",
            "src/parser/mod.rs",
            "a/x",
            "./a/x",
            "b",
            "",
            ".",
            "./",
        ];
        for options in [
            PatternOptions::walker(),
            PatternOptions::walker().match_hidden(true),
            PatternOptions::walker().case_insensitive(true),
            PatternOptions::default(),
        ] {
            let set = PatternSet::new(globs, options).expect("the globs are valid");
            let patterns = globs
                .iter()
                .map(|glob| Pattern::compile(glob, options).expect("the glob is valid"))
                .collect::<Vec<_>>();
            for path in paths {
                for path in [path.to_owned(), path.to_ascii_uppercase()] {
                    let shown = || format!("{path:?} under {options:?}");
                    assert_set_agrees(&set, &patterns, path.as_bytes(), &shown);
                }
            }
        }
    }

    #[test]
    fn common_shapes_are_keyed() {
        let set = PatternSet::new(
            [
                "**/*.rs",
                "**/node_modules/**",
                "dist/**",
                "**/Makefile",
                "**/*rc",
                "test_*",
                "./src/**",
                "*",
                "**/*.@(js|ts)",
            ],
            PatternOptions::walker(),
        )
        .expect("the globs are valid");
        assert_eq!(set.exact.extensions.len(), 1);
        assert_eq!(set.exact.components.len(), 4);
        assert_eq!(set.exact.suffixes.len(), 1);
        assert_eq!(set.exact.prefixes.len(), 1);
        // `*` has no literal, and an extglob program is not read from tokens.
        assert_eq!(set.always, [7, 8]);
    }

    #[test]
    fn interior_literals_are_keyed_and_answer_like_their_members() {
        let globs = [
            "**/*test*",
            "**/*x12y*",
            "*ab*",
            "**/*a/b*",
            "./**/*mid*",
            "**/*q*",
            "*[ab]cd?*",
            "**/*TeSt*",
        ];
        let sensitive = PatternOptions::walker();
        let folded = sensitive.case_insensitive(true);
        let patterns = globs
            .iter()
            .enumerate()
            .map(|(index, glob)| {
                let options = if index == globs.len() - 1 {
                    folded
                } else {
                    sensitive
                };
                Pattern::compile(glob, options).expect("the glob is valid")
            })
            .collect::<Vec<_>>();
        let set = patterns.iter().cloned().collect::<PatternSet>();
        // `**/*q*` has only a one-byte literal, too unselective to index. On
        // Windows a separator also matches `\`, so `a/b` is two one-byte
        // literals there and joins it.
        let (exact_needles, always) = if cfg!(windows) {
            (5, &[3, 5][..])
        } else {
            (6, &[5][..])
        };
        assert_eq!(set.exact.contains.needles.len(), exact_needles);
        assert_eq!(set.folded.contains.needles.len(), 1);
        assert_eq!(set.always, always);
        for path in [
            "src/test.rs",
            "src/unit_test/main.rs",
            "docs/x12y.md",
            "x12/y",
            "ab",
            "a",
            "dir/a/b",
            "dira/bfile",
            "a/b",
            "./mid",
            "deep/./mid.rs",
            "sub/xmidx",
            "q",
            "zcd!",
            "acd",
            "src/TEST.RS",
            "src/Tes/t",
            "",
            "./",
            "x1",
        ] {
            assert_set_agrees(&set, &patterns, path.as_bytes(), &|| path.to_owned());
        }
        // One, two and three distinct first bytes take the `memchr` scan.
        for globs in [
            &["**/*ab*", "**/*ac*"][..],
            &["**/*ab*", "**/*bc*"],
            &["**/*ab*", "**/*bc*", "**/*cd*"],
        ] {
            let patterns = globs
                .iter()
                .map(|glob| Pattern::compile(glob, sensitive).expect("the glob is valid"))
                .collect::<Vec<_>>();
            let set = patterns.iter().cloned().collect::<PatternSet>();
            assert!(set.exact.contains.first_bytes.is_some());
            for path in ["ab", "xab", "a/b", "zzbc", "ccd/x", "c", "", "bca"] {
                assert_set_agrees(&set, &patterns, path.as_bytes(), &|| path.to_owned());
            }
        }
    }

    #[test]
    fn bracket_classes_holding_extglob_bytes_stay_keyed() {
        let globs = ["src/[@(]/foo", "src/[a-z@(]/*.rs", "[@(]/*.toml"];
        let options = PatternOptions::walker();
        let set = PatternSet::new(globs, options).expect("the globs are valid");
        let patterns = globs
            .iter()
            .map(|glob| Pattern::compile(glob, options).expect("valid"))
            .collect::<Vec<_>>();
        assert!(
            set.always.is_empty(),
            "patterns whose extglob-like bytes live inside `[...]` must stay indexed: {:?}",
            set.always
        );
        assert_eq!(set.exact.components.len(), 1);
        assert_eq!(set.exact.extensions.len(), 2);
        for path in [
            "src/@/foo",
            "src/(/foo",
            "src/a/foo",
            "src/@/lib.rs",
            "src/a/lib.rs",
            "src/A/lib.rs",
            "@/Cargo.toml",
            "(/Cargo.toml",
            "a/Cargo.toml",
        ] {
            let shown = || format!("{path:?}");
            assert_set_agrees(&set, &patterns, path.as_bytes(), &shown);
        }
    }

    #[test]
    fn compile_errors_name_the_failed_glob() {
        let error = PatternSet::new(["*.rs", "*.md", "[a-"], PatternOptions::walker())
            .expect_err("the class is never closed");
        assert_eq!(error.index(), 2);
        assert_eq!(
            error.pattern_error(),
            &Pattern::compile("[a-", PatternOptions::walker()).expect_err("same error")
        );
        assert!(error.to_string().starts_with("pattern 2: "));
        assert!(
            error
                .source()
                .is_some_and(|source| source.to_string() == error.pattern_error().to_string())
        );
    }

    #[test]
    fn pattern_set_is_send_sync_clone_debug() {
        fn assert_traits<T: Send + Sync + Clone + fmt::Debug + Default>() {}
        assert_traits::<PatternSet>();
        fn assert_error<T: Send + Sync + Error + Clone + Eq + 'static>() {}
        assert_error::<PatternSetError>();
    }

    #[test]
    fn key_build_hasher_is_seeded_per_instance_and_breaks_fxhash_msb_collisions() {
        let hashes: Vec<u64> = (0..8)
            .map(|_| KeyBuildHasher::new().hash_one(b"node_modules".as_slice()))
            .collect();
        assert!(
            hashes.iter().any(|&hash| hash != hashes[0]),
            "independent KeyBuildHasher instances must not share one deterministic seed: {hashes:?}"
        );

        // Classic FxHash MSB differential pair: flipping bit 63 of word 0 and
        // bit 4 of word 1 always collided in unseeded/seeded FxHash regardless
        // of the initial seed. Folded multiplication must separate them.
        let mut left = [0_u8; 16];
        let mut right = [0_u8; 16];
        left[..8].copy_from_slice(&0x1234_5678_9abc_def0_u64.to_le_bytes());
        left[8..].copy_from_slice(&0x0fed_cba9_8765_4321_u64.to_le_bytes());
        right[..8].copy_from_slice(&(0x1234_5678_9abc_def0_u64 ^ (1_u64 << 63)).to_le_bytes());
        right[8..].copy_from_slice(&(0x0fed_cba9_8765_4321_u64 ^ (1_u64 << 4)).to_le_bytes());
        let hasher = KeyBuildHasher::new();
        assert_ne!(
            hasher.hash_one(left.as_slice()),
            hasher.hash_one(right.as_slice()),
            "folded multiply must not exhibit FxHash MSB differential collisions"
        );
    }

    #[test]
    fn affix_tries_handle_many_lengths_high_fanout_overlaps_and_long_labels() {
        let mut globs = Vec::new();
        // Distinct lengths 1..=64 plus overlapping prefixes.
        for len in 1..=64 {
            globs.push(format!("{}*", "p".repeat(len)));
        }
        // High fanout (>16 children) to exercise binary search in `MemberTrie::next_child`.
        for byte in b'a'..=b'z' {
            globs.push(format!("fan_{}*", char::from(byte)));
            globs.push(format!("**/*{}_end", char::from(byte)));
        }
        // Distinct suffix lengths 1..=64 plus overlapping suffixes.
        for len in 1..=64 {
            globs.push(format!("**/*{}", "s".repeat(len)));
        }
        // Long unbranched compressed labels (both prefix and suffix).
        let long_prefix = format!("{}_pre*", "q".repeat(1_200));
        let long_suffix = format!("**/*suf_{}", "r".repeat(1_200));
        globs.push(long_prefix);
        globs.push(long_suffix);
        // Shared key mapped to multiple members.
        globs.push("fan_a*extra".to_string());
        globs.push("**/*a_end".to_string());

        for options in [
            PatternOptions::walker(),
            PatternOptions::walker().case_insensitive(true),
        ] {
            let set = PatternSet::new(&globs, options).expect("affix trie globs compile");
            let patterns: Vec<Pattern> = globs
                .iter()
                .map(|glob| Pattern::compile(glob, options).expect("valid"))
                .collect();
            let index = if options.case_insensitive {
                &set.folded
            } else {
                &set.exact
            };
            assert!(index.prefixes.len() >= 90);
            assert!(index.suffixes.len() >= 90);

            let long_pre_hit = format!("{}_pre_file", "q".repeat(1_200));
            let long_pre_miss = format!("{}_prx_file", "q".repeat(1_200));
            let long_suf_hit = format!("dir/file_suf_{}", "r".repeat(1_200));
            let long_suf_miss = format!("dir/file_sux_{}", "r".repeat(1_200));
            let candidates = [
                "".to_string(),
                "p".to_string(),
                "pppppp".to_string(),
                "p".repeat(64),
                "p".repeat(65),
                "fan_a_extra".to_string(),
                "fan_z_item".to_string(),
                "fan_0_miss".to_string(),
                "dir/foo_a_end".to_string(),
                "dir/foo_z_end".to_string(),
                "dir/foo_0_end".to_string(),
                "s".to_string(),
                "s".repeat(32),
                "dir/".to_string() + &"s".repeat(64),
                long_pre_hit,
                long_pre_miss,
                long_suf_hit,
                long_suf_miss,
                "FAN_M_UPPER".to_string(),
                "DIR/ITEM_M_END".to_string(),
            ];
            for candidate in &candidates {
                let shown = || format!("{candidate:?} under {options:?}");
                assert_set_agrees(&set, &patterns, candidate.as_bytes(), &shown);
            }
        }
    }

    #[test]
    fn repeated_components_deduplicate_candidates_inline_and_when_spilled() {
        // Inline dedup (<= 4 distinct candidates) with 500 repeated `a`
        // components. Patterns without a trailing `.ext` key by component `a`,
        // so every `a/` triggers the bucket.
        let inline_globs = ["**/a/hit*", "**/a/other*", "**/b/hit*"];
        let inline_set =
            PatternSet::new(inline_globs, PatternOptions::walker()).expect("inline set compiles");
        assert_eq!(inline_set.exact.components.len(), 2);
        let repeated_miss = format!("{}miss", "a/".repeat(500));
        let repeated_hit = format!("{}hit_file", "a/".repeat(500));
        let mut matches = Vec::with_capacity(4);
        inline_set.matches_glob_path_into(&repeated_miss, &mut matches);
        assert!(matches.is_empty());
        assert_eq!(
            matches.capacity(),
            4,
            "non-matching repeated components must not grow caller's matches buffer"
        );
        assert!(!inline_set.is_match_glob_path(&repeated_miss));
        inline_set.matches_glob_path_into(&repeated_hit, &mut matches);
        assert_eq!(matches, [0]);
        assert!(inline_set.is_match_glob_path(&repeated_hit));

        // Spilled dedup (> 4 distinct candidates in component `a`, plus `always`).
        let mut spilled_globs: Vec<String> =
            (0..24).map(|index| format!("**/a/tag{index}_*")).collect();
        spilled_globs.push("**/a/any_*".to_string());
        spilled_globs.push("**/*".to_string());
        let spilled_set = PatternSet::new(&spilled_globs, PatternOptions::walker())
            .expect("spilled set compiles");
        assert_eq!(spilled_set.exact.components.len(), 1);
        let spilled_patterns: Vec<Pattern> = spilled_globs
            .iter()
            .map(|glob| Pattern::compile(glob, PatternOptions::walker()).expect("valid"))
            .collect();
        for path in [
            format!("{}tag17_file", "a/".repeat(200)),
            format!("{}miss", "a/".repeat(200)),
        ] {
            let shown = || format!("{path:?}");
            assert_set_agrees(&spilled_set, &spilled_patterns, path.as_bytes(), &shown);
        }

        // Direct check of `CandidateDedup` for a set larger than
        // `MAX_RETAINED_SEEN_SLOTS`: spilling past the inline array must use
        // the candidate-proportional sparse set rather than growing
        // `SEEN_SCRATCH` to `pattern_count`.
        let before_cap = SEEN_SCRATCH.with(|cell| cell.borrow().stamps.capacity());
        let mut dedup = CandidateDedup::new(100_000);
        for index in [3, 17, 42, 99, 50_000, 99_999] {
            assert!(dedup.mark_new(index));
            assert!(!dedup.mark_new(index));
        }
        assert_eq!(dedup.tls_generation, 0);
        let sparse = &dedup
            .sparse
            .0
            .as_ref()
            .expect("large sets spill to sparse candidate-proportional dedup")
            .0;
        assert_eq!(sparse.len(), 6);
        assert!(
            sparse.capacity() < 128,
            "sparse dedup capacity ({}) must scale with visited candidates, not pattern_count",
            sparse.capacity()
        );
        SEEN_SCRATCH.with(|cell| {
            assert_eq!(
                cell.borrow().stamps.capacity(),
                before_cap,
                "large-set sparse dedup must not grow SEEN_SCRATCH"
            );
        });
    }

    #[test]
    fn large_pattern_set_above_seen_cap_deduplicates_candidates_accurately() {
        let total = MAX_RETAINED_SEEN_SLOTS + 32;
        let options = PatternOptions::walker();
        // Reuse a single compiled pattern for the filler slots so building
        // 16,416 patterns stays instant while exercising `patterns.len() > 16_384`.
        let filler = Pattern::compile("**/unused_component/z*", options).expect("filler compiles");
        let mut patterns = vec![filler; total];
        let active_indices = [0, 5, 1_000, 8_000, MAX_RETAINED_SEEN_SLOTS, total - 1];
        for (slot, &pattern_index) in active_indices.iter().enumerate() {
            let source = if slot % 2 == 0 {
                format!("**/a/hit{slot}*")
            } else {
                format!("**/a/miss{slot}*")
            };
            patterns[pattern_index] = Pattern::compile(source, options).expect("active compiles");
        }
        let set: PatternSet = patterns.into_iter().collect();
        assert_eq!(set.len(), total);

        let repeated_miss = format!("{}other", "a/".repeat(64));
        assert!(!set.is_match_glob_path(&repeated_miss));
        let mut matches = Vec::new();
        set.matches_glob_path_into(&repeated_miss, &mut matches);
        assert!(matches.is_empty());

        let repeated_hit_last = format!("{}hit4_ok", "a/".repeat(64));
        assert!(set.is_match_glob_path(&repeated_hit_last));
        set.matches_glob_path_into(&repeated_hit_last, &mut matches);
        assert_eq!(matches, [MAX_RETAINED_SEEN_SLOTS]);

        let repeated_hit_first = format!("{}hit0_ok", "a/".repeat(64));
        assert!(set.is_match_glob_path(&repeated_hit_first));
        set.matches_glob_path_into(&repeated_hit_first, &mut matches);
        assert_eq!(matches, [0]);
    }

    #[test]
    fn brace_duplicate_affixes_deduplicate_during_member_trie_build() {
        // 16 patterns x 1,024 brace arms sharing the same 128-byte suffix (and
        // another 16 sharing the same 128-byte prefix) must collapse to one
        // trie key each rather than retaining 16,384 duplicate key copies.
        let shared_suffix = "s".repeat(128);
        let shared_prefix = "p".repeat(128);
        let brace_1024 = "{a,b}".repeat(10);
        let mut globs = Vec::with_capacity(32);
        for index in 0..16 {
            globs.push(format!("*{brace_1024}_{index:02}*{shared_suffix}"));
        }
        for index in 0..16 {
            globs.push(format!("{shared_prefix}*{brace_1024}_{index:02}*"));
        }
        let options = PatternOptions::walker();
        let set = PatternSet::new(&globs, options).expect("braced affix globs compile");
        assert_eq!(set.exact.suffixes.len(), 1);
        assert_eq!(set.exact.suffixes.members.len(), 16);
        assert_eq!(set.exact.prefixes.len(), 1);
        assert_eq!(set.exact.prefixes.members.len(), 16);

        let suf_hit = format!("x{}_07_y{shared_suffix}", "a".repeat(10));
        let pre_hit = format!("{shared_prefix}_mid_{}_11", "b".repeat(10));
        let miss = "unrelated/path";
        let mut matches = Vec::new();
        set.matches_glob_path_into(&suf_hit, &mut matches);
        assert_eq!(matches, [7]);
        assert!(set.is_match_glob_path(&suf_hit));
        set.matches_glob_path_into(&pre_hit, &mut matches);
        assert_eq!(matches, [16 + 11]);
        assert!(set.is_match_glob_path(&pre_hit));
        set.matches_glob_path_into(miss, &mut matches);
        assert!(matches.is_empty());
        assert!(!set.is_match_glob_path(miss));
    }

    #[test]
    fn folded_lookup_handles_uppercase_paths_past_stack_and_retained_caps() {
        let globs = ["**/*.rs", "**/target/**", "docs_*", "**/*_tail"];
        let options = PatternOptions::walker().case_insensitive(true);
        let set = PatternSet::new(globs, options).expect("folded set compiles");
        let patterns: Vec<Pattern> = globs
            .iter()
            .map(|glob| Pattern::compile(glob, options).expect("valid"))
            .collect();

        for len in [
            255,
            256,
            257,
            1_024,
            MAX_RETAINED_FOLD_BYTES,
            MAX_RETAINED_FOLD_BYTES + 128,
        ] {
            let prefix = "DOCS_";
            let suffix = "/TARGET/FILE.RS";
            let pad = len.saturating_sub(prefix.len() + suffix.len());
            let hit = format!("{prefix}{}{suffix}", "A".repeat(pad));
            let miss = format!("MISS_{}.TXT", "B".repeat(len.saturating_sub(9)));
            for candidate in [hit, miss] {
                let shown = || format!("len={} head={:?}", candidate.len(), &candidate[..16]);
                assert_set_agrees(&set, &patterns, candidate.as_bytes(), &shown);
            }
        }
        FOLD_SCRATCH.with(|cell| {
            assert!(
                cell.borrow().capacity() <= MAX_RETAINED_FOLD_BYTES,
                "retained fold buffer must not exceed MAX_RETAINED_FOLD_BYTES"
            );
        });
    }
}
