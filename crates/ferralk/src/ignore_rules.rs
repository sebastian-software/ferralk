//! Gitignore rules, parsed here and matched with ferralk-glob.
//!
//! ADR-0014: Git stays normative, the engine is ours. A rule line is read the
//! way `gitignore(5)` describes it - comment, negation, trailing spaces,
//! directory-only slash, anchoring - and its body becomes a matcher. The
//! evaluation around this, the per-directory chains and what a subtree carries
//! down, lives in [`super::gitignore`] and is untouched.
//!
//! A rule compiles per path component rather than into one pattern, because the
//! two dialects still read `**` differently at the edges. Both make only a
//! whole-component `**` recursive (ADR-0020), but Git also reads a run of three
//! or more stars as one, lets `**` before an escaped slash span one or more
//! components, and requires something below a trailing `/**`. The component
//! walk is ours; each component is a ferralk-glob pattern, which is where
//! stars, classes and escapes are actually matched.
//!
//! What that leaves for the translation:
//!
//! - A rule without a separator matches at any level, which is a leading `**`
//!   component; one with a separator is anchored to its own directory.
//! - A trailing `/**` covers what is inside a directory, so something has to
//!   follow it.
//! - Asterisks inside a component are ordinary stars in Git, so `a**b`
//!   collapses to `a*b`; only a whole `**` component spans directories. Before
//!   an escaped slash that run must consume at least one component.

use std::{borrow::Cow, num::NonZeroU32, path::Path};

use ferralk_glob::{Pattern, PatternOptions};
use memchr::memmem;

use super::glob_path_bytes;

/// A single ignore file may contribute at most this many parsed path parts or
/// compiled components. The byte and line limits bound I/O and rule count,
/// while this dimension prevents one long line of tiny components (such as
/// `"a/"` repeated millions of times) from amplifying an 8 MiB file into
/// gigabytes of compiled matchers.
///
/// 250,000 components fits 100,000 unanchored single-component rules (whether
/// literal or wildcard, each costing `AnyDirs` plus one component = 200,000
/// components total), while long wildcard and multi-star memory bombs are
/// bounded separately by `MAX_IGNORE_FILE_RETAINED_BYTES`.
pub(crate) const MAX_IGNORE_FILE_COMPONENTS: usize = 250_000;

/// Conservative estimated retained matcher storage cap for one ignore file.
/// Accommodates up to the full 100,000-line / 8 MiB ceiling of ordinary literal
/// rules on every architecture while rejecting pathological wildcard and
/// multi-star `WideSweepEngine` memory bombs before retaining unbounded heap
/// state.
pub(crate) const MAX_IGNORE_FILE_RETAINED_BYTES: usize = 256 * 1024 * 1024;

/// Calibrated conservative bounds for retained inline and heap storage owned by
/// a compiled ignore `Rule` and its `Component` entries, derived from target
/// struct sizes so `Vec` initial/doubled capacities stay upper-bounded on every
/// architecture (arm64, x86_64, etc.) and verified against `allocation-counter`
/// measurements in `pattern_memory_regression`.
///
/// The initial 4-element `Vec<Rule>` backing allocation is charged once on the
/// first rule (`RULE_FIRST_EXTRA_BYTES`), and `2 * size_of::<Rule>() + 128` per
/// rule thereafter so `sum_{i=1..=N} >= cap(N) * size_of::<Rule>()` for every
/// `N >= 1` without penalizing large legitimate rule files by 2x.
const RULE_FIXED_RETAINED_BYTES: usize = std::mem::size_of::<Rule>() * 2 + 128;
const RULE_FIRST_EXTRA_BYTES: usize = std::mem::size_of::<Rule>() * 2 + 128;
const ANY_DIRS_COMPONENT_BYTES: usize = std::mem::size_of::<Component>() * 2;
const LITERAL_COMPONENT_FIXED_BYTES: usize = std::mem::size_of::<Component>() * 4 + 256;
const PATTERN_COMPONENT_FIXED_BYTES: usize =
    std::mem::size_of::<Component>() * 4 + std::mem::size_of::<memmem::Finder<'static>>() + 768;
const WIDE_SWEEP_EXTRA_BYTES: usize = 2_048;
const MAX_COMPONENT_FIXED_BYTES: usize =
    (PATTERN_COMPONENT_FIXED_BYTES + WIDE_SWEEP_EXTRA_BYTES) * 2;
const LITERAL_BYTES_PER_BYTE: usize = 10;
const PATTERN_BYTES_PER_BYTE: usize = 128;
const MAX_BYTES_PER_BYTE: usize = PATTERN_BYTES_PER_BYTE * 2;
/// Under `component_options` (`braces: false`, `extglob: false`,
/// `recursive_double_star: false`), `ferralk-glob`'s `IrBudget` (1,048,576 IR
/// units) cannot be exhausted by any single component shorter than 64 KiB.
const MIN_IR_LIMIT_COMPONENT_BYTES: usize = 65_536;

/// Returned when a rule line would exceed a component or byte budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComponentBudgetExceeded {
    /// Per-file parsed-part or compiled-matcher-unit limit.
    File,
    /// Per-file estimated retained matcher byte limit.
    FileBytes,
    /// Active ancestor-chain retained compiled-matcher-unit limit.
    Chain,
    /// Active ancestor-chain estimated retained matcher byte limit.
    ChainBytes,
}

/// Remaining budgets passed to one ignore-rule line parse.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LineBudgets {
    pub(crate) file_parsed_remaining: usize,
    pub(crate) file_compiled_remaining: usize,
    pub(crate) file_bytes_remaining: usize,
    pub(crate) chain_compiled_remaining: usize,
    pub(crate) chain_bytes_remaining: usize,
}

impl LineBudgets {
    #[cfg(any(test, feature = "unstable-test-hooks"))]
    pub(crate) const UNBOUNDED: Self = Self {
        file_parsed_remaining: usize::MAX,
        file_compiled_remaining: usize::MAX,
        file_bytes_remaining: usize::MAX,
        chain_compiled_remaining: usize::MAX,
        chain_bytes_remaining: usize::MAX,
    };

    #[cfg(any(test, feature = "unstable-test-hooks"))]
    pub(crate) const fn single_file(remaining_units: usize, remaining_bytes: usize) -> Self {
        Self {
            file_parsed_remaining: remaining_units,
            file_compiled_remaining: remaining_units,
            file_bytes_remaining: remaining_bytes,
            chain_compiled_remaining: remaining_units,
            chain_bytes_remaining: remaining_bytes,
        }
    }
}

/// Budget units charged by one parsed ignore-rule line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineBudgetUsage {
    /// Non-empty path parts scanned/parsed from the line, including lines
    /// discarded as malformed.
    pub(crate) parsed_parts: usize,
    /// Retained compiled matcher units stored in the resulting [`RuleSet`].
    pub(crate) compiled_units: usize,
    /// Conservative estimated retained matcher bytes stored in the resulting
    /// [`RuleSet`].
    pub(crate) retained_bytes: usize,
}

/// Snapshot of [`RuleSetBuilder`] state used to roll back a file atomically
/// when a later line in that file breaches a safety budget.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RuleSetCheckpoint {
    rules_len: usize,
    compiled_units: usize,
    retained_bytes: usize,
}

/// How ferralk-glob reads one rule component.
///
/// Braces stay literal because Git does not expand them in ignore files, and
/// extglobs are not Git syntax either. Hidden files are matchable: an ignore
/// rule is expected to see dotfiles. Recursive stars are off because a `**`
/// component never reaches the compiler - it becomes [`Component::AnyDirs`] -
/// and any other run has already collapsed to a single star.
fn component_options(case_insensitive: bool) -> PatternOptions {
    PatternOptions::default()
        .braces(false)
        .extglob(false)
        .recursive_double_star(false)
        .match_hidden(true)
        // Git's WM_CASEFOLD is ASCII-only. `PatternOptions` has exactly that
        // contract, including in literals and bracket classes.
        .case_insensitive(case_insensitive)
}

/// Reusable per-entry buffers hoisted across every [`RuleSet`] in an ancestor
/// chain so NFC precomposition and ASCII case folding happen at most once per
/// entry with zero heap allocations once warm.
#[derive(Debug, Default)]
pub(crate) struct IgnoreMatchScratch {
    precomposed: Vec<u8>,
    precomposed_prefix: Vec<u8>,
    folded: Vec<u8>,
}

impl IgnoreMatchScratch {
    pub(crate) fn reset_for_reuse(&mut self, max_retained_bytes: usize) {
        self.precomposed.clear();
        if self.precomposed.capacity() > max_retained_bytes {
            self.precomposed.shrink_to(max_retained_bytes);
        }
        self.precomposed_prefix.clear();
        if self.precomposed_prefix.capacity() > max_retained_bytes {
            self.precomposed_prefix.shrink_to(max_retained_bytes);
        }
        self.folded.clear();
        if self.folded.capacity() > max_retained_bytes {
            self.folded.shrink_to(max_retained_bytes);
        }
    }

    #[cfg(test)]
    pub(crate) fn resize_for_test(&mut self, len: usize) {
        self.precomposed.resize(len, b'p');
        self.precomposed_prefix.resize(len, b'x');
        self.folded.resize(len, b'f');
    }

    #[cfg(test)]
    pub(crate) fn precomposed_capacity(&self) -> usize {
        self.precomposed
            .capacity()
            .max(self.precomposed_prefix.capacity())
    }

    #[cfg(test)]
    pub(crate) fn folded_capacity(&self) -> usize {
        self.folded.capacity()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrecomposedState {
    Unchecked,
    SameAsRaw,
    Buffered,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewKind {
    Raw,
    Precomposed,
}

/// Per-entry lazy view state shared across every [`RuleSet`] in one ancestor
/// chain. Carries only stack metadata while borrowing the caller's reusable
/// [`IgnoreMatchScratch`].
pub(crate) struct EntryMatchContext<'a> {
    raw: &'a [u8],
    raw_basename_start: Option<usize>,
    precomposed_basename_start: Option<usize>,
    precomposed_state: PrecomposedState,
    /// Byte offset into `raw` where `precomposed_state` (`SameAsRaw` or
    /// `Buffered`) starts. Bytes before this offset lie above the outermost
    /// applicable `precompose_unicode` rule set and are never normalized.
    precomposed_origin: usize,
    /// Start index of the active precomposed slice inside
    /// `scratch.precomposed` (`&scratch.precomposed[precomposed_head..]`).
    /// Headroom before `precomposed_head` allows prepending ancestor prefixes
    /// in amortized `O(1)` per byte instead of rotating the suffix on every
    /// ancestor.
    precomposed_head: usize,
    /// Invariant: `raw[raw_ascii_from..]` is known to be pure ASCII.
    raw_ascii_from: usize,
    /// Which view `scratch.folded` currently holds, if populated.
    folded_view: Option<ViewKind>,
    /// Byte offset into `view_bytes(folded_view)` where the active folded
    /// slice `&scratch.folded[folded_head..]` begins, so basename-only rules
    /// fold only the basename instead of the entire ancestor path.
    folded_from: usize,
    /// Start index of the active folded slice inside `scratch.folded`
    /// (`&scratch.folded[folded_head..]`). Headroom before `folded_head` allows
    /// prepending outer-ancestor prefixes in amortized `O(1)` per byte instead
    /// of rotating the suffix on every ancestor.
    folded_head: usize,
    /// Invariant: `raw[raw_clean_from..]` contains no ASCII uppercase bytes.
    raw_clean_from: usize,
    /// Invariant: `view_bytes(ViewKind::Precomposed)[precomposed_clean_from..]`
    /// contains no ASCII uppercase bytes.
    precomposed_clean_from: usize,
    scratch: &'a mut IgnoreMatchScratch,
}

impl<'a> EntryMatchContext<'a> {
    #[inline]
    pub(crate) fn new(raw: &'a [u8], scratch: &'a mut IgnoreMatchScratch) -> Self {
        let (precomposed_state, precomposed_origin) = if cfg!(not(target_os = "macos")) {
            (PrecomposedState::SameAsRaw, 0)
        } else {
            (PrecomposedState::Unchecked, raw.len())
        };
        let raw_clean_from = trimmed_trailing_slash_len(raw);
        Self {
            raw,
            raw_basename_start: None,
            precomposed_basename_start: None,
            precomposed_state,
            precomposed_origin,
            precomposed_head: 0,
            raw_ascii_from: raw.len(),
            folded_view: None,
            folded_from: 0,
            folded_head: 0,
            raw_clean_from,
            precomposed_clean_from: 0,
            scratch,
        }
    }

    #[inline]
    fn view_bytes(&self, view: ViewKind) -> &[u8] {
        match view {
            ViewKind::Raw => self.raw,
            ViewKind::Precomposed => &self.scratch.precomposed[self.precomposed_head..],
        }
    }

    fn resolve_precomposed(&mut self, raw_rel_start: usize) -> (ViewKind, usize) {
        if self.precomposed_state == PrecomposedState::SameAsRaw
            && raw_rel_start >= self.precomposed_origin
        {
            return (ViewKind::Raw, raw_rel_start);
        }
        if self.precomposed_state == PrecomposedState::Buffered
            && raw_rel_start >= self.precomposed_origin
        {
            let eff_rel_start = if raw_rel_start == self.precomposed_origin {
                0
            } else {
                map_rel_start_to_precomposed(
                    &self.raw[self.precomposed_origin..],
                    raw_rel_start - self.precomposed_origin,
                    &self.scratch.precomposed[self.precomposed_head..],
                )
            };
            return (ViewKind::Precomposed, eff_rel_start);
        }
        match self.precomposed_state {
            PrecomposedState::Unchecked => {
                // Fast ASCII bypass: pure-ASCII relative suffixes never change
                // under NFC and skip UTF-8 validation and NFC table lookups
                // completely without touching any bytes above `raw_rel_start`.
                if raw_rel_start >= self.raw_ascii_from {
                    return (ViewKind::Raw, raw_rel_start);
                }
                if is_ascii_tracked(&self.raw[raw_rel_start..self.raw_ascii_from]) {
                    self.raw_ascii_from = raw_rel_start;
                    return (ViewKind::Raw, raw_rel_start);
                }
                // Normalize only the current node's required suffix so
                // ancestors and root prefixes are never touched when an inner
                // ignore file already decides the verdict.
                self.precomposed_origin = raw_rel_start;
                if precompose_into(&self.raw[raw_rel_start..], &mut self.scratch.precomposed) {
                    self.precomposed_state = PrecomposedState::Buffered;
                    self.precomposed_head = 0;
                    self.precomposed_clean_from =
                        trimmed_trailing_slash_len(&self.scratch.precomposed);
                    self.precomposed_basename_start = None;
                    if self.folded_view == Some(ViewKind::Precomposed) {
                        self.folded_view = None;
                    }
                    (ViewKind::Precomposed, 0)
                } else {
                    self.precomposed_state = PrecomposedState::SameAsRaw;
                    (ViewKind::Raw, raw_rel_start)
                }
            }
            PrecomposedState::SameAsRaw => {
                // Expand strictly across `&raw[raw_rel_start..old_origin]`:
                // never rescan `&raw[old_origin..]` for ASCII and never jump
                // past this node into unreached grandparents.
                let old_origin = self.precomposed_origin;
                let prefix = &self.raw[raw_rel_start..old_origin];
                if is_ascii_tracked(prefix) {
                    self.precomposed_origin = raw_rel_start;
                    return (ViewKind::Raw, raw_rel_start);
                }
                let changed = if prefix.ends_with(b"/") {
                    if precompose_into(prefix, &mut self.scratch.precomposed) {
                        self.scratch
                            .precomposed
                            .extend_from_slice(&self.raw[old_origin..]);
                        true
                    } else {
                        false
                    }
                } else {
                    precompose_into(&self.raw[raw_rel_start..], &mut self.scratch.precomposed)
                };
                self.precomposed_origin = raw_rel_start;
                if changed {
                    self.precomposed_state = PrecomposedState::Buffered;
                    self.precomposed_head = 0;
                    self.precomposed_clean_from =
                        trimmed_trailing_slash_len(&self.scratch.precomposed);
                    self.precomposed_basename_start = None;
                    if self.folded_view == Some(ViewKind::Precomposed) {
                        self.folded_view = None;
                    }
                    (ViewKind::Precomposed, 0)
                } else {
                    (ViewKind::Raw, raw_rel_start)
                }
            }
            PrecomposedState::Buffered => {
                // Expand strictly across `&raw[raw_rel_start..old_origin]` by
                // prepending the newly exposed segment into headroom before
                // `precomposed_head` — amortized O(L) bytes moved and never
                // touching ancestors above the deciding node.
                let old_origin = self.precomposed_origin;
                let prefix = &self.raw[raw_rel_start..old_origin];
                if is_ascii_tracked(prefix) {
                    let _moved = prepend_with_headroom(
                        &mut self.scratch.precomposed,
                        &mut self.precomposed_head,
                        prefix,
                    );
                    #[cfg(test)]
                    PRECOMPOSE_SUFFIX_MOVE_BYTES.with(|count| count.set(count.get() + _moved));
                    let prefix_nfc_len = prefix.len();
                    if let Some(basename_start) = self.precomposed_basename_start {
                        self.precomposed_basename_start = Some(basename_start + prefix_nfc_len);
                    }
                    self.precomposed_clean_from += prefix_nfc_len;
                    if self.folded_view == Some(ViewKind::Precomposed) {
                        self.folded_from += prefix_nfc_len;
                    }
                } else if prefix.ends_with(b"/") {
                    self.scratch.precomposed_prefix.clear();
                    precompose_append(prefix, &mut self.scratch.precomposed_prefix);
                    let _moved = prepend_with_headroom(
                        &mut self.scratch.precomposed,
                        &mut self.precomposed_head,
                        &self.scratch.precomposed_prefix,
                    );
                    #[cfg(test)]
                    PRECOMPOSE_SUFFIX_MOVE_BYTES.with(|count| count.set(count.get() + _moved));
                    let prefix_nfc_len = self.scratch.precomposed_prefix.len();
                    if let Some(basename_start) = self.precomposed_basename_start {
                        self.precomposed_basename_start = Some(basename_start + prefix_nfc_len);
                    }
                    self.precomposed_clean_from += prefix_nfc_len;
                    if self.folded_view == Some(ViewKind::Precomposed) {
                        self.folded_from += prefix_nfc_len;
                    }
                } else {
                    precompose_into(&self.raw[raw_rel_start..], &mut self.scratch.precomposed);
                    self.precomposed_head = 0;
                    self.precomposed_clean_from =
                        trimmed_trailing_slash_len(&self.scratch.precomposed);
                    self.precomposed_basename_start = None;
                    if self.folded_view == Some(ViewKind::Precomposed) {
                        self.folded_view = None;
                    }
                }
                self.precomposed_origin = raw_rel_start;
                (ViewKind::Precomposed, 0)
            }
        }
    }

    /// Resolves the precomposed view for a basename-only rule set without
    /// expanding ancestor directories when `Precomposed` is already buffered.
    fn resolve_precomposed_basename(&mut self, raw_rel_start: usize) -> (ViewKind, usize) {
        let raw_base_start = self.basename_range(ViewKind::Raw, raw_rel_start).0;
        if self.precomposed_state == PrecomposedState::Buffered
            && raw_base_start >= self.precomposed_origin
            && (raw_base_start == 0 || self.raw[raw_base_start - 1] == b'/')
        {
            let base_start = self.basename_range(ViewKind::Precomposed, 0).0;
            return (ViewKind::Precomposed, base_start);
        }
        self.resolve_precomposed(raw_base_start)
    }

    #[cfg(test)]
    fn folded_len(&self) -> usize {
        self.scratch.folded.len() - self.folded_head
    }

    /// Returns `(start, end)` byte offsets into `view_bytes(view)` for the
    /// final path component of `&view_bytes(view)[rel_start..]`, matching
    /// [`matches_components`] for `[Component::AnyDirs, Component::Pattern(_)]`.
    #[inline]
    fn basename_range(&mut self, view: ViewKind, rel_start: usize) -> (usize, usize) {
        let (bytes, cached_start) = match view {
            ViewKind::Raw => (self.raw, &mut self.raw_basename_start),
            ViewKind::Precomposed => (
                &self.scratch.precomposed[self.precomposed_head..],
                &mut self.precomposed_basename_start,
            ),
        };
        let trimmed_len = trimmed_trailing_slash_len(bytes);
        let path_basename_start = *cached_start.get_or_insert_with(|| {
            memchr::memrchr(b'/', &bytes[..trimmed_len]).map_or(0, |pos| pos + 1)
        });
        let start = path_basename_start.max(rel_start).min(trimmed_len);
        (start, trimmed_len)
    }

    /// Returns the ASCII-lowercased haystack slice for a case-insensitive
    /// rule's `memmem::Finder` gate.
    ///
    /// Borrows `&view_bytes[start..end]` with zero copying whenever that slice
    /// has no ASCII uppercase bytes. Otherwise folds only `&view_bytes[start..]`
    /// on the first uppercase slice, and if an outer rule or ancestor later
    /// needs `start < folded_from`, prepends strictly `&view_bytes[start..old_from]`
    /// into front headroom so neither unreached ancestors nor root prefixes are
    /// ever folded and total moved bytes stay amortized `O(repo_relative_len)`.
    #[inline]
    fn gate_haystack_ci(&mut self, view: ViewKind, start: usize, end: usize) -> &[u8] {
        if self.folded_view == Some(view) {
            if start >= self.folded_from {
                let head = self.folded_head;
                let offset = self.folded_from;
                return &self.scratch.folded[head + (start - offset)..head + (end - offset)];
            }
            if end > self.folded_from {
                let old_from = self.folded_from;
                let prefix_len = old_from - start;
                #[cfg(test)]
                {
                    CASE_FOLD_CALLS.with(|count| count.set(count.get() + 1));
                    CASE_FOLD_BYTES.with(|count| count.set(count.get() + prefix_len));
                }
                let prefix = match view {
                    ViewKind::Raw => &self.raw[start..old_from],
                    ViewKind::Precomposed => {
                        &self.scratch.precomposed
                            [self.precomposed_head + start..self.precomposed_head + old_from]
                    }
                };
                let _moved =
                    prepend_with_headroom(&mut self.scratch.folded, &mut self.folded_head, prefix);
                #[cfg(test)]
                FOLD_SUFFIX_MOVE_BYTES.with(|count| count.set(count.get() + _moved));
                let head = self.folded_head;
                self.scratch.folded[head..head + prefix_len].make_ascii_lowercase();
                self.folded_from = start;
                return &self.scratch.folded[head..head + (end - start)];
            }
        }
        let clean_from = match view {
            ViewKind::Raw => &mut self.raw_clean_from,
            ViewKind::Precomposed => &mut self.precomposed_clean_from,
        };
        if start >= *clean_from {
            return &self.view_bytes(view)[start..end];
        }
        let bytes = match view {
            ViewKind::Raw => self.raw,
            ViewKind::Precomposed => &self.scratch.precomposed[self.precomposed_head..],
        };
        let scan_end = end.min(*clean_from);
        let has_upper = bytes[start..scan_end].iter().any(u8::is_ascii_uppercase);
        if !has_upper {
            if end >= *clean_from {
                *clean_from = start;
            }
            return &bytes[start..end];
        }
        #[cfg(test)]
        {
            CASE_FOLD_CALLS.with(|count| count.set(count.get() + 1));
            CASE_FOLD_BYTES.with(|count| count.set(count.get() + (bytes.len() - start)));
        }
        self.scratch.folded.clear();
        self.folded_head = 0;
        match view {
            ViewKind::Raw => self.scratch.folded.extend_from_slice(&self.raw[start..]),
            ViewKind::Precomposed => self
                .scratch
                .folded
                .extend_from_slice(&self.scratch.precomposed[self.precomposed_head + start..]),
        }
        self.scratch.folded.make_ascii_lowercase();
        self.folded_view = Some(view);
        self.folded_from = start;
        &self.scratch.folded[..end - start]
    }
}

#[inline]
fn is_ascii_tracked(bytes: &[u8]) -> bool {
    #[cfg(test)]
    NFC_ASCII_SCAN_BYTES.with(|count| count.set(count.get() + bytes.len()));
    bytes.is_ascii()
}

/// Prepends `prefix` in front of `&buf[*head..]` using amortized geometric
/// headroom so repeated outer-ancestor prepends move `O(total_len)` suffix
/// bytes across arbitrarily deep chains rather than `O(D * total_len)`.
/// Returns the number of suffix bytes moved.
fn prepend_with_headroom(buf: &mut Vec<u8>, head: &mut usize, prefix: &[u8]) -> usize {
    let p = prefix.len();
    if p == 0 {
        return 0;
    }
    if *head >= p {
        let new_head = *head - p;
        buf[new_head..*head].copy_from_slice(prefix);
        *head = new_head;
        return 0;
    }
    let old_head = *head;
    let old_len = buf.len();
    let suffix_len = old_len - old_head;
    let new_head = (suffix_len + p).max(64);
    let new_suffix_start = new_head + p;
    buf.resize(new_suffix_start + suffix_len, 0);
    buf.copy_within(old_head..old_len, new_suffix_start);
    buf[new_head..new_suffix_start].copy_from_slice(prefix);
    *head = new_head;
    suffix_len
}

#[inline]
fn trimmed_trailing_slash_len(bytes: &[u8]) -> usize {
    if bytes.ends_with(b"/") {
        bytes.len() - 1
    } else {
        bytes.len()
    }
}

/// Maps a relative-candidate offset in `raw` to the corresponding offset in
/// `precomposed`. Component-wise NFC preserves every `/` separator 1-to-1, so
/// counting `/` bytes inside `&raw[raw_rel_start..]` locates the exact matching
/// separator in `precomposed`.
fn map_rel_start_to_precomposed(raw: &[u8], raw_rel_start: usize, precomposed: &[u8]) -> usize {
    if raw_rel_start == 0 {
        return 0;
    }
    #[cfg(test)]
    NFC_OFFSET_MAP_BYTES.with(|count| count.set(count.get() + raw.len()));
    if raw[raw_rel_start - 1] == b'/' {
        let trailing_slashes = memchr::memchr_iter(b'/', &raw[raw_rel_start..]).count();
        if let Some(slash) = memchr::memrchr_iter(b'/', precomposed).nth(trailing_slashes) {
            return slash + 1;
        }
    }
    debug_assert!(raw[..raw_rel_start].is_ascii());
    raw_rel_start
}

#[inline]
fn last_component_offset(candidate: &[u8]) -> (usize, usize) {
    let trimmed_len = trimmed_trailing_slash_len(candidate);
    let start = memchr::memrchr(b'/', &candidate[..trimmed_len]).map_or(0, |pos| pos + 1);
    (start, trimmed_len)
}

/// The rules of one ignore file, in file order.
///
/// Rules are scanned one by one, where the engine this replaced compiled all of
/// them into a single automaton. Measured with `rule_engine_cost`, that is much
/// faster for the sets real repositories carry - a verdict costs about 1 ns
/// against 60 with one rule, and 28 ns against 125 with ten - and slower once a
/// file grows past roughly forty rules, where the automaton's constant cost
/// finally beats a linear scan. Closing that end means a multi-pattern search
/// over the gates, which is a dependency this crate just shed; it is worth
/// revisiting only if ignore files that large turn up in practice.
#[derive(Debug)]
pub(crate) struct RuleSet {
    /// Normalized glob prefix of the directory the rules are anchored to,
    /// trailing separator included unless the root is empty or a bare Windows
    /// drive.
    ///
    /// Candidates arrive as the whole path, converted once for the entire
    /// chain, so each set strips its own prefix instead of walking the
    /// components again. Checking the prefix also prevents a mismatched
    /// spelling from slicing mid-filename.
    root_prefix: Vec<u8>,
    case_insensitive: bool,
    precompose_unicode: bool,
    /// Whether at least one rule in `rules` can match non-directory entries
    /// (`!rule.directory_only`). Lets file candidates skip directory-only rule
    /// sets (such as a root `.gitignore` containing only `node_modules/`) in
    /// one branch before prefix stripping or normalization.
    has_file_rules: bool,
    /// Whether every rule in `rules` is `RuleKind::Basename`, meaning this set
    /// depends strictly on the final path component and never needs ancestor
    /// directories to be NFC-precomposed.
    only_basename_rules: bool,
    rules: Vec<Rule>,
    compiled_units: usize,
    retained_bytes: usize,
}

impl RuleSet {
    pub(crate) fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub(crate) fn compiled_units(&self) -> usize {
        self.compiled_units
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// The verdict of the last rule that matches, or `None` when none does.
    ///
    /// `path` is the candidate's whole path in glob bytes; the part below this
    /// set's directory is what the rules see. Last-match-wins is Git's rule, so
    /// the scan runs backwards and stops at the first hit.
    #[cfg(any(test, feature = "unstable-test-hooks"))]
    pub(crate) fn matched(&self, path: &[u8], is_dir: bool) -> Option<bool> {
        let mut scratch = IgnoreMatchScratch::default();
        let mut ctx = EntryMatchContext::new(path, &mut scratch);
        self.matched_in_context(&mut ctx, is_dir)
    }

    /// Evaluates this rule set using a shared [`EntryMatchContext`] hoisted
    /// across an ancestor chain, reusing precomposed and case-folded buffers
    /// without per-node allocation or redundant normalization.
    pub(crate) fn matched_in_context(
        &self,
        ctx: &mut EntryMatchContext<'_>,
        is_dir: bool,
    ) -> Option<bool> {
        if !is_dir && !self.has_file_rules {
            return None;
        }
        let raw_rel_start = self.strip_root_prefix_offset(ctx.raw)?;
        if raw_rel_start == ctx.raw.len() {
            return None;
        }
        let (view, rel_start) = if self.precompose_unicode {
            if self.only_basename_rules {
                ctx.resolve_precomposed_basename(raw_rel_start)
            } else {
                ctx.resolve_precomposed(raw_rel_start)
            }
        } else {
            (ViewKind::Raw, raw_rel_start)
        };
        let end = ctx.view_bytes(view).len();
        if rel_start == end {
            return None;
        }

        if !self.case_insensitive {
            let candidate = &ctx.view_bytes(view)[rel_start..end];
            if candidate.len() <= 256 {
                // Typical paths: SIMD `memmem::Finder` over `candidate` is
                // faster than extracting a short basename (< 16 B SIMD width),
                // so run `rule.gate` directly before dispatching on `rule.kind`.
                let mut basename_offset: Option<(usize, usize)> = None;
                let mut is_nested: Option<bool> = None;
                let mut component_counter = ComponentCounter::default();
                for rule in self.rules.iter().rev() {
                    if rule.directory_only && !is_dir {
                        continue;
                    }
                    if let Some(gate) = rule.gate.as_ref()
                        && gate.find(candidate).is_none()
                    {
                        continue;
                    }
                    let matched = match rule.kind {
                        RuleKind::RootSingle => {
                            let nested = *is_nested.get_or_insert_with(|| {
                                let trimmed = trimmed_trailing_slash_len(candidate);
                                memchr::memchr(b'/', &candidate[..trimmed]).is_some()
                            });
                            if nested {
                                false
                            } else {
                                let trimmed = trimmed_trailing_slash_len(candidate);
                                match rule.components.as_slice() {
                                    [Component::AnyOne] => true,
                                    [Component::Pattern(pattern)] => {
                                        pattern.is_match_glob_path(&candidate[..trimmed])
                                    }
                                    components => matches_components(components, candidate),
                                }
                            }
                        }
                        RuleKind::Basename => {
                            let (base_start, base_end) = *basename_offset
                                .get_or_insert_with(|| last_component_offset(candidate));
                            match rule.components.as_slice() {
                                [_, Component::AnyOne] => true,
                                [_, Component::Pattern(pattern)] => {
                                    pattern.is_match_glob_path(&candidate[base_start..base_end])
                                }
                                components => matches_components(components, candidate),
                            }
                        }
                        RuleKind::FixedDepthBasename => {
                            let (depth, fixed_byte_len) = unpack_pair_meta(rule.fast_meta);
                            if !component_counter.matches_exact(candidate, depth) {
                                false
                            } else {
                                let (base_start, base_end) = *basename_offset
                                    .get_or_insert_with(|| last_component_offset(candidate));
                                let base = &candidate[base_start..base_end];
                                if let Some(expected_len) = fixed_byte_len
                                    && base.len() != expected_len
                                {
                                    false
                                } else {
                                    match rule.components.last() {
                                        Some(Component::AnyOne) => true,
                                        Some(Component::Pattern(pattern)) => {
                                            pattern.is_match_glob_path(base)
                                        }
                                        _ => matches_components(&rule.components, candidate),
                                    }
                                }
                            }
                        }
                        RuleKind::AnchoredFastPrefix | RuleKind::Multi => {
                            matches_components(&rule.components, candidate)
                        }
                    };
                    if matched {
                        return Some(!rule.negated);
                    }
                }
                return None;
            }
            let mut basename_offset: Option<(usize, usize)> = None;
            let mut is_nested: Option<bool> = None;
            let mut component_counter = ComponentCounter::default();
            for rule in self.rules.iter().rev() {
                if rule.directory_only && !is_dir {
                    continue;
                }
                match rule.kind {
                    RuleKind::RootSingle => {
                        let nested = *is_nested.get_or_insert_with(|| {
                            let trimmed = trimmed_trailing_slash_len(candidate);
                            memchr::memchr(b'/', &candidate[..trimmed]).is_some()
                        });
                        if nested {
                            continue;
                        }
                        let trimmed = trimmed_trailing_slash_len(candidate);
                        if let Some(expected_len) = rule.fast_meta
                            && trimmed != expected_len.get() as usize
                        {
                            continue;
                        }
                        if let Some(gate) = rule.gate.as_ref()
                            && gate.find(candidate).is_none()
                        {
                            continue;
                        }
                        let matched = match rule.components.as_slice() {
                            [Component::AnyOne] => true,
                            [Component::Pattern(pattern)] => {
                                pattern.is_match_glob_path(&candidate[..trimmed])
                            }
                            components => matches_components(components, candidate),
                        };
                        if matched {
                            return Some(!rule.negated);
                        }
                    }
                    RuleKind::Basename => {
                        let (base_start, base_end) = *basename_offset
                            .get_or_insert_with(|| last_component_offset(candidate));
                        let base = &candidate[base_start..base_end];
                        if let Some(expected_len) = rule.fast_meta
                            && base.len() != expected_len.get() as usize
                        {
                            continue;
                        }
                        if let Some(gate) = rule.gate.as_ref()
                            && gate.find(base).is_none()
                        {
                            continue;
                        }
                        let matched = match rule.components.as_slice() {
                            [_, Component::AnyOne] => true,
                            [_, Component::Pattern(pattern)] => pattern.is_match_glob_path(base),
                            components => matches_components(components, candidate),
                        };
                        if matched {
                            return Some(!rule.negated);
                        }
                    }
                    RuleKind::FixedDepthBasename => {
                        let (depth, fixed_byte_len) = unpack_pair_meta(rule.fast_meta);
                        if !component_counter.matches_exact(candidate, depth) {
                            continue;
                        }
                        let (base_start, base_end) = *basename_offset
                            .get_or_insert_with(|| last_component_offset(candidate));
                        let base = &candidate[base_start..base_end];
                        if let Some(expected_len) = fixed_byte_len
                            && base.len() != expected_len
                        {
                            continue;
                        }
                        if let Some(gate) = rule.gate.as_ref()
                            && gate.find(base).is_none()
                        {
                            continue;
                        }
                        let matched = match rule.components.last() {
                            Some(Component::AnyOne) => true,
                            Some(Component::Pattern(pattern)) => pattern.is_match_glob_path(base),
                            _ => matches_components(&rule.components, candidate),
                        };
                        if matched {
                            return Some(!rule.negated);
                        }
                    }
                    RuleKind::AnchoredFastPrefix => {
                        let (_, fixed_depth) = unpack_pair_meta(rule.fast_meta);
                        if self.rules.len() > 1
                            && let Some(expected_depth) = fixed_depth
                            && !component_counter.matches_exact(candidate, expected_depth)
                        {
                            continue;
                        }
                        if let Some(gate) = rule.gate.as_ref()
                            && gate.find(candidate).is_none()
                        {
                            continue;
                        }
                        if matches_components(&rule.components, candidate) {
                            return Some(!rule.negated);
                        }
                    }
                    RuleKind::Multi => {
                        if let Some(expected_depth) = rule.fast_meta
                            && !component_counter
                                .matches_exact(candidate, expected_depth.get() as usize)
                        {
                            continue;
                        }
                        if let Some(gate) = rule.gate.as_ref()
                            && gate.find(candidate).is_none()
                        {
                            continue;
                        }
                        if matches_components(&rule.components, candidate) {
                            return Some(!rule.negated);
                        }
                    }
                }
            }
            return None;
        }

        // Case-insensitive path: reject impossible `RootSingle` rules on
        // nested candidates, reject fixed-width `RootSingle`/`Basename` rules
        // by byte length before folding/gating, match all leading non-universal
        // anchored components of `AnchoredFastPrefix` rules before folding or
        // gating any tail, reject fixed-depth `Multi`/`AnchoredFastPrefix`
        // rules by component count before folding/gating, and gate `Basename`
        // rules strictly on the basename slice (skipping folding completely
        // when `!rule.gate_needs_fold`).
        let mut basename: Option<(usize, usize)> = None;
        let mut is_nested: Option<bool> = None;
        let mut first_split: Option<(usize, usize)> = None;
        let mut component_counter = ComponentCounter::default();
        for rule in self.rules.iter().rev() {
            if rule.directory_only && !is_dir {
                continue;
            }
            match rule.kind {
                RuleKind::RootSingle => {
                    let nested = *is_nested.get_or_insert_with(|| {
                        let cand = &ctx.view_bytes(view)[rel_start..end];
                        let trimmed = trimmed_trailing_slash_len(cand);
                        memchr::memchr(b'/', &cand[..trimmed]).is_some()
                    });
                    if nested {
                        continue;
                    }
                    let trimmed_end = rel_start
                        + trimmed_trailing_slash_len(&ctx.view_bytes(view)[rel_start..end]);
                    if let Some(expected_len) = rule.fast_meta
                        && trimmed_end - rel_start != expected_len.get() as usize
                    {
                        continue;
                    }
                    if let Some(gate) = rule.gate.as_ref() {
                        let haystack = if rule.gate_needs_fold {
                            ctx.gate_haystack_ci(view, rel_start, trimmed_end)
                        } else {
                            &ctx.view_bytes(view)[rel_start..trimmed_end]
                        };
                        if gate.find(haystack).is_none() {
                            continue;
                        }
                    }
                    let bytes = ctx.view_bytes(view);
                    let matched = match rule.components.as_slice() {
                        [Component::AnyOne] => true,
                        [Component::Pattern(pattern)] => {
                            pattern.is_match_glob_path(&bytes[rel_start..trimmed_end])
                        }
                        components => matches_components(components, &bytes[rel_start..end]),
                    };
                    if matched {
                        return Some(!rule.negated);
                    }
                }
                RuleKind::AnchoredFastPrefix => {
                    let (leading, fixed_depth) = unpack_pair_meta(rule.fast_meta);
                    if self.rules.len() > 1
                        && let Some(expected_depth) = fixed_depth
                        && !component_counter
                            .matches_exact(&ctx.view_bytes(view)[rel_start..end], expected_depth)
                    {
                        continue;
                    }
                    let (first_head_end, first_tail_start) =
                        *first_split.get_or_insert_with(|| {
                            let cand = &ctx.view_bytes(view)[rel_start..end];
                            match memchr::memchr(b'/', cand) {
                                Some(sep) => (rel_start + sep, rel_start + sep + 1),
                                None => (end, end),
                            }
                        });
                    let bytes = ctx.view_bytes(view);
                    match rule.components.first() {
                        Some(Component::AnyOne) => {}
                        Some(Component::Pattern(first_pat)) => {
                            if !first_pat.is_match_glob_path(&bytes[rel_start..first_head_end]) {
                                continue;
                            }
                        }
                        _ => {
                            if matches_components(&rule.components, &bytes[rel_start..end]) {
                                return Some(!rule.negated);
                            }
                            continue;
                        }
                    }
                    let mut cursor = first_tail_start;
                    let mut prefix_matched = true;
                    for comp in &rule.components[1..leading] {
                        let rem = &bytes[cursor..end];
                        if rem.is_empty() {
                            prefix_matched = false;
                            break;
                        }
                        let (head_end, next_cursor) = match memchr::memchr(b'/', rem) {
                            Some(sep) => (cursor + sep, cursor + sep + 1),
                            None => (end, end),
                        };
                        match comp {
                            Component::AnyOne => {}
                            Component::Pattern(pat) => {
                                if !pat.is_match_glob_path(&bytes[cursor..head_end]) {
                                    prefix_matched = false;
                                    break;
                                }
                            }
                            _ => {
                                prefix_matched = false;
                                break;
                            }
                        }
                        cursor = next_cursor;
                    }
                    if !prefix_matched {
                        continue;
                    }
                    let rest = &rule.components[leading..];
                    let matched = if rest.is_empty() {
                        cursor == end
                    } else {
                        // `rule.gate` was derived strictly from
                        // `parts[leading..]`, so search only `cursor..end`.
                        let gate_ok = rule.gate.as_ref().is_none_or(|gate| {
                            let haystack = if rule.gate_needs_fold {
                                ctx.gate_haystack_ci(view, cursor, end)
                            } else {
                                &ctx.view_bytes(view)[cursor..end]
                            };
                            gate.find(haystack).is_some()
                        });
                        gate_ok && matches_components(rest, &ctx.view_bytes(view)[cursor..end])
                    };
                    if matched {
                        return Some(!rule.negated);
                    }
                }
                RuleKind::Basename => {
                    let (base_start, base_end) =
                        *basename.get_or_insert_with(|| ctx.basename_range(view, rel_start));
                    if let Some(expected_len) = rule.fast_meta
                        && base_end - base_start != expected_len.get() as usize
                    {
                        continue;
                    }
                    if let Some(gate) = rule.gate.as_ref() {
                        let haystack = if rule.gate_needs_fold {
                            ctx.gate_haystack_ci(view, base_start, base_end)
                        } else {
                            &ctx.view_bytes(view)[base_start..base_end]
                        };
                        if gate.find(haystack).is_none() {
                            continue;
                        }
                    }
                    let bytes = ctx.view_bytes(view);
                    let base = &bytes[base_start..base_end];
                    #[cfg(debug_assertions)]
                    {
                        let candidate = &bytes[rel_start..end];
                        let (s, e) = last_component_offset(candidate);
                        debug_assert_eq!(base, &candidate[s..e]);
                    }
                    let matched = match rule.components.as_slice() {
                        [_, Component::AnyOne] => true,
                        [_, Component::Pattern(pattern)] => pattern.is_match_glob_path(base),
                        components => matches_components(components, &bytes[rel_start..end]),
                    };
                    if matched {
                        return Some(!rule.negated);
                    }
                }
                RuleKind::FixedDepthBasename => {
                    let (depth, fixed_byte_len) = unpack_pair_meta(rule.fast_meta);
                    if !component_counter
                        .matches_exact(&ctx.view_bytes(view)[rel_start..end], depth)
                    {
                        continue;
                    }
                    let (base_start, base_end) =
                        *basename.get_or_insert_with(|| ctx.basename_range(view, rel_start));
                    if let Some(expected_len) = fixed_byte_len
                        && base_end - base_start != expected_len
                    {
                        continue;
                    }
                    if let Some(gate) = rule.gate.as_ref() {
                        let haystack = if rule.gate_needs_fold {
                            ctx.gate_haystack_ci(view, base_start, base_end)
                        } else {
                            &ctx.view_bytes(view)[base_start..base_end]
                        };
                        if gate.find(haystack).is_none() {
                            continue;
                        }
                    }
                    let bytes = ctx.view_bytes(view);
                    let base = &bytes[base_start..base_end];
                    let matched = match rule.components.last() {
                        Some(Component::AnyOne) => true,
                        Some(Component::Pattern(pattern)) => pattern.is_match_glob_path(base),
                        _ => matches_components(&rule.components, &bytes[rel_start..end]),
                    };
                    if matched {
                        return Some(!rule.negated);
                    }
                }
                RuleKind::Multi => {
                    if let Some(expected_depth) = rule.fast_meta
                        && !component_counter.matches_exact(
                            &ctx.view_bytes(view)[rel_start..end],
                            expected_depth.get() as usize,
                        )
                    {
                        continue;
                    }
                    if let Some(gate) = rule.gate.as_ref() {
                        let haystack = if rule.gate_needs_fold {
                            ctx.gate_haystack_ci(view, rel_start, end)
                        } else {
                            &ctx.view_bytes(view)[rel_start..end]
                        };
                        if gate.find(haystack).is_none() {
                            continue;
                        }
                    }
                    if matches_components(&rule.components, &ctx.view_bytes(view)[rel_start..end]) {
                        return Some(!rule.negated);
                    }
                }
            }
        }
        None
    }

    #[inline]
    fn strip_root_prefix_offset(&self, path: &[u8]) -> Option<usize> {
        if self.root_prefix.is_empty() {
            return Some(0);
        }
        let rest = path.strip_prefix(self.root_prefix.as_slice())?;
        let mut offset = self.root_prefix.len();
        if self.root_prefix.ends_with(b"/") && rest.first() == Some(&b'/') {
            offset += rest.iter().take_while(|&&byte| byte == b'/').count();
        }
        Some(offset)
    }
}

/// Fast-path shape of a compiled [`Rule`], stored inline so rule scanning
/// selects the gate haystack without chasing `Rule::components` on gate misses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleKind {
    /// `[Component::AnyDirs, Component::AnyOne | Component::Pattern(_)]` —
    /// matches only the final path component. `Rule::fast_meta` holds the exact
    /// byte length when the pattern is fixed-width (contains no `*`).
    Basename,
    /// `[Component::AnyOne | Component::Pattern(_)]` — matches only a
    /// single-component candidate. `Rule::fast_meta` holds the exact byte
    /// length when the pattern is fixed-width (contains no `*`).
    RootSingle,
    /// Multi-component anchored rule starting with `Rule::fast_meta` (`>= 1`)
    /// leading single-component elements (`Component::AnyOne | Component::Pattern(_)`,
    /// before any `AnyDirs` / `AnyDirsNonEmpty`) that include at least one
    /// non-universal `Component::Pattern` (excluding `FixedDepthBasename`,
    /// whose leading `k - 1` components are all `Component::AnyOne`). Checks
    /// all leading components before folding or gating any tail.
    AnchoredFastPrefix,
    /// Fixed-depth rule `[AnyOne, ..., AnyOne, AnyOne | Pattern]` (`k >= 2`
    /// components where all `k - 1` leading components are universal `*`). Once
    /// `ComponentCounter::matches_exact(candidate, k)` succeeds, all leading
    /// `*` components match unconditionally and only the final component
    /// (`basename`) needs matching (zero folding on simple tail patterns).
    FixedDepthBasename,
    /// General multi-component rule gated first and walked by
    /// [`matches_components`].
    Multi,
}

#[derive(Debug)]
struct Rule {
    /// The rule's directory-spanning component spelling.
    components: Vec<Component>,
    /// A searcher for the longest run of literal bytes the rule requires.
    ///
    /// A file with a hundred rules asks every one of them about every entry,
    /// and almost all of those questions have the same answer. A rule that
    /// spells out `node_modules` or `.tmp7` cannot match a candidate that does
    /// not contain those bytes, so one substring search rejects it before the
    /// component walk starts. The searcher is built once with the rule, not per
    /// candidate, because building one costs more than running it. When
    /// `core.ignoreCase = true`, the gate literal is stored ASCII-lowercased
    /// and tested against either the raw candidate (when it has no ASCII
    /// uppercase bytes or `!gate_needs_fold`) or a per-entry folded buffer.
    /// Rules made only of wildcards have no gate and are walked as before.
    gate: Option<memmem::Finder<'static>>,
    /// Inline fast-path discriminator for `components`.
    kind: RuleKind,
    /// `!` prefix: a match re-includes instead of ignoring.
    negated: bool,
    /// Trailing `/`: the rule matches directories only.
    directory_only: bool,
    /// Whether `gate` contains at least one ASCII letter under
    /// `case_insensitive = true`. When `false`, the gate literal is case-neutral
    /// (e.g. `"2026"`, `"---123---"`) and searches raw/precomposed bytes
    /// directly without ASCII folding.
    gate_needs_fold: bool,
    /// Kind-specific inline fast-path metadata (fits in existing 4-byte tail
    /// padding of `Rule`, keeping `size_of::<Rule>()` unchanged):
    /// - `Basename` / `RootSingle`: exact matched byte length when fixed-width.
    /// - `AnchoredFastPrefix`: low 16 bits = count of leading anchored
    ///   single-component elements (`1..=u16::MAX`, containing at least one
    ///   `Component::Pattern`), high 16 bits = exact fixed component count (`0`
    ///   when variable depth).
    /// - `FixedDepthBasename`: low 16 bits = exact component count `k`
    ///   (`2..=u16::MAX`), high 16 bits = exact byte length of the final
    ///   component (`0` when variable width).
    /// - `Multi`: exact fixed component count when every component is
    ///   `Component::AnyOne | Component::Pattern(_)` (no
    ///   `AnyDirs`/`AnyDirsNonEmpty`), else `None`.
    fast_meta: Option<NonZeroU32>,
}

#[inline]
fn pack_pair_meta(primary: usize, secondary: Option<usize>) -> Option<NonZeroU32> {
    let primary = u16::try_from(primary).ok().filter(|&n| n > 0)?;
    let secondary = secondary
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(0);
    NonZeroU32::new((u32::from(secondary) << 16) | u32::from(primary))
}

#[inline]
fn unpack_pair_meta(meta: Option<NonZeroU32>) -> (usize, Option<usize>) {
    let raw = meta.map_or(1, NonZeroU32::get);
    let primary = (raw & 0xFFFF) as usize;
    let secondary = (raw >> 16) as usize;
    (primary, (secondary != 0).then_some(secondary))
}

/// Progressive, resumable component-count checker for a non-empty `candidate`.
///
/// Advances through `&candidate[..trimmed_trailing_slash_len(candidate)]` only
/// until either `components > needed` or the entire slice has been scanned, so
/// shallow fixed-depth rules stop at the `needed`-th `/` on multi-kilobyte deep
/// paths while mixed-depth rule sets resume without ever rescanning a byte.
struct ComponentCounter {
    components: usize,
    scanned: usize,
}

impl Default for ComponentCounter {
    #[inline]
    fn default() -> Self {
        Self {
            components: 1,
            scanned: 0,
        }
    }
}

impl ComponentCounter {
    #[inline]
    fn matches_exact(&mut self, candidate: &[u8], needed: usize) -> bool {
        let trimmed = &candidate[..trimmed_trailing_slash_len(candidate)];
        while self.components <= needed && self.scanned < trimmed.len() {
            match memchr::memchr(b'/', &trimmed[self.scanned..]) {
                Some(pos) => {
                    self.components += 1;
                    self.scanned += pos + 1;
                }
                None => {
                    self.scanned = trimmed.len();
                    break;
                }
            }
        }
        self.components == needed && self.scanned == trimmed.len()
    }
}

#[cfg(test)]
impl Rule {
    fn matches(&self, candidate: &[u8], is_dir: bool) -> bool {
        if self.directory_only && !is_dir {
            return false;
        }
        self.gate
            .as_ref()
            .is_none_or(|gate| gate.find(candidate).is_some())
            && matches_components(&self.components, candidate)
    }
}

#[derive(Debug)]
enum Component {
    /// A `**` component: zero or more whole path components.
    AnyDirs,
    /// A `**` followed by an escaped slash: one or more path components.
    ///
    /// Git lets that star run cross separators, but its zero-width shortcut
    /// applies only when the following slash is unescaped.
    AnyDirsNonEmpty,
    /// A single universal `*` component: matches exactly one path component
    /// unconditionally without invoking `ferralk-glob`.
    AnyOne,
    /// One component, matched by ferralk-glob.
    Pattern(Pattern),
}

/// Whether the rule's components match the whole candidate.
///
/// The classic wildcard walk, one level up: `AnyDirs` stands for any run of
/// components, every other component consumes exactly one, and a mismatch lets
/// the last run swallow one component more. That keeps the work proportional to
/// components times rule length; the recursive form this replaced was
/// exponential in the number of `**` components, and rules come from files
/// inside the tree being walked, so that was reachable input.
///
/// Whether a trailing `**` may match nothing is decided when the rule is
/// parsed, not here: `abc/**` gains a component that requires something below
/// `abc`.
fn matches_components(components: &[Component], candidate: &[u8]) -> bool {
    #[cfg(test)]
    FULL_COMPONENT_MATCHES.with(|count| count.set(count.get() + 1));

    let mut index = 0;
    let mut rest = candidate;
    // Where the last `**` began: its position in the rule, and what it had not
    // yet swallowed.
    let mut run: Option<(usize, &[u8])> = None;

    while !rest.is_empty() {
        if let Some(Component::AnyDirs) = components.get(index) {
            run = Some((index, rest));
            index += 1;
            continue;
        }
        if let Some(Component::AnyDirsNonEmpty) = components.get(index) {
            let (_, tail) = split_component(rest);
            run = Some((index, tail));
            index += 1;
            rest = tail;
            continue;
        }
        if let Some(Component::AnyOne) = components.get(index) {
            let (_, tail) = split_component(rest);
            index += 1;
            rest = tail;
            continue;
        }
        if let Some(Component::Pattern(pattern)) = components.get(index) {
            let (head, tail) = split_component(rest);
            if pattern.is_match_glob_path(head) {
                index += 1;
                rest = tail;
                continue;
            }
        }
        // Either the rule ran out with candidate left, or this component does
        // not match: both are for the last run to absorb, if there is one.
        let Some((run_index, unswallowed)) = run else {
            return false;
        };
        if unswallowed.is_empty() {
            return false;
        }
        let (_, tail) = split_component(unswallowed);
        run = Some((run_index, tail));
        index = run_index + 1;
        rest = tail;
    }

    // The candidate is spent, so what is left of the rule may only be runs,
    // which are allowed to match nothing.
    components[index..]
        .iter()
        .all(|component| matches!(component, Component::AnyDirs))
}

/// Returns `true` when `pattern` has at most one unescaped `*` run and no
/// unescaped `?` or `[` metacharacters (`literal`, `*literal`, `literal*`, or
/// `literal*literal`). `ferralk-glob` matches these directly without scanning
/// the full component, so a `memmem::Finder` gate under `case_insensitive = true`
/// adds redundant folding/scanning overhead.
fn is_simple_single_component_pattern(pattern: &[u8]) -> bool {
    let mut stars = 0_usize;
    let mut index = 0;
    while index < pattern.len() {
        match pattern[index] {
            b'\\' => index += 2,
            b'?' | b'[' => return false,
            b'*' => {
                stars += 1;
                if stars > 1 {
                    return false;
                }
                while index < pattern.len() && pattern[index] == b'*' {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    true
}

/// Returns the exact byte length matched by a normalized single component when
/// it contains no unescaped `*` wildcard, or `None` if its matched length is
/// variable or overflows `u32`.
fn fixed_component_byte_len(pattern: &[u8]) -> Option<NonZeroU32> {
    let mut byte_len = 0_usize;
    let mut index = 0;
    while index < pattern.len() {
        match pattern[index] {
            b'*' => return None,
            b'\\' => {
                if index + 1 >= pattern.len() {
                    return None;
                }
                byte_len += 1;
                index += 2;
            }
            b'[' => {
                let (_, next) = parse_class_ir_members(pattern, index)?;
                byte_len += 1;
                index = next;
            }
            _ => {
                byte_len += 1;
                index += 1;
            }
        }
    }
    u32::try_from(byte_len).ok().and_then(NonZeroU32::new)
}

/// Splits off the first path component; the tail is empty at the last one.
fn split_component(candidate: &[u8]) -> (&[u8], &[u8]) {
    match memchr::memchr(b'/', candidate) {
        Some(separator) => (&candidate[..separator], &candidate[separator + 1..]),
        None => (candidate, &[]),
    }
}

/// Collects the rules of one or more ignore files for the same directory.
pub(crate) struct RuleSetBuilder {
    root_prefix: Vec<u8>,
    case_insensitive: bool,
    precompose_unicode: bool,
    rules: Vec<Rule>,
    compiled_units: usize,
    retained_bytes: usize,
}

impl RuleSetBuilder {
    pub(crate) fn new(root: &Path, case_insensitive: bool, precompose_unicode: bool) -> Self {
        let root = glob_path_bytes(root);
        let mut root_prefix = without_dot_components(&root).into_owned();
        while root_prefix.ends_with(b"//") {
            root_prefix.pop();
        }
        // The separator between the directory and what follows it belongs to
        // the prefix, unless the walk spelled its entries without one: after
        // a trailing separator or a bare Windows drive (#436). The empty root
        // is used by the fuzz helper, where candidates are already relative
        // and so have no prefix to remove.
        if crate::needs_separator_before_name(&root_prefix) {
            root_prefix.push(b'/');
        }
        Self {
            root_prefix,
            case_insensitive,
            precompose_unicode,
            rules: Vec::new(),
            compiled_units: 0,
            retained_bytes: 0,
        }
    }

    pub(crate) fn compiled_units(&self) -> usize {
        self.compiled_units
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(crate) fn checkpoint(&self) -> RuleSetCheckpoint {
        RuleSetCheckpoint {
            rules_len: self.rules.len(),
            compiled_units: self.compiled_units,
            retained_bytes: self.retained_bytes,
        }
    }

    pub(crate) fn restore(&mut self, checkpoint: RuleSetCheckpoint) {
        self.rules.truncate(checkpoint.rules_len);
        self.rules.shrink_to_fit();
        self.compiled_units = checkpoint.compiled_units;
        self.retained_bytes = checkpoint.retained_bytes;
    }

    #[cfg(test)]
    pub(crate) fn rules_capacity(&self) -> usize {
        self.rules.capacity()
    }

    /// Adds one line within the given remaining per-file and active-chain
    /// budgets. A line that is blank, a comment, or malformed adds no rule,
    /// while any path parts already parsed still charge `parsed_parts`.
    pub(crate) fn add_line_within_budget(
        &mut self,
        line: impl AsRef<[u8]>,
        budgets: LineBudgets,
    ) -> Result<LineBudgetUsage, ComponentBudgetExceeded> {
        let first_rule_overhead = if self.rules.is_empty() {
            RULE_FIRST_EXTRA_BYTES.saturating_add(self.root_prefix.capacity())
        } else {
            0
        };
        let ParsedRuleLine {
            rule,
            parsed_parts,
            compiled_units,
            retained_bytes,
        } = parse_rule_with_options(line, self.case_insensitive, first_rule_overhead, budgets)?;
        let Some(rule) = rule else {
            return Ok(LineBudgetUsage {
                parsed_parts,
                compiled_units: 0,
                retained_bytes: 0,
            });
        };
        self.compiled_units += compiled_units;
        self.retained_bytes += retained_bytes;
        self.rules.push(rule);
        Ok(LineBudgetUsage {
            parsed_parts,
            compiled_units,
            retained_bytes,
        })
    }

    /// Adds one line under the single-file safety ceilings. A line that is
    /// blank, a comment, malformed, or over budget adds nothing.
    #[cfg(any(test, feature = "unstable-test-hooks"))]
    pub(crate) fn add_line(&mut self, line: impl AsRef<[u8]>) {
        let remaining_units = MAX_IGNORE_FILE_COMPONENTS.saturating_sub(self.compiled_units);
        let remaining_bytes = MAX_IGNORE_FILE_RETAINED_BYTES.saturating_sub(self.retained_bytes);
        let _ = self.add_line_within_budget(
            line,
            LineBudgets::single_file(remaining_units, remaining_bytes),
        );
    }

    pub(crate) fn build(self) -> RuleSet {
        let has_file_rules = self.rules.iter().any(|rule| !rule.directory_only);
        let only_basename_rules = self
            .rules
            .iter()
            .all(|rule| rule.kind == RuleKind::Basename);
        RuleSet {
            root_prefix: self.root_prefix,
            case_insensitive: self.case_insensitive,
            precompose_unicode: self.precompose_unicode,
            has_file_rules,
            only_basename_rules,
            rules: self.rules,
            compiled_units: self.compiled_units,
            retained_bytes: self.retained_bytes,
        }
    }
}

/// Removes components that are exactly `.`, preserving every other byte and
/// separator. Walk roots keep their caller-visible spelling; this is only the
/// lexical view used to align ignore-file prefixes with candidate paths.
pub(crate) fn without_dot_components(path: &[u8]) -> Cow<'_, [u8]> {
    if !path.split(|byte| *byte == b'/').any(|part| part == b".") {
        return Cow::Borrowed(path);
    }

    let mut normalized = Vec::with_capacity(path.len());
    let mut wrote_component = false;
    for component in path.split(|byte| *byte == b'/') {
        if component == b"." {
            continue;
        }
        if wrote_component {
            normalized.push(b'/');
        }
        normalized.extend_from_slice(component);
        wrote_component = true;
    }
    Cow::Owned(normalized)
}

/// Fuzz entry point for the rule layer.
///
/// Reads one rule line and asks it about one candidate, which is the whole
/// surface the harness needs: parsing and matching are the two halves that must
/// stay total, whatever bytes arrive.
#[cfg(any(test, feature = "unstable-test-hooks"))]
pub fn fuzz_rule(line: &str, candidate: &[u8], is_dir: bool) -> Option<bool> {
    fuzz_rule_bytes(line.as_bytes(), candidate, is_dir)
}

/// Opaque handle returned by [`build_ignore_rule_set_for_memory_test`] so
/// `pattern_memory_regression` measures `RuleSet`'s exact heap allocations
/// without extra boxing.
#[doc(hidden)]
#[cfg(feature = "unstable-test-hooks")]
#[derive(Debug)]
pub struct MemoryTestRuleSet(RuleSet);

#[doc(hidden)]
#[cfg(feature = "unstable-test-hooks")]
impl MemoryTestRuleSet {
    pub fn estimated_retained_bytes(&self) -> usize {
        self.0.retained_bytes()
    }
}

/// Development-only hook for `pattern_memory_regression` to measure actual
/// retained heap bytes of one compiled `RuleSet` (single-line or multi-line)
/// against its production estimator without duplicating the budget formula in
/// the test.
#[doc(hidden)]
#[cfg(feature = "unstable-test-hooks")]
pub fn build_ignore_rule_set_for_memory_test(contents: &[u8]) -> MemoryTestRuleSet {
    build_ignore_rule_set_for_memory_test_with_options(contents, false)
}

/// Development-only hook for `pattern_memory_regression` with explicit
/// `case_insensitive` configuration.
#[doc(hidden)]
#[cfg(feature = "unstable-test-hooks")]
pub fn build_ignore_rule_set_for_memory_test_with_options(
    contents: &[u8],
    case_insensitive: bool,
) -> MemoryTestRuleSet {
    let mut builder = RuleSetBuilder::new(Path::new("/fixture"), case_insensitive, false);
    for line in contents.split(|&byte| byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let usage = builder
            .add_line_within_budget(line, LineBudgets::UNBOUNDED)
            .expect("unbounded memory-test rule line must fit budget");
        assert!(
            usage.compiled_units > 0,
            "memory-test fixture {:?} compiled zero units",
            String::from_utf8_lossy(line)
        );
    }
    let rules = builder.build();
    assert!(
        !rules.is_empty(),
        "memory-test fixture {:?} produced an empty RuleSet",
        String::from_utf8_lossy(contents)
    );
    MemoryTestRuleSet(rules)
}

/// Byte-oriented fuzz entry point for the rule layer.
///
/// Ignore files are bytes, so this also reaches patterns that are not valid
/// UTF-8. It shares the same empty-root setup as [`fuzz_rule`].
#[doc(hidden)]
#[cfg(any(test, feature = "unstable-test-hooks"))]
pub fn fuzz_rule_bytes(line: &[u8], candidate: &[u8], is_dir: bool) -> Option<bool> {
    let mut builder = RuleSetBuilder::new(Path::new(""), false, false);
    builder.add_line(line);
    let rules = builder.build();
    // Production walks normalize `\` to `/` before calling `RuleSet` on
    // Windows; mirror that boundary here so `Component::AnyOne` sees the same
    // slash-separated candidate form as real walks.
    #[cfg(windows)]
    if candidate.contains(&b'\\') {
        let normalized: Vec<u8> = candidate
            .iter()
            .map(|&byte| if byte == b'\\' { b'/' } else { byte })
            .collect();
        return rules.matched(&normalized, is_dir);
    }
    rules.matched(candidate, is_dir)
}

struct ParsedRuleLine {
    rule: Option<Rule>,
    parsed_parts: usize,
    compiled_units: usize,
    retained_bytes: usize,
}

impl ParsedRuleLine {
    const fn discarded(parsed_parts: usize) -> Self {
        Self {
            rule: None,
            parsed_parts,
            compiled_units: 0,
            retained_bytes: 0,
        }
    }
}

/// Reads one `gitignore(5)` line into a rule, or nothing.
#[cfg(test)]
fn parse_rule(line: impl AsRef<[u8]>) -> Option<Rule> {
    parse_rule_with_options(line, false, RULE_FIRST_EXTRA_BYTES, LineBudgets::UNBOUNDED)
        .ok()
        .and_then(|parsed| parsed.rule)
}

fn parse_rule_with_options(
    line: impl AsRef<[u8]>,
    case_insensitive: bool,
    first_rule_overhead: usize,
    budgets: LineBudgets,
) -> Result<ParsedRuleLine, ComponentBudgetExceeded> {
    let line = line.as_ref();
    if line.is_empty() || line.starts_with(b"#") {
        return Ok(ParsedRuleLine::discarded(0));
    }
    let body = strip_trailing_spaces(line);
    if body.is_empty() {
        return Ok(ParsedRuleLine::discarded(0));
    }

    let (negated, body) = match body.strip_prefix(b"!") {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    // A rule ending in an unpaired backslash escapes nothing and matches
    // nothing; Git drops it rather than guessing.
    if trailing_backslashes(body) % 2 == 1 {
        return Ok(ParsedRuleLine::discarded(0));
    }

    let plain_trailing_separator = body
        .strip_suffix(b"/")
        .filter(|rest| trailing_backslashes(rest).is_multiple_of(2));
    let (directory_only, body) = match plain_trailing_separator {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    // Git decides anchoring with a plain slash search, even when that slash is
    // a dead member of a bracket class. Splitting still keeps class slashes in
    // their component because candidates never contain separators there.
    let anchored = body.contains(&b'/');
    let body = body.strip_prefix(b"/").unwrap_or(body);
    if body.is_empty() {
        return Ok(ParsedRuleLine::discarded(0));
    }

    let max_compiled = budgets
        .file_compiled_remaining
        .min(budgets.chain_compiled_remaining);
    let compiled_budget_exceeded =
        if budgets.file_compiled_remaining <= budgets.chain_compiled_remaining {
            ComponentBudgetExceeded::File
        } else {
            ComponentBudgetExceeded::Chain
        };
    let max_bytes = budgets
        .file_bytes_remaining
        .min(budgets.chain_bytes_remaining);
    let bytes_budget_exceeded = if budgets.file_bytes_remaining <= budgets.chain_bytes_remaining {
        ComponentBudgetExceeded::FileBytes
    } else {
        ComponentBudgetExceeded::ChainBytes
    };

    let rule_fixed_bytes = RULE_FIXED_RETAINED_BYTES.saturating_add(first_rule_overhead);

    // Every non-empty component before the last consumes at least one slash
    // (so compiled components <= raw_slashes + 2), each component pays up to
    // `MAX_COMPONENT_FIXED_BYTES` fixed overhead, and each byte pays up to
    // `MAX_BYTES_PER_BYTE` retained bytes. When the line could breach any
    // component or retained-byte budget, run a zero-allocation scan first so
    // crafted inputs fail fast before allocating `Vec<RulePart>`,
    // `Vec<NormalizedPart>`, or `Pattern`, and reuse the computed `RuleCost`
    // when the line passes.
    let min_unit_remaining = budgets.file_parsed_remaining.min(max_compiled);
    let slash_threshold = min_unit_remaining.saturating_sub(1);
    let max_possible_bytes_from_len = rule_fixed_bytes
        .saturating_add(
            (body.len() / 2)
                .saturating_add(2)
                .saturating_mul(MAX_COMPONENT_FIXED_BYTES),
        )
        .saturating_add(body.len().saturating_mul(MAX_BYTES_PER_BYTE));
    let might_exceed_bytes =
        max_bytes < usize::MAX && max_possible_bytes_from_len >= max_bytes && {
            let raw_slashes = body.iter().filter(|&&byte| byte == b'/').count();
            let refined_max_bytes = rule_fixed_bytes
                .saturating_add(
                    raw_slashes
                        .saturating_add(2)
                        .saturating_mul(MAX_COMPONENT_FIXED_BYTES),
                )
                .saturating_add(body.len().saturating_mul(MAX_BYTES_PER_BYTE));
            refined_max_bytes >= max_bytes
        };
    let might_exceed_before_split = min_unit_remaining <= 1
        || might_exceed_bytes
        || (body.len() >= slash_threshold
            && body
                .iter()
                .filter(|&&byte| byte == b'/')
                .take(slash_threshold)
                .count()
                >= slash_threshold);
    let pre_scanned_cost = if might_exceed_before_split {
        let mut acc = RuleCostAccumulator::new(anchored, rule_fixed_bytes, case_insensitive);
        let (valid, scanned_parts) =
            scan_rule_components(body, budgets.file_parsed_remaining, |part| {
                acc.push_part(&part);
            })
            .map_err(|()| ComponentBudgetExceeded::File)?;
        if !valid {
            return Ok(ParsedRuleLine::discarded(scanned_parts));
        }
        let cost = acc.finish();
        if cost.compiled_count == usize::from(!anchored) {
            return Ok(ParsedRuleLine::discarded(scanned_parts));
        }
        match check_rule_budget_overflow(
            body,
            case_insensitive,
            cost,
            max_compiled,
            compiled_budget_exceeded,
            max_bytes,
            bytes_budget_exceeded,
        ) {
            Ok(()) => Some(cost),
            Err(None) => return Ok(ParsedRuleLine::discarded(scanned_parts)),
            Err(Some(exceeded)) => return Err(exceeded),
        }
    } else {
        None
    };

    // A single leading separator is the anchor. Any other empty component is
    // unmatchable in Git, rather than an alternate spelling of a valid rule.
    let (parts, parsed_parts) = split_rule_components_bounded(body, budgets.file_parsed_remaining)
        .map_err(|()| ComponentBudgetExceeded::File)?;
    let Some(parts) = parts else {
        return Ok(ParsedRuleLine::discarded(parsed_parts));
    };

    // Reuse the pre-scan totals when available; otherwise compute compiled
    // components and retained bytes once before normalizing or compiling so
    // over-budget lines fail fast without allocating `NormalizedPart` or
    // `Pattern`.
    let cost = if let Some(cost) = pre_scanned_cost {
        cost
    } else {
        let mut acc = RuleCostAccumulator::new(anchored, rule_fixed_bytes, case_insensitive);
        for part in &parts {
            acc.push_part(part);
        }
        let cost = acc.finish();
        // A body of nothing but separators is not a rule.
        if cost.compiled_count == usize::from(!anchored) {
            return Ok(ParsedRuleLine::discarded(parsed_parts));
        }
        match check_rule_budget_overflow(
            body,
            case_insensitive,
            cost,
            max_compiled,
            compiled_budget_exceeded,
            max_bytes,
            bytes_budget_exceeded,
        ) {
            Ok(()) => cost,
            Err(None) => return Ok(ParsedRuleLine::discarded(parsed_parts)),
            Err(Some(exceeded)) => return Err(exceeded),
        }
    };

    let mut normalized = Vec::with_capacity(parts.len());
    for part in &parts {
        let Some(bytes) = normalize_bracket_classes(part.bytes, case_insensitive) else {
            return Ok(ParsedRuleLine::discarded(parsed_parts));
        };
        normalized.push(NormalizedPart {
            bytes,
            escaped_separator_after: part.escaped_separator_after,
        });
    }

    let mut components = Vec::with_capacity(cost.compiled_count);
    if !anchored {
        components.push(Component::AnyDirs);
    }
    for part in &normalized {
        let is_whole_stars = all_stars(&part.bytes) && part.bytes.len() >= 2;
        if is_whole_stars
            && !part.escaped_separator_after
            && matches!(components.last(), Some(Component::AnyDirs))
        {
            continue;
        }
        let Some(component) =
            compile_component(&part.bytes, case_insensitive, part.escaped_separator_after)
        else {
            return Ok(ParsedRuleLine::discarded(parsed_parts));
        };
        components.push(component);
    }
    if cost.trailing_whole_run {
        // `abc/**` is what is inside `abc`, so one component has to follow.
        let Some(component) = compile_component(b"*", case_insensitive, false) else {
            return Ok(ParsedRuleLine::discarded(parsed_parts));
        };
        components.push(component);
    }
    debug_assert_eq!(components.len(), cost.compiled_count);

    let leading_pattern_count = if anchored {
        components
            .iter()
            .take_while(|comp| matches!(comp, Component::AnyOne | Component::Pattern(_)))
            .count()
    } else {
        0
    };
    let leading_has_non_universal = components[..leading_pattern_count]
        .iter()
        .any(|comp| matches!(comp, Component::Pattern(_)));
    let leading_anchored_count = if leading_has_non_universal {
        leading_pattern_count
    } else {
        0
    };

    let fixed_depth = components
        .iter()
        .all(|comp| matches!(comp, Component::AnyOne | Component::Pattern(_)))
        .then_some(components.len());

    let is_fixed_depth_basename = fixed_depth.is_some_and(|depth| depth >= 2)
        && components[..components.len() - 1]
            .iter()
            .all(|comp| matches!(comp, Component::AnyOne));

    let (kind, fast_meta) = match components.as_slice() {
        [
            Component::AnyDirs,
            Component::AnyOne | Component::Pattern(_),
        ] => (
            RuleKind::Basename,
            normalized
                .last()
                .and_then(|part| fixed_component_byte_len(&part.bytes)),
        ),
        [Component::AnyOne | Component::Pattern(_)] => (
            RuleKind::RootSingle,
            normalized
                .last()
                .and_then(|part| fixed_component_byte_len(&part.bytes)),
        ),
        [Component::AnyOne | Component::Pattern(_), _, ..]
            if is_fixed_depth_basename
                && let Some(meta) = pack_pair_meta(
                    components.len(),
                    normalized
                        .last()
                        .and_then(|part| fixed_component_byte_len(&part.bytes))
                        .map(|len| len.get() as usize),
                ) =>
        {
            (RuleKind::FixedDepthBasename, Some(meta))
        }
        [Component::AnyOne | Component::Pattern(_), _, ..]
            if let Some(meta) = pack_pair_meta(leading_anchored_count, fixed_depth) =>
        {
            (RuleKind::AnchoredFastPrefix, Some(meta))
        }
        _ => (
            RuleKind::Multi,
            fixed_depth
                .and_then(|depth| u32::try_from(depth).ok())
                .and_then(NonZeroU32::new),
        ),
    };

    let gate_part_start = if case_insensitive {
        match kind {
            RuleKind::AnchoredFastPrefix => {
                // Under `case_insensitive = true`, `AnchoredFastPrefix` matches
                // all `leading_anchored_count` leading components directly;
                // skip compiling any gate when all components are covered, and
                // otherwise derive the gate strictly from `normalized[K..]`.
                if leading_anchored_count == components.len() {
                    None
                } else {
                    Some(leading_anchored_count)
                }
            }
            RuleKind::Basename | RuleKind::RootSingle | RuleKind::FixedDepthBasename
                if normalized
                    .last()
                    .is_some_and(|part| is_simple_single_component_pattern(&part.bytes)) =>
            {
                // Simple single-component patterns (`literal`, `*literal`,
                // `literal*`, `literal*literal`) already use direct fast paths
                // in `ferralk-glob`, so a case-insensitive `memmem::Finder`
                // gate would only add redundant folding/scanning overhead.
                None
            }
            RuleKind::FixedDepthBasename => Some(normalized.len() - 1),
            _ => Some(0),
        }
    } else {
        Some(0)
    };
    let gate_literal = gate_part_start.and_then(|start| {
        let mut best: Option<Vec<u8>> = None;
        for part in &normalized[start..] {
            if let Some(literal) = longest_literal_run(&part.bytes)
                && best.as_ref().is_none_or(|cur| cur.len() < literal.len())
            {
                best = Some(literal);
            }
        }
        best
    });
    let gate_needs_fold = case_insensitive
        && gate_literal
            .as_ref()
            .is_some_and(|literal| literal.iter().any(u8::is_ascii_alphabetic));
    let gate = gate_literal.map(|mut literal| {
        if gate_needs_fold {
            literal.make_ascii_lowercase();
        }
        memmem::Finder::new(&literal).into_owned()
    });

    Ok(ParsedRuleLine {
        rule: Some(Rule {
            components,
            gate,
            kind,
            negated,
            directory_only,
            gate_needs_fold,
            fast_meta,
        }),
        parsed_parts,
        compiled_units: cost.compiled_count,
        retained_bytes: cost.retained_bytes,
    })
}

#[derive(Clone, Copy)]
struct RuleCost {
    compiled_count: usize,
    retained_bytes: usize,
    trailing_whole_run: bool,
    saw_bracket: bool,
    has_large_component: bool,
}

struct RuleCostAccumulator {
    compiled_count: usize,
    retained_bytes: usize,
    case_insensitive: bool,
    prev_any_dirs: bool,
    last_was_whole_stars: bool,
    saw_bracket: bool,
    has_large_component: bool,
}

impl RuleCostAccumulator {
    fn new(anchored: bool, rule_fixed_bytes: usize, case_insensitive: bool) -> Self {
        let unanchored = usize::from(!anchored);
        Self {
            compiled_count: unanchored,
            retained_bytes: rule_fixed_bytes.saturating_add(unanchored * ANY_DIRS_COMPONENT_BYTES),
            case_insensitive,
            prev_any_dirs: !anchored,
            last_was_whole_stars: false,
            saw_bracket: false,
            has_large_component: false,
        }
    }

    fn push_part(&mut self, part: &RulePart<'_>) {
        if part.bytes.contains(&b'[') {
            self.saw_bracket = true;
        }
        let is_whole_stars = all_stars(part.bytes) && part.bytes.len() >= 2;
        if !is_whole_stars && part.bytes.len() >= MIN_IR_LIMIT_COMPONENT_BYTES {
            self.has_large_component = true;
        }
        self.last_was_whole_stars = is_whole_stars;
        if is_whole_stars && !part.escaped_separator_after {
            if !self.prev_any_dirs {
                self.prev_any_dirs = true;
                self.compiled_count += 1;
                self.retained_bytes = self.retained_bytes.saturating_add(ANY_DIRS_COMPONENT_BYTES);
            }
        } else {
            self.prev_any_dirs = false;
            self.compiled_count += 1;
            let bytes = if is_whole_stars {
                ANY_DIRS_COMPONENT_BYTES
            } else {
                component_cost(part.bytes, self.case_insensitive)
            };
            self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        }
    }

    fn finish(mut self) -> RuleCost {
        let trailing_whole_run = self.last_was_whole_stars;
        if trailing_whole_run {
            let bytes = component_cost(b"*", self.case_insensitive);
            self.compiled_count += 1;
            self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        }
        RuleCost {
            compiled_count: self.compiled_count,
            retained_bytes: self.retained_bytes,
            trailing_whole_run,
            saw_bracket: self.saw_bracket,
            has_large_component: self.has_large_component,
        }
    }
}

/// Verifies whether `cost` fits within the component and retained-byte budgets.
///
/// Returns `Ok(())` when within budget, `Err(None)` when an over-budget rule is
/// actually malformed or rejected by `ferralk-glob`'s per-pattern IR limit (and
/// therefore must be discarded cleanly regardless of line order or remaining
/// component/byte budget), or `Err(Some(exceeded))` for a genuine budget breach.
fn check_rule_budget_overflow(
    body: &[u8],
    case_insensitive: bool,
    cost: RuleCost,
    max_compiled: usize,
    compiled_budget_exceeded: ComponentBudgetExceeded,
    max_bytes: usize,
    bytes_budget_exceeded: ComponentBudgetExceeded,
) -> Result<(), Option<ComponentBudgetExceeded>> {
    let exceeded = if cost.compiled_count > max_compiled {
        compiled_budget_exceeded
    } else if cost.retained_bytes > max_bytes {
        bytes_budget_exceeded
    } else {
        return Ok(());
    };
    if cost.saw_bracket && !validate_rule_syntax_streaming(body, case_insensitive) {
        return Err(None);
    }
    if cost.has_large_component && !validate_large_components_ir_streaming(body, case_insensitive) {
        return Err(None);
    }
    Err(Some(exceeded))
}

/// Conservative upper-bound retained matcher bytes for one non-`**` component:
/// - Plain literal components (no unescaped `*`, `?`, `[`) cost
///   `LITERAL_COMPONENT_FIXED_BYTES + LITERAL_BYTES_PER_BYTE * part.len()`
///   retained bytes (covering `Component`, `CompiledAlternative`, literal/raw/
///   fast-path copies with `Vec` capacity doubling, and a rule-level
///   `memmem::Finder` gate).
/// - Non-literal components (containing unescaped `*`, `?`, or `[`) cost
///   `PATTERN_COMPONENT_FIXED_BYTES + [WIDE_SWEEP_EXTRA_BYTES] + PATTERN_BYTES_PER_BYTE * part.len()`
///   bytes (covering `Vec` power-of-two capacity doubling in `tokens` plus
///   `FastPath::DeterministicTokens` copies, single-star sweep state, `Class`
///   members, and `WideSweepEngine`'s 256-row bitmask table plus 64-bit word
///   rounding), doubled whenever a separator `/` appears inside the component
///   (e.g., an unrecognized POSIX class like `[[:a/b:]]`) because `PathFilter`
///   may retain a second compiled alternative.
///
/// Every compiled `Component` counts as 1 component toward component budgets.
/// Runs in O(part.len()) with zero heap allocations and no divergent
/// bracket-class parser.
/// Compact 256-bit byte set used to deduplicate repeated standalone literals
/// within a single bracket class across both [`component_cost`] and
/// [`normalize_bracket_class`].
#[derive(Default)]
struct ByteSet([u64; 4]);

impl ByteSet {
    fn insert(&mut self, byte: u8) -> bool {
        let word = &mut self.0[(byte >> 6) as usize];
        let mask = 1_u64 << (byte & 63);
        let fresh = (*word & mask) == 0;
        *word |= mask;
        fresh
    }

    /// Marks every byte in `lo..=hi` and returns `true` iff at least one byte
    /// in the range was not already present.
    fn insert_range(&mut self, lo: u8, hi: u8) -> bool {
        debug_assert!(lo <= hi);
        let start_word = (lo >> 6) as usize;
        let end_word = (hi >> 6) as usize;
        let mut fresh = false;
        for word_idx in start_word..=end_word {
            let low_bit = if word_idx == start_word { lo & 63 } else { 0 };
            let high_bit = if word_idx == end_word { hi & 63 } else { 63 };
            let width = high_bit - low_bit + 1;
            let mask = if width == 64 {
                u64::MAX
            } else {
                ((1_u64 << width) - 1) << low_bit
            };
            let word = &mut self.0[word_idx];
            if (*word & mask) != mask {
                fresh = true;
            }
            *word |= mask;
        }
        fresh
    }
}

#[derive(Clone, Copy)]
enum RawClassToken {
    Close,
    Range { lower: u8, upper: u8 },
    PosixSpan { bit: Option<u16>, is_space: bool },
    Literal(u8),
}

/// Shared raw bracket-class scanner state mirroring Git's `wildmatch.c` so
/// [`scan_rule_components`] and [`component_cost`] always agree on class
/// boundaries (including range-endpoint `[` consumption such as `a-[:digit:]`,
/// escapes, leading `]`/`!]`/`^]`, and `[:...:]` spans).
#[derive(Default)]
struct RawClassState {
    in_class: bool,
    saw_member: bool,
    previous: Option<u8>,
}

impl RawClassState {
    fn open(&mut self, bytes: &[u8], index: usize) -> usize {
        self.in_class = true;
        self.saw_member = false;
        self.previous = None;
        let next = index + 1;
        if matches!(bytes.get(next), Some(b'!' | b'^')) {
            next + 1
        } else {
            next
        }
    }

    fn step_in_class(
        &mut self,
        bytes: &[u8],
        index: usize,
        next_rbracket: &mut Option<usize>,
    ) -> (usize, RawClassToken) {
        debug_assert!(self.in_class);
        match bytes[index] {
            b'\\' => {
                self.saw_member = true;
                let literal = bytes.get(index + 1).copied().unwrap_or(b'\\');
                self.previous = Some(literal);
                (
                    (index + 2).min(bytes.len()),
                    RawClassToken::Literal(literal),
                )
            }
            b'-' if let Some(lower) = self.previous
                && let Some(&next) = bytes.get(index + 1)
                && next != b']' =>
            {
                let (upper, endpoint_len) = if next == b'\\' {
                    (bytes.get(index + 2).copied().unwrap_or(b'\\'), 2)
                } else {
                    (next, 1)
                };
                self.saw_member = true;
                self.previous = None;
                (
                    (index + 1 + endpoint_len).min(bytes.len()),
                    RawClassToken::Range { lower, upper },
                )
            }
            b'[' if bytes.get(index + 1) == Some(&b':') => {
                let name_start = index + 2;
                if let Some(rbracket) = next_closing_bracket(bytes, name_start, next_rbracket)
                    && rbracket > name_start
                    && bytes[rbracket - 1] == b':'
                {
                    let name = &bytes[name_start..rbracket - 1];
                    self.saw_member = true;
                    self.previous = None;
                    (
                        rbracket + 1,
                        RawClassToken::PosixSpan {
                            bit: git_posix_class_bit(name),
                            is_space: name == b"space",
                        },
                    )
                } else {
                    self.saw_member = true;
                    self.previous = Some(b'[');
                    (index + 1, RawClassToken::Literal(b'['))
                }
            }
            b']' => {
                if self.saw_member {
                    self.in_class = false;
                    self.previous = None;
                    (index + 1, RawClassToken::Close)
                } else {
                    // The first `]` after `[` or `[!` is a literal member;
                    // only a later one closes the class.
                    self.saw_member = true;
                    self.previous = Some(b']');
                    (index + 1, RawClassToken::Literal(b']'))
                }
            }
            byte => {
                self.saw_member = true;
                self.previous = Some(byte);
                (index + 1, RawClassToken::Literal(byte))
            }
        }
    }
}

/// Marks the exact normalized byte coverage of raw range `[lower..=upper]` in
/// `covered_bytes`, returning `true` iff any sub-range adds a new byte.
fn mark_normalized_range(
    covered_bytes: &mut ByteSet,
    lower: u8,
    upper: u8,
    case_insensitive: bool,
) -> bool {
    if lower > upper {
        return false;
    }
    let mut mark_plain = |lo: u8, hi: u8| -> bool {
        match (lo, hi) {
            (b'/', b'/') => false,
            (b'/', hi) => covered_bytes.insert_range(b'0', hi),
            (lo, b'/') => covered_bytes.insert_range(lo, b'.'),
            (lo, hi) => covered_bytes.insert_range(lo, hi),
        }
    };
    if !case_insensitive {
        return mark_plain(lower, upper);
    }
    let mut fresh = false;
    if lower <= b'@' {
        fresh |= mark_plain(lower, upper.min(b'@'));
    }
    if lower <= b'Z' && upper >= b'A' {
        let lo = lower.max(b'A').to_ascii_lowercase();
        let hi = upper.min(b'Z').to_ascii_lowercase();
        fresh |= mark_plain(lo, hi);
    }
    if upper >= b'[' {
        fresh |= mark_plain(lower.max(b'['), upper);
    }
    fresh
}

fn component_cost(part: &[u8], case_insensitive: bool) -> usize {
    let mut has_wildcard_or_class = false;
    let mut saw_raw_slash = false;
    let mut star_runs = 0_usize;
    let mut effective_len = 0_usize;
    let mut class = RawClassState::default();
    let mut covered_bytes = ByteSet::default();
    let mut seen_posix = 0_u16;
    let mut prev_charged = false;
    let mut next_rbracket: Option<usize> = None;
    let mut index = 0;
    while index < part.len() {
        if class.in_class {
            let (next, token) = class.step_in_class(part, index, &mut next_rbracket);
            if !saw_raw_slash && part[index..next].contains(&b'/') {
                saw_raw_slash = true;
            }
            match token {
                RawClassToken::Literal(byte) => {
                    if is_matchable_class_literal(byte, case_insensitive)
                        && covered_bytes.insert(byte)
                    {
                        effective_len += next - index;
                        prev_charged = true;
                    } else {
                        prev_charged = false;
                    }
                }
                RawClassToken::Range { lower, upper } => {
                    if mark_normalized_range(&mut covered_bytes, lower, upper, case_insensitive) {
                        effective_len += (next - index) + usize::from(!prev_charged);
                    }
                    prev_charged = false;
                }
                RawClassToken::PosixSpan { bit, is_space } => {
                    if is_space {
                        for &space in b" \t\n\r" {
                            if covered_bytes.insert(space) {
                                effective_len += 1;
                            }
                        }
                    } else if let Some(posix_bit) = bit
                        && (seen_posix & posix_bit) == 0
                    {
                        seen_posix |= posix_bit;
                        effective_len += next - index;
                    }
                    prev_charged = false;
                }
                RawClassToken::Close => {
                    effective_len += next - index;
                    prev_charged = false;
                }
            }
            index = next;
            continue;
        }
        match part[index] {
            b'\\' if index + 1 < part.len() => {
                if part[index + 1] == b'/' {
                    saw_raw_slash = true;
                }
                effective_len += 2;
                index += 2;
            }
            b'/' => {
                saw_raw_slash = true;
                effective_len += 1;
                index += 1;
            }
            b'*' => {
                has_wildcard_or_class = true;
                star_runs += 1;
                effective_len += 1;
                while index < part.len() && part[index] == b'*' {
                    index += 1;
                }
            }
            b'?' => {
                has_wildcard_or_class = true;
                effective_len += 1;
                index += 1;
            }
            b'[' => {
                has_wildcard_or_class = true;
                covered_bytes = ByteSet::default();
                seen_posix = 0;
                prev_charged = false;
                let next = class.open(part, index);
                effective_len += next - index;
                index = next;
            }
            _ => {
                effective_len += 1;
                index += 1;
            }
        }
    }
    if !has_wildcard_or_class {
        LITERAL_COMPONENT_FIXED_BYTES
            .saturating_add(effective_len.saturating_mul(LITERAL_BYTES_PER_BYTE))
    } else {
        let base = if star_runs >= 2 {
            PATTERN_COMPONENT_FIXED_BYTES + WIDE_SWEEP_EXTRA_BYTES
        } else {
            PATTERN_COMPONENT_FIXED_BYTES
        };
        let bytes = base.saturating_add(effective_len.saturating_mul(PATTERN_BYTES_PER_BYTE));
        if saw_raw_slash {
            bytes.saturating_mul(2)
        } else {
            bytes
        }
    }
}

const MAX_POSIX_CLASS_NAME_LEN: usize = 6;

/// The twelve POSIX character classes supported by Git's `wildmatch.c`,
/// mapped to a unique bit for deduplication. Non-Git extensions such as
/// `ascii` and `word` are deliberately excluded so ignore rules match Git's
/// `WM_ABORT_ALL` rejection of unknown classes.
fn git_posix_class_bit(name: &[u8]) -> Option<u16> {
    let idx = match name {
        b"alnum" => 0,
        b"alpha" => 1,
        b"blank" => 2,
        b"cntrl" => 3,
        b"digit" => 4,
        b"graph" => 5,
        b"lower" => 6,
        b"print" => 7,
        b"punct" => 8,
        b"space" => 9,
        b"upper" => 10,
        b"xdigit" => 11,
        _ => return None,
    };
    Some(1_u16 << idx)
}

fn is_git_posix_class_name(name: &[u8]) -> bool {
    git_posix_class_bit(name).is_some()
}

/// Zero-allocation syntax validator matching `ferralk-glob::Pattern::compile`
/// under `component_options` (`braces: false`, `extglob: false`,
/// `recursive_double_star: false`, `escape: true`).
///
/// Under those options the only syntax errors `ferralk-glob` can report are
/// empty, trailing-escape, or unclosed bracket expressions (`parse_class`).
fn is_valid_ignore_component_syntax(pattern: &[u8]) -> bool {
    let mut index = 0;
    while index < pattern.len() {
        match pattern[index] {
            b'\\' => {
                index += usize::from(index + 1 < pattern.len()) + 1;
            }
            b'[' => {
                let Some(next) = validate_glob_class(pattern, index) else {
                    return false;
                };
                index = next;
            }
            _ => index += 1,
        }
    }
    true
}

/// Mirrors `ferralk-glob::parse_class(pattern, start, true)` without allocating
/// `Class` or `ClassMember` vectors.
fn validate_glob_class(pattern: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    if pattern
        .get(index)
        .is_some_and(|byte| matches!(byte, b'!' | b'^'))
    {
        index += 1;
    }
    let mut has_member = false;
    if pattern.get(index) == Some(&b']') {
        has_member = true;
        index += 1;
    }
    while let Some(&byte) = pattern.get(index) {
        if byte == b']' {
            return has_member.then_some(index + 1);
        }
        if byte == b'[' && pattern.get(index + 1) == Some(&b':') {
            let name_start = index + 2;
            let search_end = pattern.len().min(name_start + MAX_POSIX_CLASS_NAME_LEN + 2);
            if let Some(posix_end) = memmem::find(&pattern[name_start..search_end], b":]")
                && is_git_posix_class_name(&pattern[name_start..name_start + posix_end])
            {
                has_member = true;
                index = name_start + posix_end + 2;
                continue;
            }
        }
        if byte == b'\\' {
            pattern.get(index + 1)?;
            has_member = true;
            index += 2;
        } else {
            has_member = true;
            index += 1;
        }
    }
    None
}

/// Streams through `body`'s components using a single reusable scratch buffer,
/// checking only components that contain `[` for bracket-class syntax validity
/// without ever invoking `Pattern::compile`.
///
/// Returns `true` when every component is syntactically valid (so the caller
/// should report budget overflow), or `false` when any component is empty or
/// malformed (so the caller should discard the rule cleanly).
fn validate_rule_syntax_streaming(body: &[u8], case_insensitive: bool) -> bool {
    let mut scratch = Vec::new();
    let mut valid = true;
    let scan_result = scan_rule_components(body, usize::MAX, |part| {
        if !valid || !part.bytes.contains(&b'[') {
            return;
        }
        scratch.clear();
        if !normalize_bracket_classes_into(part.bytes, case_insensitive, &mut scratch) {
            valid = false;
            return;
        }
        if !(all_stars(&scratch) && scratch.len() >= 2)
            && !is_valid_ignore_component_syntax(&scratch)
        {
            valid = false;
        }
    });
    matches!(scan_result, Ok((true, _))) && valid
}

/// Mirrors `ferralk-glob`'s `IrBudget` constants under `component_options`
/// (`braces: false`, `extglob: false`, `recursive_double_star: false`,
/// `escape: true`, `match_hidden: true`) so large over-budget components can be
/// checked against `IrBudget` without materializing a `Pattern`.
const GLOB_MAX_COMPILED_IR_UNITS: usize = 1 << 20;
const GLOB_TOKEN_SIZE: usize = std::mem::size_of::<usize>() * 4;
const GLOB_NARROW_IR_UNITS: usize = 2_128_usize.div_ceil(GLOB_TOKEN_SIZE);
const GLOB_MAX_NARROW_POSITIONS: usize = 63;
const GLOB_WIDE_WORD_ROWS_AT_COMPILE: usize = 256 + 9 + 3;

fn glob_sweep_ir_units(position_count: usize) -> Option<usize> {
    if position_count > GLOB_MAX_NARROW_POSITIONS {
        let word_count = position_count.checked_add(1)?.div_ceil(u64::BITS as usize);
        let peak_words = word_count.checked_mul(GLOB_WIDE_WORD_ROWS_AT_COMPILE)?;
        let peak_bytes = peak_words.checked_mul(std::mem::size_of::<u64>())?;
        Some(peak_bytes.div_ceil(GLOB_TOKEN_SIZE))
    } else {
        Some(GLOB_NARROW_IR_UNITS)
    }
}

/// Zero-allocation parser mirroring `ferralk-glob::parse_class` + `class_members`
/// to count exact `ClassMember` IR units and locate the closing `]`.
fn parse_class_ir_members(pattern: &[u8], start: usize) -> Option<(usize, usize)> {
    #[derive(Clone, Copy)]
    enum Pending {
        Empty,
        One,
        OneThenDash,
    }
    let push_value =
        |pending: &mut Pending, members: &mut usize, is_unescaped_dash: bool| match *pending {
            Pending::Empty => *pending = Pending::One,
            Pending::One => {
                if is_unescaped_dash {
                    *pending = Pending::OneThenDash;
                } else {
                    *members += 1;
                }
            }
            Pending::OneThenDash => {
                *members += 1;
                *pending = Pending::Empty;
            }
        };
    let flush = |pending: &mut Pending, members: &mut usize| {
        match *pending {
            Pending::Empty => {}
            Pending::One => *members += 1,
            Pending::OneThenDash => *members += 2,
        }
        *pending = Pending::Empty;
    };

    let mut index = start + 1;
    if pattern
        .get(index)
        .is_some_and(|byte| matches!(byte, b'!' | b'^'))
    {
        index += 1;
    }
    let mut pending = Pending::Empty;
    let mut members = 0_usize;
    let mut has_any = false;
    if pattern.get(index) == Some(&b']') {
        has_any = true;
        push_value(&mut pending, &mut members, false);
        index += 1;
    }
    while let Some(&byte) = pattern.get(index) {
        if byte == b']' {
            if !has_any {
                return None;
            }
            flush(&mut pending, &mut members);
            return Some((members, index + 1));
        }
        if byte == b'[' && pattern.get(index + 1) == Some(&b':') {
            let name_start = index + 2;
            let search_end = pattern.len().min(name_start + MAX_POSIX_CLASS_NAME_LEN + 2);
            if let Some(posix_end) = memmem::find(&pattern[name_start..search_end], b":]")
                && is_git_posix_class_name(&pattern[name_start..name_start + posix_end])
            {
                has_any = true;
                flush(&mut pending, &mut members);
                members += 1;
                index = name_start + posix_end + 2;
                continue;
            }
        }
        if byte == b'\\' {
            pattern.get(index + 1)?;
            has_any = true;
            push_value(&mut pending, &mut members, false);
            index += 2;
        } else {
            has_any = true;
            push_value(&mut pending, &mut members, byte == b'-');
            index += 1;
        }
    }
    None
}

/// Zero-allocation exact check for whether `Pattern::compile` (under
/// `component_options`) would reject `pattern` for syntax or `IrBudget`,
/// assuming consecutive unescaped `*` runs have already been collapsed (or
/// collapsing them on the fly outside bracket classes).
fn component_exceeds_glob_ir_budget(pattern: &[u8]) -> bool {
    let mut token_count = 0_usize;
    let mut class_member_units = 0_usize;
    let mut position_count = 0_usize;
    let mut has_star = false;
    let mut prev_was_separator = false;
    let mut needs_path_filter_copy = pattern.starts_with(b"./");
    let mut first_token_is_single_dot = false;
    let mut second_token_is_separator = false;
    let mut in_literal_run = false;
    let mut literal_run_len = 0_usize;
    let mut first_literal_byte = 0_u8;

    let flush_literal = |in_literal_run: &mut bool,
                         literal_run_len: &mut usize,
                         first_literal_byte: u8,
                         token_count: &mut usize,
                         first_token_is_single_dot: &mut bool,
                         prev_was_separator: &mut bool| {
        if *in_literal_run {
            *token_count += 1;
            if *token_count == 1 && *literal_run_len == 1 && first_literal_byte == b'.' {
                *first_token_is_single_dot = true;
            }
            *in_literal_run = false;
            *literal_run_len = 0;
            *prev_was_separator = false;
        }
    };

    let mut index = 0;
    while index < pattern.len() {
        match pattern[index] {
            b'/' => {
                flush_literal(
                    &mut in_literal_run,
                    &mut literal_run_len,
                    first_literal_byte,
                    &mut token_count,
                    &mut first_token_is_single_dot,
                    &mut prev_was_separator,
                );
                token_count += 1;
                position_count += 1;
                if token_count == 2 && first_token_is_single_dot {
                    second_token_is_separator = true;
                }
                prev_was_separator = true;
                index += 1;
            }
            b'*' => {
                flush_literal(
                    &mut in_literal_run,
                    &mut literal_run_len,
                    first_literal_byte,
                    &mut token_count,
                    &mut first_token_is_single_dot,
                    &mut prev_was_separator,
                );
                if prev_was_separator {
                    needs_path_filter_copy = true;
                }
                token_count += 1;
                position_count += 1;
                has_star = true;
                prev_was_separator = false;
                while index < pattern.len() && pattern[index] == b'*' {
                    index += 1;
                }
            }
            b'?' => {
                flush_literal(
                    &mut in_literal_run,
                    &mut literal_run_len,
                    first_literal_byte,
                    &mut token_count,
                    &mut first_token_is_single_dot,
                    &mut prev_was_separator,
                );
                if prev_was_separator {
                    needs_path_filter_copy = true;
                }
                token_count += 1;
                position_count += 1;
                prev_was_separator = false;
                index += 1;
            }
            b'[' => {
                flush_literal(
                    &mut in_literal_run,
                    &mut literal_run_len,
                    first_literal_byte,
                    &mut token_count,
                    &mut first_token_is_single_dot,
                    &mut prev_was_separator,
                );
                let Some((members, next)) = parse_class_ir_members(pattern, index) else {
                    return true;
                };
                if prev_was_separator {
                    needs_path_filter_copy = true;
                }
                class_member_units = class_member_units.saturating_add(members);
                token_count += 1;
                position_count += 1;
                prev_was_separator = false;
                index = next;
            }
            b'\\' => {
                let byte = if let Some(&escaped) = pattern.get(index + 1) {
                    index += 2;
                    escaped
                } else {
                    index += 1;
                    b'\\'
                };
                if !in_literal_run {
                    in_literal_run = true;
                    literal_run_len = 0;
                    first_literal_byte = byte;
                }
                literal_run_len += 1;
                position_count += 1;
            }
            byte => {
                if !in_literal_run {
                    in_literal_run = true;
                    literal_run_len = 0;
                    first_literal_byte = byte;
                }
                literal_run_len += 1;
                position_count += 1;
                index += 1;
            }
        }
    }
    flush_literal(
        &mut in_literal_run,
        &mut literal_run_len,
        first_literal_byte,
        &mut token_count,
        &mut first_token_is_single_dot,
        &mut prev_was_separator,
    );

    let mut total_ir = token_count.saturating_add(class_member_units);
    if total_ir > GLOB_MAX_COMPILED_IR_UNITS {
        return true;
    }
    if has_star {
        let Some(sweep) = glob_sweep_ir_units(position_count) else {
            return true;
        };
        total_ir = total_ir.saturating_add(sweep);
        if total_ir > GLOB_MAX_COMPILED_IR_UNITS {
            return true;
        }
    }
    let leading_dot_slash =
        pattern.starts_with(b"./") && first_token_is_single_dot && second_token_is_separator;
    if needs_path_filter_copy {
        let copy_tokens = if leading_dot_slash {
            token_count - 2
        } else {
            token_count
        };
        let copy_positions = if leading_dot_slash {
            position_count - 2
        } else {
            position_count
        };
        total_ir = total_ir.saturating_add(copy_tokens);
        if has_star {
            let Some(sweep) = glob_sweep_ir_units(copy_positions) else {
                return true;
            };
            total_ir = total_ir.saturating_add(sweep);
        }
        if total_ir > GLOB_MAX_COMPILED_IR_UNITS {
            return true;
        }
    }
    false
}

fn collapse_partial_stars_in_place(bytes: &mut Vec<u8>) {
    let mut read = 0;
    let mut write = 0;
    while read < bytes.len() {
        if bytes[read] == b'\\' && read + 1 < bytes.len() {
            bytes[write] = bytes[read];
            bytes[write + 1] = bytes[read + 1];
            read += 2;
            write += 2;
        } else if bytes[read] == b'[' {
            // Preserve literal `*` bytes inside bracket classes verbatim so
            // adjacent `*` range endpoints (e.g. `[#-**-0]`) are never merged.
            let end = validate_glob_class(bytes, read).unwrap_or(bytes.len());
            if write != read {
                bytes.copy_within(read..end, write);
            }
            write += end - read;
            read = end;
        } else if bytes[read] == b'*' {
            bytes[write] = b'*';
            write += 1;
            while read < bytes.len() && bytes[read] == b'*' {
                read += 1;
            }
        } else {
            bytes[write] = bytes[read];
            read += 1;
            write += 1;
        }
    }
    bytes.truncate(write);
}

/// Streams through `body`'s components using a zero-materialization `IrBudget`
/// check on components large enough to possibly trip `ferralk-glob`'s
/// per-pattern `IrBudget`, allocating a single reusable normalization buffer
/// only when a large component contains `[`.
///
/// Returns `true` when every large component fits `IrBudget` (so the caller
/// should report the budget overflow), or `false` when any component would be
/// rejected by `Pattern::compile` (so the caller should discard just this rule
/// cleanly without rolling back the file).
fn validate_large_components_ir_streaming(body: &[u8], case_insensitive: bool) -> bool {
    let mut scratch = Vec::new();
    let mut valid = true;
    let scan_result = scan_rule_components(body, usize::MAX, |part| {
        if !valid
            || part.bytes.len() < MIN_IR_LIMIT_COMPONENT_BYTES
            || (all_stars(part.bytes) && part.bytes.len() >= 2)
        {
            return;
        }
        let target: &[u8] = if part.bytes.contains(&b'[') {
            scratch.clear();
            if !normalize_bracket_classes_into(part.bytes, case_insensitive, &mut scratch) {
                valid = false;
                return;
            }
            collapse_partial_stars_in_place(&mut scratch);
            &scratch
        } else {
            part.bytes
        };
        if component_exceeds_glob_ir_budget(target) {
            valid = false;
        }
    });
    matches!(scan_result, Ok((true, _))) && valid
}

struct RulePart<'a> {
    bytes: &'a [u8],
    escaped_separator_after: bool,
}

struct NormalizedPart {
    bytes: Vec<u8>,
    escaped_separator_after: bool,
}

#[cfg(test)]
std::thread_local! {
    static POSIX_CLOSER_SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static POSIX_CLOSER_BYTES_SCANNED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PATTERN_COMPILES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PART_VEC_ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static PRECOMPOSE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static PRECOMPOSE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static PRECOMPOSE_SUFFIX_MOVE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static NFC_ASCII_SCAN_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static NFC_OFFSET_MAP_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static CASE_FOLD_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static CASE_FOLD_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static FOLD_SUFFIX_MOVE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FULL_COMPONENT_MATCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Finds the first `]` at or after `from` in `haystack`, caching the last
/// located position (or `usize::MAX` when no `]` remains) so repeated `[:`
/// openers inside a pattern scan every byte of `haystack` at most once.
fn next_closing_bracket(haystack: &[u8], from: usize, cached: &mut Option<usize>) -> Option<usize> {
    if let Some(pos) = *cached {
        if pos == usize::MAX {
            return None;
        }
        if pos >= from {
            return Some(pos);
        }
    }
    #[cfg(test)]
    POSIX_CLOSER_SEARCHES.with(|count| count.set(count.get() + 1));
    match memchr::memchr(b']', &haystack[from..]) {
        Some(rel) => {
            #[cfg(test)]
            POSIX_CLOSER_BYTES_SCANNED.with(|bytes| bytes.set(bytes.get() + rel + 1));
            let pos = from + rel;
            *cached = Some(pos);
            Some(pos)
        }
        None => {
            #[cfg(test)]
            POSIX_CLOSER_BYTES_SCANNED
                .with(|bytes| bytes.set(bytes.get() + haystack.len().saturating_sub(from)));
            *cached = Some(usize::MAX);
            None
        }
    }
}

/// Splits a rule on path separators, keeping a slash inside a bracket class in
/// its component. Escaped brackets do not open a class, matching the glob
/// parser's byte-oriented escape handling. The separator spelling is retained
/// because Git does not let an escaped slash activate `**`'s zero-width case.
///
/// Returns `Ok((None, scanned_parts))` immediately when any component is empty,
/// and `Err(())` as soon as the non-empty component count would exceed
/// `max_parts`.
fn split_rule_components_bounded(
    body: &[u8],
    max_parts: usize,
) -> Result<(Option<Vec<RulePart<'_>>>, usize), ()> {
    #[cfg(test)]
    PART_VEC_ALLOCS.with(|count| count.set(count.get() + 1));
    let mut parts = Vec::new();
    let (valid, scanned_parts) = scan_rule_components(body, max_parts, |part| parts.push(part))?;
    if !valid {
        return Ok((None, scanned_parts));
    }
    Ok((Some(parts), scanned_parts))
}

#[cfg(test)]
fn split_rule_components(body: &[u8]) -> Vec<RulePart<'_>> {
    split_rule_components_bounded(body, usize::MAX)
        .expect("unbounded test split")
        .0
        .unwrap_or_default()
}

/// Scans `body` into non-empty path components without requiring allocation.
///
/// Returns `Ok((false, scanned_parts))` when any component is empty (which
/// makes the rule unmatchable in Git), or `Err(())` as soon as more than
/// `max_parts` non-empty components are encountered.
fn scan_rule_components<'a>(
    body: &'a [u8],
    max_parts: usize,
    mut on_part: impl FnMut(RulePart<'a>),
) -> Result<(bool, usize), ()> {
    let mut count = 0;
    let mut start = 0;
    let mut class = RawClassState::default();
    let mut next_rbracket: Option<usize> = None;
    let mut index = 0;
    while index < body.len() {
        if class.in_class {
            index = class.step_in_class(body, index, &mut next_rbracket).0;
            continue;
        }
        match body[index] {
            b'\\' if body.get(index + 1) == Some(&b'/') => {
                // Git's wildmatch treats an escaped slash as the same path
                // separator. The escape is inert here and must not remain in
                // either component handed to the glob compiler.
                let slice = &body[start..index];
                if slice.is_empty() {
                    return Ok((false, count));
                }
                if count >= max_parts {
                    return Err(());
                }
                count += 1;
                on_part(RulePart {
                    bytes: slice,
                    escaped_separator_after: true,
                });
                start = index + 2;
                index += 2;
            }
            b'\\' => {
                index += 2;
            }
            b'[' => {
                index = class.open(body, index);
            }
            b'/' => {
                let slice = &body[start..index];
                if slice.is_empty() {
                    return Ok((false, count));
                }
                if count >= max_parts {
                    return Err(());
                }
                count += 1;
                on_part(RulePart {
                    bytes: slice,
                    escaped_separator_after: false,
                });
                start = index + 1;
                index += 1;
            }
            _ => {
                index += 1;
            }
        }
    }
    let slice = &body[start..];
    if slice.is_empty() {
        return Ok((false, count));
    }
    if count >= max_parts {
        return Err(());
    }
    count += 1;
    on_part(RulePart {
        bytes: slice,
        escaped_separator_after: false,
    });
    Ok((true, count))
}

fn compile_component(
    part: &[u8],
    case_insensitive: bool,
    escaped_separator_after: bool,
) -> Option<Component> {
    if all_stars(part) && part.len() >= 2 {
        return Some(if escaped_separator_after {
            Component::AnyDirsNonEmpty
        } else {
            Component::AnyDirs
        });
    }
    if part == b"*" {
        return Some(Component::AnyOne);
    }
    let collapsed = collapse_partial_stars(part);
    #[cfg(test)]
    PATTERN_COMPILES.with(|count| count.set(count.get() + 1));
    Pattern::compile(&collapsed, component_options(case_insensitive))
        .ok()
        .map(Component::Pattern)
}

fn all_stars(part: &[u8]) -> bool {
    !part.is_empty() && part.iter().all(|byte| *byte == b'*')
}

/// Rewrites each bracket class of one component the way Git's wildmatch reads
/// it, in a spelling ferralk-glob compiles to the same members.
///
/// Git walks a class with one piece of state: the previous literal byte. A dash
/// is the range operator only after such a byte and before something other
/// than the closing bracket; a completed range or POSIX class clears that
/// state, so the dash after either is literal. Git also tests a range's lower
/// endpoint as a literal before it compares the range, so a reversed range
/// still admits that byte. Every literal dash is emitted escaped, which keeps
/// the compiler's grouping identical to Git's even next to a dropped member.
///
/// Candidate components never contain a path separator, so a slash inside a
/// class is a dead member. ferralk-glob rejects it as a separator, so it is
/// dropped: as the upper endpoint of an ascending range its reachable
/// neighbour `.` stands in, as the lower endpoint `0` does.
///
/// A class left without a matchable member matches nothing, or any byte when
/// negated. An unclosed class or an unknown/non-Git POSIX class name matches
/// nothing, as in Git (`WM_ABORT_ALL`).
fn normalize_bracket_classes(part: &[u8], case_insensitive: bool) -> Option<Vec<u8>> {
    // Escaping literal dashes and endpoints grows a small class by at most one
    // byte per member, while huge repeated-member classes compact to O(1)
    // bytes; capping the initial reservation avoids multi-MiB transient buffers
    // on compacted classes.
    let mut result = Vec::with_capacity(part.len().saturating_mul(2).min(64 * 1024));
    normalize_bracket_classes_into(part, case_insensitive, &mut result).then_some(result)
}

fn normalize_bracket_classes_into(
    part: &[u8],
    case_insensitive: bool,
    result: &mut Vec<u8>,
) -> bool {
    // Shared across all classes in `part`: caches the next `]` position so
    // repeated `[:` openers scan every byte of `part` at most once.
    let mut next_rbracket: Option<usize> = None;
    let mut index = 0;
    while index < part.len() {
        match part[index] {
            b'\\' if index + 1 < part.len() => {
                result.extend_from_slice(&part[index..=index + 1]);
                index += 2;
            }
            b'[' => {
                let Some(next) = normalize_bracket_class(
                    part,
                    index,
                    case_insensitive,
                    result,
                    &mut next_rbracket,
                ) else {
                    return false;
                };
                index = next;
            }
            byte => {
                result.push(byte);
                index += 1;
            }
        }
    }
    true
}

/// Rewrites the class opening at `start` into `result` and returns the index
/// after its closing bracket.
fn normalize_bracket_class(
    part: &[u8],
    start: usize,
    case_insensitive: bool,
    result: &mut Vec<u8>,
    next_rbracket: &mut Option<usize>,
) -> Option<usize> {
    let class_start = result.len();
    result.push(b'[');
    let mut index = start + 1;
    let negated = matches!(part.get(index), Some(b'!' | b'^'));
    if negated {
        result.push(part[index]);
        index += 1;
    }
    // Git's `prev_ch`: the raw literal byte a range operator may follow, plus
    // whether that byte was actually emitted at the tail of `result` (false
    // when dropped as `/`, dropped as uppercase under `WM_CASEFOLD`, or
    // deduplicated as an already-emitted standalone literal).
    let mut previous: Option<u8> = None;
    let mut prev_pushed = false;
    // Bytes already covered by emitted literals, ranges, or `[:space:]` in this
    // class, plus a 12-bit mask of emitted POSIX classes, so long runs of
    // duplicate literals/ranges/POSIX classes compact strictly to O(1) members
    // without disturbing range endpoints or changing match semantics.
    let mut covered_bytes = ByteSet::default();
    let mut seen_posix = 0_u16;
    // Git closes the class at a `]` only after one member, so a leading `]`
    // is a literal.
    let mut saw_member = false;
    // Whether the compiler will find a member. Dropped slashes, uppercase
    // standalone literals under `WM_CASEFOLD`, and reversed ranges contribute
    // none.
    let mut has_member = false;
    while index < part.len() {
        let byte = part[index];
        if byte == b']' && saw_member {
            if has_member {
                result.push(b']');
            } else if negated {
                result.truncate(class_start);
                result.push(b'?');
            } else {
                return None;
            }
            return Some(index + 1);
        }
        saw_member = true;

        if byte == b'-'
            && let Some(lower) = previous
            && let Some(&next) = part.get(index + 1)
            && next != b']'
        {
            let (upper, endpoint_len) = if next == b'\\' {
                (*part.get(index + 2)?, 2)
            } else {
                (next, 1)
            };
            // Git compares raw byte endpoints (`prev_ch <= upper`): a reversed
            // range contributes nothing beyond whatever `lower` already matched
            // as a literal. Under `WM_CASEFOLD`, Git tests both lowercased
            // `t_ch` and `toupper(t_ch)` against the raw range `[lower..=upper]`;
            // splitting around `b'A'..=b'Z'` lets `ferralk-glob`'s folded range
            // matcher reproduce that exact set.
            if lower <= upper
                && push_normalized_range(
                    result,
                    lower,
                    upper,
                    case_insensitive,
                    prev_pushed,
                    &mut covered_bytes,
                )
            {
                has_member = true;
            }
            previous = None;
            prev_pushed = false;
            index += 1 + endpoint_len;
            continue;
        }

        match byte {
            b'\\' => {
                let escaped = *part.get(index + 1)?;
                prev_pushed =
                    push_class_literal(result, escaped, case_insensitive, true, &mut covered_bytes);
                if prev_pushed {
                    has_member = true;
                }
                previous = Some(escaped);
                index += 2;
            }
            b'[' if part.get(index + 1) == Some(&b':') => {
                let name_start = index + 2;
                let rbracket = next_closing_bracket(part, name_start, next_rbracket)?;
                if rbracket > name_start && part[rbracket - 1] == b':' {
                    let name = &part[name_start..rbracket - 1];
                    let Some(posix_bit) = git_posix_class_bit(name) else {
                        // Git's `wildmatch.c` returns `WM_ABORT_ALL` on any
                        // unknown or non-Git POSIX class name (such as
                        // `[:bogus:]`, `[:ascii:]`, `[:word:]`, `[::]`, or
                        // `[:a/b:]`), aborting the whole pattern match even
                        // when negated or combined with valid members.
                        return None;
                    };
                    if name == b"space" {
                        // Git's `sane-ctype.h` `isspace` matches only `' '`,
                        // `'\t'`, `'\n'`, and `'\r'`, excluding `\x0B` (VT) and
                        // `\x0C` (FF) that general POSIX `[:space:]` admits.
                        for &space in b" \t\n\r" {
                            if covered_bytes.insert(space) {
                                result.push(space);
                            }
                        }
                    } else if (seen_posix & posix_bit) == 0 {
                        seen_posix |= posix_bit;
                        result.extend_from_slice(&part[index..=rbracket]);
                    }
                    previous = None;
                    prev_pushed = false;
                    has_member = true;
                    index = rbracket + 1;
                } else {
                    // No `:]` before the first `]`: Git falls back to treating
                    // `[` as a literal member and resumes at `:`. Emit it
                    // escaped so downstream `ferralk-glob` never confuses it
                    // with a POSIX class opener.
                    if covered_bytes.insert(b'[') {
                        result.extend_from_slice(b"\\[");
                        has_member = true;
                        prev_pushed = true;
                    } else {
                        prev_pushed = false;
                    }
                    previous = Some(b'[');
                    index += 1;
                }
            }
            byte => {
                prev_pushed =
                    push_class_literal(result, byte, case_insensitive, false, &mut covered_bytes);
                if prev_pushed {
                    has_member = true;
                }
                previous = Some(byte);
                index += 1;
            }
        }
    }
    None
}

/// Whether a standalone literal byte inside `[...]` can ever match in Git's
/// `wildmatch.c`. Candidate components never contain `/`, and under
/// `WM_CASEFOLD` Git lowercases `t_ch` before testing `t_ch == p_ch`, so a
/// standalone uppercase ASCII byte can never match directly (though it can
/// still open an ascending range whose upper check tests `toupper(t_ch)`).
fn is_matchable_class_literal(byte: u8, case_insensitive: bool) -> bool {
    byte != b'/' && !(case_insensitive && byte.is_ascii_uppercase())
}

fn push_class_literal(
    result: &mut Vec<u8>,
    byte: u8,
    case_insensitive: bool,
    was_escaped: bool,
    covered_bytes: &mut ByteSet,
) -> bool {
    if !is_matchable_class_literal(byte, case_insensitive) || !covered_bytes.insert(byte) {
        return false;
    }
    // A literal dash must not group a range, and dropping `/` or an uppercase
    // literal must never promote `!`/`^` into first place or fuse `[` with a
    // following `:...:]` into a synthetic POSIX class (e.g. `[[A:digit:]]` or
    // `[[/:digit:]]`).
    if was_escaped || matches!(byte, b'\\' | b'-' | b'[' | b'!' | b'^') {
        result.push(b'\\');
        result.push(byte);
    } else {
        result.push(byte);
    }
    true
}

/// Emits one ascending raw range `[lower..=upper]` in a form `ferralk-glob`
/// evaluates identically to Git's `wildmatch.c`.
fn push_normalized_range(
    result: &mut Vec<u8>,
    lower: u8,
    upper: u8,
    case_insensitive: bool,
    lower_already_pushed: bool,
    covered_bytes: &mut ByteSet,
) -> bool {
    if !case_insensitive {
        return push_plain_range(result, lower, upper, lower_already_pushed, covered_bytes);
    }
    // Under `WM_CASEFOLD`, Git matches candidate `c` iff `fold(c)` is in
    // `[lower..=upper]` OR `fold(c)` is ASCII lowercase and
    // `fold(c).to_ascii_uppercase()` is in `[lower..=upper]`. Partition
    // `[lower..=upper]` around `b'A'..=b'Z'` so every emitted sub-range has
    // fold-invariant endpoints in `ferralk-glob`'s `case_insensitive(true)`.
    let mut added = false;
    if lower <= b'@' {
        let hi = upper.min(b'@');
        if push_plain_range(result, lower, hi, lower_already_pushed, covered_bytes) {
            added = true;
        }
    }
    if lower <= b'Z' && upper >= b'A' {
        let lo = lower.max(b'A').to_ascii_lowercase();
        let hi = upper.min(b'Z').to_ascii_lowercase();
        if push_plain_range(result, lo, hi, false, covered_bytes) {
            added = true;
        }
    }
    if upper >= b'[' {
        let lo = lower.max(b'[');
        if push_plain_range(
            result,
            lo,
            upper,
            lower >= b'[' && lower_already_pushed,
            covered_bytes,
        ) {
            added = true;
        }
    }
    added
}

fn push_plain_range(
    result: &mut Vec<u8>,
    lower: u8,
    upper: u8,
    lower_already_pushed: bool,
    covered_bytes: &mut ByteSet,
) -> bool {
    let (lo, hi, lo_pushed) = match (lower, upper) {
        (b'/', b'/') => return false,
        (b'/', hi) => (b'0', hi, false),
        (lo, b'/') => (lo, b'.', lower_already_pushed),
        (lo, hi) => (lo, hi, lower_already_pushed),
    };
    if !covered_bytes.insert_range(lo, hi) {
        return false;
    }
    if lo == hi {
        if !lo_pushed {
            push_range_endpoint(result, lo);
        }
        return true;
    }
    if !lo_pushed {
        push_range_endpoint(result, lo);
    }
    result.push(b'-');
    push_range_endpoint(result, hi);
    true
}

/// Writes a range endpoint so the compiler cannot read it as syntax.
fn push_range_endpoint(result: &mut Vec<u8>, byte: u8) {
    if matches!(byte, b'\\' | b'-' | b'[' | b']' | b'!' | b'^') {
        result.push(b'\\');
    }
    result.push(byte);
}

/// Adapts the candidate path that ignore files see, not their raw patterns.
/// Mac Git precomposes filesystem names this way before matching; each path
/// component is independent so an invalid UTF-8 component cannot make a valid
/// sibling component lose its NFC conversion.
///
/// Returns `false` when `candidate` is already unchanged under component-wise
/// NFC (including all pure-ASCII inputs, with zero UTF-8 or NFC table work),
/// and `true` after writing the precomposed bytes into `out`.
#[cfg(target_os = "macos")]
fn precompose_into(candidate: &[u8], out: &mut Vec<u8>) -> bool {
    use unicode_normalization::{UnicodeNormalization, is_nfc};

    if candidate.is_ascii() {
        return false;
    }
    #[cfg(test)]
    {
        PRECOMPOSE_CALLS.with(|count| count.set(count.get() + 1));
        PRECOMPOSE_BYTES.with(|count| count.set(count.get() + candidate.len()));
    }

    let already_nfc = candidate.split(|&byte| byte == b'/').all(|component| {
        component.is_ascii() || std::str::from_utf8(component).map_or(true, is_nfc)
    });
    if already_nfc {
        return false;
    }

    out.clear();
    out.reserve(candidate.len());
    for (index, component) in candidate.split(|&byte| byte == b'/').enumerate() {
        if index != 0 {
            out.push(b'/');
        }
        if component.is_ascii() {
            out.extend_from_slice(component);
            continue;
        }
        match std::str::from_utf8(component) {
            Ok(component) => {
                for character in component.nfc() {
                    let mut encoded = [0; 4];
                    out.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
                }
            }
            Err(_) => out.extend_from_slice(component),
        }
    }
    true
}

/// Appends the component-wise NFC form of `prefix` to `out` (used in Stage 2
/// before `rotate_right` so an already-precomposed leaf suffix is never
/// re-normalized).
#[cfg(target_os = "macos")]
fn precompose_append(prefix: &[u8], out: &mut Vec<u8>) {
    use unicode_normalization::{UnicodeNormalization, is_nfc};

    if prefix.is_ascii() {
        out.extend_from_slice(prefix);
        return;
    }
    #[cfg(test)]
    {
        PRECOMPOSE_CALLS.with(|count| count.set(count.get() + 1));
        PRECOMPOSE_BYTES.with(|count| count.set(count.get() + prefix.len()));
    }

    let already_nfc = prefix.split(|&byte| byte == b'/').all(|component| {
        component.is_ascii() || std::str::from_utf8(component).map_or(true, is_nfc)
    });
    if already_nfc {
        out.extend_from_slice(prefix);
        return;
    }

    out.reserve(prefix.len());
    for (index, component) in prefix.split(|&byte| byte == b'/').enumerate() {
        if index != 0 {
            out.push(b'/');
        }
        if component.is_ascii() {
            out.extend_from_slice(component);
            continue;
        }
        match std::str::from_utf8(component) {
            Ok(component) => {
                for character in component.nfc() {
                    let mut encoded = [0; 4];
                    out.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
                }
            }
            Err(_) => out.extend_from_slice(component),
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn precompose_into(_candidate: &[u8], _out: &mut Vec<u8>) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
fn precompose_append(_prefix: &[u8], _out: &mut Vec<u8>) {}

/// Drops the trailing spaces Git drops: those that are not escaped.
fn strip_trailing_spaces(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && line[end - 1] == b' ' {
        if trailing_backslashes(&line[..end - 1]) % 2 == 1 {
            break;
        }
        end -= 1;
    }
    &line[..end]
}

fn trailing_backslashes(text: &[u8]) -> usize {
    text.iter().rev().take_while(|byte| **byte == b'\\').count()
}

/// The longest run of bytes one component matches literally.
///
/// Only runs outside a bracket expression count, and an escaped metacharacter
/// counts as the character it stands for, so the run is something the candidate
/// has to contain verbatim. Runs shorter than two bytes are not worth a search.
fn longest_literal_run(part: &[u8]) -> Option<Vec<u8>> {
    let mut longest: Vec<u8> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    // Shared across all classes in `part`: caches the next `]` position so
    // repeated `[:` openers stay linear without rescanning.
    let mut next_rbracket: Option<usize> = None;
    let mut index = 0;
    while index < part.len() {
        match part[index] {
            b'\\' if index + 1 < part.len() => {
                current.push(part[index + 1]);
                index += 2;
            }
            b'*' | b'?' => {
                index += 1;
                if current.len() > longest.len() {
                    longest = std::mem::take(&mut current);
                }
                current.clear();
            }
            b'[' => {
                // Skip the class: its contents match a single byte, not
                // themselves. Backslash escapes protect `]` and `[`, and a
                // POSIX name brings its own brackets, so `[:x:]` is skipped
                // whole rather than ending the class at its `]`.
                let mut scan = index + 1;
                if matches!(part.get(scan), Some(b'!' | b'^')) {
                    scan += 1;
                }
                if part.get(scan) == Some(&b']') {
                    scan += 1;
                }
                while scan < part.len() && part[scan] != b']' {
                    if part[scan] == b'\\' {
                        scan = (scan + 2).min(part.len());
                    } else if part[scan] == b'[' && part.get(scan + 1) == Some(&b':') {
                        let name_start = scan + 2;
                        if let Some(rbracket) =
                            next_closing_bracket(part, name_start, &mut next_rbracket)
                            && rbracket > name_start
                            && part[rbracket - 1] == b':'
                            && is_git_posix_class_name(&part[name_start..rbracket - 1])
                        {
                            scan = rbracket + 1;
                        } else {
                            scan += 1;
                        }
                    } else {
                        scan += 1;
                    }
                }
                index = if scan < part.len() {
                    scan + 1
                } else {
                    part.len()
                };
                if current.len() > longest.len() {
                    longest = std::mem::take(&mut current);
                }
                current.clear();
            }
            byte => {
                current.push(byte);
                index += 1;
            }
        }
    }
    if current.len() > longest.len() {
        longest = current;
    }
    (longest.len() >= 2).then_some(longest)
}

/// Collapses a run of asterisks outside bracket classes into a single star.
///
/// A `**` component has already been taken out by [`compile_component`], so
/// every unbracketed run reaching here is one ordinary star in Git, and an
/// ordinary star never crosses a separator. An escaped asterisk or an asterisk
/// inside a bracket class is literal and must never be collapsed (otherwise
/// adjacent `*` range endpoints such as `[#-**-0]` would lose a range).
fn collapse_partial_stars(part: &[u8]) -> Vec<u8> {
    let mut collapsed = Vec::with_capacity(part.len());
    let mut index = 0;
    while index < part.len() {
        match part[index] {
            b'\\' if index + 1 < part.len() => {
                collapsed.extend_from_slice(&part[index..index + 2]);
                index += 2;
            }
            b'[' => {
                let end = validate_glob_class(part, index).unwrap_or(part.len());
                collapsed.extend_from_slice(&part[index..end]);
                index = end;
            }
            b'*' => {
                let run = part[index..]
                    .iter()
                    .take_while(|byte| **byte == b'*')
                    .count();
                collapsed.push(b'*');
                index += run;
            }
            byte => {
                collapsed.push(byte);
                index += 1;
            }
        }
    }
    collapsed
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::path::{Path, PathBuf};

    use super::{
        ComponentBudgetExceeded, EntryMatchContext, IgnoreMatchScratch, LineBudgets,
        MAX_IGNORE_FILE_COMPONENTS, MAX_IGNORE_FILE_RETAINED_BYTES, RuleSetBuilder, fuzz_rule,
        fuzz_rule_bytes, longest_literal_run, matches_components, normalize_bracket_classes,
        parse_rule, split_rule_components, without_dot_components,
    };

    #[test]
    fn dot_components_are_removed_without_resolving_parents_or_separators() {
        assert_eq!(
            without_dot_components(b"/repo/./src").as_ref(),
            b"/repo/src"
        );
        assert_eq!(without_dot_components(b"./src").as_ref(), b"src");
        assert_eq!(
            without_dot_components(b"a//./b/../c").as_ref(),
            b"a//b/../c"
        );
        assert!(matches!(
            without_dot_components(b"plain/path"),
            Cow::Borrowed(_)
        ));
    }

    /// Rule shapes to compare, one per line of a synthetic ignore file.
    const RULES: &[&str] = &[
        "debug.log",
        "*.log",
        "*.o",
        "build/",
        "logs/",
        "/root.txt",
        "src/temp.o",
        "**/foo",
        "a/**/b",
        "abc/**",
        "abc/*",
        "a**b",
        "doc/frotz",
        "!keep.log",
        "file[a-c].txt",
        "file[!a-c].txt",
        "*.[oa]",
        "*.[[:digit:]]",
        "file[[:upper:]].txt",
        "*.{ts,tsx}",
        "\\!literal",
        "\\#literal",
        "trailing ",
        "trailing\\ ",
        "#comment",
        "",
        "sp ace.txt",
        "**",
        "*",
        "node_modules/",
        "!node_modules/keep/",
        "deep/nested/*.rs",
        ".env",
        ".*",
    ];

    /// Candidates, matched as both a file and a directory.
    const PATHS: &[&str] = &[
        "debug.log",
        "keep.log",
        "sub/debug.log",
        "build",
        "build/main.o",
        "logs",
        "root.txt",
        "sub/root.txt",
        "src/temp.o",
        "foo",
        "a/b/foo",
        "a/b",
        "a/x/y/b",
        "abc",
        "abc/x.txt",
        "abc/deep/x.txt",
        "axxb",
        "a/x/b",
        "doc/frotz",
        "sub/doc/frotz",
        "filea.txt",
        "filez.txt",
        "fileA.txt",
        "main.o",
        "a.7",
        "a.ts",
        "a.{ts,tsx}",
        "!literal",
        "#literal",
        "trailing",
        "trailing ",
        "sp ace.txt",
        "node_modules",
        "node_modules/keep",
        "deep/nested/main.rs",
        ".env",
        ".hidden/x",
        // near misses, so an over-matching rule cannot hide
        "xfoo",
        "a/xfoo",
        "subdebug.log",
        "a/xb",
        "ax/b",
        "abcx/y",
        "xbuild",
        "build2",
        "x.tsx",
        "keep.log.bak",
        "node_modules2/x",
    ];

    /// Rules where this layer deliberately differs from the engine it replaces.
    ///
    /// Each is a case where the `ignore` crate does not reproduce Git, verified
    /// against `git check-ignore` and recorded in `corpus/ignore.jsonl`. Any
    /// other disagreement is a bug in this layer, which is what the test below
    /// turns into a failure.
    const DELIBERATE: &[&str] = &["*.[[:digit:]]", "*.{ts,tsx}", "file[[:upper:]].txt"];

    /// The candidate as the walker hands it over: the whole path in glob bytes.
    fn candidate(root: &Path, path: &str) -> Vec<u8> {
        crate::glob_path_bytes(&root.join(path)).into_owned()
    }

    fn ours(rule: &str, path: &str, is_dir: bool) -> Option<bool> {
        let root = PathBuf::from("/fixture");
        let mut builder = RuleSetBuilder::new(&root, false, false);
        builder.add_line(rule);
        builder.build().matched(&candidate(&root, path), is_dir)
    }

    fn theirs(rule: &str, path: &str, is_dir: bool) -> Option<bool> {
        let root = PathBuf::from("/fixture");
        let mut builder = ignore::gitignore::GitignoreBuilder::new(&root);
        let _ = builder.add_line(None, rule);
        let Ok(matcher) = builder.build() else {
            return None;
        };
        let matched = matcher.matched(root.join(path), is_dir);
        if matched.is_none() {
            None
        } else {
            Some(matched.is_ignore())
        }
    }

    /// Every rule and candidate, against the engine ADR-0014 replaces.
    ///
    /// The two agree everywhere except where the old engine is known to differ
    /// from Git, so a new disagreement means this layer moved.
    #[test]
    fn the_rule_layer_agrees_with_the_engine_it_replaces() {
        let mut disagreed = Vec::new();
        let mut compared = 0_usize;
        for rule in RULES {
            for path in PATHS {
                for is_dir in [false, true] {
                    compared += 1;
                    if ours(rule, path, is_dir) != theirs(rule, path, is_dir) {
                        disagreed.push(*rule);
                    }
                }
            }
        }
        disagreed.sort_unstable();
        disagreed.dedup();
        assert!(
            compared > 1000,
            "the matrix has to be broad, was {compared}"
        );
        assert_eq!(
            disagreed, DELIBERATE,
            "unexpected disagreement with the previous engine"
        );
    }

    /// The parser's own decisions, independent of any matcher.
    #[test]
    fn rule_lines_are_read_the_way_gitignore_describes_them() {
        assert!(parse_rule("").is_none(), "a blank line is not a rule");
        assert!(parse_rule("# comment").is_none());
        assert!(parse_rule("   ").is_none(), "spaces alone are not a rule");
        assert!(parse_rule("/").is_none(), "a lone separator is not a rule");
        for rule in ["a//b", "//foo", "x///y"] {
            assert!(
                parse_rule(rule).is_none(),
                "repeated separators are unmatchable: {rule}"
            );
        }
        assert!(
            parse_rule("foo\\").is_none(),
            "a dangling escape matches nothing"
        );
        assert!(
            parse_rule("\\#literal").is_some(),
            "an escaped hash is a rule"
        );

        let negated = parse_rule("!keep.log").expect("negation parses");
        assert!(negated.negated);
        assert!(!negated.directory_only);

        let directory = parse_rule("build/").expect("directory rule parses");
        assert!(directory.directory_only);
        assert!(!directory.negated);
    }

    #[test]
    fn a_slash_inside_a_bracket_class_is_not_a_path_separator() {
        assert_eq!(fuzz_rule("a[b/c]d", b"abd", false), Some(true));
        assert_eq!(fuzz_rule("a[b/c]d", b"acd", false), Some(true));
        assert_eq!(fuzz_rule("x[[:alpha:]/]y", b"xay", false), Some(true));
    }

    #[test]
    fn slash_range_endpoints_keep_every_matchable_byte() {
        for candidate in [b"x0y".as_slice(), b"x5y", b"x9y"] {
            assert_eq!(fuzz_rule_bytes(b"x[/-9]y", candidate, false), Some(true));
            assert_eq!(fuzz_rule_bytes(b"x[!/-9]y", candidate, false), None);
        }
        assert_eq!(fuzz_rule_bytes(b"x[.-/]y", b"x.y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[.-/]y", b"x-y", false), None);
        assert_eq!(fuzz_rule_bytes(b"[a-/]", b"a", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[a-/]", b"-", false), None);
        assert_eq!(fuzz_rule_bytes(b"[!a-/]", b"a", false), None);
        assert_eq!(fuzz_rule_bytes(b"[!a-/]", b"b", false), Some(true));
        for candidate in [b"a".as_slice(), b"-", b"9"] {
            assert_eq!(fuzz_rule_bytes(b"[a-/-9]", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"[a-/-9]", b"5", false), None);
        assert_eq!(fuzz_rule_bytes(b"[c-a]", b"c", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[c-a]", b"a", false), None);
        assert_eq!(fuzz_rule_bytes(b"[!c-a]", b"c", false), None);
        assert_eq!(fuzz_rule_bytes(b"[!c-a]", b"b", false), Some(true));
        for candidate in [b"c".as_slice(), b"-", b"z"] {
            assert_eq!(fuzz_rule_bytes(b"[c-a-z]", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"[c-a-z]", b"b", false), None);
    }

    #[test]
    fn slash_endpoints_keep_git_s_range_state() {
        // An ascending range that ends in a slash keeps the bytes below it.
        assert_eq!(fuzz_rule_bytes(b"x[--/]y", b"x.y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[--/]y", b"x-y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[--/]y", b"x0y", false), None);
        assert_eq!(fuzz_rule_bytes(b"x[+-/]y", b"x,y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[!--/]y", b"x.y", false), None);
        assert_eq!(fuzz_rule_bytes(b"x[\\!-/]y", b"x#y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[--/-9]y", b"x.y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[--/-9]y", b"x9y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[--/-9]y", b"x5y", false), None);
        // A completed range or POSIX class resets Git's range state, so the
        // dash before the slash is a literal member, not a reversed range.
        for rule in [b"x[a-c-/]y".as_slice(), b"x[[:alpha:]-/]y", b"x[z-a-/]y"] {
            assert_eq!(
                fuzz_rule_bytes(rule, b"x-y", false),
                Some(true),
                "{}",
                String::from_utf8_lossy(rule)
            );
        }
        assert_eq!(fuzz_rule_bytes(b"x[a-c-/]y", b"xby", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[z-a-/]y", b"xby", false), None);
        // A slash after a literal dash is itself a literal, so it may open
        // the following range.
        for candidate in [b"xay".as_slice(), b"x-y", b"xmy", b"x0y"] {
            assert_eq!(
                fuzz_rule_bytes(b"x[a-c-/-z]y", candidate, false),
                Some(true)
            );
        }
        assert_eq!(fuzz_rule_bytes(b"x[a-c-/-z]y", b"x.y", false), None);
        // A dropped slash does not move a literal negation marker into the
        // first position.
        assert_eq!(fuzz_rule_bytes(b"x[/^a]y", b"x^y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[/^a]y", b"xby", false), None);
        assert_eq!(fuzz_rule_bytes(b"x[/!a]y", b"x!y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[/!a]y", b"xby", false), None);
    }

    #[test]
    fn an_escaped_slash_is_a_component_separator() {
        assert_eq!(fuzz_rule_bytes(b"a\\/b", b"a/b", false), Some(true));
        for candidate in [b"a/y".as_slice(), b"ax/y"] {
            assert_eq!(fuzz_rule_bytes(b"a**\\/y", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"a**\\/y", b"a/x/y", false), None);
        assert_eq!(fuzz_rule_bytes(b"a/**\\/b", b"a/b", false), None);
        for candidate in [b"a/x/b".as_slice(), b"a/x/y/b"] {
            assert_eq!(fuzz_rule_bytes(b"a/**\\/b", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"**\\/a", b"a", false), None);
        assert_eq!(fuzz_rule_bytes(b"**\\/a", b"x/a", false), Some(true));
    }

    #[test]
    fn a_trailing_escaped_slash_does_not_name_a_backslash_directory() {
        for (rule, candidate) in [
            (b"a\\/".as_slice(), b"a\\".as_slice()),
            (b"**\\/", b"a\\"),
            (b"a*\\/", b"ax\\"),
        ] {
            assert_eq!(fuzz_rule_bytes(rule, candidate, true), None);
        }
    }

    #[test]
    fn whole_and_attached_star_runs_follow_git() {
        for candidate in [b"b".as_slice(), b"x/y/b"] {
            assert_eq!(fuzz_rule_bytes(b"***/b", candidate, false), Some(true));
        }
        for candidate in [b"a/y".as_slice(), b"ax/y"] {
            assert_eq!(fuzz_rule_bytes(b"a**/y", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"a**/y", b"a/x/y", false), None);
        assert_eq!(fuzz_rule_bytes(b"a**/y", b"ay", false), None);
        assert_eq!(fuzz_rule_bytes(b"a**/**", b"a", false), None);
        assert_eq!(fuzz_rule_bytes(b"a**/**", b"ab", false), None);
        assert_eq!(fuzz_rule_bytes(b"a**/**", b"a/x", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"a**b", b"a/x/b", false), None);
    }

    #[test]
    fn suffix_star_runs_respect_git_s_literal_prefix_and_juncture() {
        for rule in [
            b"[a]**/y".as_slice(),
            b"a?**/y",
            b"*a**/y",
            b"**/a**/y",
            b"a\\**/y",
        ] {
            assert_eq!(
                fuzz_rule_bytes(rule, b"a/x/y", false),
                None,
                "a wildcard or escape before the run makes it an ordinary star: {}",
                String::from_utf8_lossy(rule)
            );
        }

        assert_eq!(fuzz_rule_bytes(b"a**/y", b"ay", false), None);
        assert_eq!(fuzz_rule_bytes(b"a**/y", b"axy", false), None);
        assert_eq!(fuzz_rule_bytes(b"a**/y", b"aXy", false), None);
    }

    #[test]
    fn final_attached_and_repeated_whole_star_runs_follow_git() {
        for candidate in [b"a".as_slice(), b"ab"] {
            assert_eq!(fuzz_rule_bytes(b"/a**", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"/a**", b"a/b", false), None);
        assert_eq!(fuzz_rule_bytes(b"/a**", b"a/x/y", false), None);
        assert_eq!(fuzz_rule_bytes(b"a**/**/y", b"ay", false), None);
        assert_eq!(fuzz_rule_bytes(b"a**/**/y", b"a/y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"a**/**/y", b"a/x/y", false), Some(true));
    }

    #[test]
    fn slash_classes_use_git_s_anchoring_and_negation_rules() {
        assert_eq!(fuzz_rule_bytes(b"a[b/c]d", b"x/abd", false), None);
        assert_eq!(fuzz_rule_bytes(b"x[[:alpha:]/]y", b"q/xay", false), None);
        assert_eq!(fuzz_rule_bytes(b"a[!/]d", b"abd", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"a[!/]d", b"acd", false), Some(true));
    }

    #[test]
    fn first_closing_bracket_remains_a_literal_class_member() {
        assert_eq!(fuzz_rule_bytes(b"[]]", b"]", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[!]]", b"a", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[!]]", b"]", false), None);
        assert_eq!(fuzz_rule_bytes(b"[!]", b"a", false), None);
        assert_eq!(fuzz_rule_bytes(b"[]/]", b"]", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[]/]", b"x/]", false), None);
        assert_eq!(fuzz_rule_bytes(b"[!]/]", b"a", false), Some(true));
    }

    #[test]
    fn escaped_brackets_inside_classes_keep_accurate_literal_gates() {
        assert_eq!(longest_literal_run(b"[\\]a]"), None);
        assert_eq!(longest_literal_run(b"[a\\]b]"), None);
        assert_eq!(longest_literal_run(b"[a\\]foo]x"), None);
        assert_eq!(longest_literal_run(b"[a\\]foo]xy"), Some(b"xy".to_vec()));
        assert_eq!(longest_literal_run(b"[!\\]a]xy"), Some(b"xy".to_vec()));
        assert_eq!(longest_literal_run(b"[]\\]a]xy"), Some(b"xy".to_vec()));
        assert_eq!(longest_literal_run(b"[\\\\]xy"), Some(b"xy".to_vec()));
        assert_eq!(longest_literal_run(b"[a\\[:digit:]]"), None);
        assert_eq!(
            longest_literal_run(b"[a\\[:digit:]]xy"),
            Some(b"]xy".to_vec())
        );
        assert_eq!(
            longest_literal_run(b"[a\\[:digit:]foo]x"),
            Some(b"foo]x".to_vec())
        );
        assert_eq!(longest_literal_run(b"[[:foo]bar"), Some(b"bar".to_vec()));

        assert_eq!(fuzz_rule_bytes(b"[\\]a]", b"]", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[\\]a]", b"a", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[\\]a]", b"a]", false), None);

        for candidate in [b"a".as_slice(), b"]", b"b"] {
            assert_eq!(fuzz_rule_bytes(b"[a\\]b]", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"[a\\]b]", b"b]", false), None);

        for candidate in [b"ax".as_slice(), b"]x", b"fx", b"ox"] {
            assert_eq!(fuzz_rule_bytes(b"[a\\]foo]x", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"[a\\]foo]x", b"foo]x", false), None);

        for candidate in [b"a]".as_slice(), b"[]", b":]", b"d]"] {
            assert_eq!(
                fuzz_rule_bytes(b"[a\\[:digit:]]", candidate, false),
                Some(true)
            );
        }
        assert_eq!(fuzz_rule_bytes(b"[a\\[:digit:]]", b"0]", false), None);
        assert_eq!(fuzz_rule_bytes(b"[a\\[:digit:]]", b"a", false), None);
    }

    #[test]
    fn ranges_ending_or_starting_in_escaped_closing_bracket_match_git() {
        // On Windows, `\` is a path separator in `is_match_glob_path` and
        // cannot match inside a single path component.
        let expected_backslash = if cfg!(windows) { None } else { Some(true) };

        for candidate in [b"A".as_slice(), b"Z", b"[", b"]"] {
            assert_eq!(fuzz_rule_bytes(b"[A-\\]]", candidate, false), Some(true));
        }
        assert_eq!(
            fuzz_rule_bytes(b"[A-\\]]", b"\\", false),
            expected_backslash
        );
        for candidate in [b"A]".as_slice(), b"^", b"a"] {
            assert_eq!(fuzz_rule_bytes(b"[A-\\]]", candidate, false), None);
        }

        for candidate in [b"0".as_slice(), b"A", b"Z", b"[", b"]"] {
            assert_eq!(fuzz_rule_bytes(b"[/-\\]]", candidate, false), Some(true));
        }
        assert_eq!(
            fuzz_rule_bytes(b"[/-\\]]", b"\\", false),
            expected_backslash
        );
        for candidate in [b".".as_slice(), b"^", b"a"] {
            assert_eq!(fuzz_rule_bytes(b"[/-\\]]", candidate, false), None);
        }

        for candidate in [b"A".as_slice(), b"Z", b"[", b"]", b"z"] {
            assert_eq!(fuzz_rule_bytes(b"[A-\\]z]", candidate, false), Some(true));
        }
        assert_eq!(
            fuzz_rule_bytes(b"[A-\\]z]", b"\\", false),
            expected_backslash
        );
        assert_eq!(fuzz_rule_bytes(b"[A-\\]z]", b"a", false), None);

        for candidate in [b"]".as_slice(), b"^", b"_", b"a", b"z"] {
            assert_eq!(fuzz_rule_bytes(b"[\\]-z]", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"[\\]-z]", b"[", false), None);

        assert_eq!(fuzz_rule_bytes(b"[!A-\\]]", b"A", false), None);
        assert_eq!(fuzz_rule_bytes(b"[!A-\\]]", b"]", false), None);
        assert_eq!(fuzz_rule_bytes(b"[!A-\\]]", b"a", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[a-\\]]", b"a", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[a-\\]]", b"]", false), None);
        assert_eq!(fuzz_rule_bytes(b"[a-\\]]", b"-", false), None);
    }

    #[test]
    fn rule_set_normalizes_root_slashes_and_rejects_mismatched_prefixes() {
        for root in [
            Path::new("/repo"),
            Path::new("/repo/"),
            Path::new("/repo//"),
            Path::new("/repo///"),
        ] {
            let mut builder = RuleSetBuilder::new(root, false, false);
            builder.add_line("/foo.txt");
            builder.add_line("sub/bar.txt");
            let rules = builder.build();

            assert_eq!(rules.matched(b"/repo/foo.txt", false), Some(true));
            assert_eq!(rules.matched(b"/repo//foo.txt", false), Some(true));
            assert_eq!(rules.matched(b"/repo/sub/bar.txt", false), Some(true));
            assert_eq!(rules.matched(b"/repo//sub/bar.txt", false), Some(true));
            assert_eq!(rules.matched(b"/repo/deep/foo.txt", false), None);
            assert_eq!(rules.matched(b"/other/foo.txt", false), None);
            assert_eq!(rules.matched(b"/foo.txt", false), None);
        }

        for root in [Path::new("/"), Path::new("//")] {
            let mut builder = RuleSetBuilder::new(root, false, false);
            builder.add_line("/foo.txt");
            builder.add_line("sub/bar.txt");
            let rules = builder.build();

            assert_eq!(rules.matched(b"/foo.txt", false), Some(true));
            assert_eq!(rules.matched(b"//foo.txt", false), Some(true));
            assert_eq!(rules.matched(b"/sub/bar.txt", false), Some(true));
            assert_eq!(rules.matched(b"/deep/foo.txt", false), None);
        }
    }

    #[test]
    fn fuzz_rule_uses_empty_root_candidates_without_dropping_a_byte() {
        assert_eq!(fuzz_rule("abc", b"abc", false), Some(true));
        assert_eq!(fuzz_rule("abc", b"Xabc", false), None);
        assert_eq!(fuzz_rule("/a", b"a", false), Some(true));
        assert_eq!(fuzz_rule("/a", b"Xa", false), None);
        assert_eq!(
            fuzz_rule_bytes(b"\xE9latin1.txt", b"\xE9latin1.txt", false),
            Some(true)
        );
    }

    #[test]
    fn deeply_nested_unclosed_posix_openers_are_rejected() {
        let mut rule = String::from("[");
        rule.push_str("[:".repeat(32_768).as_str());
        assert!(parse_rule(&rule).is_none());
    }

    fn count_posix_closer_searches<T>(f: impl FnOnce() -> T) -> (T, usize) {
        super::POSIX_CLOSER_SEARCHES.with(|count| count.set(0));
        let output = f();
        let searches = super::POSIX_CLOSER_SEARCHES.with(std::cell::Cell::get);
        (output, searches)
    }

    fn count_posix_closer_bytes<T>(f: impl FnOnce() -> T) -> (T, usize) {
        super::POSIX_CLOSER_BYTES_SCANNED.with(|bytes| bytes.set(0));
        let output = f();
        let scanned = super::POSIX_CLOSER_BYTES_SCANNED.with(std::cell::Cell::get);
        (output, scanned)
    }

    #[test]
    fn large_unclosed_posix_openers_scan_at_most_once_per_pass() {
        // 262_144 * 2 bytes = 512 KiB of `[:` openers: verify deterministically
        // that each parser pass performs at most one `]` search rather than
        // rescanning the remainder at every nested opener.
        let openers = "[:".repeat(262_144);

        let mut unclosed = String::with_capacity(openers.len() + 1);
        unclosed.push('[');
        unclosed.push_str(&openers);
        let (parsed, searches) = count_posix_closer_searches(|| parse_rule(&unclosed));
        assert!(parsed.is_none());
        // One failed search each in `split_rule_components`, `component_cost`,
        // and `normalize_bracket_class` (`longest_literal_run` is not reached).
        assert_eq!(searches, 3);

        // Closed with a non-colon byte before `]` so no `:]` exists anywhere
        // in the rule. `normalize_bracket_class` escapes every literal `[`, so
        // `longest_literal_run` sees no unescaped `[:` and needs 0 searches.
        let mut closed_without_posix_closer = String::with_capacity(openers.len() + 3);
        closed_without_posix_closer.push('[');
        closed_without_posix_closer.push_str(&openers);
        closed_without_posix_closer.push_str("a]");
        let (rule, searches) =
            count_posix_closer_searches(|| parse_rule(&closed_without_posix_closer));
        let rule = rule.expect("closed class parses");
        assert_eq!(searches, 3);
        assert!(rule.matches(b"[", false));
        assert!(rule.matches(b":", false));
        assert!(rule.matches(b"a", false));
        assert!(!rule.matches(b"b", false));

        // Trailing `:]` without an outer `]` closes the first `[:` opener with
        // an invalid POSIX class name and aborts in one scan per pass.
        let mut trailing_posix_closer = String::with_capacity(openers.len() + 3);
        trailing_posix_closer.push('[');
        trailing_posix_closer.push_str(&openers);
        trailing_posix_closer.push_str(":]");
        let (parsed, searches) = count_posix_closer_searches(|| parse_rule(&trailing_posix_closer));
        assert!(parsed.is_none());
        assert_eq!(searches, 3);

        // Trailing `:]]` closes the first `[:` opener with an invalid POSIX
        // class name (`[:[:...`), which Git aborts with `WM_ABORT_ALL` in one
        // scan per pass.
        let mut closed_with_posix_closer = String::with_capacity(openers.len() + 4);
        closed_with_posix_closer.push('[');
        closed_with_posix_closer.push_str(&openers);
        closed_with_posix_closer.push_str(":]]");
        let (parsed, searches) =
            count_posix_closer_searches(|| parse_rule(&closed_with_posix_closer));
        assert!(parsed.is_none());
        assert_eq!(searches, 3);

        // Repeated separate bracket classes each holding a non-POSIX `[:a]`:
        // every byte is scanned at most once per pass (strictly O(N) total
        // scanned bytes).
        let repeated_classes = "[[:a]".repeat(100_000);
        let (parts, split_bytes) =
            count_posix_closer_bytes(|| split_rule_components(repeated_classes.as_bytes()));
        assert_eq!(parts.len(), 1);
        assert!(split_bytes <= repeated_classes.len());

        let (normalized, norm_bytes) =
            count_posix_closer_bytes(|| normalize_bracket_classes(parts[0].bytes, false));
        let normalized = normalized.expect("repeated closed classes normalize");
        assert_eq!(normalized.len(), repeated_classes.len() + 100_000);
        assert!(norm_bytes <= repeated_classes.len());

        let (literal, literal_bytes) =
            count_posix_closer_bytes(|| longest_literal_run(&normalized));
        assert!(literal.is_none());
        assert_eq!(literal_bytes, 0);
    }

    #[test]
    fn trailing_spaces_are_dropped_unless_escaped() {
        let root = Path::new("/fixture");
        let mut builder = RuleSetBuilder::new(root, false, false);
        builder.add_line("trailing ");
        assert_eq!(
            builder.build().matched(&candidate(root, "trailing"), false),
            Some(true)
        );

        let mut builder = RuleSetBuilder::new(root, false, false);
        builder.add_line("trailing\\ ");
        let rules = builder.build();
        assert_eq!(
            rules.matched(&candidate(root, "trailing "), false),
            Some(true)
        );
        assert_eq!(rules.matched(&candidate(root, "trailing"), false), None);
    }

    #[test]
    fn the_last_matching_rule_decides() {
        let root = Path::new("/fixture");
        let mut builder = RuleSetBuilder::new(root, false, false);
        builder.add_line("*.log");
        builder.add_line("!keep.log");
        builder.add_line("keep.log");
        assert_eq!(
            builder.build().matched(&candidate(root, "keep.log"), false),
            Some(true),
            "the later rule wins over the negation before it"
        );
    }

    /// A rule with many runs, against a candidate built to make each one
    /// backtrack.
    ///
    /// The recursive matcher this replaced was exponential in the number of
    /// `**` components: eight of them against a deep path took longer than any
    /// walk would wait, and a rule file inside the tree being walked is where
    /// that input would come from. The bound is generous on purpose - the point
    /// is the difference between milliseconds and never.
    #[test]
    fn many_runs_do_not_explode() {
        let root = PathBuf::from("/fixture");
        let mut builder = RuleSetBuilder::new(&root, false, false);
        builder.add_line("**/a/**/a/**/a/**/a/**/a/**/a/**/a/**/x");
        let rules = builder.build();
        let deep = "a/".repeat(24) + "b";

        let start = std::time::Instant::now();
        assert_eq!(rules.matched(&candidate(&root, &deep), false), None);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "matching took {:?}",
            start.elapsed()
        );
    }

    fn count_pattern_compiles<T>(f: impl FnOnce() -> T) -> (T, usize) {
        super::PATTERN_COMPILES.with(|count| count.set(0));
        let output = f();
        let compiles = super::PATTERN_COMPILES.with(std::cell::Cell::get);
        (output, compiles)
    }

    fn count_part_vec_allocs<T>(f: impl FnOnce() -> T) -> (T, usize) {
        super::PART_VEC_ALLOCS.with(|count| count.set(0));
        let output = f();
        let allocs = super::PART_VEC_ALLOCS.with(std::cell::Cell::get);
        (output, allocs)
    }

    #[test]
    fn glob_ir_budget_estimator_matches_pattern_compile_exact_boundaries() {
        let options = super::component_options(false);
        let check = |label: &str, pattern: &[u8], expect_exceeds: bool| {
            let compiled_err = super::Pattern::compile(pattern, options).is_err();
            let estimated_err = super::component_exceeds_glob_ir_budget(pattern);
            assert_eq!(
                compiled_err, expect_exceeds,
                "{label}: Pattern::compile unexpected result"
            );
            assert_eq!(
                estimated_err, compiled_err,
                "{label}: zero-alloc IR estimator disagreed with Pattern::compile"
            );
        };

        // `?` token boundary: 1,048,576 fits, 1,048,577 exceeds.
        check("qmarks-at-limit", &vec![b'?'; 1_048_576], false);
        check("qmarks-over-limit", &vec![b'?'; 1_048_577], true);

        // WideSweepEngine `*<lit>*` boundary: each 64-position word charges
        // `(268 * 8) / GLOB_TOKEN_SIZE` IR units (67 on 64-bit, 134 on 32-bit),
        // plus 3 tokens for `[Star, Literal, Star]`.
        let wide_word_units = (super::GLOB_WIDE_WORD_ROWS_AT_COMPILE * std::mem::size_of::<u64>())
            .div_ceil(super::GLOB_TOKEN_SIZE);
        let max_wide_words = (super::GLOB_MAX_COMPILED_IR_UNITS - 3) / wide_word_units;
        let max_wide_positions = max_wide_words * (u64::BITS as usize) - 1;
        let max_lit_len = max_wide_positions - 2;
        let mut wide_fit = Vec::with_capacity(max_lit_len + 2);
        wide_fit.push(b'*');
        wide_fit.resize(max_lit_len + 1, b'a');
        wide_fit.push(b'*');
        check("wide-sweep-at-limit", &wide_fit, false);
        wide_fit.insert(1, b'a');
        check("wide-sweep-over-limit", &wide_fit, true);

        // Bracket-class boundary: each `[a]` or `[a-z]` is 1 token + 1 member = 2 IR units.
        check(
            "class-byte-at-limit",
            "[a]".repeat(524_288).as_bytes(),
            false,
        );
        check(
            "class-byte-over-limit",
            "[a]".repeat(524_289).as_bytes(),
            true,
        );
        check(
            "class-range-at-limit",
            "[a-z]".repeat(524_288).as_bytes(),
            false,
        );
        check(
            "class-range-over-limit",
            "[a-z]".repeat(524_289).as_bytes(),
            true,
        );
        // Trailing dash `[a-]` is 1 token + 2 members = 3 IR units: 349,525 * 3 = 1,048,575.
        check(
            "class-trailing-dash-at-limit",
            "[a-]".repeat(349_525).as_bytes(),
            false,
        );
        check(
            "class-trailing-dash-over-limit",
            "[a-]".repeat(349_526).as_bytes(),
            true,
        );

        // PathFilter second-alternative copy (`a/` followed by `?`s):
        // original = 2 + k tokens, copy = 2 + k tokens -> 2*(2 + k) <= 1,048,576 -> k = 524,286.
        let mut sep_fit = b"a/".to_vec();
        sep_fit.resize(2 + 524_286, b'?');
        check("path-filter-copy-at-limit", &sep_fit, false);
        sep_fit.push(b'?');
        check("path-filter-copy-over-limit", &sep_fit, true);

        let root = Path::new("/fixture");

        // A long consecutive `*` run collapses via `collapse_partial_stars` to
        // one `*` before `Pattern::compile`, so `component_cost` charges its
        // collapsed length rather than raw source length and never trips
        // `FileBytes`.
        let collapsed_star_rule = format!("{}*.log", "*".repeat(2_100_000));
        let mut collapsed_builder = RuleSetBuilder::new(root, false, false);
        collapsed_builder
            .add_line_within_budget(
                &collapsed_star_rule,
                LineBudgets::single_file(
                    MAX_IGNORE_FILE_COMPONENTS,
                    MAX_IGNORE_FILE_RETAINED_BYTES,
                ),
            )
            .expect("collapsed star run fits comfortably within FileBytes budget");
        let collapsed_rules = collapsed_builder.build();
        assert_eq!(
            collapsed_rules.matched(b"/fixture/foo.log", false),
            Some(true)
        );
        assert_eq!(collapsed_rules.matched(b"/fixture/foo.txt", false), None);

        // Rejecting a large valid component (> 64 KiB) under a tight byte budget
        // must NOT compile any Pattern or allocate `Vec<RulePart>`.
        for large_valid in ["?".repeat(200_000), format!("*{}*", "a".repeat(200_000))] {
            let mut builder = RuleSetBuilder::new(root, false, false);
            let ((result, compiles), part_vec_allocs) = count_part_vec_allocs(|| {
                count_pattern_compiles(|| {
                    builder.add_line_within_budget(
                        &large_valid,
                        LineBudgets::single_file(MAX_IGNORE_FILE_COMPONENTS, 1_000_000),
                    )
                })
            });
            assert_eq!(result, Err(ComponentBudgetExceeded::FileBytes));
            assert_eq!(compiles, 0);
            assert_eq!(part_vec_allocs, 0);
        }
    }

    #[test]
    fn retained_byte_constants_cover_rule_and_component_struct_sizes() {
        let rule_size = std::mem::size_of::<super::Rule>();
        let component_size = std::mem::size_of::<super::Component>();
        assert!(
            rule_size * 2 <= super::RULE_FIXED_RETAINED_BYTES,
            "RULE_FIXED_RETAINED_BYTES ({}) must cover Rule inline size * 2 ({})",
            super::RULE_FIXED_RETAINED_BYTES,
            rule_size * 2
        );
        assert!(
            rule_size * 4 <= super::RULE_FIXED_RETAINED_BYTES + super::RULE_FIRST_EXTRA_BYTES,
            "first-rule fixed overhead ({}) must cover Rule inline size * 4 ({})",
            super::RULE_FIXED_RETAINED_BYTES + super::RULE_FIRST_EXTRA_BYTES,
            rule_size * 4
        );
        assert!(
            component_size * 2 <= super::ANY_DIRS_COMPONENT_BYTES,
            "ANY_DIRS_COMPONENT_BYTES ({}) must cover Component inline size * 2 ({})",
            super::ANY_DIRS_COMPONENT_BYTES,
            component_size * 2
        );
        assert!(
            component_size * 4 <= super::LITERAL_COMPONENT_FIXED_BYTES,
            "LITERAL_COMPONENT_FIXED_BYTES ({}) must cover Component inline size * 4 ({})",
            super::LITERAL_COMPONENT_FIXED_BYTES,
            component_size * 4
        );
        assert!(
            component_size * 4 <= super::PATTERN_COMPONENT_FIXED_BYTES,
            "PATTERN_COMPONENT_FIXED_BYTES ({}) must cover Component inline size * 4 ({})",
            super::PATTERN_COMPONENT_FIXED_BYTES,
            component_size * 4
        );
    }

    #[test]
    fn single_line_component_budget_enforces_boundaries_and_rejects_explosions() {
        let root = Path::new("/fixture");
        let budgets = |file_parsed, file_compiled, chain_compiled| LineBudgets {
            file_parsed_remaining: file_parsed,
            file_compiled_remaining: file_compiled,
            file_bytes_remaining: usize::MAX,
            chain_compiled_remaining: chain_compiled,
            chain_bytes_remaining: usize::MAX,
        };

        // Exact anchored boundary: 5 literal parts fit in a budget of 5, not 4.
        // The first rule charges `RULE_FIRST_EXTRA_BYTES` for the initial
        // 4-slot `Vec<Rule>` backing allocation; subsequent rules charge only
        // `RULE_FIXED_RETAINED_BYTES`.
        let mut builder = RuleSetBuilder::new(root, false, false);
        assert_eq!(
            builder.add_line_within_budget("a/b/c/d/e", budgets(4, 4, 100)),
            Err(ComponentBudgetExceeded::File)
        );
        assert_eq!(builder.compiled_units(), 0);
        let usage = builder
            .add_line_within_budget("a/b/c/d/e", budgets(5, 5, 100))
            .expect("five parts fit in five-unit budget");
        assert_eq!(usage.parsed_parts, 5);
        assert_eq!(usage.compiled_units, 5);
        assert_eq!(
            usage.retained_bytes,
            super::RULE_FIXED_RETAINED_BYTES
                + super::RULE_FIRST_EXTRA_BYTES
                + builder.root_prefix.capacity()
                + 5 * (super::LITERAL_COMPONENT_FIXED_BYTES + super::LITERAL_BYTES_PER_BYTE)
        );
        let second = builder
            .add_line_within_budget("f/g", budgets(5, 5, 100))
            .expect("second rule fits");
        assert_eq!(
            second.retained_bytes,
            super::RULE_FIXED_RETAINED_BYTES
                + 2 * (super::LITERAL_COMPONENT_FIXED_BYTES + super::LITERAL_BYTES_PER_BYTE)
        );
        assert_eq!(builder.compiled_units(), 7);

        // Byte-only overflow under the 250k component cap: `/*a*/` repeated
        // 82,000 times has 82,000 compiled components (< 250,000) and > 256 MiB
        // estimated retained storage, so it must trigger the zero-allocation
        // pre-scan without allocating `Vec<RulePart>` or compiling any patterns.
        let wide_bytes_explosion = format!("/{}", "*a*/".repeat(82_000));
        let mut builder = RuleSetBuilder::new(root, false, false);
        let ((result, compiles), part_vec_allocs) = count_part_vec_allocs(|| {
            count_pattern_compiles(|| {
                builder.add_line_within_budget(
                    &wide_bytes_explosion,
                    LineBudgets::single_file(
                        MAX_IGNORE_FILE_COMPONENTS,
                        MAX_IGNORE_FILE_RETAINED_BYTES,
                    ),
                )
            })
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::FileBytes));
        assert_eq!(compiles, 0);
        assert_eq!(
            part_vec_allocs, 0,
            "byte-only multi-component overflow must reject in pre-scan before Vec<RulePart>"
        );

        // Single-line path-component explosion (`"a/".repeat(250_001)`) must
        // reject in the zero-allocation pre-scan with 0 `Vec<RulePart>`
        // allocations and 0 pattern compiles.
        let component_explosion = "a/".repeat(MAX_IGNORE_FILE_COMPONENTS + 1);
        let mut builder = RuleSetBuilder::new(root, false, false);
        let ((result, compiles), part_vec_allocs) = count_part_vec_allocs(|| {
            count_pattern_compiles(|| {
                builder.add_line_within_budget(
                    &component_explosion,
                    LineBudgets::single_file(
                        MAX_IGNORE_FILE_COMPONENTS,
                        MAX_IGNORE_FILE_RETAINED_BYTES,
                    ),
                )
            })
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::File));
        assert_eq!(compiles, 0);
        assert_eq!(
            part_vec_allocs, 0,
            "250,001-component explosion must reject in pre-scan before Vec<RulePart>"
        );

        // Every compiled `Component` counts as 1 component toward component
        // budgets: `/[a/]` (whose `/` is stripped by normalization) fits a
        // 1-component budget, while invalid POSIX classes like `/[[:a/b:]]?`
        // and `/[[:a_b:]]?` are rejected cleanly (`WM_ABORT_ALL`) with 0
        // compiled units and 0 retained bytes.
        let mut builder = RuleSetBuilder::new(root, false, false);
        let stripped_slash_usage = builder
            .add_line_within_budget("/[a/]", budgets(100, 1, 100))
            .expect("normalized slash in [a/] costs 1 compiled component");
        assert_eq!(stripped_slash_usage.compiled_units, 1);

        for invalid_posix in ["/[[:a/b:]]?", "/[[:a_b:]]?"] {
            let mut invalid_builder = RuleSetBuilder::new(root, false, false);
            let usage = invalid_builder
                .add_line_within_budget(invalid_posix, budgets(100, 1, 100))
                .expect("invalid POSIX class is discarded cleanly");
            assert_eq!(usage.parsed_parts, 1);
            assert_eq!(usage.compiled_units, 0);
            assert_eq!(usage.retained_bytes, 0);
        }

        let slash_class_component_explosion =
            format!("/{}", "[[:a/b:]]/".repeat(MAX_IGNORE_FILE_COMPONENTS + 1));
        let mut builder = RuleSetBuilder::new(root, false, false);
        let ((result, compiles), part_vec_allocs) = count_part_vec_allocs(|| {
            count_pattern_compiles(|| {
                builder.add_line_within_budget(
                    &slash_class_component_explosion,
                    LineBudgets::single_file(
                        MAX_IGNORE_FILE_COMPONENTS,
                        MAX_IGNORE_FILE_RETAINED_BYTES,
                    ),
                )
            })
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::File));
        assert_eq!(compiles, 0);
        assert_eq!(
            part_vec_allocs, 0,
            "separator-bearing class component overflow must reject in pre-scan before Vec<RulePart>"
        );

        // A long `root_prefix` (~8 KiB) charges `root_prefix.capacity()` once
        // on the first retained rule (and counts toward byte budgets), while an
        // empty `RuleSet` retains zero bytes.
        let deep_root = PathBuf::from(format!("/{}", "dir/".repeat(2_048)));
        let empty_deep = RuleSetBuilder::new(&deep_root, false, false);
        let deep_prefix_cap = empty_deep.root_prefix.capacity();
        assert!(deep_prefix_cap >= 8 * 1024);
        assert_eq!(empty_deep.retained_bytes(), 0);
        assert_eq!(empty_deep.build().retained_bytes(), 0);

        let mut deep_builder = RuleSetBuilder::new(&deep_root, false, false);
        let mut short_builder = RuleSetBuilder::new(Path::new("/a"), false, false);
        let short_prefix_cap = short_builder.root_prefix.capacity();
        let deep_first = deep_builder
            .add_line_within_budget("foo", LineBudgets::UNBOUNDED)
            .expect("deep root first rule succeeds");
        let short_first = short_builder
            .add_line_within_budget("foo", LineBudgets::UNBOUNDED)
            .expect("short root first rule succeeds");
        assert_eq!(
            deep_first.retained_bytes - short_first.retained_bytes,
            deep_prefix_cap - short_prefix_cap,
            "first rule must charge exact root_prefix.capacity() difference"
        );
        let deep_second = deep_builder
            .add_line_within_budget("bar", LineBudgets::UNBOUNDED)
            .expect("deep root second rule succeeds");
        let short_second = short_builder
            .add_line_within_budget("bar", LineBudgets::UNBOUNDED)
            .expect("short root second rule succeeds");
        assert_eq!(
            deep_second.retained_bytes, short_second.retained_bytes,
            "root_prefix.capacity() must be charged only once per non-empty RuleSet"
        );
        // Tight chain-byte budget smaller than `deep_first.retained_bytes`
        // rejects the deep-root rule with `ChainBytes`.
        let mut tight_deep = RuleSetBuilder::new(&deep_root, false, false);
        assert_eq!(
            tight_deep.add_line_within_budget(
                "foo",
                LineBudgets {
                    file_parsed_remaining: 100,
                    file_compiled_remaining: 100,
                    file_bytes_remaining: usize::MAX,
                    chain_compiled_remaining: 100,
                    chain_bytes_remaining: deep_first.retained_bytes - 1,
                }
            ),
            Err(ComponentBudgetExceeded::ChainBytes)
        );

        // Wildcard-heavy line `"?x/".repeat(200_000)` (200,000 components <
        // 250,000, > 256 MiB estimated retained storage) must trigger the
        // zero-allocation pre-scan and fail fast on `FileBytes` without
        // allocating `Vec<RulePart>` or compiling any patterns.
        let qx_explosion = "?x/".repeat(200_000);
        let mut builder = RuleSetBuilder::new(root, false, false);
        let ((result, compiles), part_vec_allocs) = count_part_vec_allocs(|| {
            count_pattern_compiles(|| {
                builder.add_line_within_budget(
                    &qx_explosion,
                    LineBudgets::single_file(
                        MAX_IGNORE_FILE_COMPONENTS,
                        MAX_IGNORE_FILE_RETAINED_BYTES,
                    ),
                )
            })
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::FileBytes));
        assert_eq!(compiles, 0);
        assert_eq!(
            part_vec_allocs, 0,
            "wildcard-heavy multi-component byte explosion must reject in pre-scan before Vec<RulePart>"
        );

        // Unanchored literal rules add a leading `AnyDirs` component (`AnyDirs` + `Pattern`).
        let mut builder = RuleSetBuilder::new(root, false, false);
        assert_eq!(
            builder.add_line_within_budget("foo", budgets(10, 1, 100)),
            Err(ComponentBudgetExceeded::File)
        );
        let usage = builder
            .add_line_within_budget("foo", budgets(1, 2, 100))
            .expect("unanchored literal fits in 1 parsed part and 2 compiled components");
        assert_eq!(usage.parsed_parts, 1);
        assert_eq!(usage.compiled_units, 2);

        // Ordinary wildcard components count as 1 compiled component (2 with
        // leading `AnyDirs`) so large realistic glob ignore files fit under the
        // 250,000-component ceiling, while charging higher retained bytes than
        // escaped literals.
        let mut builder = RuleSetBuilder::new(root, false, false);
        let wildcard_usage = builder
            .add_line_within_budget("*.generated-00001.json", budgets(10, 2, 10))
            .expect("wildcard suffix rule counts as 2 compiled components");
        assert_eq!(wildcard_usage.parsed_parts, 1);
        assert_eq!(wildcard_usage.compiled_units, 2);
        let literal_usage = builder
            .add_line_within_budget("a\\?b", budgets(10, 2, 10))
            .expect("escaped wildcard stays 2 compiled components");
        assert_eq!(literal_usage.compiled_units, 2);
        assert!(wildcard_usage.retained_bytes > literal_usage.retained_bytes);

        // An invalid POSIX class name (`[:bogus:]`) is discarded cleanly
        // (`WM_ABORT_ALL`), while a valid POSIX class or non-POSIX `[:foo]`
        // followed by wildcards bounds retained bytes by full component length.
        let bogus_posix = format!("[[:bogus:]]{}", "?".repeat(1_000));
        let mut builder = RuleSetBuilder::new(root, false, false);
        let usage = builder
            .add_line_within_budget(&bogus_posix, budgets(10, 2, 2))
            .expect("invalid POSIX class rule is discarded cleanly");
        assert_eq!(usage.parsed_parts, 1);
        assert_eq!(usage.compiled_units, 0);
        assert_eq!(usage.retained_bytes, 0);

        for valid_posix in [
            format!("[[:alpha:]]{}", "?".repeat(1_000)),
            format!("[[:foo]{}", "?".repeat(1_000)),
        ] {
            let mut builder = RuleSetBuilder::new(root, false, false);
            let usage = builder
                .add_line_within_budget(&valid_posix, budgets(10, 2, 2))
                .expect("single component fits 2 compiled components");
            assert_eq!(usage.parsed_parts, 1);
            assert_eq!(usage.compiled_units, 2);
            assert!(usage.retained_bytes >= valid_posix.len() * super::PATTERN_BYTES_PER_BYTE);
        }

        // Dense `?` component fails fast on `FileBytes` before allocating
        // `Vec<RulePart>` or compiling any patterns.
        let many_qmarks = "?".repeat(60_000);
        let mut builder = RuleSetBuilder::new(root, false, false);
        let ((result, compiles), part_vec_allocs) = count_part_vec_allocs(|| {
            count_pattern_compiles(|| {
                builder
                    .add_line_within_budget(&many_qmarks, LineBudgets::single_file(100, 1_000_000))
            })
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::FileBytes));
        assert_eq!(
            compiles, 0,
            "wildcard-heavy component exceeding byte budget must fail before compiling"
        );
        assert_eq!(part_vec_allocs, 0);
        let usage = builder
            .add_line_within_budget(&many_qmarks, LineBudgets::single_file(100, 10_000_000))
            .expect("60,000 question marks fit in 10 MB retained-byte budget");
        assert_eq!(usage.parsed_parts, 1);
        assert_eq!(usage.compiled_units, 2);
        assert!(usage.retained_bytes >= many_qmarks.len() * super::PATTERN_BYTES_PER_BYTE);

        // Multi-star `WideSweepEngine` component charges conservative retained
        // bytes and fails fast on `FileBytes` before compiling.
        let wide_sweep = format!("*{}*", "a".repeat(40_000));
        let mut builder = RuleSetBuilder::new(root, false, false);
        let (result, compiles) = count_pattern_compiles(|| {
            builder.add_line_within_budget(
                &wide_sweep,
                LineBudgets {
                    file_parsed_remaining: 100,
                    file_compiled_remaining: 100_000,
                    file_bytes_remaining: 1_000_000,
                    chain_compiled_remaining: 1_000_000,
                    chain_bytes_remaining: 32 * 1024 * 1024,
                },
            )
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::FileBytes));
        assert_eq!(
            compiles, 0,
            "multi-star WideSweepEngine bomb must fail fast before compiling"
        );

        // Trailing `/**` adds a synthetic `*` unit (`a`, `AnyDirs`, `*`).
        let mut builder = RuleSetBuilder::new(root, false, false);
        assert_eq!(
            builder.add_line_within_budget("a/**", budgets(2, 2, 100)),
            Err(ComponentBudgetExceeded::File)
        );
        let usage = builder
            .add_line_within_budget("a/**", budgets(2, 3, 100))
            .expect("trailing double-star rule fits in 2 parsed parts and 3 compiled units");
        assert_eq!(usage.parsed_parts, 2);
        assert_eq!(usage.compiled_units, 3);

        // Repeated `**/` collapses into one `AnyDirs` in the compiled rule,
        // allocates exact capacity, and tracks parsed parts separately from
        // compiled units.
        let repeated_stars = format!("{}a", "**/".repeat(20));
        let mut builder = RuleSetBuilder::new(root, false, false);
        assert_eq!(
            builder.add_line_within_budget(&repeated_stars, budgets(20, 100, 100)),
            Err(ComponentBudgetExceeded::File)
        );
        let usage = builder
            .add_line_within_budget(&repeated_stars, budgets(21, 2, 100))
            .expect("collapsed double-star rule fits within 21 parsed parts and 2 compiled slots");
        assert_eq!(usage.parsed_parts, 21);
        assert_eq!(usage.compiled_units, 2);
        let rules = builder.build();
        assert_eq!(rules.compiled_units(), 2);
        assert_eq!(rules.rules[0].components.capacity(), 2);

        // With 2 chain slots remaining, `**/**/name` (3 parsed parts, 2 compiled
        // units) is accepted; with 1 chain slot remaining, it is rejected
        // as a chain-budget breach without compiling any patterns.
        let mut builder = RuleSetBuilder::new(root, false, false);
        let (result, compiles) = count_pattern_compiles(|| {
            builder.add_line_within_budget("**/**/name", budgets(10, 10, 1))
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::Chain));
        assert_eq!(compiles, 0);
        let usage = builder
            .add_line_within_budget("**/**/name", budgets(10, 10, 2))
            .expect("collapsed rule fits in two remaining chain slots");
        assert_eq!(usage.parsed_parts, 3);
        assert_eq!(usage.compiled_units, 2);
        assert_eq!(
            builder.build().matched(b"/fixture/x/y/name", false),
            Some(true)
        );

        // Discarded malformed rules still charge their parsed parts without
        // consuming compiled or chain slots — and without invoking
        // `Pattern::compile`, even for `[[:x][:][]` with 1 compiled slot left.
        let mut builder = RuleSetBuilder::new(root, false, false);
        for (malformed, expected_parts) in [
            ("a/b/c/[", 4),
            ("a/b/c/[/]", 4),
            ("a/b/c/[!]", 4),
            ("a/b/c//d", 3),
            ("[[:x][:][]", 1),
        ] {
            let (result, compiles) = count_pattern_compiles(|| {
                builder.add_line_within_budget(malformed, budgets(10, 1, 1))
            });
            let usage = result.expect("malformed line is discarded instead of triggering overflow");
            assert_eq!(compiles, 0, "{malformed}");
            assert_eq!(usage.parsed_parts, expected_parts, "{malformed}");
            assert_eq!(usage.compiled_units, 0, "{malformed}");
            assert_eq!(usage.retained_bytes, 0, "{malformed}");
        }
        assert_eq!(builder.compiled_units(), 0);
        assert!(builder.build().is_empty());

        // An anchored rule with `MAX_IGNORE_FILE_COMPONENTS` parsed parts
        // ending in `/**` requires `MAX_IGNORE_FILE_COMPONENTS + 1` compiled
        // units (synthetic trailing `*`) and must fail fast BEFORE compiling
        // any patterns — both without brackets and with `[[:alpha:]]`.
        let trailing_double_star_overflow =
            format!("{}**", "a/".repeat(MAX_IGNORE_FILE_COMPONENTS - 1));
        let mut builder = RuleSetBuilder::new(root, false, false);
        let (result, compiles) = count_pattern_compiles(|| {
            builder.add_line_within_budget(
                &trailing_double_star_overflow,
                budgets(
                    MAX_IGNORE_FILE_COMPONENTS,
                    MAX_IGNORE_FILE_COMPONENTS,
                    MAX_IGNORE_FILE_COMPONENTS * 4,
                ),
            )
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::File));
        assert_eq!(
            compiles, 0,
            "synthetic trailing wildcard overflow must fail fast before compiling patterns"
        );
        assert_eq!(builder.compiled_units(), 0);
        assert!(builder.build().is_empty());

        // Over compiled budget with a valid `[[:alpha:]]` POSIX class:
        // zero-allocation syntax validation avoids `Pattern::compile`
        // completely and rejects with `ComponentBudgetExceeded::File`.
        let posix_bracket_overflow = format!(
            "{}[[:alpha:]]/**",
            "a/".repeat(MAX_IGNORE_FILE_COMPONENTS - 2)
        );
        let mut builder = RuleSetBuilder::new(root, false, false);
        let (result, compiles) = count_pattern_compiles(|| {
            builder.add_line_within_budget(
                &posix_bracket_overflow,
                budgets(
                    MAX_IGNORE_FILE_COMPONENTS,
                    MAX_IGNORE_FILE_COMPONENTS,
                    MAX_IGNORE_FILE_COMPONENTS * 4,
                ),
            )
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::File));
        assert_eq!(
            compiles, 0,
            "over-budget rule with a valid POSIX bracket class must fail fast with 0 Pattern::compile calls"
        );

        // Same scale ending in a malformed pseudo-POSIX class `[[:x][:][]`:
        // streaming syntax validation detects the malformed component with 0
        // `Pattern::compile` calls and discards the rule cleanly.
        let malformed_tail = format!("{}[[:x][:][]", "a/".repeat(MAX_IGNORE_FILE_COMPONENTS - 1));
        let mut builder = RuleSetBuilder::new(root, false, false);
        let (result, compiles) = count_pattern_compiles(|| {
            builder.add_line_within_budget(
                &malformed_tail,
                budgets(
                    MAX_IGNORE_FILE_COMPONENTS,
                    MAX_IGNORE_FILE_COMPONENTS - 1,
                    MAX_IGNORE_FILE_COMPONENTS * 4,
                ),
            )
        });
        let usage =
            result.expect("over-budget rule with malformed bracket tail is discarded cleanly");
        assert_eq!(compiles, 0);
        assert_eq!(usage.parsed_parts, MAX_IGNORE_FILE_COMPONENTS);
        assert_eq!(usage.compiled_units, 0);

        // A single-line component explosion fails fast without compiling or
        // retaining rules.
        let exploding = "a/".repeat(MAX_IGNORE_FILE_COMPONENTS + 1);
        let mut builder = RuleSetBuilder::new(root, false, false);
        let (result, compiles) = count_pattern_compiles(|| {
            builder.add_line_within_budget(
                &exploding,
                LineBudgets::single_file(
                    MAX_IGNORE_FILE_COMPONENTS,
                    MAX_IGNORE_FILE_RETAINED_BYTES,
                ),
            )
        });
        assert_eq!(result, Err(ComponentBudgetExceeded::File));
        assert_eq!(compiles, 0);
        assert_eq!(builder.compiled_units(), 0);
        assert!(builder.build().is_empty());

        // Fuzz entry point stays bounded on the same crafted input.
        assert_eq!(fuzz_rule_bytes(exploding.as_bytes(), b"a", true), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn precomposeunicode_normalizes_only_valid_candidate_components() {
        let root = PathBuf::from("/fixture");
        let mut enabled = RuleSetBuilder::new(&root, false, true);
        enabled.add_line("caf\u{e9}.txt");
        let enabled = enabled.build();

        let decomposed = b"/fixture/cafe\xCC\x81.txt";
        assert_eq!(enabled.matched(decomposed, false), Some(true));

        // An invalid component remains byte-exact without preventing a later
        // valid component from receiving the NFC adaptation.
        assert_eq!(
            enabled.matched(b"/fixture/bad\xFF/cafe\xCC\x81.txt", false),
            Some(true)
        );

        let mut disabled = RuleSetBuilder::new(&root, false, false);
        disabled.add_line("caf\u{e9}.txt");
        assert_eq!(disabled.build().matched(decomposed, false), None);
    }

    fn match_ignorecase(rule: &[u8], candidate: &[u8]) -> Option<bool> {
        let mut builder = RuleSetBuilder::new(Path::new(""), true, false);
        builder.add_line(rule);
        builder.build().matched(candidate, false)
    }

    #[test]
    fn case_insensitive_bracket_classes_and_ranges_match_git_wildmatch() {
        // Specific high-value boundary cases verified against `git check-ignore`
        // with `core.ignoreCase = true`.
        assert_eq!(match_ignorecase(b"[a-Z]", b"a"), Some(true));
        assert_eq!(match_ignorecase(b"[a-Z]", b"A"), Some(true));
        assert_eq!(match_ignorecase(b"[a-Z]", b"m"), None);
        assert_eq!(match_ignorecase(b"[a-Z]", b"Z"), None);

        for matched in [
            b"A".as_slice(),
            b"a",
            b"Z",
            b"z",
            b"[",
            b"\\",
            b"]",
            b"^",
            b"_",
            b"`",
        ] {
            assert_eq!(
                match_ignorecase(b"[Z-a]", matched),
                if cfg!(windows) && matched == b"\\" {
                    None
                } else {
                    Some(true)
                },
                "[Z-a] under ignoreCase for {:?}",
                String::from_utf8_lossy(matched)
            );
        }
        assert_eq!(match_ignorecase(b"[Z-a]", b"b"), None);
        assert_eq!(match_ignorecase(b"[Z-a]", b"B"), None);

        assert_eq!(match_ignorecase(b"[!Z-a]", b"b"), Some(true));
        assert_eq!(match_ignorecase(b"[!Z-a]", b"B"), Some(true));
        assert_eq!(match_ignorecase(b"[!Z-a]", b"Z"), None);
        assert_eq!(match_ignorecase(b"[!Z-a]", b"z"), None);
        assert_eq!(match_ignorecase(b"[!Z-a]", b"a"), None);
        assert_eq!(match_ignorecase(b"[!Z-a]", b"["), None);

        assert_eq!(match_ignorecase(b"[!a-Z]", b"a"), None);
        assert_eq!(match_ignorecase(b"[!a-Z]", b"A"), None);
        assert_eq!(match_ignorecase(b"[!a-Z]", b"Z"), Some(true));
        assert_eq!(match_ignorecase(b"[!a-Z]", b"z"), Some(true));

        // Standalone uppercase literals in `[...]` never match in Git under
        // `WM_CASEFOLD` because Git lowercases `t_ch` before testing `t_ch == p_ch`.
        assert_eq!(match_ignorecase(b"[Z]", b"Z"), None);
        assert_eq!(match_ignorecase(b"[Z]", b"z"), None);
        assert_eq!(match_ignorecase(b"[!Z]", b"Z"), Some(true));
        assert_eq!(match_ignorecase(b"[!Z]", b"z"), Some(true));
        assert_eq!(match_ignorecase(b"[Z-/]", b"Z"), None);
        assert_eq!(match_ignorecase(b"[!Z-/]", b"Z"), Some(true));
        assert_eq!(match_ignorecase(b"[Z-Z]", b"Z"), Some(true));
        assert_eq!(match_ignorecase(b"[Z-Z]", b"z"), Some(true));
        assert_eq!(match_ignorecase(b"[\\Z-a]", b"Z"), Some(true));
        assert_eq!(match_ignorecase(b"[\\Z-a]", b"z"), Some(true));
        assert_eq!(match_ignorecase(b"[\\Z-a]", b"["), Some(true));
        assert_eq!(match_ignorecase(b"[\\Z-a]", b"b"), None);

        // Dropping an unmatchable uppercase byte or `/` must not promote `!` or
        // `^` into a negation marker, nor fuse a literal `[` with `:digit:]`
        // into a synthetic POSIX class.
        assert_eq!(match_ignorecase(b"[A!a]", b"!"), Some(true));
        assert_eq!(match_ignorecase(b"[A!a]", b"A"), Some(true));
        assert_eq!(match_ignorecase(b"[A!a]", b"b"), None);
        assert_eq!(match_ignorecase(b"[A^a]", b"^"), Some(true));
        assert_eq!(match_ignorecase(b"[A^a]", b"b"), None);
        assert_eq!(match_ignorecase(b"[[A:digit:]]", b"d]"), Some(true));
        assert_eq!(match_ignorecase(b"[[A:digit:]]", b"D]"), Some(true));
        assert_eq!(match_ignorecase(b"[[A:digit:]]", b"[]"), Some(true));
        assert_eq!(match_ignorecase(b"[[A:digit:]]", b"1"), None);
        assert_eq!(match_ignorecase(b"[[A:digit:]]", b"1]"), None);
        assert_eq!(fuzz_rule_bytes(b"[[/:digit:]]", b"d]", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[[/:digit:]]", b"[]", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[[/:digit:]]", b"1", false), None);
        assert_eq!(match_ignorecase(b"[A-[:digit:]]", b"d]"), Some(true));
        assert_eq!(match_ignorecase(b"[A-[:digit:]]", b"[]"), Some(true));
        assert_eq!(match_ignorecase(b"[A-[:digit:]]", b"a]"), Some(true));
        assert_eq!(match_ignorecase(b"[A-[:digit:]]", b"1"), None);

        // Adjacent `*` range endpoints inside `[...]` (whether originally
        // adjacent as in `[#-**-0]` or brought together by dropping uppercase
        // `A` under `ignoreCase` in `[#-*A*-0]`) must never be collapsed by
        // `collapse_partial_stars`.
        for candidate in [b"x#y".as_slice(), b"x*y", b"x+y", b"x-y", b"x.y", b"x0y"] {
            assert_eq!(
                match_ignorecase(b"x[#-*A*-0]y", candidate),
                Some(true),
                "x[#-*A*-0]y under ignoreCase for {:?}",
                String::from_utf8_lossy(candidate)
            );
            assert_eq!(
                fuzz_rule_bytes(b"x[#-**-0]y", candidate, false),
                Some(true),
                "x[#-**-0]y for {:?}",
                String::from_utf8_lossy(candidate)
            );
        }
        assert_eq!(match_ignorecase(b"x[#-*A*-0]y", b"xAy"), None);
        assert_eq!(match_ignorecase(b"x[#-*A*-0]y", b"x1y"), None);
        assert_eq!(fuzz_rule_bytes(b"x[#-**-0]y", b"x1y", false), None);
        assert_eq!(fuzz_rule_bytes(b"x[**-0]y", b"x+y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[*-*]**y", b"x*foo y", false), Some(true));

        // Deduplicating repeated standalone literals and covered ranges must
        // never disturb later ranges that reuse the same lower endpoint or
        // partially overlap (e.g. `[*a*-0]`, `[aZa-z]`, `[a-cb-d]`, `[b-da-c]`,
        // `[ba-b]`, `[a-cA-C]`).
        for candidate in [b"x*y".as_slice(), b"xay", b"x+y", b"x0y"] {
            assert_eq!(fuzz_rule_bytes(b"x[*a*-0]y", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"x[*a*-0]y", b"xby", false), None);
        assert_eq!(match_ignorecase(b"x[aZa-z]y", b"xmy"), Some(true));
        assert_eq!(match_ignorecase(b"x[aZa-z]y", b"xMy"), Some(true));
        assert_eq!(match_ignorecase(b"x[aZa-z]y", b"x0y"), None);
        for candidate in [b"a".as_slice(), b"b", b"c", b"d"] {
            assert_eq!(fuzz_rule_bytes(b"[a-cb-d]", candidate, false), Some(true));
            assert_eq!(fuzz_rule_bytes(b"[b-da-c]", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"[a-cb-d]", b"e", false), None);
        assert_eq!(fuzz_rule_bytes(b"[b-da-c]", b"e", false), None);
        assert_eq!(fuzz_rule_bytes(b"[ba-b]", b"a", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[ba-b]", b"b", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"[ba-b]", b"c", false), None);
        assert_eq!(match_ignorecase(b"[a-cA-C]", b"B"), Some(true));
        assert_eq!(match_ignorecase(b"[a-cA-C]", b"d"), None);

        // Verify `collapse_partial_stars_in_place` agrees with
        // `collapse_partial_stars` inside and outside bracket classes.
        let mut in_place = b"x[#-**-0]**[[:digit:]**]***y".to_vec();
        super::collapse_partial_stars_in_place(&mut in_place);
        assert_eq!(in_place, b"x[#-**-0]*[[:digit:]**]*y");
        assert_eq!(
            super::collapse_partial_stars(b"x[#-**-0]**[[:digit:]**]***y"),
            in_place
        );

        // Repeated literals, covered ranges, and POSIX classes compact to O(1)
        // members in both `normalize_bracket_classes` and `component_cost`.
        let bracketed_stars = format!("[{}]", "*".repeat(8_192));
        assert_eq!(
            normalize_bracket_classes(bracketed_stars.as_bytes(), false).as_deref(),
            Some(b"[*]".as_slice())
        );
        assert_eq!(
            super::component_cost(bracketed_stars.as_bytes(), false),
            super::component_cost(b"[*]", false)
        );
        let bracketed_ranges = format!("[{}]", "*-*".repeat(10_000));
        assert_eq!(
            super::component_cost(bracketed_ranges.as_bytes(), false),
            super::component_cost(b"[*]", false)
        );
        let million_az_ranges = format!("[{}]", "a-z".repeat(1_100_000));
        assert_eq!(
            normalize_bracket_classes(million_az_ranges.as_bytes(), false).as_deref(),
            Some(b"[a-z]".as_slice())
        );
        assert_eq!(
            super::component_cost(million_az_ranges.as_bytes(), false),
            super::component_cost(b"[a-z]", false)
        );
        let repeated_digits = format!("[{}]", "[:digit:]".repeat(500_000));
        assert_eq!(
            normalize_bracket_classes(repeated_digits.as_bytes(), false).as_deref(),
            Some(b"[[:digit:]]".as_slice())
        );
        assert_eq!(
            super::component_cost(repeated_digits.as_bytes(), false),
            super::component_cost(b"[[:digit:]]", false)
        );

        // Files with `keep.log` + 256 rules of `[` + 8,192 `*` + `]`, or
        // `keep.log` + `[` + 1,100,000 `a-z` + `]` + `[` + 500,000 `[:digit:]`
        // + `]` stay far below the 256 MiB retained-byte budget and keep
        // `keep.log` ignored.
        let mut star_file_builder = RuleSetBuilder::new(Path::new("/fixture"), false, false);
        let mut remaining_units = MAX_IGNORE_FILE_COMPONENTS;
        let mut remaining_bytes = MAX_IGNORE_FILE_RETAINED_BYTES;
        for line in std::iter::once("keep.log")
            .chain(std::iter::repeat_n(bracketed_stars.as_str(), 256))
            .chain([million_az_ranges.as_str(), repeated_digits.as_str()])
        {
            let usage = star_file_builder
                .add_line_within_budget(
                    line,
                    LineBudgets::single_file(remaining_units, remaining_bytes),
                )
                .expect("deduplicated bracket-class file fits comfortably within 256 MiB budget");
            remaining_units -= usage.compiled_units;
            remaining_bytes -= usage.retained_bytes;
        }
        let star_rules = star_file_builder.build();
        assert!(star_rules.retained_bytes() < 4 * 1024 * 1024);
        assert_eq!(star_rules.matched(b"/fixture/keep.log", false), Some(true));
        assert_eq!(star_rules.matched(b"/fixture/*", false), Some(true));
        assert_eq!(star_rules.matched(b"/fixture/m", false), Some(true));
        assert_eq!(star_rules.matched(b"/fixture/5", false), Some(true));

        // `[a-[:digit:][]` + 10,000 `*` + `]`, `[` + 10,000 `*-*` + `]`,
        // `[` + 1,100,000 `a-z` + `]`, `[` + 10,923 `@-a` + `]` (which splits
        // into 3 sub-ranges under `WM_CASEFOLD`), and repeated POSIX classes
        // keep `retained_bytes() >= measured` under both `case_insensitive = false`
        // and `case_insensitive = true`.
        let range_consumed_stars = format!("[a-[:digit:][]{}]", "*".repeat(10_000));
        let split_ranges = format!("[{}]", "@-a".repeat(10_923));
        let repeated_spaces = format!("[{}]", "[:space:]".repeat(10_000));
        assert_eq!(
            normalize_bracket_classes(split_ranges.as_bytes(), true).as_deref(),
            Some(b"[@a-z\\[-a]".as_slice())
        );
        assert_eq!(
            normalize_bracket_classes(repeated_spaces.as_bytes(), false).as_deref(),
            Some(b"[ \t\n\r]".as_slice())
        );
        assert_eq!(
            super::component_cost(repeated_spaces.as_bytes(), false),
            super::component_cost(b"[[:space:]]", false)
        );
        for (label, rule) in [
            ("range_consumed_stars", range_consumed_stars.as_str()),
            ("bracketed_ranges", bracketed_ranges.as_str()),
            ("bracketed_stars", bracketed_stars.as_str()),
            ("million_az_ranges", million_az_ranges.as_str()),
            ("split_ranges", split_ranges.as_str()),
            ("repeated_digits", repeated_digits.as_str()),
            ("repeated_spaces", repeated_spaces.as_str()),
        ] {
            for case_insensitive in [false, true] {
                let mut kept = None;
                let info = allocation_counter::measure(|| {
                    let mut builder =
                        RuleSetBuilder::new(Path::new("/fixture"), case_insensitive, false);
                    builder
                        .add_line_within_budget(rule, LineBudgets::UNBOUNDED)
                        .expect("memory-check rule fits unbounded budget");
                    kept = Some(builder.build());
                });
                let rules = kept.expect("RuleSet built");
                let measured = usize::try_from(info.bytes_current).expect("non-negative bytes");
                assert!(
                    rules.retained_bytes() >= measured,
                    "{label} (case_insensitive={case_insensitive}): estimate ({}) fell below measured ({measured})",
                    rules.retained_bytes()
                );
                drop(std::hint::black_box(rules));
            }
        }

        // Exhaustive check against Git's `wildmatch.c` model across all 19
        // ASCII region boundaries (positive + negated, single + range).
        let endpoints: &[u8] = b"!./0@AMZ[\\]^_`amz{";
        let candidates: Vec<u8> = (33_u8..127)
            .filter(|&b| b != b'/' && !(cfg!(windows) && b == b'\\'))
            .collect();
        let escape_endpoint = |byte: u8| -> Vec<u8> {
            if matches!(byte, b'\\' | b']' | b'-' | b'!' | b'^') {
                vec![b'\\', byte]
            } else {
                vec![byte]
            }
        };

        for &lo in endpoints {
            let lo_enc = escape_endpoint(lo);
            for negated in [false, true] {
                let mut single = Vec::new();
                single.extend_from_slice(b"x[");
                if negated {
                    single.push(b'!');
                }
                single.extend_from_slice(&lo_enc);
                single.extend_from_slice(b"]y");
                let mut builder = RuleSetBuilder::new(Path::new(""), true, false);
                builder.add_line(&single);
                let rules = builder.build();
                for &c in &candidates {
                    let t = c.to_ascii_lowercase();
                    let expected = (t == lo) != negated;
                    let actual = rules.matched(&[b'x', c, b'y'], false) == Some(true);
                    assert_eq!(
                        actual,
                        expected,
                        "single {} on {:?}",
                        String::from_utf8_lossy(&single),
                        char::from(c)
                    );
                }

                for &hi in endpoints {
                    let hi_enc = escape_endpoint(hi);
                    let mut range = Vec::new();
                    range.extend_from_slice(b"x[");
                    if negated {
                        range.push(b'!');
                    }
                    range.extend_from_slice(&lo_enc);
                    range.push(b'-');
                    range.extend_from_slice(&hi_enc);
                    range.extend_from_slice(b"]y");
                    let mut builder = RuleSetBuilder::new(Path::new(""), true, false);
                    builder.add_line(&range);
                    let rules = builder.build();
                    for &c in &candidates {
                        let t = c.to_ascii_lowercase();
                        let mut matched = t == lo;
                        if lo <= t && t <= hi {
                            matched = true;
                        } else if t.is_ascii_lowercase() {
                            let tu = t.to_ascii_uppercase();
                            if lo <= tu && tu <= hi {
                                matched = true;
                            }
                        }
                        let expected = matched != negated;
                        let actual = rules.matched(&[b'x', c, b'y'], false) == Some(true);
                        assert_eq!(
                            actual,
                            expected,
                            "range {} on {:?}",
                            String::from_utf8_lossy(&range),
                            char::from(c)
                        );
                    }
                }
            }
        }

        // Under tight compiled budgets, an unmatchable case-folded class like
        // `[Z]` is discarded cleanly without rolling back earlier rules.
        let mut tight = RuleSetBuilder::new(Path::new("/fixture"), true, false);
        let usage = tight
            .add_line_within_budget(
                "[Z]",
                LineBudgets {
                    file_parsed_remaining: 10,
                    file_compiled_remaining: 1,
                    file_bytes_remaining: usize::MAX,
                    chain_compiled_remaining: 1,
                    chain_bytes_remaining: usize::MAX,
                },
            )
            .expect("empty case-folded class is discarded cleanly under tight budget");
        assert_eq!(usage.parsed_parts, 1);
        assert_eq!(usage.compiled_units, 0);
    }

    #[test]
    fn posix_character_classes_enforce_git_twelve_class_set_and_abort_semantics() {
        // Every one of Git's 12 POSIX classes works.
        for (rule, matched, rejected) in [
            ("[[:alnum:]]", b"a".as_slice(), b"-".as_slice()),
            ("[[:alpha:]]", b"Z", b"0"),
            ("[[:blank:]]", b"\t", b"a"),
            ("[[:cntrl:]]", b"\x07", b"a"),
            ("[[:digit:]]", b"9", b"a"),
            ("[[:graph:]]", b"!", b" "),
            ("[[:lower:]]", b"z", b"Z"),
            ("[[:print:]]", b" ", b"\x07"),
            ("[[:punct:]]", b"!", b"a"),
            ("[[:space:]]", b" ", b"a"),
            ("[[:upper:]]", b"A", b"a"),
            ("[[:xdigit:]]", b"f", b"g"),
        ] {
            assert_eq!(fuzz_rule(rule, matched, false), Some(true), "{rule}");
            assert_eq!(fuzz_rule(rule, rejected, false), None, "{rule}");
        }
        assert_eq!(match_ignorecase(b"[[:upper:]]", b"a"), Some(true));
        assert_eq!(match_ignorecase(b"[[:lower:]]", b"A"), Some(true));

        // Git's `[:space:]` uses `sane-ctype.h` (`' '`, `'\t'`, `'\n'`, `'\r'`)
        // and excludes `\x0B` (VT) and `\x0C` (FF).
        for byte in *b" \t\n\r" {
            assert_eq!(
                fuzz_rule_bytes(b"x[[:space:]]y", &[b'x', byte, b'y'], false),
                Some(true)
            );
            assert_eq!(
                fuzz_rule_bytes(b"x[![:space:]]y", &[b'x', byte, b'y'], false),
                None
            );
        }
        for byte in [0x0b_u8, 0x0c_u8] {
            assert_eq!(
                fuzz_rule_bytes(b"x[[:space:]]y", &[b'x', byte, b'y'], false),
                None
            );
            assert_eq!(
                fuzz_rule_bytes(b"x[![:space:]]y", &[b'x', byte, b'y'], false),
                Some(true)
            );
        }
        assert_eq!(
            fuzz_rule_bytes(b"x[[:a]/[[:space:]]", b"x[/ ", false),
            Some(true)
        );
        assert_eq!(
            fuzz_rule_bytes(b"x[[:a]/[[:space:]]", b"x[/\x0c", false),
            None
        );
        assert_eq!(
            fuzz_rule_bytes(b"x[[:space:]-a]y", b"x-y", false),
            Some(true)
        );

        // Unknown and non-Git POSIX classes abort the entire rule (Git's
        // `WM_ABORT_ALL`), even when negated or paired with valid members.
        for rule in [
            b"[[:bogus:]]".as_slice(),
            b"[![:bogus:]]",
            b"[a[:bogus:]]",
            b"[!a[:bogus:]]",
            b"[[:bogus:]a]",
            b"[[:ascii:]]",
            b"[![:ascii:]]",
            b"[[:word:]]",
            b"[![:word:]]",
            b"[[:a/b:]]",
            b"[![:a/b:]]",
            b"[[::]]",
            b"[![::]]",
            b"[[:DIGIT:]]",
            b"[[:ALPHA:]]",
            b"[[:foo[:digit:]]",
            b"[[:alpha\\:]]",
            b"*[[:bogus:]]*",
            b"a/[[:bogus:]]",
        ] {
            assert!(
                parse_rule(rule).is_none(),
                "invalid/non-Git POSIX class must be rejected: {}",
                String::from_utf8_lossy(rule)
            );
            assert_eq!(
                match_ignorecase(rule, b"a"),
                None,
                "invalid/non-Git POSIX class must stay rejected under ignoreCase: {}",
                String::from_utf8_lossy(rule)
            );
        }

        // `[[:a/b:]]` must never pass an unstripped `/` into `ferralk-glob`.
        assert!(normalize_bracket_classes(b"[[:a/b:]]", false).is_none());
        assert!(normalize_bracket_classes(b"[[:a/b:]]", true).is_none());

        // Without `:]` before the first `]`, Git falls back to treating `[` as a
        // literal class member.
        for candidate in [b"x[y".as_slice(), b"x:y", b"xay", b"xly"] {
            assert_eq!(
                fuzz_rule_bytes(b"x[[:alpha]y", candidate, false),
                Some(true)
            );
        }
        assert_eq!(fuzz_rule_bytes(b"x[[:alpha]y", b"xby", false), None);
        assert_eq!(fuzz_rule_bytes(b"x[[:]y", b"x[y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[[:]y", b"x:y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[[:]y", b"x]y", false), None);
        assert_eq!(fuzz_rule_bytes(b"x[[:alpha", b"xa", false), None);

        // Escaped `]` before `:]` causes the `[:` check to stop at that `]`,
        // see `\` != `:`, and treat `[` as literal while `\]` stays a member.
        for candidate in [b"x[y".as_slice(), b"x:y", b"xay", b"x]y", b"xby"] {
            assert_eq!(
                fuzz_rule_bytes(b"x[[:a\\]b]y", candidate, false),
                Some(true)
            );
        }

        // Component splitting stops at the first `]` of a non-POSIX `[:foo]`
        // and honors `[` consumed as a range upper endpoint (`a-[:digit:]`).
        assert_eq!(
            fuzz_rule_bytes(b"x[[:foo]/bar[[:digit:]]]y", b"x[/bar0]y", false),
            Some(true)
        );
        assert_eq!(
            fuzz_rule_bytes(b"x[[:foo]/bar[[:digit:]]]y", b"xf/bar9]y", false),
            Some(true)
        );
        assert_eq!(fuzz_rule_bytes(b"x[[:a/b]c]y", b"xac]y", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"x[[:a/b]c]y", b"x[c]y", false), Some(true));
        for candidate in [b"xa/bar]y".as_slice(), b"xd/bar]y", b"x:/bar]y"] {
            assert_eq!(
                fuzz_rule_bytes(b"x[a-[:digit:]/bar]y", candidate, false),
                Some(true)
            );
        }
        assert_eq!(
            fuzz_rule_bytes(b"x[a-[:digit:]/bar]y", b"x0/bar]y", false),
            None
        );
    }

    /// What one verdict costs, both engines, in one process.
    ///
    /// A walk is syscall-bound, so measuring the engine through it reports the
    /// machine's mood rather than the matcher. This asks the two matchers the
    /// same questions back to back instead. It is a measurement, not a verdict,
    /// so it stays out of the suite:
    ///
    /// ```text
    /// cargo test -p ferralk --release -- --ignored --nocapture rule_engine_cost
    /// ```
    #[test]
    #[ignore = "measurement, not a verdict"]
    fn rule_engine_cost() {
        measure_rule_engine_cost("case-sensitive", false);
        measure_rule_engine_cost("ignoreCase+precomposeUnicode", true);
    }

    fn measure_rule_engine_cost(mode: &str, ignore_case: bool) {
        use std::time::Instant;

        let root = PathBuf::from("/fixture");
        let candidates = (0..64)
            .map(|index| candidate(&root, &format!("src/area-{index}/module/file-{index}.txt")))
            .collect::<Vec<_>>();

        for count in [1_usize, 10, 120] {
            let lines = (0..count)
                .map(|index| match index % 3 {
                    0 => format!("build-{index}/"),
                    1 => format!("*.tmp{index}"),
                    _ => format!("**/cache-{index}/**"),
                })
                .collect::<Vec<_>>();

            let mut ours = RuleSetBuilder::new(&root, ignore_case, ignore_case);
            for line in &lines {
                ours.add_line(line);
            }
            let ours = ours.build();

            let mut theirs = ignore::gitignore::GitignoreBuilder::new(&root);
            theirs
                .case_insensitive(ignore_case)
                .expect("case option is valid");
            for line in &lines {
                let _ = theirs.add_line(None, line);
            }
            let theirs = theirs.build().expect("the previous engine builds");

            let rounds = 5_000;
            let mut scratch = IgnoreMatchScratch::default();
            let start = Instant::now();
            for _ in 0..rounds {
                for path in &candidates {
                    let mut ctx = EntryMatchContext::new(path, &mut scratch);
                    std::hint::black_box(ours.matched_in_context(&mut ctx, false));
                }
            }
            let ours_ns = start.elapsed().as_nanos() as f64 / (rounds * candidates.len()) as f64;

            let start = Instant::now();
            for _ in 0..rounds {
                for path in &candidates {
                    std::hint::black_box(theirs.matched(
                        std::str::from_utf8(path).expect("ASCII fixture path"),
                        false,
                    ));
                }
            }
            let theirs_ns = start.elapsed().as_nanos() as f64 / (rounds * candidates.len()) as f64;

            println!(
                "{mode}, {count:>3} rules: ours {ours_ns:6.1} ns/verdict   previous engine {theirs_ns:6.1} ns/verdict"
            );
        }
    }

    #[test]
    fn case_insensitive_literal_gates_stay_active_and_fold_lazily() {
        let mut root_builder = RuleSetBuilder::new(Path::new("/Users/Sebastian/repo"), true, false);
        for line in [
            // Complex wildcard and multi-component rules keep active
            // `memmem::Finder` gates under `core.ignoreCase = true`.
            "*Cache-*.BIN*",
            "[0-9]*Report*.LOG",
            "a?b-marker",
            "**/vendor/Bundle-*.JS",
            "src/**/generated-42",
            // Simple single-component fast-path rules and 2-component
            // `AnchoredLiteralFirst` skip redundant/unused gates under
            // `core.ignoreCase = true`.
            "*.LOG",
            "Node_Modules/",
            "/build-out",
            "dist/Bundle-*.JS",
            "*",
        ] {
            root_builder.add_line(line);
        }
        let root_rules = root_builder.build();
        assert_eq!(root_rules.rules.len(), 10);
        for rule in &root_rules.rules[..5] {
            assert!(
                rule.gate.is_some(),
                "expected active memmem gate for complex/multi rule under case_insensitive=true"
            );
        }
        for rule in &root_rules.rules[5..] {
            assert!(
                rule.gate.is_none(),
                "expected no redundant/unused memmem gate for simple/2-comp literal-first rule under case_insensitive=true"
            );
        }

        // Check accuracy across ASCII lowercase, uppercase, case-neutral, and
        // non-ASCII bytes (Git's WM_CASEFOLD is strictly ASCII-only).
        let mut builder = RuleSetBuilder::new(Path::new("/Users/Sebastian/repo"), true, false);
        for line in [
            "*.LOG",
            "Node_Modules/",
            "---123---",
            "ÄPFEL.txt",
            "!Keep.LOG",
            "dist/Bundle-*.JS",
            "**/vendor/Bundle-*.JS",
        ] {
            builder.add_line(line);
        }
        let rules = builder.build();
        assert_eq!(
            rules.matched(b"/Users/Sebastian/repo/a/b/app.log", false),
            Some(true)
        );
        assert_eq!(
            rules.matched(b"/Users/Sebastian/repo/a/b/APP.LOG", false),
            Some(true)
        );
        assert_eq!(
            rules.matched(b"/Users/Sebastian/repo/a/b/keep.log", false),
            Some(false)
        );
        assert_eq!(
            rules.matched(b"/Users/Sebastian/repo/a/b/KEEP.Log", false),
            Some(false)
        );
        assert_eq!(
            rules.matched(b"/Users/Sebastian/repo/a/node_modules", true),
            Some(true)
        );
        assert_eq!(
            rules.matched(b"/Users/Sebastian/repo/a/NODE_MODULES", false),
            None
        );
        assert_eq!(
            rules.matched(b"/Users/Sebastian/repo/a/---123---", false),
            Some(true)
        );
        assert_eq!(
            rules.matched("/Users/Sebastian/repo/a/Äpfel.TXT".as_bytes(), false),
            Some(true)
        );
        // Non-ASCII `ä` does NOT match `Ä` under Git's ASCII-only WM_CASEFOLD.
        assert_eq!(
            rules.matched("/Users/Sebastian/repo/a/äpfel.TXT".as_bytes(), false),
            None
        );
        assert_eq!(
            rules.matched(b"/Users/Sebastian/repo/DIST/bundle-1.js", false),
            Some(true)
        );
        assert_eq!(
            rules.matched(b"/Users/Sebastian/repo/a/b/plain.rs", false),
            None
        );

        // Multi-level nested rule sets sharing one `EntryMatchContext`:
        // 1) Pure lowercase relative candidate under an uppercase walk root
        //    never folds.
        // 2) Uppercase relative candidate folds AT MOST ONCE across all 3 sets
        //    and performs zero allocations once scratch is warm.
        let mut mid_builder =
            RuleSetBuilder::new(Path::new("/Users/Sebastian/repo/apps"), true, false);
        mid_builder.add_line("*.TMP");
        mid_builder.add_line("sub/Cache-*.BIN");
        let mid_rules = mid_builder.build();

        let mut leaf_builder =
            RuleSetBuilder::new(Path::new("/Users/Sebastian/repo/apps/sub"), true, false);
        leaf_builder.add_line("*.BAK");
        let leaf_rules = leaf_builder.build();

        let mut scratch = IgnoreMatchScratch::default();
        // Warm scratch once with an uppercase path.
        {
            let mut ctx = EntryMatchContext::new(
                b"/Users/Sebastian/repo/apps/sub/WarmUpper.TXT",
                &mut scratch,
            );
            assert_eq!(leaf_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(mid_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(rules.matched_in_context(&mut ctx, false), None);
        }

        // Lowercase relative path under uppercase walk root: 0 folds, 0 allocs.
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        let lower_allocs = allocation_counter::measure(|| {
            let mut ctx =
                EntryMatchContext::new(b"/Users/Sebastian/repo/apps/sub/plain.rs", &mut scratch);
            assert_eq!(leaf_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(mid_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(rules.matched_in_context(&mut ctx, false), None);
        });
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "lowercase relative path must not case-fold even under uppercase walk root"
        );
        assert_eq!(lower_allocs.count_total, 0);

        // Uppercase relative path across all 3 levels: each level folds only
        // its own needed suffix (never the `/Users/Sebastian/repo/` root
        // prefix), with zero allocations once scratch is warm.
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        let upper_allocs = allocation_counter::measure(|| {
            let mut ctx = EntryMatchContext::new(
                b"/Users/Sebastian/repo/apps/sub/PlainUpper.RS",
                &mut scratch,
            );
            assert_eq!(leaf_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(mid_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(
                ctx.folded_len(),
                "apps/sub/PlainUpper.RS".len(),
                "Multi rules must fold only from rel_start, never the walk root prefix"
            );
        });
        assert!(super::CASE_FOLD_CALLS.with(|c| c.get()) <= 3);
        assert_eq!(upper_allocs.count_total, 0);

        // Long 800-byte root prefix + unanchored multi-component rule
        // `**/obj/*.o` on `src/T.c`: must fold ONLY the 7 bytes of `src/T.c`,
        // never the 800-byte root prefix.
        let long_root = format!("/{}", "LongRootDir/".repeat(67));
        let mut long_builder = RuleSetBuilder::new(Path::new(&long_root), true, false);
        long_builder.add_line("**/obj/*.o");
        let long_rules = long_builder.build();
        let long_candidate = format!("{long_root}src/T.c");
        {
            let mut ctx = EntryMatchContext::new(long_candidate.as_bytes(), &mut scratch);
            assert_eq!(long_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(
                ctx.folded_len(),
                "src/T.c".len(),
                "UnanchoredMulti rules under an 800-byte root must fold only the 7-byte relative candidate"
            );
        }

        // Depth-128 path with 100 anchored `RootSingle` rules (`/build-N`)
        // must skip every rule BEFORE folding or gating (`CASE_FOLD_CALLS == 0`).
        let mut anchored_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        for index in 0..100 {
            anchored_builder.add_line(format!("/build-{index}"));
        }
        let anchored_rules = anchored_builder.build();
        let deep_upper_path = format!("/repo/{}UPPER.TXT", "Dir/".repeat(128));
        let deep_lower_base = format!("/repo/{}plain.rs", "Dir/".repeat(128));
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        let anchored_allocs = allocation_counter::measure(|| {
            let mut ctx = EntryMatchContext::new(deep_upper_path.as_bytes(), &mut scratch);
            assert_eq!(anchored_rules.matched_in_context(&mut ctx, false), None);
        });
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "RootSingle rules on nested paths must reject before folding"
        );
        assert_eq!(anchored_allocs.count_total, 0);

        // Depth-128 path (`src/.../UPPER.rs`) with anchored multi-component
        // rules (`target/*.txt`, `target/**/*.txt`) must reject on the first
        // component BEFORE folding or gating (`CASE_FOLD_CALLS == 0`), while
        // still matching valid `TARGET/file.TXT` and `TARGET/a/b/file.TXT`.
        let mut anchored_multi_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        for index in 0..100 {
            anchored_multi_builder.add_line(format!("target-{index}/*.txt"));
        }
        anchored_multi_builder.add_line("target/*.txt");
        anchored_multi_builder.add_line("target/deep/**/*.bin");
        let anchored_multi_rules = anchored_multi_builder.build();
        let deep_src_path = format!("/repo/{}UPPER.rs", "src/".repeat(128));
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        let anchored_multi_allocs = allocation_counter::measure(|| {
            let mut ctx = EntryMatchContext::new(deep_src_path.as_bytes(), &mut scratch);
            assert_eq!(
                anchored_multi_rules.matched_in_context(&mut ctx, false),
                None
            );
        });
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "AnchoredMulti rules must reject mismatched first components before folding"
        );
        assert_eq!(anchored_multi_allocs.count_total, 0);
        assert_eq!(
            anchored_multi_rules.matched(b"/repo/TARGET/file.TXT", false),
            Some(true)
        );
        assert_eq!(
            anchored_multi_rules.matched(b"/repo/TARGET/sub/file.TXT", false),
            None
        );
        assert_eq!(anchored_multi_rules.matched(b"/repo/TARGET", false), None);
        assert_eq!(
            anchored_multi_rules.matched(b"/repo/TARGET/DEEP/a/b/out.BIN", false),
            Some(true)
        );

        // Fixed-depth rules with universal leading `*` components
        // (`*/generated-42.txt`, `*/generated-42.?xt`) use
        // `RuleKind::FixedDepthBasename` (matching the basename directly
        // without folding simple tails, and gating complex tails strictly on
        // the basename); anchored rules whose leading non-`AnyDirs` prefix has
        // at least one non-universal component (`target/*.txt`,
        // `build-*/artifact-*.o`, `pkg[0-9]/specificname-42.log`,
        // `src/x/generated*`, `*/build-0/**/generated-output.txt`,
        // `*/a*/generated-42.txt`, `lit\*/file.txt`) use
        // `RuleKind::AnchoredFastPrefix`; unanchored multi-component rules
        // (`**/vendor/Bundle-*.JS`) stay `RuleKind::Multi` with an active
        // folded gate.
        let mut kind_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        kind_builder.add_line("*/generated-42.txt");
        kind_builder.add_line("target/*.txt");
        kind_builder.add_line("build-*/artifact-*.o");
        kind_builder.add_line("pkg[0-9]/specificname-42.log");
        kind_builder.add_line("src/x/generated*");
        kind_builder.add_line(r"lit\*/file.txt");
        kind_builder.add_line("*/build-0/**/generated-output.txt");
        kind_builder.add_line("*/generated-42.?xt");
        kind_builder.add_line("**/vendor/Bundle-*.JS");
        let kind_rules = kind_builder.build();
        assert_eq!(
            kind_rules.rules[0].kind,
            super::RuleKind::FixedDepthBasename
        );
        assert_eq!(
            super::unpack_pair_meta(kind_rules.rules[0].fast_meta),
            (2, Some(16))
        );
        assert!(kind_rules.rules[0].gate.is_none());
        assert_eq!(
            kind_rules.rules[1].kind,
            super::RuleKind::AnchoredFastPrefix
        );
        assert_eq!(
            kind_rules.rules[2].kind,
            super::RuleKind::AnchoredFastPrefix
        );
        assert_eq!(
            kind_rules.rules[3].kind,
            super::RuleKind::AnchoredFastPrefix
        );
        assert_eq!(
            kind_rules.rules[4].kind,
            super::RuleKind::AnchoredFastPrefix
        );
        assert_eq!(
            super::unpack_pair_meta(kind_rules.rules[4].fast_meta),
            (3, Some(3))
        );
        assert!(kind_rules.rules[4].gate.is_none());
        assert_eq!(
            kind_rules.rules[5].kind,
            super::RuleKind::AnchoredFastPrefix
        );
        assert_eq!(
            kind_rules.rules[6].kind,
            super::RuleKind::AnchoredFastPrefix
        );
        assert_eq!(
            super::unpack_pair_meta(kind_rules.rules[6].fast_meta),
            (2, None)
        );
        assert!(kind_rules.rules[6].gate.is_some());
        assert_eq!(
            kind_rules.rules[7].kind,
            super::RuleKind::FixedDepthBasename
        );
        assert_eq!(
            super::unpack_pair_meta(kind_rules.rules[7].fast_meta),
            (2, Some(16))
        );
        assert!(
            kind_rules.rules[7].gate.is_some() && kind_rules.rules[7].gate_needs_fold,
            "corpus rule `*/generated-42.?xt` must keep an active folded gate under case_insensitive=true"
        );
        assert_eq!(kind_rules.rules[8].kind, super::RuleKind::Multi);
        assert!(kind_rules.rules[8].gate.is_some() && kind_rules.rules[8].gate_needs_fold);
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        {
            let mut b_only = RuleSetBuilder::new(Path::new("/repo"), true, false);
            b_only.add_line("*/generated-42.txt");
            let r_only = b_only.build();
            let mut ctx = EntryMatchContext::new(b"/repo/any/GENERATED-42.TXT", &mut scratch);
            assert_eq!(r_only.matched_in_context(&mut ctx, false), Some(true));
        }
        {
            let mut b_star_lead = RuleSetBuilder::new(Path::new("/repo"), true, false);
            b_star_lead.add_line("*/build-0/**/generated-output.txt");
            let r_star_lead = b_star_lead.build();
            let mut ctx = EntryMatchContext::new(deep_src_path.as_bytes(), &mut scratch);
            assert_eq!(r_star_lead.matched_in_context(&mut ctx, false), None);
        }
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "FixedDepthBasename (`*/generated-42.txt`) and leading-star AnchoredFastPrefix (`*/build-0/**/...`) must not fold"
        );
        assert_eq!(
            kind_rules.matched(b"/repo/any/BUILD-0/x/y/GENERATED-OUTPUT.TXT", false),
            Some(true)
        );
        assert_eq!(
            kind_rules.matched(b"/repo/any/GENERATED-43.TXT", false),
            None
        );
        assert_eq!(
            kind_rules.matched(b"/repo/any/sub/GENERATED-42.TXT", false),
            None
        );
        assert_eq!(
            kind_rules.matched(b"/repo/any/GENERATED-42.TXT", false),
            Some(true)
        );
        assert_eq!(
            kind_rules.matched(b"/repo/BUILD-OUT/ARTIFACT-1.O", false),
            Some(true)
        );
        assert_eq!(
            kind_rules.matched(b"/repo/PKG3/SPECIFICNAME-42.LOG", false),
            Some(true)
        );
        assert_eq!(
            kind_rules.matched(b"/repo/SRC/X/GENERATED-1", false),
            Some(true)
        );
        assert_eq!(
            kind_rules.matched(b"/repo/SRC/X/sub/GENERATED-1", false),
            None
        );
        assert_eq!(
            kind_rules.matched(b"/repo/LIT*/FILE.TXT", false),
            Some(true)
        );
        let mut multi_prefix_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        multi_prefix_builder.add_line("pkg[0-9]/specificname-42.log");
        multi_prefix_builder.add_line("src/x/generated*");
        let multi_prefix_rules = multi_prefix_builder.build();
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        {
            let mut ctx = EntryMatchContext::new(deep_src_path.as_bytes(), &mut scratch);
            assert_eq!(multi_prefix_rules.matched_in_context(&mut ctx, false), None);
        }
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "AnchoredFastPrefix (`pkg[0-9]/...`, `src/x/generated*`) must reject mismatched prefix components without folding"
        );

        // Case-neutral gate (`**/2026/*.o`): needle `"2026"` has no ASCII
        // letters so `gate_needs_fold == false` and uppercase candidates never
        // fold.
        let mut neutral_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        neutral_builder.add_line("**/2026/*.o");
        let neutral_rules = neutral_builder.build();
        assert!(neutral_rules.rules[0].gate.is_some());
        assert!(!neutral_rules.rules[0].gate_needs_fold);
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        {
            let mut ctx = EntryMatchContext::new(b"/repo/UPPER/FILE.TXT", &mut scratch);
            assert_eq!(neutral_rules.matched_in_context(&mut ctx, false), None);
        }
        {
            let mut ctx = EntryMatchContext::new(b"/repo/UPPER/2026/FILE.O", &mut scratch);
            assert_eq!(
                neutral_rules.matched_in_context(&mut ctx, false),
                Some(true)
            );
        }
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "case-neutral gate (`**/2026/*.o`) must never fold uppercase paths"
        );

        // Fixed-width `RootSingle` / `Basename` rules (`/artifact-???.tar`,
        // `img-[0-9][0-9].png`): reject wrong-length uppercase candidates by
        // byte length BEFORE folding or gating (`CASE_FOLD_CALLS == 0`).
        let mut fixed_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        fixed_builder.add_line("/artifact-???.tar");
        fixed_builder.add_line("img-[0-9][0-9].png");
        let fixed_rules = fixed_builder.build();
        assert_eq!(
            fixed_rules.rules[0].fast_meta,
            std::num::NonZeroU32::new(16)
        );
        assert_eq!(
            fixed_rules.rules[1].fast_meta,
            std::num::NonZeroU32::new(10)
        );
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        {
            let mut ctx =
                EntryMatchContext::new(b"/repo/LONG_UPPERCASE_FILENAME_32_BYTES", &mut scratch);
            assert_eq!(fixed_rules.matched_in_context(&mut ctx, false), None);
        }
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "fixed-width RootSingle/Basename rules must reject wrong-length uppercase candidates before folding"
        );
        assert_eq!(
            fixed_rules.matched(b"/repo/ARTIFACT-123.TAR", false),
            Some(true)
        );
        assert_eq!(
            fixed_rules.matched(b"/repo/sub/IMG-42.PNG", false),
            Some(true)
        );

        // Fixed-depth `Multi` (`*/output0`) and `AnchoredFastPrefix`
        // (`src/*/output0`) rules must reject wrong-depth uppercase candidates
        // by component count BEFORE folding or gating (`CASE_FOLD_CALLS == 0`).
        let mut depth_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        for index in 0..20 {
            depth_builder.add_line(format!("*/output{index}"));
            depth_builder.add_line(format!("src/*/output{index}"));
        }
        let depth_rules = depth_builder.build();
        assert_eq!(
            depth_rules.rules[0].kind,
            super::RuleKind::FixedDepthBasename
        );
        assert_eq!(
            super::unpack_pair_meta(depth_rules.rules[0].fast_meta),
            (2, Some(7))
        );
        assert_eq!(
            depth_rules.rules[1].kind,
            super::RuleKind::AnchoredFastPrefix
        );
        assert_eq!(
            super::unpack_pair_meta(depth_rules.rules[1].fast_meta),
            (3, Some(3))
        );
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        {
            let mut ctx = EntryMatchContext::new(deep_src_path.as_bytes(), &mut scratch);
            assert_eq!(depth_rules.matched_in_context(&mut ctx, false), None);
        }
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "fixed-depth Multi and AnchoredFastPrefix rules must reject wrong-depth candidates before folding"
        );
        assert_eq!(depth_rules.matched(b"/repo/any/OUTPUT0", false), Some(true));
        assert_eq!(
            depth_rules.matched(b"/repo/SRC/mid/OUTPUT0", false),
            Some(true)
        );
        assert_eq!(depth_rules.matched(b"/repo/SRC/a/b/OUTPUT0", false), None);

        // Progressive resumable component counter: mixed fixed depths (k=2,
        // k=5, k=3) in any rule order agree with `matches_components` across
        // depths 0, 1, 2, 3, 5, and 24 (including trailing/double slashes) and
        // stop scanning early on long 24-level paths.
        let mixed_orders: [&[&str]; 3] = [
            &["*/hit.txt", "*/*/*/*/hit.txt", "*/*/hit.txt"],
            &["*/*/*/*/hit.txt", "*/hit.txt", "*/*/hit.txt"],
            &["*/*/hit.txt", "*/*/*/*/hit.txt", "*/hit.txt"],
        ];
        let seg = "z".repeat(145);
        let deep24_path = format!("/repo/{}HIT.TXT", format!("{seg}/").repeat(23));
        let depth_candidates: Vec<(&[u8], Option<bool>)> = vec![
            (b"/repo/", None),
            (b"/repo/HIT.TXT", None),
            (b"/repo/a/HIT.TXT", Some(true)),
            (b"/repo/a/HIT.TXT/", Some(true)),
            (b"/repo/a/HIT.TXT//", None),
            (b"/repo/a/b/HIT.TXT", Some(true)),
            (b"/repo/a/b/c/HIT.TXT", None),
            (b"/repo/a/b/c/d/HIT.TXT", Some(true)),
            (b"/repo/a/b/c/d/e/HIT.TXT", None),
            (deep24_path.as_bytes(), None),
        ];
        for ci in [false, true] {
            for order in mixed_orders {
                let mut b = RuleSetBuilder::new(Path::new("/repo"), ci, false);
                for rule in order {
                    b.add_line(rule);
                }
                let set = b.build();
                for &(cand, expected_ci) in &depth_candidates {
                    let expected = if ci { expected_ci } else { None };
                    assert_eq!(
                        set.matched(cand, false),
                        expected,
                        "mixed-depth mismatch (ci={ci}, order={order:?}) on {:?}",
                        String::from_utf8_lossy(cand)
                    );
                }
                // Lowercase exact hits match under both `ci=false` and `ci=true`.
                assert_eq!(set.matched(b"/repo/a/hit.txt", false), Some(true));
                assert_eq!(set.matched(b"/repo/a/b/hit.txt", false), Some(true));
                assert_eq!(set.matched(b"/repo/a/b/c/d/hit.txt", false), Some(true));
                assert_eq!(set.matched(b"/repo/a/b/c/hit.txt", false), None);
            }
        }
        // Verify `ComponentCounter` stops at the 2nd `/` on a 3.5 KB 24-level
        // path when asked for `needed = 2` (never scanning all 3.5 KB).
        let rel24 = &deep24_path.as_bytes()[b"/repo/".len()..];
        let mut counter = super::ComponentCounter::default();
        assert!(!counter.matches_exact(rel24, 2));
        assert_eq!(counter.components, 3);
        assert_eq!(counter.scanned, 2 * (145 + 1));
        assert!(counter.scanned < 400 && rel24.len() > 3000);
        // Resuming for `needed = 5` advances only to the 5th `/` without rescanning.
        assert!(!counter.matches_exact(rel24, 5));
        assert_eq!(counter.components, 6);
        assert_eq!(counter.scanned, 5 * (145 + 1));
        // Asking for `needed = 3` does not advance further.
        assert!(!counter.matches_exact(rel24, 3));
        assert_eq!(counter.components, 6);
        assert_eq!(counter.scanned, 5 * (145 + 1));

        // 2-component `AnchoredLiteralFirst` (`generated/*.o`) must NOT fold
        // or run a redundant `components[0]` gate after `generated` matches;
        // 3+ component `AnchoredLiteralFirst` draws its gate strictly from
        // `parts[1..]` so uppercase `components[0]` does not trigger folding.
        let mut tail_gate_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        tail_gate_builder.add_line("generated/*.o");
        tail_gate_builder.add_line("generated_dir/**/a");
        tail_gate_builder.add_line("src/**/generated-42");
        let tail_gate_rules = tail_gate_builder.build();
        assert!(tail_gate_rules.rules[0].gate.is_none());
        assert!(tail_gate_rules.rules[1].gate.is_none());
        assert!(tail_gate_rules.rules[2].gate.is_some());
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        {
            let mut ctx = EntryMatchContext::new(b"/repo/generated/SomeFile.rs", &mut scratch);
            assert_eq!(tail_gate_rules.matched_in_context(&mut ctx, false), None);
        }
        {
            let mut ctx = EntryMatchContext::new(b"/repo/SRC/sub/main.rs", &mut scratch);
            assert_eq!(tail_gate_rules.matched_in_context(&mut ctx, false), None);
        }
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "AnchoredLiteralFirst must not fold for 2-component rules or uppercase component 0 when tail is lowercase"
        );
        assert_eq!(
            tail_gate_rules.matched(b"/repo/GENERATED/out.O", false),
            Some(true)
        );
        assert_eq!(
            tail_gate_rules.matched(b"/repo/SRC/sub/GENERATED-42", false),
            Some(true)
        );

        // Under `case_insensitive = false`, long-path `AnchoredFastPrefix`
        // rules with a wildcard prefix (`a????*/unmatched-output-{i}.tmp`) must
        // check `rule.gate` before running `matches_components`.
        let mut cs_wildcard_prefix = RuleSetBuilder::new(Path::new("/repo"), false, false);
        for index in 0..16 {
            cs_wildcard_prefix.add_line(format!("a????*/unmatched-output-{index}.tmp"));
        }
        let cs_wildcard_rules = cs_wildcard_prefix.build();
        assert!(
            cs_wildcard_rules
                .rules
                .iter()
                .all(|rule| rule.kind == super::RuleKind::AnchoredFastPrefix && rule.gate.is_some())
        );
        let long_miss = format!("/repo/a{}/foo.rs", "x".repeat(255));
        let long_hit = format!("/repo/a{}/unmatched-output-7.tmp", "x".repeat(255));
        super::FULL_COMPONENT_MATCHES.with(|c| c.set(0));
        assert_eq!(cs_wildcard_rules.matched(long_miss.as_bytes(), false), None);
        assert_eq!(
            super::FULL_COMPONENT_MATCHES.with(|c| c.get()),
            0,
            "case-sensitive AnchoredFastPrefix must reject via rule.gate before matches_components"
        );
        assert_eq!(
            cs_wildcard_rules.matched(long_hit.as_bytes(), false),
            Some(true)
        );

        // Under `case_insensitive = true`, multi-rule fixed-depth
        // `AnchoredFastPrefix` sets (`src/*/output-{i}`) must reject
        // wrong-depth candidates via `ComponentCounter` BEFORE matching
        // leading components (zero folds and zero full component matches).
        let mut ci_fixed_anchored = RuleSetBuilder::new(Path::new("/repo"), true, false);
        for index in 0..16 {
            ci_fixed_anchored.add_line(format!("src/*/output-{index}"));
        }
        let ci_fixed_anchored_rules = ci_fixed_anchored.build();
        assert!(
            ci_fixed_anchored_rules
                .rules
                .iter()
                .all(|rule| rule.kind == super::RuleKind::AnchoredFastPrefix
                    && super::unpack_pair_meta(rule.fast_meta) == (3, Some(3)))
        );
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        super::FULL_COMPONENT_MATCHES.with(|c| c.set(0));
        assert_eq!(
            ci_fixed_anchored_rules.matched(b"/repo/src/a/b/c/d/file.txt", false),
            None
        );
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "fixed-depth AnchoredFastPrefix must not fold on wrong-depth miss"
        );
        assert_eq!(
            super::FULL_COMPONENT_MATCHES.with(|c| c.get()),
            0,
            "fixed-depth AnchoredFastPrefix must reject wrong-depth candidates before component matching"
        );
        assert_eq!(
            ci_fixed_anchored_rules.matched(b"/repo/SRC/sub/OUTPUT-7", false),
            Some(true)
        );
        assert_eq!(
            ci_fixed_anchored_rules.matched(b"/repo/SRC/sub/OUTPUT-7/extra", false),
            None
        );

        // `fuzz_rule_bytes` preserves platform separator semantics for
        // `Component::AnyOne` (`/*`): `\` is a separator on Windows and a
        // normal filename byte on Unix.
        #[cfg(windows)]
        assert_eq!(super::fuzz_rule_bytes(b"/*", br"dir\file", false), None);
        #[cfg(not(windows))]
        assert_eq!(
            super::fuzz_rule_bytes(b"/*", br"dir\file", false),
            Some(true)
        );

        // Depth-128 path with 100 simple `Basename` rules (`*.TMP{i}`):
        // direct fast path in `Pattern` skips folding completely even on
        // uppercase basenames (`CASE_FOLD_CALLS == 0`).
        let mut simple_basename_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        for index in 0..100 {
            simple_basename_builder.add_line(format!("*.TMP{index}"));
        }
        let simple_basename_rules = simple_basename_builder.build();
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        {
            let mut ctx = EntryMatchContext::new(deep_upper_path.as_bytes(), &mut scratch);
            assert_eq!(
                simple_basename_rules.matched_in_context(&mut ctx, false),
                None
            );
        }
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "simple Basename rules (`*.TMPN`) must not fold or gate under case_insensitive=true"
        );

        // Depth-128 path with 100 complex wildcard `Basename` rules
        // (`*TMP{i}*.BAK`):
        // - uppercase ancestors + lowercase basename: 0 folds!
        // - uppercase ancestors + uppercase basename: 1 fold of ONLY the basename!
        let mut basename_builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        for index in 0..100 {
            basename_builder.add_line(format!("*TMP{index}*.BAK"));
        }
        let basename_rules = basename_builder.build();
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        {
            let mut ctx = EntryMatchContext::new(deep_lower_base.as_bytes(), &mut scratch);
            assert_eq!(basename_rules.matched_in_context(&mut ctx, false), None);
        }
        assert_eq!(
            super::CASE_FOLD_CALLS.with(|c| c.get()),
            0,
            "complex Basename rules must not fold when only ancestor directories have uppercase"
        );
        super::CASE_FOLD_CALLS.with(|c| c.set(0));
        {
            let mut ctx = EntryMatchContext::new(deep_upper_path.as_bytes(), &mut scratch);
            assert_eq!(basename_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(
                ctx.folded_len(),
                "UPPER.TXT".len(),
                "complex Basename rules on deep paths must fold only the basename slice"
            );
        }
        assert_eq!(super::CASE_FOLD_CALLS.with(|c| c.get()), 1);
    }

    #[test]
    fn basename_and_single_component_fast_paths_match_full_component_walk() {
        let rule_lines = [
            "*.log",
            "node_modules/",
            "!keep.log",
            "**/foo",
            r"**\/foo",
            "/foo",
            "/*.log",
            "*",
            "target/*.txt",
            "target/**",
            "target/**/*.txt",
            "*/output0",
            "src/*/output0",
            "a/b/*.log",
        ];
        let candidates: [&[u8]; 26] = [
            b"",
            b"/",
            b"//",
            b"foo",
            b"foo/",
            b"foo//",
            b"app.log",
            b"APP.LOG",
            b"keep.log",
            b"KEEP.LOG",
            b"target",
            b"target/",
            b"target/app.txt",
            b"TARGET/APP.TXT",
            b"target/sub/app.txt",
            b"a/output0",
            b"a/output0/",
            b"a/output0//",
            b"src/mid/output0",
            b"src/mid/output0/",
            b"src/mid/output0//",
            b"a/b/c/d/e/f/g/foo",
            b"a/b/c/d/e/f/g/foo/",
            b"a/b/c/d/e/f/g/foo//",
            b"a/b/c/d/e/f/g/node_modules",
            b"a/b/app.log",
        ];

        for case_insensitive in [false, true] {
            for line in rule_lines {
                let mut builder = RuleSetBuilder::new(Path::new(""), case_insensitive, false);
                builder.add_line(line);
                let set = builder.build();
                let rule = &set.rules[0];
                for candidate in candidates {
                    for is_dir in [false, true] {
                        let expected = if rule.directory_only && !is_dir {
                            None
                        } else if matches_components(&rule.components, candidate) {
                            Some(!rule.negated)
                        } else {
                            None
                        };
                        let actual = set.matched(candidate, is_dir);
                        assert_eq!(
                            actual,
                            expected,
                            "fast-path mismatch for rule {line:?} (ignoreCase={case_insensitive}, is_dir={is_dir}) on {:?}",
                            String::from_utf8_lossy(candidate)
                        );
                    }
                }
            }
        }

        // Verify basename-only and single-component anchored rules bypass
        // `matches_components` completely on deeply nested paths.
        let mut builder = RuleSetBuilder::new(Path::new("/repo"), true, false);
        for line in ["*.log", "node_modules/", "!keep.log", "**/foo", "/root.txt"] {
            builder.add_line(line);
        }
        let set = builder.build();
        super::FULL_COMPONENT_MATCHES.with(|c| c.set(0));
        for candidate in [
            b"/repo/a/b/c/d/e/f/g/h/i/j/app.log".as_slice(),
            b"/repo/a/b/c/d/e/f/g/h/i/j/KEEP.LOG",
            b"/repo/a/b/c/d/e/f/g/h/i/j/node_modules",
            b"/repo/a/b/c/d/e/f/g/h/i/j/FOO",
            b"/repo/a/b/c/d/e/f/g/h/i/j/unmatched.rs",
            b"/repo/ROOT.TXT",
            b"/repo/sub/ROOT.TXT",
        ] {
            let _ = set.matched(candidate, false);
            let _ = set.matched(candidate, true);
        }
        assert_eq!(
            super::FULL_COMPONENT_MATCHES.with(|c| c.get()),
            0,
            "basename-only and anchored single-component rules must not call matches_components"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn precompose_unicode_skips_ascii_and_hoists_nfd_across_rule_sets() {
        // Three nested rule sets with mixed `core.precomposeUnicode` settings:
        // leaf=true, mid=false, root=true (e.g., nested repos with different config).
        let mut root_builder = RuleSetBuilder::new(Path::new("/repo"), true, true);
        root_builder.add_line("caf\u{e9}.txt");
        let root_rules = root_builder.build();

        let mut mid_builder = RuleSetBuilder::new(Path::new("/repo/sub"), true, false);
        // Matches only raw NFD `na\u{0069}\u{0308}ve.raw` (NOT NFC).
        mid_builder.add_line("nai\u{0308}ve.raw");
        let mid_rules = mid_builder.build();

        let mut leaf_builder = RuleSetBuilder::new(Path::new("/repo/sub/deep"), true, true);
        leaf_builder.add_line("other.tmp");
        let leaf_rules = leaf_builder.build();

        let mut scratch = IgnoreMatchScratch::default();

        // 1. Pure ASCII path never invokes `precompose_into`.
        super::PRECOMPOSE_CALLS.with(|c| c.set(0));
        {
            let mut ctx = EntryMatchContext::new(b"/repo/sub/deep/plain_ascii.rs", &mut scratch);
            assert_eq!(leaf_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(mid_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(root_rules.matched_in_context(&mut ctx, false), None);
        }
        assert_eq!(super::PRECOMPOSE_CALLS.with(|c| c.get()), 0);

        // 2. Non-ASCII NFD walk root prefix with ASCII relative path also
        //    skips `precompose_into`.
        let mut nfd_root_builder =
            RuleSetBuilder::new(Path::new("/tmp/cafe\u{0301}/repo"), true, true);
        nfd_root_builder.add_line("*.log");
        let nfd_root_rules = nfd_root_builder.build();
        super::PRECOMPOSE_CALLS.with(|c| c.set(0));
        {
            let mut ctx = EntryMatchContext::new(
                "/tmp/cafe\u{0301}/repo/src/main.rs".as_bytes(),
                &mut scratch,
            );
            assert_eq!(nfd_root_rules.matched_in_context(&mut ctx, false), None);
        }
        assert_eq!(super::PRECOMPOSE_CALLS.with(|c| c.get()), 0);

        // 3. Warm scratch on NFD + uppercase candidate, then verify:
        //    - `precompose_into` runs AT MOST ONCE across all 3 nested sets;
        //    - outer `precompose=true` set matches NFC + case-insensitive;
        //    - middle `precompose=false` set still sees raw NFD bytes;
        //    - zero heap allocations once scratch is warm.
        let nfd_cafe = "/repo/sub/deep/Cafe\u{0301}.TXT".as_bytes();
        let nfd_naive = "/repo/sub/deep/NAi\u{0308}ve.RAW".as_bytes();
        {
            let mut ctx = EntryMatchContext::new(nfd_cafe, &mut scratch);
            assert_eq!(leaf_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(mid_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(root_rules.matched_in_context(&mut ctx, false), Some(true));
        }
        {
            let mut ctx = EntryMatchContext::new(nfd_naive, &mut scratch);
            assert_eq!(leaf_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(mid_rules.matched_in_context(&mut ctx, false), Some(true));
        }

        super::PRECOMPOSE_CALLS.with(|c| c.set(0));
        let cafe_allocs = allocation_counter::measure(|| {
            let mut ctx = EntryMatchContext::new(nfd_cafe, &mut scratch);
            assert_eq!(leaf_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(mid_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(root_rules.matched_in_context(&mut ctx, false), Some(true));
        });
        assert_eq!(
            super::PRECOMPOSE_CALLS.with(|c| c.get()),
            1,
            "NFD candidate with ASCII ancestors must precompose at most once across nested precompose_unicode sets"
        );
        assert_eq!(cafe_allocs.count_total, 0);

        // Middle node (`precompose_unicode = false`) sees raw NFD and matches
        // `nai\u{0308}ve.raw` case-insensitively, even after leaf node
        // (`precompose_unicode = true`) precomposed the path into scratch.
        super::PRECOMPOSE_CALLS.with(|c| c.set(0));
        let mixed_allocs = allocation_counter::measure(|| {
            let mut ctx = EntryMatchContext::new(nfd_naive, &mut scratch);
            assert_eq!(leaf_rules.matched_in_context(&mut ctx, false), None);
            assert_eq!(mid_rules.matched_in_context(&mut ctx, false), Some(true));
        });
        assert_eq!(super::PRECOMPOSE_CALLS.with(|c| c.get()), 1);
        assert_eq!(mixed_allocs.count_total, 0);

        // 4. Root prefix containing `e` + 12 combining acute accents (which
        //    would spill `unicode_normalization`'s internal stack buffer if
        //    normalized) must NEVER be normalized when matching 100 already-NFC
        //    non-ASCII filenames below that root.
        let combining_root = format!("/tmp/e{}/repo", "\u{0301}".repeat(12));
        let mut combining_builder = RuleSetBuilder::new(Path::new(&combining_root), true, true);
        combining_builder.add_line("*.log");
        let combining_rules = combining_builder.build();
        let nfc_candidates: Vec<Vec<u8>> = (0..100)
            .map(|index| format!("{combining_root}/日本語-{index:03}.txt").into_bytes())
            .collect();
        let mut fresh_scratch = IgnoreMatchScratch::default();
        let nfc_allocs = allocation_counter::measure(|| {
            for candidate in &nfc_candidates {
                let mut ctx = EntryMatchContext::new(candidate, &mut fresh_scratch);
                assert_eq!(combining_rules.matched_in_context(&mut ctx, false), None);
            }
        });
        assert_eq!(
            nfc_allocs.count_total, 0,
            "already-NFC filenames under a combining-mark root must make zero NFC allocations"
        );
        assert_eq!(fresh_scratch.precomposed_capacity(), 0);
    }
}
