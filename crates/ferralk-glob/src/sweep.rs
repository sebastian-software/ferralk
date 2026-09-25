//! Bit-parallel Shift-And engine for the general match path.
//!
//! The general matcher explores the token/path state graph one state at a
//! time, bounded by the [`FailedStates`](crate::FailedStates) memo to
//! `tokens x path` visits. This module walks the same graph column by column
//! instead: every token expands to byte-consuming *positions*, the set of
//! reachable positions lives in a compact bitset, and each candidate byte
//! advances it one machine word at a time. The state is proportional to the
//! compiled pattern rather than the pattern-by-candidate product.
//!
//! This is not a regex engine and stays inside ADR-0013: there is no program
//! to dispatch, no captures, no search — only the token list this crate
//! already compiles, laid out as a Glushkov automaton whose transitions are
//! word-parallel. The semantics are the general matcher's, derived rule by
//! rule from [`Pattern::matches_from`](crate::Pattern::matches_from); the
//! differential tests and the fuzz harness hold the two engines equal.
//!
//! One engine serves every entry point. The component policy is the only rule
//! that changes between [`is_match`](crate::Pattern::is_match),
//! [`is_match_path`](crate::Pattern::is_match_path) and
//! [`is_match_glob_path`](crate::Pattern::is_match_glob_path), and it only
//! ever decides which wildcard positions may consume a separator byte — so
//! each policy is one precomputed block mask, chosen per call, while the byte
//! table and the star structure are shared.

use std::sync::Arc;

use crate::{IrBudget, PatternError, PatternOptions, TOO_MUCH_COMPILED_IR, Token, is_separator};

/// Most byte-consuming positions the single-register sweep may hold.
///
/// The state word also carries one boundary past the last position (the
/// accept boundary), so 63 positions is what a `u64` holds. Longer patterns
/// use the multiword representation below.
const MAX_NARROW_POSITIONS: usize = 63;

/// What one compiled engine charges against the shared IR budget.
///
/// The engine is a fixed-size block — dominated by the 2 KiB byte table it is
/// built with — so it is charged as the size of its widest form in
/// [`Token`]-sized units, the currency the budget already counts, whichever
/// column width it is stored at. The value is pinned by a test: it decides
/// which patterns fit the budget.
const NARROW_IR_UNITS: usize = size_of::<NarrowSweepEngine<u64>>().div_ceil(size_of::<Token>());

/// Persistent and temporary word rows allocated while compiling a wide sweep.
///
/// The byte table owns 256 rows, the engine keeps nine policy/state rows, and
/// compilation uses three more rows for the wildcard and recursive-prefix
/// sets. Charging the peak before allocation keeps the existing compiled-IR
/// budget meaningful for a literal whose bytes expand to many sweep positions.
const WIDE_WORD_ROWS_AT_COMPILE: usize = 256 + 9 + 3;

/// A compiled Shift-And automaton for one alternative's token list.
///
/// Bit `p` of the state bitset is the *boundary* before position `p`: it is set
/// when the tokens up to that position have matched the candidate bytes
/// consumed so far. Bit `position_count` is the boundary after the last
/// position, which accepts once the candidate is exhausted.
///
/// The byte table answers "which positions consume this byte" with the
/// component and leading-dot policies left out; those depend on where in the
/// candidate the byte sits, so they are applied per byte as block masks. Case
/// folding and class membership are resolved into the table at compile time.
///
/// The tables are shared: a clone refers to the same tables, which is how a
/// list-filter copy with the same tokens carries its original's engine.
///
/// A narrow engine stores its byte table at the narrowest column width that
/// holds its positions, so a short pattern keeps a 256-byte table rather than
/// a 2 KiB one. Each width is its own variant, dispatched once per candidate
/// rather than once per byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SweepEngine {
    Narrow8(Arc<NarrowSweepEngine<u8>>),
    Narrow16(Arc<NarrowSweepEngine<u16>>),
    Narrow32(Arc<NarrowSweepEngine<u32>>),
    Narrow64(Arc<NarrowSweepEngine<u64>>),
    Wide(Arc<WideSweepEngine>),
}

/// Runs `$narrow` with `$engine` bound to whichever narrow engine `$sweep`
/// is, at its own column width, or `$wide` for the multiword engine.
macro_rules! dispatch {
    ($sweep:expr, $engine:ident => $narrow:expr, $wide:ident => $wide_body:expr) => {
        match $sweep {
            SweepEngine::Narrow8($engine) => $narrow,
            SweepEngine::Narrow16($engine) => $narrow,
            SweepEngine::Narrow32($engine) => $narrow,
            SweepEngine::Narrow64($engine) => $narrow,
            SweepEngine::Wide($wide) => $wide_body,
        }
    };
}

/// One byte-table entry: the positions a byte reaches, at a width that holds
/// every position of the engine.
pub(crate) trait Column: Copy + Default + Eq + std::fmt::Debug {
    /// Positions this width holds.
    const POSITIONS: usize;
    /// `positions`, every one of which is below [`Self::POSITIONS`].
    fn narrowed(positions: u64) -> Self;
    fn positions(self) -> u64;
}

macro_rules! column {
    ($($width:ty),*) => {$(
        impl Column for $width {
            const POSITIONS: usize = <$width>::BITS as usize;

            #[inline]
            fn narrowed(positions: u64) -> Self {
                debug_assert!(positions >> (Self::POSITIONS - 1) >> 1 == 0);
                positions as Self
            }

            #[inline]
            fn positions(self) -> u64 {
                u64::from(self)
            }
        }
    )*};
}

column!(u8, u16, u32, u64);

/// Mutable state of one sweep. Extglob repetition keeps one per alternative
/// and injects a new start boundary whenever the previous repetition reaches
/// the current candidate offset.
pub(crate) enum SweepState {
    Narrow(u64),
    Wide { state: Vec<u64>, next: Vec<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NarrowSweepEngine<C> {
    /// Positions consuming each byte, before any policy mask.
    table: [C; 256],
    /// Positions that repeat and may be skipped: every star-like token.
    stars: u64,
    /// `stars` without the `RecursivePrefix` positions: the closure after a
    /// byte that is not a separator, where the next byte does not start a
    /// component. A `**/` prefix may only hand over at a component start, so
    /// its skip edge is taken only after a separator or at a component-start
    /// candidate start.
    stars_mid_component: u64,
    /// Positions a separator byte never reaches under `component_wildcards`
    /// without `root_component_wildcards`: wildcards directly after an
    /// explicit separator.
    sep_block_component: u64,
    /// Positions a separator byte never reaches under both component options:
    /// every wildcard except the recursive stars.
    sep_block_glob: u64,
    /// Positions a `.` at a component start never reaches. Empty when hidden
    /// entries match; otherwise every wildcard position, literals exempt.
    dot_block: u64,
    /// Boundaries immediately after an ordinary star. These are removed
    /// before a leading period is consumed so the star cannot stop zero-width
    /// and hand that period to a literal. `RecursivePrefix` is exempt.
    dot_stop_block: u64,
    /// Boundaries that accept once the candidate is exhausted: the final
    /// boundary, plus the boundary before a trailing `Separator` +
    /// `RecursiveStar` pair — `src/**` accepts `src` itself.
    accept: u64,
    /// The epsilon closure of the start boundary, for a candidate that starts
    /// a component.
    initial: u64,
    /// The same closure for a candidate that starts inside a component, where
    /// a leading `RecursivePrefix` cannot hand over yet.
    initial_mid_component: u64,
    /// The option the leading-dot rule was folded in under, pinned so a
    /// mismatched match-time option is caught in debug builds.
    match_hidden: bool,
    /// The option the byte table was folded under, pinned likewise.
    case_insensitive: bool,
    /// Whether position zero is an ordinary wildcard. It is then
    /// component-local under `candidate_root_component_wildcard` too, as an
    /// extglob alternative directly behind a separator has it.
    root_wildcard: bool,
}

/// Whether the first token is an ordinary (non-recursive) wildcard, the one
/// position `candidate_root_component_wildcard` makes component-local.
fn root_wildcard(tokens: &[Token]) -> bool {
    matches!(
        tokens.first(),
        Some(Token::Any | Token::Class(_) | Token::Star)
    )
}

impl SweepEngine {
    /// Compiles the automaton for `tokens`.
    ///
    /// Every token kind has a position encoding. Extglob programs never reach
    /// this — the caller keeps them on their own matcher.
    pub(crate) fn compile(
        tokens: &[Token],
        options: PatternOptions,
        budget: &mut IrBudget,
    ) -> Result<Option<Self>, PatternError> {
        Self::charge(tokens, budget)?;
        Self::build(tokens, options).map(Some)
    }

    /// Charges `budget` exactly what [`Self::compile`] charges, without
    /// building the engine.
    ///
    /// An alternative whose fast path answers every entry point never runs
    /// its engine, so it is not built; charging for it all the same keeps
    /// which patterns fit the budget independent of that choice.
    pub(crate) fn charge(tokens: &[Token], budget: &mut IrBudget) -> Result<(), PatternError> {
        let position_count = position_count(tokens)?;
        if position_count > MAX_NARROW_POSITIONS {
            WideSweepEngine::charge(position_count, budget)
        } else {
            budget.charge(NARROW_IR_UNITS, 0)
        }
    }

    /// Builds the engine [`Self::charge`] has already paid for.
    pub(crate) fn build(tokens: &[Token], options: PatternOptions) -> Result<Self, PatternError> {
        let position_count = position_count(tokens)?;
        if position_count > MAX_NARROW_POSITIONS {
            return WideSweepEngine::build(tokens, options, position_count)
                .map(|engine| Self::Wide(Arc::new(engine)));
        }

        // Built at full width, then stored at the narrowest one that holds
        // every position.
        let mut engine = NarrowSweepEngine {
            table: [0_u64; 256],
            stars: 0,
            stars_mid_component: 0,
            sep_block_component: 0,
            sep_block_glob: 0,
            dot_block: 0,
            dot_stop_block: 0,
            accept: 1_u64 << position_count,
            initial: 0,
            initial_mid_component: 0,
            match_hidden: options.match_hidden,
            case_insensitive: options.case_insensitive,
            root_wildcard: root_wildcard(tokens),
        };

        // Wildcard positions answer to the component and leading-dot
        // policies; the subset that consumes any byte at all is widened into
        // the table wholesale after the loop, while a class contributes
        // exactly its members.
        let mut wildcards = 0_u64;
        let mut consume_any = 0_u64;
        let mut recursive_prefixes = 0_u64;
        let mut position = 0_usize;
        for (token_index, token) in tokens.iter().enumerate() {
            let bit = 1_u64 << position;
            // The component policy asks whether the *token before this one*
            // is an explicit separator; a wildcard elsewhere in the pattern
            // stays free to cross separators under `component_wildcards`
            // alone. Mirrors `Pattern::component_wildcard`.
            let after_separator =
                token_index > 0 && matches!(tokens[token_index - 1], Token::Separator);
            match token {
                Token::Literal(literal) => {
                    for &expected in literal {
                        let bit = 1_u64 << position;
                        engine.table[usize::from(expected)] |= bit;
                        if options.case_insensitive {
                            engine.table[usize::from(expected.to_ascii_lowercase())] |= bit;
                            engine.table[usize::from(expected.to_ascii_uppercase())] |= bit;
                        }
                        position += 1;
                    }
                    // Literal positions are exempt from every policy mask:
                    // an escaped separator or a literal dot matches wherever
                    // it stands, exactly as `advance_literal` has it.
                    continue;
                }
                Token::Separator => {
                    engine.table[usize::from(b'/')] |= bit;
                    if cfg!(windows) {
                        engine.table[usize::from(b'\\')] |= bit;
                    }
                }
                Token::Any => {
                    wildcards |= bit;
                    consume_any |= bit;
                    if after_separator {
                        engine.sep_block_component |= bit;
                    }
                    engine.sep_block_glob |= bit;
                }
                Token::Class(class) => {
                    for byte in 0..=u8::MAX {
                        if class.matches(byte, options.case_insensitive) {
                            engine.table[usize::from(byte)] |= bit;
                        }
                    }
                    wildcards |= bit;
                    if after_separator {
                        engine.sep_block_component |= bit;
                    }
                    engine.sep_block_glob |= bit;
                }
                Token::Star => {
                    engine.stars |= bit;
                    if !options.match_hidden {
                        engine.dot_stop_block |= bit << 1;
                    }
                    wildcards |= bit;
                    consume_any |= bit;
                    if after_separator {
                        engine.sep_block_component |= bit;
                    }
                    engine.sep_block_glob |= bit;
                }
                // Recursive stars cross separators under every policy; only
                // the leading-dot rule still binds them.
                Token::RecursiveStar => {
                    engine.stars |= bit;
                    if !options.match_hidden {
                        engine.dot_stop_block |= bit << 1;
                    }
                    wildcards |= bit;
                    consume_any |= bit;
                }
                Token::RecursivePrefix => {
                    engine.stars |= bit;
                    recursive_prefixes |= bit;
                    wildcards |= bit;
                    consume_any |= bit;
                }
            }
            position += 1;
        }
        debug_assert_eq!(position, position_count);
        engine.stars_mid_component = engine.stars & !recursive_prefixes;

        for entry in &mut engine.table {
            *entry |= consume_any;
        }
        engine.dot_block = if options.match_hidden { 0 } else { wildcards };

        // `src/**` accepts `src`: a candidate that ends where the separator
        // would be, with only the terminal recursive star behind it, matches.
        // The boundary before that separator position is therefore accepting.
        // Mirrors the end-of-path case in `matches_from`.
        if let [.., Token::Separator, Token::RecursiveStar] = tokens {
            engine.accept |= 1_u64 << (position_count - 2);
        }
        engine.initial = eclose(1, engine.stars);
        engine.initial_mid_component = eclose(1, engine.stars_mid_component);
        Ok(match position_count {
            0..=8 => Self::Narrow8(Arc::new(engine.narrowed())),
            9..=16 => Self::Narrow16(Arc::new(engine.narrowed())),
            17..=32 => Self::Narrow32(Arc::new(engine.narrowed())),
            _ => Self::Narrow64(Arc::new(engine)),
        })
    }

    /// Matches the entire candidate, byte by byte.
    ///
    /// The two component options are the only ones read at match time; the
    /// rest were folded into the tables when the pattern was compiled, and
    /// they never change between entry points of one [`Pattern`](crate::Pattern).
    pub(crate) fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
        dispatch!(self, engine => engine.is_match(path, options), _wide => {
            let mut state = self.empty_state();
            self.inject_start(&mut state, options.candidate_starts_component);
            let mut at_component_start = options.candidate_starts_component;
            for &byte in path {
                if !self.advance(&mut state, byte, at_component_start, options) {
                    return false;
                }
                at_component_start = is_separator(byte);
            }
            self.accepts(&state)
        })
    }

    pub(crate) fn matching_prefix_ends(
        &self,
        path: &[u8],
        options: PatternOptions,
        base: usize,
        retained_wide: &mut Option<SweepState>,
        output: &mut Vec<usize>,
    ) {
        // A narrow sweep is just one register. Wide sweeps need two heap
        // rows, so keep those on the caller's thread-local extglob scratch
        // instead of allocating them for every group encounter. Narrow
        // sweeps deliberately leave that retained wide state intact.
        dispatch!(self, engine => engine.matching_prefix_ends(path, options, base, output), _wide => {
            let state = retained_wide.get_or_insert_with(|| self.empty_state());
            self.reset_state(state);
            self.inject_start(state, options.candidate_starts_component);
            if self.accepts(state) {
                output.push(base);
            }
            let mut at_component_start = options.candidate_starts_component;
            for (offset, &byte) in path.iter().enumerate() {
                if !self.advance(state, byte, at_component_start, options) {
                    break;
                }
                if self.accepts(state) {
                    output.push(base + offset + 1);
                }
                at_component_start = is_separator(byte);
            }
        })
    }

    /// Whether `self` and `other` are one engine, shared rather than built
    /// twice.
    #[cfg(test)]
    pub(crate) fn shares_tables_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Narrow8(one), Self::Narrow8(two)) => Arc::ptr_eq(one, two),
            (Self::Narrow16(one), Self::Narrow16(two)) => Arc::ptr_eq(one, two),
            (Self::Narrow32(one), Self::Narrow32(two)) => Arc::ptr_eq(one, two),
            (Self::Narrow64(one), Self::Narrow64(two)) => Arc::ptr_eq(one, two),
            (Self::Wide(one), Self::Wide(two)) => Arc::ptr_eq(one, two),
            _ => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn retained_state_capacity(state: &SweepState) -> usize {
        match state {
            SweepState::Narrow(_) => 0,
            SweepState::Wide { state, next } => state.capacity() + next.capacity(),
        }
    }

    pub(crate) fn state_exceeds_retained_words(state: &SweepState, limit: usize) -> bool {
        match state {
            SweepState::Narrow(_) => false,
            SweepState::Wide { state, next } => state.capacity() > limit || next.capacity() > limit,
        }
    }

    pub(crate) fn empty_state(&self) -> SweepState {
        dispatch!(self, _engine => SweepState::Narrow(0), engine => SweepState::Wide {
            state: vec![0; engine.stars.len()],
            next: vec![0; engine.stars.len()],
        })
    }

    /// Clears a retained state for another pass through this engine. A state
    /// with a different representation or width belongs to another engine and
    /// is replaced once; steady-state extglob repetitions keep their buffers.
    pub(crate) fn reset_state(&self, state: &mut SweepState) {
        dispatch!(self, _engine => match state {
            SweepState::Narrow(value) => *value = 0,
            SweepState::Wide { .. } => *state = self.empty_state(),
        }, engine => match state {
            SweepState::Wide {
                state: current,
                next,
            } if current.len() == engine.stars.len() && next.len() == engine.stars.len() => {
                current.fill(0);
                next.fill(0);
            }
            SweepState::Wide { .. } | SweepState::Narrow(_) => *state = self.empty_state(),
        })
    }

    /// Adds the start boundary, closed for a candidate offset that does or
    /// does not start a path component.
    pub(crate) fn inject_start(&self, state: &mut SweepState, starts_component: bool) {
        dispatch!(self, engine => {
            let SweepState::Narrow(state) = state else {
                unreachable!("a sweep state belongs to its engine");
            };
            *state |= engine.initial(starts_component);
        }, engine => {
            let SweepState::Wide { state, .. } = state else {
                unreachable!("a sweep state belongs to its engine");
            };
            let initial = if starts_component {
                &engine.initial
            } else {
                &engine.initial_mid_component
            };
            for (state, initial) in state.iter_mut().zip(initial) {
                *state |= *initial;
            }
        })
    }

    pub(crate) fn advance(
        &self,
        state: &mut SweepState,
        byte: u8,
        at_component_start: bool,
        options: PatternOptions,
    ) -> bool {
        dispatch!(self, engine => {
            let SweepState::Narrow(state) = state else {
                unreachable!("a sweep state belongs to its engine");
            };
            *state = engine.advance(*state, byte, at_component_start, options);
            *state != 0
        }, engine => {
            let SweepState::Wide { state, next } = state else {
                unreachable!("a sweep state belongs to its engine");
            };
            engine.advance(state, next, byte, at_component_start, options)
        })
    }

    pub(crate) fn accepts(&self, state: &SweepState) -> bool {
        dispatch!(self, engine => {
            let SweepState::Narrow(state) = state else {
                unreachable!("a sweep state belongs to its engine");
            };
            state & engine.accept != 0
        }, engine => {
            let SweepState::Wide { state, .. } = state else {
                unreachable!("a sweep state belongs to its engine");
            };
            state
                .iter()
                .zip(&engine.accept)
                .any(|(state, accept)| state & accept != 0)
        })
    }
}

impl NarrowSweepEngine<u64> {
    /// The same engine with its byte table stored at width `C`, which holds
    /// every position.
    fn narrowed<C: Column>(&self) -> NarrowSweepEngine<C> {
        NarrowSweepEngine {
            table: self.table.map(C::narrowed),
            stars: self.stars,
            stars_mid_component: self.stars_mid_component,
            sep_block_component: self.sep_block_component,
            sep_block_glob: self.sep_block_glob,
            dot_block: self.dot_block,
            dot_stop_block: self.dot_stop_block,
            accept: self.accept,
            initial: self.initial,
            initial_mid_component: self.initial_mid_component,
            match_hidden: self.match_hidden,
            case_insensitive: self.case_insensitive,
            root_wildcard: self.root_wildcard,
        }
    }
}

impl<C: Column> NarrowSweepEngine<C> {
    fn initial(&self, starts_component: bool) -> u64 {
        if starts_component {
            self.initial
        } else {
            self.initial_mid_component
        }
    }

    /// [`SweepEngine::is_match`] for this width, one register throughout.
    fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
        let mut state = self.initial(options.candidate_starts_component);
        let mut at_component_start = options.candidate_starts_component;
        for &byte in path {
            state = self.advance(state, byte, at_component_start, options);
            if state == 0 {
                return false;
            }
            at_component_start = is_separator(byte);
        }
        state & self.accept != 0
    }

    /// [`SweepEngine::matching_prefix_ends`] for this width.
    fn matching_prefix_ends(
        &self,
        path: &[u8],
        options: PatternOptions,
        base: usize,
        output: &mut Vec<usize>,
    ) {
        let mut state = self.initial(options.candidate_starts_component);
        if state & self.accept != 0 {
            output.push(base);
        }
        let mut at_component_start = options.candidate_starts_component;
        for (offset, &byte) in path.iter().enumerate() {
            state = self.advance(state, byte, at_component_start, options);
            if state == 0 {
                break;
            }
            if state & self.accept != 0 {
                output.push(base + offset + 1);
            }
            at_component_start = is_separator(byte);
        }
    }

    fn advance(
        &self,
        mut state: u64,
        byte: u8,
        at_component_start: bool,
        options: PatternOptions,
    ) -> u64 {
        debug_assert_eq!(
            (options.match_hidden, options.case_insensitive),
            (self.match_hidden, self.case_insensitive),
            "sweep tables were folded under different options"
        );
        let sep_block = if !options.component_wildcards {
            0
        } else if options.root_component_wildcards {
            self.sep_block_glob
        } else {
            // Position zero is bit zero.
            self.sep_block_component
                | u64::from(options.candidate_root_component_wildcard && self.root_wildcard)
        };

        let separator = is_separator(byte);
        let mut mask = self.table[usize::from(byte)].positions();
        if separator {
            mask &= !sep_block;
        } else if byte == b'.' && at_component_start {
            state &= !self.dot_stop_block;
            mask &= !self.dot_block;
        }
        let consuming = state & mask;
        // The byte after a separator starts a component, and only there may a
        // `**/` prefix hand over.
        let closure = if separator {
            self.stars
        } else {
            self.stars_mid_component
        };
        eclose(
            ((consuming & !self.stars) << 1) | (consuming & self.stars),
            closure,
        )
    }
}

/// Byte-consuming positions `tokens` expand to.
fn position_count(tokens: &[Token]) -> Result<usize, PatternError> {
    let mut count = 0_usize;
    for token in tokens {
        count = count
            .checked_add(match token {
                Token::Literal(literal) => literal.len(),
                _ => 1,
            })
            .ok_or_else(|| PatternError::new(0, TOO_MUCH_COMPILED_IR))?;
    }
    Ok(count)
}

/// A multiword Shift-And engine for alternatives too wide for one register.
///
/// Matching keeps one word per 64 pattern positions, independent of candidate
/// length. The byte table is contiguous by byte then word, so each candidate
/// byte touches only its own row and the compact policy/state rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WideSweepEngine {
    table: Vec<u64>,
    stars: Vec<u64>,
    stars_mid_component: Vec<u64>,
    sep_block_component: Vec<u64>,
    sep_block_glob: Vec<u64>,
    dot_block: Vec<u64>,
    dot_stop_block: Vec<u64>,
    accept: Vec<u64>,
    initial: Vec<u64>,
    initial_mid_component: Vec<u64>,
    match_hidden: bool,
    case_insensitive: bool,
    /// See [`NarrowSweepEngine::root_wildcard`].
    root_wildcard: bool,
}

impl WideSweepEngine {
    fn charge(position_count: usize, budget: &mut IrBudget) -> Result<(), PatternError> {
        let word_count = (position_count + 1).div_ceil(u64::BITS as usize);
        let peak_words = word_count
            .checked_mul(WIDE_WORD_ROWS_AT_COMPILE)
            .ok_or_else(|| PatternError::new(0, TOO_MUCH_COMPILED_IR))?;
        let peak_bytes = peak_words
            .checked_mul(size_of::<u64>())
            .ok_or_else(|| PatternError::new(0, TOO_MUCH_COMPILED_IR))?;
        budget.charge(peak_bytes.div_ceil(size_of::<Token>()), 0)
    }

    fn build(
        tokens: &[Token],
        options: PatternOptions,
        position_count: usize,
    ) -> Result<Self, PatternError> {
        let word_count = (position_count + 1).div_ceil(u64::BITS as usize);
        let table_len = 256_usize
            .checked_mul(word_count)
            .ok_or_else(|| PatternError::new(0, TOO_MUCH_COMPILED_IR))?;
        let mut engine = Self {
            table: vec![0; table_len],
            stars: vec![0; word_count],
            stars_mid_component: vec![0; word_count],
            sep_block_component: vec![0; word_count],
            sep_block_glob: vec![0; word_count],
            dot_block: vec![0; word_count],
            dot_stop_block: vec![0; word_count],
            accept: vec![0; word_count],
            initial: vec![0; word_count],
            initial_mid_component: vec![0; word_count],
            match_hidden: options.match_hidden,
            case_insensitive: options.case_insensitive,
            root_wildcard: root_wildcard(tokens),
        };
        set_bit(&mut engine.accept, position_count);

        let mut wildcards = vec![0_u64; word_count];
        let mut consume_any = vec![0_u64; word_count];
        let mut recursive_prefixes = vec![0_u64; word_count];
        let mut position = 0_usize;
        for (token_index, token) in tokens.iter().enumerate() {
            let after_separator =
                token_index > 0 && matches!(tokens[token_index - 1], Token::Separator);
            match token {
                Token::Literal(literal) => {
                    for &expected in literal {
                        engine.set_table(expected, position);
                        if options.case_insensitive {
                            engine.set_table(expected.to_ascii_lowercase(), position);
                            engine.set_table(expected.to_ascii_uppercase(), position);
                        }
                        position += 1;
                    }
                    continue;
                }
                Token::Separator => {
                    engine.set_table(b'/', position);
                    if cfg!(windows) {
                        engine.set_table(b'\\', position);
                    }
                }
                Token::Any => {
                    set_bit(&mut wildcards, position);
                    set_bit(&mut consume_any, position);
                    if after_separator {
                        set_bit(&mut engine.sep_block_component, position);
                    }
                    set_bit(&mut engine.sep_block_glob, position);
                }
                Token::Class(class) => {
                    for byte in 0..=u8::MAX {
                        if class.matches(byte, options.case_insensitive) {
                            engine.set_table(byte, position);
                        }
                    }
                    set_bit(&mut wildcards, position);
                    if after_separator {
                        set_bit(&mut engine.sep_block_component, position);
                    }
                    set_bit(&mut engine.sep_block_glob, position);
                }
                Token::Star => {
                    set_bit(&mut engine.stars, position);
                    if !options.match_hidden {
                        set_bit(&mut engine.dot_stop_block, position + 1);
                    }
                    set_bit(&mut wildcards, position);
                    set_bit(&mut consume_any, position);
                    if after_separator {
                        set_bit(&mut engine.sep_block_component, position);
                    }
                    set_bit(&mut engine.sep_block_glob, position);
                }
                Token::RecursiveStar => {
                    set_bit(&mut engine.stars, position);
                    if !options.match_hidden {
                        set_bit(&mut engine.dot_stop_block, position + 1);
                    }
                    set_bit(&mut wildcards, position);
                    set_bit(&mut consume_any, position);
                }
                Token::RecursivePrefix => {
                    set_bit(&mut engine.stars, position);
                    set_bit(&mut recursive_prefixes, position);
                    set_bit(&mut wildcards, position);
                    set_bit(&mut consume_any, position);
                }
            }
            position += 1;
        }
        debug_assert_eq!(position, position_count);
        for ((mid, stars), prefixes) in engine
            .stars_mid_component
            .iter_mut()
            .zip(&engine.stars)
            .zip(&recursive_prefixes)
        {
            *mid = stars & !prefixes;
        }

        for row in engine.table.chunks_exact_mut(word_count) {
            for (word, any) in row.iter_mut().zip(&consume_any) {
                *word |= *any;
            }
        }
        if !options.match_hidden {
            engine.dot_block.copy_from_slice(&wildcards);
        }
        if let [.., Token::Separator, Token::RecursiveStar] = tokens {
            set_bit(&mut engine.accept, position_count - 2);
        }
        set_bit(&mut engine.initial, 0);
        engine
            .initial_mid_component
            .copy_from_slice(&engine.initial);
        eclose_wide(&mut engine.initial, &engine.stars);
        eclose_wide(
            &mut engine.initial_mid_component,
            &engine.stars_mid_component,
        );
        Ok(engine)
    }

    fn set_table(&mut self, byte: u8, position: usize) {
        let word_count = self.stars.len();
        let word = position / u64::BITS as usize;
        let bit = position % u64::BITS as usize;
        self.table[usize::from(byte) * word_count + word] |= 1_u64 << bit;
    }

    fn advance(
        &self,
        state: &mut Vec<u64>,
        next: &mut Vec<u64>,
        byte: u8,
        at_component_start: bool,
        options: PatternOptions,
    ) -> bool {
        debug_assert_eq!(
            (options.match_hidden, options.case_insensitive),
            (self.match_hidden, self.case_insensitive),
            "sweep tables were folded under different options"
        );
        let sep_block = if !options.component_wildcards {
            None
        } else if options.root_component_wildcards {
            Some(self.sep_block_glob.as_slice())
        } else {
            Some(self.sep_block_component.as_slice())
        };

        let word_count = self.stars.len();
        let separator = is_separator(byte);
        let row_start = usize::from(byte) * word_count;
        let row = &self.table[row_start..row_start + word_count];
        if byte == b'.' && at_component_start {
            for (state, blocked) in state.iter_mut().zip(&self.dot_stop_block) {
                *state &= !blocked;
            }
        }
        let blocked = if separator {
            sep_block
        } else if byte == b'.' && at_component_start {
            Some(self.dot_block.as_slice())
        } else {
            None
        };

        // Position zero is bit zero of word zero.
        let root_block = u64::from(
            separator
                && options.component_wildcards
                && !options.root_component_wildcards
                && options.candidate_root_component_wildcard
                && self.root_wildcard,
        );

        let mut carry = 0_u64;
        let mut any = false;
        for index in 0..word_count {
            let mut mask = row[index] & !blocked.map_or(0, |bits| bits[index]);
            if index == 0 {
                mask &= !root_block;
            }
            let consuming = state[index] & mask;
            let advancing = consuming & !self.stars[index];
            let shifted = (advancing << 1) | carry;
            carry = advancing >> (u64::BITS - 1);
            next[index] = shifted | (consuming & self.stars[index]);
            any |= next[index] != 0;
        }
        if !any {
            state.fill(0);
            return false;
        }
        eclose_wide(
            next,
            if separator {
                &self.stars
            } else {
                &self.stars_mid_component
            },
        );
        std::mem::swap(state, next);
        next.fill(0);
        true
    }
}

fn set_bit(bits: &mut [u64], position: usize) {
    bits[position / u64::BITS as usize] |= 1_u64 << (position % u64::BITS as usize);
}

/// Multiword form of [`eclose`], using the same addition identity over the
/// complete little-endian bitset and carrying between machine words.
fn eclose_wide(state: &mut [u64], stars: &[u64]) {
    let mut carry = false;
    for (state, &stars) in state.iter_mut().zip(stars) {
        let (sum, first_carry) = stars.overflowing_add(*state & stars);
        let (sum, second_carry) = sum.overflowing_add(u64::from(carry));
        *state |= sum ^ stars;
        carry = first_carry || second_carry;
    }
}

/// Epsilon closure: propagates each boundary upward through runs of stars.
///
/// A star position may match zero bytes, so a boundary before it is also a
/// boundary after it. Within one maximal run of star bits the closure of a
/// set bit is everything from that bit to just past the run, and the carry of
/// a single addition walks exactly that span: adding the run's own mask to
/// the set bits ripples from the lowest set bit of each run to the first zero
/// above it, and the XOR recovers every bit the ripple flipped. Bits the
/// ripple stepped over without flipping are set in `state` already, which the
/// union restores. Runs without a set bit add their own mask back unchanged
/// and cancel in the XOR.
///
/// The addition cannot overflow: star bits sit below [`MAX_POSITIONS`], so
/// the highest carry lands on bit 63.
const fn eclose(state: u64, stars: u64) -> u64 {
    state | ((stars + (state & stars)) ^ stars)
}

#[cfg(test)]
mod tests {
    use super::{MAX_NARROW_POSITIONS, NARROW_IR_UNITS, SweepEngine, eclose, position_count};
    use crate::{IrBudget, PatternOptions, Token};

    /// The closure spelled as the loop the bit trick replaces.
    fn eclose_reference(state: u64, stars: u64) -> u64 {
        let mut closed = state;
        loop {
            let grown = closed | (closed & stars) << 1;
            if grown == closed {
                return closed;
            }
            closed = grown;
        }
    }

    #[test]
    fn eclose_matches_the_reference_loop_over_generated_masks() {
        // A multiplicative generator covers runs, gaps, and multiple set bits
        // per run without depending on a random-number crate.
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = || {
            seed = seed.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(1);
            seed
        };
        for _ in 0..10_000 {
            // Narrow star bits stay below the single-word cap.
            let stars = next() & ((1 << MAX_NARROW_POSITIONS) - 1);
            let state = next();
            assert_eq!(
                eclose(state, stars),
                eclose_reference(state, stars),
                "closure diverges for state {state:#x} over stars {stars:#x}"
            );
        }
    }

    #[test]
    fn eclose_handles_runs_touching_the_position_cap() {
        let stars = ((1_u64 << MAX_NARROW_POSITIONS) - 1) & !1;
        assert_eq!(
            eclose(1 << 1, stars) & (1 << MAX_NARROW_POSITIONS),
            1 << MAX_NARROW_POSITIONS,
            "a run ending at the cap must close into the accept boundary"
        );
    }

    #[test]
    fn narrow_engines_keep_their_budget_charge() {
        // The charge decides which patterns compile, so storing the table at
        // a narrower width must not move it: 67 units is the full-width
        // engine.
        assert_eq!(NARROW_IR_UNITS, 67);
        let tokens = [Token::Star, Token::Literal(b".rs".to_vec())];
        let mut charged = IrBudget::new();
        SweepEngine::charge(&tokens, &mut charged).expect("fits the budget");
        let mut compiled = IrBudget::new();
        SweepEngine::compile(&tokens, PatternOptions::default(), &mut compiled)
            .expect("fits the budget");
        assert_eq!(charged.remaining, compiled.remaining);
        assert_eq!(IrBudget::new().remaining - charged.remaining, 67);
    }

    #[test]
    fn narrow_engines_take_the_narrowest_width_that_holds_their_positions() {
        let options = PatternOptions::default();
        let literal = |len: usize| [Token::Literal(vec![b'a'; len]), Token::Star];
        let width = |tokens: &[Token]| match SweepEngine::build(tokens, options).unwrap() {
            SweepEngine::Narrow8(_) => 8,
            SweepEngine::Narrow16(_) => 16,
            SweepEngine::Narrow32(_) => 32,
            SweepEngine::Narrow64(_) => 64,
            SweepEngine::Wide(_) => 0,
        };
        // A literal of `len` bytes and a star take `len + 1` positions.
        assert_eq!(width(&literal(7)), 8);
        assert_eq!(width(&literal(8)), 16);
        assert_eq!(width(&literal(15)), 16);
        assert_eq!(width(&literal(16)), 32);
        assert_eq!(width(&literal(31)), 32);
        assert_eq!(width(&literal(32)), 64);
        assert_eq!(width(&literal(62)), 64);
        assert_eq!(width(&literal(63)), 0);
        // Every width answers alike at its boundary positions.
        for len in [7, 8, 15, 16, 31, 32, 62] {
            let engine = SweepEngine::build(&literal(len), options).unwrap();
            let exact = vec![b'a'; len];
            let longer = [exact.as_slice(), b"xyz"].concat();
            let short = vec![b'a'; len - 1];
            assert!(engine.is_match(&exact, options), "{len}");
            assert!(engine.is_match(&longer, options), "{len}");
            assert!(!engine.is_match(&short, options), "{len}");
        }
    }

    #[test]
    fn position_counting_respects_the_cap() {
        let short = vec![Token::Literal(vec![b'a'; 60]), Token::Star, Token::Any];
        assert_eq!(position_count(&short), Ok(62));
        let exact = vec![Token::Literal(vec![b'a'; 63])];
        assert_eq!(position_count(&exact), Ok(63));
        let long = vec![Token::Literal(vec![b'a'; 63]), Token::Any];
        assert_eq!(position_count(&long), Ok(64));

        let mut budget = IrBudget::new();
        assert!(
            SweepEngine::compile(&long, PatternOptions::default(), &mut budget)
                .expect("the wide sweep is valid")
                .is_some(),
            "an oversized pattern must compile to the wide sweep"
        );
    }

    #[test]
    fn wide_eclose_carries_across_word_boundaries() {
        let mut state = vec![0, 0];
        let mut stars = vec![0, 0];
        super::set_bit(&mut state, 62);
        for position in 62..=66 {
            super::set_bit(&mut stars, position);
        }
        super::eclose_wide(&mut state, &stars);
        assert_ne!(state[1] & (1 << 3), 0, "closure must reach boundary 67");
    }
}
