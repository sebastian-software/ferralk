#![forbid(unsafe_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use codspeed_criterion_compat::{Criterion, black_box, criterion_group, criterion_main};
use ferralk::{WalkOptions, Walker};
use ignore::{WalkBuilder, WalkState, overrides::OverrideBuilder};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
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
        for branch in 0..16 {
            for depth in 0..4 {
                let directory = root.join(format!("branch-{branch}/depth-{depth}"));
                fs::create_dir_all(&directory).expect("create benchmark directory");
                fs::write(directory.join("match.rs"), b"fixture").expect("write matching file");
                fs::write(directory.join("skip.txt"), b"fixture").expect("write non-matching file");
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

fn walker(c: &mut Criterion) {
    let fixture = Fixture::new();
    let options = WalkOptions::default();

    c.bench_function("walker/serial_filtered", |benchmark| {
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
    c.bench_function("walker/parallel_filtered", |benchmark| {
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
    c.bench_function("walker/ignore_parallel_filtered", |benchmark| {
        benchmark.iter(|| {
            ignore_parallel_filtered(&fixture.root);
            black_box(())
        })
    });
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
