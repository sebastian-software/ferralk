#![forbid(unsafe_code)]
//! Walker benchmarks, measured as wall time.
//!
//! These benches do real filesystem work and use several threads, so elapsed
//! time is the only unit that says anything about them: an instruction count
//! would report the cost of a parallel walk as though it had run serially.
//! They run in the wall-time lane in `.github/workflows/walker-bench.yml`,
//! which since 2026-08-19 is the repository's only automated performance lane.
//!
//! **Cache assumption:** every fixture is written immediately before the
//! measurement, so its directory entries and inodes are in the page cache.
//! These numbers describe warm-cache traversal. Cold-cache behaviour is a
//! different measurement and is not made here.

use std::{
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use criterion::{Criterion, criterion_group, criterion_main};
use ferralk::{Verdict, WalkOptions, Walker};
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState, overrides::OverrideBuilder};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

/// Benchmark-id prefix, so a native-backend run does not overwrite the
/// portable run's series when both report to the same collector.
const LANE: &str = if cfg!(feature = "native-linux") {
    "walker_native_linux"
} else if cfg!(feature = "native-macos") {
    "walker_native_macos"
} else {
    "walker"
};

/// A nested directory tree of known shape.
struct Fixture {
    root: PathBuf,
    files: usize,
}

impl Fixture {
    /// Builds `branches` chains that are `depth` directories deep, each
    /// directory holding `files_per_directory` files, half of them matching.
    ///
    /// The directories nest — `branch-3/depth-0/depth-1/depth-2` — so `**`
    /// recursion actually descends. An earlier revision created the `depth-N`
    /// directories as siblings, which fixed the effective depth at two.
    fn new(branches: usize, depth: usize, files_per_directory: usize) -> Self {
        let unique = format!(
            "ferralk-bench-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        );
        let root = std::env::temp_dir().join(unique);
        let mut files = 0;
        for branch in 0..branches {
            let mut directory = root.join(format!("branch-{branch}"));
            for level in 0..depth {
                directory = directory.join(format!("depth-{level}"));
                fs::create_dir_all(&directory).expect("create benchmark directory");
                for index in 0..files_per_directory {
                    let name = if index % 2 == 0 {
                        format!("match-{index}.rs")
                    } else {
                        format!("skip-{index}.txt")
                    };
                    fs::write(directory.join(name), b"fixture").expect("write benchmark file");
                    files += 1;
                }
            }
        }
        Self { root, files }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn walker(c: &mut Criterion) {
    // 16 chains, four levels deep, two files per directory: the same 128 files
    // the previous fixture held, now at an effective depth of four.
    let small = Fixture::new(16, 4, 2);
    // A repository-sized tree: 5120 files across 320 directories, eight levels
    // deep, where per-entry cost stops being lost in fixed overhead.
    let large = Fixture::new(40, 8, 16);
    assert!(
        large.files >= 5000,
        "the large fixture must exceed 5k files"
    );

    // A tree the size of the Palamedes trial's synthetic case, where every
    // parallel arm lost to its own serial form.
    let mini = Fixture::new(2, 1, 6);
    assert_eq!(mini.files, 12, "the mini fixture models the trial's tree");

    bench_tree(c, "", &small);
    bench_tree(c, "large/", &large);
    bench_caller_matching(c, "large/", &large);
    bench_caller_matching(c, "mini/", &mini);
}

/// The shape the Palamedes trial measured: the caller keeps a matcher of its
/// own and the walker only has to find the candidates.
///
/// `collect_then_filter` is the arm the trial ran, and the one that lost to a
/// hand-pruned parallel `ignore` at four threads — the walk parallelises and
/// the caller's `GlobSet` then runs over every entry on one thread.
/// `visit_in_worker` is the same matcher moved into the workers.
fn bench_caller_matching(c: &mut Criterion, group: &str, fixture: &Fixture) {
    let matcher = caller_matcher();

    c.bench_function(
        &format!("{LANE}/{group}caller_match/collect_then_filter"),
        |benchmark| {
            benchmark.iter(|| {
                let result = Walker::new(&fixture.root)
                    .threads(4)
                    .options(WalkOptions::default())
                    .collect()
                    .expect("benchmark walk succeeds");
                let kept = result
                    .entries()
                    .iter()
                    .filter(|entry| matcher.is_match(entry.path()))
                    .count();
                black_box(kept)
            })
        },
    );

    c.bench_function(
        &format!("{LANE}/{group}caller_match/visit_in_worker"),
        |benchmark| {
            benchmark.iter(|| {
                let result = Walker::new(&fixture.root)
                    .threads(4)
                    .options(WalkOptions::default())
                    .visit(|entry| {
                        if matcher.is_match(entry.path()) {
                            Verdict::Keep
                        } else {
                            Verdict::Skip
                        }
                    })
                    .expect("benchmark walk succeeds");
                black_box(result.entries().len())
            })
        },
    );

    c.bench_function(
        &format!("{LANE}/{group}caller_match/collect_only"),
        |benchmark| {
            benchmark.iter(|| {
                let result = Walker::new(&fixture.root)
                    .threads(4)
                    .options(WalkOptions::default())
                    .collect()
                    .expect("benchmark walk succeeds");
                black_box(result.entries().len())
            })
        },
    );

    c.bench_function(
        &format!("{LANE}/{group}caller_match/ignore_parallel"),
        |benchmark| {
            benchmark.iter(|| {
                black_box(ignore_parallel_caller_matched(&fixture.root, &matcher));
            })
        },
    );
}

/// The caller's own matcher, standing in for the `GlobSet` Palamedes keeps for
/// parity. Deliberately not a ferralk pattern: the point is a predicate the
/// walker cannot absorb.
fn caller_matcher() -> GlobSet {
    // A catalog rather than one pattern. Palamedes carries roughly this many
    // source globs, and a single-pattern set is cheap enough per entry that a
    // serial pass over the result hides inside the walk.
    let mut builder = GlobSetBuilder::new();
    for extension in [
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "toml", "yaml", "yml", "md", "css",
        "scss", "html", "py", "go", "java", "kt", "swift", "c", "h", "cpp", "hpp",
    ] {
        builder.add(Glob::new(&format!("**/*.{extension}")).expect("benchmark glob is valid"));
    }
    builder.build().expect("benchmark glob set builds")
}

/// The baseline: `ignore` in parallel with the same matcher applied in-worker,
/// which is what ferralk had no answer to before `visit`.
fn ignore_parallel_caller_matched(root: &Path, matcher: &GlobSet) -> usize {
    let kept = AtomicUsize::new(0);
    let mut builder = WalkBuilder::new(root);
    builder.threads(4).standard_filters(false);
    builder.build_parallel().run(|| {
        Box::new(|entry| {
            let entry = entry.expect("benchmark walk succeeds");
            if matcher.is_match(entry.path()) {
                kept.fetch_add(1, Ordering::Relaxed);
            }
            WalkState::Continue
        })
    });
    kept.load(Ordering::Relaxed)
}

fn bench_tree(c: &mut Criterion, group: &str, fixture: &Fixture) {
    let options = WalkOptions::default();

    c.bench_function(&format!("{LANE}/{group}serial_filtered"), |benchmark| {
        benchmark.iter(|| {
            black_box(
                Walker::new(&fixture.root)
                    .threads(1)
                    .include("**/*.rs")
                    .expect("benchmark include is valid")
                    .options(options)
                    .collect()
                    .expect("benchmark walk succeeds"),
            )
        })
    });
    c.bench_function(&format!("{LANE}/{group}parallel_filtered"), |benchmark| {
        benchmark.iter(|| {
            black_box(
                Walker::new(&fixture.root)
                    .threads(4)
                    .include("**/*.rs")
                    .expect("benchmark include is valid")
                    .options(options)
                    .collect()
                    .expect("benchmark walk succeeds"),
            )
        })
    });
    c.bench_function(
        &format!("{LANE}/{group}ignore_parallel_filtered"),
        |benchmark| {
            benchmark.iter(|| {
                ignore_parallel_filtered(&fixture.root);
                black_box(())
            })
        },
    );
}

fn ignore_parallel_filtered(root: &Path) {
    let mut overrides = OverrideBuilder::new(root);
    overrides
        .add("**/*.rs")
        .expect("benchmark include is valid");
    let mut builder = WalkBuilder::new(root);
    builder
        .threads(4)
        .standard_filters(false)
        .overrides(overrides.build().expect("benchmark override builds"));
    builder.build_parallel().run(|| {
        Box::new(|entry| {
            black_box(entry.expect("benchmark walk succeeds"));
            WalkState::Continue
        })
    });
}

criterion_group!(benches, walker);
criterion_main!(benches);
