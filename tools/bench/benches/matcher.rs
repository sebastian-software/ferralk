#![forbid(unsafe_code)]

use codspeed_criterion_compat::{Criterion, black_box, criterion_group, criterion_main};
use ferralk_glob::{Pattern, PatternOptions};

fn matcher(c: &mut Criterion) {
    let pattern = Pattern::compile(
        "src/**/+(main|lib).{rs,toml}",
        PatternOptions::default()
            .braces(true)
            .recursive_double_star(true)
            .extglob(true),
    )
    .expect("benchmark pattern is valid");
    let matching = b"src/deep/nested/main.rs";
    let non_matching = b"src/deep/nested/main.txt";

    c.bench_function("compiled_matcher/matching", |benchmark| {
        benchmark.iter(|| black_box(pattern.is_match(black_box(matching))))
    });
    c.bench_function("compiled_matcher/non_matching", |benchmark| {
        benchmark.iter(|| black_box(pattern.is_match(black_box(non_matching))))
    });
}

criterion_group!(benches, matcher);
criterion_main!(benches);
