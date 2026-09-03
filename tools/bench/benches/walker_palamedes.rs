#![forbid(unsafe_code)]
//! The comparison the RFC opened with, reproducible on the current code.
//!
//! The RFC's business case rested on one local measurement of a 50,000-file
//! tree: serial `ignore + globset` at 2,355 ms, `ignore` parallel with subtree
//! pruning at 150 ms, zlob at 140 ms. That measurement predates ferralk and was
//! never re-runnable. This bench rebuilds the same shape - a JavaScript
//! repository whose file count is dominated by `node_modules` - and puts
//! ferralk next to those baselines on today's code.
//!
//! Three queries, because they exercise different parts of the walk:
//!
//! - `**/*.{ts,tsx}` traverses everything, `node_modules` included, and leans
//!   on per-entry filtering.
//! - `{src,packages}/**/*.{ts,tsx}` can prune whole subtrees before opening
//!   them, which is what the RFC's "safe subtree pruning" arm did by hand.
//! - `**/*.{ts,tsx}` plus `**/node_modules/**` as an exclude returns the same
//!   files as the scoped query, but reaches that result through exclude
//!   pruning. A companion arm gets the same policy from `.gitignore`.
//!
//! **Cache assumption:** the fixture is written once, immediately before the
//! measurements, so its directory entries and inodes are warm. These are
//! warm-cache numbers, like the RFC's.
//!
//! **Not a gate.** Wall time on a shared or busy machine is indicative. Run the
//! arms you want to compare in one invocation, on an idle machine, and compare
//! within that run rather than across runs.
//!
//! ```text
//! cargo bench -p bench --bench walker_palamedes
//! cargo bench -p bench --bench walker_palamedes --features thread-sweep -- thread_sweep/
//! cargo bench -p bench --bench walker_palamedes --features zlob-oracle  # needs Zig
//! ```

use std::{hint::black_box, path::Path, time::Duration};

use bench::{
    NODE_MODULES_EXCLUDE, RepositoryFixture, SCOPED_TYPESCRIPT_PATTERN, TYPESCRIPT_PATTERN,
};
use criterion::{Criterion, criterion_group, criterion_main};
use ferralk::{WalkOptions, Walker};
use globset::{Glob, GlobSetBuilder};
use globwalk::{FileType as GlobwalkFileType, GlobWalkerBuilder};
use ignore::{WalkBuilder, WalkState, overrides::OverrideBuilder};
use jwalk::{Parallelism as JwalkParallelism, WalkDir as JwalkWalkDir};
use walkdir::WalkDir;
use wax::Glob as WaxGlob;
use wax::walk::Entry as _;

/// Threads for every parallel arm, so the comparison is like for like rather
/// than a reading of whatever `available_parallelism` reports on the host.
const THREADS: usize = 4;
fn palamedes(c: &mut Criterion) {
    let fixture = RepositoryFixture::new();
    assert!(
        fixture.files() >= 50_000,
        "the fixture must reach the 50k files the RFC measured, has {}",
        fixture.files()
    );
    println!(
        "palamedes fixture: {} files, {} of them TypeScript sources",
        fixture.files(),
        fixture.sources()
    );

    let mut group = c.benchmark_group("walker_palamedes");
    // A single walk of this tree costs tens of milliseconds, so criterion's
    // default sample count would run for minutes per arm.
    group
        .sample_size(10)
        .warm_up_time(Duration::from_secs(2))
        .measurement_time(Duration::from_secs(8));

    for (query, ferralk_pattern, override_globs, scoped_roots) in [
        (
            "unscoped",
            TYPESCRIPT_PATTERN,
            ["**/*.ts", "**/*.tsx"],
            &[][..],
        ),
        (
            "scoped",
            SCOPED_TYPESCRIPT_PATTERN,
            ["{src,packages}/**/*.ts", "{src,packages}/**/*.tsx"],
            &["src", "packages"][..],
        ),
    ] {
        // A faster arm that finds fewer files would not be a comparison. Every
        // arm is run once here and has to agree on the count before any of them
        // is timed.
        let found = ferralk_walk(fixture.root(), ferralk_pattern, THREADS);
        assert_eq!(
            found,
            ferralk_walk(fixture.root(), ferralk_pattern, 1),
            "{query}: ferralk's own arms disagree"
        );
        assert_eq!(
            found,
            ignore_serial_globset(fixture.root(), &override_globs),
            "{query}: ignore + globset disagrees with ferralk"
        );
        assert_eq!(
            found,
            walkdir_serial_globset(fixture.root(), &override_globs),
            "{query}: walkdir + globset disagrees with ferralk"
        );
        assert_eq!(
            found,
            jwalk_globset(fixture.root(), &override_globs, JwalkParallelism::Serial,),
            "{query}: jwalk serial + globset disagrees with ferralk"
        );
        assert_eq!(
            found,
            jwalk_globset(
                fixture.root(),
                &override_globs,
                JwalkParallelism::RayonNewPool(THREADS),
            ),
            "{query}: jwalk parallel + globset disagrees with ferralk"
        );
        assert_eq!(
            found,
            globwalk_serial(fixture.root(), &override_globs),
            "{query}: globwalk disagrees with ferralk"
        );
        assert_eq!(
            found,
            wax_walk(fixture.root(), ferralk_pattern),
            "{query}: wax disagrees with ferralk"
        );
        assert_eq!(
            found,
            ignore_parallel_overrides(fixture.root(), &override_globs),
            "{query}: ignore overrides disagree with ferralk"
        );
        if !scoped_roots.is_empty() {
            assert_eq!(
                found,
                ignore_parallel_pruned(fixture.root(), &override_globs, scoped_roots),
                "{query}: hand-pruned ignore disagrees with ferralk"
            );
        }
        #[cfg(feature = "zlob-oracle")]
        assert_eq!(
            found,
            zlob_walk(fixture.root(), ferralk_pattern, THREADS),
            "{query}: zlob disagrees with ferralk"
        );
        println!("palamedes {query}: every arm found {found} files");

        group.bench_function(format!("{query}/ferralk_serial"), |benchmark| {
            benchmark.iter(|| black_box(ferralk_walk(fixture.root(), ferralk_pattern, 1)))
        });
        group.bench_function(format!("{query}/ferralk_parallel"), |benchmark| {
            benchmark.iter(|| black_box(ferralk_walk(fixture.root(), ferralk_pattern, THREADS)))
        });
        // The RFC's "current Palamedes" arm: walk everything serially and match
        // every path with globset, without telling the walker what to skip.
        group.bench_function(format!("{query}/ignore_serial_globset"), |benchmark| {
            benchmark.iter(|| black_box(ignore_serial_globset(fixture.root(), &override_globs)))
        });
        group.bench_function(format!("{query}/walkdir_serial_globset"), |benchmark| {
            benchmark.iter(|| black_box(walkdir_serial_globset(fixture.root(), &override_globs)))
        });
        group.bench_function(format!("{query}/jwalk_serial_globset"), |benchmark| {
            benchmark.iter(|| {
                black_box(jwalk_globset(
                    fixture.root(),
                    &override_globs,
                    JwalkParallelism::Serial,
                ))
            })
        });
        group.bench_function(format!("{query}/jwalk_parallel_globset"), |benchmark| {
            benchmark.iter(|| {
                black_box(jwalk_globset(
                    fixture.root(),
                    &override_globs,
                    JwalkParallelism::RayonNewPool(THREADS),
                ))
            })
        });
        group.bench_function(format!("{query}/globwalk_serial"), |benchmark| {
            benchmark.iter(|| black_box(globwalk_serial(fixture.root(), &override_globs)))
        });
        group.bench_function(format!("{query}/wax_serial"), |benchmark| {
            benchmark.iter(|| black_box(wax_walk(fixture.root(), ferralk_pattern)))
        });
        // `ignore` parallel with the same globs as overrides. Overrides decide
        // which entries are yielded, not which directories are opened, so this
        // arm still reads the whole tree.
        group.bench_function(format!("{query}/ignore_parallel_overrides"), |benchmark| {
            benchmark.iter(|| black_box(ignore_parallel_overrides(fixture.root(), &override_globs)))
        });
        // The RFC's "safe subtree pruning" arm: the pruning Palamedes wrote by
        // hand, expressed as `filter_entry`, so `ignore` skips the directories
        // a scoped query can never match in. An unscoped query has nothing to
        // prune, which would make this the previous arm run twice.
        if !scoped_roots.is_empty() {
            group.bench_function(format!("{query}/ignore_parallel_pruned"), |benchmark| {
                benchmark.iter(|| {
                    black_box(ignore_parallel_pruned(
                        fixture.root(),
                        &override_globs,
                        scoped_roots,
                    ))
                })
            });
        }
        #[cfg(feature = "zlob-oracle")]
        group.bench_function(format!("{query}/zlob_parallel"), |benchmark| {
            benchmark.iter(|| black_box(zlob_walk(fixture.root(), ferralk_pattern, THREADS)))
        });
    }
    bench_exclude_pruning(&mut group, &fixture);
    #[cfg(feature = "thread-sweep")]
    bench_thread_sweep(&mut group, &fixture);
    group.finish();
}

/// Measures the scaling curve without adding ten filesystem-heavy points to
/// the ordinary pull-request lane. Enable `thread-sweep` locally or in the
/// manual zlob workflow; every point uses the same fixture and result gate.
#[cfg(feature = "thread-sweep")]
fn bench_thread_sweep(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    fixture: &RepositoryFixture,
) {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let points = [
        ("threads_1".to_owned(), 1),
        ("threads_2".to_owned(), 2),
        ("threads_4".to_owned(), 4),
        ("threads_8".to_owned(), 8),
        (format!("threads_available_{available}"), available),
    ];
    let expected = ferralk_walk(fixture.root(), TYPESCRIPT_PATTERN, 1);
    println!(
        "palamedes thread sweep: available_parallelism={available}, every arm found {expected} files"
    );

    for (label, threads) in points {
        assert_eq!(
            ferralk_walk(fixture.root(), TYPESCRIPT_PATTERN, threads),
            expected,
            "thread sweep: ferralk at {threads} threads disagrees"
        );
        group.bench_function(
            format!("thread_sweep/unscoped/ferralk/{label}"),
            |benchmark| {
                benchmark
                    .iter(|| black_box(ferralk_walk(fixture.root(), TYPESCRIPT_PATTERN, threads)))
            },
        );

        #[cfg(feature = "zlob-oracle")]
        {
            assert_eq!(
                zlob_walk(fixture.root(), TYPESCRIPT_PATTERN, threads),
                expected,
                "thread sweep: zlob at {threads} threads disagrees"
            );
            group.bench_function(format!("thread_sweep/unscoped/zlob/{label}"), |benchmark| {
                benchmark.iter(|| black_box(zlob_walk(fixture.root(), TYPESCRIPT_PATTERN, threads)))
            });
        }
    }
}

/// The documented include-plus-exclude configuration and its gitignore
/// equivalent. Both must select exactly the scoped query's files before any
/// timings run, so a pruning arm cannot appear fast by silently finding less.
fn bench_exclude_pruning(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    fixture: &RepositoryFixture,
) {
    let expected = ferralk_walk(fixture.root(), SCOPED_TYPESCRIPT_PATTERN, THREADS);
    let excluded = ferralk_excluded_walk(fixture.root(), THREADS);
    assert_eq!(
        excluded,
        ferralk_excluded_walk(fixture.root(), 1),
        "exclude-pruned: ferralk's own arms disagree"
    );
    assert_eq!(
        excluded, expected,
        "exclude-pruned and scoped queries must select the same files"
    );
    let gitignored = ferralk_gitignore_walk(fixture.root(), THREADS);
    assert_eq!(
        gitignored,
        ferralk_gitignore_walk(fixture.root(), 1),
        "gitignore-pruned: ferralk's own arms disagree"
    );
    assert_eq!(
        gitignored, expected,
        "gitignore-pruned and scoped queries must select the same files"
    );
    println!("palamedes exclude-pruned: every arm found {expected} files");

    group.bench_function("exclude_pruned/ferralk_serial", |benchmark| {
        benchmark.iter(|| black_box(ferralk_excluded_walk(fixture.root(), 1)))
    });
    group.bench_function("exclude_pruned/ferralk_parallel", |benchmark| {
        benchmark.iter(|| black_box(ferralk_excluded_walk(fixture.root(), THREADS)))
    });
    group.bench_function("gitignore_pruned/ferralk_parallel", |benchmark| {
        benchmark.iter(|| black_box(ferralk_gitignore_walk(fixture.root(), THREADS)))
    });
}

fn ferralk_walk(root: &Path, pattern: &str, threads: usize) -> usize {
    Walker::new(root)
        .threads(threads)
        .include(pattern)
        .expect("benchmark include is valid")
        .options(WalkOptions::default())
        .collect()
        .expect("benchmark walk succeeds")
        .entries()
        .len()
}

fn ferralk_excluded_walk(root: &Path, threads: usize) -> usize {
    Walker::new(root)
        .threads(threads)
        .include(TYPESCRIPT_PATTERN)
        .expect("benchmark include is valid")
        .exclude(NODE_MODULES_EXCLUDE)
        .expect("benchmark exclude is valid")
        .options(WalkOptions::default())
        .collect()
        .expect("benchmark walk succeeds")
        .entries()
        .len()
}

fn ferralk_gitignore_walk(root: &Path, threads: usize) -> usize {
    Walker::new(root)
        .threads(threads)
        .respect_git_ignore(true)
        .include(TYPESCRIPT_PATTERN)
        .expect("benchmark include is valid")
        .options(WalkOptions::default())
        .collect()
        .expect("benchmark walk succeeds")
        .entries()
        .len()
}

fn build_globset(globs: &[&str; 2]) -> globset::GlobSet {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(Glob::new(glob).expect("benchmark glob is valid"));
    }
    builder.build().expect("benchmark glob set builds")
}

fn ignore_serial_globset(root: &Path, globs: &[&str; 2]) -> usize {
    let set = build_globset(globs);
    let mut matched = 0;
    for entry in WalkBuilder::new(root).standard_filters(false).build() {
        let entry = entry.expect("benchmark walk succeeds");
        let path = entry
            .path()
            .strip_prefix(root)
            .expect("entry belongs to the fixture");
        if set.is_match(path) {
            matched += 1;
        }
    }
    matched
}

fn walkdir_serial_globset(root: &Path, globs: &[&str; 2]) -> usize {
    let set = build_globset(globs);
    let mut matched = 0;
    for entry in WalkDir::new(root) {
        let entry = entry.expect("benchmark walk succeeds");
        if entry.file_type().is_file()
            && set.is_match(
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("entry belongs to the fixture"),
            )
        {
            matched += 1;
        }
    }
    matched
}

fn jwalk_globset(root: &Path, globs: &[&str; 2], parallelism: JwalkParallelism) -> usize {
    let set = build_globset(globs);
    JwalkWalkDir::new(root)
        .skip_hidden(false)
        .parallelism(parallelism)
        .into_iter()
        .map(|entry| entry.expect("benchmark walk succeeds"))
        .filter(|entry| {
            entry.file_type().is_file()
                && set.is_match(
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("entry belongs to the fixture"),
                )
        })
        .count()
}

fn globwalk_serial(root: &Path, globs: &[&str; 2]) -> usize {
    GlobWalkerBuilder::from_patterns(root, globs)
        .file_type(GlobwalkFileType::FILE)
        .build()
        .expect("benchmark globwalk builds")
        .inspect(|entry| {
            entry.as_ref().expect("benchmark walk succeeds");
        })
        .count()
}

fn wax_walk(root: &Path, pattern: &str) -> usize {
    WaxGlob::new(pattern)
        .expect("benchmark wax pattern is valid")
        .walk(root)
        .map(|entry| entry.expect("benchmark walk succeeds"))
        .filter(|entry| entry.file_type().is_file())
        .count()
}

fn ignore_parallel_overrides(root: &Path, globs: &[&str; 2]) -> usize {
    let mut overrides = OverrideBuilder::new(root);
    for glob in globs {
        overrides.add(glob).expect("benchmark override is valid");
    }
    let mut builder = WalkBuilder::new(root);
    builder
        .threads(THREADS)
        .standard_filters(false)
        .overrides(overrides.build().expect("benchmark override builds"));
    let matched = std::sync::atomic::AtomicUsize::new(0);
    builder.build_parallel().run(|| {
        Box::new(|entry| {
            let entry = entry.expect("benchmark walk succeeds");
            if entry.file_type().is_some_and(|kind| kind.is_file()) {
                matched.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            WalkState::Continue
        })
    });
    matched.load(std::sync::atomic::Ordering::Relaxed)
}

/// The same walk, plus the subtree pruning the RFC's arm did by hand: a
/// directory is opened only when a scoped root is on its line of descent.
fn ignore_parallel_pruned(root: &Path, globs: &[&str; 2], scoped_roots: &[&str]) -> usize {
    let mut overrides = OverrideBuilder::new(root);
    for glob in globs {
        overrides.add(glob).expect("benchmark override is valid");
    }
    let mut builder = WalkBuilder::new(root);
    builder
        .threads(THREADS)
        .standard_filters(false)
        .overrides(overrides.build().expect("benchmark override builds"));
    if !scoped_roots.is_empty() {
        let root = root.to_path_buf();
        let scoped_roots = scoped_roots
            .iter()
            .map(|scoped| root.join(scoped))
            .collect::<Vec<_>>();
        builder.filter_entry(move |entry| {
            if entry.file_type().is_some_and(|kind| !kind.is_dir()) {
                return true;
            }
            entry.path() == root
                || scoped_roots.iter().any(|scoped| {
                    entry.path().starts_with(scoped) || scoped.starts_with(entry.path())
                })
        });
    }
    let matched = std::sync::atomic::AtomicUsize::new(0);
    builder.build_parallel().run(|| {
        Box::new(|entry| {
            let entry = entry.expect("benchmark walk succeeds");
            if entry.file_type().is_some_and(|kind| kind.is_file()) {
                matched.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            WalkState::Continue
        })
    });
    matched.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(feature = "zlob-oracle")]
fn zlob_walk(root: &Path, pattern: &str, threads: usize) -> usize {
    use zlob::{
        ZlobFlags,
        walk::{WalkBuilder as ZlobWalkBuilder, WalkFlags},
    };

    let mut walker = ZlobWalkBuilder::new(root).expect("benchmark root is valid");
    walker
        .options(WalkFlags::empty())
        .threads(threads)
        .include(pattern)
        .expect("benchmark include is valid")
        .include_flags(ZlobFlags::RECOMMENDED);
    walker.collect().expect("benchmark walk succeeds").len()
}

criterion_group!(benches, palamedes);
criterion_main!(benches);
