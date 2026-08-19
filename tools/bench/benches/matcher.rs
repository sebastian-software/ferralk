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

    c.bench_function("compile/long_recursive_pattern", |benchmark| {
        benchmark.iter(|| {
            black_box(
                Pattern::compile(
                    black_box("src/**/vendor/*/lib/[a-z]*/{main,mod,lib}.{rs,toml}"),
                    PatternOptions::default()
                        .braces(true)
                        .recursive_double_star(true),
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
    let common_casefold = Pattern::compile(
        "Src/**/*.RS",
        PatternOptions::default()
            .recursive_double_star(true)
            .case_insensitive(true),
    )
    .expect("case-folded common pattern is valid");
    c.bench_function(
        "recursive_casefold/ferralk_compiled/matching",
        |benchmark| {
            benchmark.iter(|| black_box(common_casefold.is_match(black_box("src/deep/main.rs"))))
        },
    );
    c.bench_function(
        "recursive_casefold/ferralk_compiled/non_matching",
        |benchmark| {
            benchmark.iter(|| black_box(common_casefold.is_match(black_box("src/deep/main.txt"))))
        },
    );

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

    let general = Pattern::compile("src/*[a-z]?*.rs", PatternOptions::default())
        .expect("general benchmark pattern is valid");
    c.bench_function("general_ir/ferralk_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(general.is_match(black_box("src/deep/main.rs"))))
    });
    c.bench_function("general_ir/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(general.is_match(black_box("src/deep/main.txt"))))
    });

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
    let deterministic_component_filter = Pattern::compile(
        "src/[ab]?.[Rr][Ss]",
        PatternOptions::default().case_insensitive(true),
    )
    .expect("deterministic component pattern is valid");
    let deterministic_component_paths = [
        "src/a1.rs",
        "src/Bx.RS",
        "src/c1.rs",
        "src/a/.rs",
        "src/.1.rs",
        "src/a1.rs/extra",
    ];
    c.bench_function("path_filter/deterministic_component", |benchmark| {
        benchmark.iter(|| {
            black_box(
                deterministic_component_filter
                    .filter_paths(black_box(&deterministic_component_paths))
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
    // Every comparator input is black-boxed, including the pattern. These two
    // benches used to pass compile-time constants, which let the optimizer
    // fold work the ferralk and globset benches still had to do.
    c.bench_function("common/fast_glob_interpreted/matching", |benchmark| {
        benchmark.iter(|| {
            black_box(fast_glob::glob_match(
                black_box(common_pattern),
                black_box(common_matching),
            ))
        })
    });
    c.bench_function("common/fast_glob_interpreted/non_matching", |benchmark| {
        benchmark.iter(|| {
            black_box(fast_glob::glob_match(
                black_box(common_pattern),
                black_box(common_non_matching),
            ))
        })
    });

    comparators(c);
    long_paths(c);
    adversarial(c);
    large_path_filter(c);
}

/// Pendants for the scenarios that previously ran against no comparator.
fn comparators(c: &mut Criterion) {
    let literal = GlobBuilder::new("src/deep/nested/main.rs")
        .literal_separator(true)
        .build()
        .expect("literal comparator pattern is valid")
        .compile_matcher();
    c.bench_function("literal/globset_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(literal.is_match(black_box("src/deep/nested/main.rs"))))
    });
    c.bench_function("literal/globset_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(literal.is_match(black_box("src/deep/nested/main.txt"))))
    });

    let casefold = GlobBuilder::new("Src/**/*.RS")
        .literal_separator(true)
        .case_insensitive(true)
        .build()
        .expect("case-folded comparator pattern is valid")
        .compile_matcher();
    c.bench_function(
        "recursive_casefold/globset_compiled/matching",
        |benchmark| benchmark.iter(|| black_box(casefold.is_match(black_box("src/deep/main.rs")))),
    );
    c.bench_function(
        "recursive_casefold/globset_compiled/non_matching",
        |benchmark| benchmark.iter(|| black_box(casefold.is_match(black_box("src/deep/main.txt")))),
    );

    let deterministic = GlobBuilder::new("src/[ab]?.[Rr][Ss]")
        .literal_separator(true)
        .case_insensitive(true)
        .build()
        .expect("deterministic comparator pattern is valid")
        .compile_matcher();
    c.bench_function("deterministic/globset_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(deterministic.is_match(black_box("src/aX.RS"))))
    });
    c.bench_function("deterministic/globset_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(deterministic.is_match(black_box("src/zz.rs"))))
    });
}

/// Candidates long enough that per-byte cost dominates fixed overhead.
///
/// The existing candidates are 15 to 25 bytes, short enough that a matcher's
/// setup cost hides its scanning cost.
fn long_paths(c: &mut Criterion) {
    let matching = long_path("main.rs");
    let non_matching = long_path("main.txt");
    assert!(
        matching.len() > 100,
        "the long candidate must exceed 100 bytes"
    );

    let pattern = "src/**/*.rs";
    let ferralk = Pattern::compile(
        pattern,
        PatternOptions::default().recursive_double_star(true),
    )
    .expect("long-path benchmark pattern is valid");
    let globset = GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .expect("long-path comparator pattern is valid")
        .compile_matcher();

    c.bench_function("long_path/ferralk_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(ferralk.is_match(black_box(matching.as_str()))))
    });
    c.bench_function("long_path/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(ferralk.is_match(black_box(non_matching.as_str()))))
    });
    c.bench_function("long_path/globset_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(globset.is_match(black_box(matching.as_str()))))
    });
    c.bench_function("long_path/globset_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(globset.is_match(black_box(non_matching.as_str()))))
    });
    c.bench_function("long_path/fast_glob_interpreted/matching", |benchmark| {
        benchmark.iter(|| {
            black_box(fast_glob::glob_match(
                black_box(pattern),
                black_box(matching.as_str()),
            ))
        })
    });
    c.bench_function(
        "long_path/fast_glob_interpreted/non_matching",
        |benchmark| {
            benchmark.iter(|| {
                black_box(fast_glob::glob_match(
                    black_box(pattern),
                    black_box(non_matching.as_str()),
                ))
            })
        },
    );
}

/// The star-chain worst case, where a naive matcher backtracks exponentially.
///
/// `a*a*a*a*b` against a run of `a` never matches, so every engine must
/// exhaust its search. ferralk memoizes failed token/candidate pairs, which is
/// exactly what this bench guards.
fn adversarial(c: &mut Criterion) {
    let pattern = "a*a*a*a*b";
    let candidate = "a".repeat(64);
    let ferralk =
        Pattern::compile(pattern, PatternOptions::default()).expect("adversarial pattern is valid");
    let globset = GlobBuilder::new(pattern)
        .build()
        .expect("adversarial comparator pattern is valid")
        .compile_matcher();

    c.bench_function("backtracking/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(ferralk.is_match(black_box(candidate.as_str()))))
    });
    c.bench_function("backtracking/globset_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(globset.is_match(black_box(candidate.as_str()))))
    });
    c.bench_function(
        "backtracking/fast_glob_interpreted/non_matching",
        |benchmark| {
            benchmark.iter(|| {
                black_box(fast_glob::glob_match(
                    black_box(pattern),
                    black_box(candidate.as_str()),
                ))
            })
        },
    );
}

/// A candidate list at the size a caller actually filters.
///
/// The other `path_filter` benches pass four to six entries, where the call
/// overhead dominates the per-entry work.
fn large_path_filter(c: &mut Criterion) {
    let paths: Vec<String> = (0..1024)
        .map(|index| {
            if index % 3 == 0 {
                format!("src/module{index}/lib.rs")
            } else {
                format!("docs/module{index}/notes.md")
            }
        })
        .collect();
    let filter = Pattern::compile(
        "src/**/*.rs",
        PatternOptions::default().recursive_double_star(true),
    )
    .expect("large-list filter pattern is valid");

    c.bench_function("path_filter/large_list", |benchmark| {
        benchmark.iter(|| black_box(filter.filter_paths(black_box(paths.as_slice())).len()))
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

criterion_group!(benches, matcher);
criterion_main!(benches);
