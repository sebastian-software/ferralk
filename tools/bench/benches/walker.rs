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
use ferralk::{Verdict, WalkOptions, Walker, WildcardMode};
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

    /// Builds one very deep chain with short components, keeping the
    /// complete path below `PATH_MAX` while making full-path directory opens
    /// repeatedly resolve every ancestor.
    fn deep(depth: usize) -> Self {
        let unique = format!(
            "ferralk-bench-deep-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir(&root).expect("create deep benchmark root");
        let mut directory = root.clone();
        for _ in 0..depth {
            directory.push("d");
            fs::create_dir(&directory).expect("create deep benchmark directory");
            fs::write(directory.join("match.rs"), b"fixture").expect("write deep benchmark file");
        }
        Self { root, files: depth }
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
    // Parent-relative opens have constant component-resolution work per
    // level; full-path opens repeat every ancestor. This fixture isolates that
    // asymptotic difference from the wide repository-shaped fixtures above.
    let deep = Fixture::deep(400);
    assert_eq!(deep.files, 400, "the deep fixture exercises 400 levels");

    // A tree the size of the Palamedes trial's synthetic case, where every
    // parallel arm lost to its own serial form.
    let mini = Fixture::new(2, 1, 6);
    assert_eq!(mini.files, 12, "the mini fixture models the trial's tree");

    // Three source trees of a size a caller would really hold, walked as one
    // walk and as three.
    let roots = [
        Fixture::new(16, 8, 16),
        Fixture::new(16, 8, 16),
        Fixture::new(16, 8, 16),
    ];
    assert_eq!(roots[0].files, 2048, "each root is a repository-sized tree");

    bench_tree(c, "", &small);
    bench_tree(c, "large/", &large);
    bench_tree(c, "deep/", &deep);
    bench_covering_exclude(c, "large/", &large);
    bench_caller_matching(c, "large/", &large);
    bench_caller_matching(c, "mini/", &mini);
    bench_multi_root(c, &roots);
}

/// The cheapest per-PR guard for include-plus-exclude pruning. The include
/// alone has to inspect the existing 5k-file fixture; the covering exclude
/// rejects every branch before it is opened. A regression that merely filters
/// excluded files after traversal therefore becomes a large visible wall-time
/// jump without adding another fixture or comparison matrix.
fn bench_covering_exclude(c: &mut Criterion, group: &str, fixture: &Fixture) {
    let walk = |threads| {
        Walker::new(&fixture.root)
            .threads(threads)
            .include("**/*.rs")
            .expect("benchmark include is valid")
            .exclude("**/branch-*/**")
            .expect("benchmark exclude is valid")
            .options(WalkOptions::default())
            .collect()
            .expect("benchmark walk succeeds")
            .entries()
            .len()
    };
    assert_eq!(walk(1), 0, "covering excludes reject the whole fixture");
    assert_eq!(walk(4), 0, "serial and parallel exclusion must agree");

    c.bench_function(
        &format!("{LANE}/{group}include_exclude/parallel"),
        |benchmark| benchmark.iter(|| black_box(walk(4))),
    );
}

/// One walker across several roots against one walker per root.
///
/// The difference is thread-pool startup: three walkers start three pools over
/// the same work. Both arms produce the same entries, so what is being measured
/// is only how many times the walk pays for its threads.
fn bench_multi_root(c: &mut Criterion, roots: &[Fixture]) {
    let paths = roots
        .iter()
        .map(|root| root.root.clone())
        .collect::<Vec<_>>();
    let total = roots.iter().map(|root| root.files).sum::<usize>();

    c.bench_function(&format!("{LANE}/multi_root/one_walker"), |benchmark| {
        benchmark.iter(|| {
            let mut walker = Walker::new(&paths[0])
                .threads(4)
                .options(WalkOptions::default());
            for path in &paths[1..] {
                walker = walker.add_root(path).expect("benchmark root is valid");
            }
            let result = walker.collect().expect("benchmark walk succeeds");
            black_box(result.entries().len())
        })
    });

    c.bench_function(
        &format!("{LANE}/multi_root/one_walker_per_root"),
        |benchmark| {
            benchmark.iter(|| {
                let mut entries = 0;
                for path in &paths {
                    let result = Walker::new(path)
                        .threads(4)
                        .options(WalkOptions::default())
                        .collect()
                        .expect("benchmark walk succeeds");
                    entries += result.entries().len();
                }
                black_box(entries)
            })
        },
    );

    // A trial-sized version of the same question, where the helper floor keeps
    // both arms serial and the only difference left is the walk itself.
    let tiny = [
        Fixture::new(2, 1, 2),
        Fixture::new(2, 1, 2),
        Fixture::new(2, 1, 2),
    ];
    let tiny_paths = tiny
        .iter()
        .map(|root| root.root.clone())
        .collect::<Vec<_>>();

    c.bench_function(&format!("{LANE}/multi_root/tiny/one_walker"), |benchmark| {
        benchmark.iter(|| {
            let mut walker = Walker::new(&tiny_paths[0])
                .threads(4)
                .options(WalkOptions::default());
            for path in &tiny_paths[1..] {
                walker = walker.add_root(path).expect("benchmark root is valid");
            }
            let result = walker.collect().expect("benchmark walk succeeds");
            black_box(result.entries().len())
        })
    });

    c.bench_function(
        &format!("{LANE}/multi_root/tiny/one_walker_per_root"),
        |benchmark| {
            benchmark.iter(|| {
                let mut entries = 0;
                for path in &tiny_paths {
                    let result = Walker::new(path)
                        .threads(4)
                        .options(WalkOptions::default())
                        .collect()
                        .expect("benchmark walk succeeds");
                    entries += result.entries().len();
                }
                black_box(entries)
            })
        },
    );

    black_box(total);
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

    // The arm the caller-side matcher exists to avoid: the same catalog handed
    // to the walker as includes, read the way `globset` reads it. Nothing runs
    // per entry outside the walk.
    c.bench_function(
        &format!("{LANE}/{group}caller_match/walker_includes_crossing"),
        |benchmark| {
            benchmark.iter(|| {
                let mut walker = Walker::new(&fixture.root)
                    .threads(4)
                    .wildcard_mode(WildcardMode::SeparatorCrossing)
                    .options(WalkOptions::default());
                for extension in CALLER_EXTENSIONS {
                    walker = walker
                        .include(format!("*.{extension}"))
                        .expect("benchmark include is valid");
                }
                let result = walker.collect().expect("benchmark walk succeeds");
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

/// The extensions the caller's catalog covers.
///
/// A catalog rather than one pattern. Palamedes carries roughly this many
/// source globs, and a single-pattern set is cheap enough per entry that a
/// serial pass over the result hides inside the walk.
const CALLER_EXTENSIONS: [&str; 24] = [
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "toml", "yaml", "yml", "md", "css",
    "scss", "html", "py", "go", "java", "kt", "swift", "c", "h", "cpp", "hpp",
];

/// The caller's own matcher, standing in for the `GlobSet` Palamedes keeps for
/// parity. The `walker_includes_crossing` arm above is the same selection
/// expressed as walker includes instead.
fn caller_matcher() -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for extension in CALLER_EXTENSIONS {
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
