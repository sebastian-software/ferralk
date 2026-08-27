//! Bit-parallel Shift-And engine for the general match path.
//!
//! The general matcher explores the token/path state graph one state at a
//! time, bounded by the [`FailedStates`](crate::FailedStates) memo to
//! `tokens x path` visits. This module walks the same graph column by column
//! instead: every token expands to byte-consuming *positions*, the set of
//! reachable positions lives in one machine word, and each candidate byte
//! advances the whole set with a handful of bitwise operations. The cost is
//! `path` steps flat — the adversarial star chain that keeps the memoized
//! matcher busy for `tokens x path` visits costs the same as any other
//! candidate here.
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

use crate::{IrBudget, PatternError, PatternOptions, Token, is_separator};

/// Most byte-consuming positions a pattern may expand to and still sweep.
///
/// The state word also carries one boundary past the last position (the
/// accept boundary), so 63 positions is what a `u64` holds. A pattern past
/// the cap falls back to the memoized matcher; nothing about it is invalid.
const MAX_POSITIONS: usize = 63;

/// What one compiled engine charges against the shared IR budget.
///
/// The engine is a fixed-size block — dominated by the 2 KiB byte table — so
/// it is charged as its size in [`Token`]-sized units, the currency the
/// budget already counts.
const IR_UNITS: usize = size_of::<SweepEngine>().div_ceil(size_of::<Token>());

/// A compiled Shift-And automaton for one alternative's token list.
///
/// Bit `p` of a state word is the *boundary* before position `p`: it is set
/// when the tokens up to that position have matched the candidate bytes
/// consumed so far. Bit `position_count` is the boundary after the last
/// position, which accepts once the candidate is exhausted.
///
/// The byte table answers "which positions consume this byte" with the
/// component and leading-dot policies left out; those depend on where in the
/// candidate the byte sits, so they are applied per byte as block masks. Case
/// folding and class membership are resolved into the table at compile time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SweepEngine {
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
    /// Compiles the automaton for `tokens`, or `None` where they do not fit.
    ///
    /// Only the position cap makes a token list unsuitable: every token kind
    /// has a position encoding. Extglob programs never reach this — the
    /// caller keeps them on their own interpreter.
    pub(crate) fn compile(
        tokens: &[Token],
        options: PatternOptions,
        budget: &mut IrBudget,
    ) -> Result<Option<Box<Self>>, PatternError> {
        let Some(position_count) = position_count(tokens) else {
            return Ok(None);
        };
        budget.charge(IR_UNITS, 0)?;

        let mut engine = Box::new(Self {
            table: [0_u64; 256],
            stars: 0,
            sep_block_component: 0,
            sep_block_glob: 0,
            dot_block: 0,
            accept: 1_u64 << position_count,
            initial: 0,
            match_hidden: options.match_hidden,
            case_insensitive: options.case_insensitive,
        });

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
                    wildcards |= bit;
                    consume_any |= bit;
                    engine.sep_block_component |= bit;
                    engine.sep_block_glob |= bit;
                }
                // Recursive stars cross separators under every policy; only
                // the leading-dot rule still binds them.
                Token::RecursiveStar | Token::RecursivePrefix => {
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
        Ok(Some(engine))
    }

    /// Matches the entire candidate, byte by byte.
    ///
    /// The two component options are the only ones read at match time; the
    /// rest were folded into the tables when the pattern was compiled, and
    /// they never change between entry points of one [`Pattern`](crate::Pattern).
    pub(crate) fn is_match(&self, path: &[u8], options: PatternOptions) -> bool {
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

        let mut state = self.initial;
        let mut at_component_start = options.candidate_starts_component;
        for &byte in path {
            let separator = is_separator(byte);
            let mut mask = self.table[usize::from(byte)];
            if separator {
                mask &= !sep_block;
            } else if byte == b'.' && at_component_start {
                mask &= !self.dot_block;
            }
            // Boundaries whose position consumes this byte: a star stays put
            // (it may consume more), everything else moves past its position.
            let consuming = state & mask;
            state = ((consuming & !self.stars) << 1) | (consuming & self.stars);
            state = eclose(state, self.stars);
            if state == 0 {
                return false;
            }
            at_component_start = separator;
        }
        state & self.accept != 0
    }
}

/// Byte-consuming positions `tokens` expand to, or `None` past the cap.
fn position_count(tokens: &[Token]) -> Option<usize> {
    let mut count = 0_usize;
    for token in tokens {
        count += match token {
            Token::Literal(literal) => literal.len(),
            _ => 1,
        };
        if count > MAX_POSITIONS {
            return None;
        }
    }
    Some(count)
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
    use super::{MAX_POSITIONS, SweepEngine, eclose, position_count};
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
            // Star bits stay below the position cap, as compilation ensures.
            let stars = next() & ((1 << MAX_POSITIONS) - 1);
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
        let stars = ((1_u64 << MAX_POSITIONS) - 1) & !1;
        assert_eq!(
            eclose(1 << 1, stars) & (1 << MAX_POSITIONS),
            1 << MAX_POSITIONS,
            "a run ending at the cap must close into the accept boundary"
        );
    }

    #[test]
    fn position_counting_respects_the_cap() {
        let short = vec![Token::Literal(vec![b'a'; 60]), Token::Star, Token::Any];
        assert_eq!(position_count(&short), Some(62));
        let exact = vec![Token::Literal(vec![b'a'; 63])];
        assert_eq!(position_count(&exact), Some(63));
        let long = vec![Token::Literal(vec![b'a'; 63]), Token::Any];
        assert_eq!(position_count(&long), None);

        let mut budget = IrBudget::new();
        assert!(
            SweepEngine::compile(&long, PatternOptions::default(), &mut budget)
                .expect("the cap is not an error")
                .is_none(),
            "an oversized pattern must fall back to the memoized matcher"
        );
    }
}
