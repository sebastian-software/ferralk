//! Walks a root with a fast-glob or globby style pattern list, in which a
//! leading `!` marks an exclude.
//!
//! ```sh
//! cargo run -p ferralk --example include_exclude -- <root> ['<glob>'...]
//! cargo run -p ferralk --example include_exclude -- . 'crates/**/*.rs' '!**/tests/**'
//! ```
//!
//! Without globs the list is `**/*.rs !**/target/**`. An invalid pattern is
//! reported and skipped; the others still apply.

use std::{env, error::Error, path::PathBuf};

use ferralk::{WalkOptions, Walker};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let root = args
        .next()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let mut globs = args.collect::<Vec<_>>();
    if globs.is_empty() {
        globs = vec!["**/*.rs".to_owned(), "!**/target/**".to_owned()];
    }

    // fast-glob's default `onlyFiles: true`; the walker returns directories
    // too unless told otherwise.
    let mut walker = Walker::new(&root).options(WalkOptions::default().files_only(true).sort(true));
    for glob in &globs {
        // The walker rejects a leading `!` instead of reading it as negation,
        // so move those entries to `exclude`. `!(…)` is a negated extglob and
        // stays an include, as it does in fast-glob.
        let (pattern, added) = match glob.strip_prefix('!').filter(|rest| !rest.starts_with('(')) {
            Some(excluded) => (excluded, walker.try_exclude(excluded).map(|_| ())),
            None => (glob.as_str(), walker.try_include(glob).map(|_| ())),
        };
        // The `try_` forms leave the walker as it was when a pattern is
        // invalid, so one bad entry does not cost the rest of the list.
        if let Err(error) = added {
            // The offset is a byte offset into the pattern that was passed.
            let column = pattern
                .get(..error.offset())
                .map_or(error.offset(), |before| before.chars().count());
            let marker = " ".repeat(column);
            eprintln!("skipping {glob:?}: {error}\n  {pattern}\n  {marker}^");
        }
    }

    let result = walker.collect()?;
    for entry in result.entries() {
        println!("{}", entry.relative_path().display());
    }
    for error in result.errors() {
        match error.source() {
            Some(cause) => eprintln!("warning: {error}: {cause}"),
            None => eprintln!("warning: {error}"),
        }
    }
    Ok(())
}
