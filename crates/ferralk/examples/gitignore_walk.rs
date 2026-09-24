//! Lists the files below a root that the ignore rules leave in:
//! `.gitignore`, `.ignore`, and `.git/info/exclude` apply, including those in
//! the directories between the repository root and the walk root.
//!
//! ```sh
//! cargo run -p ferralk --example gitignore_walk -- <root> ['<glob>']
//! cargo run -p ferralk --example gitignore_walk -- . '**/*.rs'
//! ```
//!
//! Paths are printed relative to the root. Without a glob every file that is
//! not ignored is listed, hidden files such as `.gitignore` included.

use std::{env, error::Error, path::PathBuf};

use ferralk::{WalkError, WalkOptions, Walker};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let root = args
        .next()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);

    let mut walker = Walker::new(&root)
        .respect_git_ignore(true)
        // One `WalkOptions` value: a second `options()` call would replace it.
        .options(WalkOptions::default().files_only(true).sort(true));
    if let Some(glob) = args.next() {
        // Patterns are bytes, so a non-UTF-8 argument works too.
        walker = walker.include(glob.as_encoded_bytes())?;
    }
    let result = walker.collect()?;

    for entry in result.entries() {
        // `path()` is the root joined with the relative path.
        let relative = entry.path().strip_prefix(entry.root())?;
        println!("{}", relative.display());
    }

    // `collect()` returned `Ok`, but a directory that could not be read, even
    // the root itself, is reported only here.
    report(result.errors())
}

fn report(errors: &[WalkError]) -> Result<(), Box<dyn Error>> {
    for error in errors {
        // `Display` names the operation and the path; `source()` says why.
        match error.source() {
            Some(cause) => eprintln!("warning: {error}: {cause}"),
            None => eprintln!("warning: {error}"),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("{} path(s) could not be read", errors.len()).into())
    }
}
