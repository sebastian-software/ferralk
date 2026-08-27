#![forbid(unsafe_code)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use zlob::{ZlobFlags, ZlobPattern};

fn matcher_zlob(c: &mut Criterion) {
    let pattern = ZlobPattern::compile("src/**/*.rs", ZlobFlags::DOUBLESTAR_RECURSIVE)
        .expect("common benchmark pattern is valid");
    let matching = "src/deep/nested/main.rs";
    let non_matching = "src/deep/nested/main.txt";

    c.bench_function("common/zlob_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(pattern.matches_default(black_box(matching))))
    });
    c.bench_function("common/zlob_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(pattern.matches_default(black_box(non_matching))))
    });

    let long_pattern = ZlobPattern::compile("src/**/*.rs", ZlobFlags::DOUBLESTAR_RECURSIVE)
        .expect("long-path benchmark pattern is valid");
    let long_matching = long_path("main.rs");
    let long_non_matching = long_path("main.txt");
    c.bench_function("long_path/zlob_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(long_pattern.matches_default(black_box(&long_matching))))
    });
    c.bench_function("long_path/zlob_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(long_pattern.matches_default(black_box(&long_non_matching))))
    });

    let adversarial_pattern = ZlobPattern::compile("a*a*a*a*b", ZlobFlags::empty())
        .expect("adversarial benchmark pattern is valid");
    let adversarial_candidate = "a".repeat(64);
    c.bench_function("backtracking/zlob_compiled/non_matching", |benchmark| {
        benchmark.iter(|| {
            black_box(adversarial_pattern.matches_default(black_box(&adversarial_candidate)))
        })
    });
}

fn long_path(file: &str) -> String {
    let mut path = String::from("src");
    for segment in 0..12 {
        path.push_str(&format!("/segment{segment}"));
    }
    path.push('/');
    path.push_str(file);
    path
}

criterion_group!(benches, matcher_zlob);
criterion_main!(benches);
