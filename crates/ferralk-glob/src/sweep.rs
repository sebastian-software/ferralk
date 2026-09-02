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

use crate::{IrBudget, PatternError, PatternOptions, TOO_MUCH_COMPILED_IR, Token, is_separator};

/// Most byte-consuming positions the single-register sweep may hold.
///
/// The state word also carries one boundary past the last position (the
/// accept boundary), so 63 positions is what a `u64` holds. Longer patterns
/// use the multiword representation below.
const MAX_NARROW_POSITIONS: usize = 63;

/// What one compiled engine charges against the shared IR budget.
///
/// The engine is a fixed-size block — dominated by the 2 KiB byte table — so
/// it is charged as its size in [`Token`]-sized units, the currency the
/// budget already counts.
const NARROW_IR_UNITS: usize = size_of::<NarrowSweepEngine>().div_ceil(size_of::<Token>());

/// Persistent and temporary word rows allocated while compiling a wide sweep.
///
/// The byte table owns 256 rows, the engine keeps seven policy/state rows, and
/// compilation uses two more rows for the wildcard sets. Charging the peak
/// before allocation keeps the existing compiled-IR budget meaningful for a
/// literal whose bytes expand to many sweep positions.
const WIDE_WORD_ROWS_AT_COMPILE: usize = 256 + 7 + 2;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SweepEngine {
    Narrow(Box<NarrowSweepEngine>),
    Wide(Box<WideSweepEngine>),
}

/// Mutable state of one sweep. Extglob repetition keeps one per alternative
/// and injects a new start boundary whenever the previous repetition reaches
/// the current candidate offset.
pub(crate) enum SweepState {
    Narrow(u64),
    Wide { state: Vec<u64>, next: Vec<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NarrowSweepEngine {
    /// Positions consuming each byte, before any policy mask.
    table: [u64; 256],
    /// Positions that repeat and may be skipped: every star-like token.
    stars: u64,
    /// Positions a separator byte never reaches under `component_wildcards`
    /// without `root_component_wildcards`: wildcards directly after an
    /// explicit separator, and `PathStar` wherever it stands.
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
    /// The epsilon closure of the start boundary.
    initial: u64,
    /// The option the leading-dot rule was folded in under, pinned so a
    /// mismatched match-time option is caught in debug builds.
    match_hidden: bool,
    /// The option the byte table was folded under, pinned likewise.
    case_insensitive: bool,
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
    ) -> Result<Option<Box<Self>>, PatternError> {
        let position_count = position_count(tokens)?;
        if position_count > MAX_NARROW_POSITIONS {
            return WideSweepEngine::compile(tokens, options, position_count, budget)
                .map(|engine| Some(Box::new(Self::Wide(Box::new(engine)))));
        }
        budget.charge(NARROW_IR_UNITS, 0)?;

        let mut engine = NarrowSweepEngine {
            table: [0_u64; 256],
            stars: 0,
            sep_block_component: 0,
            sep_block_glob: 0,
            dot_block: 0,
            dot_stop_block: 0,
            accept: 1_u64 << position_count,
            initial: 0,
            match_hidden: options.match_hidden,
            case_insensitive: options.case_insensitive,
        };

        // Wildcard positions answer to the component and leading-dot
        // policies; the subset that consumes any byte at all is widened into
        // the table wholesale after the loop, while a class contributes
        // exactly its members.
        let mut wildcards = 0_u64;
        let mut consume_any = 0_u64;
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
                // A non-recursive `**` in a path filter is component-local
                // wherever it stands, not only after a separator; see
                // `Token::PathStar` in `matches_from`.
                Token::PathStar => {
                    engine.stars |= bit;
                    if !options.match_hidden {
                        engine.dot_stop_block |= bit << 1;
                    }
                    wildcards |= bit;
                    consume_any |= bit;
                    engine.sep_block_component |= bit;
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
                    wildcards |= bit;
                    consume_any |= bit;
                }
            }
            position += 1;
        }
        debug_assert_eq!(position, position_count);

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
        Ok(Some(Box::new(Self::Narrow(Box::new(engine)))))
    }

    /// Matches the entire candidate, byte by byte.
    ///
    /// The two component options are the only ones read at match time; the
    /// rest were folded into the tables when the pattern was compiled, and
    /// they never change between entry points of one [`Pattern`](crate::Pattern).
    pub(crate) fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
        let mut state = self.empty_state();
        self.inject_start(&mut state);
        let mut at_component_start = options.candidate_starts_component;
        for &byte in path {
            if !self.advance(&mut state, byte, at_component_start, options) {
                return false;
            }
            at_component_start = is_separator(byte);
        }
        self.accepts(&state)
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
        let mut narrow = SweepState::Narrow(0);
        let state = match self {
            Self::Narrow(_) => &mut narrow,
            Self::Wide(_) => {
                let state = retained_wide.get_or_insert_with(|| self.empty_state());
                self.reset_state(state);
                state
            }
        };
        self.inject_start(state);
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
        match self {
            Self::Narrow(_) => SweepState::Narrow(0),
            Self::Wide(engine) => SweepState::Wide {
                state: vec![0; engine.stars.len()],
                next: vec![0; engine.stars.len()],
            },
        }
    }

    /// Clears a retained state for another pass through this engine. A state
    /// with a different representation or width belongs to another engine and
    /// is replaced once; steady-state extglob repetitions keep their buffers.
    pub(crate) fn reset_state(&self, state: &mut SweepState) {
        match self {
            Self::Narrow(_) => match state {
                SweepState::Narrow(value) => *value = 0,
                SweepState::Wide { .. } => *state = self.empty_state(),
            },
            Self::Wide(engine) => match state {
                SweepState::Wide {
                    state: current,
                    next,
                } if current.len() == engine.stars.len() && next.len() == engine.stars.len() => {
                    current.fill(0);
                    next.fill(0);
                }
                SweepState::Wide { .. } | SweepState::Narrow(_) => *state = self.empty_state(),
            },
        }
    }

    pub(crate) fn inject_start(&self, state: &mut SweepState) {
        match (self, state) {
            (Self::Narrow(engine), SweepState::Narrow(state)) => *state |= engine.initial,
            (Self::Wide(engine), SweepState::Wide { state, .. }) => {
                for (state, initial) in state.iter_mut().zip(&engine.initial) {
                    *state |= *initial;
                }
            }
            _ => unreachable!("a sweep state belongs to its engine"),
        }
    }

    pub(crate) fn advance(
        &self,
        state: &mut SweepState,
        byte: u8,
        at_component_start: bool,
        options: PatternOptions,
    ) -> bool {
        match (self, state) {
            (Self::Narrow(engine), SweepState::Narrow(state)) => {
                *state = engine.advance(*state, byte, at_component_start, options);
                *state != 0
            }
            (Self::Wide(engine), SweepState::Wide { state, next }) => {
                engine.advance(state, next, byte, at_component_start, options)
            }
            _ => unreachable!("a sweep state belongs to its engine"),
        }
    }

    pub(crate) fn accepts(&self, state: &SweepState) -> bool {
        match (self, state) {
            (Self::Narrow(engine), SweepState::Narrow(state)) => state & engine.accept != 0,
            (Self::Wide(engine), SweepState::Wide { state, .. }) => state
                .iter()
                .zip(&engine.accept)
                .any(|(state, accept)| state & accept != 0),
            _ => unreachable!("a sweep state belongs to its engine"),
        }
    }
}

impl NarrowSweepEngine {
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
            self.sep_block_component
        };

        let separator = is_separator(byte);
        let mut mask = self.table[usize::from(byte)];
        if separator {
            mask &= !sep_block;
        } else if byte == b'.' && at_component_start {
            state &= !self.dot_stop_block;
            mask &= !self.dot_block;
        }
        let consuming = state & mask;
        eclose(
            ((consuming & !self.stars) << 1) | (consuming & self.stars),
            self.stars,
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
    sep_block_component: Vec<u64>,
    sep_block_glob: Vec<u64>,
    dot_block: Vec<u64>,
    dot_stop_block: Vec<u64>,
    accept: Vec<u64>,
    initial: Vec<u64>,
    match_hidden: bool,
    case_insensitive: bool,
}

impl WideSweepEngine {
    fn compile(
        tokens: &[Token],
        options: PatternOptions,
        position_count: usize,
        budget: &mut IrBudget,
    ) -> Result<Self, PatternError> {
        let word_count = (position_count + 1).div_ceil(u64::BITS as usize);
        let peak_words = word_count
            .checked_mul(WIDE_WORD_ROWS_AT_COMPILE)
            .ok_or_else(|| PatternError::new(0, TOO_MUCH_COMPILED_IR))?;
        let peak_bytes = peak_words
            .checked_mul(size_of::<u64>())
            .ok_or_else(|| PatternError::new(0, TOO_MUCH_COMPILED_IR))?;
        budget.charge(peak_bytes.div_ceil(size_of::<Token>()), 0)?;

        let table_len = 256_usize
            .checked_mul(word_count)
            .ok_or_else(|| PatternError::new(0, TOO_MUCH_COMPILED_IR))?;
        let mut engine = Self {
            table: vec![0; table_len],
            stars: vec![0; word_count],
            sep_block_component: vec![0; word_count],
            sep_block_glob: vec![0; word_count],
            dot_block: vec![0; word_count],
            dot_stop_block: vec![0; word_count],
            accept: vec![0; word_count],
            initial: vec![0; word_count],
            match_hidden: options.match_hidden,
            case_insensitive: options.case_insensitive,
        };
        set_bit(&mut engine.accept, position_count);

        let mut wildcards = vec![0_u64; word_count];
        let mut consume_any = vec![0_u64; word_count];
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
                Token::PathStar => {
                    set_bit(&mut engine.stars, position);
                    if !options.match_hidden {
                        set_bit(&mut engine.dot_stop_block, position + 1);
                    }
                    set_bit(&mut wildcards, position);
                    set_bit(&mut consume_any, position);
                    set_bit(&mut engine.sep_block_component, position);
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
                    set_bit(&mut wildcards, position);
                    set_bit(&mut consume_any, position);
                }
            }
            position += 1;
        }
        debug_assert_eq!(position, position_count);

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
        eclose_wide(&mut engine.initial, &engine.stars);
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

        let mut carry = 0_u64;
        let mut any = false;
        for index in 0..word_count {
            let mask = row[index] & !blocked.map_or(0, |bits| bits[index]);
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
        eclose_wide(next, &self.stars);
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
    use super::{MAX_NARROW_POSITIONS, SweepEngine, eclose, position_count};
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
