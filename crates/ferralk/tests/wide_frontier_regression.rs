//! Peak-live-heap regression coverage for wide directory frontiers.
//!
//! Allocation-count gates live in the neighbouring integration tests. This
//! process instead prefixes allocations with a measurement epoch so it can
//! distinguish buffers created during a walk from fixture and test-harness
//! allocations that happen to be freed while the workers run.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    fs, io,
    path::{Path, PathBuf},
    ptr,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::Walker;

const DEFAULT_WIDE_DIRECTORY_COUNT: usize = 10_000;
static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

#[allow(unsafe_code)]
mod live_heap {
    use super::*;

    static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);
    static ACTIVE_EPOCH: AtomicU64 = AtomicU64::new(0);
    static CURRENT_BYTES: AtomicU64 = AtomicU64::new(0);
    static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Header {
        epoch: u64,
    }

    pub(super) struct CountingAllocator;

    #[global_allocator]
    static GLOBAL: CountingAllocator = CountingAllocator;

    fn combined_layout(layout: Layout) -> (Layout, usize) {
        let (combined, offset) = Layout::new::<Header>()
            .extend(layout)
            .expect("allocation layout fits");
        (combined.pad_to_align(), offset)
    }

    fn add(bytes: usize) {
        let live = CURRENT_BYTES.fetch_add(bytes as u64, Ordering::Relaxed) + bytes as u64;
        PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
    }

    fn subtract(bytes: usize) {
        CURRENT_BYTES.fetch_sub(bytes as u64, Ordering::Relaxed);
    }

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let (combined, offset) = combined_layout(layout);
            let base = unsafe { System.alloc(combined) };
            if base.is_null() {
                return base;
            }
            let epoch = ACTIVE_EPOCH.load(Ordering::Relaxed);
            unsafe { base.cast::<Header>().write(Header { epoch }) };
            if epoch != 0 {
                add(layout.size());
            }
            unsafe { base.add(offset) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let (combined, offset) = combined_layout(layout);
            let base = unsafe { System.alloc_zeroed(combined) };
            if base.is_null() {
                return base;
            }
            let epoch = ACTIVE_EPOCH.load(Ordering::Relaxed);
            unsafe { base.cast::<Header>().write(Header { epoch }) };
            if epoch != 0 {
                add(layout.size());
            }
            unsafe { base.add(offset) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            let (combined, offset) = combined_layout(layout);
            let base = unsafe { pointer.sub(offset) };
            let epoch = unsafe { base.cast::<Header>().read().epoch };
            if epoch != 0 && epoch == ACTIVE_EPOCH.load(Ordering::Relaxed) {
                subtract(layout.size());
            }
            unsafe { System.dealloc(base, combined) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let (combined, offset) = combined_layout(layout);
            let base = unsafe { pointer.sub(offset) };
            let old_epoch = unsafe { base.cast::<Header>().read().epoch };
            let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
            let (new_combined, new_offset) = combined_layout(new_layout);
            debug_assert_eq!(offset, new_offset);
            let new_base = unsafe { System.realloc(base, combined, new_combined.size()) };
            if new_base.is_null() {
                return ptr::null_mut();
            }
            let active = ACTIVE_EPOCH.load(Ordering::Relaxed);
            let new_epoch = if active == 0 { old_epoch } else { active };
            unsafe {
                new_base.cast::<Header>().write(Header { epoch: new_epoch });
            }
            if active != 0 {
                if old_epoch == active {
                    if new_size >= layout.size() {
                        add(new_size - layout.size());
                    } else {
                        subtract(layout.size() - new_size);
                    }
                } else {
                    add(new_size);
                }
            }
            unsafe { new_base.add(new_offset) }
        }
    }

    #[derive(Clone, Copy, Debug)]
    pub(super) struct Measurement {
        pub(super) live_bytes: u64,
        pub(super) peak_bytes: u64,
    }

    pub(super) fn start() {
        assert_eq!(ACTIVE_EPOCH.load(Ordering::Relaxed), 0);
        CURRENT_BYTES.store(0, Ordering::Relaxed);
        PEAK_BYTES.store(0, Ordering::Relaxed);
        let epoch = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
        assert_ne!(epoch, 0);
        ACTIVE_EPOCH.store(epoch, Ordering::Relaxed);
    }

    pub(super) fn finish() -> Measurement {
        assert_ne!(ACTIVE_EPOCH.swap(0, Ordering::Relaxed), 0);
        Measurement {
            live_bytes: CURRENT_BYTES.load(Ordering::Relaxed),
            peak_bytes: PEAK_BYTES.load(Ordering::Relaxed),
        }
    }
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn wide(count: usize) -> Self {
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "ferralk-wide-frontier-{}-{}",
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
                Err(error) => panic!("create wide-frontier fixture: {error}"),
            }
        };
        for index in 0..count {
            fs::create_dir(root.join(format!("child-{index:05}")))
                .expect("create wide-frontier child");
        }
        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn serial_peak(root: &Path, expected: usize) -> live_heap::Measurement {
    live_heap::start();
    let result = Walker::new(root)
        .threads(1)
        .collect()
        .expect("serial wide walk succeeds");
    let measurement = live_heap::finish();
    assert_eq!(result.entries().len(), expected);
    std::hint::black_box(result);
    measurement
}

fn stream_peak(root: &Path, expected: usize) -> live_heap::Measurement {
    live_heap::start();
    let entries = Walker::new(root)
        .stream()
        .collect::<Result<Vec<_>, _>>()
        .expect("streaming wide walk succeeds");
    let measurement = live_heap::finish();
    assert_eq!(entries.len(), expected);
    std::hint::black_box(entries);
    measurement
}

fn parallel_peak(root: &Path, expected: usize) -> live_heap::Measurement {
    live_heap::start();
    let result = Walker::new(root)
        .threads(4)
        .collect()
        .expect("parallel wide walk succeeds");
    let measurement = live_heap::finish();
    assert_eq!(result.entries().len(), expected);
    std::hint::black_box(result);
    measurement
}

#[test]
fn wide_frontiers_stay_within_their_peak_live_heap_budgets() {
    let count = std::env::var("FERRALK_WIDE_FRONTIER_COUNT")
        .map(|value| value.parse().expect("wide-frontier count is an integer"))
        .unwrap_or(DEFAULT_WIDE_DIRECTORY_COUNT);
    let fixture = Fixture::wide(count);
    let serial = serial_peak(&fixture.root, count);
    let stream = stream_peak(&fixture.root, count);
    let parallel = parallel_peak(&fixture.root, count);

    eprintln!(
        "{count} empty directories: serial peak={} B, stream peak={} B, parallel peak={} B, parallel result={} B",
        serial.peak_bytes, stream.peak_bytes, parallel.peak_bytes, parallel.live_bytes
    );

    assert!(
        stream.peak_bytes <= serial.peak_bytes * 2,
        "stream peak {} exceeds twice serial peak {}",
        stream.peak_bytes,
        serial.peak_bytes
    );
    assert!(parallel.live_bytes > 0, "parallel result owns live heap");
    assert!(
        parallel.peak_bytes < parallel.live_bytes * 2,
        "parallel peak {} is not below twice result heap {}",
        parallel.peak_bytes,
        parallel.live_bytes
    );
}
