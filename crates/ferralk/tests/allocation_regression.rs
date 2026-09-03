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
/// An ordinary directory width, wider than the 64-entry batch the parallel
/// walk once split listings into. A directory this size must be classified in
/// one piece: split, its name buffers travelled into the continuation and a
/// sibling started over with none.
const WIDE_ENTRIES_PER_DIRECTORY: usize = 100;
const CONSTANT_GROWTH_BUDGET: u64 = 16;
static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn plain(directories: usize) -> Self {
        Self::new(directories, ENTRIES_PER_DIRECTORY, false)
    }

    fn ignored(directories: usize) -> Self {
        Self::new(directories, ENTRIES_PER_DIRECTORY, true)
    }

    fn wide(directories: usize) -> Self {
        Self::new(directories, WIDE_ENTRIES_PER_DIRECTORY, false)
    }

    fn new(directories: usize, entries_per_directory: usize, ignored: bool) -> Self {
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
            for index in 0..entries_per_directory {
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
    count_skipping_walk_with(root, 1, ENTRIES_PER_DIRECTORY, gitignore)
}

/// Counts the allocations of one walk on this thread. With more than one
/// thread configured the walk takes the parallel route, but a fixture below
/// the helper floor is drained by the caller alone, so what this thread
/// counts is the whole walk, and it is repeatable.
fn count_skipping_walk_with(
    root: &Path,
    threads: usize,
    entries_per_directory: usize,
    gitignore: bool,
) -> u64 {
    let walker = Walker::new(root)
        .threads(threads)
        .respect_git_ignore(gitignore);
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
            visited < entries_per_directory,
            "ignore rules must filter the candidate files"
        );
    } else {
        assert!(
            visited >= entries_per_directory,
            "plain walks must reach the candidate files"
        );
    }
    allocations.count_total
}

fn assert_walk_growth(label: &str, one_batch: u64, two_batches: u64) {
    assert_walk_growth_per_entry(label, ENTRIES_PER_DIRECTORY, one_batch, two_batches);
}

fn assert_walk_growth_per_entry(
    label: &str,
    entries_per_directory: usize,
    one_batch: u64,
    two_batches: u64,
) {
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
        (entries_per_directory * 2) as u64
    } else {
        entries_per_directory as u64
    };
    let allowed = backend_floor + CONSTANT_GROWTH_BUDGET;
    assert!(
        growth <= allowed,
        "{label}: a second {entries_per_directory}-entry sibling grew allocations by {growth}; \
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

    // On the parallel route a directory of ordinary width is classified in
    // one piece, so the second sibling reads into the name buffers the first
    // one left behind. A listing batch smaller than the directory would hand
    // those buffers off with the continuation and start the sibling from
    // nothing.
    let wide_small = Fixture::wide(1);
    let wide_large = Fixture::wide(2);
    let wide_one = count_skipping_walk_with(&wide_small.root, 4, WIDE_ENTRIES_PER_DIRECTORY, false);
    let wide_two = count_skipping_walk_with(&wide_large.root, 4, WIDE_ENTRIES_PER_DIRECTORY, false);
    assert_walk_growth_per_entry(
        "parallel walk over wide siblings",
        WIDE_ENTRIES_PER_DIRECTORY,
        wide_one,
        wide_two,
    );
}
