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
//! 3. **Suffix.** The trailing fixed run's last component, looked up once per
//!    distinct key length.
//! 4. **Prefix.** The leading fixed run's first component, likewise per
//!    length.
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
    collections::HashMap,
    error::Error,
    fmt,
    hash::{BuildHasherDefault, Hasher},
};

use memchr::memrchr;

use crate::{
    CompiledAlternative, Pattern, PatternError, PatternOptions, Prefilter, Token, next_separator,
    without_leading_dot_slash,
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
/// lookups per path rather than hundreds of matches. Patterns without such a
/// literal, such as `*` or an extglob, are asked for every path, as they
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
        let mut set = Self {
            patterns,
            ..Self::default()
        };
        for (index, pattern) in set.patterns.iter().enumerate() {
            let keys = pattern
                .alternatives
                .iter()
                .map(Key::of)
                .collect::<Option<Vec<_>>>();
            let Some(keys) = keys else {
                set.always.push(index);
                continue;
            };
            let target = if pattern.options.case_insensitive {
                &mut set.folded
            } else {
                &mut set.exact
            };
            for key in keys {
                target.insert(key, index, pattern.options.case_insensitive);
            }
        }
        set
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
        let found = self.for_each_candidate(entry.view(path), |index| {
            entry.matches(&self.patterns[index], path)
        });
        found
            || self
                .always
                .iter()
                .any(|&index| entry.matches(&self.patterns[index], path))
    }

    fn collect(&self, path: &[u8], entry: Entry, matches: &mut Vec<usize>) {
        matches.clear();
        self.for_each_candidate(entry.view(path), |index| {
            matches.push(index);
            false
        });
        // The keyed candidates come bucket by bucket, and one member may sit
        // in several buckets; the always-asked members are merged in last so
        // a set without keys skips the sort.
        if !matches.is_empty() {
            matches.extend_from_slice(&self.always);
            matches.sort_unstable();
            matches.dedup();
        } else {
            matches.extend_from_slice(&self.always);
        }
        matches.retain(|&index| entry.matches(&self.patterns[index], path));
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
            // Most candidates fit the stack copy; a longer one is folded on
            // the heap rather than refused.
            let mut buffer = [0; 256];
            if let Some(folded) = buffer.get_mut(..view.len()) {
                folded.copy_from_slice(view);
                folded.make_ascii_lowercase();
                self.folded.find(folded, &mut visit)
            } else {
                self.folded.find(&view.to_ascii_lowercase(), &mut visit)
            }
        } else {
            self.folded.find(view, &mut visit)
        }
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
        let stripped = alternative.raw.starts_with(b"./")
            && matches!(
                alternative.tokens.as_slice(),
                [Token::Literal(dot), Token::Separator, ..] if dot == b"."
            );
        let tokens = if stripped {
            &alternative.tokens[2..]
        } else {
            &alternative.tokens[..]
        };
        let fixed = Prefilter::compile(tokens);
        let suffix = last_component(&fixed.suffix);
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
        let prefix = first_component(&fixed.prefix);
        (!stripped && !prefix.is_empty()).then(|| Self::Prefix(prefix.to_vec()))
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

/// Keys of one case policy, each mapped to the members that filed it.
#[derive(Debug, Clone, Default)]
struct KeyIndex {
    extensions: KeyMap,
    components: KeyMap,
    /// Suffix maps, one per key length.
    suffixes: Vec<(usize, KeyMap)>,
    /// Prefix maps, one per key length.
    prefixes: Vec<(usize, KeyMap)>,
}

/// Members by key, with the key lengths present kept aside so a candidate
/// slice of another length is turned away before it is hashed.
#[derive(Debug, Clone, Default)]
struct KeyMap {
    members: HashMap<Box<[u8]>, Vec<usize>, BuildHasherDefault<KeyHasher>>,
    /// Bit `n` is set when a key of length `n` exists; bit 63 stands for
    /// every length from 63 up.
    lengths: u64,
}

impl KeyMap {
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
        let members = self.members.entry(key.into_boxed_slice()).or_default();
        // Members are inserted in index order, so a repeat is the last entry.
        if members.last() != Some(&index) {
            members.push(index);
        }
    }

    fn get(&self, key: &[u8]) -> Option<&[usize]> {
        if self.lengths & Self::length_bit(key.len()) == 0 {
            return None;
        }
        self.members.get(key).map(Vec::as_slice)
    }
}

impl KeyIndex {
    fn is_empty(&self) -> bool {
        self.extensions.is_empty()
            && self.components.is_empty()
            && self.suffixes.is_empty()
            && self.prefixes.is_empty()
    }

    fn insert(&mut self, key: Key, index: usize, fold: bool) {
        let (map, mut bytes) = match key {
            Key::Extension(bytes) => (&mut self.extensions, bytes),
            Key::Component(bytes) => (&mut self.components, bytes),
            Key::Suffix(bytes) => (by_length(&mut self.suffixes, bytes.len()), bytes),
            Key::Prefix(bytes) => (by_length(&mut self.prefixes, bytes.len()), bytes),
        };
        if fold {
            bytes.make_ascii_lowercase();
        }
        map.insert(bytes, index);
    }

    /// Visits the members of every key `view` satisfies, until `visit` returns
    /// `true`.
    fn find(&self, view: &[u8], visit: &mut impl FnMut(usize) -> bool) -> bool {
        let mut hits = |map: &KeyMap, key: &[u8]| {
            map.get(key)
                .is_some_and(|members| members.iter().any(|&index| visit(index)))
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
        for (length, map) in &self.suffixes {
            if let Some(start) = view.len().checked_sub(*length)
                && hits(map, &view[start..])
            {
                return true;
            }
        }
        for (length, map) in &self.prefixes {
            if let Some(prefix) = view.get(..*length)
                && hits(map, prefix)
            {
                return true;
            }
        }
        false
    }
}

fn by_length(maps: &mut Vec<(usize, KeyMap)>, length: usize) -> &mut KeyMap {
    let position = match maps.iter().position(|(existing, _)| *existing == length) {
        Some(position) => position,
        None => {
            maps.push((length, KeyMap::default()));
            maps.len() - 1
        }
    };
    &mut maps[position].1
}

/// A multiply-rotate hash for short byte keys, in the manner of `FxHash`.
///
/// The keys are pattern literals and the lookups candidate bytes, so the
/// default SipHash's resistance to chosen collisions buys nothing: only the
/// pattern author chooses keys, and a pattern author already chooses the
/// cost of every match. Hashing a key is most of a lookup's cost, which is
/// why the set does not pay for SipHash.
#[derive(Debug, Clone, Copy, Default)]
struct KeyHasher(u64);

impl KeyHasher {
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

    fn mix(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(Self::SEED);
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
        self.0
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
}
