#![forbid(unsafe_code)]

use std::{env, fs, path::Path};

use corpus::{Case, decode_bytes};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::args().nth(1).unwrap_or_else(|| "corpus".to_owned());
    let mut files = Vec::new();
    collect_jsonl(Path::new(&root), &mut files)?;
    files.sort();

    let mut cases = 0_usize;
    for file in files {
        for (line_number, line) in fs::read_to_string(&file)?.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let case: Case = serde_json::from_str(line)
                .map_err(|error| format!("{}:{}: {error}", file.display(), line_number + 1))?;
            decode_bytes(&case.pattern).map_err(|error| {
                format!(
                    "{}:{}: invalid pattern: {error}",
                    file.display(),
                    line_number + 1
                )
            })?;
            decode_bytes(&case.path).map_err(|error| {
                format!(
                    "{}:{}: invalid path: {error}",
                    file.display(),
                    line_number + 1
                )
            })?;
            cases += 1;
        }
    }

    println!("validated {cases} corpus cases from {root}");
    Ok(())
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
