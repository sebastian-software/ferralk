//! Prints the homepage samples as JSON: real output of the published ferralk
//! and ferralk-glob crates at the versions pinned in Cargo.toml.
//!
//! `homepage/scripts/render-home-samples.mjs` runs this program and writes its
//! output to `homepage/app/data/home-samples.json`. The walk builds its project
//! in a fresh temporary directory, so the program needs a Unix filesystem: one
//! folder of that project is made unreadable to show what a walk never opens.

use std::{
    env,
    fmt::Write as _,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{WalkOptions, Walker};
use ferralk_glob::{Pattern, PatternOptions};
use serde_json::{Value, json};

type Failure = Box<dyn std::error::Error>;

/// The paths and patterns of the matcher sample, read the way the walker reads
/// them: wildcards stay inside one folder, `**` crosses folders.
const MATCH_PATHS: [&str; 5] = [
    "main.ts",
    "src/main.ts",
    "src/main.test.ts",
    "src/components/button.tsx",
    "docs/guide.md",
];
const MATCH_PATTERNS: [(&str, &str); 4] = [
    ("star", "*.ts"),
    ("globstar", "**/*.ts"),
    ("braces", "src/**/*.{ts,tsx}"),
    ("extglob", "**/!(*.test).ts"),
];

/// The project the walk sample searches.
const PROJECT_FILES: [&str; 8] = [
    "src/main.ts",
    "src/components/button.tsx",
    "packages/ui/index.ts",
    "public/legacy.ts",
    "node_modules/vendor/index.ts",
    "dist/main.js",
    ".cache/old.ts",
    "pgdata/base.ts",
];
const GIT_IGNORE: &str = "node_modules/\ndist/\n";
const UNREADABLE: &str = "pgdata";
const WALK_QUERIES: [(&str, &str, bool); 3] = [
    ("scoped", "{src,packages}/**/*.{ts,tsx}", true),
    ("unscoped", "**/*.{ts,tsx}", true),
    ("unignored", "**/*.{ts,tsx}", false),
];

/// Three file names that differ only in one letter's bytes.
const BYTE_NAMES: [(&str, &[u8]); 3] = [
    ("UTF-8", b"caf\xc3\xa9.txt"),
    ("Latin-1", b"caf\xe9.txt"),
    ("Latin-1", b"caf\xe8.txt"),
];
const BYTE_PATTERNS: [&[u8]; 3] = [b"*.txt", b"caf\xc3\xa9.txt", b"caf\xe9.txt"];

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        process::exit(1);
    }
}

fn run() -> Result<(), Failure> {
    let samples = json!({
        "matcher": matcher()?,
        "walk": walk()?,
        "bytes": bytes()?,
    });
    println!("{}", serde_json::to_string_pretty(&samples)?);
    Ok(())
}

fn matcher() -> Result<Value, Failure> {
    let mut patterns = Vec::new();
    for (id, source) in MATCH_PATTERNS {
        let pattern = Pattern::compile(source, PatternOptions::walker())?;
        let matches: Vec<bool> = MATCH_PATHS
            .iter()
            .map(|path| pattern.is_match_glob_path(path))
            .collect();
        patterns.push(json!({ "id": id, "pattern": source, "matches": matches }));
    }
    Ok(json!({ "paths": MATCH_PATHS, "patterns": patterns }))
}

fn walk() -> Result<Value, Failure> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let project = env::temp_dir().join(format!("ferralk-homepage-{}-{stamp}", process::id()));
    let result = build_project(&project).and_then(|()| walk_project(&project));
    // Make the locked folder removable again before cleaning up.
    let _ = fs::set_permissions(project.join(UNREADABLE), fs::Permissions::from_mode(0o755));
    fs::remove_dir_all(&project)?;
    result
}

fn build_project(project: &Path) -> Result<(), Failure> {
    fs::create_dir_all(project.join(".git"))?;
    for file in PROJECT_FILES {
        let path = project.join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, "// A file of the homepage's sample project.\n")?;
    }
    fs::write(project.join(".gitignore"), GIT_IGNORE)?;
    fs::set_permissions(project.join(UNREADABLE), fs::Permissions::from_mode(0o000))?;
    if fs::read_dir(project.join(UNREADABLE)).is_ok() {
        return Err(
            "The sample needs a user who cannot read a 000 folder; do not run it as root.".into(),
        );
    }
    Ok(())
}

fn walk_project(project: &Path) -> Result<Value, Failure> {
    let previous: PathBuf = env::current_dir()?;
    env::set_current_dir(project)?;
    let queries = WALK_QUERIES
        .iter()
        .map(|&(id, pattern, git_ignore)| query(id, pattern, git_ignore))
        .collect::<Result<Vec<_>, _>>();
    env::set_current_dir(previous)?;
    Ok(json!({
        "files": PROJECT_FILES,
        "gitIgnore": GIT_IGNORE,
        "unreadable": UNREADABLE,
        "queries": queries?,
    }))
}

fn query(id: &str, pattern: &str, git_ignore: bool) -> Result<Value, Failure> {
    let result = Walker::new(".")
        .include(pattern)?
        .respect_git_ignore(git_ignore)
        .options(WalkOptions::default().files_only(true).sort(true))
        .collect()?;
    let paths: Vec<String> = result
        .entries()
        .iter()
        .map(|entry| entry.relative_path().display().to_string())
        .collect();
    let errors: Vec<Value> = result
        .errors()
        .iter()
        .map(|error| {
            let cause = std::error::Error::source(error).map(ToString::to_string);
            json!({ "error": error.to_string(), "cause": cause })
        })
        .collect();
    Ok(json!({
        "id": id,
        "pattern": pattern,
        "gitIgnore": git_ignore,
        "paths": paths,
        "errors": errors,
    }))
}

fn bytes() -> Result<Value, Failure> {
    let names: Vec<Value> = BYTE_NAMES
        .iter()
        .map(|(encoding, name)| {
            json!({
                "encoding": encoding,
                "escaped": escaped(name),
                "hex": name.iter().map(|byte| format!("{byte:02X}")).collect::<Vec<_>>().join(" "),
                "lossy": String::from_utf8_lossy(name),
            })
        })
        .collect();
    let mut patterns = Vec::new();
    for source in BYTE_PATTERNS {
        let pattern = Pattern::compile(source, PatternOptions::walker())?;
        let matches: Vec<bool> = BYTE_NAMES
            .iter()
            .map(|(_, name)| pattern.is_match_glob_path(name))
            .collect();
        patterns.push(json!({ "pattern": escaped(source), "matches": matches }));
    }
    Ok(json!({ "names": names, "patterns": patterns }))
}

/// Printable ASCII and valid UTF-8 as text, every other byte as `\xHH`.
fn escaped(bytes: &[u8]) -> String {
    bytes
        .utf8_chunks()
        .map(|chunk| {
            let mut text = chunk.valid().to_owned();
            for byte in chunk.invalid() {
                let _ = write!(text, "\\x{byte:02X}");
            }
            text
        })
        .collect()
}
