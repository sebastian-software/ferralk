#![forbid(unsafe_code)]
//! `Verdict::Prune`: the entry is left out and nothing below it is walked, on
//! the serial and the parallel frontend alike.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{CancellationToken, Verdict, WalkEntry, WalkOptions, Walker};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

/// Worker budgets to run every scenario under: the serial frontend and two
/// parallel ones.
const THREADS: [usize; 3] = [1, 2, 8];

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(files: &[&str]) -> Self {
        let unique = format!(
            "ferralk-prune-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&root).expect("create fixture root");
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().expect("fixture file has parent"))
                .expect("create fixture parent");
            fs::write(path, b"fixture").expect("write fixture file");
        }
        Self { root }
    }

    /// The small tree most scenarios share.
    fn small() -> Self {
        Self::new(&[
            "top.txt",
            "keep/a.txt",
            "keep/sub/b.txt",
            "keep/sub/deeper/c.txt",
            "vendor/d.txt",
            "vendor/nested/e.txt",
        ])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn relative_path(entry: &WalkEntry, root: &Path) -> String {
    entry
        .path()
        .strip_prefix(root)
        .expect("entry below the root")
        .to_string_lossy()
        .replace('\\', "/")
}

fn relative(entries: &[WalkEntry], root: &Path) -> BTreeSet<String> {
    entries
        .iter()
        .map(|entry| relative_path(entry, root))
        .collect()
}

fn set(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

/// Runs `walker` with `verdict` and returns what it kept and every path the
/// visitor was asked about.
fn visit(
    walker: Walker,
    root: &Path,
    verdict: impl Fn(&str) -> Verdict + Sync,
) -> (BTreeSet<String>, BTreeSet<String>, bool) {
    let asked = Mutex::new(BTreeSet::new());
    let result = walker
        .visit(|entry| {
            let path = relative_path(entry, root);
            let decision = verdict(&path);
            asked.lock().expect("visitor lock").insert(path);
            decision
        })
        .expect("walk succeeds");
    assert!(result.errors().is_empty(), "{:?}", result.errors());
    (
        relative(result.entries(), root),
        asked.into_inner().expect("visitor lock"),
        result.was_cancelled(),
    )
}

#[test]
fn a_pruned_directory_is_left_out_and_never_walked() {
    let fixture = Fixture::small();
    let root = &fixture.root;
    for threads in THREADS {
        let (kept, asked, cancelled) = visit(Walker::new(root).threads(threads), root, |path| {
            if path == "vendor" {
                Verdict::Prune
            } else {
                Verdict::Keep
            }
        });
        assert_eq!(
            kept,
            set(&[
                "top.txt",
                "keep",
                "keep/a.txt",
                "keep/sub",
                "keep/sub/b.txt",
                "keep/sub/deeper",
                "keep/sub/deeper/c.txt",
            ]),
            "{threads} thread(s)"
        );
        assert!(
            asked.iter().all(|path| !path.starts_with("vendor/")),
            "{threads} thread(s) asked about {asked:?}"
        );
        assert!(!cancelled);
    }
}

#[test]
fn a_nested_directory_is_pruned_below_its_kept_parent() {
    let fixture = Fixture::small();
    let root = &fixture.root;
    for threads in THREADS {
        let (kept, asked, _) = visit(
            Walker::new(root)
                .threads(threads)
                .options(WalkOptions::default().sort(true)),
            root,
            |path| {
                if path == "keep/sub" {
                    Verdict::Prune
                } else {
                    Verdict::Keep
                }
            },
        );
        assert_eq!(
            kept,
            set(&[
                "top.txt",
                "keep",
                "keep/a.txt",
                "vendor",
                "vendor/d.txt",
                "vendor/nested",
                "vendor/nested/e.txt",
            ]),
            "{threads} thread(s)"
        );
        assert!(asked.iter().all(|path| !path.starts_with("keep/sub/")));
    }
}

/// `Prune` differs from `Skip` only in the subtree: a skipped directory is
/// still walked, a pruned one is not, and for a file the two are the same.
#[test]
fn prune_is_skip_plus_the_subtree() {
    let fixture = Fixture::small();
    let root = &fixture.root;
    for threads in THREADS {
        let (skipped, _, _) = visit(Walker::new(root).threads(threads), root, |path| {
            if path == "vendor" || path == "top.txt" {
                Verdict::Skip
            } else {
                Verdict::Keep
            }
        });
        let (pruned, _, _) = visit(Walker::new(root).threads(threads), root, |path| {
            if path == "vendor" || path == "top.txt" {
                Verdict::Prune
            } else {
                Verdict::Keep
            }
        });
        let vendor_subtree = set(&["vendor/d.txt", "vendor/nested", "vendor/nested/e.txt"]);
        assert!(skipped.is_superset(&vendor_subtree));
        assert_eq!(
            skipped
                .difference(&pruned)
                .cloned()
                .collect::<BTreeSet<_>>(),
            vendor_subtree,
            "{threads} thread(s)"
        );
        assert!(!pruned.contains("top.txt") && !pruned.contains("vendor"));
    }
}

/// A directory emitted without being descended into, here at the depth
/// limit, has no subtree to cut, so pruning it only leaves it out.
#[test]
fn pruning_a_directory_at_the_depth_limit_leaves_it_out() {
    let fixture = Fixture::small();
    let root = &fixture.root;
    for threads in THREADS {
        let (kept, _, _) = visit(
            Walker::new(root)
                .threads(threads)
                .options(WalkOptions::default().max_depth(1)),
            root,
            |path| {
                if path == "keep" {
                    Verdict::Prune
                } else {
                    Verdict::Keep
                }
            },
        );
        assert_eq!(kept, set(&["top.txt", "vendor"]), "{threads} thread(s)");
    }
}

/// The visitor is asked only about entries the filters let through, so a
/// directory is prunable when an include selects it.
#[test]
fn pruning_composes_with_includes_that_select_the_directory() {
    let fixture = Fixture::small();
    let root = &fixture.root;
    for threads in THREADS {
        let (kept, asked, _) = visit(
            Walker::new(root)
                .threads(threads)
                .include("{keep,vendor}/**")
                .expect("valid include"),
            root,
            |path| {
                if path == "vendor" || path == "keep/sub/deeper" {
                    Verdict::Prune
                } else {
                    Verdict::Keep
                }
            },
        );
        assert_eq!(
            kept,
            set(&["keep", "keep/a.txt", "keep/sub", "keep/sub/b.txt"]),
            "{threads} thread(s)"
        );
        assert!(asked.iter().all(|path| !path.starts_with("vendor/")));
    }
}

/// Every other top-level directory of a tree wide enough for the parallel
/// walk to use its helpers is pruned; the rest is walked completely.
#[test]
fn pruning_many_root_level_directories_in_a_wide_parallel_walk() {
    let mut files = Vec::new();
    for directory in 0..96 {
        for file in 0..16 {
            files.push(format!("d{directory:02}/f{file:02}.txt"));
        }
        for file in 0..4 {
            files.push(format!("d{directory:02}/inner/g{file}.txt"));
        }
    }
    let fixture = Fixture::new(&files.iter().map(String::as_str).collect::<Vec<_>>());
    let root = &fixture.root;
    let is_pruned = |path: &str| {
        path.strip_prefix('d')
            .and_then(|rest| rest.get(..2))
            .and_then(|index| index.parse::<usize>().ok())
            .is_some_and(|index| index % 2 == 1)
    };
    let expected = files
        .iter()
        .filter(|file| !is_pruned(file))
        .flat_map(|file| {
            let directory = &file[..3];
            let mut entries = vec![directory.to_owned(), file.clone()];
            if file.contains("/inner/") {
                entries.push(format!("{directory}/inner"));
            }
            entries
        })
        .collect::<BTreeSet<_>>();
    for threads in THREADS {
        let (kept, asked, cancelled) = visit(Walker::new(root).threads(threads), root, |path| {
            if !path.contains('/') && is_pruned(path) {
                Verdict::Prune
            } else {
                Verdict::Keep
            }
        });
        assert_eq!(kept, expected, "{threads} thread(s)");
        assert!(
            asked
                .iter()
                .all(|path| !path.contains('/') || !is_pruned(path)),
            "{threads} thread(s) walked into a pruned directory"
        );
        assert!(!cancelled);
    }
}

/// A stop still ends the walk while other directories are being pruned, and
/// a pruned subtree stays unwalked whichever came first.
#[test]
fn prune_and_stop_in_one_walk() {
    let fixture = Fixture::small();
    let root = &fixture.root;
    for threads in THREADS {
        let (kept, asked, cancelled) =
            visit(
                Walker::new(root).threads(threads),
                root,
                |path| match path {
                    "vendor" => Verdict::Prune,
                    "keep/sub/b.txt" => Verdict::Stop,
                    _ => Verdict::Keep,
                },
            );
        assert!(asked.iter().all(|path| !path.starts_with("vendor/")));
        assert!(!kept.contains("vendor") && !kept.contains("keep/sub/b.txt"));
        // The stop is only reached if the walk got that far first; when it
        // did, the walk reports it.
        assert_eq!(
            cancelled,
            asked.contains("keep/sub/b.txt"),
            "{threads} thread(s)"
        );
    }

    // A stop on a directory ends the walk before its subtree is read.
    for threads in THREADS {
        let (kept, asked, cancelled) = visit(Walker::new(root).threads(threads), root, |path| {
            if path == "keep" {
                Verdict::Stop
            } else {
                Verdict::Prune
            }
        });
        assert!(asked.iter().all(|path| !path.contains('/')), "{asked:?}");
        assert!(kept.is_empty());
        assert!(cancelled);
    }
}

#[test]
fn cancellation_ends_a_walk_that_prunes() {
    let fixture = Fixture::small();
    let root = &fixture.root;
    for threads in THREADS {
        let token = CancellationToken::default();
        let (kept, asked, cancelled) = visit(
            Walker::new(root)
                .threads(threads)
                .cancellation(token.clone()),
            root,
            |path| {
                if path == "vendor" {
                    token.cancel();
                    Verdict::Prune
                } else {
                    Verdict::Keep
                }
            },
        );
        // A walk checks the token between directories and every few dozen
        // entries, so one with nothing left to read may finish first; then it
        // finished completely.
        assert!(
            cancelled
                || kept
                    == set(&[
                        "top.txt",
                        "keep",
                        "keep/a.txt",
                        "keep/sub",
                        "keep/sub/b.txt",
                        "keep/sub/deeper",
                        "keep/sub/deeper/c.txt",
                    ]),
            "{threads} thread(s): {kept:?}"
        );
        assert!(!kept.contains("vendor"));
        assert!(asked.iter().all(|path| !path.starts_with("vendor/")));

        // A token cancelled before the walk starts ends it before any verdict.
        let token = CancellationToken::default();
        token.cancel();
        let (kept, asked, cancelled) = visit(
            Walker::new(root).threads(threads).cancellation(token),
            root,
            |_| Verdict::Prune,
        );
        assert!(cancelled && kept.is_empty() && asked.is_empty());
    }
}
