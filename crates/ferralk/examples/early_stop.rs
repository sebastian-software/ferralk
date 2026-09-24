//! Three ways to end a walk before it has seen the whole tree.
//!
//! ```sh
//! cargo run -p ferralk --example early_stop -- <root> [count] [deadline-ms]
//! cargo run -p ferralk --example early_stop -- . 5 50
//! ```

use std::{env, error::Error, ffi::OsStr, path::PathBuf, sync::OnceLock, thread, time::Duration};

use ferralk::{CancellationToken, Verdict, WalkOptions, Walker};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let root = args
        .next()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let count = args
        .next()
        .map(|count| count.parse())
        .transpose()?
        .unwrap_or(5);
    let deadline =
        Duration::from_millis(args.next().map(|ms| ms.parse()).transpose()?.unwrap_or(50));

    // 1. The first `count` files, on the calling thread. A stream stops when
    //    the iterator is dropped. Errors are items too, so they are handled
    //    before `take` counts, or `take(count)` could return fewer files.
    println!("first {count} files:");
    let first = Walker::new(&root)
        .options(WalkOptions::default().files_only(true))
        .stream()
        .filter_map(|item| {
            item.inspect_err(|error| eprintln!("  warning: {error}"))
                .ok()
        })
        .take(count);
    for entry in first {
        println!("  {}", entry.path().display());
    }

    // 2. A parallel walk that ends as soon as one entry answers the question.
    let found = OnceLock::new();
    let result = Walker::new(&root).visit(|entry| {
        if entry.basename() == Some(OsStr::new("Cargo.toml")) {
            let _ = found.set(entry.path().to_path_buf());
            Verdict::Stop
        } else {
            Verdict::Skip
        }
    })?;
    // `Verdict::Stop` ends the walk the way a cancellation does.
    match found.get() {
        Some(path) => println!(
            "a Cargo.toml: {} (walk stopped: {})",
            path.display(),
            result.was_cancelled()
        ),
        None => println!("no Cargo.toml below {}", root.display()),
    }

    // 3. A deadline, enforced from another thread through a token. The walk
    //    returns what it found so far and says that it stopped early.
    let token = CancellationToken::default();
    {
        let token = token.clone();
        // Not joined: if the walk finishes first, the timer is simply left
        // behind when the program ends.
        thread::spawn(move || {
            thread::sleep(deadline);
            token.cancel();
        });
    }
    let result = Walker::new(&root).cancellation(token).collect()?;
    let state = if result.was_cancelled() {
        "stopped at the deadline"
    } else {
        "complete"
    };
    println!(
        "{} entries within {deadline:?}: {state}",
        result.entries().len()
    );
    Ok(())
}
