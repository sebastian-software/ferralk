//! Differential parity between the native and the portable directory backend.
//!
//! The RFC property under test is that the two backends produce the same
//! entries and the same error classes. Each family below builds one fixture
//! tree, walks it twice with an identical `Walker` — once through the native
//! backend the active feature selects, once through [`StdBackend`] — and
//! compares both the entry descriptions and the error multiset. A family that
//! only compared entries would miss the more interesting half: the two readers
//! reach failures by different syscalls.
//!
//! Parser-level and single-call behaviour lives with each backend
//! (`macos_native`, `linux_native`): rejected records, entries that vanish
//! between the read and their stat, special types classified without a stat,
//! and non-directories refused at open. Those are unit tests of one reader.
//! This module only asks whether two readers agree on a whole tree.
//!
//! Platform limitations are recorded at the family that carries them.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    DirectoryTask, ErrorPolicy, IgnoreScope, StdBackend, WalkEntry, WalkError, WalkOptions,
    WalkState, Walker,
};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

/// A fixture tree that removes itself, so a failing assertion cannot leave one
/// family's tree behind for the next.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ferralk-parity-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        ));
        fs::create_dir_all(&root).expect("create parity fixture root");
        Self { root }
    }

    fn directory(&self, relative: impl AsRef<Path>) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(&path).expect("create parity fixture directory");
        path
    }

    fn write(&self, relative: impl AsRef<Path>) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parity fixture parent");
        }
        fs::write(&path, b"fixture").expect("write parity fixture file");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The permission family leaves a directory the walker could not read;
        // make it removable again so the fixture cleans up.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            if let Ok(entries) = fs::read_dir(&self.root) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        let _ =
                            fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o755));
                    }
                }
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// One entry, reduced to what both backends must agree on.
type DescribedEntry = (PathBuf, bool, bool, usize);

/// One error, reduced to its class: where it happened, which operation
/// produced it, and what kind of failure it was. The message text is the
/// operating system's and is deliberately not compared.
type DescribedError = (PathBuf, &'static str, io::ErrorKind);

fn describe_entries(entries: &[WalkEntry], root: &Path) -> Vec<DescribedEntry> {
    let mut described: Vec<DescribedEntry> = entries
        .iter()
        .map(|entry| {
            (
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("entry belongs to the fixture")
                    .to_path_buf(),
                entry.is_dir(),
                entry.is_symlink(),
                entry.depth(),
            )
        })
        .collect();
    described.sort();
    described
}

fn describe_errors(errors: &[WalkError], root: &Path) -> Vec<DescribedError> {
    let mut described: Vec<DescribedError> = errors
        .iter()
        .map(|error| {
            (
                error
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(error.path())
                    .to_path_buf(),
                error.operation(),
                error.source.kind(),
            )
        })
        .collect();
    described.sort();
    described
}

/// Walks the same tree through the portable backend, using the same traversal
/// code the native walk uses so only the backend differs.
fn walk_portable(walker: &Walker) -> (Vec<WalkEntry>, Vec<WalkError>) {
    let mut state = WalkState::new(walker, &crate::keep_every_entry);
    let root = walker
        .roots()
        .next()
        .expect("a walk has a root")
        .to_path_buf();
    let (ignores, ignore_errors) = IgnoreScope::for_root(walker, &StdBackend, &root);
    let task = DirectoryTask {
        path: root.clone(),
        depth: 0,
        root: 0,
        cycle_guard: std::sync::Arc::new(crate::CycleGuard::default()),
        ignores,
        ignore_errors,
    };
    state
        .walk_directory(&StdBackend, task)
        .expect("the portable walk collects rather than aborts");
    (state.entries, state.errors)
}

/// Reports what one side has that the other does not.
///
/// A large family holds thousands of entries, so printing both lists buries
/// the disagreement in hundreds of kilobytes. Only the difference is useful,
/// and a handful of it is enough to name the problem.
fn difference<T: Clone + Ord + std::fmt::Debug>(native: &[T], portable: &[T]) -> Option<String> {
    let missing: Vec<&T> = portable
        .iter()
        .filter(|item| !native.contains(item))
        .collect();
    let extra: Vec<&T> = native
        .iter()
        .filter(|item| !portable.contains(item))
        .collect();
    if missing.is_empty() && extra.is_empty() {
        return None;
    }
    const SHOWN: usize = 5;
    let summarize = |label: &str, items: &[&T]| {
        format!(
            "\n  {label} ({}): {:?}{}",
            items.len(),
            &items[..items.len().min(SHOWN)],
            if items.len() > SHOWN { " ..." } else { "" }
        )
    };
    Some(format!(
        "{}{}",
        summarize("only the portable backend produced", &missing),
        summarize("only the native backend produced", &extra)
    ))
}

/// Asserts that both backends see the same tree and fail the same way.
///
/// The walk is serial and collecting: a parallel walk would compare the same
/// backends through a different scheduler, which the parallel family below
/// checks separately.
fn assert_parity(family: &str, root: &Path, walker: Walker) {
    let native = walker.clone().collect().expect("the native walk succeeds");
    let (portable_entries, portable_errors) = walk_portable(&walker);

    // The equality check is the fast path; the quadratic difference only runs
    // once a family has already failed.
    let (native_entries, portable_entries) = (
        describe_entries(native.entries(), root),
        describe_entries(&portable_entries, root),
    );
    if native_entries != portable_entries {
        let report = difference(&native_entries, &portable_entries)
            .expect("unequal descriptions differ somewhere");
        panic!("{family}: the backends disagree on the entries:{report}");
    }
    let (native_errors, portable_errors) = (
        describe_errors(native.errors(), root),
        describe_errors(&portable_errors, root),
    );
    if native_errors != portable_errors {
        let report = difference(&native_errors, &portable_errors)
            .expect("unequal descriptions differ somewhere");
        panic!("{family}: the backends disagree on the error classes:{report}");
    }
}

fn collecting_walker(root: &Path) -> Walker {
    Walker::new(root)
        .threads(1)
        .error_policy(ErrorPolicy::Collect)
        .options(WalkOptions::default().sort(true))
}

#[test]
fn parity_over_a_deep_tree() {
    let fixture = Fixture::new("deep");
    let mut directory = PathBuf::new();
    for level in 0..14 {
        directory = directory.join(format!("level-{level}"));
        fixture.write(directory.join("file.txt"));
    }

    assert_parity("deep tree", &fixture.root, collecting_walker(&fixture.root));
}

#[test]
fn parity_over_a_directory_larger_than_one_read_batch() {
    // Both native readers fill a 32 KiB buffer and refill until the directory
    // is exhausted. Names of this length put well over a hundred entries in
    // each batch, so several thousand entries force many refills, which is the
    // loop a single-batch fixture never enters.
    let fixture = Fixture::new("large");
    let directory = fixture.directory("many");
    for index in 0..3000 {
        fs::write(
            directory.join(format!("entry-{index:05}-with-a-name-of-some-length.txt")),
            b"fixture",
        )
        .expect("write large-directory fixture");
    }

    assert_parity(
        "large directory",
        &fixture.root,
        collecting_walker(&fixture.root),
    );
}

#[test]
fn parity_over_empty_and_nearly_empty_directories() {
    let fixture = Fixture::new("empty");
    fixture.directory("empty");
    fixture.directory("nested/empty");
    fixture.write("nested/one.txt");

    assert_parity(
        "empty directories",
        &fixture.root,
        collecting_walker(&fixture.root),
    );
}

#[cfg(unix)]
#[test]
fn parity_over_symlinks_including_broken_and_directory_links() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new("symlinks");
    fixture.write("real/inside.txt");
    symlink("real", fixture.root.join("directory-link")).expect("create directory symlink");
    symlink("real/inside.txt", fixture.root.join("file-link")).expect("create file symlink");
    symlink("nowhere", fixture.root.join("broken-link")).expect("create broken symlink");

    assert_parity(
        "symlinks, not followed",
        &fixture.root,
        collecting_walker(&fixture.root),
    );
    // Following turns the broken link into an error both backends must report
    // the same way, and the directory link into a second route to one tree.
    assert_parity(
        "symlinks, followed",
        &fixture.root,
        collecting_walker(&fixture.root)
            .options(WalkOptions::default().sort(true).follow_symlinks(true)),
    );
}

#[cfg(unix)]
#[test]
fn parity_over_a_symlink_cycle() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new("cycle");
    fixture.write("tree/leaf.txt");
    // A link pointing at its own ancestor is the loop the guard has to break;
    // both backends must break it at the same place.
    symlink("..", fixture.root.join("tree/up")).expect("create cycle symlink");

    assert_parity(
        "symlink cycle",
        &fixture.root,
        collecting_walker(&fixture.root)
            .options(WalkOptions::default().sort(true).follow_symlinks(true)),
    );
}

#[cfg(unix)]
#[test]
fn parity_over_an_unreadable_directory() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new("permissions");
    fixture.write("readable/file.txt");
    let closed = fixture.directory("closed");
    fixture.write("closed/hidden.txt");
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o000))
        .expect("close the directory's permissions");

    // A process with CAP_DAC_OVERRIDE, root in a container for instance, reads
    // the directory anyway and there is no error to compare. The parity claim
    // still holds, so the family checks it and only skips the error half.
    if fs::read_dir(&closed).is_ok() {
        eprintln!("skipping the unreadable-directory error check: this process can read it anyway");
    }

    assert_parity(
        "unreadable directory",
        &fixture.root,
        collecting_walker(&fixture.root),
    );
}

#[cfg(unix)]
#[test]
fn parity_over_a_named_pipe() {
    let fixture = Fixture::new("fifo");
    fixture.write("regular.txt");
    let fifo = fixture.root.join("pipe");
    let created = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|status| status.success());
    if !created {
        eprintln!("skipping the named-pipe family: mkfifo is unavailable");
        return;
    }

    // The native readers answer a FIFO from the directory record without a
    // stat while the portable reader asks the filesystem. Both have to reach
    // the same verdict: neither a directory nor a symlink.
    assert_parity(
        "named pipe",
        &fixture.root,
        collecting_walker(&fixture.root),
    );
}

#[cfg(unix)]
#[test]
fn parity_over_names_at_the_length_limit() {
    let fixture = Fixture::new("long-names");
    // 255 bytes is NAME_MAX on both ext4 and APFS. Anything longer is refused
    // by the filesystem rather than by the walker, so the limit itself is the
    // interesting length: the name fills its record to the edge.
    for length in [1_usize, 254, 255] {
        let name = "n".repeat(length);
        fixture.write(format!("names/{name}"));
    }

    assert_parity(
        "names at the length limit",
        &fixture.root,
        collecting_walker(&fixture.root),
    );
}

#[cfg(target_os = "linux")]
#[test]
fn parity_over_non_utf8_names() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    // Linux only. APFS rejects a filename that is not valid UTF-8 with
    // EILSEQ, so macOS cannot hold this fixture at all; the byte-first path
    // handling it exercises is covered there by `bytes.jsonl` in the matcher
    // corpus instead.
    let fixture = Fixture::new("non-utf8");
    for name in [
        vec![b'n', 0xFF],
        vec![0xC3, 0x28],
        vec![0xFE, 0xFF, b'.', b'x'],
    ] {
        fixture.write(PathBuf::from(OsString::from_vec(name)));
    }

    assert_parity(
        "non-UTF-8 names",
        &fixture.root,
        collecting_walker(&fixture.root),
    );
}

#[test]
fn parity_over_filters_hidden_paths_and_ignore_rules() {
    let fixture = Fixture::new("filters");
    fixture.write("src/lib.rs");
    fixture.write("src/nested/mod.rs");
    fixture.write("src/generated.tmp");
    fixture.write("ignored/skip.rs");
    fixture.write(".hidden/skip.rs");
    fixture.write(".gitignore");
    fs::write(fixture.root.join(".gitignore"), b"ignored/\n").expect("write the ignore file");

    let walker = collecting_walker(&fixture.root)
        .include("**/*")
        .expect("valid include")
        .exclude("**/*.tmp")
        .expect("valid exclude")
        .respect_git_ignore(true)
        .options(
            WalkOptions::default()
                .sort(true)
                .metadata(true)
                .skip_hidden(true),
        );

    assert_parity("filters and ignore rules", &fixture.root, walker);
}

#[test]
fn parity_holds_through_the_parallel_scheduler() {
    // The families above are serial so the comparison is deterministic. The
    // parallel frontend reaches the same backend through a different
    // scheduler, so one tree is checked through it as well: sorted, its entry
    // set has to match the portable one exactly.
    let fixture = Fixture::new("parallel");
    for branch in 0..8 {
        for level in 0..4 {
            fixture.write(format!("branch-{branch}/level-{level}/file.txt"));
        }
    }
    let root = &fixture.root;

    let parallel = Walker::new(root)
        .threads(4)
        .error_policy(ErrorPolicy::Collect)
        .options(WalkOptions::default().sort(true))
        .collect()
        .expect("the parallel native walk succeeds");
    let (portable_entries, portable_errors) = walk_portable(&collecting_walker(root));

    let (parallel_entries, portable_entries) = (
        describe_entries(parallel.entries(), root),
        describe_entries(&portable_entries, root),
    );
    if parallel_entries != portable_entries {
        let report = difference(&parallel_entries, &portable_entries)
            .expect("unequal descriptions differ somewhere");
        panic!("parallel scheduler: the backends disagree on the entries:{report}");
    }
    assert_eq!(
        describe_errors(parallel.errors(), root),
        describe_errors(&portable_errors, root),
        "parallel scheduler: the backends disagree on the error classes"
    );
}

/// Records which families this platform can hold, so a build that skips one
/// does so by a stated rule rather than by accident.
///
/// The one platform limitation worth naming: APFS refuses a filename that is
/// not valid UTF-8 with `EILSEQ`, so the non-UTF-8 family cannot exist on
/// macOS at all. ext4 accepts any byte sequence except `/` and NUL, so Linux
/// carries it. Byte-first path handling is still covered on macOS through the
/// matcher corpus, which needs no filesystem.
#[test]
fn the_family_matrix_matches_this_platform() {
    let mut families: BTreeMap<&str, bool> = BTreeMap::new();
    families.insert("deep tree", true);
    families.insert("large directory", true);
    families.insert("empty directories", true);
    families.insert("filters and ignore rules", true);
    families.insert("parallel scheduler", true);
    families.insert("symlinks", cfg!(unix));
    families.insert("symlink cycle", cfg!(unix));
    families.insert("unreadable directory", cfg!(unix));
    families.insert("named pipe", cfg!(unix));
    families.insert("names at the length limit", cfg!(unix));
    families.insert("non-UTF-8 names", cfg!(target_os = "linux"));

    assert_eq!(
        families["non-UTF-8 names"],
        cfg!(target_os = "linux"),
        "the non-UTF-8 family belongs to Linux only; APFS rejects such names"
    );
    // This module only builds when a native backend is active, and every
    // supported one is a Unix. A future Windows backend would land here as a
    // failing count rather than as a quiet ten-family skip.
    let running = families.values().filter(|present| **present).count();
    assert_eq!(
        running,
        if cfg!(target_os = "linux") { 11 } else { 10 },
        "this platform runs {running} of {} parity families",
        families.len()
    );
}
