//! Retained-size regression coverage for compiled patterns and walkers.
//!
//! Each window compiles something and keeps it: the bytes still allocated when
//! the window closes are what the compiled value holds. `allocation-counter`
//! counts requested sizes on this thread, so the numbers do not depend on the
//! allocator's size classes.
//!
//! The ceilings are loose on purpose, roughly one and a half to two times
//! what was measured when they were set, and each below what the same value
//! held before #405 was worked on, given beside it. They catch a structure
//! that starts growing per alternative or per root again, not a few bytes.

use std::hint::black_box;

use ferralk::{
    Walker,
    ferralk_glob::{Pattern, PatternOptions, PatternSet},
};

/// Bytes still allocated by what `build` returned, measured while it is alive.
fn retained<T>(build: impl FnOnce() -> T) -> u64 {
    let mut kept = None;
    let info = allocation_counter::measure(|| kept = Some(build()));
    let bytes = u64::try_from(info.bytes_current).expect("a kept value retains memory");
    drop(black_box(kept));
    bytes
}

fn includes() -> Vec<String> {
    (0..100)
        .map(|index| match index % 4 {
            0 => format!("pkg{index}/src/**/*.ts"),
            1 => format!("pkg{index}/**/*.{{js,jsx}}"),
            2 => format!("docs{index}/*.md"),
            _ => format!("**/fixture{index}/**"),
        })
        .collect()
}

fn excludes() -> Vec<String> {
    (0..100)
        .map(|index| match index % 4 {
            0 => format!("**/node_modules{index}/**"),
            1 => format!("**/*.tmp{index}"),
            2 => format!("build{index}/**"),
            _ => format!("**/.cache{index}/**"),
        })
        .collect()
}

fn walker(roots: &[&str]) -> Walker {
    let mut walker = Walker::new(roots[0]);
    for pattern in includes() {
        walker.try_include(pattern).expect("valid include");
    }
    for pattern in excludes() {
        walker.try_exclude(pattern).expect("valid exclude");
    }
    for root in &roots[1..] {
        walker
            .try_add_root(root)
            .expect("relative patterns fit every root");
    }
    walker
}

#[test]
fn compiled_patterns_stay_within_their_retained_size() {
    // One-off process and thread state is paid before the first window.
    let _ = retained(|| Pattern::compile("warm/*.{a,b}", PatternOptions::walker()));
    let _ = retained(|| walker(&["warm"]));

    let cases: [(&str, u64, u64); 7] = [
        // (pattern, ceiling, retained before #405)
        ("src/lib.rs", 1_024, 2_019),
        ("**/*.rs", 1_024, 4_063),
        ("docs/*.md", 2 * 1_024, 8_168),
        ("**/node_modules/**", 2 * 1_024, 4_190),
        ("src/**/*.@(ts|tsx|js)", 5 * 1_024, 5_923),
        (
            "*.{rs,ts,tsx,js,jsx,json,md,toml,yaml,css}",
            8 * 1_024,
            32_601,
        ),
        ("{a,b,c,d}/{e,f,g,h}/**/*.{i,j,k,l}", 96 * 1_024, 192_128),
    ];
    for (source, ceiling, before) in cases {
        let bytes =
            retained(|| Pattern::compile(source, PatternOptions::walker()).expect("valid pattern"));
        assert!(
            bytes <= ceiling,
            "{source} retains {bytes} bytes, above its {ceiling}-byte ceiling \
             ({before} before #405)"
        );
    }

    // A set is its members plus an index of their literal parts.
    let globs = includes()
        .into_iter()
        .take(50)
        .chain(excludes().into_iter().take(50))
        .collect::<Vec<_>>();
    let set = retained(|| PatternSet::new(&globs, PatternOptions::walker()).expect("valid set"));
    assert!(
        set <= 192 * 1_024,
        "a set of 100 globs retains {set} bytes (522,135 before #405)"
    );

    // A walker keeps each include and exclude once, whatever it adds to a
    // bare compile: the directory-subtree matcher and the literal prefilters.
    let one_root = retained(|| walker(&["/probe/a"]));
    assert!(
        one_root <= 512 * 1_024,
        "100 includes and 100 excludes retain {one_root} bytes (1,453,141 before #405)"
    );
    // Relative patterns do not depend on the root, so further roots share
    // them rather than adding a copy each.
    let four_roots = retained(|| walker(&["/probe/a", "/probe/b", "/probe/c", "/probe/d"]));
    assert!(
        four_roots <= one_root + 32 * 1_024,
        "four roots retain {four_roots} bytes against {one_root} for one \
         (5,785,483 before #405)"
    );
}
