//! Walks several roots and keeps going past everything that cannot be read,
//! then reports it: a missing root, a directory without permission, an
//! unreadable ignore file.
//!
//! ```sh
//! cargo run -p ferralk --example keep_going -- <root>...
//! cargo run -p ferralk --example keep_going -- crates does-not-exist
//! ```

use std::{env, error::Error, io, path::PathBuf};

use ferralk::{WalkOperation, Walker};

fn main() -> Result<(), Box<dyn Error>> {
    let mut roots = env::args_os().skip(1).map(PathBuf::from);
    let first = roots.next().unwrap_or_else(|| PathBuf::from("."));
    let walker = Walker::new(first)
        .add_roots(roots)?
        .respect_git_ignore(true);

    // The default `ErrorPolicy::Collect`: every recoverable error is kept next
    // to the entries, and one failing root does not discard the others.
    let result = walker.collect()?;
    println!("{} entries", result.entries().len());

    for error in result.errors() {
        // Decide from the typed parts; the message text is for people.
        let advice = match (error.operation(), error.io_kind()) {
            (WalkOperation::ReadDir, io::ErrorKind::NotFound) => "does not exist",
            (WalkOperation::ReadDir, io::ErrorKind::PermissionDenied) => "not permitted",
            (WalkOperation::ReadIgnore, _) => "ignore rules skipped",
            _ => "not walked",
        };
        let cause = error.source().map(ToString::to_string).unwrap_or_default();
        eprintln!("{}: {advice} ({error}: {cause})", error.path().display());
    }

    if result.errors().is_empty() {
        Ok(())
    } else {
        Err(format!("{} error(s)", result.errors().len()).into())
    }
}
