#![forbid(unsafe_code)]
//! Checks that `PatternOptions::walker()` is the dialect the walker selects in.
//!
//! The preset is public so that a consumer can validate a walker pattern up
//! front or match it again away from the walk. That promise holds only if a
//! pattern compiled with the preset answers every root-relative path exactly
//! as `Walker::include` does, so this replays the corpus patterns and paths
//! through both and compares the selections. `Walker::exclude` is replayed the
//! same way against the preset with `match_hidden(true)`, the dialect every
//! exclude is compiled in whatever the walker's own `match_hidden` says
//! (#424). Every comparison runs twice, once with `Walker::case_insensitive`
//! against the preset with `case_insensitive(true)`, which is how the walker
//! documents that switch. The corpus verdicts themselves are not consulted:
//! each case was recorded under its own flags, and the question here is
//! whether the walker and the preset agree.
//!
//! An integration test rather than a unit test because it needs the
//! unpublished `corpus` package. Cargo strips that path-only dev-dependency
//! when packaging, so this file is excluded from the published crate and
//! `cargo test` from the tarball stays buildable.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{
    WalkEntry, Walker, WildcardMode,
    ferralk_glob::{Pattern, PatternOptions},
};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let unique = format!(
            "ferralk-preset-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&root).expect("create fixture root");
        Self { root }
    }

    /// Creates `path` as a file, or reports that this host cannot spell it.
    fn try_write(&self, path: &str) -> bool {
        let path = self.root.join(path);
        let Some(parent) = path.parent() else {
            return false;
        };
        fs::create_dir_all(parent).is_ok() && fs::write(path, b"fixture").is_ok()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn relative_paths(entries: &[WalkEntry], root: &Path) -> BTreeSet<PathBuf> {
    entries
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect()
}

/// Patterns and root-relative candidate paths the corpus records.
///
/// Only operations whose paths are root-relative contribute paths; the `_at`
/// list operations strip a base directory first and add nothing a walk root
/// does not already express.
fn corpus_inputs() -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let mut files = fs::read_dir(root)
        .expect("read corpus directory")
        .map(|entry| entry.expect("read corpus entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect::<Vec<_>>();
    files.sort();

    let mut rejected = BTreeSet::new();
    let mut pairs = BTreeSet::new();
    for file in files {
        for (line_number, line) in fs::read_to_string(&file)
            .expect("read corpus file")
            .lines()
            .enumerate()
        {
            if line.trim().is_empty() {
                continue;
            }
            let case = corpus::parse_case(line)
                .unwrap_or_else(|error| panic!("{}:{}: {error}", file.display(), line_number + 1));
            if !case.runs_on_host() {
                continue;
            }
            let Some(pattern) = text(&case.pattern) else {
                continue;
            };
            match case.kind {
                corpus::CaseKind::CompileError => {
                    rejected.insert(pattern);
                }
                corpus::CaseKind::Matcher | corpus::CaseKind::MatchGlobPath => {
                    if let Some(path) = text(&case.path) {
                        pairs.insert((pattern, path));
                    }
                }
                corpus::CaseKind::MatchPaths | corpus::CaseKind::MatchPathIndices => {
                    for path in &case.paths {
                        if let Some(path) = text(path) {
                            pairs.insert((pattern.clone(), path));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    (rejected, pairs)
}

/// The decoded corpus bytes, when they are UTF-8 and so name a path on every
/// host the same way.
fn text(encoded: &str) -> Option<String> {
    String::from_utf8(corpus::decode_bytes(encoded).expect("corpus bytes decode")).ok()
}

/// Whether `path` is a relative path a walk can report exactly as spelled.
///
/// Empty, `.` and `..` components have no entry of their own, a backslash
/// separates on Windows and an ordinary byte elsewhere, and a control byte is
/// not worth a filesystem's opinion.
fn walkable(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.bytes().any(|byte| byte.is_ascii_control())
        && path
            .split('/')
            .all(|component| !matches!(component, "" | "." | ".."))
}

/// Whether the walker reads `pattern` exactly as the matcher does, rather than
/// through one of the walker rules the preset documents as outside the
/// dialect: a trailing `/` for directories only, an absolute pattern rewritten
/// against the root, and one leading `./`, which only the path entry points
/// share.
fn relative_walker_pattern(pattern: &str) -> bool {
    !pattern.ends_with('/')
        && !pattern.starts_with('/')
        && !pattern.starts_with("./")
        && !Path::new(pattern).is_absolute()
}

/// Every entry the fixture holds for `path`: the file and each directory
/// above it, all of which a walk reports.
fn entries_for(path: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut end = 0;
    while let Some(offset) = path[end..].find('/') {
        end += offset;
        entries.push(path[..end].to_owned());
        end += 1;
    }
    entries.push(path.to_owned());
    entries
}

/// Whether an unfiltered walk of `root` reports exactly `entries`.
fn holds_exactly(root: &Path, entries: &[String]) -> bool {
    let walked = Walker::new(root)
        .threads(1)
        .collect()
        .expect("unfiltered walk succeeds");
    relative_paths(walked.entries(), root) == entries.iter().map(PathBuf::from).collect()
}

fn walks(walker: impl Fn() -> Walker, root: &Path) -> [(&'static str, BTreeSet<PathBuf>); 3] {
    let serial = walker().threads(1).collect().expect("serial walk succeeds");
    let parallel = walker()
        .threads(4)
        .collect()
        .expect("parallel walk succeeds");
    let streamed = walker()
        .stream()
        .collect::<Result<Vec<_>, _>>()
        .expect("streaming walk succeeds");
    [
        ("serial collect", relative_paths(serial.entries(), root)),
        ("parallel collect", relative_paths(parallel.entries(), root)),
        ("stream", relative_paths(&streamed, root)),
    ]
}

/// A pattern the preset rejects, the walker rejects the same way, so compiling
/// with the preset is a sound pre-validation of `Walker::include`.
#[test]
fn preset_rejections_are_walker_rejections() {
    let (rejected, pairs) = corpus_inputs();
    let mut checked = 0;
    for pattern in rejected
        .iter()
        .chain(pairs.iter().map(|(pattern, _)| pattern))
    {
        for (match_hidden, case_insensitive) in dialects() {
            let options = PatternOptions::walker()
                .match_hidden(match_hidden)
                .case_insensitive(case_insensitive);
            let Err(expected) = Pattern::compile(pattern, options) else {
                continue;
            };
            for mode in [
                WildcardMode::ComponentScoped,
                WildcardMode::SeparatorCrossing,
            ] {
                let walker = || {
                    walker_in(Path::new("."), match_hidden, case_insensitive).wildcard_mode(mode)
                };
                for (operation, result) in [
                    ("include", walker().include(pattern)),
                    ("exclude", walker().exclude(pattern)),
                ] {
                    let error = result.err().unwrap_or_else(|| {
                        panic!("{operation} accepted {pattern:?}, which the preset rejects")
                    });
                    assert_eq!(
                        error, expected,
                        "{operation} rejects {pattern:?} differently from the preset"
                    );
                }
            }
            checked += 1;
        }
    }
    assert!(checked > 0, "the corpus supplies rejected patterns");
}

/// Every `(match_hidden, case_insensitive)` pair the walker can be configured
/// with.
fn dialects() -> [(bool, bool); 4] {
    [(false, false), (true, false), (false, true), (true, true)]
}

/// A walker of `root` with the two dialect switches set.
fn walker_in(root: &Path, match_hidden: bool, case_insensitive: bool) -> Walker {
    Walker::new(root)
        .match_hidden(match_hidden)
        .case_insensitive(case_insensitive)
        .expect("a walker without patterns folds case without recompiling")
}

/// The preset's verdict on `path` under the walker's wildcard `mode`.
fn preset_matches(matcher: &Pattern, path: &str, mode: WildcardMode) -> bool {
    match mode {
        WildcardMode::ComponentScoped => matcher.is_match_glob_path(path),
        _ => matcher.is_match_crossing_path(path),
    }
}

/// Calls `check` once per corpus pattern and walkable path, with a fixture
/// holding exactly that path, and returns how many comparisons it reported.
fn for_each_corpus_fixture(mut check: impl FnMut(&str, &Fixture, &[String]) -> usize) -> usize {
    let (_, pairs) = corpus_inputs();
    let mut checked = 0;
    for (pattern, path) in &pairs {
        if !walkable(path) || !relative_walker_pattern(pattern) {
            continue;
        }
        let fixture = Fixture::new();
        let entries = entries_for(path);
        if !fixture.try_write(path) || !holds_exactly(&fixture.root, &entries) {
            // The host cannot spell this path, or stores it under another
            // name, as Windows does for a trailing period.
            continue;
        }
        checked += check(pattern, &fixture, &entries);
    }
    checked
}

/// A pattern the walker accepts selects exactly the entries the preset
/// matches: through `is_match_glob_path` under the default wildcard mode and,
/// under the separator-crossing one, through `is_match` with the path entry
/// points' per-alternative `./` rule (`is_match_crossing_path`, #395).
#[test]
fn preset_matches_what_the_walker_selects() {
    let checked = for_each_corpus_fixture(|pattern, fixture, entries| {
        let mut checked = 0;
        for (match_hidden, case_insensitive) in dialects() {
            let options = PatternOptions::walker()
                .match_hidden(match_hidden)
                .case_insensitive(case_insensitive);
            let Ok(matcher) = Pattern::compile(pattern, options) else {
                // Rejected patterns belong to the test above.
                continue;
            };
            for mode in [
                WildcardMode::ComponentScoped,
                WildcardMode::SeparatorCrossing,
            ] {
                let base =
                    || walker_in(&fixture.root, match_hidden, case_insensitive).wildcard_mode(mode);
                if base().include(pattern).is_err() {
                    // A walker-only refusal, such as a `..` component, which
                    // the preset documents as outside the dialect.
                    continue;
                }
                let expected = entries
                    .iter()
                    .filter(|entry| preset_matches(&matcher, entry, mode))
                    .map(PathBuf::from)
                    .collect::<BTreeSet<_>>();
                let walker = || {
                    base()
                        .include(pattern)
                        .expect("the walker accepted this include above")
                };
                for (frontend, selected) in walks(walker, &fixture.root) {
                    assert_eq!(
                        selected, expected,
                        "{frontend}: include {pattern:?} over {entries:?} under {mode:?}, \
                         match_hidden {match_hidden}, case_insensitive {case_insensitive}"
                    );
                }
                checked += 1;
            }
        }
        checked
    });
    assert!(
        checked > 1000,
        "only {checked} corpus comparisons reached the walker"
    );
}

/// A pattern the walker accepts as an exclude removes exactly what the preset
/// with `match_hidden(true)` matches, under either walker `match_hidden`: the
/// matching entries and, with no include to re-admit anything, everything
/// below a matching directory.
#[test]
fn preset_with_hidden_matching_is_what_the_walker_excludes() {
    let checked = for_each_corpus_fixture(|pattern, fixture, entries| {
        let mut checked = 0;
        for (match_hidden, case_insensitive) in dialects() {
            let options = PatternOptions::walker()
                .match_hidden(true)
                .case_insensitive(case_insensitive);
            let Ok(matcher) = Pattern::compile(pattern, options) else {
                // Rejected patterns belong to the rejection test.
                continue;
            };
            for mode in [
                WildcardMode::ComponentScoped,
                WildcardMode::SeparatorCrossing,
            ] {
                let base =
                    || walker_in(&fixture.root, match_hidden, case_insensitive).wildcard_mode(mode);
                if base().exclude(pattern).is_err() {
                    // A walker-only refusal, as for includes.
                    continue;
                }
                // `entries` lists every ancestor before its descendants, so
                // the first match removes the rest of the chain.
                let expected = entries
                    .iter()
                    .take_while(|entry| !preset_matches(&matcher, entry, mode))
                    .map(PathBuf::from)
                    .collect::<BTreeSet<_>>();
                let walker = || {
                    base()
                        .exclude(pattern)
                        .expect("the walker accepted this exclude above")
                };
                for (frontend, kept) in walks(walker, &fixture.root) {
                    assert_eq!(
                        kept, expected,
                        "{frontend}: exclude {pattern:?} over {entries:?} under {mode:?}, \
                         match_hidden {match_hidden}, case_insensitive {case_insensitive}"
                    );
                }
                checked += 1;
            }
        }
        checked
    });
    assert!(
        checked > 1000,
        "only {checked} corpus comparisons reached the walker"
    );
}
