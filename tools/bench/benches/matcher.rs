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

    let deterministic = Pattern::compile(
        "src/[ab]?.[Rr][Ss]",
        PatternOptions::default().case_insensitive(true),
    )
    .expect("deterministic benchmark pattern is valid");
    c.bench_function("deterministic/ferralk_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(deterministic.is_match(black_box("src/aX.RS"))))
    });
    c.bench_function("deterministic/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(deterministic.is_match(black_box("src/zz.rs"))))
    });

    let terminal_recursive = Pattern::compile(
        "src/**",
        PatternOptions::default().recursive_double_star(true),
    )
    .expect("terminal recursive benchmark pattern is valid");
    c.bench_function(
        "recursive_terminal/ferralk_compiled/matching",
        |benchmark| {
            benchmark.iter(|| {
                black_box(terminal_recursive.is_match(black_box("src/deep/nested/main.rs")))
            })
        },
    );
    c.bench_function(
        "recursive_terminal/ferralk_compiled/non_matching",
        |benchmark| {
            benchmark.iter(|| black_box(terminal_recursive.is_match(black_box("lib/main.rs"))))
        },
    );

    let infix_star = Pattern::compile("src*.rs", PatternOptions::default())
        .expect("infix-star benchmark pattern is valid");
    c.bench_function("infix_star/ferralk_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(infix_star.is_match(black_box("src/deep/main.rs"))))
    });
    c.bench_function("infix_star/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(infix_star.is_match(black_box("src/deep/main.txt"))))
    });

    let static_star = Pattern::compile("src/lib/*.rs", PatternOptions::default())
        .expect("static-star benchmark pattern is valid");
    c.bench_function("static_star/ferralk_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(static_star.is_match(black_box("src/lib/main.rs"))))
    });
    c.bench_function("static_star/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(static_star.is_match(black_box("src/lib/main.txt"))))
    });

    let static_prefix_star = Pattern::compile("src/lib/*", PatternOptions::default())
        .expect("static-prefix-star benchmark pattern is valid");
    c.bench_function(
        "static_prefix_star/ferralk_compiled/matching",
        |benchmark| {
            benchmark.iter(|| black_box(static_prefix_star.is_match(black_box("src/lib/main.rs"))))
        },
    );
    c.bench_function(
        "static_prefix_star/ferralk_compiled/non_matching",
        |benchmark| {
            benchmark
                .iter(|| black_box(static_prefix_star.is_match(black_box("src/other/main.rs"))))
        },
    );

    let suffix_star = Pattern::compile("*.rs", PatternOptions::default())
        .expect("suffix-star benchmark pattern is valid");
    c.bench_function("single_star/ferralk_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(suffix_star.is_match(black_box(common_matching))))
    });
    c.bench_function("single_star/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(suffix_star.is_match(black_box(common_non_matching))))
    });

    let root_filter = Pattern::compile("*.rs", PatternOptions::default())
        .expect("root filter benchmark pattern is valid");
    let root_paths = ["lib.rs", "main.rs", "README.md", "src/nested.rs"];
    c.bench_function("path_filter/root", |benchmark| {
        benchmark.iter(|| black_box(root_filter.filter_paths(black_box(&root_paths)).len()))
    });
    let component_filter = Pattern::compile(
        "**/lua/*.lua",
        PatternOptions::default().recursive_double_star(true),
    )
    .expect("component filter benchmark pattern is valid");
    let component_paths = [
        "lua/init.lua",
        "nvim/lua/setup.lua",
        "nvim/lua/sub/nested.lua",
        "src/main.rs",
    ];
    c.bench_function("path_filter/component", |benchmark| {
        benchmark.iter(|| {
            black_box(
                component_filter
                    .filter_paths(black_box(&component_paths))
                    .len(),
            )
        })
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
