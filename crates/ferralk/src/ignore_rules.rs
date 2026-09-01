//! Gitignore rules, parsed here and matched with ferralk-glob.
//!
//! ADR-0014: Git stays normative, the engine is ours. A rule line is read the
//! way `gitignore(5)` describes it - comment, negation, trailing spaces,
//! directory-only slash, anchoring - and its body becomes a matcher. The
//! evaluation around this, the per-directory chains and what a subtree carries
//! down, lives in [`super::gitignore`] and is untouched.
//!
//! A rule compiles per path component rather than into one pattern, because the
//! two dialects read `**` differently. ferralk's `**` is an ordinary recursive
//! wildcard that need not stop on a component boundary - deliberately, as
//! `fastglob-034` in the corpus records - while Git's is only ever a whole
//! component. Translating `foo` into `**/foo` would therefore also ignore
//! `xfoo`. The component walk is ours; each component is a ferralk-glob
//! pattern, which is where stars, classes and escapes are actually matched.
//!
//! What that leaves for the translation:
//!
//! - A rule without a separator matches at any level, which is a leading `**`
//!   component; one with a separator is anchored to its own directory.
//! - A trailing `/**` covers what is inside a directory, so something has to
//!   follow it.
//! - Asterisks inside a component are ordinary stars in Git, so `a**b`
//!   collapses to `a*b`; a run of two or more stars at a component suffix also
//!   has Git's directory-spanning and zero-width forms.

use std::{borrow::Cow, path::Path};

use ferralk_glob::{Pattern, PatternOptions};
use memchr::memmem;

use super::glob_path_bytes;

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
    /// Bytes of the directory the rules are anchored to, separator included.
    ///
    /// Candidates arrive as the whole path, converted once for the entire
    /// chain, so each set slices off its own prefix instead of walking the
    /// components again.
    root_len: usize,
    precompose_unicode: bool,
    rules: Vec<Rule>,
}

impl RuleSet {
    pub(crate) fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The verdict of the last rule that matches, or `None` when none does.
    ///
    /// `path` is the candidate's whole path in glob bytes; the part below this
    /// set's directory is what the rules see. Last-match-wins is Git's rule, so
    /// the scan runs backwards and stops at the first hit. The ordinary
    /// byte-exact path is allocation-free; enabled precomposition allocates
    /// only when a valid UTF-8 component actually changes under NFC.
    pub(crate) fn matched(&self, path: &[u8], is_dir: bool) -> Option<bool> {
        let candidate = path.get(self.root_len..).unwrap_or(path);
        if self.precompose_unicode {
            let candidate = normalize_candidate(candidate);
            self.matched_candidate(&candidate, is_dir)
        } else {
            // The ordinary byte-exact path neither allocates nor constructs a
            // normalization iterator.
            self.matched_candidate(candidate, is_dir)
        }
    }

    fn matched_candidate(&self, candidate: &[u8], is_dir: bool) -> Option<bool> {
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.matches(candidate, is_dir))
            .map(|rule| !rule.negated)
    }
}

#[derive(Debug)]
struct Rule {
    /// The ordinary directory-spanning spelling of the rule.
    components: Vec<Component>,
    /// The spelling where the one partial suffix run and any adjacent whole
    /// `**` components all take their zero-width form.
    zero_components: Option<Vec<Component>>,
    /// A searcher for the longest run of literal bytes the rule requires.
    ///
    /// A file with a hundred rules asks every one of them about every entry,
    /// and almost all of those questions have the same answer. A rule that
    /// spells out `node_modules` or `.tmp7` cannot match a candidate that does
    /// not contain those bytes, so one substring search rejects it before the
    /// component walk starts. The searcher is built once with the rule, not per
    /// candidate, because building one costs more than running it. Rules made
    /// only of wildcards have no gate and are walked as before.
    gate: Option<memmem::Finder<'static>>,
    /// `!` prefix: a match re-includes instead of ignoring.
    negated: bool,
    /// Trailing `/`: the rule matches directories only.
    directory_only: bool,
}

impl Rule {
    fn matches(&self, candidate: &[u8], is_dir: bool) -> bool {
        if self.directory_only && !is_dir {
            return false;
        }
        self.gate
            .as_ref()
            .is_none_or(|gate| gate.find(candidate).is_some())
            && (matches_components(&self.components, candidate)
                || self
                    .zero_components
                    .as_ref()
                    .is_some_and(|components| matches_components(components, candidate)))
    }
}

#[derive(Debug)]
enum Component {
    /// A `**` component: zero or more whole path components.
    AnyDirs,
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

/// Splits off the first path component; the tail is empty at the last one.
fn split_component(candidate: &[u8]) -> (&[u8], &[u8]) {
    match candidate.iter().position(|byte| *byte == b'/') {
        Some(separator) => (&candidate[..separator], &candidate[separator + 1..]),
        None => (candidate, &[]),
    }
}

/// Collects the rules of one or more ignore files for the same directory.
pub(crate) struct RuleSetBuilder {
    root_len: usize,
    case_insensitive: bool,
    precompose_unicode: bool,
    rules: Vec<Rule>,
}

impl RuleSetBuilder {
    pub(crate) fn new(root: &Path, case_insensitive: bool, precompose_unicode: bool) -> Self {
        let root = glob_path_bytes(root);
        // The separator between the directory and what follows it belongs to
        // the prefix, unless the directory already ends in one. The empty
        // root is used by the fuzz helper, where candidates are already
        // relative and so have no prefix to remove.
        let root_len = if root.is_empty() {
            0
        } else {
            root.len() + usize::from(!root.ends_with(b"/"))
        };
        Self {
            root_len,
            case_insensitive,
            precompose_unicode,
            rules: Vec::new(),
        }
    }

    /// Adds one line. A line that is blank, a comment, or malformed adds
    /// nothing, which is what Git does with it.
    pub(crate) fn add_line(&mut self, line: impl AsRef<[u8]>) {
        if let Some(rule) = parse_rule_with_options(line, self.case_insensitive) {
            self.rules.push(rule);
        }
    }

    pub(crate) fn build(self) -> RuleSet {
        RuleSet {
            root_len: self.root_len,
            precompose_unicode: self.precompose_unicode,
            rules: self.rules,
        }
    }
}

/// Fuzz entry point for the rule layer.
///
/// Reads one rule line and asks it about one candidate, which is the whole
/// surface the harness needs: parsing and matching are the two halves that must
/// stay total, whatever bytes arrive.
pub fn fuzz_rule(line: &str, candidate: &[u8], is_dir: bool) -> Option<bool> {
    fuzz_rule_bytes(line.as_bytes(), candidate, is_dir)
}

/// Byte-oriented fuzz entry point for the rule layer.
///
/// Ignore files are bytes, so this also reaches patterns that are not valid
/// UTF-8. It shares the same empty-root setup as [`fuzz_rule`].
#[doc(hidden)]
pub fn fuzz_rule_bytes(line: &[u8], candidate: &[u8], is_dir: bool) -> Option<bool> {
    let mut builder = RuleSetBuilder::new(Path::new(""), false, false);
    builder.add_line(line);
    builder.build().matched(candidate, is_dir)
}

/// Reads one `gitignore(5)` line into a rule, or nothing.
#[cfg(test)]
fn parse_rule(line: impl AsRef<[u8]>) -> Option<Rule> {
    parse_rule_with_options(line, false)
}

fn parse_rule_with_options(line: impl AsRef<[u8]>, case_insensitive: bool) -> Option<Rule> {
    let line = line.as_ref();
    if line.is_empty() || line.starts_with(b"#") {
        return None;
    }
    let body = strip_trailing_spaces(line);
    if body.is_empty() {
        return None;
    }

    let (negated, body) = match body.strip_prefix(b"!") {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    // A rule ending in an unpaired backslash escapes nothing and matches
    // nothing; Git drops it rather than guessing.
    if trailing_backslashes(body) % 2 == 1 {
        return None;
    }

    let (directory_only, body) = match body.strip_suffix(b"/") {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    // Git decides anchoring with a plain slash search, even when that slash is
    // a dead member of a bracket class. Splitting still keeps class slashes in
    // their component because candidates never contain separators there.
    let anchored = body.contains(&b'/');
    let body = body.strip_prefix(b"/").unwrap_or(body);
    if body.is_empty() {
        return None;
    }
    // A single leading separator is the anchor. Any other empty component is
    // unmatchable in Git, rather than an alternate spelling of a valid rule.
    let parts = split_rule_components(body);
    if parts.iter().any(|part| part.is_empty()) {
        return None;
    }

    let mut normalized = Vec::with_capacity(parts.len());
    let mut gate: Option<Vec<u8>> = None;
    for part in &parts {
        let part = strip_bracket_separators(part)?;
        if let Some(literal) = longest_literal_run(&part)
            && gate.as_ref().is_none_or(|best| best.len() < literal.len())
        {
            gate = Some(literal);
        }
        normalized.push(part);
    }

    let mut wildcard_seen = false;
    let mut star_run = None;
    for (index, part) in normalized.iter().enumerate() {
        if !wildcard_seen
            && (index + 1 < normalized.len() || anchored)
            && !all_stars(part)
            && special_suffix_prefix(part).is_some()
        {
            star_run = Some(index);
        }
        wildcard_seen |= part
            .iter()
            .any(|byte| matches!(byte, b'*' | b'?' | b'[' | b'\\'));
    }

    let mut components = Vec::with_capacity(normalized.len() + 2);
    if !anchored {
        components.push(Component::AnyDirs);
    }
    for (index, part) in normalized.iter().enumerate() {
        let component = compile_component(part, case_insensitive)?;
        if matches!(component, Component::AnyDirs)
            && matches!(components.last(), Some(Component::AnyDirs))
        {
            continue;
        }
        components.push(component);
        if star_run == Some(index) {
            components.push(Component::AnyDirs);
        }
    }
    let trailing_whole_run = normalized
        .last()
        .is_some_and(|part| all_stars(part) && part.len() >= 2);
    let partial_reaches_trailing_runs = star_run.is_some_and(|index| {
        normalized[index + 1..]
            .iter()
            .all(|part| all_stars(part) && part.len() >= 2)
    });
    let trailing_requires_component = trailing_whole_run && !partial_reaches_trailing_runs;
    if trailing_requires_component {
        // `abc/**` is what is inside `abc`, so one component has to follow.
        components.push(compile_component(b"*", case_insensitive)?);
    }
    // A body of nothing but separators is not a rule.
    if components.len() == usize::from(!anchored) {
        return None;
    }

    let zero_components = star_run.and_then(|index| {
        let mut fused = special_suffix_prefix(&normalized[index])?.to_vec();
        let mut next = index + 1;
        while normalized
            .get(next)
            .is_some_and(|part| all_stars(part) && part.len() >= 2)
        {
            next += 1;
        }
        if let Some(part) = normalized.get(next) {
            fused.extend_from_slice(part);
        }

        let mut zero = Vec::with_capacity(components.len());
        if !anchored {
            zero.push(Component::AnyDirs);
        }
        for (part_index, part) in normalized.iter().enumerate() {
            if part_index == index {
                zero.push(compile_component(&fused, case_insensitive)?);
            } else if part_index > index && part_index <= next {
                continue;
            } else {
                let component = compile_component(part, case_insensitive)?;
                if matches!(component, Component::AnyDirs)
                    && matches!(zero.last(), Some(Component::AnyDirs))
                {
                    continue;
                }
                zero.push(component);
            }
        }
        if trailing_requires_component && next < normalized.len() - 1 {
            zero.push(compile_component(b"*", case_insensitive)?);
        }
        Some(zero)
    });

    Some(Rule {
        components,
        zero_components,
        // This prefilter is byte-exact, so it would reject a case-folded
        // candidate before the rule matcher can evaluate it.
        gate: (!case_insensitive)
            .then(|| gate.map(|literal| memmem::Finder::new(&literal).into_owned()))
            .flatten(),
        negated,
        directory_only,
    })
}

/// Splits a rule on path separators, keeping a slash inside a bracket class in
/// its component. Escaped brackets do not open a class, matching the glob
/// parser's byte-oriented escape handling.
fn split_rule_components(body: &[u8]) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_class = false;
    let mut class_saw_member = false;
    let mut index = 0;
    while index < body.len() {
        match body[index] {
            b'\\' => {
                if in_class {
                    class_saw_member = true;
                }
                index += 2;
            }
            b'[' if in_class && body.get(index + 1) == Some(&b':') => {
                let start = index + 2;
                if let Some(end) = memmem::find(&body[start..], b":]") {
                    class_saw_member = true;
                    index = start + end + 2;
                } else {
                    class_saw_member = true;
                    index += 1;
                }
            }
            b'[' if !in_class => {
                in_class = true;
                class_saw_member = false;
                index += 1;
                if matches!(body.get(index), Some(b'!' | b'^')) {
                    index += 1;
                }
            }
            b']' if in_class => {
                if class_saw_member {
                    in_class = false;
                } else {
                    // The first `]` after `[` or `[!` is a literal member;
                    // only a later one closes the class.
                    class_saw_member = true;
                }
                index += 1;
            }
            b'/' if in_class => {
                class_saw_member = true;
                index += 1;
            }
            b'/' if !in_class => {
                parts.push(&body[start..index]);
                start = index + 1;
                index += 1;
            }
            _ => {
                if in_class {
                    class_saw_member = true;
                }
                index += 1;
            }
        }
    }
    parts.push(&body[start..]);
    parts
}

fn compile_component(part: &[u8], case_insensitive: bool) -> Option<Component> {
    if all_stars(part) && part.len() >= 2 {
        return Some(Component::AnyDirs);
    }
    Pattern::compile(
        collapse_partial_stars(part),
        component_options(case_insensitive),
    )
    .ok()
    .map(Component::Pattern)
}

fn all_stars(part: &[u8]) -> bool {
    !part.is_empty() && part.iter().all(|byte| *byte == b'*')
}

/// Prefix before a special suffix star run, on Git's literal-prefix terms.
///
/// Escaped metacharacters stay literal. An unescaped wildcard or class before
/// the final run means Git already handed the whole component to wildmatch, so
/// the run is an ordinary `*` rather than a directory-spanning spelling.
fn special_suffix_prefix(part: &[u8]) -> Option<&[u8]> {
    let mut index = 0;
    let mut wildcard_before_run = false;
    let mut run_start = None;
    let mut run_length = 0;
    while index < part.len() {
        match part[index] {
            b'\\' if index + 1 < part.len() => {
                wildcard_before_run = true;
                run_start = None;
                run_length = 0;
                index += 2;
            }
            b'*' => {
                let length = part[index..]
                    .iter()
                    .take_while(|byte| **byte == b'*')
                    .count();
                run_start = Some(index);
                run_length = length;
                if index + length < part.len() {
                    wildcard_before_run = true;
                }
                index += length;
            }
            b'?' | b'[' => {
                wildcard_before_run = true;
                run_start = None;
                run_length = 0;
                index += 1;
            }
            _ => {
                run_start = None;
                run_length = 0;
                index += 1;
            }
        }
    }
    if run_length >= 2 && !wildcard_before_run {
        run_start.map(|start| &part[..start])
    } else {
        None
    }
}

/// Candidate components never contain a path separator, so a slash admitted
/// by Git inside a bracket class cannot affect matching. ferralk-glob rejects
/// it as a separator; drop just that dead class member before compiling.
fn strip_bracket_separators(part: &[u8]) -> Option<Vec<u8>> {
    let mut result = Vec::with_capacity(part.len());
    let mut in_class = false;
    let mut class_start = 0;
    let mut class_negated = false;
    let mut class_has_member = false;
    let mut class_saw_member = false;
    let mut index = 0;
    while index < part.len() {
        match part[index] {
            b'\\' if index + 1 < part.len() => {
                result.extend_from_slice(&part[index..=index + 1]);
                if in_class {
                    class_has_member = true;
                    class_saw_member = true;
                }
                index += 2;
            }
            b'[' if in_class && part.get(index + 1) == Some(&b':') => {
                let start = index + 2;
                if let Some(end) = memmem::find(&part[start..], b":]") {
                    result.extend_from_slice(&part[index..start + end + 2]);
                    class_has_member = true;
                    class_saw_member = true;
                    index = start + end + 2;
                } else {
                    result.push(b'[');
                    class_has_member = true;
                    class_saw_member = true;
                    index += 1;
                }
            }
            b'[' if !in_class => {
                in_class = true;
                class_start = result.len();
                class_negated = matches!(part.get(index + 1), Some(b'!' | b'^'));
                class_has_member = false;
                class_saw_member = false;
                result.push(b'[');
                index += 1;
                if class_negated {
                    result.push(part[index]);
                    index += 1;
                }
            }
            b']' if in_class => {
                if class_saw_member {
                    in_class = false;
                    if class_has_member {
                        result.push(b']');
                    } else if class_negated {
                        result.truncate(class_start);
                        result.push(b'?');
                    } else {
                        return None;
                    }
                } else {
                    // Like Git's wildmatch and the glob compiler, a closing
                    // bracket in the first member position is literal. A
                    // second bracket is required to close `[]]` or `[!]]`.
                    result.push(b']');
                    class_has_member = true;
                    class_saw_member = true;
                }
                index += 1;
            }
            b'/' if in_class => {
                class_saw_member = true;
                index += 1;
            }
            byte => {
                result.push(byte);
                if in_class {
                    class_has_member = true;
                    class_saw_member = true;
                }
                index += 1;
            }
        }
    }
    Some(result)
}

/// Adapts the candidate that one ignore file sees, not its raw pattern. Mac
/// Git precomposes filesystem names this way before matching; each path
/// component is independent so an invalid UTF-8 component cannot make a valid
/// sibling component lose its NFC conversion.
#[cfg(target_os = "macos")]
fn normalize_candidate(candidate: &[u8]) -> Cow<'_, [u8]> {
    use unicode_normalization::{UnicodeNormalization, is_nfc};

    if candidate
        .split(|byte| *byte == b'/')
        .all(|component| std::str::from_utf8(component).map_or(true, is_nfc))
    {
        return Cow::Borrowed(candidate);
    }

    let mut normalized = Vec::with_capacity(candidate.len());
    for (index, component) in candidate.split(|byte| *byte == b'/').enumerate() {
        if index != 0 {
            normalized.push(b'/');
        }
        match std::str::from_utf8(component) {
            Ok(component) => {
                for character in component.nfc() {
                    let mut encoded = [0; 4];
                    normalized.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
                }
            }
            Err(_) => normalized.extend_from_slice(component),
        }
    }
    Cow::Owned(normalized)
}

#[cfg(not(target_os = "macos"))]
fn normalize_candidate(candidate: &[u8]) -> Cow<'_, [u8]> {
    Cow::Borrowed(candidate)
}

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
                // themselves. A POSIX name brings its own brackets, so `[:x:]`
                // is skipped whole rather than ending the class at its `]`.
                let mut scan = index + 1;
                if matches!(part.get(scan), Some(b'!' | b'^')) {
                    scan += 1;
                }
                if part.get(scan) == Some(&b']') {
                    scan += 1;
                }
                while scan < part.len() && part[scan] != b']' {
                    if part[scan] == b'[' && part.get(scan + 1) == Some(&b':') {
                        match part[scan + 2..].windows(2).position(|pair| pair == b":]") {
                            Some(end) => scan += 2 + end + 2,
                            // The rest of an unclosed POSIX class cannot be a
                            // literal run. Skipping it also keeps malformed
                            // input linear instead of re-scanning the suffix
                            // at every nested `[:` opener.
                            None => scan = part.len(),
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

/// Collapses a run of asterisks into a single star.
///
/// A `**` component has already been taken out by [`compile_component`], so
/// every run reaching here is one ordinary star in Git, and an ordinary star
/// never crosses a separator. An escaped asterisk is literal and not part of a
/// run.
fn collapse_partial_stars(part: &[u8]) -> Vec<u8> {
    let mut collapsed = Vec::with_capacity(part.len());
    let mut index = 0;
    while index < part.len() {
        match part[index] {
            b'\\' if index + 1 < part.len() => {
                collapsed.extend_from_slice(&part[index..index + 2]);
                index += 2;
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
    use std::path::{Path, PathBuf};

    use super::{RuleSetBuilder, fuzz_rule, fuzz_rule_bytes, parse_rule};

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
        root.join(path).to_string_lossy().into_owned().into_bytes()
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
    fn suffix_star_runs_follow_git_s_directory_and_empty_cases() {
        for candidate in [b"b".as_slice(), b"x/y/b"] {
            assert_eq!(fuzz_rule_bytes(b"***/b", candidate, false), Some(true));
        }
        for candidate in [b"ay".as_slice(), b"a/y", b"ax/y", b"a/x/y"] {
            assert_eq!(fuzz_rule_bytes(b"a**/y", candidate, false), Some(true));
        }
        for candidate in [b"a".as_slice(), b"ab", b"a/x"] {
            assert_eq!(fuzz_rule_bytes(b"a**/**", candidate, false), Some(true));
        }
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

        assert_eq!(fuzz_rule_bytes(b"a**/y", b"ay", false), Some(true));
        assert_eq!(fuzz_rule_bytes(b"a**/y", b"axy", false), None);
        assert_eq!(fuzz_rule_bytes(b"a**/y", b"aXy", false), None);
    }

    #[test]
    fn final_and_repeated_special_star_runs_follow_git() {
        for candidate in [b"a".as_slice(), b"ab", b"a/b", b"a/x/y"] {
            assert_eq!(fuzz_rule_bytes(b"/a**", candidate, false), Some(true));
        }
        assert_eq!(fuzz_rule_bytes(b"a**/**/y", b"ay", false), Some(true));
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

            let mut ours = RuleSetBuilder::new(&root, false, false);
            for line in &lines {
                ours.add_line(line);
            }
            let ours = ours.build();

            let mut theirs = ignore::gitignore::GitignoreBuilder::new(&root);
            for line in &lines {
                let _ = theirs.add_line(None, line);
            }
            let theirs = theirs.build().expect("the previous engine builds");

            let rounds = 200;
            let start = Instant::now();
            for _ in 0..rounds {
                for path in &candidates {
                    std::hint::black_box(ours.matched(path, false));
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
                "{count:>3} rules: ours {ours_ns:6.1} ns/verdict   previous engine {theirs_ns:6.1} ns/verdict"
            );
        }
    }
}
