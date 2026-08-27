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

use std::{cell::RefCell, collections::HashSet, error::Error, fmt, ops::RangeInclusive};

use memchr::{memchr, memchr2, memchr3, memmem};

mod sweep;

use sweep::SweepEngine;

/// A compiled glob pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    alternatives: Vec<CompiledAlternative>,
    path_filter_alternatives: Option<Vec<CompiledAlternative>>,
    walker_path_viability: WalkerPathViability,
    walker_path_problem_offset: Option<usize>,
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

/// Whether a compiled pattern has an arm a walker can spell as a candidate.
///
/// This is compiler metadata for path-consuming embeddings. It is calculated
/// after brace expansion from the compiled token and extglob branch objects,
/// rather than by parsing the source expression again.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkerPathViability {
    /// At least one compiled arm avoids root-only, `.` and `..` components.
    Viable,
    /// Every compiled arm contains a literal `..` component.
    ParentComponent,
    /// Every compiled arm names only the walk root.
    Root,
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
    pub fn compile(
        pattern: impl AsRef<[u8]>,
        options: PatternOptions,
    ) -> Result<Self, PatternError> {
        let pattern = pattern.as_ref();
        let mut compiled = Self::compile_within(pattern, options, &mut IrBudget::new(), Some(0))?;
        let (viability, offset) = walker_path_analysis(&compiled.alternatives);
        compiled.walker_path_viability = viability;
        compiled.walker_path_problem_offset = offset;
        Ok(compiled)
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
    /// Brace expansion can make source locations ambiguous, in which case the
    /// compiler returns `None` and an embedding may use its conventional
    /// fallback location.
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
    fn compile_within(
        pattern: &[u8],
        options: PatternOptions,
        budget: &mut IrBudget,
        walker_offset_base: Option<usize>,
    ) -> Result<Self, PatternError> {
        if options.braces {
            let parse_options = PatternOptions {
                braces: false,
                ..options
            };
            let mut alternatives = Vec::new();
            let expanded = expand_brace_alternatives(pattern, options.escape)?;
            let preserves_source = expanded.len() == 1 && expanded[0] == pattern;
            for alternative in expanded {
                // Brace expansion produces a new byte sequence, so its
                // branches no longer have one unambiguous source offset. A
                // pattern without an active brace retains its original bytes.
                let compiled = Self::compile_within(
                    &alternative,
                    parse_options,
                    budget,
                    preserves_source.then_some(walker_offset_base).flatten(),
                )?;
                alternatives.extend(compiled.alternatives);
            }
            return Self::from_alternatives(alternatives, options, budget);
        }
        let mut tokens = Vec::new();
        let mut literals = Vec::new();
        // Keep the walker-only path shape beside the token build. In
        // particular an escaped dot and an ordinary dot both become a literal
        // matcher token, but only the latter is a path operation a walker
        // must reject.
        let mut walker_path = WalkerPathState::default();
        let mut index = 0;
        // Charged as the tokens appear rather than per byte: a literal run is
        // one token however long it is, so billing bytes would reject patterns
        // that compile to almost nothing.
        let mut charged = 0;

        while index < pattern.len() {
            budget.charge(tokens.len() - charged, 0)?;
            charged = tokens.len();
            match pattern[index] {
                b'/' => {
                    flush_literals(&mut tokens, &mut literals);
                    tokens.push(Token::Separator);
                    walker_path.separator(index, walker_offset_base);
                    index += 1;
                }
                b'*' if options.recursive_double_star && pattern.get(index + 1) == Some(&b'*') => {
                    flush_literals(&mut tokens, &mut literals);
                    if pattern.get(index + 2) == Some(&b'/') {
                        tokens.push(Token::RecursivePrefix);
                        walker_path.wildcard();
                        walker_path.separator(index + 2, walker_offset_base);
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
                    let (class, next) = parse_class(pattern, index, options.escape)?;
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
                    walker_path.literal(byte, index, walker_offset_base);
                    index += 1;
                }
            }
        }
        flush_literals(&mut tokens, &mut literals);
        budget.charge(tokens.len() - charged, 0)?;

        let extglob = compile_extglob(pattern, options, budget, walker_offset_base)?;
        let fast_path = FastPath::compile(&tokens, options);
        let sweep = compile_sweep(&tokens, &fast_path, extglob.as_ref(), options, budget)?;
        let walker_path_shape = walker_path.finish();
        Self::from_alternatives(
            vec![CompiledAlternative {
                extglob,
                raw: pattern.to_vec(),
                fast_path,
                prefilter: Prefilter::compile(&tokens),
                sweep,
                tokens,
                walker_path_shape,
            }],
            options,
            budget,
        )
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
    /// `prefilters` neutralises the fixed-ends prefilter as well, which turns
    /// the stripped pattern into the bare memoized engine — the oracle the
    /// prefilter and the sweep are both held against.
    fn strip_engines(&mut self, fast_paths: bool, sweeps: bool, prefilters: bool) {
        for alternative in self
            .alternatives
            .iter_mut()
            .chain(self.path_filter_alternatives.iter_mut().flatten())
        {
            alternative.strip_engines(fast_paths, sweeps, prefilters);
        }
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
                && (!options.component_wildcards
                    || matches!(
                        fast_path,
                        FastPath::LiteralTokens(_) | FastPath::DeterministicTokens(_)
                    ))
            {
                fast_path.is_match(path, options)
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
        Self::match_alternatives(&self.alternatives, options, path.as_ref())
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
        Self::match_alternatives(alternatives, options, path)
    }

    fn from_alternatives(
        alternatives: Vec<CompiledAlternative>,
        options: PatternOptions,
        budget: &mut IrBudget,
    ) -> Result<Self, PatternError> {
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
                // A second compiled copy of every alternative, so it costs the
                // budget a second time.
                alternatives
                    .iter()
                    .map(|alternative| {
                        Self::compile_path_filter_alternative(alternative, options, budget)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        Ok(Self {
            alternatives,
            path_filter_alternatives,
            walker_path_viability: WalkerPathViability::Viable,
            walker_path_problem_offset: None,
            options,
        })
    }

    fn compile_path_filter_alternative(
        alternative: &CompiledAlternative,
        options: PatternOptions,
        budget: &mut IrBudget,
    ) -> Result<CompiledAlternative, PatternError> {
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
        budget.charge(tokens.len(), 0)?;
        let extglob = compile_extglob(&raw, options, budget, None)?;
        let sweep = compile_sweep(&tokens, &fast_path, extglob.as_ref(), options, budget)?;
        Ok(CompiledAlternative {
            extglob,
            raw,
            fast_path,
            prefilter: Prefilter::compile(&tokens),
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
    /// The compiled extglob program, present exactly when this alternative
    /// carries extglob syntax and the option is on. Matching borrows it; the
    /// interpreter used to re-derive all of it from `raw` on every call.
    extglob: Option<CompiledExtglob>,
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
    sweep: Option<Box<SweepEngine>>,
}

impl CompiledAlternative {
    /// Removes accelerated engines, descending into extglob alternatives so a
    /// differential run pins one engine for the sub-matches too.
    fn strip_engines(&mut self, fast_paths: bool, sweeps: bool, prefilters: bool) {
        if fast_paths {
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
                for alternative in &mut group.alternatives {
                    for nested in alternative.compiled.iter_mut().flatten() {
                        nested.strip_engines(fast_paths, sweeps, prefilters);
                    }
                }
            }
        }
    }
}

/// Compiles the Shift-And engine where the alternative would use it.
///
/// An extglob program keeps its own interpreter, and the literal and
/// deterministic fast paths win the dispatch under every option profile, so
/// building an engine behind either would spend budget on dead tables. Every
/// other shape may reach the general path — the starred fast paths lose the
/// dispatch once wildcards are component-local — and gets an engine when its
/// positions fit one word.
fn compile_sweep(
    tokens: &[Token],
    fast_path: &Option<FastPath>,
    extglob: Option<&CompiledExtglob>,
    options: PatternOptions,
    budget: &mut IrBudget,
) -> Result<Option<Box<SweepEngine>>, PatternError> {
    if extglob.is_some()
        || matches!(
            fast_path,
            Some(FastPath::LiteralTokens(_) | FastPath::DeterministicTokens(_))
        )
    {
        return Ok(None);
    }
    SweepEngine::compile(tokens, options, budget)
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Prefilter {
    /// Bytes every match starts with, one per byte of the leading
    /// `Literal`/`Separator` run.
    prefix: Vec<u8>,
    /// Bytes every match ends with, one per byte of the trailing run.
    suffix: Vec<u8>,
    /// Bytes the pattern consumes even when every star consumes nothing.
    min_length: usize,
}

impl Prefilter {
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
    }
}

/// Bytes a run of `Literal`/`Separator` tokens spells out, with a separator
/// written as `/`. Any other token in `run` would be a caller bug and is
/// counted as nothing, which keeps the run shorter and the filter weaker.
fn run_bytes(run: &[Token]) -> Vec<u8> {
    run.iter()
        .flat_map(|token| match token {
            Token::Literal(literal) => literal.as_slice(),
            _ => b"/".as_slice(),
        })
        .copied()
        .collect()
}

fn token_min_length(token: &Token) -> usize {
    match token {
        Token::Literal(literal) => literal.len(),
        Token::Separator | Token::Any | Token::Class(_) => 1,
        Token::Star | Token::PathStar | Token::RecursiveStar | Token::RecursivePrefix => 0,
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

const MAX_POSIX_CLASS_NAME_LEN: usize = 6;

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum WalkerComponentKind {
    #[default]
    Empty,
    Dot,
    Parent,
    Other,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct WalkerComponent {
    kind: WalkerComponentKind,
    /// The first literal dot in this component, when it still has a
    /// determinate location in the caller's unexpanded source bytes.
    offset: Option<usize>,
}

impl WalkerComponent {
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
            WalkerComponentKind::Empty => {}
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
#[derive(Clone, Default)]
struct WalkerPathState {
    components: Vec<WalkerComponent>,
    current: WalkerComponent,
}

impl WalkerPathState {
    fn literal(&mut self, byte: u8, index: usize, offset_base: Option<usize>) {
        self.current
            .push_literal(byte, offset_base.map(|base| base + index));
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

    fn separator(&mut self, _index: usize, _offset_base: Option<usize>) {
        self.components.push(self.current);
        self.current = WalkerComponent::default();
    }

    fn append_shape(&mut self, shape: &WalkerPathShape) {
        for (index, component) in shape.components.iter().copied().enumerate() {
            self.current.append(component);
            if index + 1 != shape.components.len() {
                self.separator(0, None);
            }
        }
    }

    fn finish(mut self) -> WalkerPathShape {
        self.components.push(self.current);
        WalkerPathShape {
            components: self.components,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WalkerPathShape {
    components: Vec<WalkerComponent>,
}

impl WalkerPathShape {
    fn has_separator(&self) -> bool {
        self.components.len() > 1
    }

    fn problem(&self) -> Option<WalkerPathProblem> {
        walker_component_problem(&self.components)
    }
}

#[derive(Clone, Copy)]
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
) -> (WalkerPathViability, Option<usize>) {
    let mut first_problem = None;
    for alternative in alternatives {
        match alternative.walker_path_problem() {
            None => return (WalkerPathViability::Viable, None),
            Some(problem) => first_problem.get_or_insert(problem),
        };
    }
    match first_problem {
        Some(problem) => (problem.viability, problem.offset),
        None => (WalkerPathViability::Viable, None),
    }
}

impl CompiledAlternative {
    fn walker_path_problem(&self) -> Option<WalkerPathProblem> {
        match &self.extglob {
            Some(program) => program.walker_path_problem(),
            None => self.walker_path_shape.problem(),
        }
    }
}

impl CompiledExtglob {
    fn walker_path_problem(&self) -> Option<WalkerPathProblem> {
        let mut states = vec![WalkerPathState::default()];
        let mut index = 0;
        while let Some(step) = self.steps.get(index) {
            match step {
                ExtglobStep::Byte(b'/') => {
                    for state in &mut states {
                        state.separator(index, self.walker_offset_base);
                    }
                    index += 1;
                }
                ExtglobStep::Byte(byte) => {
                    for state in &mut states {
                        state.literal(*byte, index, self.walker_offset_base);
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
                    states = self.groups[*group].apply_to(states);
                    index = self.groups[*group].rest;
                }
                ExtglobStep::Star { next } | ExtglobStep::Class { next, .. } => {
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
        for state in states {
            let problem = state.finish().problem()?;
            first_problem.get_or_insert(problem);
        }
        first_problem
    }
}

impl ExtglobGroup {
    fn apply_to(&self, states: Vec<WalkerPathState>) -> Vec<WalkerPathState> {
        match self.kind {
            ExtglobKind::Negated | ExtglobKind::ZeroOrMore | ExtglobKind::OneOrMore => {
                let mut states = states;
                for state in &mut states {
                    // Repetition and negation can generate a non-literal
                    // component. A later outer `/.` or `/..` is still
                    // processed after this placeholder.
                    state.wildcard();
                }
                states
            }
            ExtglobKind::Optional => {
                let mut output = states.clone();
                output.extend(self.exact_states(states));
                output
            }
            ExtglobKind::ExactlyOne => self.exact_states(states),
        }
    }

    fn exact_states(&self, states: Vec<WalkerPathState>) -> Vec<WalkerPathState> {
        let mut output = Vec::new();
        for alternative in &self.alternatives {
            let Some(compiled) = &alternative.compiled else {
                continue;
            };
            for arm in compiled {
                for state in &states {
                    let mut state = state.clone();
                    if arm.walker_path_shape.has_separator() {
                        state.append_shape(&arm.walker_path_shape);
                    } else {
                        // A group that remains inside its containing component
                        // is matcher text (`@(..)`), not a path operation.
                        state.wildcard();
                    }
                    output.push(state);
                }
            }
        }
        if output.is_empty() {
            // An uncompileable arm has no viable matcher branch, but its
            // surrounding program still carries top-level path components
            // that must not be hidden by an empty state set.
            output = states;
            for state in &mut output {
                state.wildcard();
            }
        }
        output
    }
}

fn walker_component_problem(components: &[WalkerComponent]) -> Option<WalkerPathProblem> {
    // The conventional leading `./` spelling is removed by traversal filters
    // before they match candidates. A second dot component remains a real
    // non-normalized operation.
    let components = if components.len() > 1 && components[0].kind == WalkerComponentKind::Dot {
        &components[1..]
    } else {
        components
    };
    if components.iter().all(|component| {
        matches!(
            component.kind,
            WalkerComponentKind::Empty | WalkerComponentKind::Dot
        )
    }) {
        return Some(WalkerPathProblem {
            viability: WalkerPathViability::Root,
            offset: components.iter().find_map(|component| component.offset),
        });
    }
    if let Some(component) = components
        .iter()
        .find(|component| component.kind == WalkerComponentKind::Parent)
    {
        return Some(WalkerPathProblem {
            viability: WalkerPathViability::ParentComponent,
            offset: component.offset,
        });
    }
    let last = components
        .iter()
        .rposition(|component| component.kind != WalkerComponentKind::Empty);
    if last.is_some_and(|index| components[index].kind == WalkerComponentKind::Dot) {
        return Some(WalkerPathProblem {
            viability: WalkerPathViability::TrailingDot,
            offset: components[last.expect("checked above")].offset,
        });
    }
    components.iter().find_map(|component| {
        (component.kind == WalkerComponentKind::Dot).then_some(WalkerPathProblem {
            viability: WalkerPathViability::DotComponent,
            offset: component.offset,
        })
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

/// Bytes brace expansion may write before a pattern is rejected.
///
/// The alternative count alone does not bound the work. Expansion rewrites the
/// whole pattern once per group it resolves, so a pattern of many one-way
/// groups costs the square of its length while expanding to a single
/// alternative: `{a}` repeated 200,000 times is 600 KB and took 11.8 s. And a
/// pattern inside the alternative budget still materialises that budget times
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
/// A unit is one token, one program step, or one class member — a class token
/// owns its member list, so it is charged for both. [`Token`] is 32 bytes and
/// [`ExtglobStep`] is 40, pinned by a test so this arithmetic cannot go stale,
/// which puts the ceiling around 40 MB of compiled program and tens of
/// milliseconds of work. That is orders of magnitude past any real pattern: a
/// language's extension list against a path glob is a few hundred units.
const MAX_COMPILED_IR_UNITS: usize = 1 << 20;

/// Tracks compiled units across every alternative of one [`Pattern::compile`].
///
/// Charged before each allocation rather than after, so a pattern that would
/// pass the ceiling stops there instead of building its way past it first.
/// The one message the compiled-size ceiling reports.
const TOO_MUCH_COMPILED_IR: &str = "pattern compiles to too much";

struct IrBudget {
    remaining: usize,
}

impl IrBudget {
    const fn new() -> Self {
        Self {
            remaining: MAX_COMPILED_IR_UNITS,
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
/// Reports a pattern that asks for more than [`MAX_BRACE_ALTERNATIVES`]
/// alternatives, or whose expansion would write more than
/// [`MAX_BRACE_EXPANSION_BYTES`], so a caller never has to assume the expansion
/// succeeds. Glob syntax is not checked here: an unclosed brace is ordinary
/// text, the way [`Pattern::compile`] treats it, and anything else malformed is
/// reported when the alternative it belongs to is compiled.
pub fn expand_braces(
    pattern: impl AsRef<[u8]>,
    options: PatternOptions,
) -> Result<Vec<Vec<u8>>, PatternError> {
    let pattern = pattern.as_ref();
    if !options.braces {
        return Ok(vec![pattern.to_vec()]);
    }
    expand_brace_alternatives(pattern, options.escape)
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
/// whose expansion would pass [`MAX_BRACE_ALTERNATIVES`], and bounds the memory
/// rather than only the result.
///
/// The bytes written are counted against [`MAX_BRACE_EXPANSION_BYTES`] the same
/// way, and that is what bounds the time: rewriting the whole pattern per group
/// is quadratic in its length even where it expands to one alternative. The
/// running total only grows too, so stopping at the first write that passes the
/// limit rejects exactly the patterns whose finished expansion would.
fn expand_brace_alternatives(pattern: &[u8], escapes: bool) -> Result<Vec<Vec<u8>>, PatternError> {
    let Some(first_open) = first_unescaped_brace(pattern, escapes) else {
        return Ok(vec![pattern.to_vec()]);
    };

    let mut expanded: Vec<Vec<u8>> = Vec::new();
    let mut pending: Vec<Vec<u8>> = vec![pattern.to_vec()];
    let mut written = pattern.len();
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
            let length = open + alternative.len() + current.len() - close - 1;
            written = written.saturating_add(length);
            if written > MAX_BRACE_EXPANSION_BYTES {
                return Err(PatternError {
                    offset: first_open,
                    message: "brace expansion is too large",
                });
            }
            let mut combined = Vec::with_capacity(length);
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
    steps: Vec<ExtglobStep>,
    groups: Vec<ExtglobGroup>,
    walker_offset_base: Option<usize>,
}

/// What the walk does at one byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExtglobStep {
    /// Nothing matches here; the walk falls back to its last star. Also the
    /// filler for an offset no walk reaches.
    NoMatch,
    /// A run of `*`, resuming at the offset after it.
    Star { next: usize },
    /// `?`.
    Any,
    /// A bracket class, resuming at the offset after it.
    Class { class: Class, next: usize },
    /// A backslash with a byte to escape. The escaped byte matches and skips
    /// both offsets; a literal backslash matches and skips only this one,
    /// which leaves the walk on the escaped byte as ordinary text.
    Escape { escaped: u8 },
    /// An ordinary byte.
    Byte(u8),
    /// An extglob group, indexing [`CompiledExtglob::groups`].
    Group(usize),
    /// A group opener whose parenthesis never closes. It still refuses a
    /// leading period, then reads `byte` as ordinary text.
    UnclosedGroup { byte: u8 },
}

/// One alternative of a group, compiled as a whole-candidate pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExtglobAlternative {
    /// `None` is an alternative whose syntax does not compile, which matched
    /// nothing before either.
    compiled: Option<Vec<CompiledAlternative>>,
    /// Byte width when every token consumes a fixed count, which lets the scan
    /// over candidate end offsets visit one offset instead of the component.
    width: Option<usize>,
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
}

/// Compiles the extglob program for `pattern`, or `None` when it has no group.
///
/// This subsumes the scan `is_match` used to repeat on every call.
fn compile_extglob(
    pattern: &[u8],
    options: PatternOptions,
    budget: &mut IrBudget,
    walker_offset_base: Option<usize>,
) -> Result<Option<CompiledExtglob>, PatternError> {
    if !options.extglob || !contains_extglob(pattern, options.escape) {
        return Ok(None);
    }
    // The step table is one entry per byte offset whatever the walk reaches, so
    // it is charged in full before it is allocated.
    budget.charge(pattern.len(), 0)?;
    let mut steps = vec![ExtglobStep::NoMatch; pattern.len()];
    let mut compiled = vec![false; pattern.len()];
    let mut groups = Vec::new();
    let mut pending = vec![0_usize];
    while let Some(index) = pending.pop() {
        if index >= pattern.len() || compiled[index] {
            continue;
        }
        compiled[index] = true;
        let step = compile_extglob_step(
            pattern,
            index,
            options,
            &mut groups,
            budget,
            walker_offset_base,
        )?;
        match &step {
            ExtglobStep::Group(group) => pending.push(groups[*group].rest),
            ExtglobStep::Star { next } | ExtglobStep::Class { next, .. } => pending.push(*next),
            // A literal backslash consumes one offset and leaves the walk on
            // the escaped byte, which is read as ordinary text from there.
            ExtglobStep::Escape { .. } => {
                pending.push(index + 1);
                pending.push(index + 2);
            }
            ExtglobStep::Any | ExtglobStep::Byte(_) | ExtglobStep::UnclosedGroup { .. } => {
                pending.push(index + 1);
            }
            // The matcher still stops at this state, but compiling the suffix
            // records the same outer path structure for embeddings that need
            // to classify every generated branch.
            ExtglobStep::NoMatch => pending.push(index + 1),
        }
        steps[index] = step;
    }
    Ok(Some(CompiledExtglob {
        steps,
        groups,
        walker_offset_base,
    }))
}

/// Classifies one byte offset the way the interpreter classified it.
fn compile_extglob_step(
    pattern: &[u8],
    index: usize,
    options: PatternOptions,
    groups: &mut Vec<ExtglobGroup>,
    budget: &mut IrBudget,
    walker_offset_base: Option<usize>,
) -> Result<ExtglobStep, PatternError> {
    if let Some(kind) = detect_extglob_at(pattern, index) {
        let open = index + 1;
        let Some(close) = closing_extglob_parenthesis(pattern, open, options.escape) else {
            return Ok(ExtglobStep::UnclosedGroup {
                byte: pattern[index],
            });
        };
        let mut alternatives = Vec::new();
        for range in split_extglob_alternatives(&pattern[open + 1..close], options.escape) {
            let start = open + 1 + range.start;
            alternatives.push(compile_extglob_alternative(
                &pattern[start..open + 1 + range.end],
                options,
                budget,
                walker_offset_base.map(|base| base + start),
            )?);
        }
        groups.push(ExtglobGroup {
            kind,
            alternatives,
            start: index,
            rest: close + 1,
        });
        return Ok(ExtglobStep::Group(groups.len() - 1));
    }
    Ok(match pattern[index] {
        b'*' => {
            let mut next = index + 1;
            while pattern.get(next) == Some(&b'*') {
                next += 1;
            }
            ExtglobStep::Star { next }
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
    walker_offset_base: Option<usize>,
) -> Result<ExtglobAlternative, PatternError> {
    let options = PatternOptions {
        braces: false,
        extglob: false,
        component_wildcards: false,
        root_component_wildcards: false,
        ..options
    };
    // A syntax error in one alternative is not a compile failure — it is an
    // alternative that matches nothing, as it always was. Running out of budget
    // is a different thing and has to reach the caller.
    let compiled = match Pattern::compile_within(alternative, options, budget, walker_offset_base) {
        Ok(pattern) => Some(pattern.alternatives),
        Err(error) if error.message() == TOO_MUCH_COMPILED_IR => return Err(error),
        Err(_) => None,
    };
    let width = compiled.as_deref().and_then(fixed_token_width);
    Ok(ExtglobAlternative { compiled, width })
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
            Token::Star | Token::PathStar | Token::RecursiveStar | Token::RecursivePrefix => {
                return None;
            }
        };
    }
    Some(width)
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
    /// Program/path states explored by this match. Kept sparse so a large
    /// pattern and candidate do not allocate their full Cartesian product.
    failed: HashSet<(usize, usize)>,
    /// Deferred continuation positions, partitioned by active repetition
    /// calls so sequential groups reuse one allocation without sharing work.
    repeated: Vec<usize>,
}

/// Borrows the extglob scratch buffers for one match.
struct ExtglobMatchState<'scratch> {
    visited: &'scratch mut Vec<u64>,
    failed: &'scratch mut HashSet<(usize, usize)>,
    repeated: &'scratch mut Vec<usize>,
}

thread_local! {
    static EXTGLOB_SCRATCH: RefCell<ExtglobScratch> = RefCell::new(ExtglobScratch::default());
}

/// Runs a compiled extglob program on the thread's reusable visited buffer.
///
/// That buffer is separate from the general matcher's scratch on purpose: an
/// alternative is matched through [`Pattern::match_alternatives`], which takes
/// the general scratch, so holding both at once would push every sub-match
/// onto the re-entrancy fallback and allocate there.
fn match_extglob_program(program: &CompiledExtglob, path: &[u8], options: PatternOptions) -> bool {
    EXTGLOB_SCRATCH.with(|cell| match cell.try_borrow_mut() {
        Ok(mut scratch) => {
            let matched = {
                let ExtglobScratch {
                    visited,
                    failed,
                    repeated,
                } = &mut *scratch;
                visited.clear();
                failed.clear();
                repeated.clear();
                let mut state = ExtglobMatchState {
                    visited,
                    failed,
                    repeated,
                };
                match_extglob_from(program, path, 0, 0, options, &mut state)
            };
            scratch.visited.clear();
            if scratch.visited.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.visited.shrink_to(RETAINED_SCRATCH_WORDS);
            }
            if scratch.failed.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.failed.shrink_to(RETAINED_SCRATCH_WORDS);
            }
            if scratch.repeated.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.repeated.shrink_to(RETAINED_SCRATCH_WORDS);
            }
            matched
        }
        Err(_) => {
            let mut scratch = ExtglobScratch::default();
            let ExtglobScratch {
                visited,
                failed,
                repeated,
            } = &mut scratch;
            let mut state = ExtglobMatchState {
                visited,
                failed,
                repeated,
            };
            match_extglob_from(program, path, 0, 0, options, &mut state)
        }
    })
}

/// Capacity the calling thread's extglob visited buffer currently holds.
#[cfg(test)]
fn extglob_visited_capacity() -> usize {
    EXTGLOB_SCRATCH.with(|cell| cell.borrow().visited.capacity())
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

fn match_extglob_from(
    program: &CompiledExtglob,
    path: &[u8],
    start: usize,
    start_path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) -> bool {
    let steps = &program.steps;
    let mut pattern_index = start;
    let mut path_index = start_path_index;
    let mut star_pattern_index = 0_usize;
    let mut star_path_index = 0_usize;
    let mut has_star = false;

    // Every recursive branch reaches a program step with no inherited star
    // backtrack point. It can therefore be shared across sequential groups and
    // within one repetition: once a suffix has been explored, another
    // partition cannot make it succeed.
    if !state.failed.insert((pattern_index, path_index)) {
        return false;
    }

    while path_index < path.len() || pattern_index < steps.len() {
        if pattern_index < steps.len() {
            let leading_period_is_forbidden = match &steps[pattern_index] {
                ExtglobStep::Group(group) => {
                    !extglob_group_allows_literal_leading_period(&program.groups[*group])
                }
                ExtglobStep::UnclosedGroup { .. } => true,
                _ => false,
            };
            if leading_period_is_forbidden
                && !options.match_hidden
                && path.get(path_index) == Some(&b'.')
                && at_component_start(path, path_index)
            {
                return false;
            }
            match &steps[pattern_index] {
                ExtglobStep::Group(group) => {
                    if match_extglob_group(
                        program,
                        &program.groups[*group],
                        path,
                        path_index,
                        options,
                        state,
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
                ExtglobStep::Star { next } => {
                    star_pattern_index = *next;
                    star_path_index = path_index;
                    has_star = true;
                    pattern_index = *next;
                    continue;
                }
                ExtglobStep::UnclosedGroup { byte: b'*' } => {
                    star_pattern_index = pattern_index + 1;
                    star_path_index = path_index;
                    has_star = true;
                    pattern_index += 1;
                    continue;
                }
                ExtglobStep::Any | ExtglobStep::UnclosedGroup { byte: b'?' } => {
                    if path.get(path_index).is_some_and(|&byte| {
                        (!options.component_wildcards || !is_separator(byte))
                            && (options.match_hidden
                                || byte != b'.'
                                || !at_component_start(path, path_index))
                    }) {
                        pattern_index += 1;
                        path_index += 1;
                        continue;
                    }
                    // The wildcard arm falling through leaves `?` readable as
                    // an ordinary byte, which only a literal `?` can match.
                    if path
                        .get(path_index)
                        .is_some_and(|&actual| bytes_equal(b'?', actual, options.case_insensitive))
                    {
                        pattern_index += 1;
                        path_index += 1;
                        continue;
                    }
                }
                ExtglobStep::Class { class, next } => {
                    if path.get(path_index).is_some_and(|&byte| {
                        (!options.component_wildcards || !is_separator(byte))
                            && (options.match_hidden
                                || byte != b'.'
                                || !at_component_start(path, path_index))
                            && class.matches(byte, options.case_insensitive)
                    }) {
                        pattern_index = *next;
                        path_index += 1;
                        continue;
                    }
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
                    if path
                        .get(path_index)
                        .is_some_and(|&byte| bytes_equal(b'\\', byte, options.case_insensitive))
                    {
                        pattern_index += 1;
                        path_index += 1;
                        continue;
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
        alternative.compiled.as_ref().is_some_and(|alternatives| {
            alternatives.iter().any(|alternative| {
                matches!(
                    alternative.tokens.first(),
                    Some(Token::Literal(literal)) if literal.first() == Some(&b'.')
                )
            })
        })
    })
}

fn match_extglob_group(
    program: &CompiledExtglob,
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) -> bool {
    match group.kind {
        ExtglobKind::ExactlyOne => {
            match_extglob_alternative(program, group, path, path_index, options, state)
        }
        ExtglobKind::Optional => {
            match_extglob_from(program, path, group.rest, path_index, options, state)
                || match_extglob_alternative(program, group, path, path_index, options, state)
        }
        ExtglobKind::ZeroOrMore => {
            match_extglob_from(program, path, group.rest, path_index, options, state)
                || match_extglob_repetition(program, group, path, path_index, options, state)
        }
        ExtglobKind::OneOrMore => {
            match_extglob_repetition(program, group, path, path_index, options, state)
        }
        ExtglobKind::Negated => {
            for end in path_index..=extglob_component_end(path, path_index, options) {
                if group.alternatives.iter().all(|alternative| {
                    !match_extglob_alternative_exact(alternative, &path[path_index..end], options)
                }) && match_extglob_from(program, path, group.rest, end, options, state)
                {
                    return true;
                }
            }
            false
        }
    }
}

/// Runs a repetition under its own visited frame.
///
/// The frame replaces the hash set the interpreter built per group encounter.
/// It is a stack because a repetition can reach another one through its rest,
/// and each needs its own set of already-tried positions.
fn match_extglob_repetition(
    program: &CompiledExtglob,
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) -> bool {
    let base = push_visited(state.visited, path.len() + 1);
    let matched = match_extglob_repeated(program, group, path, path_index, options, state, base);
    state.visited.truncate(base);
    matched
}

fn match_extglob_repeated(
    program: &CompiledExtglob,
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
    base: usize,
) -> bool {
    let work_base = state.repeated.len();
    state.repeated.push(path_index);
    let mut matched = false;
    while state.repeated.len() > work_base {
        let path_index = state
            .repeated
            .pop()
            .expect("the non-empty repetition work slice has a position");
        if !visit(state.visited, base, path_index) {
            continue;
        }
        for alternative in &group.alternatives {
            for end in extglob_alternative_ends(alternative, path, path_index, options)
                .into_iter()
                .flatten()
            {
                if match_extglob_alternative_exact(alternative, &path[path_index..end], options) {
                    if match_extglob_from(program, path, group.rest, end, options, state) {
                        matched = true;
                        break;
                    }
                    if end > path_index && state.failed.insert((group.start, end)) {
                        state.repeated.push(end);
                    }
                }
            }
            if matched {
                break;
            }
        }
        if matched {
            break;
        }
    }
    state.repeated.truncate(work_base);
    matched
}

fn match_extglob_alternative(
    program: &CompiledExtglob,
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) -> bool {
    for alternative in &group.alternatives {
        for end in extglob_alternative_ends(alternative, path, path_index, options)
            .into_iter()
            .flatten()
        {
            if match_extglob_alternative_exact(alternative, &path[path_index..end], options)
                && match_extglob_from(program, path, group.rest, end, options, state)
            {
                return true;
            }
        }
    }
    false
}

/// Matches one compiled alternative against the whole of `path`.
///
/// The component policy comes from the caller, reproducing the entry point the
/// per-match compile picked: `is_match_glob_path` once the root is
/// component-local, `is_match` otherwise.
fn match_extglob_alternative_exact(
    alternative: &ExtglobAlternative,
    path: &[u8],
    options: PatternOptions,
) -> bool {
    if alternative.width.is_some_and(|width| width != path.len()) {
        return false;
    }
    let Some(alternatives) = &alternative.compiled else {
        return false;
    };
    let mut options = PatternOptions {
        braces: false,
        extglob: false,
        ..options
    };
    if options.root_component_wildcards {
        options.component_wildcards = true;
    }
    Pattern::match_alternatives(alternatives, options, path)
}

/// End offsets worth trying for one alternative.
///
/// A fixed-width alternative can only accept a substring of that width, so
/// every other offset in the component would be rejected on length alone.
fn extglob_alternative_ends(
    alternative: &ExtglobAlternative,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
) -> Option<RangeInclusive<usize>> {
    let component_end = extglob_component_end(path, path_index, options);
    let Some(width) = alternative.width else {
        return Some(path_index..=component_end);
    };
    // `None` where a fixed-width alternative cannot fit at all.
    let end = path_index.checked_add(width)?;
    (end <= component_end).then_some(end..=end)
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
    use super::{
        FailedStates, FastPath, Pattern, PatternOptions, Prefilter, Token, WalkerPathViability,
        extglob_visited_capacity, scratch_capacities,
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
        pattern.strip_engines(false, true, false);
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

        // Brace expansion and extglob alternatives are already compiled when
        // viability is calculated. A dead arm cannot invalidate a compiled
        // sibling that can name a walker candidate.
        assert_eq!(
            viability("{dead/../branch,src/main.rs}", options),
            WalkerPathViability::Viable
        );
        assert_eq!(
            viability("@(dead/../branch|src/main.rs)", options),
            WalkerPathViability::Viable
        );
        assert_eq!(
            viability("{dead/{nested/../branch},src/main.rs}", options),
            WalkerPathViability::Viable
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
            "@(dead/[)]]/../x|src/main.rs)",
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

        // 4096 alternatives of a kilobyte each: inside the alternative budget
        // and inside the expansion byte budget, but millions of compiled units.
        let extglob = format!("@(a|b){}{}", "x".repeat(1_000), "{a,b}".repeat(12));
        let error = Pattern::compile(&extglob, options).unwrap_err();
        assert_eq!(error.message(), "pattern compiles to too much");

        // The same dimension without extglob: one token per wildcard byte.
        let wildcards = format!("{}{}", "?".repeat(1_000), "{a,b}".repeat(12));
        let error =
            Pattern::compile(&wildcards, PatternOptions::default().braces(true)).unwrap_err();
        assert_eq!(error.message(), "pattern compiles to too much");

        // A literal run is one token however long, so the same alternative
        // count over the same number of bytes compiles.
        let literals = format!("{}{}", "x".repeat(1_000), "{a,b}".repeat(12));
        assert!(Pattern::compile(&literals, PatternOptions::default().braces(true)).is_ok());

        // The budget is a compile-time ceiling, not a syntax rule.
        assert!(Pattern::compile(&wildcards, PatternOptions::default()).is_ok());

        // A separator before a wildcard makes the pattern carry a second
        // compiled copy for the path-filter reading, which costs the budget a
        // second time. The same pattern without one compiles.
        let braces = PatternOptions::default().braces(true);
        let single = format!("{}{}", "{a,b}".repeat(9), "?".repeat(700));
        let doubled = format!("{}/{}", "{a,b}".repeat(9), "?".repeat(700));
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
        // global extglob state memo without a wall-clock assertion.
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
    fn extglob_matches_reuse_the_thread_local_visited_buffer() {
        // The repetition kinds are the ones that used to build a hash set per
        // group encounter.
        let options = PatternOptions::default().extglob(true);
        let pattern = Pattern::compile("+(a|ab)c", options).expect("repetition compiles");
        let candidate = "aababc";
        for _ in 0..8 {
            assert!(pattern.is_match(candidate));
        }
        let warm = extglob_visited_capacity();
        assert!(warm > 0, "a repetition must have used the visited buffer");
        for _ in 0..1_000 {
            assert!(pattern.is_match(candidate));
        }
        assert_eq!(
            extglob_visited_capacity(),
            warm,
            "steady-state extglob matching must not grow the visited buffer"
        );
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
        let alternatives = pattern
            .alternatives
            .iter_mut()
            .chain(pattern.path_filter_alternatives.iter_mut().flatten());
        for alternative in alternatives {
            alternative.fast_path = None;
        }
    }

    /// The same pattern with the prefilter neutralised, so the engine decides
    /// on its own.
    fn without_the_prefilter(pattern: &Pattern) -> Pattern {
        let mut unfiltered = pattern.clone();
        let alternatives = unfiltered
            .alternatives
            .iter_mut()
            .chain(unfiltered.path_filter_alternatives.iter_mut().flatten());
        for alternative in alternatives {
            alternative.prefilter = Prefilter::default();
        }
        unfiltered
    }

    /// The prefilter may only reject what the general engine rejects.
    ///
    /// Every candidate is decided twice through the same engine — once with the
    /// compiled prefilter in front of it, once with the prefilter neutralised —
    /// across the three entry points that reach it, because each supplies its
    /// own `PatternOptions` and the prefilter is derived without them. The fast
    /// paths are removed from both copies so nothing routes around the engine
    /// and turns the comparison into a tautology.
    #[test]
    fn the_prefilter_rejects_only_what_the_general_engine_rejects() {
        let plain = PatternOptions::default();
        let recursive = PatternOptions::default().recursive_double_star(true);
        let folded = recursive.case_insensitive(true);
        let cases: [(&str, PatternOptions); 16] = [
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
        ];
        let mut candidates = byte_words(b"ab./", 4);
        candidates.push(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec());
        candidates.push(b"a/b/a/b/a/b.b".to_vec());
        candidates.push(b"A/B/A.B".to_vec());

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

        // Past the position cap the memoized matcher stays responsible, and
        // both engines still answer alike.
        let oversized_source = [b"a".repeat(70), b"*B".to_vec()].concat();
        let oversized = Pattern::compile(&oversized_source, PatternOptions::default()).unwrap();
        assert!(oversized.alternatives[0].sweep.is_none());
        let candidate = [b"a".repeat(70), b"xB".to_vec()].concat();
        assert!(oversized.is_match(&candidate));
        assert!(oversized.engines_agree(&candidate));
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
}
