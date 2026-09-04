//! The worker budget a walk uses when the caller sizes none.
//!
//! A warm metadata walk is not CPU work. Over the 53,600-file repository
//! fixture the serial macOS-native walk spends 91% of its samples inside
//! `openat` and `getdirentries64`, and a raw C walker that issues the same
//! syscalls and does nothing else reaches the same wall time. What limits it
//! is how many threads the kernel's namespace layer serves at once, not how
//! many cores are idle.
//!
//! That ceiling is well below `available_parallelism` on Apple Silicon. On a
//! 10-core M1 Pro (8 performance cores in two clusters of four, 2 efficiency
//! cores) aggregate throughput peaks at four concurrent walkers and then
//! falls sharply: the same fixture takes 19.8 ms at four threads, 25.7 ms at
//! five, and 34.8 ms at the ten `available_parallelism` reports. The knee is
//! not ferralk's: the raw C walker and zlob 1.6.5 both show it at the same
//! point, and a pure-CPU control scales flat to eight threads on the same
//! host, so it is neither thermal nor scheduler noise.
//!
//! Four is also the host's `hw.perflevel0.cpusperl2` — the number of
//! performance cores sharing one L2. Threads that stay inside one performance
//! cluster keep the kernel's shared namespace state in that cluster's cache;
//! spilling to the second cluster turns every contended line into
//! cross-cluster traffic. That is a mechanism the sysctl already names, so
//! the ceiling is read from it rather than hard-coded, and it applies only
//! where it was measured: Apple Silicon macOS.
//!
//! [`crate::Walker::threads`] overrides this. The ceiling shapes the default
//! for callers who express no preference, and a caller who knows their
//! filesystem rewards more concurrency still asks for it.

use std::num::NonZeroUsize;

/// The default worker budget: what the host reports, held under the ceiling
/// the platform's metadata layer actually scales to, and under
/// [`crate::MAX_WORKERS`].
///
/// Every term is at least one, so the walk always keeps its calling thread.
pub(crate) fn default_threads() -> usize {
    let available = std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1);
    available
        .min(metadata_concurrency_ceiling())
        .min(crate::MAX_WORKERS)
}

/// No measured ceiling for this platform: use every core the host reports.
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
const fn metadata_concurrency_ceiling() -> usize {
    usize::MAX
}

/// One Apple Silicon performance cluster, or no ceiling when the host does
/// not report one.
///
/// The result is read once. `sysctlbyname` costs about a microsecond, which
/// is small next to a walk but not next to constructing a [`crate::Walker`]
/// that a caller may build in a loop.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn metadata_concurrency_ceiling() -> usize {
    use std::sync::OnceLock;

    static CEILING: OnceLock<usize> = OnceLock::new();
    *CEILING.get_or_init(|| {
        // Two is the smallest cluster any Apple Silicon performance level
        // reports; a smaller or absent answer means the host is not the shape
        // this ceiling was measured on, so it imposes none.
        performance_cluster_width()
            .filter(|width| *width >= 2)
            .unwrap_or(usize::MAX)
    })
}

/// Performance cores sharing one L2, as the kernel reports them.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn performance_cluster_width() -> Option<usize> {
    let mut width: u32 = 0;
    let mut size = std::mem::size_of::<u32>();
    // SAFETY: the name is a NUL-terminated literal, the output pointer
    // addresses a live `u32` whose length is passed in `size`, and a read-only
    // query passes a null new-value pointer with a zero length, which is what
    // `sysctlbyname` documents for that case.
    let status = unsafe {
        libc::sysctlbyname(
            c"hw.perflevel0.cpusperl2".as_ptr(),
            (&raw mut width).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size != std::mem::size_of::<u32>() {
        return None;
    }
    Some(width as usize)
}

#[cfg(test)]
mod tests {
    use super::{default_threads, metadata_concurrency_ceiling};

    #[test]
    fn default_is_a_usable_worker_count() {
        let threads = default_threads();
        assert!(threads >= 1, "the default must start at least one worker");
        assert!(
            threads <= crate::MAX_WORKERS,
            "the default must respect the eager worker ceiling"
        );
        assert!(
            threads
                <= std::thread::available_parallelism()
                    .map(std::num::NonZeroUsize::get)
                    .unwrap_or(1),
            "the default must not exceed what the host reports"
        );
    }

    #[test]
    fn the_ceiling_never_starves_a_walk() {
        assert!(
            metadata_concurrency_ceiling() >= 2,
            "a ceiling below two would make the default serial on every host"
        );
    }

    /// The ceiling is a property of the host, so a walk that asks for more
    /// still gets it: this is a default, not a clamp.
    #[test]
    fn an_explicit_budget_is_not_capped_by_the_ceiling() {
        let asked = metadata_concurrency_ceiling().saturating_add(1).min(64);
        let walker = crate::Walker::new(".").threads(asked);
        assert_eq!(walker.threads, asked);
    }
}
