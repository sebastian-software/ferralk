#![forbid(unsafe_code)]
#![doc = "Portable, byte-first glob matching."]

//! Compiled, byte-first glob patterns with explicit behaviour-changing options.
//!
//! The M1 implementation currently covers literals, `*`, `?`, `**`, character
//! classes, escapes, leading-period handling, ASCII case folding, nested brace
//! expansion, and Bash-style extglobs.
//!
//! Provenance: semantics are ported and differentially checked against zlob
//! v1.6.3, source commit 4bc4da2cbc823d3911b4a1436448687c398977dd, primarily
//! `zig-src/fnmatch.zig`, `zig-src/pattern_context.zig`, and
//! `test/test_fnmatch.zig`. Deliberate differences live in
//! the checked-in corpus and compatibility matrix.

use std::{cell::RefCell, collections::HashSet, error::Error, fmt};

use memchr::{memchr, memchr2, memchr3, memmem};

/// A compiled glob pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    alternatives: Vec<CompiledAlternative>,
    path_filter_alternatives: Option<Vec<CompiledAlternative>>,
    options: PatternOptions,
}

/// Explicit switches that affect glob interpretation.
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
}

impl Default for PatternOptions {
    fn default() -> Self {
        Self {
            braces: false,
            recursive_double_star: false,
            extglob: false,
            match_hidden: false,
            case_insensitive: false,
            escape: true,
            component_wildcards: false,
            root_component_wildcards: false,
        }
    }
}

impl PatternOptions {
    /// Enables nested brace alternatives.
    #[must_use]
    pub const fn braces(mut self, enabled: bool) -> Self {
        self.braces = enabled;
        self
    }

    /// Gives a consecutive `**` recursive, separator-crossing semantics.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError {
    offset: usize,
    message: &'static str,
}

impl PatternError {
    /// Zero-based byte offset of the invalid construct.
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Stable description of the invalid construct.
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
    pub fn compile(
        pattern: impl AsRef<[u8]>,
        options: PatternOptions,
    ) -> Result<Self, PatternError> {
        let pattern = pattern.as_ref();
        if options.braces {
            let parse_options = PatternOptions {
                braces: false,
                ..options
            };
            let alternatives = expand_braces(pattern, options.escape)?
                .into_iter()
                .map(|alternative| {
                    Self::compile(alternative, parse_options).map(|pattern| pattern.alternatives)
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
            return Ok(Self::from_alternatives(alternatives, options));
        }
        let mut tokens = Vec::new();
        let mut literals = Vec::new();
        let mut index = 0;

        while index < pattern.len() {
            match pattern[index] {
                b'/' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Separator);
                    index += 1;
                }
                b'*' if options.recursive_double_star && pattern.get(index + 1) == Some(&b'*') => {
                    flush_literals(&mut tokens, &mut literals);
                    if pattern.get(index + 2) == Some(&b'/') {
                        tokens.push(Token::RecursivePrefix);
                        index += 3;
                    } else {
                        tokens.push(Token::RecursiveStar);
                        index += 2;
                    }
                }
                b'*' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Star);
                    index += 1;
                }
                b'?' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Any);
                    index += 1;
                }
                b'[' => {
                    flush_literals(&mut tokens, &mut literals);
                    let (class, next) = parse_class(pattern, index, options.escape)?;
                    tokens.push(Token::Class(class));
                    index = next;
                }
                b'\\' if options.escape => {
                    if let Some(&escaped) = pattern.get(index + 1) {
                        literals.push(escaped);
                        index += 2;
                    } else {
                        // zlob's fnmatch core treats a trailing backslash as
                        // a literal backslash instead of rejecting the pattern.
                        literals.push(b'\\');
                        index += 1;
                    }
                }
                byte => {
                    literals.push(byte);
                    index += 1;
                }
            }
        }
        flush_literals(&mut tokens, &mut literals);

        Ok(Self::from_alternatives(
            vec![CompiledAlternative {
                raw: pattern.to_vec(),
                fast_path: FastPath::compile(&tokens, options),
                tokens,
            }],
            options,
        ))
    }

    /// Reports whether a pattern is syntactically valid without retaining it.
    pub fn validate(
        pattern: impl AsRef<[u8]>,
        options: PatternOptions,
    ) -> Result<(), PatternError> {
        Self::compile(pattern, options).map(|_| ())
    }

    /// Matches the entire candidate path.
    #[must_use]
    pub fn is_match(&self, path: impl AsRef<[u8]>) -> bool {
        let path = path.as_ref();
        if !self.options.extglob
            && let [alternative] = self.alternatives.as_slice()
            && let Some(fast_path) = &alternative.fast_path
        {
            return fast_path.is_match(path, self.options);
        }
        self.is_match_with(&self.alternatives, self.options, path)
    }

    fn is_match_with(
        &self,
        alternatives: &[CompiledAlternative],
        options: PatternOptions,
        path: &[u8],
    ) -> bool {
        alternatives.iter().any(|alternative| {
            if options.extglob && contains_extglob(&alternative.raw, options.escape) {
                match_extglob(&alternative.raw, path, options)
            } else if let Some(fast_path) = &alternative.fast_path
                && (!options.component_wildcards
                    || matches!(
                        fast_path,
                        FastPath::LiteralTokens(_) | FastPath::DeterministicTokens(_)
                    ))
            {
                fast_path.is_match(path, options)
            } else {
                Self::matches_general(&alternative.tokens, path, options)
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

    /// Matches one root-relative path with the same component-local wildcard
    /// policy used by [`Pattern::filter_paths`].
    #[must_use]
    pub fn is_match_path(&self, path: impl AsRef<[u8]>) -> bool {
        self.matches_path_filter(path.as_ref())
    }

    /// Matches one root-relative filesystem-glob path. Every ordinary
    /// wildcard stays within its path component; recursive `**` remains the
    /// separator-crossing form. This is stricter than [`Pattern::is_match_path`]
    /// at the root component and is suitable for traversal filters.
    #[must_use]
    pub fn is_match_glob_path(&self, path: impl AsRef<[u8]>) -> bool {
        let options = PatternOptions {
            component_wildcards: true,
            root_component_wildcards: true,
            ..self.options
        };
        self.is_match_with(&self.alternatives, options, path.as_ref())
    }

    /// Returns the input paths accepted relative to `base_path`, preserving
    /// the original full paths and caller order. Candidates outside the base
    /// directory are ignored.
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
        let Some(alternatives) = &self.path_filter_alternatives else {
            return self.is_match(path);
        };
        let options = PatternOptions {
            component_wildcards: true,
            ..self.options
        };
        self.is_match_with(alternatives, options, path)
    }

    fn from_alternatives(alternatives: Vec<CompiledAlternative>, options: PatternOptions) -> Self {
        let path_filter_alternatives = alternatives
            .iter()
            .any(|alternative| {
                alternative.raw.starts_with(b"./")
                    || alternative.tokens.windows(2).any(|tokens| {
                        matches!(
                            tokens,
                            [Token::Separator, Token::Any | Token::Star | Token::Class(_)]
                        )
                    })
            })
            .then(|| {
                alternatives
                    .iter()
                    .map(|alternative| Self::compile_path_filter_alternative(alternative, options))
                    .collect()
            });
        Self {
            alternatives,
            path_filter_alternatives,
            options,
        }
    }

    fn compile_path_filter_alternative(
        alternative: &CompiledAlternative,
        options: PatternOptions,
    ) -> CompiledAlternative {
        let leading_dot_slash = alternative.raw.starts_with(b"./")
            && matches!(alternative.tokens.as_slice(), [Token::Literal(dot), Token::Separator, ..] if dot == b".");
        let raw = if leading_dot_slash {
            alternative.raw[2..].to_vec()
        } else {
            alternative.raw.clone()
        };
        let source_tokens = if leading_dot_slash {
            &alternative.tokens[2..]
        } else {
            &alternative.tokens
        };
        let tokens = if options.recursive_double_star {
            source_tokens.to_vec()
        } else {
            path_list_tokens(source_tokens.to_vec())
        };
        let fast_path = FastPath::compile(
            &tokens,
            PatternOptions {
                component_wildcards: true,
                ..options
            },
        );
        CompiledAlternative {
            raw,
            fast_path,
            tokens,
        }
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
                        !Self::component_wildcard(tokens, token_index, options),
                        deferred,
                        work,
                    ),
                    Token::PathStar => Self::advance_star::<SKIP>(
                        token_index,
                        path_index,
                        path,
                        options,
                        false,
                        deferred,
                        work,
                    ),
                    Token::RecursiveStar | Token::RecursivePrefix => Self::advance_star::<SKIP>(
                        token_index,
                        path_index,
                        path,
                        options,
                        true,
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
            && (options.match_hidden || byte != b'.' || !at_component_start(path, path_index)))
        .then_some((token_index + 1, path_index + 1))
    }

    /// Queues the star's repetition branch and returns the branch that stops
    /// consuming here, which the caller explores first.
    fn advance_star<const SKIP: bool>(
        token_index: usize,
        path_index: usize,
        path: &[u8],
        options: PatternOptions,
        recursive: bool,
        deferred: &mut Vec<(usize, usize)>,
        work: &mut StarWork<'_>,
    ) -> Option<(usize, usize)> {
        if let Some(next) = Self::next_star_position::<SKIP>(
            token_index,
            path_index,
            path,
            options,
            recursive,
            work,
        ) {
            deferred.push((token_index, next));
        }
        Some((token_index + 1, path_index))
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
    /// recognised by their generation and reset when first used, so starting a
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
            Token::Star | Token::PathStar | Token::RecursiveStar | Token::RecursivePrefix
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
        && (options.match_hidden || byte != b'.' || !at_component_start(path, path_index))
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
fn next_component_dot(path: &[u8], index: usize, cache: &mut FirstAtOrAfter) -> usize {
    if let Some(found) = cache.get(index) {
        return found;
    }
    if index == 0 && path.first() == Some(&b'.') {
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
    RecursiveStar,
    RecursivePrefix,
    PathStar,
    Class(Class),
}

fn path_list_tokens(tokens: Vec<Token>) -> Vec<Token> {
    let mut normalized = Vec::with_capacity(tokens.len());
    for token in tokens {
        if matches!(token, Token::Star) && matches!(normalized.last(), Some(Token::Star)) {
            normalized.pop();
            normalized.push(Token::PathStar);
        } else {
            normalized.push(token);
        }
    }
    normalized
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
}

/// An allocation-free matcher for a common recursive-prefix/suffix shape.
///
/// It is deliberately narrower than the full token matcher. Keeping it as a
/// compiled variant makes the optimization opt-in by syntax and leaves every
/// other pattern on the corpus-tested general path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FastPath {
    LiteralTokens(Vec<Token>),
    DeterministicTokens(Vec<Token>),
    Star,
    PrefixStar {
        prefix: Vec<u8>,
    },
    StarSuffix {
        suffix: Vec<u8>,
    },
    InfixStar {
        prefix: Vec<u8>,
        suffix: Vec<u8>,
    },
    StaticStar {
        prefix: Vec<Token>,
        suffix: Vec<Token>,
    },
    RecursiveTerminalPrefix {
        prefix: Vec<u8>,
    },
    RecursivePrefixSuffix {
        prefix: Vec<u8>,
        suffix: Vec<u8>,
        suffix_last: u8,
    },
}

impl FastPath {
    fn compile(tokens: &[Token], options: PatternOptions) -> Option<Self> {
        if tokens
            .iter()
            .all(|token| matches!(token, Token::Literal(_) | Token::Separator))
        {
            return Some(Self::LiteralTokens(tokens.to_vec()));
        }
        if tokens.iter().all(|token| {
            matches!(
                token,
                Token::Literal(_) | Token::Separator | Token::Any | Token::Class(_)
            )
        }) {
            return Some(Self::DeterministicTokens(tokens.to_vec()));
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
                    suffix: suffix.clone(),
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
            return Some(Self::StaticStar {
                prefix: tokens[..star_index].to_vec(),
                suffix: tokens[star_index + 1..].to_vec(),
            });
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
        let mut prefix_with_separator = prefix.clone();
        prefix_with_separator.push(b'/');
        Some(Self::RecursivePrefixSuffix {
            prefix: prefix_with_separator,
            suffix: suffix.clone(),
            suffix_last: *suffix.last().expect("literal token is non-empty"),
        })
    }

    fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
        match self {
            Self::LiteralTokens(tokens) => {
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
            Self::DeterministicTokens(tokens) => {
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
                                && at_component_start(path, path_index)
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
                                    && at_component_start(path, path_index))
                            {
                                return false;
                            }
                            path_index += 1;
                        }
                        Token::Star
                        | Token::RecursiveStar
                        | Token::RecursivePrefix
                        | Token::PathStar => return false,
                    }
                }
                path_index == path.len()
            }
            Self::Star => {
                options.match_hidden || !contains_hidden_component_in(path, 0, path.len())
            }
            Self::PrefixStar { prefix } => {
                let Some(variable) = strip_literal_prefix(path, prefix, options.case_insensitive)
                else {
                    return false;
                };
                options.match_hidden
                    || !contains_hidden_component_in(path, path.len() - variable.len(), path.len())
            }
            Self::StarSuffix { suffix } => {
                let Some(variable) = strip_literal_suffix(path, suffix, options.case_insensitive)
                else {
                    return false;
                };
                options.match_hidden || !contains_hidden_component_in(path, 0, variable.len())
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
                options.match_hidden
                    || !contains_hidden_component_in(
                        path,
                        variable_start,
                        variable_start + variable.len(),
                    )
            }
            Self::StaticStar { prefix, suffix } => {
                let Some(variable_start) = match_static_prefix(prefix, path, options) else {
                    return false;
                };
                let Some(variable_end) = match_static_suffix(suffix, path, options) else {
                    return false;
                };
                if variable_start > variable_end {
                    return false;
                }
                options.match_hidden
                    || !contains_hidden_component_in(path, variable_start, variable_end)
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
                            ))
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
                let Some(suffix_start) = path.len().checked_sub(suffix.len()) else {
                    return false;
                };
                if !suffix
                    .iter()
                    .zip(&path[suffix_start..])
                    .all(|(&expected, &actual)| {
                        bytes_equal(expected, actual, options.case_insensitive)
                    })
                {
                    return false;
                }
                let prefix_and_variable = &path[..suffix_start];
                let Some(variable) =
                    strip_literal_prefix(prefix_and_variable, prefix, options.case_insensitive)
                else {
                    return false;
                };
                let variable_start = prefix.len();
                options.match_hidden
                    || !contains_hidden_component_in(
                        path,
                        variable_start,
                        variable_start + variable.len(),
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

fn contains_hidden_component_in(path: &[u8], start: usize, end: usize) -> bool {
    let Some(segment) = path.get(start..end) else {
        return false;
    };
    let mut offset = start;
    while let Some(found) = memchr(b'.', &segment[offset - start..]) {
        let index = offset + found;
        if index == 0 || is_separator(path[index - 1]) {
            return true;
        }
        offset = index + 1;
    }
    false
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

impl PosixClass {
    fn matches(self, byte: u8, case_insensitive: bool) -> bool {
        let folded = fold_ascii(byte, case_insensitive);
        match self {
            Self::Alnum => folded.is_ascii_alphanumeric(),
            Self::Alpha => folded.is_ascii_alphabetic(),
            Self::Ascii => byte.is_ascii(),
            Self::Blank => matches!(byte, b' ' | b'\t'),
            Self::Cntrl => byte.is_ascii_control(),
            Self::Digit => byte.is_ascii_digit(),
            Self::Graph => byte.is_ascii_graphic(),
            Self::Lower => folded.is_ascii_lowercase(),
            Self::Print => byte.is_ascii_graphic() || byte == b' ',
            Self::Punct => byte.is_ascii_punctuation(),
            Self::Space => byte.is_ascii_whitespace(),
            Self::Upper => folded.is_ascii_uppercase(),
            Self::Word => folded.is_ascii_alphanumeric() || folded == b'_',
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
        if byte == b'['
            && pattern.get(index + 1) == Some(&b':')
            && let Some(end) = memmem::find(&pattern[index + 2..], b":]")
            && let Some(class) = parse_posix_class(&pattern[index + 2..index + 2 + end])
        {
            let name_end = index + 2 + end;
            members.extend(class_members(std::mem::take(&mut values)));
            members.push(ClassMember::Posix(class));
            index = name_end + 2;
            continue;
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

fn flush_literals(tokens: &mut Vec<Token>, literals: &mut Vec<u8>) {
    if !literals.is_empty() {
        tokens.push(Token::Literal(std::mem::take(literals)));
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
/// whose expansion would pass [`MAX_BRACE_ALTERNATIVES`], and bounds the work
/// and the memory rather than only the result.
fn expand_braces(pattern: &[u8], escapes: bool) -> Result<Vec<Vec<u8>>, PatternError> {
    let Some(first_open) = first_unescaped_brace(pattern, escapes) else {
        return Ok(vec![pattern.to_vec()]);
    };

    let mut expanded: Vec<Vec<u8>> = Vec::new();
    let mut pending: Vec<Vec<u8>> = vec![pattern.to_vec()];
    while let Some(current) = pending.pop() {
        let Some(open) = first_unescaped_brace(&current, escapes) else {
            expanded.push(current);
            continue;
        };
        let Some(close) = matching_brace(&current, open, escapes) else {
            // zlob treats an unmatched brace as ordinary text.
            expanded.push(current);
            continue;
        };

        let alternatives = split_brace_alternatives(&current[open + 1..close], escapes);
        for alternative in alternatives.iter().rev() {
            if expanded.len() + pending.len() >= MAX_BRACE_ALTERNATIVES {
                return Err(PatternError {
                    // Offsets into a partly expanded pattern would not point
                    // into the caller's, so report where its expansion starts.
                    offset: first_open,
                    message: "too many brace alternatives",
                });
            }
            let mut combined =
                Vec::with_capacity(open + alternative.len() + current.len() - close - 1);
            combined.extend_from_slice(&current[..open]);
            combined.extend_from_slice(alternative);
            combined.extend_from_slice(&current[close + 1..]);
            pending.push(combined);
        }
    }
    Ok(expanded)
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

fn split_brace_alternatives(content: &[u8], escapes: bool) -> Vec<&[u8]> {
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
                alternatives.push(&content[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    alternatives.push(&content[start..]);
    alternatives
}

fn is_separator(byte: u8) -> bool {
    byte == b'/' || (cfg!(windows) && byte == b'\\')
}

fn at_component_start(path: &[u8], index: usize) -> bool {
    index == 0 || path.get(index - 1).is_some_and(|byte| is_separator(*byte))
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

fn contains_extglob(pattern: &[u8], escapes: bool) -> bool {
    let mut index = 0;
    while index + 1 < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += 2;
            continue;
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

fn closing_extglob_parenthesis(pattern: &[u8], open: usize, escapes: bool) -> Option<usize> {
    if pattern.get(open) != Some(&b'(') {
        return None;
    }
    let mut depth = 1_usize;
    let mut index = open + 1;
    while index < pattern.len() {
        if escapes && pattern[index] == b'\\' {
            index += 2;
            continue;
        }
        match pattern[index] {
            b'(' => depth += 1,
            b')' => {
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

fn split_extglob_alternatives(content: &[u8], escapes: bool) -> Vec<&[u8]> {
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
                alternatives.push(&content[start..index]);
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    alternatives.push(&content[start..]);
    alternatives
}

fn match_extglob(pattern: &[u8], path: &[u8], options: PatternOptions) -> bool {
    match_extglob_from(pattern, path, 0, 0, options)
}

fn match_extglob_from(
    pattern: &[u8],
    path: &[u8],
    mut pattern_index: usize,
    mut path_index: usize,
    options: PatternOptions,
) -> bool {
    let mut star_pattern_index = 0_usize;
    let mut star_path_index = 0_usize;
    let mut has_star = false;

    while path_index < path.len() || pattern_index < pattern.len() {
        if pattern_index < pattern.len() {
            if let Some(kind) = detect_extglob_at(pattern, pattern_index) {
                if !options.match_hidden
                    && path.get(path_index) == Some(&b'.')
                    && at_component_start(path, path_index)
                {
                    return false;
                }
                let open = pattern_index + 1;
                if let Some(close) = closing_extglob_parenthesis(pattern, open, options.escape) {
                    let alternatives =
                        split_extglob_alternatives(&pattern[open + 1..close], options.escape);
                    if match_extglob_group(
                        kind,
                        &alternatives,
                        &pattern[close + 1..],
                        path,
                        path_index,
                        options,
                    ) {
                        return true;
                    }
                    if has_star && star_path_index < path.len() {
                        pattern_index = star_pattern_index;
                        star_path_index += 1;
                        path_index = star_path_index;
                        continue;
                    }
                    return false;
                }
            }

            match pattern[pattern_index] {
                b'*' => {
                    pattern_index += 1;
                    while pattern.get(pattern_index) == Some(&b'*') {
                        pattern_index += 1;
                    }
                    star_pattern_index = pattern_index;
                    star_path_index = path_index;
                    has_star = true;
                    continue;
                }
                b'?' if path.get(path_index).is_some_and(|&byte| {
                    (!options.component_wildcards || !is_separator(byte))
                        && (options.match_hidden
                            || byte != b'.'
                            || !at_component_start(path, path_index))
                }) =>
                {
                    pattern_index += 1;
                    path_index += 1;
                    continue;
                }
                b'[' => {
                    if let Ok((class, next)) = parse_class(pattern, pattern_index, options.escape)
                        && path.get(path_index).is_some_and(|&byte| {
                            (!options.component_wildcards || !is_separator(byte))
                                && (options.match_hidden
                                    || byte != b'.'
                                    || !at_component_start(path, path_index))
                                && class.matches(byte, options.case_insensitive)
                        })
                    {
                        pattern_index = next;
                        path_index += 1;
                        continue;
                    }
                }
                b'\\'
                    if options.escape
                        && pattern_index + 1 < pattern.len()
                        && path.get(path_index).is_some_and(|&byte| {
                            bytes_equal(pattern[pattern_index + 1], byte, options.case_insensitive)
                        }) =>
                {
                    pattern_index += 2;
                    path_index += 1;
                    continue;
                }
                byte if path
                    .get(path_index)
                    .is_some_and(|&actual| bytes_equal(byte, actual, options.case_insensitive)) =>
                {
                    pattern_index += 1;
                    path_index += 1;
                    continue;
                }
                _ => {}
            }
        }

        if has_star && star_path_index < path.len() {
            if options.component_wildcards && is_separator(path[star_path_index]) {
                pattern_index = star_pattern_index;
                path_index = star_path_index;
                has_star = false;
                continue;
            }
            if !options.match_hidden
                && path.get(star_path_index) == Some(&b'.')
                && at_component_start(path, star_path_index)
            {
                return false;
            }
            pattern_index = star_pattern_index;
            star_path_index += 1;
            path_index = star_path_index;
            continue;
        }
        return false;
    }
    true
}

fn match_extglob_group(
    kind: ExtglobKind,
    alternatives: &[&[u8]],
    rest: &[u8],
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
) -> bool {
    match kind {
        ExtglobKind::ExactlyOne => {
            match_extglob_alternative(alternatives, rest, path, path_index, options)
        }
        ExtglobKind::Optional => {
            match_extglob_from(rest, path, 0, path_index, options)
                || match_extglob_alternative(alternatives, rest, path, path_index, options)
        }
        ExtglobKind::ZeroOrMore => {
            match_extglob_from(rest, path, 0, path_index, options)
                || match_extglob_repeated(
                    alternatives,
                    rest,
                    path,
                    path_index,
                    options,
                    &mut HashSet::new(),
                )
        }
        ExtglobKind::OneOrMore => match_extglob_repeated(
            alternatives,
            rest,
            path,
            path_index,
            options,
            &mut HashSet::new(),
        ),
        ExtglobKind::Negated => {
            for end in path_index..=extglob_component_end(path, path_index, options) {
                if alternatives.iter().all(|alternative| {
                    !match_extglob_alternative_exact(alternative, &path[path_index..end], options)
                }) && match_extglob_from(rest, path, 0, end, options)
                {
                    return true;
                }
            }
            false
        }
    }
}

fn match_extglob_alternative(
    alternatives: &[&[u8]],
    rest: &[u8],
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
) -> bool {
    alternatives.iter().any(|alternative| {
        (path_index..=extglob_component_end(path, path_index, options)).any(|end| {
            match_extglob_alternative_exact(alternative, &path[path_index..end], options)
                && match_extglob_from(rest, path, 0, end, options)
        })
    })
}

fn match_extglob_repeated(
    alternatives: &[&[u8]],
    rest: &[u8],
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    visited: &mut HashSet<usize>,
) -> bool {
    if !visited.insert(path_index) {
        return false;
    }
    for alternative in alternatives {
        for end in path_index..=extglob_component_end(path, path_index, options) {
            if match_extglob_alternative_exact(alternative, &path[path_index..end], options)
                && (match_extglob_from(rest, path, 0, end, options)
                    || (end > path_index
                        && match_extglob_repeated(alternatives, rest, path, end, options, visited)))
            {
                return true;
            }
        }
    }
    false
}

fn match_extglob_alternative_exact(
    alternative: &[u8],
    path: &[u8],
    options: PatternOptions,
) -> bool {
    let options = PatternOptions {
        braces: false,
        extglob: false,
        ..options
    };
    Pattern::compile(alternative, options).is_ok_and(|pattern| {
        if options.root_component_wildcards {
            pattern.is_match_glob_path(path)
        } else {
            pattern.is_match(path)
        }
    })
}

fn extglob_component_end(path: &[u8], path_index: usize, options: PatternOptions) -> usize {
    if options.component_wildcards {
        path_index
            + path[path_index..]
                .iter()
                .position(|byte| is_separator(*byte))
                .unwrap_or(path.len() - path_index)
    } else {
        path.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{FailedStates, FastPath, Pattern, PatternOptions, Token, scratch_capacities};

    fn compile(pattern: &str) -> Pattern {
        Pattern::compile(pattern, PatternOptions::default()).unwrap()
    }

    #[test]
    fn compiled_patterns_stay_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Pattern>();
        assert_send_sync::<PatternOptions>();
    }

    #[test]
    fn repeated_general_matches_reuse_the_thread_local_scratch() {
        // libtest gives each test its own thread, so the scratch observed here
        // is private to this test.
        let pattern = Pattern::compile(
            "**/*.ts",
            PatternOptions::default().recursive_double_star(true),
        )
        .unwrap();
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
        assert!(
            !Pattern::compile("*a*y", PatternOptions::default())
                .unwrap()
                .is_match(&long)
        );
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

    #[test]
    fn matches_literals_and_component_wildcards() {
        assert!(compile("src/*.rs").is_match("src/lib.rs"));
        assert!(compile("src/?.rs").is_match("src/a.rs"));
        assert!(compile("src/*.rs").is_match("src/bin/main.rs"));
        assert!(compile("*.rs").is_match("lib.rs"));
    }

    #[test]
    fn leading_period_requires_an_explicit_option_or_literal() {
        assert!(!compile("*").is_match(".gitignore"));
        assert!(compile(".*").is_match(".gitignore"));
        assert!(
            Pattern::compile("*", PatternOptions::default().match_hidden(true))
                .unwrap()
                .is_match(".gitignore")
        );
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
            super::expand_braces(b"{a,b}{c,d}", true).unwrap(),
            vec![
                b"ac".to_vec(),
                b"ad".to_vec(),
                b"bc".to_vec(),
                b"bd".to_vec()
            ]
        );
        assert_eq!(
            super::expand_braces(b"{x,{y,z}}!", true).unwrap(),
            vec![b"x!".to_vec(), b"y!".to_vec(), b"z!".to_vec()]
        );
    }

    #[test]
    fn brace_expansion_stops_at_the_alternative_budget() {
        let options = PatternOptions::default().braces(true);
        // Two-way groups make the boundary exact: 2^12 is the budget.
        let within = "{a,b}".repeat(12);
        assert_eq!(
            super::expand_braces(within.as_bytes(), true).unwrap().len(),
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
        // so only the expansion's own recursion used to fail here.
        let options = PatternOptions::default().braces(true);
        let pattern = "{a}".repeat(20_000);
        let compiled = Pattern::compile(&pattern, options).unwrap();
        assert!(compiled.is_match("a".repeat(20_000)));
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
    fn extglob_zero_width_forms_honor_leading_period_policy() {
        let optional = Pattern::compile("?(a|b).c", PatternOptions::default().extglob(true))
            .expect("optional extglob compiles");
        assert!(!optional.is_match(".c"));
        assert!(optional.is_match("a.c"));

        let repeating = Pattern::compile("*(ab).c", PatternOptions::default().extglob(true))
            .expect("repeating extglob compiles");
        assert!(!repeating.is_match(".c"));
        assert!(repeating.is_match("ab.c"));

        let hidden = Pattern::compile(
            "?(a|b).c",
            PatternOptions::default().extglob(true).match_hidden(true),
        )
        .expect("period-enabled optional extglob compiles");
        assert!(hidden.is_match(".c"));
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
        }
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
            Some(FastPath::LiteralTokens(_))
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
            Some(FastPath::DeterministicTokens(_))
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
            }
        }
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
    fn filter_paths_precompiles_its_component_sensitive_ir() {
        let component_pattern = Pattern::compile("src/*.rs", PatternOptions::default())
            .expect("component pattern compiles");
        assert!(component_pattern.path_filter_alternatives.is_some());
        assert!(
            component_pattern
                .path_filter_alternatives
                .as_ref()
                .is_some_and(|alternatives| alternatives
                    .iter()
                    .all(|alternative| alternative.fast_path.is_none()))
        );

        let root_pattern =
            Pattern::compile("*.rs", PatternOptions::default()).expect("root pattern compiles");
        assert!(root_pattern.path_filter_alternatives.is_none());
    }

    #[test]
    fn component_deterministic_fast_path_matches_the_general_matcher() {
        let options = PatternOptions::default().case_insensitive(true);
        let fast = Pattern::compile("src/[ab]?.[Rr][Ss]", options).expect("pattern compiles");
        assert!(
            fast.path_filter_alternatives
                .as_ref()
                .is_some_and(|alternatives| {
                    matches!(
                        alternatives[0].fast_path,
                        Some(FastPath::DeterministicTokens(_))
                    )
                })
        );
        let mut general = fast.clone();
        for alternative in &mut general.alternatives {
            alternative.fast_path = None;
        }
        for alternative in general
            .path_filter_alternatives
            .as_mut()
            .expect("component path filter is compiled")
        {
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
    fn filter_paths_treats_non_recursive_double_star_as_one_component() {
        let pattern = Pattern::compile("**/*.c", PatternOptions::default()).unwrap();
        let paths = [
            "file1.c",
            "dir1/file1.c",
            "dir1/subdir1/file1.c",
            "dir2/file1.c",
        ];
        assert_eq!(
            pattern.filter_paths(&paths),
            vec![&"dir1/file1.c", &"dir2/file1.c"]
        );
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
    }

    #[test]
    fn invalid_syntax_has_a_location() {
        let error = Pattern::compile("[abc", PatternOptions::default()).unwrap_err();
        assert_eq!(error.offset(), 0);
        assert_eq!(error.message(), "unclosed character class");
        assert!(compile("foo\\").is_match("foo\\"));
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
}
