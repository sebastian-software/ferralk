//! A shared guard against unbounded brace expansion.
//!
//! `Pattern::compile` expands braces eagerly and has no budget, so a pattern
//! costs the product of its groups' alternative counts: ten nine-way groups
//! are 100 bytes and 3.5 billion alternatives. libFuzzer then reports an
//! out-of-memory that says nothing about parsing or matching and hides every
//! other finding behind it.
//!
//! Until the matcher grows an expansion budget, the targets skip inputs above
//! this cap. The cap is deliberately generous: it still admits thousands of
//! alternatives, which is far more than any real pattern uses.

/// The largest expansion a target will hand to the matcher.
pub const MAX_ALTERNATIVES: u64 = 1 << 12;

/// Whether brace expansion of `pattern` stays inside [`MAX_ALTERNATIVES`].
///
/// The count is an upper bound: every closing brace multiplies the total by
/// the number of alternatives its group holds. An unmatched brace never
/// multiplies, matching the matcher's reading of it as ordinary text.
pub fn within_budget(pattern: &[u8]) -> bool {
    let mut total = 1_u64;
    let mut commas_by_depth: Vec<u64> = Vec::new();
    let mut index = 0;
    while index < pattern.len() {
        match pattern[index] {
            b'\\' => index += 1,
            b'{' => commas_by_depth.push(0),
            b',' => {
                if let Some(commas) = commas_by_depth.last_mut() {
                    *commas += 1;
                }
            }
            b'}' => {
                if let Some(commas) = commas_by_depth.pop() {
                    total = total.saturating_mul(commas + 1);
                    if total > MAX_ALTERNATIVES {
                        return false;
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
    true
}
