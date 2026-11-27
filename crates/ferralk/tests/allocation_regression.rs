//! Allocation-count regression coverage for the matcher and walker hot paths.
//!
//! This is deliberately one test in its own integration-test process. The
//! replacement allocator is process-global while measurements are thread-local;
//! keeping the windows serial and the walker on one thread makes their scope
//! explicit and repeatable.

use std::{
    fs,
    hint::black_box,
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{
    Verdict, Walker,
    ferralk_glob::{Pattern, PatternOptions, PatternSet},
};

const ENTRIES_PER_DIRECTORY: usize = 64;
/// An ordinary directory width, wider than the 64-entry batch the parallel
/// walk once split listings into. A directory this size must be classified in
/// one piece: split, its name buffers traveled into the continuation and a
/// sibling started over with none.
const WIDE_ENTRIES_PER_DIRECTORY: usize = 100;
const CONSTANT_GROWTH_BUDGET: u64 = 16;
static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn plain(directories: usize) -> Self {
        Self::new(directories, ENTRIES_PER_DIRECTORY, false)
    }

    fn ignored(directories: usize) -> Self {
        Self::new(directories, ENTRIES_PER_DIRECTORY, true)
    }

    fn wide(directories: usize) -> Self {
        Self::new(directories, WIDE_ENTRIES_PER_DIRECTORY, false)
    }

    fn adapted(directories: usize) -> Self {
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "ferralk-allocations-adapted-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after epoch")
                    .as_nanos(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create adapted allocation fixture: {error}"),
            }
        };
        // Lowercase ASCII gated rule matches uppercase filenames via
        // `scratch.folded`, and NFC composed gated rule matches NFD decomposed
        // filenames on macOS via `scratch.precomposed`. Using `?` ensures
        // `memmem::Finder` gates stay active under `core.ignoreCase = true`.
        let rules = if cfg!(target_os = "macos") {
            "ignored-???.tmp\ncaf\u{00e9}?-???.tmp\n"
        } else {
            "ignored-???.tmp\ncaf\u{00e9}??-???.tmp\n"
        };
        fs::write(root.join(".gitignore"), rules)
            .expect("write adapted allocation fixture ignore rules");
        for directory in 0..directories {
            let directory = root.join(format!("batch-{directory}"));
            fs::create_dir(&directory).expect("create adapted allocation fixture batch");
            for index in 0..ENTRIES_PER_DIRECTORY {
                let even_name = format!("IGNORED-{index:03}.TMP");
                let odd_name = if cfg!(target_os = "macos") {
                    // Both NFD (`e\u{0301}`) and uppercase (`C`, `.TMP`),
                    // padded to 15 encoded bytes to match `even_name` so
                    // arbitrary directory enumeration order never changes
                    // `Listing::push` slot growth.
                    format!("Cafe\u{0301}x-{index:03}.TMP")
                } else {
                    // ASCII uppercase (`CAF`, `.TMP`) with NFC `é`, padded to
                    // 15 encoded bytes to match `even_name`.
                    format!("CAF\u{00e9}xx-{index:03}.TMP")
                };
                assert_eq!(
                    std::ffi::OsStr::new(&even_name).as_encoded_bytes().len(),
                    std::ffi::OsStr::new(&odd_name).as_encoded_bytes().len(),
                    "adapted fixture filename shapes must have identical encoded byte lengths"
                );
                let name = if index % 2 == 0 { even_name } else { odd_name };
                fs::write(directory.join(name), b"fixture")
                    .expect("write adapted allocation fixture entry");
            }
        }
        Self { root }
    }

    fn new(directories: usize, entries_per_directory: usize, ignored: bool) -> Self {
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "ferralk-allocations-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after epoch")
                    .as_nanos(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create allocation fixture: {error}"),
            }
        };
        if ignored {
            fs::write(root.join(".gitignore"), b"ignored-*.tmp\n")
                .expect("write allocation fixture ignore rules");
        }
        for directory in 0..directories {
            let directory = root.join(format!("batch-{directory}"));
            fs::create_dir(&directory).expect("create allocation fixture batch");
            for index in 0..entries_per_directory {
                let name = if ignored {
                    format!("ignored-{index:03}.tmp")
                } else {
                    format!("entry-{index:03}.dat")
                };
                fs::write(directory.join(name), b"fixture")
                    .expect("write allocation fixture entry");
            }
        }
        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn count_skipping_walk(root: &Path, gitignore: bool) -> u64 {
    count_skipping_walk_with(root, 1, ENTRIES_PER_DIRECTORY, gitignore)
}

/// Counts the allocations of one walk on this thread. With more than one
/// thread configured the walk takes the parallel route, but a fixture below
/// the helper floor is drained by the caller alone, so what this thread
/// counts is the whole walk, and it is repeatable.
fn count_skipping_walk_with(
    root: &Path,
    threads: usize,
    entries_per_directory: usize,
    gitignore: bool,
) -> u64 {
    let walker = Walker::new(root)
        .threads(threads)
        .respect_git_ignore(gitignore);
    let visited = AtomicUsize::new(0);
    let mut outcome = None;
    let allocations = allocation_counter::measure(|| {
        outcome = Some(
            walker
                .visit(|_| {
                    visited.fetch_add(1, Ordering::Relaxed);
                    Verdict::Skip
                })
                .expect("allocation fixture walk succeeds"),
        );
    });
    let outcome = outcome.expect("measurement produced a walk result");
    assert!(outcome.entries().is_empty());
    let _ = black_box(outcome);
    let visited = visited.load(Ordering::Relaxed);
    if gitignore {
        assert!(
            visited < entries_per_directory,
            "ignore rules must filter the candidate files"
        );
    } else {
        assert!(
            visited >= entries_per_directory,
            "plain walks must reach the candidate files"
        );
    }
    allocations.count_total
}

fn assert_walk_growth(label: &str, one_batch: u64, two_batches: u64) {
    assert_walk_growth_per_entry(label, ENTRIES_PER_DIRECTORY, one_batch, two_batches);
}

fn assert_walk_growth_per_entry(
    label: &str,
    entries_per_directory: usize,
    one_batch: u64,
    two_batches: u64,
) {
    // Native readers reuse their name buffers. For the portable backend,
    // measure std::fs's per-entry cost separately: the standard reader may
    // change its own name storage between platforms or Rust releases. This
    // leaves the walker's additional allocations under the same fixed budget.
    let backend_floor = if cfg!(any(
        all(feature = "native-linux", target_os = "linux"),
        all(feature = "native-macos", target_os = "macos")
    )) {
        0
    } else {
        portable_reader_floor(entries_per_directory)
    };
    assert_walk_growth_with_floor(
        label,
        entries_per_directory,
        one_batch,
        two_batches,
        backend_floor,
    );
}

fn assert_walk_growth_with_floor(
    label: &str,
    entries_per_directory: usize,
    one_batch: u64,
    two_batches: u64,
    backend_floor: u64,
) {
    let growth = two_batches.checked_sub(one_batch).unwrap_or_else(|| {
        panic!("{label}: the larger fixture allocated less ({two_batches} < {one_batch})")
    });
    let allowed = backend_floor + CONSTANT_GROWTH_BUDGET;
    assert!(
        growth <= allowed,
        "{label}: a second {entries_per_directory}-entry sibling grew allocations by {growth}; \
         backend floor plus constant budget is {allowed} ({one_batch} -> {two_batches})"
    );
}

fn portable_reader_floor(entries_per_directory: usize) -> u64 {
    let populated = Fixture::new(1, entries_per_directory, false);
    portable_reader_floor_for_directory(&populated.root.join("batch-0"))
}

fn portable_reader_floor_for_directory(directory: &Path) -> u64 {
    let empty = Fixture::new(1, 0, false);
    let count = |dir: &Path| {
        allocation_counter::measure(|| {
            for entry in fs::read_dir(dir).expect("read control directory") {
                black_box(entry.expect("read control entry").file_name());
            }
        })
        .count_total
    };
    count(directory)
        .checked_sub(count(&empty.root.join("batch-0")))
        .expect("the populated standard reader cannot allocate less than the empty reader")
}

#[test]
fn hot_paths_keep_their_allocation_floors() {
    let pattern = Pattern::compile(
        "**/*.rs",
        PatternOptions::default().recursive_double_star(true),
    )
    .expect("compile allocation fixture pattern");
    assert!(pattern.is_match_glob_path("src/deep/module.rs"));
    assert!(!pattern.is_match_glob_path("src/deep/module.ts"));
    let matcher = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(pattern.is_match_glob_path(black_box("src/deep/module.rs")));
            black_box(pattern.is_match_glob_path(black_box("src/deep/module.ts")));
        }
    });
    assert_eq!(
        matcher.count_total, 0,
        "warm compiled matches must not allocate"
    );

    let nested_scoped = Pattern::compile(r"{a,a\/.b}/**/*.{rs,ts}", PatternOptions::walker())
        .expect("compile nested scoped suffix set pattern");
    assert!(nested_scoped.is_match_glob_path("a/.b/x.ts"));
    assert!(!nested_scoped.is_match_glob_path("a/.b/x.md"));
    let nested_matcher = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(nested_scoped.is_match_glob_path(black_box("a/.b/x.ts")));
            black_box(nested_scoped.is_match_glob_path(black_box("a/.b/x.md")));
        }
    });
    assert_eq!(
        nested_matcher.count_total, 0,
        "warm nested-prefix ScopedSuffixSet matches must not allocate"
    );

    let deep_prefixes: Vec<String> = (1..=24)
        .map(|depth| {
            if depth == 1 {
                "a".to_string()
            } else {
                let mut parts = vec!["a", ".b"];
                parts.extend(std::iter::repeat_n("a", depth - 2));
                parts.join(r"\/")
            }
        })
        .collect();
    let deep_scoped = Pattern::compile(
        format!("{{{}}}/**/*.{{rs,ts}}", deep_prefixes.join(",")),
        PatternOptions::walker(),
    )
    .expect("compile deep nested scoped suffix set pattern");
    let mut deep_parts = vec!["a", ".b"];
    deep_parts.extend(std::iter::repeat_n("a", 22));
    let deep_accept = format!("{}/x.ts", deep_parts.join("/"));
    let deep_reject = format!("{}/x.md", deep_parts.join("/"));
    assert!(deep_scoped.is_match_glob_path(&deep_accept));
    assert!(!deep_scoped.is_match_glob_path(&deep_reject));
    let deep_matcher = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(deep_scoped.is_match_glob_path(black_box(deep_accept.as_str())));
            black_box(deep_scoped.is_match_glob_path(black_box(deep_reject.as_str())));
        }
    });
    assert_eq!(
        deep_matcher.count_total, 0,
        "warm deep nested-prefix ScopedSuffixSet spill matches must not allocate"
    );

    let narrow_rep = Pattern::compile("+(a/**)/z*", PatternOptions::walker())
        .expect("compile narrow repeated extglob pattern");
    let wide_rep_src = format!("+({}/**|c)/z*", "a".repeat(80));
    let wide_rep = Pattern::compile(&wide_rep_src, PatternOptions::walker())
        .expect("compile wide repeated extglob pattern");
    let wide_rep_accept = format!("{}/sub/z1", "a".repeat(80));
    let wide_rep_reject = format!("b{}", "x".repeat(120));
    assert!(narrow_rep.is_match("a/sub/z1"));
    assert!(!narrow_rep.is_match(&wide_rep_reject));
    assert!(wide_rep.is_match(&wide_rep_accept));
    assert!(!wide_rep.is_match(&wide_rep_reject));
    let narrow_rep_matcher = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(narrow_rep.is_match(black_box(wide_rep_reject.as_str())));
        }
    });
    // Warm the wide sweep slot once after the narrow baseline, then verify
    // steady-state wide matches allocate no more than the narrow baseline
    // (zero per-call `SweepState::Wide` allocations).
    assert!(!wide_rep.is_match(&wide_rep_reject));
    let wide_rep_matcher = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(wide_rep.is_match(black_box(wide_rep_reject.as_str())));
        }
    });
    assert_eq!(
        wide_rep_matcher.count_total, narrow_rep_matcher.count_total,
        "warm wide repeated extglob nonmatching calls must reuse SweepState buffers without extra allocations"
    );

    // Accepting `+(a/**)/z*` on `"a/z"` must not queue a dead `group.rest`
    // separator continuation and therefore allocates no more than `@(a/**)/z*`.
    let single_through =
        Pattern::compile("@(a/**)/z*", PatternOptions::walker()).expect("compile single through");
    assert!(single_through.is_match("a/z"));
    assert!(narrow_rep.is_match("a/z"));
    let single_through_allocs = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(single_through.is_match(black_box("a/z")));
        }
    });
    let rep_through_allocs = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(narrow_rep.is_match(black_box("a/z")));
        }
    });
    assert_eq!(
        rep_through_allocs.count_total, single_through_allocs.count_total,
        "accepting `+(a/**)/z*` must not allocate an extra memo row for `group.rest`"
    );

    // 65 narrow repetition arms must stay in retained `narrow_sweeps` without
    // growing and shrinking on every call.
    let sixty_five_arms = (0..65)
        .map(|i| format!("a{i}*"))
        .collect::<Vec<_>>()
        .join("|");
    let wide_narrow_rep =
        Pattern::compile(format!("+({sixty_five_arms})y*"), PatternOptions::walker())
            .expect("compile 65-arm narrow repetition");
    assert!(!wide_narrow_rep.is_match("zzz"));
    let sixty_five_allocs = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(wide_narrow_rep.is_match(black_box("zzz")));
        }
    });
    assert_eq!(
        sixty_five_allocs.count_total, narrow_rep_matcher.count_total,
        "65 narrow repetition arms must reuse `narrow_sweeps` without per-call reallocations"
    );

    // Mixed wide + narrow repetition groups must keep `wide_sweeps` separate
    // from `narrow_sweeps` so the second group never evicts the first group's
    // retained wide buffers. Using `|c` keeps the first group non-idempotent so
    // it actually populates `wide_sweeps` in `matching_extglob_repetition_ends`.
    let mixed_narrow = Pattern::compile("+(a/**|c)/+(b*)z*", PatternOptions::walker())
        .expect("compile narrow mixed repetition");
    let mixed_wide = Pattern::compile(
        format!("+({}/**|c)/+(b*)z*", "a".repeat(80)),
        PatternOptions::walker(),
    )
    .expect("compile wide+narrow mixed repetition");
    let mixed_wide_path = format!("{}/bz", "a".repeat(80));
    assert!(mixed_narrow.is_match("a/bz"));
    assert!(mixed_wide.is_match(&mixed_wide_path));
    let mixed_narrow_allocs = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(mixed_narrow.is_match(black_box("a/bz")));
        }
    });
    assert!(mixed_wide.is_match(&mixed_wide_path));
    let mixed_wide_allocs = allocation_counter::measure(|| {
        for _ in 0..1_000 {
            black_box(mixed_wide.is_match(black_box(mixed_wide_path.as_str())));
        }
    });
    assert_eq!(
        mixed_wide_allocs.count_total, mixed_narrow_allocs.count_total,
        "mixed wide + narrow repetition groups must not evict retained wide sweep buffers"
    );

    // Warm `PatternSet` queries must not allocate on repeated components
    // (inline or spilled dedup via component `a`) or >256-byte uppercase folded
    // candidates (#493).
    let mut set_globs: Vec<String> = (0..16).map(|index| format!("**/a/tag{index}_*")).collect();
    set_globs.push("**/a/any_*".to_string());
    set_globs.push("docs_*".to_string());
    set_globs.push("**/*_tail".to_string());
    let folded_set = PatternSet::new(&set_globs, PatternOptions::walker().case_insensitive(true))
        .expect("compile PatternSet allocation fixture");
    let repeated_path = format!("{}miss", "a/".repeat(200));
    let long_upper_hit = format!("DOCS_{}/A/TAG9_FILE", "X".repeat(400));
    let long_upper_miss = format!("OTHER_{}/MISS", "Y".repeat(400));
    let mut set_matches = Vec::with_capacity(32);
    // Warm up thread-local seen/fold buffers once.
    assert!(!folded_set.is_match_glob_path(&repeated_path));
    folded_set.matches_glob_path_into(&repeated_path, &mut set_matches);
    assert!(set_matches.is_empty());
    assert!(folded_set.is_match_glob_path(&long_upper_hit));
    folded_set.matches_glob_path_into(&long_upper_hit, &mut set_matches);
    assert!(!set_matches.is_empty());
    assert!(!folded_set.is_match_glob_path(&long_upper_miss));
    let pattern_set_allocs = allocation_counter::measure(|| {
        for _ in 0..500 {
            black_box(folded_set.is_match_glob_path(black_box(repeated_path.as_str())));
            folded_set.matches_glob_path_into(black_box(repeated_path.as_str()), &mut set_matches);
            black_box(set_matches.len());
            black_box(folded_set.is_match_glob_path(black_box(long_upper_hit.as_str())));
            folded_set.matches_glob_path_into(black_box(long_upper_hit.as_str()), &mut set_matches);
            black_box(set_matches.len());
            black_box(folded_set.is_match_glob_path(black_box(long_upper_miss.as_str())));
        }
    });
    assert_eq!(
        pattern_set_allocs.count_total, 0,
        "warm PatternSet repeated-component and >256-byte uppercase folded matches must not allocate"
    );

    let small = Fixture::plain(1);
    let large = Fixture::plain(2);
    let plain_small = count_skipping_walk(&small.root, false);
    let plain_large = count_skipping_walk(&large.root, false);

    let ignored_small = Fixture::ignored(1);
    let ignored_large = Fixture::ignored(2);
    let gitignore_small = count_skipping_walk(&ignored_small.root, true);
    let gitignore_large = count_skipping_walk(&ignored_large.root, true);

    assert_walk_growth("plain serial walk", plain_small, plain_large);
    assert_walk_growth(
        "gitignore-enabled serial walk",
        gitignore_small,
        gitignore_large,
    );

    // Dot-spelled root (`/tmp/.../.`) exercises `CandidateRoot::spell_into`
    // without allocating per entry, and `git_ignore_case(true)` +
    // `git_precompose_unicode(true)` exercises the macOS/Windows default
    // ignore hot path without per-entry folding or NFC allocations.
    let dot_small = count_skipping_walk(&ignored_small.root.join("."), true);
    let dot_large = count_skipping_walk(&ignored_large.root.join("."), true);
    assert_walk_growth(
        "gitignore-enabled dot-spelled serial walk",
        dot_small,
        dot_large,
    );

    let count_adapted_walk = |root: &Path| {
        let walker = Walker::new(root)
            .threads(1)
            .respect_git_ignore(true)
            .git_ignore_case(true)
            .git_precompose_unicode(true);
        let visited = AtomicUsize::new(0);
        let mut outcome = None;
        let allocations = allocation_counter::measure(|| {
            outcome = Some(
                walker
                    .visit(|_| {
                        visited.fetch_add(1, Ordering::Relaxed);
                        Verdict::Skip
                    })
                    .expect("adapted gitignore walk succeeds"),
            );
        });
        let outcome = outcome.expect("measurement produced a walk result");
        assert!(outcome.entries().is_empty());
        let _ = black_box(outcome);
        assert!(
            visited.load(Ordering::Relaxed) < ENTRIES_PER_DIRECTORY,
            "case-insensitive ignore rules must filter the candidate files"
        );
        allocations.count_total
    };
    let adapted_small = count_adapted_walk(&ignored_small.root);
    let adapted_large = count_adapted_walk(&ignored_large.root);
    assert_walk_growth(
        "gitignore-enabled case-insensitive precomposed ASCII fast-path serial walk",
        adapted_small,
        adapted_large,
    );

    // Exercise both the ASCII case-folding buffer (`scratch.folded`) and, on
    // macOS, the NFC precomposition buffer (`scratch.precomposed`) end-to-end.
    let folded_nfd_small = Fixture::adapted(1);
    let folded_nfd_large = Fixture::adapted(2);
    // Sanity check: without adaptations enabled, none of the uppercase/NFD
    // entries match the lowercase/NFC rules.
    let unadapted_entries = Walker::new(&folded_nfd_small.root)
        .threads(1)
        .respect_git_ignore(true)
        .git_ignore_case(false)
        .git_precompose_unicode(false)
        .collect()
        .expect("unadapted walk succeeds")
        .entries()
        .len();
    assert!(
        unadapted_entries >= ENTRIES_PER_DIRECTORY,
        "unadapted fixture entries must require ignoreCase/precomposeUnicode to be filtered"
    );
    let folded_nfd_one = count_adapted_walk(&folded_nfd_small.root);
    let folded_nfd_two = count_adapted_walk(&folded_nfd_large.root);
    // Measure the portable reader floor on the adapted fixture's own filename
    // mix (on Windows, UTF-16 -> WTF-8 conversion of non-ASCII names like `é`
    // allocates extra inside `DirEntry::file_name()`).
    let adapted_backend_floor = if cfg!(any(
        all(feature = "native-linux", target_os = "linux"),
        all(feature = "native-macos", target_os = "macos")
    )) {
        0
    } else {
        portable_reader_floor_for_directory(&folded_nfd_large.root.join("batch-1"))
    };
    assert_walk_growth_with_floor(
        "gitignore-enabled uppercase/NFD adapted serial walk",
        ENTRIES_PER_DIRECTORY,
        folded_nfd_one,
        folded_nfd_two,
        adapted_backend_floor,
    );

    // On the parallel route a directory of ordinary width is classified in
    // one piece, so the second sibling reads into the name buffers the first
    // one left behind. A listing batch smaller than the directory would hand
    // those buffers off with the continuation and start the sibling from
    // nothing.
    let wide_small = Fixture::wide(1);
    let wide_large = Fixture::wide(2);
    let wide_one = count_skipping_walk_with(&wide_small.root, 4, WIDE_ENTRIES_PER_DIRECTORY, false);
    let wide_two = count_skipping_walk_with(&wide_large.root, 4, WIDE_ENTRIES_PER_DIRECTORY, false);
    assert_walk_growth_per_entry(
        "parallel walk over wide siblings",
        WIDE_ENTRIES_PER_DIRECTORY,
        wide_one,
        wide_two,
    );
}
