#![no_main]
//! Differential target: ferralk and Oxc fast-glob over their shared syntax.
//!
//! ADR-0007 designates fast-glob as the second reference for the syntax both
//! engines document the same way. This target generates a pattern and a
//! candidate, keeps only inputs inside that shared subset, and asserts equal
//! verdicts. Every documented divergence is excluded structurally, by the
//! shape of the pattern, so a failure is always a new finding rather than a
//! rediscovery of a known difference. The divergences and the exclusion that
//! covers each are listed in `docs/fast-glob-reference.md`.
//!
//! A disagreement is reported as a ready-to-paste corpus line, so a finding
//! goes straight into `corpus/fast-glob.jsonl` after review.

use std::{
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use corpus::{Case, Source, encode_bytes};
use ferralk_fuzz::{in_shared_subset, matcher_options, split_input};
use ferralk_glob::Pattern;
use libfuzzer_sys::{Corpus, fuzz_target};

fuzz_target!(|data: &[u8]| -> Corpus {
    let (pattern, path) = split_input(data);
    let shared = in_shared_subset(pattern, path);
    record_subset_hit(shared);
    if !shared {
        return Corpus::Reject;
    }
    // fast-glob rejects patterns ferralk accepts and the reverse; comparing a
    // verdict either engine declines to produce would compare error models,
    // not matching. `validate` is a parse and stays cheap on every input.
    if fast_glob::validate(pattern).is_err() {
        return Corpus::Reject;
    }
    // Compiling before `glob_match` is what keeps this target fast: fast-glob
    // backtracks over brace alternatives instead of expanding them, and spends
    // 42 s on the ten-group pattern from issue #42. ferralk's expansion budget
    // rejects that pattern here, so only patterns inside the budget — measured
    // at microseconds in fast-glob — ever reach the comparison. Raising
    // `MAX_BRACE_ALTERNATIVES` would need this checked again.
    let Ok(compiled) = Pattern::compile(pattern, matcher_options()) else {
        return Corpus::Reject;
    };

    // fast-glob keeps every ordinary wildcard inside one path component, which
    // is what `is_match_glob_path` does; `is_match` is the fnmatch-style form
    // zlob defines and is deliberately not comparable here.
    let ours = compiled.is_match_glob_path(path);
    let reference = fast_glob::glob_match(pattern, path);
    assert!(
        ours == reference,
        "ferralk and fast-glob disagree; corpus candidate:\n{}",
        corpus_candidate(pattern, path, ours, reference)
    );
    Corpus::Keep
});

const SUBSET_STATS_INTERVAL: u64 = 4_096;
static SUBSET_STATS_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static SUBSET_INPUTS: AtomicU64 = AtomicU64::new(0);
static SUBSET_HITS: AtomicU64 = AtomicU64::new(0);

/// Saves a cheap rolling checkpoint for the workflow to print after fuzzing.
///
/// libFuzzer owns process shutdown and offers no safe target-level finalizer.
/// Updating every 4,096 inputs keeps the reported ratio close to the final
/// count without putting filesystem I/O on the hot path of every execution.
fn record_subset_hit(shared: bool) {
    let Some(path) = SUBSET_STATS_PATH
        .get_or_init(|| std::env::var_os("FERRALK_FUZZ_SUBSET_STATS").map(PathBuf::from))
    else {
        return;
    };

    let inputs = SUBSET_INPUTS.fetch_add(1, Ordering::Relaxed) + 1;
    if shared {
        SUBSET_HITS.fetch_add(1, Ordering::Relaxed);
    }
    if inputs != 1 && !inputs.is_multiple_of(SUBSET_STATS_INTERVAL) {
        return;
    }

    let hits = SUBSET_HITS.load(Ordering::Relaxed);
    let rate = 100.0 * hits as f64 / inputs as f64;
    let summary = format!(
        "ferralk_vs_fast_glob shared-subset checkpoint: {hits}/{inputs} inputs ({rate:.2}%); at most {} later executions are omitted\n",
        SUBSET_STATS_INTERVAL - 1
    );
    let _ = std::fs::write(path, summary);
}

/// Renders a disagreement as one `corpus/fast-glob.jsonl` line.
fn corpus_candidate(pattern: &[u8], path: &[u8], ours: bool, reference: bool) -> String {
    let case = Case {
        id: format!("fastglob-diff-{:016x}", fingerprint(pattern, path)),
        kind: corpus::CaseKind::MatchGlobPath,
        paths: Vec::new(),
        matches: Vec::new(),
        oracle_matches: None,
        base_path: String::new(),
        rewritten: None,
        windows_paths: false,
        indices: Vec::new(),
        oracle_indices: None,
        pattern: encode_bytes(pattern),
        path: encode_bytes(path),
        flags: vec![
            "braces".to_owned(),
            "recursive_double_star".to_owned(),
            "match_hidden".to_owned(),
        ],
        ignore_rules: Vec::new(),
        nested_ignore_rules: Vec::new(),
        exclude_rules: Vec::new(),
        candidate_is_dir: false,
        candidate_is_symlink: false,
        git_ignorecase: false,
        expected: ours,
        oracle_expected: Some(reference),
        error_offset: None,
        error_message: None,
        platform: None,
        source: Source::FastGlob,
        disputed: true,
        note: Some(
            "Found by the ferralk_vs_fast_glob differential target; review before adopting."
                .to_owned(),
        ),
    };
    serde_json::to_string(&case).expect("a corpus case serializes")
}

/// A stable, dependency-free name for one input pair.
fn fingerprint(pattern: &[u8], path: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in pattern.iter().chain(b"\n").chain(path) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}
