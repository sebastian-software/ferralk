#![forbid(unsafe_code)]

use codspeed_criterion_compat::{Criterion, black_box, criterion_group, criterion_main};
use ferralk_glob::{Pattern, PatternOptions};
use globset::GlobBuilder;

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
    c.bench_function("compile/posix_classes_and_braces", |benchmark| {
        benchmark.iter(|| {
            black_box(
                Pattern::compile(
                    "src/{lib,bin}/[[:alpha:]][[:digit:]]*.{rs,toml}",
                    PatternOptions::default().braces(true),
                )
                .expect("compile benchmark pattern is valid"),
            )
        })
    });

    let common_pattern = "src/**/*.rs";
    let common_options = PatternOptions::default().recursive_double_star(true);
    let ferralk = Pattern::compile(common_pattern, common_options)
        .expect("common benchmark pattern is valid");
    let globset = GlobBuilder::new(common_pattern)
        .literal_separator(true)
        .build()
        .expect("common benchmark globset pattern is valid")
        .compile_matcher();
    let common_matching = "src/deep/nested/main.rs";
    let common_non_matching = "src/deep/nested/main.txt";

    c.bench_function("common/ferralk_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(ferralk.is_match(black_box(common_matching))))
    });
    c.bench_function("common/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(ferralk.is_match(black_box(common_non_matching))))
    });

    let literal = Pattern::compile("src/deep/nested/main.rs", PatternOptions::default())
        .expect("literal benchmark pattern is valid");
    c.bench_function("literal/ferralk_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(literal.is_match(black_box(common_matching))))
    });
    c.bench_function("literal/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(literal.is_match(black_box(common_non_matching))))
    });

    let suffix_star = Pattern::compile("*.rs", PatternOptions::default())
        .expect("suffix-star benchmark pattern is valid");
    c.bench_function("single_star/ferralk_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(suffix_star.is_match(black_box(common_matching))))
    });
    c.bench_function("single_star/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(suffix_star.is_match(black_box(common_non_matching))))
    });
    c.bench_function("common/globset_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(globset.is_match(black_box(common_matching))))
    });
    c.bench_function("common/globset_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(globset.is_match(black_box(common_non_matching))))
    });
    c.bench_function("common/fast_glob_interpreted/matching", |benchmark| {
        benchmark.iter(|| black_box(fast_glob::glob_match(common_pattern, common_matching)))
    });
    c.bench_function("common/fast_glob_interpreted/non_matching", |benchmark| {
        benchmark.iter(|| black_box(fast_glob::glob_match(common_pattern, common_non_matching)))
    });
}

criterion_group!(benches, matcher);
criterion_main!(benches);
