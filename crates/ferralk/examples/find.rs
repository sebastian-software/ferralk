use std::{env, error::Error, path::Path};

use ferralk::{ErrorPolicy, WalkOptions, Walker};
use ferralk_glob::{Pattern, PatternOptions};

fn main() -> Result<(), Box<dyn Error>> {
    let pattern = env::args()
        .nth(1)
        .ok_or("usage: cargo run --example find -- '<glob>'")?;

    // Exercise the matcher crate directly before giving the same filesystem
    // glob dialect to the walker.
    Pattern::compile(
        &pattern,
        PatternOptions::default()
            .braces(true)
            .recursive_double_star(true)
            .extglob(true),
    )?;

    let walker =
        if Path::new("crates/ferralk-glob").is_dir() && Path::new("crates/ferralk").is_dir() {
            Walker::new("crates/ferralk-glob").add_root("crates/ferralk")?
        } else {
            Walker::new(".")
        };

    let result = walker
        .include(&pattern)?
        .respect_git_ignore(true)
        .error_policy(ErrorPolicy::Collect)
        .options(WalkOptions::default().files_only(true).sort(true))
        .collect()?;

    for entry in result.entries() {
        println!("{}", entry.path().display());
    }
    for error in result.errors() {
        eprintln!("{error}");
    }

    if result.errors().is_empty() {
        Ok(())
    } else {
        Err(format!("walk completed with {} error(s)", result.errors().len()).into())
    }
}
