#![forbid(unsafe_code)]

use std::{collections::BTreeMap, fs, path::Path};

use corpus::{Case, CaseKind, Source, decode_bytes, parse_case};
use zlob::{
    ZlobFlags, has_wildcards, zlob_match_paths, zlob_match_paths_at, zlob_match_paths_indices,
    zlob_match_paths_indices_at,
};

/// Exact number of cases this adapter must hand to zlob.
///
/// Any corpus change must update this inventory deliberately, so additions
/// cannot bypass the oracle through a broadened skip condition.
const EXPECTED_REPLAYED: usize = 534;
const EXPECTED_SKIPPED: usize = 268;

/// Cases the zlob 1.6.3 Rust API cannot express, counted by reason.
#[derive(Default)]
struct Skipped {
    /// zlob 1.6.3 has no case-folding flag.
    case_folding: usize,
    /// zlob's Rust API takes `&str`, so raw non-UTF-8 bytes cannot be passed.
    non_utf8: usize,
    /// Rejected patterns are a ferralk contract with its own error taxonomy.
    compile_error: usize,
    /// zlob has no component-local wildcard mode to compare against.
    glob_path: usize,
    /// Rewriting an absolute pattern for a walk root is a walker contract, and
    /// zlob has no walker.
    absolute_pattern: usize,
    /// The verdict describes a separator platform this runner is not.
    platform: usize,
    /// Git ignore cases belong to the Git oracle.
    ignore: usize,
    /// fast-glob-backed cases belong to the Oxc oracle.
    fast_glob: usize,
    /// zlob's Rust FFI cannot safely expose NOCHECK's synthetic empty-list result.
    nocheck_empty_list: usize,
}

impl Skipped {
    fn total(&self) -> usize {
        self.case_folding
            + self.non_utf8
            + self.compile_error
            + self.glob_path
            + self.absolute_pattern
            + self.platform
            + self.ignore
            + self.fast_glob
            + self.nocheck_empty_list
    }
}

#[test]
#[ignore = "requires Zig 0.16 and libclang; run only from the oracle workflow"]
fn checked_in_matcher_cases_agree_with_zlob_1_6_3() {
    let corpus_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    let mut files = Vec::new();
    collect_jsonl(&corpus_root, &mut files).expect("read corpus directories");
    files.sort();

    let mut replayed = 0_usize;
    let mut replayed_by_file = BTreeMap::new();
    let mut skipped = Skipped::default();
    for file in files {
        let mut file_replayed = 0_usize;
        for (line_number, line) in fs::read_to_string(&file)
            .expect("read corpus file")
            .lines()
            .enumerate()
        {
            if line.trim().is_empty() {
                continue;
            }
            let case = parse_case(line)
                .unwrap_or_else(|error| panic!("{}:{}: {error}", file.display(), line_number + 1));
            if case.kind == CaseKind::Ignore {
                skipped.ignore += 1;
                continue;
            }
            if case.source == Source::FastGlob {
                skipped.fast_glob += 1;
                continue;
            }
            if !case.runs_on_host() {
                skipped.platform += 1;
                continue;
            }
            if case.kind == CaseKind::CompileError {
                skipped.compile_error += 1;
                continue;
            }
            if case.kind == CaseKind::MatchGlobPath {
                skipped.glob_path += 1;
                continue;
            }
            if case.kind == CaseKind::AbsolutePattern {
                skipped.absolute_pattern += 1;
                continue;
            }
            let Some(flags) = zlob_flags(&case.flags) else {
                skipped.case_folding += 1;
                continue;
            };
            let Some(text) = CaseText::decode(&case) else {
                skipped.non_utf8 += 1;
                continue;
            };
            let pattern = text.pattern.as_str();
            let path = text.path.as_str();
            if matches!(case.kind, CaseKind::MatchPaths | CaseKind::MatchPathsAt)
                && case.flags.iter().any(|flag| flag == "nocheck")
                && case.paths.is_empty()
            {
                // zlob 1.6.3's Rust FFI aborts on empty input and returns
                // corrupted bytes for this synthetic result. The frozen Zig
                // assertion remains the source evidence, but this Rust oracle
                // must not claim that it replayed the case.
                skipped.nocheck_empty_list += 1;
                continue;
            }
            let actual = match case.kind {
                CaseKind::Matcher => zlob_match_paths(pattern, &[path], flags)
                    .unwrap_or_else(|error| {
                        panic!("{}:{}: {error}", file.display(), line_number + 1)
                    })
                    .is_some(),
                CaseKind::HasWildcards => has_wildcards(pattern, flags),
                CaseKind::MatchPaths | CaseKind::MatchPathsAt => {
                    let paths = text.borrowed_paths();
                    let selected = match case.kind {
                        CaseKind::MatchPaths => zlob_match_paths(pattern, paths.as_slice(), flags),
                        CaseKind::MatchPathsAt => zlob_match_paths_at(
                            text.base_path.as_str(),
                            pattern,
                            paths.as_slice(),
                            flags,
                        ),
                        CaseKind::Matcher
                        | CaseKind::HasWildcards
                        | CaseKind::CompileError
                        | CaseKind::MatchGlobPath
                        | CaseKind::Ignore
                        | CaseKind::AbsolutePattern
                        | CaseKind::MatchPathIndices
                        | CaseKind::MatchPathIndicesAt => unreachable!(),
                    }
                    .expect("zlob match paths")
                    .map(|matches| matches.to_strings())
                    .unwrap_or_default();
                    let mut expected = case
                        .oracle_matches
                        .clone()
                        .unwrap_or_else(|| case.matches.clone());
                    expected.sort();
                    assert_eq!(
                        &selected,
                        &expected,
                        "{}:{}: list result",
                        file.display(),
                        line_number + 1
                    );
                    !selected.is_empty()
                }
                CaseKind::MatchPathIndices | CaseKind::MatchPathIndicesAt => {
                    let paths = text.borrowed_paths();
                    let selected = match case.kind {
                        CaseKind::MatchPathIndices => {
                            zlob_match_paths_indices(pattern, paths.as_slice(), flags)
                        }
                        CaseKind::MatchPathIndicesAt => zlob_match_paths_indices_at(
                            text.base_path.as_str(),
                            pattern,
                            paths.as_slice(),
                            flags,
                        ),
                        CaseKind::Matcher
                        | CaseKind::HasWildcards
                        | CaseKind::CompileError
                        | CaseKind::MatchGlobPath
                        | CaseKind::Ignore
                        | CaseKind::AbsolutePattern
                        | CaseKind::MatchPaths
                        | CaseKind::MatchPathsAt => unreachable!(),
                    }
                    .expect("zlob match path indices")
                    .as_slice()
                    .to_vec();
                    assert_eq!(
                        selected,
                        case.oracle_indices
                            .clone()
                            .unwrap_or_else(|| case.indices.clone()),
                        "{}:{}: index result",
                        file.display(),
                        line_number + 1
                    );
                    !selected.is_empty()
                }
                CaseKind::CompileError
                | CaseKind::MatchGlobPath
                | CaseKind::Ignore
                | CaseKind::AbsolutePattern => {
                    unreachable!("these kinds are skipped above")
                }
            };
            let expected = match case.kind {
                CaseKind::MatchPaths | CaseKind::MatchPathsAt => !case
                    .oracle_matches
                    .as_ref()
                    .unwrap_or(&case.matches)
                    .is_empty(),
                CaseKind::MatchPathIndices | CaseKind::MatchPathIndicesAt => !case
                    .oracle_indices
                    .as_ref()
                    .unwrap_or(&case.indices)
                    .is_empty(),
                _ => case.oracle_expected.unwrap_or(case.expected),
            };
            assert_eq!(
                actual,
                expected,
                "{}:{}: {} against {}",
                file.display(),
                line_number + 1,
                case.pattern,
                case.path
            );
            replayed += 1;
            file_replayed += 1;
        }
        replayed_by_file.insert(
            file.strip_prefix(&corpus_root)
                .expect("corpus file is below the corpus root")
                .display()
                .to_string(),
            file_replayed,
        );
    }

    for (file, count) in &replayed_by_file {
        println!("replayed {count:>3} cases from {file}");
    }
    println!(
        "replayed {replayed} corpus cases against zlob 1.6.3; skipped {} \
         ({} case-folding, {} non-UTF-8, {} compile-error, {} glob-path, \
         {} absolute-pattern, {} other-platform, {} Git-ignore, {} fast-glob, \
         {} NOCHECK-empty-list)",
        skipped.total(),
        skipped.case_folding,
        skipped.non_utf8,
        skipped.compile_error,
        skipped.glob_path,
        skipped.absolute_pattern,
        skipped.platform,
        skipped.ignore,
        skipped.fast_glob,
        skipped.nocheck_empty_list,
    );
    assert_eq!(
        replayed,
        EXPECTED_REPLAYED,
        "the oracle replayed an unexpected number of cases and skipped {}; \
         update the exact inventory only after reviewing the corpus change",
        skipped.total(),
    );
    assert_eq!(
        skipped.total(),
        EXPECTED_SKIPPED,
        "the oracle skipped an unexpected number of cases; update the exact \
         inventory only after reviewing the corpus change",
    );
}

/// A corpus case decoded into the UTF-8 strings the zlob Rust API accepts.
struct CaseText {
    pattern: String,
    path: String,
    base_path: String,
    paths: Vec<String>,
}

impl CaseText {
    /// Returns `None` when any field carries bytes that are not UTF-8.
    ///
    /// zlob's Rust API is `&str`-based, so a byte-matching case from
    /// ADR-0005 has no representation here and belongs to the harness alone.
    fn decode(case: &Case) -> Option<Self> {
        Some(Self {
            pattern: utf8(&case.pattern)?,
            path: utf8(&case.path)?,
            base_path: utf8(&case.base_path)?,
            paths: case
                .paths
                .iter()
                .map(|path| utf8(path))
                .collect::<Option<Vec<_>>>()?,
        })
    }

    fn borrowed_paths(&self) -> Vec<&str> {
        self.paths.iter().map(String::as_str).collect()
    }
}

fn utf8(encoded: &str) -> Option<String> {
    String::from_utf8(decode_bytes(encoded).expect("decode corpus byte codec")).ok()
}

/// Translates corpus flags, or reports that zlob 1.6.3 cannot express one.
fn zlob_flags(names: &[String]) -> Option<ZlobFlags> {
    let mut flags = ZlobFlags::empty();
    for name in names {
        flags |= match name.as_str() {
            "braces" => ZlobFlags::BRACE,
            "recursive_double_star" => ZlobFlags::DOUBLESTAR_RECURSIVE,
            "extglob" => ZlobFlags::EXTGLOB,
            "match_hidden" => ZlobFlags::PERIOD,
            "no_escape" => ZlobFlags::NOESCAPE,
            "nocheck" => ZlobFlags::NOCHECK,
            // zlob 1.6.3 has no case-folding flag; ferralk's ASCII folding is
            // a documented extension the harness replays on its own.
            "case_insensitive" => return None,
            unknown => panic!("unknown matcher flag {unknown:?}"),
        };
    }
    Some(flags)
}

fn collect_jsonl(root: &Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jsonl(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            files.push(path);
        }
    }
    Ok(())
}
