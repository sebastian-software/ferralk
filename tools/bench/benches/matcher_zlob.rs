#![forbid(unsafe_code)]

use codspeed_criterion_compat::{Criterion, black_box, criterion_group, criterion_main};
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
}

criterion_group!(benches, matcher_zlob);
criterion_main!(benches);
