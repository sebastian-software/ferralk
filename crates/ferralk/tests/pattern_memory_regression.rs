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

use std::{
    fs,
    hint::black_box,
    io,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{
    ErrorPolicy, Walker,
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

static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

struct TempFixture {
    root: PathBuf,
}

impl TempFixture {
    fn new(label: &str) -> Self {
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "ferralk-{label}-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after epoch")
                    .as_nanos(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create temp fixture: {error}"),
            }
        };
        // Isolate every fixture as its own Git repository root so walks never
        // inherit ancestor ignore files when `TMPDIR` lies inside a checkout.
        fs::create_dir(root.join(".git")).expect("create fixture .git");
        Self { root }
    }
}

impl Drop for TempFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn ignore_and_git_config_safety_budgets_bound_peak_memory() {
    // 1. An 8 MiB `.git/config` consisting of ~8M newline lines must stream
    //    without per-line `Vec` allocations (previously >8M allocs / >200 MiB).
    let config_fixture = TempFixture::new("config-stream");
    let suffix = b"[core]\nignorecase = true\n";
    let mut config_bytes = vec![b'\n'; 8 * 1024 * 1024 - suffix.len()];
    config_bytes.extend_from_slice(suffix);
    fs::write(config_fixture.root.join(".git/config"), config_bytes).expect("write .git/config");
    fs::write(config_fixture.root.join(".gitignore"), b"IGNORED.LOG\n").expect("write .gitignore");
    fs::write(config_fixture.root.join("ignored.log"), b"x").expect("write ignored.log");
    fs::write(config_fixture.root.join("visible.txt"), b"x").expect("write visible.txt");

    let mut config_result = None;
    let config_allocs = allocation_counter::measure(|| {
        config_result = Some(
            Walker::new(&config_fixture.root)
                .respect_git_ignore(true)
                .threads(1)
                .collect()
                .expect("walk with large .git/config succeeds"),
        );
    });
    let config_result = config_result.expect("walk produced result");
    assert!(config_result.errors().is_empty());
    assert!(
        config_result
            .entries()
            .iter()
            .any(|entry| entry.path().ends_with("visible.txt"))
    );
    assert!(
        !config_result
            .entries()
            .iter()
            .any(|entry| entry.path().ends_with("ignored.log")),
        "core.ignorecase = true at the end of the 8 MiB config must take effect"
    );
    assert!(
        config_allocs.count_total < 4_096,
        "streaming an 8 MiB newline .git/config made {} allocations",
        config_allocs.count_total
    );
    // `read_to_end` on an 8 MiB file can double from 8 MiB to 16 MiB (24 MiB
    // transient peak during realloc). Thirty-two MiB stays far below the old
    // >200 MiB per-line `Vec<Vec<u8>>` allocation amplification.
    assert!(
        config_allocs.bytes_max < 32 * 1024 * 1024,
        "streaming an 8 MiB newline .git/config peaked at {} bytes",
        config_allocs.bytes_max
    );

    // 2. A 1-line ~8 MiB `.gitignore` of `"a/"` repeated 4M times must fail
    //    fast before allocating millions of `RulePart`s / `Pattern`s
    //    (previously >12M allocs / ~1 GiB).
    let ignore_fixture = TempFixture::new("ignore-explode");
    let mut exploding = Vec::with_capacity(8 * 1024 * 1024);
    for _ in 0..4_000_000 {
        exploding.extend_from_slice(b"a/");
    }
    exploding.push(b'\n');
    fs::write(ignore_fixture.root.join(".gitignore"), exploding).expect("write exploding ignore");
    fs::write(ignore_fixture.root.join("visible.txt"), b"x").expect("write visible.txt");

    let mut ignore_result = None;
    let ignore_allocs = allocation_counter::measure(|| {
        ignore_result = Some(
            Walker::new(&ignore_fixture.root)
                .respect_git_ignore(true)
                .threads(1)
                .error_policy(ErrorPolicy::Collect)
                .collect()
                .expect("collect policy keeps walking"),
        );
    });
    let ignore_result = ignore_result.expect("walk produced result");
    assert_eq!(ignore_result.errors().len(), 1);
    assert_eq!(
        ignore_result.errors()[0].operation().as_str(),
        "read_ignore"
    );
    assert_eq!(
        ignore_result.errors()[0].io_kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        ignore_allocs.count_total < 4_096,
        "rejecting a 4M-component ignore line made {} allocations",
        ignore_allocs.count_total
    );
    // Thirty-two MiB leaves headroom for `read_to_end`'s doubling buffer on an
    // 8 MiB file while staying far below the old ~1 GiB component explosion.
    assert!(
        ignore_allocs.bytes_max < 32 * 1024 * 1024,
        "rejecting a 4M-component ignore line peaked at {} bytes",
        ignore_allocs.bytes_max
    );

    // 3. Repeated 60,000-`?` lines inside a ~6 MiB `.gitignore` must fail fast
    //    on compiled matcher units rather than retaining hundreds of MiB of
    //    `Pattern` tokens across all 100 lines.
    let qmark_fixture = TempFixture::new("ignore-qmarks");
    let mut qmarks = Vec::with_capacity(100 * 60_001);
    let qline = format!("{}\n", "?".repeat(60_000));
    for _ in 0..100 {
        qmarks.extend_from_slice(qline.as_bytes());
    }
    fs::write(qmark_fixture.root.join(".gitignore"), qmarks).expect("write qmark ignore");
    fs::write(qmark_fixture.root.join("visible.txt"), b"x").expect("write visible.txt");

    let mut qmark_result = None;
    let qmark_allocs = allocation_counter::measure(|| {
        qmark_result = Some(
            Walker::new(&qmark_fixture.root)
                .respect_git_ignore(true)
                .threads(1)
                .error_policy(ErrorPolicy::Collect)
                .collect()
                .expect("collect policy keeps walking"),
        );
    });
    let qmark_result = qmark_result.expect("walk produced result");
    assert_eq!(qmark_result.errors().len(), 1);
    assert_eq!(qmark_result.errors()[0].operation().as_str(), "read_ignore");
    assert_eq!(
        qmark_result.errors()[0].io_kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        qmark_allocs.bytes_max < 256 * 1024 * 1024,
        "rejecting 100 x 60,000-`?` ignore lines peaked at {} bytes",
        qmark_allocs.bytes_max
    );

    // 4. Multi-star `WideSweepEngine` bombs (`*` + 40 KiB literal + `*`
    //    repeated) must fail fast on the 256 MiB retained-byte budget rather
    //    than retaining unbounded heap state across all 100 lines.
    let wide_fixture = TempFixture::new("ignore-wide-sweep");
    let mut wide_rules = Vec::with_capacity(100 * 40_003);
    let wide_line = format!("*{}*\n", "a".repeat(40_000));
    for _ in 0..100 {
        wide_rules.extend_from_slice(wide_line.as_bytes());
    }
    fs::write(wide_fixture.root.join(".gitignore"), wide_rules).expect("write wide-sweep ignore");
    fs::write(wide_fixture.root.join("visible.txt"), b"x").expect("write visible.txt");

    let mut wide_result = None;
    let wide_allocs = allocation_counter::measure(|| {
        wide_result = Some(
            Walker::new(&wide_fixture.root)
                .respect_git_ignore(true)
                .threads(1)
                .error_policy(ErrorPolicy::Collect)
                .collect()
                .expect("collect policy keeps walking"),
        );
    });
    let wide_result = wide_result.expect("walk produced result");
    assert_eq!(wide_result.errors().len(), 1);
    assert_eq!(wide_result.errors()[0].operation().as_str(), "read_ignore");
    assert_eq!(
        wide_result.errors()[0].io_kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        wide_allocs.bytes_max < 256 * 1024 * 1024,
        "rejecting 100 x 40 KiB multi-star ignore lines peaked at {} bytes",
        wide_allocs.bytes_max
    );

    // 5. Pathological multi-star component line (`/*a*/` repeated 82,000 times
    //    = 246,000 units, strictly below the 250,000-unit cap) must trip the
    //    256 MiB per-file retained-byte safety limit FIRST in the zero-alloc
    //    pre-scan and report `read_ignore` with bounded peak memory.
    let bytes_fixture = TempFixture::new("ignore-wide-bytes");
    fs::write(
        bytes_fixture.root.join(".gitignore"),
        format!("/{}\n", "*a*/".repeat(82_000)),
    )
    .expect("write wide-bytes ignore");
    fs::write(bytes_fixture.root.join("visible.txt"), b"x").expect("write visible.txt");

    let mut bytes_result = None;
    let bytes_allocs = allocation_counter::measure(|| {
        bytes_result = Some(
            Walker::new(&bytes_fixture.root)
                .respect_git_ignore(true)
                .threads(1)
                .error_policy(ErrorPolicy::Collect)
                .collect()
                .expect("collect policy keeps walking"),
        );
    });
    let bytes_result = bytes_result.expect("walk produced result");
    assert_eq!(bytes_result.errors().len(), 1);
    let bytes_error = &bytes_result.errors()[0];
    assert_eq!(bytes_error.operation().as_str(), "read_ignore");
    assert_eq!(bytes_error.io_kind(), io::ErrorKind::InvalidData);
    let bytes_source = std::error::Error::source(bytes_error)
        .expect("WalkError exposes underlying io::Error")
        .to_string();
    assert!(
        bytes_source.contains("256 MiB"),
        "expected per-file retained-byte limit error, got: {bytes_source}"
    );
    assert!(
        bytes_allocs.bytes_max < 16 * 1024 * 1024,
        "rejecting 82,000 multi-star components peaked at {} bytes",
        bytes_allocs.bytes_max
    );

    // 6. A single line of 22 valid 100 KiB `*a...a*` components (~2.2 MiB
    //    file, > 256 MiB estimated retained storage, every component >= 64 KiB
    //    and within `ferralk-glob`'s `IrBudget`) must reject via the
    //    zero-materialization IR check without compiling any `Pattern`
    //    (staying far below the ~70+ MiB that `Pattern::validate` would
    //    temporarily allocate).
    let large_valid_fixture = TempFixture::new("ignore-large-valid-nomat");
    let big_part = format!("*{}*/", "a".repeat(100_000));
    fs::write(
        large_valid_fixture.root.join(".gitignore"),
        format!("/{}\n", big_part.repeat(22)),
    )
    .expect("write large valid multi-star ignore");
    fs::write(large_valid_fixture.root.join("visible.txt"), b"x").expect("write visible.txt");

    let mut large_valid_result = None;
    let large_valid_allocs = allocation_counter::measure(|| {
        large_valid_result = Some(
            Walker::new(&large_valid_fixture.root)
                .respect_git_ignore(true)
                .threads(1)
                .error_policy(ErrorPolicy::Collect)
                .collect()
                .expect("collect policy keeps walking"),
        );
    });
    let large_valid_result = large_valid_result.expect("walk produced result");
    assert_eq!(large_valid_result.errors().len(), 1);
    assert_eq!(
        large_valid_result.errors()[0].operation().as_str(),
        "read_ignore"
    );
    assert_eq!(
        large_valid_result.errors()[0].io_kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        large_valid_allocs.count_total < 4_096,
        "rejecting 22 x 100 KiB valid components made {} allocations",
        large_valid_allocs.count_total
    );
    assert!(
        large_valid_allocs.bytes_max < 16 * 1024 * 1024,
        "rejecting 22 x 100 KiB valid components peaked at {} bytes",
        large_valid_allocs.bytes_max
    );

    // 7. A ~2 MiB `.gitignore` with `keep.log` + 256 rules of `[` + 8,192 `*`
    //    + `]` must deduplicate repeated standalone class literals, fit well
    //    within the 256 MiB budget without rolling back, and still ignore
    //    `keep.log`.
    let dup_stars_fixture = TempFixture::new("ignore-dup-class-stars");
    let star_rule = format!("[{}]\n", "*".repeat(8_192));
    let mut dup_stars_ignore = String::with_capacity(16 + 256 * star_rule.len());
    dup_stars_ignore.push_str("keep.log\n");
    for _ in 0..256 {
        dup_stars_ignore.push_str(&star_rule);
    }
    fs::write(dup_stars_fixture.root.join(".gitignore"), dup_stars_ignore)
        .expect("write dup-stars ignore");
    fs::write(dup_stars_fixture.root.join("keep.log"), b"x").expect("write keep.log");
    fs::write(dup_stars_fixture.root.join("visible.txt"), b"x").expect("write visible.txt");

    let dup_stars_result = Walker::new(&dup_stars_fixture.root)
        .respect_git_ignore(true)
        .threads(1)
        .error_policy(ErrorPolicy::Collect)
        .collect()
        .expect("walk with deduplicated star-class rules succeeds");
    assert!(dup_stars_result.errors().is_empty());
    assert!(
        dup_stars_result
            .entries()
            .iter()
            .any(|entry| entry.path().ends_with("visible.txt"))
    );
    assert!(
        !dup_stars_result
            .entries()
            .iter()
            .any(|entry| entry.path().ends_with("keep.log")),
        "keep.log must remain ignored when 256 repeated-star bracket classes are deduplicated"
    );

    // 8. A `.gitignore` with `keep.log` + `[` + 1,100,000 `a-z` + `]` +
    //    `[` + 500,000 `[:digit:]` + `]` must deduplicate repeated ranges and
    //    POSIX classes in both `component_cost` and `normalize_bracket_class`,
    //    fit comfortably within the 256 MiB budget with zero errors, and keep
    //    `keep.log` ignored.
    let dup_ranges_fixture = TempFixture::new("ignore-dup-class-ranges");
    let mut dup_ranges_ignore = String::with_capacity(16 + 3_300_003 + 4_500_003);
    dup_ranges_ignore.push_str("keep.log\n[");
    for _ in 0..1_100_000 {
        dup_ranges_ignore.push_str("a-z");
    }
    dup_ranges_ignore.push_str("]\n[");
    for _ in 0..500_000 {
        dup_ranges_ignore.push_str("[:digit:]");
    }
    dup_ranges_ignore.push_str("]\n");
    fs::write(
        dup_ranges_fixture.root.join(".gitignore"),
        dup_ranges_ignore,
    )
    .expect("write dup-ranges ignore");
    fs::write(dup_ranges_fixture.root.join("keep.log"), b"x").expect("write keep.log");
    fs::write(dup_ranges_fixture.root.join("m"), b"x").expect("write m");
    fs::write(dup_ranges_fixture.root.join("5"), b"x").expect("write 5");
    fs::write(dup_ranges_fixture.root.join("visible.txt"), b"x").expect("write visible.txt");

    let dup_ranges_result = Walker::new(&dup_ranges_fixture.root)
        .respect_git_ignore(true)
        .threads(1)
        .error_policy(ErrorPolicy::Collect)
        .collect()
        .expect("walk with deduplicated range/POSIX-class rules succeeds");
    assert!(dup_ranges_result.errors().is_empty());
    assert!(
        dup_ranges_result
            .entries()
            .iter()
            .any(|entry| entry.path().ends_with("visible.txt"))
    );
    for ignored in ["keep.log", "m", "5"] {
        assert!(
            !dup_ranges_result
                .entries()
                .iter()
                .any(|entry| entry.path().ends_with(ignored)),
            "{ignored} must remain ignored when repeated ranges/POSIX classes are deduplicated"
        );
    }
}

#[cfg(feature = "unstable-test-hooks")]
#[test]
fn ignore_retained_byte_estimates_upper_bound_measured_rule_set_storage() {
    // Warm up one-off thread/allocator state before measuring.
    let _ = retained(|| ferralk::build_ignore_rule_set_for_memory_test(b"warm.txt"));

    let wide_10k = format!("*{}*", "a".repeat(10_000));
    let qmarks_1025 = "?".repeat(1_025);
    let qmarks_2049 = "?".repeat(2_049);
    let classes_513 = "[a]".repeat(513);
    let classes_1025 = "[a]".repeat(1_025);
    let astar_513 = "a*".repeat(513);
    let posix_alpha_513 = format!("[[:alpha:]]{}", "?".repeat(513));
    let posix_alpha_1025 = format!("[[:alpha:]]{}", "?".repeat(1_025));
    let range_posix_slash_513 = format!("[a-[:a/b:]]{}", "?".repeat(513));
    let range_bracket_posix_513 = format!("[a-b[:digit:]]{}", "?".repeat(513));
    let range_consumed_bracket_stars_10k = format!("[a-[:digit:][]{}]", "*".repeat(10_000));
    let bracket_stars_8k = format!("[{}]", "*".repeat(8_192));
    let bracket_star_ranges_10k = format!("[{}]", "*-*".repeat(10_000));
    let bracket_az_ranges_1m = format!("[{}]", "a-z".repeat(1_100_000));
    let bracket_posix_digits_500k = format!("[{}]", "[:digit:]".repeat(500_000));
    let lit_1025 = "a".repeat(1_025);
    let lit_10k = "a".repeat(10_000);
    let multi_5 = "a\n".repeat(5);
    let multi_1025 = "a\n".repeat(1_025);
    let multi_glob_513 = "*.log\n".repeat(513);

    let cases: [(&str, &str); 31] = [
        ("a", "a"),
        ("**", "**"),
        ("**/a", "**/a"),
        ("a/**", "a/**"),
        ("*.log", "*.log"),
        ("a?", "a?"),
        ("?*", "?*"),
        ("[a-z]", "[a-z]"),
        ("[a-z]*", "[a-z]*"),
        ("a*b*c", "a*b*c"),
        ("a/b/c", "a/b/c"),
        ("*<10KB>*", wide_10k.as_str()),
        ("?*1025", qmarks_1025.as_str()),
        ("?*2049", qmarks_2049.as_str()),
        ("[a]*513", classes_513.as_str()),
        ("[a]*1025", classes_1025.as_str()),
        ("a**513", astar_513.as_str()),
        ("[[:alpha:]]+?*513", posix_alpha_513.as_str()),
        ("[[:alpha:]]+?*1025", posix_alpha_1025.as_str()),
        ("[a-[:a/b:]]+?*513", range_posix_slash_513.as_str()),
        ("[a-b[:digit:]]+?*513", range_bracket_posix_513.as_str()),
        (
            "[a-[:digit:][]*10k]",
            range_consumed_bracket_stars_10k.as_str(),
        ),
        ("[*8192]", bracket_stars_8k.as_str()),
        ("[*-**10k]", bracket_star_ranges_10k.as_str()),
        ("[a-z*1.1m]", bracket_az_ranges_1m.as_str()),
        ("[[:digit:]*500k]", bracket_posix_digits_500k.as_str()),
        ("lit_1025", lit_1025.as_str()),
        ("lit_10k", lit_10k.as_str()),
        ("multi_a_5", multi_5.as_str()),
        ("multi_a_1025", multi_1025.as_str()),
        ("multi_glob_513", multi_glob_513.as_str()),
    ];

    for (label, rule) in cases {
        let mut kept = None;
        let info = allocation_counter::measure(|| {
            kept = Some(ferralk::build_ignore_rule_set_for_memory_test(
                rule.as_bytes(),
            ));
        });
        let rules = kept.expect("rule build produced result");
        let estimated = rules.estimated_retained_bytes();
        let measured =
            usize::try_from(info.bytes_current).expect("retained bytes are non-negative");
        drop(black_box(rules));
        assert!(
            estimated >= measured,
            "rule {label} production estimate ({estimated}) fell below measured RuleSet retained storage ({measured})"
        );
    }
}
