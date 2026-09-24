#![forbid(unsafe_code)]
//! `Walker::case_insensitive` on a tree with mixed-case names.
//!
//! No two names in one directory differ only in case, so the fixture means the
//! same on a case-sensitive and a case-insensitive filesystem, and every
//! answer below is about the patterns rather than about the host.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{WalkEntry, WalkOptions, Walker, WildcardMode};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let unique = format!(
            "ferralk-case-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&root).expect("create fixture root");
        let fixture = Self { root };
        for file in [
            "SRC/Main.RS",
            "SRC/lib.rs",
            "SRC/notes.txt",
            "SRC/Deep/Mod.Rs",
            "Docs/Guide.MD",
            "node_MODULES/pkg/Index.js",
            "other/src.rs",
            ".Hidden/x.RS",
        ] {
            fixture.write(file, b"fixture");
        }
        fixture
    }

    fn write(&self, path: &str, contents: &[u8]) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().expect("fixture file has parent"))
            .expect("create fixture parent");
        fs::write(path, contents).expect("write fixture file");
    }

    /// The root as an absolute pattern spells it: `/` separators everywhere.
    fn absolute(&self) -> String {
        self.root
            .to_str()
            .expect("the temporary directory is UTF-8 on a test host")
            .replace('\\', "/")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn relative(entries: &[WalkEntry], root: &Path) -> BTreeSet<String> {
    entries
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .expect("entry below the root")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

/// What serial `collect`, parallel `collect`, `visit` and `stream` return for
/// one configuration, which must be the same set.
fn selected(root: &Path, build: impl Fn() -> Walker) -> BTreeSet<String> {
    let serial = build().threads(1).collect().expect("serial walk");
    assert!(serial.errors().is_empty(), "{:?}", serial.errors());
    let expected = relative(serial.entries(), root);
    let parallel = build().threads(4).collect().expect("parallel walk");
    assert_eq!(relative(parallel.entries(), root), expected, "parallel");
    let visited = build()
        .threads(4)
        .visit(|_| ferralk::Verdict::Keep)
        .expect("visited walk");
    assert_eq!(relative(visited.entries(), root), expected, "visit");
    let streamed = build()
        .stream()
        .collect::<Result<Vec<_>, _>>()
        .expect("streamed walk");
    assert_eq!(relative(&streamed, root), expected, "stream");
    expected
}

fn files_only() -> WalkOptions {
    WalkOptions::default().files_only(true)
}

#[test]
fn walker_matching_is_case_sensitive_by_default() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    assert_eq!(
        selected(root, || Walker::new(root)
            .include("**/*.rs")
            .expect("valid include")
            .options(files_only())),
        set(&["SRC/lib.rs", "other/src.rs"])
    );
    assert!(
        selected(root, || Walker::new(root)
            .include("src/**")
            .expect("valid include"))
        .is_empty()
    );
}

/// The extension prefilter folds with the pattern, or it would drop
/// `Main.RS` before the matcher saw it.
#[test]
fn a_folded_extension_selects_every_spelling() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    for pattern in ["**/*.rs", "**/*.RS", "**/*.rS"] {
        assert_eq!(
            selected(root, || Walker::new(root)
                .case_insensitive(true)
                .expect("no patterns to recompile")
                .include(pattern)
                .expect("valid include")
                .options(files_only())),
            set(&[
                "SRC/Main.RS",
                "SRC/lib.rs",
                "SRC/Deep/Mod.Rs",
                "other/src.rs"
            ]),
            "{pattern}"
        );
    }
}

/// The literal-prefix pruning folds with the pattern: `src/**` must open
/// `SRC/`, and still no other directory.
#[test]
fn a_folded_literal_prefix_enters_a_differently_spelled_directory() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    let everything_in_src = set(&[
        "SRC",
        "SRC/Main.RS",
        "SRC/lib.rs",
        "SRC/notes.txt",
        "SRC/Deep",
        "SRC/Deep/Mod.Rs",
    ]);
    for pattern in ["src/**", "Src/**", "SRC/**"] {
        assert_eq!(
            selected(root, || Walker::new(root)
                .case_insensitive(true)
                .expect("no patterns to recompile")
                .include(pattern)
                .expect("valid include")),
            everything_in_src,
            "{pattern}"
        );
    }
    assert_eq!(
        selected(root, || Walker::new(root)
            .case_insensitive(true)
            .expect("no patterns to recompile")
            .include("src/deep/*.rs")
            .expect("valid include")),
        set(&["SRC/Deep/Mod.Rs"])
    );
    // A pattern without a wildcard is a literal root all the way down.
    assert_eq!(
        selected(root, || Walker::new(root)
            .case_insensitive(true)
            .expect("no patterns to recompile")
            .include("src/LIB.RS")
            .expect("valid include")),
        set(&["SRC/lib.rs"])
    );
}

#[test]
fn a_folded_exclude_prunes_a_differently_spelled_directory() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    let walked = |case_insensitive: bool| {
        selected(root, || {
            Walker::new(root)
                .exclude("**/node_modules/**")
                .expect("valid exclude")
                .case_insensitive(case_insensitive)
                .expect("the exclude recompiles")
        })
    };
    assert!(walked(false).contains("node_MODULES/pkg/Index.js"));
    let folded = walked(true);
    assert!(
        folded.iter().all(|path| !path.starts_with("node_MODULES")),
        "{folded:?}"
    );
    assert!(folded.contains("SRC/Main.RS"));
}

/// Builder order does not matter, `match_hidden` keeps the folding when it
/// recompiles, and switching folding off restores the case-sensitive walk.
#[test]
fn case_folding_composes_with_the_other_dialect_switches_in_any_order() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    let folded = set(&[
        ".Hidden/x.RS",
        "SRC/Main.RS",
        "SRC/lib.rs",
        "SRC/Deep/Mod.Rs",
        "other/src.rs",
    ]);
    assert_eq!(
        selected(root, || Walker::new(root)
            .include("**/*.rs")
            .expect("valid include")
            .case_insensitive(true)
            .expect("the include recompiles")
            .match_hidden(true)
            .options(files_only())),
        folded
    );
    assert_eq!(
        selected(root, || Walker::new(root)
            .match_hidden(true)
            .case_insensitive(true)
            .expect("no patterns to recompile")
            .include("**/*.rs")
            .expect("valid include")
            .options(files_only())),
        folded
    );
    assert_eq!(
        selected(root, || Walker::new(root)
            .case_insensitive(true)
            .expect("no patterns to recompile")
            .include("**/*.rs")
            .expect("valid include")
            .match_hidden(true)
            .case_insensitive(false)
            .expect("the include recompiles")
            .options(files_only())),
        set(&["SRC/lib.rs", "other/src.rs"])
    );
    // The separator-crossing reading folds as well.
    assert_eq!(
        selected(root, || Walker::new(root)
            .wildcard_mode(WildcardMode::SeparatorCrossing)
            .include("*.RS")
            .expect("valid include")
            .case_insensitive(true)
            .expect("the include recompiles")
            .options(files_only())),
        set(&[
            "SRC/Main.RS",
            "SRC/lib.rs",
            "SRC/Deep/Mod.Rs",
            "other/src.rs"
        ])
    );
}

/// Only the part of an absolute pattern below the root folds. The root is a
/// real path compared as spelled, so a differently spelled root names another
/// tree and selects nothing, even on a filesystem that would open it.
#[test]
fn an_absolute_pattern_folds_below_the_root_only() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    let absolute = fixture.absolute();
    let below_root = format!("{absolute}/src/*.rs");
    assert_eq!(
        selected(root, || Walker::new(root)
            .include(&below_root)
            .expect("valid include")
            .case_insensitive(true)
            .expect("the include recompiles")),
        set(&["SRC/Main.RS", "SRC/lib.rs"])
    );

    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .expect("fixture name is UTF-8");
    let respelled_root = format!(
        "{}{}/src/*.rs",
        &absolute[..absolute.len() - name.len()],
        name.to_ascii_uppercase()
    );
    assert_ne!(respelled_root, below_root);
    assert!(
        selected(root, || Walker::new(root)
            .case_insensitive(true)
            .expect("no patterns to recompile")
            .include(&respelled_root)
            .expect(
                "an out-of-root pattern selects nothing without an error"
            ))
        .is_empty()
    );
}

/// Ignore files keep Git's case rule, which `git_ignore_case` decides; the
/// walker's own switch leaves it alone in both directions.
#[test]
fn ignore_rules_keep_their_own_case_setting() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    fixture.write(".gitignore", b"*.log\n");
    fixture.write("x.log", b"");
    fixture.write("Build.LOG", b"");
    let logs = |walker_folds: bool, git_folds: bool| {
        selected(root, || {
            Walker::new(root)
                .case_insensitive(walker_folds)
                .expect("no patterns to recompile")
                .respect_git_ignore(true)
                .git_ignore_case(git_folds)
                .include("*.log")
                .expect("valid include")
        })
    };
    assert!(logs(false, false).is_empty());
    assert_eq!(logs(true, false), set(&["Build.LOG"]));
    assert!(logs(true, true).is_empty());
    assert!(logs(false, true).is_empty());
}
