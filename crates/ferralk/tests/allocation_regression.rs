//! Allocation-count regression coverage for the matcher and walker hot paths.
//!
//! This is deliberately one test in its own integration-test process. The
//! replacement allocator is process-global while measurements are thread-local;
//! keeping the windows serial and the walker on one thread makes their scope
//! explicit and repeatable.

use std::{
    fs,
    hint::black_box,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{
    Verdict, Walker,
    ferralk_glob::{Pattern, PatternOptions},
};

const ENTRIES_PER_DIRECTORY: usize = 64;
const CONSTANT_GROWTH_BUDGET: u64 = 16;
static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn plain(directories: usize) -> Self {
        Self::new(directories, false)
    }

    fn ignored(directories: usize) -> Self {
        Self::new(directories, true)
    }

    fn new(directories: usize, ignored: bool) -> Self {
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "ferralk-allocations-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after epoch")
                    .as_nanos()
                    + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create allocation fixture: {error}"),
            }
        };
        if ignored {
            fs::write(root.join(".gitignore"), b"ignored-*.tmp\n")
                .expect("write allocation fixture ignore rules");
        }
        for directory in 0..directories {
            let directory = root.join(format!("batch-{directory}"));
            fs::create_dir(&directory).expect("create allocation fixture batch");
            for index in 0..ENTRIES_PER_DIRECTORY {
                let name = if ignored {
                    format!("ignored-{index:03}.tmp")
                } else {
                    format!("entry-{index:03}.dat")
                };
                fs::write(directory.join(name), b"fixture")
                    .expect("write allocation fixture entry");
            }
        }
        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn count_skipping_walk(root: &Path, gitignore: bool) -> u64 {
    let walker = Walker::new(root).threads(1).respect_git_ignore(gitignore);
    let visited = AtomicUsize::new(0);
    let mut outcome = None;
    let allocations = allocation_counter::measure(|| {
        outcome = Some(
            walker
                .visit(|_| {
                    visited.fetch_add(1, Ordering::Relaxed);
                    Verdict::Skip
                })
                .expect("allocation fixture walk succeeds"),
        );
    });
    let outcome = outcome.expect("measurement produced a walk result");
    assert!(outcome.entries().is_empty());
    black_box(outcome);
    let visited = visited.load(Ordering::Relaxed);
    if gitignore {
        assert!(
            visited < ENTRIES_PER_DIRECTORY,
            "ignore rules must filter the candidate files"
        );
    } else {
        assert!(
            visited >= ENTRIES_PER_DIRECTORY,
            "plain walks must reach the candidate files"
        );
    }
    allocations.count_total
}

fn assert_walk_growth(label: &str, one_batch: u64, two_batches: u64) {
    let growth = two_batches.checked_sub(one_batch).unwrap_or_else(|| {
        panic!("{label}: the larger fixture allocated less ({two_batches} < {one_batch})")
    });
    // After the first sibling directory has populated the reusable listing,
    // native readers should pay only constant directory/task setup for the
    // equally wide second sibling. Portable readers always pay for the
    // OsString returned by DirEntry::file_name; on Linux, std::fs::ReadDir also
    // copies each readdir name into an owned CString before yielding it.
    let backend_floor = if cfg!(any(
        all(feature = "native-linux", target_os = "linux"),
        all(feature = "native-macos", target_os = "macos")
    )) {
        0
    } else if cfg!(target_os = "linux") {
        (ENTRIES_PER_DIRECTORY * 2) as u64
    } else {
        ENTRIES_PER_DIRECTORY as u64
    };
    let allowed = backend_floor + CONSTANT_GROWTH_BUDGET;
    assert!(
        growth <= allowed,
        "{label}: a second {ENTRIES_PER_DIRECTORY}-entry sibling grew allocations by {growth}; \
         backend floor plus constant budget is {allowed} ({one_batch} -> {two_batches})"
    );
}

#[test]
fn hot_paths_keep_their_allocation_floors() {
    let pattern = Pattern::compile(
        "**/*.rs",
        PatternOptions::default().recursive_double_star(true),
    )
    .expect("compile allocation fixture pattern");
    assert!(pattern.is_match_glob_path("src/deep/module.rs"));
    assert!(!pattern.is_match_glob_path("src/deep/module.ts"));
    let matcher = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(pattern.is_match_glob_path(black_box("src/deep/module.rs")));
            black_box(pattern.is_match_glob_path(black_box("src/deep/module.ts")));
        }
    });
    assert_eq!(
        matcher.count_total, 0,
        "warm compiled matches must not allocate"
    );

    let small = Fixture::plain(1);
    let large = Fixture::plain(2);
    let plain_small = count_skipping_walk(&small.root, false);
    let plain_large = count_skipping_walk(&large.root, false);

    let ignored_small = Fixture::ignored(1);
    let ignored_large = Fixture::ignored(2);
    let gitignore_small = count_skipping_walk(&ignored_small.root, true);
    let gitignore_large = count_skipping_walk(&ignored_large.root, true);

    assert_walk_growth("plain serial walk", plain_small, plain_large);
    assert_walk_growth(
        "gitignore-enabled serial walk",
        gitignore_small,
        gitignore_large,
    );
}
