#![forbid(unsafe_code)]

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ferralk_glob::{Pattern, PatternOptions};
use globset::GlobBuilder;
use wax::{Glob as WaxGlob, Program as _};

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
    let wax = WaxGlob::new(common_pattern).expect("common benchmark wax pattern is valid");
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

    // These component-local calls mirror the common suffix filters used by the
    // walker. Keep root, deep, and separator-rejection inputs side by side so a
    // shortcut cannot look fast by quietly accepting a nested plain-star path.
    let walker_star = Pattern::compile("*.ts", PatternOptions::default())
        .expect("walker star-suffix benchmark pattern is valid");
    let walker_recursive = Pattern::compile(
        "**/*.ts",
        PatternOptions::default().recursive_double_star(true),
    )
    .expect("walker recursive benchmark pattern is valid");
    let walker_scoped_recursive = Pattern::compile(
        "src/**/*.ts",
        PatternOptions::default().recursive_double_star(true),
    )
    .expect("walker scoped recursive benchmark pattern is valid");
    let walker_root = "index.ts";
    let walker_path = "src/deep/nested/module/component/widget/main.ts";
    let walker_other = "src/deep/nested/module/component/widget/main.tsx";
    assert!(walker_star.is_match_glob_path(walker_root));
    assert!(!walker_star.is_match_glob_path(walker_path));
    assert!(walker_recursive.is_match_glob_path(walker_root));
    assert!(walker_recursive.is_match_glob_path(walker_path));
    assert!(walker_scoped_recursive.is_match_glob_path(walker_path));

    c.bench_function("common_suffix/component_star/basename", |benchmark| {
        benchmark.iter(|| black_box(walker_star.is_match_glob_path(black_box(walker_root))))
    });
    c.bench_function(
        "common_suffix/component_star/nested_rejection",
        |benchmark| {
            benchmark.iter(|| black_box(walker_star.is_match_glob_path(black_box(walker_path))))
        },
    );
    c.bench_function("common_suffix/recursive_suffix/root", |benchmark| {
        benchmark.iter(|| black_box(walker_recursive.is_match_glob_path(black_box(walker_root))))
    });
    c.bench_function("common_suffix/recursive_suffix/deep", |benchmark| {
        benchmark.iter(|| black_box(walker_recursive.is_match_glob_path(black_box(walker_path))))
    });
    c.bench_function("common_suffix/scoped_recursive_suffix/deep", |benchmark| {
        benchmark
            .iter(|| black_box(walker_scoped_recursive.is_match_glob_path(black_box(walker_path))))
    });
    c.bench_function("walker_component/recursive_suffix/matching", |benchmark| {
        benchmark.iter(|| black_box(walker_recursive.is_match_glob_path(black_box(walker_path))))
    });
    c.bench_function(
        "walker_component/recursive_suffix/non_matching",
        |benchmark| {
            benchmark
                .iter(|| black_box(walker_recursive.is_match_glob_path(black_box(walker_other))))
        },
    );

    let general_literal_skip = Pattern::compile("*a*b.ts", PatternOptions::default())
        .expect("general literal-skip benchmark pattern is valid");
    c.bench_function("general_literal_skip/matching", |benchmark| {
        benchmark.iter(|| black_box(general_literal_skip.is_match(black_box(walker_path))))
    });
    c.bench_function("general_literal_skip/non_matching", |benchmark| {
        benchmark.iter(|| black_box(general_literal_skip.is_match(black_box(walker_other))))
    });

    // Issue #15's measurement: two spellings of the same anchored alternation.
    // The extglob form is the one that used to interpret pattern bytes per
    // match, so the pair is kept side by side to keep the gap honest.
    let alternation_options = PatternOptions::default()
        .recursive_double_star(true)
        .extglob(true)
        .braces(true);
    let alternation_extglob = Pattern::compile("@(foo|bar)/**/*.ts", alternation_options)
        .expect("extglob alternation benchmark pattern is valid");
    let alternation_braces = Pattern::compile("{foo,bar}/**/*.ts", alternation_options)
        .expect("brace alternation benchmark pattern is valid");
    let alternation_path = "foo/src/deep/nested/dir1/file1.ts";
    let alternation_other = "baz/src/deep/nested/dir1/file1.ts";
    c.bench_function("alternation/extglob/matching", |benchmark| {
        benchmark.iter(|| black_box(alternation_extglob.is_match(black_box(alternation_path))))
    });
    c.bench_function("alternation/extglob/non_matching", |benchmark| {
        benchmark.iter(|| black_box(alternation_extglob.is_match(black_box(alternation_other))))
    });
    c.bench_function("alternation/braces/matching", |benchmark| {
        benchmark.iter(|| black_box(alternation_braces.is_match(black_box(alternation_path))))
    });
    c.bench_function("alternation/braces/non_matching", |benchmark| {
        benchmark.iter(|| black_box(alternation_braces.is_match(black_box(alternation_other))))
    });

    // A positive extglob without an outer star uses the Thompson NFA. Keep an
    // equivalent brace spelling beside it so the per-match state-management
    // cost remains visible independently of the compatible extglob fallback.
    let nfa_extglob = Pattern::compile("@(src|tests)/lib.rs", alternation_options)
        .expect("fixed extglob benchmark pattern is valid");
    let nfa_braces = Pattern::compile("{src,tests}/lib.rs", alternation_options)
        .expect("fixed brace benchmark pattern is valid");
    let nfa_matching = "src/lib.rs";
    let nfa_non_matching = "vendor/lib.rs";
    for candidate in [nfa_matching, nfa_non_matching] {
        assert_eq!(
            nfa_extglob.is_match_glob_path(candidate),
            nfa_braces.is_match_glob_path(candidate),
            "fixed extglob and brace benchmark inputs must have equal semantics"
        );
    }
    c.bench_function("extglob_nfa/fixed/matching", |benchmark| {
        benchmark.iter(|| black_box(nfa_extglob.is_match_glob_path(black_box(nfa_matching))))
    });
    c.bench_function("extglob_nfa/fixed/non_matching", |benchmark| {
        benchmark.iter(|| black_box(nfa_extglob.is_match_glob_path(black_box(nfa_non_matching))))
    });
    c.bench_function("extglob_nfa/brace/matching", |benchmark| {
        benchmark.iter(|| black_box(nfa_braces.is_match_glob_path(black_box(nfa_matching))))
    });
    c.bench_function("extglob_nfa/brace/non_matching", |benchmark| {
        benchmark.iter(|| black_box(nfa_braces.is_match_glob_path(black_box(nfa_non_matching))))
    });
    // The walker compiles every traversal pattern with extglob enabled, so a
    // pattern without extglob syntax used to pay a scan per entry too.
    let extglob_free = Pattern::compile("src/**/*.ts", alternation_options)
        .expect("extglob-free benchmark pattern is valid");
    c.bench_function("alternation/extglob_enabled_but_unused", |benchmark| {
        benchmark
            .iter(|| black_box(extglob_free.is_match(black_box("src/deep/nested/dir1/file1.ts"))))
    });

    // Sequential repetitions used to re-explore every suffix for every way
    // earlier groups split this run. Keep the failure shape here: a match
    // returns at the first successful partition, while the trailing byte
    // exercises the memo across all of them.
    let chained_repetition =
        Pattern::compile("+(a)+(a)+(a)+(a)", PatternOptions::default().extglob(true))
            .expect("chained extglob repetition pattern is valid");
    let mut chained_repetition_path = "a".repeat(400);
    chained_repetition_path.push('x');
    c.bench_function("extglob/chained_repetition/non_matching", |benchmark| {
        benchmark
            .iter(|| black_box(chained_repetition.is_match(black_box(&chained_repetition_path))))
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
    c.bench_function("common/wax_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(wax.is_match(black_box(common_matching))))
    });
    c.bench_function("common/wax_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(wax.is_match(black_box(common_non_matching))))
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
    node_pattern_sets(c);
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
    let wax = WaxGlob::new(pattern).expect("long-path wax pattern is valid");

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
    c.bench_function("long_path/wax_compiled/matching", |benchmark| {
        benchmark.iter(|| black_box(wax.is_match(black_box(matching.as_str()))))
    });
    c.bench_function("long_path/wax_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(wax.is_match(black_box(non_matching.as_str()))))
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
    let wax = WaxGlob::new(pattern).expect("adversarial wax pattern is valid");

    c.bench_function("backtracking/ferralk_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(ferralk.is_match(black_box(candidate.as_str()))))
    });
    c.bench_function("backtracking/globset_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(globset.is_match(black_box(candidate.as_str()))))
    });
    c.bench_function("backtracking/wax_compiled/non_matching", |benchmark| {
        benchmark.iter(|| black_box(wax.is_match(black_box(candidate.as_str()))))
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

/// Common Node/TypeScript catalogs, with the best and worst positions kept
/// separate so a faster first alternative cannot hide a linear tail.
fn node_pattern_sets(c: &mut Criterion) {
    const EXTENSIONS: [&str; 6] = ["ts", "tsx", "js", "jsx", "mjs", "cjs"];
    const BRACE_SOURCE: &str = "**/*.{ts,tsx,js,jsx,mjs,cjs}";
    const SCOPED_SOURCE: &str = "{src,packages,apps}/**/*.{ts,tsx,js,jsx,mjs,cjs}";
    let options = PatternOptions::default()
        .braces(true)
        .recursive_double_star(true)
        .extglob(true);
    let brace = Pattern::compile(BRACE_SOURCE, options).expect("Node brace pattern is valid");
    let scoped =
        Pattern::compile(SCOPED_SOURCE, options).expect("scoped Node brace pattern is valid");
    let catalog = EXTENSIONS
        .iter()
        .map(|extension| {
            Pattern::compile(format!("**/*.{extension}"), options)
                .expect("Node catalog pattern is valid")
        })
        .collect::<Vec<_>>();
    let first = "packages/frontend/src/components/navigation/header.ts";
    let last = "packages/server/dist/runtime/worker.cjs";
    let rejected = "packages/frontend/src/components/navigation/header.vue";
    let scoped_first = "src/components/navigation/header.ts";
    let scoped_last = "apps/server/src/runtime/worker.cjs";
    let scoped_wrong_root = "vendor/frontend/src/components/header.ts";

    let mut compile = c.benchmark_group("node_pattern_set/compile");
    compile.bench_function("brace", |benchmark| {
        benchmark.iter(|| {
            black_box(
                Pattern::compile(black_box(BRACE_SOURCE), black_box(options))
                    .expect("Node brace pattern is valid"),
            )
        })
    });
    compile.bench_function("scoped_brace", |benchmark| {
        benchmark.iter(|| {
            black_box(
                Pattern::compile(black_box(SCOPED_SOURCE), black_box(options))
                    .expect("scoped Node brace pattern is valid"),
            )
        })
    });
    compile.finish();

    assert!(brace.is_match_glob_path(first));
    assert!(brace.is_match_glob_path(last));
    assert!(!brace.is_match_glob_path(rejected));
    assert!(scoped.is_match_glob_path(scoped_first));
    assert!(scoped.is_match_glob_path(scoped_last));
    assert!(!scoped.is_match_glob_path(scoped_wrong_root));
    for candidate in [first, last, rejected] {
        assert_eq!(
            brace.is_match_glob_path(candidate),
            catalog
                .iter()
                .any(|pattern| pattern.is_match_glob_path(candidate)),
            "brace and catalog forms must select the same benchmark candidate"
        );
    }

    let mut point = c.benchmark_group("node_pattern_set/point");
    point.bench_function("brace/first_extension", |benchmark| {
        benchmark.iter(|| black_box(brace.is_match_glob_path(black_box(first))))
    });
    point.bench_function("brace/last_extension", |benchmark| {
        benchmark.iter(|| black_box(brace.is_match_glob_path(black_box(last))))
    });
    point.bench_function("brace/rejected_extension", |benchmark| {
        benchmark.iter(|| black_box(brace.is_match_glob_path(black_box(rejected))))
    });
    point.bench_function("scoped_brace/first_alternative", |benchmark| {
        benchmark.iter(|| black_box(scoped.is_match_glob_path(black_box(scoped_first))))
    });
    point.bench_function("scoped_brace/last_alternative", |benchmark| {
        benchmark.iter(|| black_box(scoped.is_match_glob_path(black_box(scoped_last))))
    });
    point.bench_function("scoped_brace/rejected_root", |benchmark| {
        benchmark.iter(|| black_box(scoped.is_match_glob_path(black_box(scoped_wrong_root))))
    });
    point.bench_function("catalog/first_extension", |benchmark| {
        benchmark.iter(|| {
            black_box(
                catalog
                    .iter()
                    .any(|pattern| pattern.is_match_glob_path(black_box(first))),
            )
        })
    });
    point.bench_function("catalog/last_extension", |benchmark| {
        benchmark.iter(|| {
            black_box(
                catalog
                    .iter()
                    .any(|pattern| pattern.is_match_glob_path(black_box(last))),
            )
        })
    });
    point.bench_function("catalog/rejected_extension", |benchmark| {
        benchmark.iter(|| {
            black_box(
                catalog
                    .iter()
                    .any(|pattern| pattern.is_match_glob_path(black_box(rejected))),
            )
        })
    });
    point.finish();

    let paths = (0..1024)
        .map(|index| {
            let extension = match index % 4 {
                0 => "ts",
                1 => "tsx",
                2 => "cjs",
                _ => "vue",
            };
            format!("packages/package-{index}/src/components/widget-{index}.{extension}")
        })
        .collect::<Vec<_>>();
    let brace_count = paths
        .iter()
        .filter(|path| brace.is_match_glob_path(path.as_bytes()))
        .count();
    let catalog_count = paths
        .iter()
        .filter(|path| {
            catalog
                .iter()
                .any(|pattern| pattern.is_match_glob_path(path.as_bytes()))
        })
        .count();
    let scoped_count = paths
        .iter()
        .filter(|path| scoped.is_match_glob_path(path.as_bytes()))
        .count();
    assert_eq!(brace_count, 768);
    assert_eq!(catalog_count, brace_count);
    assert_eq!(scoped_count, brace_count);

    let mut list = c.benchmark_group("node_pattern_set/list");
    list.throughput(Throughput::Elements(paths.len() as u64));
    list.bench_function("brace/1024_paths", |benchmark| {
        benchmark.iter(|| {
            black_box(
                paths
                    .iter()
                    .filter(|path| brace.is_match_glob_path(black_box(path.as_bytes())))
                    .count(),
            )
        })
    });
    list.bench_function("scoped_brace/1024_paths", |benchmark| {
        benchmark.iter(|| {
            black_box(
                paths
                    .iter()
                    .filter(|path| scoped.is_match_glob_path(black_box(path.as_bytes())))
                    .count(),
            )
        })
    });
    list.bench_function("catalog/1024_paths", |benchmark| {
        benchmark.iter(|| {
            black_box(
                paths
                    .iter()
                    .filter(|path| {
                        catalog
                            .iter()
                            .any(|pattern| pattern.is_match_glob_path(black_box(path.as_bytes())))
                    })
                    .count(),
            )
        })
    });
    list.finish();
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
