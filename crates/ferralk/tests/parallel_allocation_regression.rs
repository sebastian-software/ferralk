//! Process-wide allocation regression coverage for the parallel walker.
//!
//! The precise serial gate uses `allocation-counter`, whose counters are
//! thread-local. This separate integration-test process owns a coarse atomic
//! allocator instead, so allocations made by scoped worker threads are visible.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    fs,
    hint::black_box,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{Verdict, Walker};

const ENTRIES_PER_DIRECTORY: usize = 64;
// Both arms must clear the helper floor on their own. Sixteen equally wide
// siblings leave enough queued work for a helper after the work floor trips.
const SMALL_DIRECTORY_COUNT: usize = 16;
const LARGE_DIRECTORY_COUNT: usize = SMALL_DIRECTORY_COUNT * 2;
// Thread startup, deque growth and shard handoff are coarse process-wide noise.
// This stays far below the 1,024 added entries, so one allocation per added
// worker entry cannot hide inside it.
const PARALLEL_GROWTH_BUDGET: u64 = 128;
static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

#[allow(unsafe_code)]
mod process_counter {
    use super::*;

    static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
    static COUNTING: AtomicBool = AtomicBool::new(false);

    struct CountingAllocator;

    #[global_allocator]
    static GLOBAL: CountingAllocator = CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count();
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count();
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count();
            unsafe { System.realloc(pointer, layout, new_size) }
        }
    }

    fn count() {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn start() {
        assert!(!COUNTING.load(Ordering::Relaxed));
        ALLOCATIONS.store(0, Ordering::Relaxed);
        assert!(!COUNTING.swap(true, Ordering::Relaxed));
    }

    pub(super) fn finish() -> u64 {
        assert!(COUNTING.swap(false, Ordering::Relaxed));
        ALLOCATIONS.load(Ordering::Relaxed)
    }
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(directories: usize) -> Self {
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "ferralk-parallel-allocations-{}-{}",
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
                Err(error) => panic!("create parallel allocation fixture: {error}"),
            }
        };
        for directory in 0..directories {
            let directory = root.join(format!("batch-{directory}"));
            fs::create_dir(&directory).expect("create parallel allocation fixture batch");
            for index in 0..ENTRIES_PER_DIRECTORY {
                fs::write(directory.join(format!("entry-{index:03}.dat")), b"fixture")
                    .expect("write parallel allocation fixture entry");
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

fn count_parallel_walk(root: &Path) -> u64 {
    let walker = Walker::new(root).threads(4);
    let caller = thread::current().id();
    let worker_seen = AtomicBool::new(false);
    let visited = AtomicUsize::new(0);

    process_counter::start();
    let outcome = walker.visit(|_| {
        visited.fetch_add(1, Ordering::Relaxed);
        if thread::current().id() != caller {
            worker_seen.store(true, Ordering::Relaxed);
        }
        Verdict::Skip
    });
    let allocations = process_counter::finish();

    let outcome = outcome.expect("parallel allocation fixture walk succeeds");
    assert!(outcome.entries().is_empty());
    black_box(outcome);
    assert!(
        worker_seen.load(Ordering::Relaxed),
        "the allocation pin must observe an actual helper thread"
    );
    assert!(
        visited.load(Ordering::Relaxed) >= ENTRIES_PER_DIRECTORY * SMALL_DIRECTORY_COUNT,
        "the allocation pin must reach the fixture entries"
    );
    allocations
}

#[test]
fn parallel_workers_do_not_gain_per_entry_allocations() {
    let small = Fixture::new(SMALL_DIRECTORY_COUNT);
    let large = Fixture::new(LARGE_DIRECTORY_COUNT);

    // The measured pass gets the same process and filesystem warm-up for both
    // fixture sizes while still constructing a fresh worker pool each time.
    black_box(count_parallel_walk(&small.root));
    let small_allocations = count_parallel_walk(&small.root);
    black_box(count_parallel_walk(&large.root));
    let large_allocations = count_parallel_walk(&large.root);

    let growth = large_allocations
        .checked_sub(small_allocations)
        .unwrap_or_else(|| {
            panic!("the larger fixture allocated less ({large_allocations} < {small_allocations})")
        });
    let added_entries = (LARGE_DIRECTORY_COUNT - SMALL_DIRECTORY_COUNT) * ENTRIES_PER_DIRECTORY;
    // The serial allocation pin establishes these unavoidable reader floors:
    // Linux ReadDir owns a CString and an OsString for each name, while other
    // portable readers own one OsString. Native readers reuse their buffers.
    let backend_floor = if cfg!(any(
        all(feature = "native-linux", target_os = "linux"),
        all(feature = "native-macos", target_os = "macos")
    )) {
        0
    } else if cfg!(target_os = "linux") {
        (added_entries * 2) as u64
    } else {
        added_entries as u64
    };
    let allowed = backend_floor + PARALLEL_GROWTH_BUDGET;
    assert!(
        growth <= allowed,
        "doubling the parallel fixture grew allocations by {growth}; backend floor plus \
         scheduler budget is {allowed} ({small_allocations} -> {large_allocations})"
    );
}
