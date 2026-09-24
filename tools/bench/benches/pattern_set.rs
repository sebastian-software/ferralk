#![forbid(unsafe_code)]
//! `PatternSet` against a loop over its `Pattern`s and against
//! `globset::GlobSet`, on 10-, 100- and 1000-glob lists.
//!
//! Every arm answers one question per path — does any glob select it, or
//! which ones do — over the same 64 root-relative paths, so a reported time
//! divided by 64 is the cost of one path. `globset` is built with
//! `literal_separator(true)`, the reading `is_match_glob_path` has.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ferralk_glob::{Pattern, PatternOptions, PatternSet};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// The shapes long include and exclude lists are made of, in rotation:
/// extensions, directory subtrees, directory names at any depth, file names
/// at any depth, and name prefixes. Every glob is distinct.
fn keyed_globs(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| match index % 5 {
            0 => format!("**/*.ext{index}"),
            1 => format!("dir{index}/**"),
            2 => format!("**/name{index}/**"),
            3 => format!("**/File{index}.txt"),
            _ => format!("prefix{index}*"),
        })
        .collect()
}

/// A list the index can say nothing about: every glob has a wildcard on both
/// sides of its only literal, so the set asks every member like a loop does.
fn unkeyed_globs(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("**/*x{index}y*")).collect()
}

/// Paths of a source repository. A few are selected by the generated lists;
/// most are not, which is the common case for an exclude list.
fn paths() -> Vec<String> {
    let mut paths = Vec::new();
    for area in 0..4 {
        for module in 0..4 {
            paths.push(format!("src/area-{area}/module-{module}/view.tsx"));
            paths.push(format!("src/area-{area}/module-{module}/unit.test.ts"));
            paths.push(format!(
                "node_modules/pkg-{area}/dist/module-{module}/index.js"
            ));
        }
    }
    paths.extend(
        [
            "Cargo.toml",
            "README.md",
            "dir1/src/lib.rs",
            "a/b/name2/c.js",
            "deep/File3.txt",
            "prefix4-notes.md",
            "docs/guide/x7y.md",
            "src/main.ext0",
            "target/debug/build/output.ext5",
            ".github/workflows/ci.yml",
            "crates/cli/src/main.rs",
            "crates/cli/tests/integration.rs",
            "packages/web/src/index.ts",
            "packages/web/package.json",
            "tools/bench/benches/pattern_set.rs",
            "LICENSE",
        ]
        .map(String::from),
    );
    assert_eq!(paths.len(), 64);
    paths
}

fn globset(globs: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(
            GlobBuilder::new(glob)
                .literal_separator(true)
                .build()
                .expect("the benchmark glob is valid for globset"),
        );
    }
    builder.build().expect("the benchmark set builds")
}

fn pattern_set(c: &mut Criterion) {
    let options = PatternOptions::walker();
    let paths = paths();
    let mut group = c.benchmark_group("pattern_set");
    group.throughput(Throughput::Elements(paths.len() as u64));
    for (shape, make) in [
        ("keyed", keyed_globs as fn(usize) -> Vec<String>),
        ("unkeyed", unkeyed_globs),
    ] {
        for count in [10, 100, 1000] {
            let globs = make(count);
            let patterns = globs
                .iter()
                .map(|glob| Pattern::compile(glob, options))
                .collect::<Result<Vec<_>, _>>()
                .expect("the benchmark globs compile");
            let set = PatternSet::new(&globs, options).expect("the benchmark globs compile");
            let globset = globset(&globs);
            for path in &paths {
                let expected = patterns
                    .iter()
                    .any(|pattern| pattern.is_match_glob_path(path));
                assert_eq!(set.is_match_glob_path(path), expected, "{path}");
                assert_eq!(globset.is_match(path), expected, "{path}");
            }
            let id = |arm: &str| BenchmarkId::new(format!("{shape}/{arm}"), count);

            group.bench_function(id("is_match/pattern_loop"), |bench| {
                bench.iter(|| {
                    paths
                        .iter()
                        .filter(|path| {
                            patterns
                                .iter()
                                .any(|pattern| pattern.is_match_glob_path(black_box(path)))
                        })
                        .count()
                })
            });
            group.bench_function(id("is_match/pattern_set"), |bench| {
                bench.iter(|| {
                    paths
                        .iter()
                        .filter(|path| set.is_match_glob_path(black_box(path)))
                        .count()
                })
            });
            group.bench_function(id("is_match/globset"), |bench| {
                bench.iter(|| {
                    paths
                        .iter()
                        .filter(|path| globset.is_match(black_box(path.as_str())))
                        .count()
                })
            });

            let mut matches = Vec::new();
            group.bench_function(id("matches/pattern_set"), |bench| {
                bench.iter(|| {
                    paths
                        .iter()
                        .map(|path| {
                            set.matches_glob_path_into(black_box(path), &mut matches);
                            matches.len()
                        })
                        .sum::<usize>()
                })
            });
            group.bench_function(id("matches/globset"), |bench| {
                bench.iter(|| {
                    paths
                        .iter()
                        .map(|path| {
                            globset.matches_into(black_box(path.as_str()), &mut matches);
                            matches.len()
                        })
                        .sum::<usize>()
                })
            });
        }
    }
    group.finish();

    let mut build = c.benchmark_group("pattern_set_build");
    let globs = keyed_globs(100);
    build.bench_function("keyed/100/pattern_set", |bench| {
        bench.iter(|| PatternSet::new(black_box(&globs), options).expect("valid"))
    });
    build.bench_function("keyed/100/pattern_loop", |bench| {
        bench.iter(|| {
            black_box(&globs)
                .iter()
                .map(|glob| Pattern::compile(glob, options))
                .collect::<Result<Vec<_>, _>>()
                .expect("valid")
        })
    });
    build.bench_function("keyed/100/globset", |bench| {
        bench.iter(|| globset(black_box(&globs)))
    });
    build.finish();
}

criterion_group!(benches, pattern_set);
criterion_main!(benches);
