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
//! Two queries, because they exercise different parts of the walk:
//!
//! - `**/*.{ts,tsx}` traverses everything, `node_modules` included, and leans
//!   on per-entry filtering.
//! - `{src,packages}/**/*.{ts,tsx}` can prune whole subtrees before opening
//!   them, which is what the RFC's "safe subtree pruning" arm did by hand.
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
//! cargo bench -p bench --bench walker_palamedes --features zlob-oracle  # needs Zig
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use codspeed_criterion_compat::{Criterion, black_box, criterion_group, criterion_main};
use ferralk::{WalkOptions, Walker};
use globset::{Glob, GlobSetBuilder};
use ignore::{WalkBuilder, WalkState, overrides::OverrideBuilder};

/// Threads for every parallel arm, so the comparison is like for like rather
/// than a reading of whatever `available_parallelism` reports on the host.
const THREADS: usize = 4;

/// A JavaScript repository whose file count sits in `node_modules`.
///
/// The proportions follow the shape the RFC measured: a few thousand source
/// files worth finding, buried under an order of magnitude more dependency
/// files that every walk has to step over.
struct Fixture {
    root: PathBuf,
    files: usize,
    sources: usize,
}

impl Fixture {
    fn new() -> Self {
        let unique = format!(
            "ferralk-palamedes-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let mut files = 0;
        let mut sources = 0;

        // Application sources: what the query is actually looking for.
        for area in 0..40 {
            for module in 0..10 {
                let directory = root
                    .join("src")
                    .join(format!("area-{area}"))
                    .join(format!("module-{module}"));
                fs::create_dir_all(&directory).expect("create source directory");
                for index in 0..10 {
                    let (name, is_source) = match index % 5 {
                        0 => (format!("view-{index}.tsx"), true),
                        1 | 2 => (format!("unit-{index}.ts"), true),
                        3 => (format!("style-{index}.css"), false),
                        _ => (format!("legacy-{index}.js"), false),
                    };
                    write(&directory.join(name));
                    files += 1;
                    sources += usize::from(is_source);
                }
            }
        }

        // A second source root, so a scoped query has more than one literal
        // root to prune to.
        for package in 0..20 {
            let directory = root
                .join("packages")
                .join(format!("pkg-{package}"))
                .join("src");
            fs::create_dir_all(&directory).expect("create package directory");
            for index in 0..20 {
                let is_source = index % 2 == 0;
                let name = if is_source {
                    format!("index-{index}.ts")
                } else {
                    format!("index-{index}.js")
                };
                write(&directory.join(name));
                files += 1;
                sources += usize::from(is_source);
            }
        }

        // Dependencies: the bulk of the tree, and the part a scoped query never
        // needs to open. Every tenth package nests its own dependencies, the
        // way a real tree does.
        for package in 0_usize..400 {
            let package_root = root.join("node_modules").join(format!("dep-{package}"));
            files += write_package(&package_root, 100);
            if package.is_multiple_of(10) {
                for nested in 0..5 {
                    let nested_root = package_root
                        .join("node_modules")
                        .join(format!("nested-{nested}"));
                    files += write_package(&nested_root, 40);
                }
            }
        }

        Self {
            root,
            files,
            sources,
        }
    }
}

/// Writes one dependency package: mostly JavaScript and metadata, plus the
/// type declarations that make `**/*.ts` match inside `node_modules` too.
fn write_package(package_root: &Path, files: usize) -> usize {
    let directory = package_root.join("lib");
    fs::create_dir_all(&directory).expect("create dependency directory");
    write(&package_root.join("package.json"));
    write(&package_root.join("README.md"));
    for index in 0..files {
        let name = match index % 10 {
            0 => format!("types-{index}.d.ts"),
            1 => format!("meta-{index}.json"),
            _ => format!("chunk-{index}.js"),
        };
        write(&directory.join(name));
    }
    files + 2
}

fn write(path: &Path) {
    fs::write(path, b"fixture").expect("write fixture file");
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn palamedes(c: &mut Criterion) {
    let fixture = Fixture::new();
    assert!(
        fixture.files >= 50_000,
        "the fixture must reach the 50k files the RFC measured, has {}",
        fixture.files
    );
    println!(
        "palamedes fixture: {} files, {} of them TypeScript sources",
        fixture.files, fixture.sources
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
            "**/*.{ts,tsx}",
            ["**/*.ts", "**/*.tsx"],
            &[][..],
        ),
        (
            "scoped",
            "{src,packages}/**/*.{ts,tsx}",
            ["{src,packages}/**/*.ts", "{src,packages}/**/*.tsx"],
            &["src", "packages"][..],
        ),
    ] {
        // A faster arm that finds fewer files would not be a comparison. Every
        // arm is run once here and has to agree on the count before any of them
        // is timed.
        let found = ferralk_walk(&fixture.root, ferralk_pattern, THREADS);
        assert_eq!(
            found,
            ferralk_walk(&fixture.root, ferralk_pattern, 1),
            "{query}: ferralk's own arms disagree"
        );
        assert_eq!(
            found,
            ignore_serial_globset(&fixture.root, &override_globs),
            "{query}: ignore + globset disagrees with ferralk"
        );
        assert_eq!(
            found,
            ignore_parallel_overrides(&fixture.root, &override_globs),
            "{query}: ignore overrides disagree with ferralk"
        );
        if !scoped_roots.is_empty() {
            assert_eq!(
                found,
                ignore_parallel_pruned(&fixture.root, &override_globs, scoped_roots),
                "{query}: hand-pruned ignore disagrees with ferralk"
            );
        }
        #[cfg(feature = "zlob-oracle")]
        assert_eq!(
            found,
            zlob_walk(&fixture.root, ferralk_pattern),
            "{query}: zlob disagrees with ferralk"
        );
        println!("palamedes {query}: every arm found {found} files");

        group.bench_function(format!("{query}/ferralk_serial"), |benchmark| {
            benchmark.iter(|| black_box(ferralk_walk(&fixture.root, ferralk_pattern, 1)))
        });
        group.bench_function(format!("{query}/ferralk_parallel"), |benchmark| {
            benchmark.iter(|| black_box(ferralk_walk(&fixture.root, ferralk_pattern, THREADS)))
        });
        // The RFC's "current Palamedes" arm: walk everything serially and match
        // every path with globset, without telling the walker what to skip.
        group.bench_function(format!("{query}/ignore_serial_globset"), |benchmark| {
            benchmark.iter(|| black_box(ignore_serial_globset(&fixture.root, &override_globs)))
        });
        // `ignore` parallel with the same globs as overrides. Overrides decide
        // which entries are yielded, not which directories are opened, so this
        // arm still reads the whole tree.
        group.bench_function(format!("{query}/ignore_parallel_overrides"), |benchmark| {
            benchmark.iter(|| black_box(ignore_parallel_overrides(&fixture.root, &override_globs)))
        });
        // The RFC's "safe subtree pruning" arm: the pruning Palamedes wrote by
        // hand, expressed as `filter_entry`, so `ignore` skips the directories
        // a scoped query can never match in. An unscoped query has nothing to
        // prune, which would make this the previous arm run twice.
        if !scoped_roots.is_empty() {
            group.bench_function(format!("{query}/ignore_parallel_pruned"), |benchmark| {
                benchmark.iter(|| {
                    black_box(ignore_parallel_pruned(
                        &fixture.root,
                        &override_globs,
                        scoped_roots,
                    ))
                })
            });
        }
        #[cfg(feature = "zlob-oracle")]
        group.bench_function(format!("{query}/zlob_parallel"), |benchmark| {
            benchmark.iter(|| black_box(zlob_walk(&fixture.root, ferralk_pattern)))
        });
    }
    group.finish();
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

fn ignore_serial_globset(root: &Path, globs: &[&str; 2]) -> usize {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(Glob::new(glob).expect("benchmark glob is valid"));
    }
    let set = builder.build().expect("benchmark glob set builds");
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
fn zlob_walk(root: &Path, pattern: &str) -> usize {
    use zlob::{
        ZlobFlags,
        walk::{WalkBuilder as ZlobWalkBuilder, WalkFlags},
    };

    let mut walker = ZlobWalkBuilder::new(root).expect("benchmark root is valid");
    walker
        .options(WalkFlags::empty())
        .threads(THREADS)
        .include(pattern)
        .expect("benchmark include is valid")
        .include_flags(ZlobFlags::RECOMMENDED);
    walker.collect().expect("benchmark walk succeeds").len()
}

criterion_group!(benches, palamedes);
criterion_main!(benches);
