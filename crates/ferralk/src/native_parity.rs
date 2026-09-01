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

    /// Unix-domain socket addresses have a small fixed path buffer. The
    /// ordinary fixture root records a descriptive, collision-resistant name
    /// under the host's temp directory and can exceed that buffer on macOS,
    /// so the socket family uses a deliberately short name in that same
    /// writable directory.
    #[cfg(unix)]
    fn new_short(label: &str) -> Self {
        for _ in 0..1024 {
            let unique = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("f-{label}-{}-{unique}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => return Self { root },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create short parity fixture root: {error}"),
            }
        }
        panic!("allocate a unique short parity fixture root");
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
        open: crate::DirectoryOpen::default(),
        depth: 0,
        root: 0,
        ancestors: crate::AncestorChain::default(),
        ignores,
        ignore_errors,
    };
    state
        .walk_directory(&StdBackend, task)
        .expect("the portable walk collects rather than aborts");
    (state.entries, state.errors)
}

/// Runs the portable backend through the aborting root-error path. Unlike the
/// collecting helper above, this returns the first error directly because
/// [`ErrorPolicy::Abort`] ends the walk at that boundary.
fn walk_portable_abort(walker: &Walker) -> WalkError {
    let mut state = WalkState::new(walker, &crate::keep_every_entry);
    let root = walker
        .roots()
        .next()
        .expect("a walk has a root")
        .to_path_buf();
    let (ignores, ignore_errors) = IgnoreScope::for_root(walker, &StdBackend, &root);
    let task = DirectoryTask {
        path: root,
        open: crate::DirectoryOpen::default(),
        depth: 0,
        root: 0,
        ancestors: crate::AncestorChain::default(),
        ignores,
        ignore_errors,
    };
    state
        .walk_directory(&StdBackend, task)
        .expect_err("an aborting missing root returns its first error")
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
/// The serial collector and stream both run against the native backend; each
/// is compared with one portable collection. This keeps backend differences
/// separate from the frontend while ensuring every fixture reaches both
/// incremental and collected traversal paths.
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

    let mut streamed_entries = Vec::new();
    let mut streamed_errors = Vec::new();
    for item in walker.stream() {
        match item {
            Ok(entry) => streamed_entries.push(entry),
            Err(error) => streamed_errors.push(error),
        }
    }
    let streamed_entries = describe_entries(&streamed_entries, root);
    let streamed_errors = describe_errors(&streamed_errors, root);
    if streamed_entries != portable_entries {
        let report = difference(&streamed_entries, &portable_entries)
            .expect("unequal descriptions differ somewhere");
        panic!("{family}: stream and portable entries disagree:{report}");
    }
    if streamed_errors != portable_errors {
        let report = difference(&streamed_errors, &portable_errors)
            .expect("unequal descriptions differ somewhere");
        panic!("{family}: stream and portable errors disagree:{report}");
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

#[cfg(target_os = "macos")]
#[test]
#[allow(unsafe_code)]
fn parity_beyond_path_max_holds_for_every_frontend() {
    use std::{
        ffi::CString,
        os::unix::{
            ffi::OsStrExt,
            io::{AsRawFd, FromRawFd},
        },
    };

    let fixture = Fixture::new("path-max");
    let mut directory = fixture.root.clone();
    let mut parent = fs::File::open(&fixture.root).expect("open fixture root");
    // Cross PATH_MAX before the retained-descriptor budget can affect the
    // serial frontend. The fixture still needs descriptor-relative creation,
    // because its final pathname is intentionally unusable.
    let name = "d".repeat(200);
    let c_name = CString::new(name.as_bytes()).expect("component contains no NUL");
    while directory.as_os_str().as_bytes().len() <= libc::PATH_MAX as usize + 16 {
        // Building with pathname syscalls would itself fail at PATH_MAX. Keep
        // only the current parent descriptor and create the next component
        // relative to it, the way the native backend does once it retains a
        // directory descriptor.
        let created = unsafe { libc::mkdirat(parent.as_raw_fd(), c_name.as_ptr(), 0o755) };
        assert_eq!(created, 0, "create one level of the long path");
        let child_fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        assert!(child_fd >= 0, "open one level of the long path");
        // SAFETY: openat returned a new owned descriptor on success.
        parent = unsafe { fs::File::from_raw_fd(child_fd) };
        directory.push(&name);
    }

    let walker = collecting_walker(&fixture.root);
    let serial = walker
        .clone()
        .collect()
        .expect("serial walk collects errors");
    let parallel = walker
        .clone()
        .threads(4)
        .collect()
        .expect("parallel walk collects errors");
    let mut streamed_entries = Vec::new();
    let mut streamed_errors = Vec::new();
    for item in walker.clone().stream() {
        match item {
            Ok(entry) => streamed_entries.push(entry),
            Err(error) => streamed_errors.push(error),
        }
    }
    let (_, portable_errors) = walk_portable(&walker);
    let expected_errors = describe_errors(&portable_errors, &fixture.root);

    for (frontend, entries, errors) in [
        ("serial", serial.entries(), serial.errors()),
        ("parallel", parallel.entries(), parallel.errors()),
        (
            "stream",
            streamed_entries.as_slice(),
            streamed_errors.as_slice(),
        ),
    ] {
        assert!(
            entries.iter().all(|entry| {
                entry.path().as_os_str().as_bytes().len() < libc::PATH_MAX as usize
            }),
            "beyond PATH_MAX ({frontend}): emitted an unusable pathname"
        );
        assert_eq!(
            describe_errors(errors, &fixture.root),
            expected_errors,
            "beyond PATH_MAX ({frontend}): error classes differ from portable"
        );
    }
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
    // `mkfifo` is a POSIX-required utility. Treating its absence as a failure
    // keeps this d_type special-file family live on every supported Unix CI
    // runner without widening the crate's audited unsafe surface for a test.
    let created = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("the POSIX mkfifo utility is available");
    assert!(created.success(), "create named pipe");

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
fn parity_over_a_unix_socket() {
    use std::os::unix::net::UnixListener;

    let fixture = Fixture::new_short("socket");
    fixture.write("regular.txt");
    let listener = match UnixListener::bind(fixture.root.join("service.sock")) {
        Ok(listener) => listener,
        // Sandboxed macOS test runners can forbid AF_UNIX outright. This is a
        // capability limitation rather than an absent test dependency; CI
        // records it explicitly while normal Unix runners execute the family.
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            eprintln!("skipping Unix-socket parity: the runner forbids AF_UNIX");
            return;
        }
        Err(error) => panic!("bind Unix socket: {error}"),
    };

    assert_parity(
        "Unix socket",
        &fixture.root,
        collecting_walker(&fixture.root),
    );
    drop(listener);
}

#[test]
fn parity_for_non_directory_and_missing_roots() {
    let fixture = Fixture::new("error-roots");
    let file = fixture.write("regular.txt");
    assert_parity("regular-file root", &file, collecting_walker(&file));

    let missing = fixture.root.join("missing");
    assert_parity("missing root", &missing, collecting_walker(&missing));
}

#[test]
fn parity_for_skip_and_abort_error_policies() {
    let fixture = Fixture::new("error-policies");
    let missing = fixture.root.join("missing");

    // `Skip` retains a root failure while continuing other roots. On this
    // single-root fixture the observable result is therefore the same error
    // channel as collect, and both backends must agree through collect and
    // stream.
    assert_parity(
        "skip-policy missing root",
        &missing,
        Walker::new(&missing)
            .threads(1)
            .error_policy(ErrorPolicy::Skip)
            .options(WalkOptions::default().sort(true)),
    );

    let aborting = Walker::new(&missing)
        .threads(1)
        .error_policy(ErrorPolicy::Abort)
        .options(WalkOptions::default().sort(true));
    let native = aborting
        .clone()
        .collect()
        .expect_err("the native aborting root rejects the missing directory");
    let portable = walk_portable_abort(&aborting);
    assert_eq!(
        (native.operation(), native.source.kind()),
        (portable.operation(), portable.source.kind()),
        "abort-policy missing root: the backends return the same first error"
    );
}

#[cfg(unix)]
#[test]
fn parity_for_a_dangling_symlink_root() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new("dangling-root");
    let dangling = fixture.root.join("dangling");
    symlink("absent-target", &dangling).expect("create dangling symlink root");
    assert_parity(
        "dangling symlink root",
        &dangling,
        collecting_walker(&dangling)
            .options(WalkOptions::default().sort(true).follow_symlinks(true)),
    );
}

#[cfg(target_os = "macos")]
#[test]
fn parity_over_nfc_and_nfd_unicode_names() {
    let fixture = Fixture::new("unicode-normalization");
    fixture.write("caf\u{00e9}.txt");
    fixture.write("cafe\u{301}.txt");
    assert_parity(
        "NFC and NFD Unicode names",
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
    families.insert("beyond PATH_MAX", cfg!(target_os = "macos"));
    families.insert("large directory", true);
    families.insert("empty directories", true);
    families.insert("filters and ignore rules", true);
    families.insert("parallel scheduler", true);
    families.insert("symlinks", cfg!(unix));
    families.insert("symlink cycle", cfg!(unix));
    families.insert("unreadable directory", cfg!(unix));
    families.insert("named pipe", cfg!(unix));
    families.insert("Unix socket", cfg!(unix));
    families.insert("error roots", true);
    families.insert("NFC and NFD Unicode names", cfg!(target_os = "macos"));
    families.insert("names at the length limit", cfg!(unix));
    families.insert("non-UTF-8 names", cfg!(target_os = "linux"));

    assert_eq!(
        families["non-UTF-8 names"],
        cfg!(target_os = "linux"),
        "the non-UTF-8 family belongs to Linux only; APFS rejects such names"
    );
    // This module only builds when a native backend is active, and every
    // supported one is a Unix. Linux carries the non-UTF-8 fixture; macOS
    // carries the PATH_MAX fixture. A future backend would land here as a
    // failing count rather than as a quiet family skip.
    let running = families.values().filter(|present| **present).count();
    assert_eq!(
        running,
        if cfg!(target_os = "macos") { 14 } else { 13 },
        "this platform runs {running} of {} parity families",
        families.len()
    );
}
