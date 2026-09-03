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
//! LIBCLANG_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib cargo +stable bench -p bench \
//!   --bench walker_zlob_ablation --features native-macos,zlob-oracle \
//!   -- --output-format bencher --noplot
//! ```

use std::{
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
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

fn path_scratch_ablation(c: &mut Criterion) {
    let directory = PathBuf::from("/tmp/ferralk-zlob-ablation/node_modules/dep-399/lib");
    let name = std::ffi::OsStr::new("types-90.d.ts");

    c.bench_function("walker_zlob_ablation/path_reset/copy_parent", |benchmark| {
        let mut path = directory.clone();
        benchmark.iter(|| {
            path.push(name);
            black_box(&path);
            path.clear();
            path.as_mut_os_string().push(directory.as_os_str());
        })
    });
    c.bench_function(
        "walker_zlob_ablation/path_reset/pop_component",
        |benchmark| {
            let mut path = directory.clone();
            benchmark.iter(|| {
                path.push(name);
                black_box(&path);
                assert!(path.pop());
            })
        },
    );
    #[cfg(unix)]
    c.bench_function(
        "walker_zlob_ablation/path_reset/truncate_bytes",
        |benchmark| {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};

            let directory_len = directory.as_os_str().as_bytes().len();
            let mut path = directory.clone();
            benchmark.iter(|| {
                path.push(name);
                black_box(&path);
                let mut bytes = std::mem::take(&mut path).into_os_string().into_vec();
                bytes.truncate(directory_len);
                path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
            })
        },
    );
}

#[derive(Clone)]
struct ChunkedPath {
    storage: Arc<[u8]>,
    start: usize,
    len: usize,
}

fn path_collection_ablation(c: &mut Criterion) {
    const MATCHES: usize = 7_400;
    const CHUNK_BYTES: usize = 256 * 1024;

    let paths = (0..MATCHES)
        .map(|index| {
            PathBuf::from(format!(
                "/tmp/ferralk-zlob-ablation/node_modules/dep-{}/lib/types-{}.d.ts",
                index % 400,
                index % 100
            ))
        })
        .collect::<Vec<_>>();

    c.bench_function(
        "walker_zlob_ablation/path_collect/owned_path_bufs",
        |benchmark| {
            benchmark.iter(|| {
                black_box(paths.to_vec());
            })
        },
    );
    c.bench_function(
        "walker_zlob_ablation/path_collect/shared_chunks",
        |benchmark| {
            benchmark.iter(|| {
                let mut chunks = vec![Vec::with_capacity(CHUNK_BYTES)];
                let mut pending = Vec::with_capacity(paths.len());
                for path in &paths {
                    let bytes = path.as_os_str().as_encoded_bytes();
                    if chunks
                        .last()
                        .is_some_and(|chunk| chunk.len() + bytes.len() > CHUNK_BYTES)
                    {
                        chunks.push(Vec::with_capacity(CHUNK_BYTES.max(bytes.len())));
                    }
                    let chunk = chunks.last_mut().expect("one chunk exists");
                    let start = chunk.len();
                    chunk.extend_from_slice(bytes);
                    pending.push((chunks.len() - 1, start, bytes.len()));
                }
                let chunks = chunks
                    .into_iter()
                    .map(Arc::<[u8]>::from)
                    .collect::<Vec<_>>();
                let retained = pending
                    .into_iter()
                    .map(|(chunk, start, len)| ChunkedPath {
                        storage: Arc::clone(&chunks[chunk]),
                        start,
                        len,
                    })
                    .collect::<Vec<_>>();
                black_box(
                    retained
                        .iter()
                        .map(|path| path.storage[path.start..][..path.len].len())
                        .sum::<usize>(),
                );
            })
        },
    );
}

criterion_group!(
    benches,
    walker_zlob_ablation,
    path_scratch_ablation,
    path_collection_ablation
);
criterion_main!(benches);
