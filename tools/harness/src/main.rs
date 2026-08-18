#![forbid(unsafe_code)]

use std::{collections::HashSet, env, fs, path::Path};

use corpus::{Case, decode_bytes};
use ferralk_glob::{Pattern, PatternOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::args().nth(1).unwrap_or_else(|| "corpus".to_owned());
    let mut files = Vec::new();
    collect_jsonl(Path::new(&root), &mut files)?;
    files.sort();

    let mut ids = HashSet::new();
    let mut cases = 0_usize;
    let mut replayed = 0_usize;
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
            if !is_ignore_topic {
                let options = options_from_flags(&case.flags)
                    .map_err(|error| format!("{}:{}: {error}", file.display(), line_number + 1))?;
                let actual = Pattern::compile(pattern, options)
                    .map_err(|error| format!("{}:{}: {error}", file.display(), line_number + 1))?
                    .is_match(path);
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

    println!("validated {cases} corpus cases; replayed {replayed} matcher cases from {root}");
    Ok(())
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
