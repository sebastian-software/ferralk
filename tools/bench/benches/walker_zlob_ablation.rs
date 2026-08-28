#![forbid(unsafe_code)]
//! Focused ferralk/zlob walker ablations.
//!
//! This is a diagnostic comparison, not a published headline benchmark. It
//! keeps the fixture, thread count, filters, and result-count gate constant,
//! while separating two costs that the complete engine matrix combines:
//!
//! - `files`: traverse the complete tree and report every file;
//! - `filtered`: traverse the same tree and report only `**/*.{ts,tsx}`;
//! - `collect`: retain every reported path;
//! - `count`: stream reported entries through an atomic counter and retain no
//!   paths.
//!
//! Run it on macOS with the native ferralk backend and the pinned zlob oracle:
//!
//! ```text
//! LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib cargo +stable bench -p bench \
//!   --bench walker_zlob_ablation --features native-macos,zlob-oracle \
//!   -- --output-format bencher --noplot
//! ```

use std::{
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use criterion::{Criterion, criterion_group, criterion_main};
use ferralk::{Verdict, WalkOptions, Walker};
use zlob::{
    ZlobFlags,
    walk::{WalkBuilder as ZlobWalkBuilder, WalkFlags, WalkState as ZlobWalkState},
};

const THREADS: usize = 4;
const FILTER: &str = "**/*.{ts,tsx}";

struct Fixture {
    root: PathBuf,
    files: usize,
    filtered: usize,
}

impl Fixture {
    fn new() -> Self {
        let unique = format!(
            "ferralk-zlob-ablation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let mut files = 0;
        let mut filtered = 0;

        for area in 0..40 {
            for module in 0..10 {
                let directory = root
                    .join("src")
                    .join(format!("area-{area}"))
                    .join(format!("module-{module}"));
                fs::create_dir_all(&directory).expect("create source directory");
                for index in 0..10 {
                    let (name, matches) = match index % 5 {
                        0 => (format!("view-{index}.tsx"), true),
                        1 | 2 => (format!("unit-{index}.ts"), true),
                        3 => (format!("style-{index}.css"), false),
                        _ => (format!("legacy-{index}.js"), false),
                    };
                    write(&directory.join(name));
                    files += 1;
                    filtered += usize::from(matches);
                }
            }
        }

        for package in 0..20 {
            let directory = root
                .join("packages")
                .join(format!("pkg-{package}"))
                .join("src");
            fs::create_dir_all(&directory).expect("create package directory");
            for index in 0..20 {
                let matches = index % 2 == 0;
                let name = if matches {
                    format!("index-{index}.ts")
                } else {
                    format!("index-{index}.js")
                };
                write(&directory.join(name));
                files += 1;
                filtered += usize::from(matches);
            }
        }

        for package in 0_usize..400 {
            let package_root = root.join("node_modules").join(format!("dep-{package}"));
            let (written, matched) = write_package(&package_root, 100);
            files += written;
            filtered += matched;
            if package.is_multiple_of(10) {
                for nested in 0..5 {
                    let nested_root = package_root
                        .join("node_modules")
                        .join(format!("nested-{nested}"));
                    let (written, matched) = write_package(&nested_root, 40);
                    files += written;
                    filtered += matched;
                }
            }
        }

        Self {
            root,
            files,
            filtered,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_package(package_root: &Path, files: usize) -> (usize, usize) {
    let directory = package_root.join("lib");
    fs::create_dir_all(&directory).expect("create dependency directory");
    write(&package_root.join("package.json"));
    write(&package_root.join("README.md"));
    let mut matched = 0;
    for index in 0..files {
        let name = match index % 10 {
            0 => {
                matched += 1;
                format!("types-{index}.d.ts")
            }
            1 => format!("meta-{index}.json"),
            _ => format!("chunk-{index}.js"),
        };
        write(&directory.join(name));
    }
    (files + 2, matched)
}

fn write(path: &Path) {
    fs::write(path, b"fixture").expect("write fixture file");
}

fn walker_zlob_ablation(c: &mut Criterion) {
    let fixture = Fixture::new();
    assert_eq!(fixture.files, 53_600);
    assert_eq!(fixture.filtered, 7_400);

    let expected_files = ferralk_collect(&fixture.root, None);
    let expected_filtered = ferralk_collect(&fixture.root, Some(FILTER));
    assert_eq!(expected_files, fixture.files);
    assert_eq!(expected_filtered, fixture.filtered);
    for (label, actual, expected) in [
        (
            "files/ferralk_count",
            ferralk_count(&fixture.root, None),
            expected_files,
        ),
        (
            "files/zlob_collect",
            zlob_collect(&fixture.root, None),
            expected_files,
        ),
        (
            "files/zlob_count",
            zlob_count(&fixture.root, None),
            expected_files,
        ),
        (
            "filtered/ferralk_count",
            ferralk_count(&fixture.root, Some(FILTER)),
            expected_filtered,
        ),
        (
            "filtered/zlob_collect",
            zlob_collect(&fixture.root, Some(FILTER)),
            expected_filtered,
        ),
        (
            "filtered/zlob_count",
            zlob_count(&fixture.root, Some(FILTER)),
            expected_filtered,
        ),
    ] {
        assert_eq!(actual, expected, "{label} returned a different result");
    }

    println!(
        "zlob ablation fixture: {} files, {} filtered matches",
        fixture.files, fixture.filtered
    );
    let mut group = c.benchmark_group("walker_zlob_ablation");
    group
        .sample_size(15)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(5));

    for (label, pattern) in [("files", None), ("filtered", Some(FILTER))] {
        group.bench_function(format!("{label}/ferralk_collect"), |benchmark| {
            benchmark.iter(|| black_box(ferralk_collect(&fixture.root, pattern)))
        });
        group.bench_function(format!("{label}/ferralk_count"), |benchmark| {
            benchmark.iter(|| black_box(ferralk_count(&fixture.root, pattern)))
        });
        group.bench_function(format!("{label}/zlob_collect"), |benchmark| {
            benchmark.iter(|| black_box(zlob_collect(&fixture.root, pattern)))
        });
        group.bench_function(format!("{label}/zlob_count"), |benchmark| {
            benchmark.iter(|| black_box(zlob_count(&fixture.root, pattern)))
        });
    }
    group.finish();
}

fn ferralk_builder(root: &Path, pattern: Option<&str>) -> Walker {
    let walker = Walker::new(root)
        .threads(THREADS)
        .options(WalkOptions::default().files_only(true));
    match pattern {
        Some(pattern) => walker.include(pattern).expect("benchmark include is valid"),
        None => walker,
    }
}

fn ferralk_collect(root: &Path, pattern: Option<&str>) -> usize {
    ferralk_builder(root, pattern)
        .collect()
        .expect("benchmark walk succeeds")
        .entries()
        .len()
}

fn ferralk_count(root: &Path, pattern: Option<&str>) -> usize {
    let matched = AtomicUsize::new(0);
    let result = ferralk_builder(root, pattern)
        .visit(|_| {
            matched.fetch_add(1, Ordering::Relaxed);
            Verdict::Skip
        })
        .expect("benchmark walk succeeds");
    assert!(result.entries().is_empty());
    matched.load(Ordering::Relaxed)
}

fn zlob_builder(root: &Path, pattern: Option<&str>) -> ZlobWalkBuilder {
    let mut walker = ZlobWalkBuilder::new(root).expect("benchmark root is valid");
    walker.options(WalkFlags::NO_REPORT_DIRS).threads(THREADS);
    if let Some(pattern) = pattern {
        walker
            .include(pattern)
            .expect("benchmark include is valid")
            .include_flags(ZlobFlags::RECOMMENDED);
    }
    walker
}

fn zlob_collect(root: &Path, pattern: Option<&str>) -> usize {
    zlob_builder(root, pattern)
        .collect()
        .expect("benchmark walk succeeds")
        .len()
}

fn zlob_count(root: &Path, pattern: Option<&str>) -> usize {
    let matched = AtomicUsize::new(0);
    zlob_builder(root, pattern)
        .run(|_| {
            matched.fetch_add(1, Ordering::Relaxed);
            ZlobWalkState::Continue
        })
        .expect("benchmark walk succeeds");
    matched.load(Ordering::Relaxed)
}

criterion_group!(benches, walker_zlob_ablation);
criterion_main!(benches);
