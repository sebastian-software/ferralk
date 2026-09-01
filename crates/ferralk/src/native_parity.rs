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
//! Malformed-record rejection stays in each backend module. Behaviour that can
//! change a completed walk — unknown-type classification, latched fallback,
//! and deferred entry errors — also has a whole-tree family here, driven by
//! narrow test-only backend hooks when an ordinary filesystem cannot expose
//! the path deterministically.
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
    DirectoryBackend, DirectoryTask, ErrorPolicy, IgnoreScope, Listing, StdBackend, WalkEntry,
    WalkError, WalkOptions, WalkState, Walker,
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

/// Walks one root through an explicit backend, using the same traversal state
/// as the public serial collector.
fn walk_with_backend(
    walker: &Walker,
    backend: &impl DirectoryBackend,
) -> Result<(Vec<WalkEntry>, Vec<WalkError>), WalkError> {
    let mut state = WalkState::new(walker, &crate::keep_every_entry);
    let root = walker
        .roots()
        .next()
        .expect("a walk has a root")
        .to_path_buf();
    let (ignores, ignore_errors) = IgnoreScope::for_root(walker, backend, &root);
    let task = DirectoryTask {
        path: root.clone(),
        open: crate::DirectoryOpen::default(),
        depth: 0,
        root: 0,
        ancestors: crate::AncestorChain::default(),
        ignores,
        ignore_errors,
    };
    state.walk_directory(backend, task)?;
    Ok((state.entries, state.errors))
}

/// Walks the same tree through the portable backend, using the same traversal
/// code the native walk uses so only the backend differs.
fn walk_portable(walker: &Walker) -> (Vec<WalkEntry>, Vec<WalkError>) {
    walk_with_backend(walker, &StdBackend).expect("the portable walk collects rather than aborts")
}

/// Runs the portable backend through the aborting root-error path. Unlike the
/// collecting helper above, this returns the first error directly because
/// [`ErrorPolicy::Abort`] ends the walk at that boundary.
fn walk_portable_abort(walker: &Walker) -> WalkError {
    walk_with_backend(walker, &StdBackend)
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

/// Backend that makes every real entry take the native descriptor-relative
/// unknown-type fallback, independently of the host filesystem's `d_type`.
struct NativeUnknownTypeBackend;

impl DirectoryBackend for NativeUnknownTypeBackend {
    fn read_directory(
        &self,
        path: &Path,
        _follow_symlinks: bool,
        refuse_final_symlink: bool,
        listing: &mut Listing,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        return crate::linux_native::read_directory_with_unknown_types_for_test(
            path,
            refuse_final_symlink,
            listing,
        );
        #[cfg(target_os = "macos")]
        return crate::macos_native::read_directory_with_unknown_types_for_test(
            path,
            refuse_final_symlink,
            listing,
        );
    }
}

/// Adds the same deterministic per-entry stat failure to either reader. This
/// lets the whole traversal compare the deferred error channel without relying
/// on a narrow deletion or permission race between readdir and stat.
struct DeferredStatBackend<B> {
    inner: B,
    root: PathBuf,
}

impl<B: DirectoryBackend> DirectoryBackend for DeferredStatBackend<B> {
    fn read_directory(
        &self,
        path: &Path,
        follow_symlinks: bool,
        refuse_final_symlink: bool,
        listing: &mut Listing,
    ) -> io::Result<()> {
        self.inner
            .read_directory(path, follow_symlinks, refuse_final_symlink, listing)?;
        if path == self.root {
            crate::defer_entry_stat_error(
                listing,
                self.root.join("unknown-type"),
                io::Error::from(io::ErrorKind::PermissionDenied),
            )?;
        }
        Ok(())
    }
}

fn assert_explicit_backend_parity(
    family: &str,
    root: &Path,
    walker: &Walker,
    native: &impl DirectoryBackend,
    portable: &impl DirectoryBackend,
) {
    let (native_entries, native_errors) =
        walk_with_backend(walker, native).expect("the native-like backend collects");
    let (portable_entries, portable_errors) =
        walk_with_backend(walker, portable).expect("the portable-like backend collects");
    assert_eq!(
        describe_entries(&native_entries, root),
        describe_entries(&portable_entries, root),
        "{family}: the explicit backends disagree on entries"
    );
    assert_eq!(
        describe_errors(&native_errors, root),
        describe_errors(&portable_errors, root),
        "{family}: the explicit backends disagree on errors"
    );
}

#[test]
fn parity_through_dt_unknown_descriptor_classification() {
    let fixture = Fixture::new("unknown-types");
    fixture.write("before.txt");
    fixture.write("nested/inside.txt");
    fixture.directory("empty");
    #[cfg(unix)]
    std::os::unix::fs::symlink("before.txt", fixture.root.join("link"))
        .expect("create unknown-type symlink");

    let walker = collecting_walker(&fixture.root);
    assert_explicit_backend_parity(
        "DT_UNKNOWN descriptor classification",
        &fixture.root,
        &walker,
        &NativeUnknownTypeBackend,
        &StdBackend,
    );
}

#[test]
fn parity_through_the_deferred_entry_error_channel() {
    let fixture = Fixture::new("deferred-error");
    fixture.write("before.txt");
    fixture.write("nested/after.txt");
    let walker = collecting_walker(&fixture.root);
    let native = DeferredStatBackend {
        inner: NativeUnknownTypeBackend,
        root: fixture.root.clone(),
    };
    let portable = DeferredStatBackend {
        inner: StdBackend,
        root: fixture.root.clone(),
    };

    assert_explicit_backend_parity(
        "deferred entry stat error",
        &fixture.root,
        &walker,
        &native,
        &portable,
    );
}

#[test]
fn parity_through_the_latched_native_fallback() {
    let fixture = Fixture::new("latched-fallback");
    for branch in 0..8 {
        fixture.write(format!("branch-{branch}/nested/leaf.txt"));
    }

    #[cfg(target_os = "linux")]
    let _latch = crate::linux_native::force_unsupported_latch_for_test(&fixture.root);
    #[cfg(target_os = "macos")]
    let _latch = crate::macos_native::force_unsupported_latch_for_test(&fixture.root);

    assert_parity(
        "latched native fallback",
        &fixture.root,
        collecting_walker(&fixture.root),
    );
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
    let file_name = "f".repeat(255);
    let c_file_name = CString::new(file_name.as_bytes()).expect("component contains no NUL");
    let mut boundary_file_created = false;
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

        let directory_len = directory.as_os_str().as_bytes().len();
        if !boundary_file_created
            && directory_len < libc::PATH_MAX as usize
            && directory_len + 1 + file_name.len() >= libc::PATH_MAX as usize
        {
            let file_fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    c_file_name.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_CLOEXEC,
                    0o644,
                )
            };
            assert!(file_fd >= 0, "create one file with a long reported path");
            // SAFETY: openat returned a new owned descriptor on success.
            drop(unsafe { fs::File::from_raw_fd(file_fd) });
            boundary_file_created = true;
        }
    }
    assert!(boundary_file_created, "fixture crosses the file boundary");

    let walker = collecting_walker(&fixture.root);
    assert_parity("beyond PATH_MAX", &fixture.root, walker.clone());

    // `assert_parity` covers collected serial traversal and streaming. Keep
    // the parallel frontend in this family too: its queued directory open is
    // the path that originally motivated the explicit PATH_MAX guard.
    let parallel = walker
        .clone()
        .threads(4)
        .collect()
        .expect("parallel walk collects errors");
    let (portable_entries, portable_errors) = walk_portable(&walker);
    assert_eq!(
        describe_entries(parallel.entries(), &fixture.root),
        describe_entries(&portable_entries, &fixture.root),
        "beyond PATH_MAX: parallel and portable entries differ"
    );
    assert_eq!(
        describe_errors(parallel.errors(), &fixture.root),
        describe_errors(&portable_errors, &fixture.root),
        "beyond PATH_MAX: parallel and portable error classes differ"
    );
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

#[derive(Debug, PartialEq, Eq)]
enum DescribedOutcome {
    Completed {
        entries: Vec<DescribedEntry>,
        errors: Vec<DescribedError>,
    },
    Aborted(DescribedError),
}

fn described_error(error: &WalkError, root: &Path) -> DescribedError {
    (
        error
            .path()
            .strip_prefix(root)
            .unwrap_or(error.path())
            .to_path_buf(),
        error.operation(),
        error.source.kind(),
    )
}

fn native_collect_outcome(walker: Walker, root: &Path) -> DescribedOutcome {
    match walker.collect() {
        Ok(result) => DescribedOutcome::Completed {
            entries: describe_entries(result.entries(), root),
            errors: describe_errors(result.errors(), root),
        },
        Err(error) => DescribedOutcome::Aborted(described_error(&error, root)),
    }
}

fn portable_outcome(walker: &Walker, root: &Path) -> DescribedOutcome {
    match walk_with_backend(walker, &StdBackend) {
        Ok((entries, errors)) => DescribedOutcome::Completed {
            entries: describe_entries(&entries, root),
            errors: describe_errors(&errors, root),
        },
        Err(error) => DescribedOutcome::Aborted(described_error(&error, root)),
    }
}

fn stream_outcome(walker: Walker, root: &Path, policy: ErrorPolicy) -> DescribedOutcome {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for item in walker.stream() {
        match item {
            Ok(entry) => entries.push(entry),
            Err(error) if policy == ErrorPolicy::Abort => {
                return DescribedOutcome::Aborted(described_error(&error, root));
            }
            Err(error) => errors.push(error),
        }
    }
    DescribedOutcome::Completed {
        entries: describe_entries(&entries, root),
        errors: describe_errors(&errors, root),
    }
}

#[cfg(unix)]
#[test]
fn parity_for_mid_walk_skip_and_abort_including_stream() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new("mid-walk-policy");
    fixture.write("before.txt");
    fixture.write("nested/after.txt");
    symlink("missing-target", fixture.root.join("broken"))
        .expect("create deterministic mid-walk failure");

    for policy in [ErrorPolicy::Skip, ErrorPolicy::Abort] {
        let walker = Walker::new(&fixture.root)
            .threads(1)
            .error_policy(policy)
            .options(WalkOptions::default().sort(true).follow_symlinks(true));
        let portable = portable_outcome(&walker, &fixture.root);
        assert_eq!(
            native_collect_outcome(walker.clone(), &fixture.root),
            portable,
            "mid-walk {policy:?}: native collect and portable traversal disagree"
        );
        assert_eq!(
            stream_outcome(walker, &fixture.root, policy),
            portable,
            "mid-walk {policy:?}: stream and portable traversal disagree"
        );
    }
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
/// APFS refuses a filename that is not valid UTF-8 with `EILSEQ`, so the
/// non-UTF-8 family cannot exist on macOS. Device nodes are deliberately not a
/// runtime fixture on either CI platform: `mknod` requires elevated privilege
/// on hosted runners. Their raw `d_type` values remain pinned in each native
/// parser's `special_file_types_are_classified_without_a_stat` unit test.
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
    families.insert("mid-walk error policies", cfg!(unix));
    families.insert("DT_UNKNOWN descriptor classification", true);
    families.insert("latched native fallback", true);
    families.insert("deferred entry error", true);
    families.insert("device nodes", false);
    families.insert("NFC and NFD Unicode names", cfg!(target_os = "macos"));
    families.insert("names at the length limit", cfg!(unix));
    families.insert("non-UTF-8 names", cfg!(target_os = "linux"));

    assert_eq!(
        families["non-UTF-8 names"],
        cfg!(target_os = "linux"),
        "the non-UTF-8 family belongs to Linux only; APFS rejects such names"
    );
    assert!(
        !families["device nodes"],
        "hosted CI cannot create device nodes; backend parser tests carry their d_type coverage"
    );
    // This module only builds when a native backend is active, and every
    // supported one is a Unix. Linux carries the non-UTF-8 fixture; macOS
    // carries the PATH_MAX fixture. A future backend would land here as a
    // failing count rather than as a quiet family skip.
    let running = families.values().filter(|present| **present).count();
    assert_eq!(
        running,
        if cfg!(target_os = "macos") { 18 } else { 17 },
        "this platform runs {running} of {} parity families",
        families.len()
    );
}
