#![forbid(unsafe_code)]
//! What zlob 1.6.3's walker reports for the three symlink shapes under
//! `ZLOB_WALK_NO_REPORT_DIRS`, which is the flag `WalkOptions::files_only`
//! maps to.
//!
//! zlob has no directories-only walk flag - `ZLOB_ONLYDIR` is a matcher flag -
//! so `files_only` is the only side of #89 that has an oracle at all. Run it
//! with:
//!
//! ```text
//! cargo test -p oracle --test zlob_walk_symlinks -- --ignored --nocapture
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use zlob::walk::{WalkBuilder, WalkFlags};

/// A tree holding one link of each shape next to one ordinary file.
fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "ferralk-zlob-symlinks-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos()
    ));
    fs::create_dir_all(root.join("target-dir")).expect("create the link target directory");
    fs::write(root.join("target-file.txt"), b"target").expect("write the link target file");
    fs::write(root.join("plain.txt"), b"plain").expect("write the ordinary file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink("target-file.txt", root.join("link-to-file")).expect("link to a file");
        symlink("target-dir", root.join("link-to-dir")).expect("link to a directory");
        symlink("nowhere", root.join("link-broken")).expect("link to nothing");
    }
    root
}

fn names(root: &Path, flags: WalkFlags) -> Vec<String> {
    let mut walker = WalkBuilder::new(root).expect("the fixture root is valid");
    walker.options(flags).threads(1);
    let mut found = walker
        .collect()
        .expect("the oracle walk succeeds")
        .iter()
        .filter_map(|entry| {
            Path::new(entry.path())
                .strip_prefix(root)
                .ok()
                .map(|relative| relative.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    found.sort();
    found
}

/// Records zlob's answer rather than asserting ferralk matches it: the point is
/// to know what the reference engine does before deciding whether to agree.
#[test]
#[ignore = "requires the Zig toolchain that builds zlob"]
#[cfg(unix)]
fn zlob_walk_reports_symlinks_under_no_report_dirs() {
    let root = fixture();

    let everything = names(&root, WalkFlags::empty());
    let files_only = names(&root, WalkFlags::NO_REPORT_DIRS);

    println!("zlob 1.6.3, no flags:        {everything:?}");
    println!("zlob 1.6.3, NO_REPORT_DIRS:  {files_only:?}");
    for shape in ["link-to-file", "link-to-dir", "link-broken"] {
        println!(
            "  {shape:<13} plain: {:<5} NO_REPORT_DIRS: {}",
            everything.iter().any(|name| name == shape),
            files_only.iter().any(|name| name == shape)
        );
    }

    let _ = fs::remove_dir_all(&root);

    // The one thing worth pinning: the fixture really was walked.
    assert!(
        everything.iter().any(|name| name == "plain.txt"),
        "the oracle walk did not see the fixture"
    );
}
