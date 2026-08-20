#![forbid(unsafe_code)]

use std::{collections::HashSet, env, fs, path::Path};

use corpus::{Case, CaseKind, decode_bytes};
use ferralk_glob::{Pattern, PatternOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::args().nth(1).unwrap_or_else(|| "corpus".to_owned());
    let mut files = Vec::new();
    collect_jsonl(Path::new(&root), &mut files)?;
    files.sort();

    let mut ids = HashSet::new();
    let mut cases = 0_usize;
    let mut replayed = 0_usize;
    let mut skipped = 0_usize;
    for file in files {
        let is_ignore_topic = file.file_name().is_some_and(|name| name == "ignore.jsonl");
        for (line_number, line) in fs::read_to_string(&file)?.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let case: Case = serde_json::from_str(line)
                .map_err(|error| format!("{}:{}: {error}", file.display(), line_number + 1))?;
            if !ids.insert((file.clone(), case.id.clone())) {
                return Err(format!(
                    "{}:{}: duplicate case id {}",
                    file.display(),
                    line_number + 1,
                    case.id
                )
                .into());
            }
            let pattern = decode_bytes(&case.pattern).map_err(|error| {
                format!(
                    "{}:{}: invalid pattern: {error}",
                    file.display(),
                    line_number + 1
                )
            })?;
            let path = decode_bytes(&case.path).map_err(|error| {
                format!(
                    "{}:{}: invalid path: {error}",
                    file.display(),
                    line_number + 1
                )
            })?;
            if !matches!(
                case.kind,
                CaseKind::CompileError | CaseKind::AbsolutePattern
            ) && (case.error_offset.is_some() || case.error_message.is_some())
            {
                return Err(format!(
                    "{}:{}: error_offset and error_message belong to a rejection case",
                    file.display(),
                    line_number + 1
                )
                .into());
            }
            if !case.runs_on_host() {
                // The verdict describes another separator platform, where a
                // different host replays it.
                skipped += 1;
                cases += 1;
                continue;
            }
            if !is_ignore_topic {
                let options = options_from_flags(&case.flags)
                    .map_err(|error| format!("{}:{}: {error}", file.display(), line_number + 1))?;
                let actual = match case.kind {
                    CaseKind::Matcher => Pattern::compile(pattern, options)
                        .map_err(|error| {
                            format!("{}:{}: {error}", file.display(), line_number + 1)
                        })?
                        .is_match(path),
                    CaseKind::MatchGlobPath => Pattern::compile(pattern, options)
                        .map_err(|error| {
                            format!("{}:{}: {error}", file.display(), line_number + 1)
                        })?
                        .is_match_glob_path(path),
                    CaseKind::HasWildcards => Pattern::has_wildcards(pattern, options),
                    CaseKind::AbsolutePattern => {
                        replay_absolute_pattern(&case).map_err(|error| {
                            format!("{}:{}: {error}", file.display(), line_number + 1)
                        })?
                    }
                    CaseKind::CompileError => replay_compile_error(&case, pattern, options)
                        .map_err(|error| {
                            format!("{}:{}: {error}", file.display(), line_number + 1)
                        })?,
                    CaseKind::MatchPaths | CaseKind::MatchPathsAt => {
                        let paths = case
                            .paths
                            .iter()
                            .map(|path| decode_bytes(path))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|error| {
                                format!(
                                    "{}:{}: invalid paths: {error}",
                                    file.display(),
                                    line_number + 1
                                )
                            })?;
                        let expected = case
                            .matches
                            .iter()
                            .map(|path| decode_bytes(path))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|error| {
                                format!(
                                    "{}:{}: invalid matches: {error}",
                                    file.display(),
                                    line_number + 1
                                )
                            })?;
                        let matcher = Pattern::compile(pattern, options).map_err(|error| {
                            format!("{}:{}: {error}", file.display(), line_number + 1)
                        })?;
                        let selected = match case.kind {
                            CaseKind::MatchPaths => matcher.filter_paths(&paths),
                            CaseKind::MatchPathsAt => {
                                let base_path = decode_bytes(&case.base_path).map_err(|error| {
                                    format!(
                                        "{}:{}: invalid base path: {error}",
                                        file.display(),
                                        line_number + 1
                                    )
                                })?;
                                matcher.filter_paths_at(base_path, &paths)
                            }
                            CaseKind::Matcher
                            | CaseKind::HasWildcards
                            | CaseKind::CompileError
                            | CaseKind::MatchGlobPath
                            | CaseKind::MatchPathIndices
                            | CaseKind::MatchPathIndicesAt
                            | CaseKind::AbsolutePattern => unreachable!(),
                        };
                        if selected
                            .iter()
                            .map(|path| path.as_slice())
                            .eq(expected.iter().map(Vec::as_slice))
                        {
                            case.expected
                        } else {
                            return Err(format!(
                                "{}:{}: selected paths differ from corpus",
                                file.display(),
                                line_number + 1
                            )
                            .into());
                        }
                    }
                    CaseKind::MatchPathIndices | CaseKind::MatchPathIndicesAt => {
                        let paths = case
                            .paths
                            .iter()
                            .map(|path| decode_bytes(path))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|error| {
                                format!(
                                    "{}:{}: invalid paths: {error}",
                                    file.display(),
                                    line_number + 1
                                )
                            })?;
                        let matcher = Pattern::compile(pattern, options).map_err(|error| {
                            format!("{}:{}: {error}", file.display(), line_number + 1)
                        })?;
                        let selected = match case.kind {
                            CaseKind::MatchPathIndices => matcher.filter_path_indices(&paths),
                            CaseKind::MatchPathIndicesAt => {
                                let base_path = decode_bytes(&case.base_path).map_err(|error| {
                                    format!(
                                        "{}:{}: invalid base path: {error}",
                                        file.display(),
                                        line_number + 1
                                    )
                                })?;
                                matcher.filter_path_indices_at(base_path, &paths)
                            }
                            CaseKind::Matcher
                            | CaseKind::HasWildcards
                            | CaseKind::CompileError
                            | CaseKind::MatchGlobPath
                            | CaseKind::MatchPaths
                            | CaseKind::MatchPathsAt
                            | CaseKind::AbsolutePattern => unreachable!(),
                        };
                        if selected != case.indices {
                            return Err(format!(
                                "{}:{}: selected indices differ from corpus",
                                file.display(),
                                line_number + 1
                            )
                            .into());
                        }
                        case.expected
                    }
                };
                if actual != case.expected {
                    return Err(format!(
                        "{}:{}: expected {}, got {} for {} against {}",
                        file.display(),
                        line_number + 1,
                        case.expected,
                        actual,
                        case.pattern,
                        case.path
                    )
                    .into());
                }
                replayed += 1;
            }
            cases += 1;
        }
    }

    println!(
        "validated {cases} corpus cases; replayed {replayed} operation cases from {root}; \
         skipped {skipped} cases written for another platform"
    );
    Ok(())
}

/// Replays a `compile_error` case and returns its always-rejecting verdict.
fn replay_compile_error(
    case: &Case,
    pattern: Vec<u8>,
    options: PatternOptions,
) -> Result<bool, String> {
    if case.expected {
        return Err("a compile_error case must record expected false".to_owned());
    }
    let error = match Pattern::compile(pattern, options) {
        Err(error) => error,
        Ok(_) => return Err(format!("{} compiled but must be rejected", case.pattern)),
    };
    if let Some(offset) = case.error_offset
        && error.offset() != offset
    {
        return Err(format!(
            "expected error offset {offset}, got {} for {}",
            error.offset(),
            case.pattern
        ));
    }
    if let Some(message) = case.error_message.as_deref()
        && error.message() != message
    {
        return Err(format!(
            "expected error message {message:?}, got {:?} for {}",
            error.message(),
            case.pattern
        ));
    }
    Ok(case.expected)
}

/// Replays one absolute-pattern rewrite: the pattern the walker ends up
/// compiling for `base_path`, that nothing can match, or that it is refused.
fn replay_absolute_pattern(case: &Case) -> Result<bool, String> {
    let pattern =
        decode_bytes(&case.pattern).map_err(|error| format!("invalid pattern: {error}"))?;
    let root =
        decode_bytes(&case.base_path).map_err(|error| format!("invalid base path: {error}"))?;
    let expects_rejection = case.error_message.is_some() || case.error_offset.is_some();
    if expects_rejection && case.rewritten.is_some() {
        return Err("a rejected rewrite has no rewritten pattern".to_owned());
    }
    match ferralk::corpus_rewrite_absolute_pattern(&pattern, &root, case.windows_paths) {
        Ok(rewritten) => {
            if expects_rejection {
                return Err(format!(
                    "{} was accepted but must be rejected",
                    case.pattern
                ));
            }
            let expected = case
                .rewritten
                .as_deref()
                .map(decode_bytes)
                .transpose()
                .map_err(|error| format!("invalid rewritten pattern: {error}"))?;
            if rewritten != expected {
                return Err(format!(
                    "expected {expected:?}, got {rewritten:?} for {} under {}",
                    case.pattern, case.base_path
                ));
            }
            Ok(case.expected)
        }
        Err(error) => {
            if !expects_rejection {
                return Err(format!("{} was rejected: {error}", case.pattern));
            }
            if let Some(offset) = case.error_offset
                && error.offset() != offset
            {
                return Err(format!(
                    "expected error offset {offset}, got {} for {}",
                    error.offset(),
                    case.pattern
                ));
            }
            if let Some(message) = case.error_message.as_deref()
                && error.message() != message
            {
                return Err(format!(
                    "expected error message {message:?}, got {:?} for {}",
                    error.message(),
                    case.pattern
                ));
            }
            Ok(case.expected)
        }
    }
}

fn options_from_flags(flags: &[String]) -> Result<PatternOptions, String> {
    let mut options = PatternOptions::default();
    for flag in flags {
        options = match flag.as_str() {
            "braces" => options.braces(true),
            "recursive_double_star" => options.recursive_double_star(true),
            "extglob" => options.extglob(true),
            "match_hidden" => options.match_hidden(true),
            "case_insensitive" => options.case_insensitive(true),
            "no_escape" => options.escape(false),
            // zlob's result-shaping NOCHECK flag is recorded for the oracle;
            // Ferralk's list filter deliberately returns no synthetic path.
            "nocheck" => options,
            _ => return Err(format!("unknown matcher flag {flag:?}")),
        };
    }
    Ok(options)
}

fn collect_jsonl(root: &Path, files: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
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
