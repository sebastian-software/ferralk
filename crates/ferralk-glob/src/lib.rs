#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![doc = "Portable, byte-first glob matching."]

//! Compiled, byte-first glob patterns with explicit behaviour-changing options.
//!
//! The matcher covers literals, `*`, `?`, `**`, character
//! classes, escapes, leading-period handling, ASCII case folding, nested brace
//! expansion, and Bash-style extglobs.
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
    fmt,
};

use memchr::{memchr, memchr2, memchr3, memmem};

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
mod suffix_word;
mod sweep;

use sweep::{SweepEngine, SweepState};

/// A compiled glob pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    alternatives: Vec<CompiledAlternative>,
    alternative_fast_path: Option<Box<AlternativeFastPath>>,
    path_filter_alternatives: Option<Vec<CompiledAlternative>>,
    can_match_hidden_component_without_match_hidden: bool,
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
    /// Whether byte zero of this candidate starts a path component.
    ///
    /// Extglob alternatives borrow a range from their enclosing candidate.
    /// The range can start in the middle of a component, where a leading dot
    /// is an ordinary byte rather than a hidden-name marker.
    candidate_starts_component: bool,
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
            candidate_starts_component: true,
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
    /// When disabled, a run of stars has the same semantics as one `*`.
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
    /// let source_file = Pattern::compile(
    ///     "src/**/*.{rs,toml}",
    ///     PatternOptions::default()
    ///         .recursive_double_star(true)
    ///         .braces(true),
    /// )?;
    ///
    /// assert!(source_file.is_match_glob_path("src/lib.rs"));
    /// assert!(!source_file.is_match_glob_path("src/generated/lib.rs.bak"));
    /// # Ok::<(), ferralk_glob::PatternError>(())
    /// ```
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
        )?;
        let (viability, offset) = walker_path_analysis(&compiled.alternatives, &mut budget)?;
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
    /// The compiler retains original source provenance through brace expansion
    /// whenever an offending byte survives in an expanded arm. It returns
    /// `None` only when no one source location is available and an embedding
    /// must use its conventional fallback location.
    #[doc(hidden)]
    #[must_use]
    pub const fn walker_path_problem_offset(&self) -> Option<usize> {
        self.walker_path_problem_offset
    }

    /// Whether an explicit literal in some compiled branch can opt a hidden
    /// path component into matching while `match_hidden` is disabled.
    ///
    /// This semantic summary includes brace-expanded and nested extglob
    /// alternatives. Walk planners use it to distinguish a wildcard's hidden
    /// blind spot from includes that can deliberately select through it.
    #[doc(hidden)]
    #[must_use]
    pub const fn can_match_hidden_component_without_match_hidden(&self) -> bool {
        self.can_match_hidden_component_without_match_hidden
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
        provenance_budget: &mut ProvenanceBudget,
        walker_source_provenance: Option<&SourceProvenance>,
        leading_dot_is_normalized: bool,
    ) -> Result<Self, PatternError> {
        if options.braces {
            let parse_options = PatternOptions {
                braces: false,
                ..options
            };
            let mut alternatives = Vec::new();
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
            for alternative in expanded {
                let compiled = Self::compile_within(
                    &alternative.bytes,
                    parse_options,
                    budget,
                    provenance_budget,
                    alternative.source_provenance.as_ref(),
                    leading_dot_is_normalized,
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
        let mut walker_path = WalkerPathShapeBuilder::new(leading_dot_is_normalized);
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
                    walker_path.separator();
                    index += 1;
                }
                b'*' if options.recursive_double_star && pattern.get(index + 1) == Some(&b'*') => {
                    flush_literals(&mut tokens, &mut literals);
                    if pattern.get(index + 2) == Some(&b'/') {
                        tokens.push(Token::RecursivePrefix);
                        walker_path.wildcard();
                        walker_path.separator();
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

    /// Matches the entire candidate byte sequence. Ordinary wildcards may
    /// cross separators, and a leading `./` is compared literally.
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
            return fast_path.is_match(path, self.options);
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
        if fast_paths {
            self.alternative_fast_path = None;
        }
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
                && (!options.component_wildcards || fast_path.supports_component_wildcards())
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

    /// Matches one root-relative path with the zlob list-filter policy used by
    /// [`Pattern::filter_paths`]. A root wildcard may cross separators, while
    /// wildcards after an explicit separator are component-local. One leading
    /// `./` is ignored on both the pattern and candidate.
    #[must_use]
    pub fn is_match_path(&self, path: impl AsRef<[u8]>) -> bool {
        self.matches_path_filter(path.as_ref())
    }

    /// Matches one root-relative filesystem-glob path. Every ordinary
    /// wildcard stays within its path component; recursive `**` remains the
    /// separator-crossing form. This is stricter than [`Pattern::is_match_path`]
    /// at the root component and is suitable for traversal filters. One
    /// leading `./` is ignored on both the pattern and candidate.
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
            && let Some(alternatives) = &self.path_filter_alternatives
        {
            return Self::match_alternatives(alternatives, options, path);
        }
        if let [alternative] = self.alternatives.as_slice()
            && (!options.extglob || alternative.extglob.is_none())
            && let Some(fast_path) = &alternative.fast_path
            && fast_path.supports_component_wildcards()
        {
            return fast_path.is_match(path, options);
        }
        if let Some(fast_path) = &self.alternative_fast_path {
            return fast_path.is_match(path, options, &self.alternatives);
        }
        Self::match_alternatives(&self.alternatives, options, path)
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
        let path = without_leading_dot_slash(path);
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
        let can_match_hidden_component_without_match_hidden = alternatives
            .iter()
            .any(CompiledAlternative::can_match_hidden_component_without_match_hidden);
        let alternative_fast_path =
            AlternativeFastPath::compile(&alternatives, options, budget)?.map(Box::new);
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
            alternative_fast_path,
            path_filter_alternatives,
            can_match_hidden_component_without_match_hidden,
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
        let tokens = source_tokens.to_vec();
        let fast_path = FastPath::compile(
            &tokens,
            PatternOptions {
                component_wildcards: true,
                ..options
            },
        );
        budget.charge(tokens.len(), 0)?;
        let mut provenance_budget = ProvenanceBudget::new();
        let extglob = compile_extglob(&raw, options, budget, &mut provenance_budget, None, false)?;
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
                        StarSemantics::ordinary(!Self::component_wildcard(
                            tokens,
                            token_index,
                            options,
                        )),
                        deferred,
                        work,
                    ),
                    Token::RecursiveStar => Self::advance_star::<SKIP>(
                        token_index,
                        path_index,
                        path,
                        options,
                        StarSemantics::ordinary(true),
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
    /// next component without consuming one.
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
        (!semantics.blocks_hidden_stop
            || options.match_hidden
            || path.get(path_index) != Some(&b'.')
            || !at_component_start(path, path_index, options))
        .then_some((token_index + 1, path_index))
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

#[derive(Clone, Copy)]
struct StarSemantics {
    recursive: bool,
    blocks_hidden_stop: bool,
}

impl StarSemantics {
    const RECURSIVE_PREFIX: Self = Self {
        recursive: true,
        blocks_hidden_stop: false,
    };

    const fn ordinary(recursive: bool) -> Self {
        Self {
            recursive,
            blocks_hidden_stop: true,
        }
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
    RecursiveStar,
    RecursivePrefix,
    Class(Class),
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
    fn can_match_hidden_component_without_match_hidden(&self) -> bool {
        tokens_can_match_hidden_component_without_match_hidden(&self.tokens)
            || self
                .extglob
                .as_ref()
                .is_some_and(CompiledExtglob::can_match_hidden_component_without_match_hidden)
    }

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
                    for nested in &mut alternative.compiled {
                        nested.strip_engines(fast_paths, sweeps, prefilters);
                    }
                }
            }
        }
    }
}

/// Whether a plain compiled alternative can place a literal period at the
/// start of any candidate component.
///
/// Single-byte wildcards can cross a separator under separator-crossing
/// semantics and leave the following token at a component boundary. Stars do
/// not: their zero-width branch is forbidden immediately before a leading
/// period, so a following period literal is not an implicit hidden opt-in.
/// The syntactic `**/` prefix is different because it explicitly advances to
/// a component boundary without consuming candidate bytes. An escaped
/// separator is folded into its literal run, and the byte behind it starts a
/// component for the matcher just as one behind a separator token does.
fn tokens_can_match_hidden_component_without_match_hidden(tokens: &[Token]) -> bool {
    let mut at_component_start = true;
    for token in tokens {
        match token {
            Token::Separator | Token::RecursivePrefix => at_component_start = true,
            Token::Literal(literal) => {
                for &byte in literal {
                    if at_component_start && byte == b'.' {
                        return true;
                    }
                    at_component_start = is_separator(byte);
                }
            }
            Token::Any | Token::Class(_) => at_component_start = true,
            Token::Star | Token::RecursiveStar => at_component_start = false,
        }
    }
    false
}

impl CompiledExtglob {
    fn can_match_hidden_component_without_match_hidden(&self) -> bool {
        // Positive group alternatives are complete compiled branches. Inspect
        // them recursively so nested groups and hidden components after an
        // alternative's separator are represented by compiler semantics too.
        if self.groups.iter().any(|group| {
            group.kind != ExtglobKind::Negated
                && group.alternatives.iter().any(|alternative| {
                    alternative
                        .compiled
                        .iter()
                        .any(CompiledAlternative::can_match_hidden_component_without_match_hidden)
                })
        }) {
            return true;
        }

        let mut at_component_start = true;
        let mut index = 0;
        while let Some(step) = self.steps.get(index) {
            match step {
                ExtglobStep::Byte(b'/') => {
                    at_component_start = true;
                    index += 1;
                }
                ExtglobStep::Byte(byte) => {
                    if at_component_start && *byte == b'.' {
                        return true;
                    }
                    at_component_start = false;
                    index += 1;
                }
                ExtglobStep::Escape { escaped } => {
                    if at_component_start && *escaped == b'.' {
                        return true;
                    }
                    // An escaped separator still ends a component.
                    at_component_start = is_separator(*escaped);
                    index += 2;
                }
                ExtglobStep::Group(group) => {
                    // Even a zero-width branch observes the group's leading-
                    // period guard before it reaches the continuation. If the
                    // group opts into a leading period through an alternative,
                    // the recursive alternative scan above has already made
                    // this summary true; otherwise the group consumes the
                    // component-start privilege just like a wildcard.
                    at_component_start = false;
                    index = self.groups[*group].rest;
                }
                ExtglobStep::Star { next, .. } | ExtglobStep::Class { next, .. } => {
                    at_component_start = false;
                    index = *next;
                }
                ExtglobStep::Any | ExtglobStep::UnclosedGroup { .. } | ExtglobStep::NoMatch => {
                    at_component_start = false;
                    index += 1;
                }
            }
        }
        false
    }
}

/// Compiles the Shift-And engine where the alternative would use it.
///
/// An extglob program keeps its own interpreter, and the literal and
/// deterministic fast paths win the dispatch under every option profile, so
/// building an engine behind either would spend budget on dead tables. Every
/// other shape may reach the general path under another option profile, or
/// retains the engine as a differential oracle behind a starred fast path, and
/// gets an engine when its positions fit one word.
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
                alternatives
                    .first()
                    .and_then(|alternative| alternative.fast_path.as_ref())
                    .is_some_and(|fast_path| fast_path.is_match(path, options))
                    || set.is_match(path, options)
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
        Ok(Some(Self {
            prefixes: AffixTrie::new(&prefixes, false, case_insensitive, budget)?,
            suffixes: AffixTrie::new(&suffixes, true, case_insensitive, budget)?,
        }))
    }

    fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
        let mut prefix_node = 0;
        let mut prefix_len = None;
        for (offset, &byte) in path.iter().enumerate() {
            let Some(next) = self
                .prefixes
                .next(prefix_node, fold_ascii(byte, options.case_insensitive))
            else {
                return false;
            };
            prefix_node = next;
            if self.prefixes.nodes[prefix_node].terminal
                && path.get(offset + 1).is_some_and(|byte| is_separator(*byte))
            {
                prefix_len = Some(offset + 1);
                break;
            }
        }
        let Some(prefix_len) = prefix_len else {
            return false;
        };
        let variable_start = prefix_len + 1;

        let mut suffix_node = 0;
        for (offset, &byte) in path.iter().rev().enumerate() {
            let Some(next) = self
                .suffixes
                .next(suffix_node, fold_ascii(byte, options.case_insensitive))
            else {
                return false;
            };
            suffix_node = next;
            if !self.suffixes.nodes[suffix_node].terminal {
                continue;
            }
            let variable_end = path.len() - offset - 1;
            if variable_start > variable_end {
                continue;
            }
            if star_stops_before_hidden_component(path, variable_end, options) {
                continue;
            }
            if options.match_hidden
                || !contains_hidden_component_in(
                    path,
                    variable_start,
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
    LiteralTokens(Vec<Token>),
    DeterministicTokens(Vec<Token>),
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
        prefix: Vec<Token>,
        suffix: Vec<Token>,
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
            Self::LiteralTokens(_)
                | Self::DeterministicTokens(_)
                | Self::StarSuffix { .. }
                | Self::RecursiveSuffix { .. }
                | Self::RecursivePrefixSuffix { .. }
        )
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
                if options.component_wildcards && next_separator(variable).is_some() {
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

fn contains_hidden_component_in(
    path: &[u8],
    start: usize,
    end: usize,
    candidate_starts_component: bool,
) -> bool {
    let Some(segment) = path.get(start..end) else {
        return false;
    };
    let mut offset = start;
    while let Some(found) = memchr(b'.', &segment[offset - start..]) {
        let index = offset + found;
        if (index == 0 && candidate_starts_component)
            || (index > 0 && is_separator(path[index - 1]))
        {
            return true;
        }
        offset = index + 1;
    }
    false
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
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

    fn separator(&mut self) {
        self.components.push(std::mem::take(&mut self.current));
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
    /// A separator closed an empty component. Such a leading or interior
    /// component is not a spelling the walker can select, even when later
    /// matcher text makes the final component nonempty.
    has_empty_leading_or_interior_component: bool,
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
            has_empty_leading_or_interior_component: false,
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

    fn separator(&mut self) {
        let component = std::mem::take(&mut self.current);
        self.has_empty_leading_or_interior_component |=
            component.kind == WalkerComponentKind::Empty;
        self.record_component(component);
    }

    fn append_shape(&mut self, shape: &WalkerPathShape) {
        for (index, component) in shape.components.iter().copied().enumerate() {
            self.current.append(component);
            if index + 1 != shape.components.len() {
                self.separator();
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
        let selects_candidate = component.kind != WalkerComponentKind::Empty
            && !self.has_empty_leading_or_interior_component;
        self.record_component(component);
        WalkerPathEvaluation {
            selects_candidate,
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
    problem: Option<WalkerPathProblem>,
}

impl WalkerPathEvaluation {
    fn complete_problem(self) -> Option<WalkerPathProblem> {
        self.problem.or_else(|| {
            (!self.selects_candidate).then_some(WalkerPathProblem {
                viability: WalkerPathViability::EmptyComponent,
                offset: None,
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
                ExtglobStep::Byte(b'/') => {
                    for state in &mut states {
                        state.separator();
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
        let mut saw_unselectable = false;
        for state in states {
            let evaluation = state.finish();
            if !evaluation.selects_candidate {
                saw_unselectable = true;
                continue;
            }
            let Some(problem) = evaluation.problem else {
                return Ok(None);
            };
            first_problem.get_or_insert(problem);
        }
        Ok(first_problem.or_else(|| {
            saw_unselectable.then_some(WalkerPathProblem {
                viability: WalkerPathViability::EmptyComponent,
                offset: None,
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
    let (question_marks, has_extglob) = unconditional_compiled_units(pattern, options);
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

/// Counts compiler units whose syntax is outside every matched brace group.
fn unconditional_compiled_units(pattern: &[u8], options: PatternOptions) -> (usize, bool) {
    let mut question_marks = 0_usize;
    let mut has_extglob = false;
    let mut index = 0;
    while index < pattern.len() {
        if options.escape && pattern[index] == b'\\' {
            index += usize::from(index + 1 < pattern.len()) + 1;
            continue;
        }
        if pattern[index] == b'{'
            && let Some(close) = matching_brace(pattern, index, options.escape)
        {
            index = close + 1;
            continue;
        }
        question_marks += usize::from(pattern[index] == b'?');
        has_extglob |= detect_extglob_at(pattern, index).is_some();
        index += 1;
    }
    (question_marks, options.extglob && has_extglob)
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
    steps: Vec<ExtglobStep>,
    groups: Vec<ExtglobGroup>,
    /// Linear-space Thompson machine for eligible positive extglobs.
    /// Negation needs substring-complement semantics, while plain outer stars
    /// retain legacy backtracking quirks; those cases stay on the compatible
    /// explicit worklist below.
    positive_nfa: Option<PositiveExtglobNfa>,
    /// Dense memo state for every interpreter offset that participates in the
    /// recurrence: the entry point plus each group start and continuation.
    /// The byte-indexed step table remains the interpreter's address space.
    memo_state_indices: Vec<usize>,
    memo_state_count: usize,
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
        /// A syntactic recursive `**/` prefix is the sole exemption.
        blocks_leading_period: bool,
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
    },
    Consume {
        matcher: PositiveExtglobMatcher,
        next: usize,
    },
    Star {
        matcher: PositiveExtglobMatcher,
        next: usize,
        blocks_leading_period: bool,
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
    /// The outer extglob interpreter keeps ordinary wildcards component-local
    /// whenever component matching is enabled.
    Extglob,
    /// Every byte of a group alternative is constrained to the current
    /// component when component matching is enabled, including literal
    /// separators and recursively spelled stars.
    Group,
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

impl PositiveExtglobNfa {
    fn compile(
        steps: &[ExtglobStep],
        groups: &[ExtglobGroup],
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
                    ExtglobStep::Byte(byte) => builder
                        .consume(PositiveExtglobMatcher::Literal(*byte), suffixes[index + 1])?,
                    ExtglobStep::Any => builder.consume(
                        PositiveExtglobMatcher::Wildcard(WildcardScope::Extglob),
                        suffixes[index + 1],
                    )?,
                    ExtglobStep::Class { class, next } => builder.consume(
                        PositiveExtglobMatcher::Class(class.clone(), WildcardScope::Extglob),
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
                        builder.group(group, suffixes[group.rest])?
                    }
                    ExtglobStep::UnclosedGroup { byte: b'*' } => {
                        unreachable!("unclosed outer stars keep the compatible interpreter")
                    }
                    ExtglobStep::UnclosedGroup { byte: b'?' } => builder.consume(
                        PositiveExtglobMatcher::Wildcard(WildcardScope::Extglob),
                        suffixes[index + 1],
                    )?,
                    ExtglobStep::UnclosedGroup { byte } => builder
                        .consume(PositiveExtglobMatcher::Literal(*byte), suffixes[index + 1])?,
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
                } => {
                    if *blocks_leading_period
                        && !options.match_hidden
                        && at_component_start
                        && byte == Some(b'.')
                    {
                        continue;
                    }
                    scratch.pending.extend(targets.iter().copied());
                }
                PositiveExtglobState::Star {
                    next,
                    blocks_leading_period,
                    ..
                } => {
                    active.push(state);
                    if !(*blocks_leading_period
                        && !options.match_hidden
                        && at_component_start
                        && byte == Some(b'.'))
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
            Self::GroupLiteral(expected) => {
                (!options.component_wildcards || !is_separator(byte))
                    && bytes_equal(*expected, byte, options.case_insensitive)
            }
            Self::GroupSeparator => !options.component_wildcards && is_separator(byte),
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
        let crosses_separator = match self {
            Self::Extglob | Self::Group => !options.component_wildcards,
        };
        (!is_separator(byte) || crosses_separator)
            && (options.match_hidden || byte != b'.' || !at_component_start)
    }
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
        blocks_leading_period: bool,
    ) -> Result<Option<usize>, PatternError> {
        next.map(|next| {
            self.push(
                PositiveExtglobState::Star {
                    matcher,
                    next,
                    blocks_leading_period,
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
            },
            edge_count,
        )
        .map(Some)
    }

    fn group(
        &mut self,
        group: &ExtglobGroup,
        rest: Option<usize>,
    ) -> Result<Option<usize>, PatternError> {
        let Some(rest) = rest else {
            return Ok(None);
        };
        let blocks_leading_period = !extglob_group_allows_literal_leading_period(group);
        match group.kind {
            ExtglobKind::ExactlyOne => {
                let alternatives = self.alternatives(group, rest)?;
                self.epsilon(alternatives, blocks_leading_period)
            }
            ExtglobKind::Optional => {
                let mut alternatives = self.alternatives(group, rest)?;
                alternatives.push(rest);
                self.epsilon(alternatives, blocks_leading_period)
            }
            ExtglobKind::ZeroOrMore | ExtglobKind::OneOrMore => {
                let hub = self.push(
                    PositiveExtglobState::Epsilon {
                        targets: Vec::new(),
                        blocks_leading_period: false,
                    },
                    0,
                )?;
                let alternatives = self.alternatives(group, hub)?;
                let mut loop_targets = alternatives.clone();
                loop_targets.push(rest);
                let edge_count = loop_targets.len();
                self.budget.charge(edge_count, 0)?;
                self.states[hub] = PositiveExtglobState::Epsilon {
                    targets: loop_targets,
                    blocks_leading_period: false,
                };
                if group.kind == ExtglobKind::ZeroOrMore {
                    self.epsilon(vec![hub], blocks_leading_period)
                } else {
                    self.epsilon(alternatives, blocks_leading_period)
                }
            }
            ExtglobKind::Negated => unreachable!("negated groups do not compile to this NFA"),
        }
    }

    fn alternatives(
        &mut self,
        group: &ExtglobGroup,
        next: usize,
    ) -> Result<Vec<usize>, PatternError> {
        let mut starts = Vec::new();
        for alternative in &group.alternatives {
            for alternative in &alternative.compiled {
                if let Some(start) = self.tokens(&alternative.tokens, next)? {
                    starts.push(start);
                }
            }
        }
        Ok(starts)
    }

    fn tokens(&mut self, tokens: &[Token], next: usize) -> Result<Option<usize>, PatternError> {
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
                    PositiveExtglobMatcher::Wildcard(WildcardScope::Group),
                    start,
                )?,
                Token::Class(class) => self.consume(
                    PositiveExtglobMatcher::Class(class.clone(), WildcardScope::Group),
                    start,
                )?,
                Token::Star => self.star(
                    PositiveExtglobMatcher::Wildcard(WildcardScope::Group),
                    start,
                    true,
                )?,
                Token::RecursiveStar => self.star(
                    PositiveExtglobMatcher::Wildcard(WildcardScope::Group),
                    start,
                    true,
                )?,
                Token::RecursivePrefix => self.star(
                    PositiveExtglobMatcher::Wildcard(WildcardScope::Group),
                    start,
                    false,
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
            provenance_budget,
            walker_source_provenance,
        )?;
        match &step {
            ExtglobStep::Group(group) => pending.push(groups[*group].rest),
            ExtglobStep::Star { next, .. } | ExtglobStep::Class { next, .. } => pending.push(*next),
            // The escaped byte is consumed together with its backslash; the
            // offset between them is never where a walk stands.
            ExtglobStep::Escape { .. } => pending.push(index + 2),
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
    let mut memo_state_indices = vec![usize::MAX; pattern.len() + 1];
    let mut memo_state_count = 0;
    let mut add_memo_state = |offset| {
        if memo_state_indices[offset] == usize::MAX {
            memo_state_indices[offset] = memo_state_count;
            memo_state_count += 1;
        }
    };
    add_memo_state(0);
    for group in &groups {
        add_memo_state(group.start);
        add_memo_state(group.rest);
    }
    let positive_nfa = PositiveExtglobNfa::compile(&steps, &groups, budget)?;
    Ok(Some(CompiledExtglob {
        steps,
        groups,
        positive_nfa,
        memo_state_indices,
        memo_state_count,
        walker_source_provenance: walker_source_provenance.cloned(),
        leading_dot_is_normalized,
    }))
}

/// Classifies one byte offset the way the interpreter classified it.
fn compile_extglob_step(
    pattern: &[u8],
    index: usize,
    options: PatternOptions,
    groups: &mut Vec<ExtglobGroup>,
    budget: &mut IrBudget,
    provenance_budget: &mut ProvenanceBudget,
    walker_source_provenance: Option<&SourceProvenance>,
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
            let alternative_provenance = match walker_source_provenance {
                Some(provenance) => {
                    Some(provenance.slice(start, open + 1 + range.end, provenance_budget, index)?)
                }
                None => None,
            };
            alternatives.push(compile_extglob_alternative(
                &pattern[start..open + 1 + range.end],
                options,
                budget,
                provenance_budget,
                alternative_provenance.as_ref(),
                false,
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
            // Shell extglob grammar gives the final star in a run to a closed
            // `*(` group. Keep the preceding stars as one ordinary wildcard,
            // but leave that last offset for the group compiler instead of
            // greedily swallowing it into the run. An unclosed opener keeps
            // the compatible literal-suffix fallback and remains part of the
            // ordinary star run.
            while pattern.get(next) == Some(&b'*') {
                let closed_group_starts_here = detect_extglob_at(pattern, next).is_some()
                    && closing_extglob_parenthesis(pattern, next + 1, options.escape).is_some();
                if closed_group_starts_here {
                    break;
                }
                next += 1;
            }
            ExtglobStep::Star {
                next,
                blocks_leading_period: !(options.recursive_double_star
                    && next - index == 2
                    && pattern.get(next) == Some(&b'/')),
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
) -> Result<ExtglobAlternative, PatternError> {
    let options = PatternOptions {
        braces: false,
        extglob: false,
        component_wildcards: false,
        root_component_wildcards: false,
        ..options
    };
    let compiled = Pattern::compile_within(
        alternative,
        options,
        budget,
        provenance_budget,
        walker_source_provenance,
        leading_dot_is_normalized,
    )?
    .alternatives;
    let width = fixed_token_width(&compiled);
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
            Token::Star | Token::RecursiveStar | Token::RecursivePrefix => {
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
    /// Deferred `(program, candidate)` continuations. Extglob groups append
    /// work here instead of recursing through the native stack.
    pending: Vec<(usize, usize)>,
    /// Reused match ends for ordinary and repeated extglob groups.
    ends: Vec<usize>,
    /// Temporary ends while one repetition alternative falls back to the
    /// general matcher.
    candidate_ends: Vec<usize>,
    /// Negated groups mark rejected offsets here before queuing the rest.
    excluded: Vec<bool>,
    /// One reusable Shift-And state per sweep-enabled repetition alternative.
    sweep_states: Vec<SweepState>,
    /// The two word rows used while a wide ordinary-group alternative reports
    /// all matching prefix ends. Narrow alternatives need no retained state.
    prefix_sweep_state: Option<SweepState>,
}

/// Borrows the extglob scratch buffers for one match.
struct ExtglobMatchState<'scratch> {
    visited: &'scratch mut Vec<u64>,
    failed: ExtglobFailedStates<'scratch>,
    pending: &'scratch mut Vec<(usize, usize)>,
    #[cfg(test)]
    pending_peak: &'scratch mut usize,
    ends: &'scratch mut Vec<usize>,
    candidate_ends: &'scratch mut Vec<usize>,
    excluded: &'scratch mut Vec<bool>,
    sweep_states: &'scratch mut Vec<SweepState>,
    prefix_sweep_state: &'scratch mut Option<SweepState>,
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
    if let Some(nfa) = &program.positive_nfa {
        return nfa.is_match(path, options);
    }
    EXTGLOB_SCRATCH.with(|cell| match cell.try_borrow_mut() {
        Ok(mut scratch) => {
            let matched = {
                let ExtglobScratch {
                    visited,
                    failed,
                    #[cfg(test)]
                    failed_page_count,
                    #[cfg(test)]
                    failed_state_count,
                    #[cfg(test)]
                    pending_peak,
                    pending,
                    ends,
                    candidate_ends,
                    excluded,
                    sweep_states,
                    prefix_sweep_state,
                } = &mut *scratch;
                visited.clear();
                failed.clear();
                pending.clear();
                ends.clear();
                candidate_ends.clear();
                excluded.clear();
                sweep_states.clear();
                #[cfg(test)]
                {
                    *failed_page_count = 0;
                    *failed_state_count = 0;
                    *pending_peak = 0;
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
                    #[cfg(test)]
                    pending_peak,
                    ends,
                    candidate_ends,
                    excluded,
                    sweep_states,
                    prefix_sweep_state,
                };
                match_extglob_from(program, path, 0, 0, options, &mut state)
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
            scratch.sweep_states.clear();
            if scratch.sweep_states.capacity() > RETAINED_SCRATCH_WORDS {
                scratch.sweep_states.shrink_to(RETAINED_SCRATCH_WORDS);
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
                pending,
                ends,
                candidate_ends,
                excluded,
                sweep_states,
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
                #[cfg(test)]
                pending_peak,
                ends,
                candidate_ends,
                excluded,
                sweep_states,
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
            scratch.sweep_states.capacity(),
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
    rows: &'scratch mut [ExtglobFailedRow],
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
        scratch.resize_with(program.memo_state_count, ExtglobFailedRow::default);
        Self {
            rows: scratch,
            #[cfg(test)]
            page_count,
            #[cfg(test)]
            state_count,
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
    let pending_base = state.pending.len();
    queue_extglob_continuation(program, state, start, start_path_index);
    let mut matched = false;
    while state.pending.len() > pending_base {
        let (start, start_path_index) = state
            .pending
            .pop()
            .expect("the extglob continuation slice is non-empty");
        if match_extglob_task(program, path, start, start_path_index, options, state) {
            matched = true;
            break;
        }
    }
    state.pending.truncate(pending_base);
    matched
}

/// Runs one deterministic outer-program branch. Groups append their possible
/// continuations to the shared work list, so sequential groups consume heap
/// work rather than native call frames. A preceding plain star keeps its local
/// two-pointer backtrack state and queues the group for each reachable split.
fn match_extglob_task(
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
                && at_component_start(path, path_index, options)
            {
                return false;
            }
            match &steps[pattern_index] {
                ExtglobStep::Group(group) => {
                    queue_extglob_group(
                        program,
                        &program.groups[*group],
                        path,
                        path_index,
                        options,
                        state,
                    );
                    if has_star && star_path_index < path.len() {
                        pattern_index = star_pattern_index;
                        star_path_index += 1;
                        path_index = star_path_index;
                        continue;
                    }
                    return false;
                }
                ExtglobStep::Star {
                    next,
                    blocks_leading_period,
                } => {
                    let blocked = *blocks_leading_period
                        && !options.match_hidden
                        && path.get(path_index) == Some(&b'.')
                        && at_component_start(path, path_index, options);
                    if !blocked {
                        star_pattern_index = *next;
                        star_path_index = path_index;
                        has_star = true;
                        pattern_index = *next;
                        continue;
                    }
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
                                || !at_component_start(path, path_index, options))
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
                                || !at_component_start(path, path_index, options))
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
                && at_component_start(path, star_path_index, options)
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
        alternative.compiled.iter().any(|alternative| {
            matches!(
                alternative.tokens.first(),
                Some(Token::Literal(literal)) if literal.first() == Some(&b'.')
            )
        })
    })
}

fn queue_extglob_group(
    program: &CompiledExtglob,
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) {
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
        }
        ExtglobKind::ZeroOrMore => {
            queue_extglob_continuation(program, state, group.rest, path_index);
            matching_extglob_repetition_ends(group, path, path_index, options, state);
            queue_extglob_continuations(program, state, group.rest);
        }
        ExtglobKind::OneOrMore => {
            matching_extglob_repetition_ends(group, path, path_index, options, state);
            queue_extglob_continuations(program, state, group.rest);
        }
        ExtglobKind::Negated => {
            let component_end = extglob_component_end(path, path_index, options);
            state.excluded.resize(component_end - path_index + 1, false);
            state.excluded.fill(false);
            matching_extglob_group_ends(
                group,
                path,
                path_index,
                options,
                state.prefix_sweep_state,
                state.ends,
            );
            for &end in state.ends.iter() {
                state.excluded[end - path_index] = true;
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
    for alternative in &group.alternatives {
        matching_extglob_alternative_ends(
            alternative,
            path,
            path_index,
            options,
            prefix_sweep_state,
            output,
        );
    }
    output.sort_unstable();
    output.dedup();
}

/// All offsets reachable after one or more repetitions of `group`.
///
/// Variable-width alternatives run as streaming sweeps. Whenever a prior
/// match reaches the current offset, their initial boundary is injected into
/// the live state. This evaluates every possible partition in one candidate
/// pass instead of rematching every suffix from every reached offset.
fn matching_extglob_repetition_ends(
    group: &ExtglobGroup,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    state: &mut ExtglobMatchState<'_>,
) {
    let component_end = extglob_component_end(path, path_index, options);
    let position_count = component_end - path_index + 1;
    state.visited.clear();
    let reachable_base = push_visited(state.visited, position_count);
    let matched_base = push_visited(state.visited, position_count);
    visit(state.visited, reachable_base, 0);
    prepare_extglob_sweeps(group, state.sweep_states);
    for offset in 0..position_count {
        let absolute = path_index + offset;
        if visited(state.visited, reachable_base, offset) {
            let mut sweep_index = 0;
            for alternative in &group.alternatives {
                if let Some(width) = alternative.width {
                    let Some(end) = absolute.checked_add(width) else {
                        continue;
                    };
                    if end <= component_end
                        && match_extglob_alternative_exact(
                            alternative,
                            &path[absolute..end],
                            at_component_start(path, absolute, options),
                            options,
                        )
                    {
                        let reached = end - path_index;
                        visit(state.visited, reachable_base, reached);
                        visit(state.visited, matched_base, reached);
                    }
                    continue;
                }
                let mut has_fallback = false;
                for compiled in &alternative.compiled {
                    if let Some(sweep) = &compiled.sweep {
                        let sweep_state = &mut state.sweep_states[sweep_index];
                        sweep_index += 1;
                        sweep.inject_start(sweep_state);
                        if sweep.accepts(sweep_state) {
                            visit(state.visited, matched_base, offset);
                        }
                    } else {
                        has_fallback = true;
                    }
                }
                if has_fallback {
                    state.candidate_ends.clear();
                    matching_extglob_alternative_ends(
                        alternative,
                        path,
                        absolute,
                        options,
                        state.prefix_sweep_state,
                        state.candidate_ends,
                    );
                    for &end in state.candidate_ends.iter() {
                        let reached = end - path_index;
                        visit(state.visited, reachable_base, reached);
                        visit(state.visited, matched_base, reached);
                    }
                }
            }
            debug_assert_eq!(
                sweep_index,
                state.sweep_states.len(),
                "prepared repetition sweeps must align with injected variable-width alternatives"
            );
        }

        if absolute == component_end {
            break;
        }
        let byte = path[absolute];
        let starts_component = at_component_start(path, absolute, options);
        let mut sweep_index = 0;
        for alternative in &group.alternatives {
            for compiled in &alternative.compiled {
                let Some(sweep) = &compiled.sweep else {
                    continue;
                };
                let sweep_state = &mut state.sweep_states[sweep_index];
                sweep_index += 1;
                if sweep.advance(sweep_state, byte, starts_component, options)
                    && sweep.accepts(sweep_state)
                {
                    visit(state.visited, reachable_base, offset + 1);
                    visit(state.visited, matched_base, offset + 1);
                }
            }
        }
        debug_assert_eq!(
            sweep_index,
            state.sweep_states.len(),
            "prepared repetition sweep count must match the advance loop"
        );
    }
    state.ends.clear();
    state.ends.extend(
        (0..position_count)
            .filter(|&offset| visited(state.visited, matched_base, offset))
            .map(|offset| path_index + offset),
    );
}

fn prepare_extglob_sweeps(group: &ExtglobGroup, states: &mut Vec<SweepState>) {
    let mut count = 0;
    for alternative in &group.alternatives {
        for compiled in &alternative.compiled {
            let Some(sweep) = &compiled.sweep else {
                continue;
            };
            if let Some(state) = states.get_mut(count) {
                sweep.reset_state(state);
            } else {
                states.push(sweep.empty_state());
            }
            count += 1;
        }
    }
    states.truncate(count);
}

fn matching_extglob_alternative_ends(
    alternative: &ExtglobAlternative,
    path: &[u8],
    path_index: usize,
    options: PatternOptions,
    prefix_sweep_state: &mut Option<SweepState>,
    output: &mut Vec<usize>,
) {
    let component_end = extglob_component_end(path, path_index, options);
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
            )
        {
            output.push(end);
        }
        return;
    }

    let alternative_options = extglob_alternative_options(path, path_index, options);
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
            for end in path_index..=component_end {
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
) -> PatternOptions {
    let mut options = PatternOptions {
        braces: false,
        extglob: false,
        candidate_starts_component: at_component_start(path, path_index, options),
        ..options
    };
    if options.root_component_wildcards {
        options.component_wildcards = true;
    }
    options
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
) -> bool {
    if alternative.width.is_some_and(|width| width != path.len()) {
        return false;
    }
    let mut options = extglob_alternative_options(path, 0, options);
    options.candidate_starts_component = candidate_starts_component;
    Pattern::match_alternatives(&alternative.compiled, options, path)
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
        AlternativeFastPath, ExtglobStep, FailedStates, FastPath, Pattern, PatternOptions,
        Prefilter, Token, WalkerPathViability, extglob_failed_len, extglob_failed_stats,
        extglob_pending_peak, extglob_scratch_capacities, positive_extglob_scratch_capacities,
        scratch_capacities,
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

    #[test]
    fn compiled_patterns_summarize_explicit_hidden_components() {
        let walker_options = PatternOptions::default()
            .braces(true)
            .recursive_double_star(true)
            .extglob(true);
        let can_match_hidden = |pattern: &str| {
            Pattern::compile(pattern, walker_options)
                .expect("pattern compiles")
                .can_match_hidden_component_without_match_hidden()
        };

        assert!(!can_match_hidden("**/*.txt"));
        assert!(!can_match_hidden("**/*.{rs,toml}"));
        assert!(!can_match_hidden("x/f*.hidden/keep.rs"));
        assert!(!can_match_hidden("visible/**"));
        assert!(!can_match_hidden("**/@(*|visible)/*.txt"));
        assert!(!can_match_hidden("**/!(.gitignore)/*.txt"));
        assert!(can_match_hidden(".hidden/keep.txt"));
        assert!(can_match_hidden("**/.hidden/keep.txt"));
        assert!(can_match_hidden("**/{visible,.hidden}/keep.txt"));
        assert!(can_match_hidden("**/@(visible|.hidden)/keep.txt"));
        assert!(can_match_hidden("visible/@(nested/.hidden|other)/keep.txt"));
        assert!(!can_match_hidden("**/?(visible).hidden/keep.txt"));
        assert!(!can_match_hidden("**/*(visible).hidden/keep.txt"));
        assert!(can_match_hidden("**/?(.visible).hidden/keep.txt"));
        // An escaped separator is folded into the literal run, and the
        // period behind it starts a component for the matcher.
        assert!(can_match_hidden("x/f*\\/.hidden/keep"));
        assert!(can_match_hidden("x/*\\/.hidden/keep"));
        assert!(can_match_hidden("**/f*\\/.hidden/keep"));
        assert!(can_match_hidden("x/@(f*)\\/.hidden/keep"));
        assert!(!can_match_hidden("x/f*\\.hidden/keep"));
        assert!(!can_match_hidden("x/@(f*)\\.hidden/keep"));

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

        // Only a raw spelling that starts with `./` is normalized by walker
        // filters. A compiler-produced leading component is still a real
        // unmatchable dot component. Nor may a nullable arm use an empty
        // leading or interior component to escape that invalid positive arm.
        for source in [
            "@(./a.rs)",
            "{./a.rs}",
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
    fn extglob_recursive_prefix_exemption_requires_exactly_two_stars() {
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
                blocks_leading_period: false
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
                blocks_leading_period: true
            }
        ));
        assert!(!ordinary.is_match(".hidden/foo"));
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
                    for alternative in interpreted
                        .alternatives
                        .iter_mut()
                        .chain(interpreted.path_filter_alternatives.iter_mut().flatten())
                    {
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

    #[test]
    fn outer_stars_keep_extglobs_legacy_backtracking_semantics() {
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
        assert!(star_before_group.is_match_glob_path("/ab"));

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
}
