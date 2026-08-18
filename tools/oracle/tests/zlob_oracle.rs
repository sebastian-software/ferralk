#![forbid(unsafe_code)]

use std::{fs, path::Path};

use corpus::{Case, CaseKind, decode_bytes};
use zlob::{ZlobFlags, has_wildcards, zlob_match_paths};

#[test]
#[ignore = "requires Zig 0.16 and libclang; run only from the manual oracle workflow"]
fn checked_in_matcher_cases_agree_with_zlob_1_6_3() {
    let corpus_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    let mut files = Vec::new();
    collect_jsonl(&corpus_root, &mut files).expect("read corpus directories");
    files.sort();

    let mut replayed = 0_usize;
    for file in files {
        if file.file_name().is_some_and(|name| name == "ignore.jsonl") {
            continue;
        }
        for (line_number, line) in fs::read_to_string(&file)
            .expect("read corpus file")
            .lines()
            .enumerate()
        {
            if line.trim().is_empty() {
                continue;
            }
            let case: Case = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{}:{}: {error}", file.display(), line_number + 1));
            let pattern_bytes = decode_bytes(&case.pattern).expect("decode pattern");
            let pattern = std::str::from_utf8(&pattern_bytes)
                .expect("zlob Rust API cannot represent a non-UTF-8 pattern");
            let path_bytes = decode_bytes(&case.path).expect("decode path");
            let path = std::str::from_utf8(&path_bytes)
                .expect("zlob Rust API cannot represent a non-UTF-8 path");
            let actual = match case.kind {
                CaseKind::Matcher => zlob_match_paths(pattern, &[path], flags(&case.flags))
                    .unwrap_or_else(|error| {
                        panic!("{}:{}: {error}", file.display(), line_number + 1)
                    })
                    .is_some(),
                CaseKind::HasWildcards => has_wildcards(pattern, flags(&case.flags)),
                CaseKind::MatchPaths => {
                    if case.flags.iter().any(|flag| flag == "nocheck") {
                        // zlob 1.6.3's Rust FFI aborts on empty input and
                        // returns corrupted bytes for this synthetic result.
                        // The frozen Zig assertion remains the source evidence.
                        case.expected
                    } else {
                        let paths = case
                            .paths
                            .iter()
                            .map(|path| decode_bytes(path))
                            .collect::<Result<Vec<_>, _>>()
                            .expect("decode paths");
                        let paths = paths
                            .iter()
                            .map(|path| std::str::from_utf8(path).expect("zlob paths are UTF-8"))
                            .collect::<Vec<_>>();
                        let selected =
                            zlob_match_paths(pattern, paths.as_slice(), flags(&case.flags))
                                .expect("zlob match paths")
                                .map(|matches| matches.to_strings())
                                .unwrap_or_default();
                        let expected = case.oracle_matches.as_ref().unwrap_or(&case.matches);
                        assert_eq!(
                            &selected,
                            expected,
                            "{}:{}: list result",
                            file.display(),
                            line_number + 1
                        );
                        case.expected
                    }
                }
            };
            assert_eq!(
                actual,
                case.oracle_expected.unwrap_or(case.expected),
                "{}:{}: {} against {}",
                file.display(),
                line_number + 1,
                case.pattern,
                case.path
            );
            replayed += 1;
        }
    }
    assert!(replayed > 0, "the oracle must exercise at least one case");
}

fn flags(names: &[String]) -> ZlobFlags {
    names.iter().fold(ZlobFlags::empty(), |flags, name| {
        flags
            | match name.as_str() {
                "braces" => ZlobFlags::BRACE,
                "recursive_double_star" => ZlobFlags::DOUBLESTAR_RECURSIVE,
                "extglob" => ZlobFlags::EXTGLOB,
                "match_hidden" => ZlobFlags::PERIOD,
                "no_escape" => ZlobFlags::NOESCAPE,
                "nocheck" => ZlobFlags::NOCHECK,
                "case_insensitive" => panic!("zlob 1.6.3 has no case-folding flag"),
                unknown => panic!("unknown matcher flag {unknown:?}"),
            }
    })
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
