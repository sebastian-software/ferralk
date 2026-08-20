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
//! - Asterisks that do not form a whole component are ordinary stars in Git and
//!   must not cross a separator, so `a**b` collapses to `a*b`.

use std::path::Path;

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
fn component_options() -> PatternOptions {
    PatternOptions::default()
        .braces(false)
        .extglob(false)
        .recursive_double_star(false)
        .match_hidden(true)
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
    /// the scan runs backwards and stops at the first hit. Nothing here
    /// allocates.
    pub(crate) fn matched(&self, path: &[u8], is_dir: bool) -> Option<bool> {
        let candidate = path.get(self.root_len..).unwrap_or(path);
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.matches(candidate, is_dir))
            .map(|rule| !rule.negated)
    }
}

#[derive(Debug)]
struct Rule {
    /// The rule body, one entry per path component.
    components: Vec<Component>,
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
            && matches_components(&self.components, candidate)
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
    rules: Vec<Rule>,
}

impl RuleSetBuilder {
    pub(crate) fn new(root: &Path) -> Self {
        let root = glob_path_bytes(root);
        // The separator between the directory and what follows it belongs to
        // the prefix, unless the directory already ends in one.
        let root_len = root.len() + usize::from(!root.ends_with(b"/"));
        Self {
            root_len,
            rules: Vec::new(),
        }
    }

    /// Adds one line. A line that is blank, a comment, or malformed adds
    /// nothing, which is what Git does with it.
    pub(crate) fn add_line(&mut self, line: &str) {
        if let Some(rule) = parse_rule(line) {
            self.rules.push(rule);
        }
    }

    pub(crate) fn build(self) -> RuleSet {
        RuleSet {
            root_len: self.root_len,
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
    let mut builder = RuleSetBuilder::new(Path::new(""));
    builder.add_line(line);
    builder.build().matched(candidate, is_dir)
}

/// Reads one `gitignore(5)` line into a rule, or nothing.
fn parse_rule(line: &str) -> Option<Rule> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let body = strip_trailing_spaces(line);
    if body.is_empty() {
        return None;
    }

    let (negated, body) = match body.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    // A rule ending in an unpaired backslash escapes nothing and matches
    // nothing; Git drops it rather than guessing.
    if trailing_backslashes(body) % 2 == 1 {
        return None;
    }

    let (directory_only, body) = match body.strip_suffix('/') {
        Some(rest) => (true, rest),
        None => (false, body),
    };
    // A separator anywhere but at the end binds the rule to its own directory;
    // without one it matches at any level.
    let anchored = body.contains('/');
    let body = body.strip_prefix('/').unwrap_or(body);
    if body.is_empty() {
        return None;
    }

    let mut components = Vec::new();
    let mut gate: Option<Vec<u8>> = None;
    if !anchored {
        components.push(Component::AnyDirs);
    }
    for part in body.split('/').filter(|part| !part.is_empty()) {
        if let Some(literal) = longest_literal_run(part.as_bytes())
            && gate.as_ref().is_none_or(|best| best.len() < literal.len())
        {
            gate = Some(literal);
        }
        let component = compile_component(part)?;
        // Two runs in a row are one run, and collapsing them keeps the matcher
        // from backtracking between them for nothing.
        if matches!(component, Component::AnyDirs)
            && matches!(components.last(), Some(Component::AnyDirs))
        {
            continue;
        }
        components.push(component);
    }
    if matches!(components.last(), Some(Component::AnyDirs)) {
        // `abc/**` is what is inside `abc`, so one component has to follow.
        components.push(compile_component("*")?);
    }
    // A body of nothing but separators is not a rule.
    if components.len() == usize::from(!anchored) {
        return None;
    }

    Some(Rule {
        components,
        gate: gate.map(|literal| memmem::Finder::new(&literal).into_owned()),
        negated,
        directory_only,
    })
}

fn compile_component(part: &str) -> Option<Component> {
    if part == "**" {
        return Some(Component::AnyDirs);
    }
    Pattern::compile(collapse_partial_stars(part.as_bytes()), component_options())
        .ok()
        .map(Component::Pattern)
}

/// Drops the trailing spaces Git drops: those that are not escaped.
fn strip_trailing_spaces(line: &str) -> &str {
    let mut end = line.len();
    while end > 0 && line.as_bytes()[end - 1] == b' ' {
        if trailing_backslashes(&line[..end - 1]) % 2 == 1 {
            break;
        }
        end -= 1;
    }
    &line[..end]
}

fn trailing_backslashes(text: &str) -> usize {
    text.bytes().rev().take_while(|byte| *byte == b'\\').count()
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
                            None => scan += 1,
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

    use super::{RuleSetBuilder, parse_rule};

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
        let mut builder = RuleSetBuilder::new(&root);
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
    fn trailing_spaces_are_dropped_unless_escaped() {
        let root = Path::new("/fixture");
        let mut builder = RuleSetBuilder::new(root);
        builder.add_line("trailing ");
        assert_eq!(
            builder.build().matched(&candidate(root, "trailing"), false),
            Some(true)
        );

        let mut builder = RuleSetBuilder::new(root);
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
        let mut builder = RuleSetBuilder::new(root);
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
        let mut builder = RuleSetBuilder::new(&root);
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

            let mut ours = RuleSetBuilder::new(&root);
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
