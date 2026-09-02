//! Linux `getdents64` directory backend.
//!
//! The syscall and its architecture numbers are confined to this module. The
//! owned buffer remains valid for each call, and every variable-length dirent
//! is validated before its fields or name are read. Architectures without a
//! reviewed syscall number report an unsupported operation so the safe adapter
//! selects the portable reader. A per-thread buffer avoids allocation on every
//! small directory read.

use std::{
    cell::RefCell,
    ffi::{CStr, CString, OsStr, OsString, c_int, c_long, c_void},
    fs::{File, OpenOptions},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawFd, FromRawFd, IntoRawFd},
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard};

use super::{DirectoryIdentity, Listing, defer_entry_stat_error};

const BUFFER_SIZE: usize = 32 * 1024;
const RECORD_LENGTH_OFFSET: usize = 16;
const TYPE_OFFSET: usize = 18;
const NAME_OFFSET: usize = 19;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;
// Types that are neither a directory nor a symlink. Knowing that is the whole
// question here, so these need no stat.
const DT_FIFO: u8 = 1;
const DT_CHR: u8 = 2;
const DT_BLK: u8 = 6;
const DT_SOCK: u8 = 12;

/// Set once the kernel reports that `getdents64` is unavailable.
///
/// Whether the syscall exists is a property of the kernel and the build, not
/// of one directory, so probing again for every directory only pays a failed
/// syscall and a second open. A filesystem-specific refusal is rarer than a
/// missing syscall; per-device memoization would need the device id before the
/// first read, which costs the stat this latch exists to avoid.
static GETDENTS_UNSUPPORTED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
static GETDENTS_LATCH_TEST_LOCK: Mutex<()> = Mutex::new(());
#[cfg(test)]
static FORCED_UNSUPPORTED_ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Restores the test-only, root-scoped latch override after a test, including
/// when the test unwinds. Scoping keeps unrelated native-reader tests on the
/// real backend while this fixture exercises the latched fallback.
#[cfg(test)]
pub(super) struct UnsupportedLatchGuard {
    previous: Option<PathBuf>,
    _lock: MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for UnsupportedLatchGuard {
    fn drop(&mut self) {
        *FORCED_UNSUPPORTED_ROOT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.previous.take();
    }
}

#[cfg(test)]
pub(super) fn force_unsupported_latch_for_test(root: &Path) -> UnsupportedLatchGuard {
    let lock = GETDENTS_LATCH_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = FORCED_UNSUPPORTED_ROOT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .replace(root.to_path_buf());
    UnsupportedLatchGuard {
        previous,
        _lock: lock,
    }
}

#[cfg(test)]
fn forced_unsupported_root_for_test() -> Option<PathBuf> {
    FORCED_UNSUPPORTED_ROOT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn unsupported_is_latched_for(_path: &Path) -> bool {
    if GETDENTS_UNSUPPORTED.load(Ordering::Relaxed) {
        return true;
    }
    #[cfg(test)]
    return FORCED_UNSUPPORTED_ROOT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .is_some_and(|root| _path.starts_with(root));
    #[cfg(not(test))]
    false
}

/// Process-wide ceiling for descriptors retained beyond one directory read.
///
/// Relative opens are an optimization, so reaching the ceiling falls back to
/// the existing full-path open instead of turning a wide tree into `EMFILE`.
const MAX_RETAINED_DIRECTORIES: usize = 256;
static RETAINED_DIRECTORIES: AtomicUsize = AtomicUsize::new(0);
/// `usize::MAX` until the first walk measures it; every walk start measures
/// it again, see [`refresh_retained_directory_limit`].
static RETAINED_DIRECTORY_LIMIT: AtomicUsize = AtomicUsize::new(usize::MAX);

fn retained_directory_limit() -> usize {
    match RETAINED_DIRECTORY_LIMIT.load(Ordering::Relaxed) {
        usize::MAX => refresh_retained_directory_limit(),
        limit => limit,
    }
}

/// Re-derives the retention ceiling from the `RLIMIT_NOFILE` in force now.
///
/// Every walk start calls this. A process that lowers its descriptor limit
/// between walks, as container runtimes and daemon setups do, must not keep
/// the ceiling an earlier walk measured: retaining a quarter of a limit that
/// no longer applies turns scheduled opens into `EMFILE` errors instead of
/// full-path fallbacks.
pub(super) fn refresh_retained_directory_limit() -> usize {
    let limit = measure_retained_directory_limit();
    RETAINED_DIRECTORY_LIMIT.store(limit, Ordering::Relaxed);
    limit
}

fn measure_retained_directory_limit() -> usize {
    let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: `limits` points at writable storage for one `rlimit`; a
    // successful call initializes it before `assume_init` below.
    let status = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limits.as_mut_ptr()) };
    if status != 0 {
        return 64;
    }
    // SAFETY: `getrlimit` returned success and initialized the output.
    let soft_limit = unsafe { limits.assume_init() }.rlim_cur;
    usize::try_from(soft_limit)
        .unwrap_or(MAX_RETAINED_DIRECTORIES)
        .saturating_div(4)
        .min(MAX_RETAINED_DIRECTORIES)
}

/// Whether a serial frame that is being suspended should release its
/// descriptor instead of keeping it across its subtree.
///
/// Pinning one descriptor per suspended ancestor costs nothing on the wide
/// trees that dominate real walks, and it spares the resumed frame a
/// full-path open and two identity checks. Past the release threshold the
/// directories below and the other walkers sharing the budget would soon be
/// denied relative opens, so from there on suspension releases the descriptor
/// and resumption reacquires it through a verified full-path open.
fn retention_under_pressure() -> bool {
    #[cfg(test)]
    if let Some(under_pressure) = super::retained_directory_test::under_pressure() {
        return under_pressure;
    }
    RETAINED_DIRECTORIES.load(Ordering::Acquire)
        >= super::retention_release_threshold(retained_directory_limit())
}

/// Parent capability and basename retained by a queued child directory.
#[derive(Debug, Clone)]
pub(super) struct RelativeDirectoryOpen {
    pub(super) parent: Arc<RetainedDirectory>,
    pub(super) name: OsString,
}

#[derive(Debug)]
enum RetentionPermit {
    /// Counted against the process-wide budget.
    Budgeted,
    /// A resumed serial frame's verified descriptor, kept when the budget
    /// denied a permit. A serial walk holds at most one of these at a time,
    /// and the frame's next suspension or completion releases it.
    Unbudgeted,
    #[cfg(test)]
    Test,
}

#[derive(Debug)]
pub(super) struct RetainedDirectory(File, RetentionPermit);

impl RetainedDirectory {
    /// Takes one permit from the budget for `directory`, or hands the
    /// descriptor back when the budget is exhausted so the caller decides
    /// whether to keep it anyway.
    fn retain(directory: File) -> Result<Arc<Self>, File> {
        #[cfg(test)]
        if let Some(granted) = super::retained_directory_test::try_acquire() {
            return if granted {
                Ok(Arc::new(Self(directory, RetentionPermit::Test)))
            } else {
                Err(directory)
            };
        }
        let permit =
            RETAINED_DIRECTORIES.fetch_update(Ordering::AcqRel, Ordering::Acquire, |retained| {
                (retained < retained_directory_limit()).then_some(retained + 1)
            });
        match permit {
            Ok(_) => Ok(Arc::new(Self(directory, RetentionPermit::Budgeted))),
            Err(_) => Err(directory),
        }
    }

    /// Keeps a verified descriptor outside the budget.
    fn unbudgeted(directory: File) -> Arc<Self> {
        Arc::new(Self(directory, RetentionPermit::Unbudgeted))
    }

    const fn is_budgeted(&self) -> bool {
        !matches!(self.1, RetentionPermit::Unbudgeted)
    }
}

impl Drop for RetainedDirectory {
    fn drop(&mut self) {
        match self.1 {
            RetentionPermit::Budgeted => {
                RETAINED_DIRECTORIES.fetch_sub(1, Ordering::AcqRel);
            }
            RetentionPermit::Unbudgeted => {}
            #[cfg(test)]
            RetentionPermit::Test => super::retained_directory_test::release(),
        }
    }
}

std::thread_local! {
    static DIRECTORY_BUFFER: RefCell<Box<[u8; BUFFER_SIZE]>> = RefCell::new(Box::new([0; BUFFER_SIZE]));
}

#[cfg(all(target_arch = "x86_64", target_pointer_width = "64"))]
const SYS_GETDENTS64: c_long = 217;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
const SYS_GETDENTS64: c_long = 61;
#[cfg(target_arch = "x86")]
const SYS_GETDENTS64: c_long = 220;
#[cfg(target_arch = "arm")]
const SYS_GETDENTS64: c_long = 217;

unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
}

pub(super) fn read_directory(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
    listing: &mut Listing,
) -> io::Result<()> {
    if unsupported_is_latched_for(path) {
        return read_portable_directory_from_path(path, relative, refuse_final_symlink, listing);
    }
    let used_portable_fallback = DIRECTORY_BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        read_open_directory_with_portable_fallback(
            path,
            relative,
            refuse_final_symlink,
            &mut buffer[..],
            listing,
            read_directory_from_open_directory,
        )
    });
    if used_portable_fallback? {
        GETDENTS_UNSUPPORTED.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Opens a directory for reading, refusing anything that is not one.
///
/// `O_DIRECTORY` makes the open fail with `ENOTDIR` instead of blocking when
/// the scheduled path was replaced by a FIFO between scheduling and open.
/// For a scheduled non-root directory in a no-follow walk, `O_NOFOLLOW` makes
/// the final component check and open one operation. A directory exchanged for
/// a symlink is therefore refused (`ELOOP` or `ENOTDIR`, according to the
/// kernel's flag handling) instead of escaping through that link. User-supplied
/// symlink roots retain portable semantics.
fn open_directory(path: &Path, refuse_final_symlink: bool) -> io::Result<File> {
    let flags = libc::O_DIRECTORY
        | if refuse_final_symlink {
            libc::O_NOFOLLOW
        } else {
            0
        };
    OpenOptions::new().read(true).custom_flags(flags).open(path)
}

fn file_identity(directory: &File) -> io::Result<DirectoryIdentity> {
    let metadata = directory.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

/// Releases a serial frame's descriptor at suspension when keeping it would
/// crowd the retention budget, returning the identity that
/// [`restore_retained_directory`] must find again. `None` keeps the
/// descriptor in the frame, which makes its resumption free.
pub(super) fn suspend_retained_directory(listing: &mut Listing) -> Option<DirectoryIdentity> {
    let directory = listing.native_directory.as_ref()?;
    if directory.is_budgeted() && !retention_under_pressure() {
        return None;
    }
    // If identity capture fails, keep the rare descriptor rather than
    // reopening an identity that could not be verified later.
    let identity = file_identity(&directory.0).ok()?;
    listing.native_directory = None;
    Some(identity)
}

/// Reacquires a serial frame's released descriptor for the rest of its
/// cached listing.
///
/// The path is opened again and must reach the identity recorded at
/// suspension. A changed identity means the cached names belong to a
/// directory this path no longer reaches, so the remaining entries are
/// reported lost instead of being resolved below the replacement. A
/// retention-budget denial keeps the verified descriptor outside the budget:
/// it is exactly the capability whose loss would force mutable full paths.
pub(super) fn restore_retained_directory(
    path: &Path,
    expected: Option<DirectoryIdentity>,
    refuse_final_symlink: bool,
    listing: &mut Listing,
) -> io::Result<()> {
    if listing.native_directory.is_some() {
        return Ok(());
    }
    let Some(expected) = expected else {
        return Ok(());
    };
    let directory = open_directory(path, refuse_final_symlink)?;
    if file_identity(&directory)? != expected {
        return Err(super::directory_replaced_while_suspended());
    }
    listing.native_directory =
        Some(RetainedDirectory::retain(directory).unwrap_or_else(RetainedDirectory::unbudgeted));
    Ok(())
}

/// Opens a queued child relative to the still-open parent that named it.
fn open_scheduled_directory(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
) -> io::Result<File> {
    // Relative `openat` can reach a directory whose reported path has already
    // crossed Linux's path-only limit. Do not let retained descriptors make
    // that an accidental frontend- or RLIMIT-dependent extension: callers
    // receive the full path and their later path-based syscalls cannot use it
    // either. Keep the portable ENAMETOOLONG boundary for every frontend.
    if path.as_os_str().as_bytes().len() >= libc::PATH_MAX as usize {
        return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    let Some(relative) = relative else {
        return open_directory(path, refuse_final_symlink);
    };
    let name = CString::new(relative.name.as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_DIRECTORY
        | if refuse_final_symlink {
            libc::O_NOFOLLOW
        } else {
            0
        };
    // SAFETY: the parent capability keeps a live directory descriptor for the
    // duration of the call; `name` is NUL-terminated and has no interior NUL.
    // No creation flag is present, so `openat` takes no mode argument.
    let descriptor = unsafe { libc::openat(relative.parent.0.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a non-negative `openat` result is a new owned descriptor. This
    // `File` is its sole owner and closes it exactly once.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

/// Reads a protected open directory with `getdents64`, falling back through
/// that same descriptor only when the syscall itself is unavailable.
///
/// Keeping this decision next to the descriptor is essential. The generic
/// backend adapter only has `path`, which is no longer safe to reopen for a
/// scheduled no-follow descendant after a replacement race.
fn read_open_directory_with_portable_fallback(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
    buffer: &mut [u8],
    listing: &mut Listing,
    native: impl FnOnce(&File, &Path, &mut [u8], &mut Listing) -> Result<(), NativeDirectoryReadError>,
) -> io::Result<bool> {
    let directory = open_scheduled_directory(path, relative, refuse_final_symlink)?;
    match native(&directory, path, buffer, listing) {
        Ok(()) => {
            listing.native_directory = RetainedDirectory::retain(directory).ok();
            Ok(false)
        }
        Err(NativeDirectoryReadError::CapabilityUnavailable) => {
            read_portable_directory_from_open_file(directory, path, listing)?;
            Ok(true)
        }
        Err(NativeDirectoryReadError::Io(error)) => Err(error),
    }
}

/// Reads one already-open directory through the raw syscall.
///
/// Only an unavailable syscall is a capability result. In particular, an
/// `Unsupported` metadata error after a batch is an ordinary error: attempting
/// to resume its advanced descriptor through `readdir` would lose entries.
fn read_directory_from_open_directory(
    directory: &File,
    path: &Path,
    buffer: &mut [u8],
    listing: &mut Listing,
) -> Result<(), NativeDirectoryReadError> {
    read_directory_from_open_directory_with_read_batch(directory, path, buffer, listing, read_batch)
}

/// Reads one already-open directory through an injected batch source.
///
/// The production reader supplies the raw syscall. Keeping the source as an
/// argument makes the distinction between an unavailable syscall and an
/// ordinary `Unsupported` error testable without changing production state.
fn read_directory_from_open_directory_with_read_batch(
    directory: &File,
    path: &Path,
    buffer: &mut [u8],
    listing: &mut Listing,
    mut next_batch: impl FnMut(&File, &mut [u8]) -> Result<usize, ReadBatchError>,
) -> Result<(), NativeDirectoryReadError> {
    let mut primed = false;
    listing.clear();
    loop {
        let byte_count = match next_batch(directory, buffer) {
            Ok(byte_count) => byte_count,
            // A reviewed architecture without a syscall number can only be
            // known before any read. It is a capability result rather than an
            // ordinary directory error.
            Err(ReadBatchError::CapabilityUnavailable) => {
                return Err(NativeDirectoryReadError::CapabilityUnavailable);
            }
            // Only an actual ENOSYS before the first accepted batch proves
            // that `getdents64` is unavailable. `ErrorKind::Unsupported`
            // also covers ordinary errors such as EOPNOTSUPP, and any error
            // after a batch must retain the advanced descriptor and listing.
            Err(ReadBatchError::Io(error))
                if !primed && error.raw_os_error() == Some(libc::ENOSYS) =>
            {
                return Err(NativeDirectoryReadError::CapabilityUnavailable);
            }
            Err(ReadBatchError::Io(error)) => return Err(NativeDirectoryReadError::Io(error)),
        };
        if byte_count == 0 {
            return Ok(());
        }
        parse_records_from_open_directory(directory, path, &buffer[..byte_count], listing)?;
        primed = true;
    }
}

/// Distinguishes an unavailable syscall from an ordinary error after the
/// directory stream has started advancing.
enum NativeDirectoryReadError {
    CapabilityUnavailable,
    Io(io::Error),
}

impl From<io::Error> for NativeDirectoryReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The raw batch source distinguishes a compile-time unavailable syscall from
/// a runtime I/O failure. Runtime ENOSYS is deliberately preserved as an I/O
/// error here so the caller can apply its pre-first-batch capability rule.
#[cfg_attr(
    any(
        all(target_arch = "x86_64", target_pointer_width = "64"),
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "x86",
        target_arch = "arm"
    ),
    allow(dead_code)
)]
enum ReadBatchError {
    CapabilityUnavailable,
    Io(io::Error),
}

/// Opens a directory once with the native walk's flags, then enumerates that
/// exact descriptor through libc's public portable API.
fn read_portable_directory_from_path(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
    listing: &mut Listing,
) -> io::Result<()> {
    let directory = open_scheduled_directory(path, relative, refuse_final_symlink)?;
    read_portable_directory_from_open_file(directory, path, listing)
}

/// Enumerates exactly the directory an open `File` already refers to.
///
/// `fdopendir` takes ownership of the descriptor on success. `readdir` and
/// the rare `fstatat` fallback are relative to that descriptor, so a no-follow
/// descendant is never reopened by path.
fn read_portable_directory_from_open_file(
    directory: File,
    reported_path: &Path,
    listing: &mut Listing,
) -> io::Result<()> {
    let descriptor = directory.into_raw_fd();
    // SAFETY: `descriptor` is a live directory descriptor transferred from
    // `directory`. On success `fdopendir` owns it; on failure the `File`
    // reconstruction below remains the sole owner and closes it once.
    let stream = unsafe { libc::fdopendir(descriptor) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: `fdopendir` did not take ownership on failure.
        unsafe { drop(File::from_raw_fd(descriptor)) };
        return Err(error);
    }
    let stream = DirectoryStream(stream);
    listing.clear();
    loop {
        // SAFETY: Linux exposes this thread's writable errno slot. Clearing
        // it distinguishes `readdir` EOF from a failed directory read.
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: `stream` owns this valid `DIR` until Drop calls `closedir`;
        // its returned record is consumed before the next `readdir` call.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(error)
            };
        }
        // SAFETY: a successful `readdir` returns a valid, NUL-terminated
        // Linux `dirent` name; its storage remains live until the next call.
        let entry = unsafe { &*entry };
        let name = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) }.to_bytes();
        if name.is_empty() || name == b"." || name == b".." {
            continue;
        }
        let name = OsStr::from_bytes(name);
        match descriptor_entry_kind(descriptor, name, entry.d_type) {
            Ok(Some((is_dir, is_symlink))) => listing.push(name, is_dir, is_symlink),
            Ok(None) => {}
            Err(error) => defer_entry_stat_error(listing, reported_path.join(name), error)?,
        }
    }
}

/// Owns the `DIR` returned by `fdopendir`, including its descriptor.
struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this is the one `closedir` corresponding to successful
        // `fdopendir`; libc closes the transferred descriptor with the stream.
        unsafe { libc::closedir(self.0) };
    }
}

/// Resolves a `DT_UNKNOWN` entry relative to the protected descriptor.
fn descriptor_entry_kind(
    directory: c_int,
    name: &OsStr,
    directory_type: u8,
) -> io::Result<Option<(bool, bool)>> {
    match directory_type {
        DT_DIR => Ok(Some((true, false))),
        DT_REG | DT_FIFO | DT_CHR | DT_BLK | DT_SOCK => Ok(Some((false, false))),
        DT_LNK => Ok(Some((false, true))),
        _ => {
            let name = CString::new(name.as_bytes()).map_err(|_| malformed_record())?;
            // SAFETY: output storage is valid, `directory` is held live by
            // `DirectoryStream`, and `name` is NUL-terminated for this call.
            let mut metadata = unsafe { std::mem::zeroed::<libc::stat>() };
            let result = unsafe {
                libc::fstatat(
                    directory,
                    name.as_ptr(),
                    &mut metadata,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result != 0 {
                let error = io::Error::last_os_error();
                return if is_vanished_entry(&error) {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            let kind = metadata.st_mode & libc::S_IFMT;
            Ok(Some((kind == libc::S_IFDIR, kind == libc::S_IFLNK)))
        }
    }
}

/// Builds a real directory listing while forcing every entry through the
/// descriptor-relative `DT_UNKNOWN` classifier. Filesystems used in CI
/// normally provide `d_type`, so the differential parity suite needs this
/// test-only entry point to exercise the fallback deterministically.
#[cfg(test)]
pub(super) fn read_directory_with_unknown_types_for_test(
    path: &Path,
    refuse_final_symlink: bool,
    listing: &mut Listing,
) -> io::Result<()> {
    let directory = open_directory(path, refuse_final_symlink)?;
    listing.clear();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        match descriptor_entry_kind(directory.as_raw_fd(), &name, 0) {
            Ok(Some((is_dir, is_symlink))) => listing.push(&name, is_dir, is_symlink),
            Ok(None) => {}
            Err(error) => defer_entry_stat_error(listing, path.join(&name), error)?,
        }
    }
    Ok(())
}

fn read_batch(directory: &File, buffer: &mut [u8]) -> Result<usize, ReadBatchError> {
    #[cfg(any(
        all(target_arch = "x86_64", target_pointer_width = "64"),
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "x86",
        target_arch = "arm"
    ))]
    loop {
        // SAFETY: `buffer` is owned, writable, and remains live for the call;
        // its length is supplied unchanged. `directory` owns the open file
        // descriptor for the call, and `SYS_GETDENTS64` is reviewed above for
        // the current target architecture.
        let byte_count = unsafe {
            syscall(
                SYS_GETDENTS64,
                directory.as_raw_fd(),
                buffer.as_mut_ptr().cast::<c_void>(),
                buffer.len(),
            )
        };
        if byte_count >= 0 {
            return Ok(byte_count as usize);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(ReadBatchError::Io(error));
    }

    #[cfg(not(any(
        all(target_arch = "x86_64", target_pointer_width = "64"),
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "x86",
        target_arch = "arm"
    )))]
    {
        let _ = (directory, buffer);
        Err(ReadBatchError::CapabilityUnavailable)
    }
}

/// Classifies raw records through the descriptor that was opened for this
/// listing, so `DT_UNKNOWN` never re-resolves a raced path by name.
fn parse_records_from_open_directory(
    open_directory: &File,
    reported_path: &Path,
    records: &[u8],
    listing: &mut Listing,
) -> io::Result<()> {
    parse_records_with_entry_kind(
        reported_path,
        records,
        listing,
        |_, name, directory_type| {
            descriptor_entry_kind(open_directory.as_raw_fd(), name, directory_type)
        },
    )
}

#[cfg(test)]
fn parse_records(directory: &Path, records: &[u8], listing: &mut Listing) -> io::Result<()> {
    parse_records_with_entry_kind(directory, records, listing, entry_kind)
}

fn parse_records_with_entry_kind(
    directory: &Path,
    records: &[u8],
    listing: &mut Listing,
    mut classify: impl FnMut(&Path, &OsStr, u8) -> io::Result<Option<(bool, bool)>>,
) -> io::Result<()> {
    for_each_record(records, |name, directory_type| {
        let name = OsStr::from_bytes(name);
        // An entry that vanished between the read and its stat costs that one
        // entry; the rest of the listing is still valid and is returned.
        match classify(directory, name, directory_type) {
            Ok(Some((is_dir, is_symlink))) => listing.push(name, is_dir, is_symlink),
            Ok(None) => {}
            Err(error) => defer_entry_stat_error(listing, directory.join(name), error)?,
        }
        Ok(())
    })
}

/// Validates raw Linux dirent records without touching the filesystem.
///
/// This is exposed only for the feature-gated cargo-fuzz target; normal walker
/// callers never observe raw records.
#[doc(hidden)]
pub fn fuzz_validate_records(records: &[u8]) {
    let _ = for_each_record(records, |_, _| Ok(()));
}

fn for_each_record(
    records: &[u8],
    mut visit: impl FnMut(&[u8], u8) -> io::Result<()>,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < records.len() {
        let record = records.get(offset..).ok_or_else(malformed_record)?;
        if record.len() < NAME_OFFSET + 1 {
            return Err(malformed_record());
        }
        let record_length = u16::from_ne_bytes(
            record[RECORD_LENGTH_OFFSET..RECORD_LENGTH_OFFSET + std::mem::size_of::<u16>()]
                .try_into()
                .expect("record length slice has fixed width"),
        ) as usize;
        if record_length < NAME_OFFSET + 1 || record_length > record.len() {
            return Err(malformed_record());
        }
        // `d_type` immediately precedes `d_name`; any alignment padding is at
        // the end of the record, so a valid terminating NUL may be its final
        // byte. Search the entire name-and-padding region.
        let name_and_padding = &record[NAME_OFFSET..record_length];
        // Both scans run for every entry of every directory the walk reads, so
        // they use the vectorised search rather than a byte-at-a-time loop.
        let Some(name_length) = memchr::memchr(0, name_and_padding) else {
            return Err(malformed_record());
        };
        let name = &name_and_padding[..name_length];
        offset = offset
            .checked_add(record_length)
            .ok_or_else(malformed_record)?;
        if name.is_empty() || name == b"." || name == b".." {
            continue;
        }
        if memchr::memchr(b'/', name).is_some() {
            return Err(malformed_record());
        }
        visit(name, record[TYPE_OFFSET])?;
    }
    Ok(())
}

/// Classifies one entry, or reports that it no longer exists.
///
/// `Ok(None)` means the entry disappeared between the directory read and its
/// stat. Persistent failures use the listing-level error channel instead.
#[cfg(test)]
fn entry_kind(
    directory: &Path,
    name: &OsStr,
    directory_type: u8,
) -> io::Result<Option<(bool, bool)>> {
    match directory_type {
        DT_DIR => Ok(Some((true, false))),
        DT_REG | DT_FIFO | DT_CHR | DT_BLK | DT_SOCK => Ok(Some((false, false))),
        DT_LNK => Ok(Some((false, true))),
        // `DT_UNKNOWN`, and any type this build does not name, need one stat,
        // and a whole path to stat with. Building one here is what keeps the
        // common cases above from needing it.
        _ => match fs::symlink_metadata(directory.join(name)) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                Ok(Some((file_type.is_dir(), file_type.is_symlink())))
            }
            Err(error) if is_vanished_entry(&error) => Ok(None),
            Err(error) => Err(error),
        },
    }
}

/// Whether a per-entry stat failure describes an entry that vanished after its
/// directory record was read. `NotADirectory` is also a replacement race: a
/// parent component that was a directory during listing is no longer one.
fn is_vanished_entry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn malformed_record() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed getdents64 record")
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::{OsStr, OsString},
        fs,
        os::unix::{
            fs::symlink,
            io::{AsRawFd, FromRawFd},
        },
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::{DirectoryBackend, ErrorPolicy, StdBackend, WalkEntry, WalkOptions, Walker};

    use super::{
        BUFFER_SIZE, DT_BLK, DT_CHR, DT_DIR, DT_FIFO, DT_REG, DT_SOCK, Listing, NAME_OFFSET,
        NativeDirectoryReadError, ReadBatchError, RelativeDirectoryOpen, RetainedDirectory,
        TYPE_OFFSET, entry_kind, for_each_record, force_unsupported_latch_for_test,
        forced_unsupported_root_for_test, open_directory, parse_records,
        parse_records_from_open_directory, read_directory,
        read_directory_from_open_directory_with_read_batch,
        read_open_directory_with_portable_fallback, read_portable_directory_from_open_file,
        unsupported_is_latched_for,
    };

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    fn record(name: &[u8], directory_type: u8) -> Vec<u8> {
        let length = (NAME_OFFSET + name.len() + 1 + 7) & !7;
        let mut record = vec![0_u8; length];
        record[16..18].copy_from_slice(&(length as u16).to_ne_bytes());
        record[NAME_OFFSET..NAME_OFFSET + name.len()].copy_from_slice(name);
        record[TYPE_OFFSET] = directory_type;
        record
    }

    #[test]
    fn parser_skips_dot_entries_and_rejects_malformed_records() {
        let mut records = record(b".", DT_DIR);
        records.extend(record(b"..", DT_DIR));
        records.extend(record(b"regular", DT_REG));
        let mut listing = Listing::default();
        parse_records(Path::new("/tmp"), &records, &mut listing).expect("dot records parse");
        assert_eq!(listing.entries().len(), 1);
        assert!(!listing.entries()[0].is_dir());
        assert!(!listing.entries()[0].is_symlink());

        assert!(parse_records(Path::new("/tmp"), &[0_u8; NAME_OFFSET], &mut listing).is_err());

        let mut zero_length = vec![0_u8; NAME_OFFSET + 1];
        zero_length[NAME_OFFSET] = DT_REG;
        assert!(parse_records(Path::new("/tmp"), &zero_length, &mut listing).is_err());

        let mut missing_nul = record(b"name", DT_REG);
        for byte in &mut missing_nul[NAME_OFFSET..] {
            *byte = b'x';
        }
        assert!(parse_records(Path::new("/tmp"), &missing_nul, &mut listing).is_err());
    }

    // Linux's dirent wire fields use host endianness. The reviewed native
    // syscall targets and checked-in corpus are little-endian; a big-endian
    // port needs its own reviewed seed corpus rather than silently accepting
    // byte sequences this parser would not read as records.
    #[cfg(target_endian = "little")]
    #[test]
    fn checked_in_linux_native_fuzz_seeds_are_valid_and_reproducible() {
        let long_name = [b'x'; 255];
        let mut multi = record(b"nested", DT_DIR);
        multi.extend(record(b"file", DT_REG));
        let seeds: [(&[u8], Vec<u8>); 4] = [
            (
                include_bytes!("../../../fuzz/corpus/linux_dirent_parser/single-regular"),
                record(b"one", DT_REG),
            ),
            (
                include_bytes!("../../../fuzz/corpus/linux_dirent_parser/minimal-name"),
                record(b"a", DT_REG),
            ),
            (
                include_bytes!("../../../fuzz/corpus/linux_dirent_parser/multi-directory-regular"),
                multi,
            ),
            (
                include_bytes!("../../../fuzz/corpus/linux_dirent_parser/long-name"),
                record(&long_name, DT_REG),
            ),
        ];
        for (seed, expected) in seeds {
            assert_eq!(
                seed,
                expected.as_slice(),
                "seed matches the generator record"
            );
            let mut records = 0;
            for_each_record(seed, |_, _| {
                records += 1;
                Ok(())
            })
            .expect("seed reaches the parser visitor");
            assert!(records > 0);
        }
    }

    #[test]
    fn a_vanished_entry_costs_one_entry_and_not_the_listing() {
        let missing = format!(
            "ferralk-vanished-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        );
        // `DT_UNKNOWN` forces the stat that races with the deletion.
        let mut records = record(missing.as_bytes(), 0);
        records.extend(record(b"survivor", DT_REG));
        let mut listing = Listing::default();

        parse_records(Path::new("/tmp"), &records, &mut listing)
            .expect("a vanished entry does not end the listing");

        assert_eq!(
            listing.entries().len(),
            1,
            "only the vanished entry is dropped"
        );
        assert_eq!(listing.entries()[0].name(), "survivor");
    }

    #[test]
    fn special_file_types_are_classified_without_a_stat() {
        // The path does not exist, so any stat would fail. Reaching a verdict
        // proves these types are decided from the record alone.
        let absent = Path::new("/ferralk-nonexistent-special");
        for directory_type in [DT_FIFO, DT_CHR, DT_BLK, DT_SOCK] {
            assert_eq!(
                entry_kind(absent, "entry".as_ref(), directory_type).expect("no stat is attempted"),
                Some((false, false))
            );
        }
    }

    #[test]
    fn opening_a_non_directory_fails_instead_of_blocking() {
        // Without `O_DIRECTORY` opening a FIFO blocks until a writer appears,
        // which hangs the worker when a scheduled directory is swapped for
        // one. A regular file shows the same refusal without needing mkfifo.
        let path = std::env::temp_dir().join(format!(
            "ferralk-not-a-directory-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, b"fixture").expect("write fixture file");
        let error = open_directory(&path, false).expect_err("a regular file is not a directory");
        assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn no_follow_directory_open_rejects_a_swapped_symlink_but_follow_opens_it() {
        let root = fixture_root("no-follow");
        let target = root.join("target");
        let link = root.join("scheduled");
        fs::create_dir_all(&target).expect("create target directory");
        fs::write(target.join("inside"), b"fixture").expect("write target entry");
        symlink(&target, &link).expect("replace scheduled directory with a link");

        let error = open_directory(&link, true).expect_err("no-follow rejects the replacement");
        assert!(matches!(
            error.raw_os_error(),
            Some(libc::ELOOP | libc::ENOTDIR)
        ));
        let mut listing = Listing::default();
        read_directory(&link, None, false, &mut listing)
            .expect("follow mode still opens through a link");
        assert!(listing.contains("inside"));
        fs::remove_dir_all(root).expect("remove no-follow fixture");
    }

    #[test]
    fn relative_child_open_stays_with_the_parent_descriptor_after_path_replacement() {
        let root = fixture_root("relative-open-parent-swap");
        let parent = root.join("parent");
        let moved = root.join("opened-parent");
        fs::create_dir_all(parent.join("child")).expect("create original child");
        fs::write(parent.join("child/original"), b"fixture").expect("write original marker");

        let relative = RelativeDirectoryOpen {
            parent: RetainedDirectory::retain(
                open_directory(&parent, false).expect("open original parent"),
            )
            .expect("the test retains one descriptor"),
            name: OsString::from("child"),
        };
        fs::rename(&parent, &moved).expect("move the opened parent");
        fs::create_dir_all(parent.join("child")).expect("create replacement child");
        fs::write(parent.join("child/escaped"), b"fixture").expect("write replacement marker");

        let mut listing = Listing::default();
        read_directory(&parent.join("child"), Some(&relative), true, &mut listing)
            .expect("open child relative to retained parent");
        assert!(listing.contains("original"));
        assert!(!listing.contains("escaped"));
        fs::remove_dir_all(root).expect("remove relative-open fixture");
    }

    #[test]
    fn path_dependent_modes_do_not_receive_a_relative_open_capability() {
        let root = fixture_root("relative-open-policy");
        fs::create_dir_all(root.join("child")).expect("create child directory");
        let listing = Listing {
            native_directory: RetainedDirectory::retain(
                open_directory(&root, false).expect("open fixture root"),
            )
            .ok(),
            ..Listing::default()
        };

        let relative =
            crate::SystemBackend.child_directory_open(&listing, OsStr::new("child"), true);
        assert!(matches!(relative, crate::DirectoryOpen::LinuxRelative(_)));
        let path_based =
            crate::SystemBackend.child_directory_open(&listing, OsStr::new("child"), false);
        assert!(matches!(path_based, crate::DirectoryOpen::None));
        fs::remove_dir_all(root).expect("remove relative-open policy fixture");
    }

    #[test]
    fn unknown_records_stay_with_the_opened_directory_after_a_path_swap() {
        let root = fixture_root("unknown-descriptor");
        let listed = root.join("listed");
        let moved = root.join("opened-before-swap");
        let target = root.join("attacker");
        fs::create_dir_all(&listed).expect("create listed directory");
        fs::write(listed.join("probe"), b"fixture").expect("create original file");
        fs::create_dir_all(target.join("probe")).expect("create swapped directory");
        let open = open_directory(&listed, true).expect("open listed directory");
        let records = record(b"probe", 0);

        fs::rename(&listed, &moved).expect("move opened directory");
        symlink(&target, &listed).expect("replace listed path with a link");

        let mut listing = Listing::default();
        parse_records_from_open_directory(&open, &listed, &records, &mut listing)
            .expect("unknown record is classified through the open descriptor");
        assert_eq!(listing.entries().len(), 1);
        assert!(
            !listing.entries()[0].is_dir(),
            "the original file wins over the replacement directory"
        );
        fs::remove_dir_all(root).expect("remove descriptor fixture");
    }

    #[test]
    fn unsupported_no_follow_descendant_reuses_its_open_descriptor() {
        let root = fixture_root("descriptor-fallback");
        let descendant = root.join("scheduled");
        let moved = root.join("opened-before-swap");
        let target = root.join("target");
        fs::create_dir_all(descendant.join("nested")).expect("create scheduled subtree");
        fs::write(descendant.join("nested/inside"), b"fixture").expect("write nested entry");
        fs::create_dir_all(&target).expect("create swapped target");
        fs::write(target.join("escaped"), b"fixture").expect("write target entry");

        let mut buffer = vec![0_u8; BUFFER_SIZE];
        let mut listing = Listing::default();
        let used_portable_fallback = read_open_directory_with_portable_fallback(
            &descendant,
            None,
            true,
            &mut buffer,
            &mut listing,
            |_, _, _, _| {
                // The native path has already opened `descendant` with
                // O_NOFOLLOW. A fallback that reopened its name after this
                // replacement would enumerate `target` instead.
                fs::rename(&descendant, &moved).expect("move opened directory");
                symlink(&target, &descendant).expect("replace path with a link");
                Err(NativeDirectoryReadError::CapabilityUnavailable)
            },
        )
        .expect("unsupported native read degrades through the descriptor");

        assert!(used_portable_fallback);
        assert!(
            listing.contains("nested"),
            "the opened descendant remains visible"
        );
        assert!(
            !listing.contains("escaped"),
            "the portable fallback never reopens the swapped path"
        );
        fs::remove_dir_all(root).expect("remove descriptor fallback fixture");
    }

    #[test]
    fn only_pre_batch_enosys_uses_the_portable_descriptor_fallback() {
        let root = fixture_root("capability-classification");
        fs::create_dir_all(&root).expect("create fixture directory");
        fs::write(root.join("portable-entry"), b"fixture").expect("write portable marker");
        let mut buffer = vec![0_u8; BUFFER_SIZE];

        let mut listing = Listing::default();
        let used_portable_fallback = read_open_directory_with_portable_fallback(
            &root,
            None,
            true,
            &mut buffer,
            &mut listing,
            |directory, path, buffer, listing| {
                read_directory_from_open_directory_with_read_batch(
                    directory,
                    path,
                    buffer,
                    listing,
                    |_, _| {
                        Err(ReadBatchError::Io(std::io::Error::from_raw_os_error(
                            libc::ENOSYS,
                        )))
                    },
                )
            },
        )
        .expect("pre-first-batch ENOSYS is a capability fallback");
        assert!(used_portable_fallback);
        assert!(listing.contains("portable-entry"));

        let mut listing = Listing::default();
        let error = read_open_directory_with_portable_fallback(
            &root,
            None,
            true,
            &mut buffer,
            &mut listing,
            |directory, path, buffer, listing| {
                read_directory_from_open_directory_with_read_batch(
                    directory,
                    path,
                    buffer,
                    listing,
                    |_, _| {
                        Err(ReadBatchError::Io(std::io::Error::from_raw_os_error(
                            libc::EOPNOTSUPP,
                        )))
                    },
                )
            },
        )
        .expect_err("EOPNOTSUPP is an ordinary directory error, not a capability latch");
        assert_eq!(error.raw_os_error(), Some(libc::EOPNOTSUPP));
        assert!(
            listing.entries().is_empty(),
            "the portable reader never restarted"
        );

        let records = record(b"already-read", DT_REG);
        let mut calls = 0;
        let error = read_open_directory_with_portable_fallback(
            &root,
            None,
            true,
            &mut buffer,
            &mut listing,
            |directory, path, buffer, listing| {
                read_directory_from_open_directory_with_read_batch(
                    directory,
                    path,
                    buffer,
                    listing,
                    |_, buffer| {
                        calls += 1;
                        if calls == 1 {
                            buffer[..records.len()].copy_from_slice(&records);
                            Ok(records.len())
                        } else {
                            Err(ReadBatchError::Io(std::io::Error::from_raw_os_error(
                                libc::ENOSYS,
                            )))
                        }
                    },
                )
            },
        )
        .expect_err("ENOSYS after an accepted batch does not restart the descriptor");
        assert_eq!(error.raw_os_error(), Some(libc::ENOSYS));
        assert!(
            listing.contains("already-read"),
            "the accepted batch is retained"
        );
        assert!(
            !listing.contains("portable-entry"),
            "an advanced descriptor never resumes through the portable reader"
        );
        fs::remove_dir_all(root).expect("remove capability-classification fixture");
    }

    #[test]
    fn portable_descriptor_fallback_closes_the_transferred_descriptor() {
        let root = fixture_root("descriptor-close");
        fs::create_dir_all(&root).expect("create fixture directory");
        fs::write(root.join("entry"), b"fixture").expect("write fixture entry");
        let directory = open_directory(&root, true).expect("open fixture directory");
        // Use a deliberately high descriptor so other concurrently-running
        // tests cannot plausibly reuse its number between `closedir` and the
        // observation below.
        let descriptor = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 1024) };
        assert!(descriptor >= 1024, "duplicate directory descriptor");
        drop(directory);
        // SAFETY: `fcntl` returned this live, uniquely-owned duplicate; the
        // portable reader takes it by value and must transfer it to `fdopendir`.
        let directory = unsafe { std::fs::File::from_raw_fd(descriptor) };
        let mut listing = Listing::default();
        read_portable_directory_from_open_file(directory, &root, &mut listing)
            .expect("read descriptor fallback");

        assert!(listing.contains("entry"));
        // SAFETY: `F_GETFD` does not modify the descriptor. `fdopendir`'s
        // successful ownership transfer means `DirectoryStream::drop` closed
        // it when the reader returned.
        assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
        fs::remove_dir_all(root).expect("remove descriptor-close fixture");
    }

    #[test]
    fn latched_unsupported_walks_descendants_in_serial_and_default_parallel_modes() {
        let root = fixture_root("latched-walker");
        for branch in 0..16 {
            let leaf = root.join(format!("branch-{branch}/nested/leaf-{branch}"));
            fs::create_dir_all(leaf.parent().expect("leaf has parent"))
                .expect("create descendant directory");
            fs::write(leaf, b"fixture").expect("write descendant file");
        }

        // This root-scoped override reaches the same branch as the process-wide
        // state after ENOSYS without redirecting unrelated parallel tests. The
        // native module must still open every scheduled no-follow descendant
        // once and enumerate it through that descriptor, in both frontends.
        let _latch = force_unsupported_latch_for_test(&root);
        for walker in [
            Walker::new(&root)
                .threads(1)
                .error_policy(ErrorPolicy::Collect)
                .options(WalkOptions::default().sort(true)),
            Walker::new(&root)
                .error_policy(ErrorPolicy::Collect)
                .options(WalkOptions::default().sort(true)),
        ] {
            let result = walker.collect().expect("latched fallback still walks");
            assert!(
                result.errors().is_empty(),
                "fallback reports no descendant errors"
            );
            assert_eq!(
                result.entries().len(),
                48,
                "all directory and file descendants remain"
            );
        }
        fs::remove_dir_all(root).expect("remove latched-walker fixture");
    }

    #[test]
    fn latched_unsupported_no_follow_failures_reach_the_caller() {
        let root = fixture_root("latched-failure");
        let target = root.join("target");
        let replaced = root.join("scheduled");
        fs::create_dir_all(&target).expect("create target directory");
        symlink(&target, &replaced).expect("create replacement link");

        let _latch = force_unsupported_latch_for_test(&root);
        let mut listing = Listing::default();
        let error = read_directory(&replaced, None, true, &mut listing)
            .expect_err("a protected fallback must still reject a replacement link");

        assert!(matches!(
            error.raw_os_error(),
            Some(libc::ELOOP | libc::ENOTDIR)
        ));
        assert!(
            listing.entries().is_empty(),
            "a failed open has no partial listing"
        );
        fs::remove_dir_all(root).expect("remove latched-failure fixture");
    }

    #[test]
    fn capability_latch_guard_restores_after_a_panic() {
        let before = forced_unsupported_root_for_test();
        let root = PathBuf::from("/ferralk-forced-latch-guard");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _latch = force_unsupported_latch_for_test(&root);
            assert!(unsupported_is_latched_for(&root));
            panic!("exercise guard unwinding");
        }));
        assert!(result.is_err(), "the injected panic was caught");
        assert_eq!(forced_unsupported_root_for_test(), before);
    }

    #[test]
    fn native_reader_matches_the_portable_reader() {
        let root = std::env::temp_dir().join(format!(
            "ferralk-linux-native-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        ));
        fs::create_dir_all(root.join("nested")).expect("create native fixture");
        fs::write(root.join("file.txt"), b"fixture").expect("write native fixture");

        let describe = |read: &dyn Fn(&mut Listing)| {
            let mut listing = Listing::default();
            read(&mut listing);
            let mut described = listing
                .entries()
                .iter()
                .map(|entry| {
                    (
                        PathBuf::from(entry.name()),
                        entry.is_dir(),
                        entry.is_symlink(),
                    )
                })
                .collect::<Vec<(PathBuf, bool, bool)>>();
            described.sort();
            described
        };
        assert_eq!(
            describe(&|listing| {
                read_directory(&root, None, false, listing).expect("native reader succeeds");
            }),
            describe(&|listing| {
                StdBackend
                    .read_directory(&root, false, false, listing)
                    .expect("portable reader succeeds");
            })
        );
        fs::remove_dir_all(root).expect("remove native fixture");
    }

    #[test]
    fn native_walker_matches_portable_filters_metadata_and_symlinks() {
        let root = fixture_root("walker");
        fs::create_dir_all(root.join("src/nested")).expect("create source fixture");
        fs::create_dir_all(root.join("ignored")).expect("create ignored fixture");
        fs::create_dir_all(root.join(".hidden")).expect("create hidden fixture");
        fs::write(root.join("src/lib.rs"), b"library").expect("write source fixture");
        fs::write(root.join("src/nested/mod.rs"), b"module").expect("write nested fixture");
        fs::write(root.join("src/generated.tmp"), b"temporary").expect("write temp fixture");
        fs::write(root.join("ignored/skip.rs"), b"ignored").expect("write ignored fixture");
        fs::write(root.join(".hidden/skip.rs"), b"hidden").expect("write hidden fixture");
        fs::write(root.join(".gitignore"), b"ignored/\n").expect("write gitignore fixture");
        symlink("src", root.join("source-link")).expect("create directory symlink");

        let walker = Walker::new(&root)
            .threads(1)
            .include("**/*")
            .expect("valid include")
            .exclude("**/*.tmp")
            .expect("valid exclude")
            .respect_git_ignore(true)
            .error_policy(ErrorPolicy::Collect)
            .options(
                WalkOptions::default()
                    .sort(true)
                    .metadata(true)
                    .skip_hidden(true),
            );
        let native = walker.clone().collect().expect("native walk succeeds");
        let (portable_entries, portable_errors) = collect_with_portable_backend(&walker);

        assert!(native.errors().is_empty());
        assert!(portable_errors.is_empty());
        assert_eq!(
            describe_walk_entries(native.entries(), &root),
            describe_walk_entries(&portable_entries, &root)
        );
        fs::remove_dir_all(root).expect("remove walker fixture");
    }

    type DescribedWalkEntry = (PathBuf, bool, bool, usize, Option<(u64, bool, bool)>);

    fn fixture_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ferralk-linux-native-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        ))
    }

    fn collect_with_portable_backend(walker: &Walker) -> (Vec<WalkEntry>, Vec<crate::WalkError>) {
        let mut state = crate::WalkState::new(walker, &crate::keep_every_entry);
        let root = walker
            .roots()
            .next()
            .expect("a walk has a root")
            .to_path_buf();
        let (ignores, ignore_errors) = crate::IgnoreScope::for_root(walker, &StdBackend, &root);
        let task = crate::DirectoryTask {
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
            .expect("portable walk succeeds");
        if walker.options.sort {
            state
                .entries
                .sort_by(|left, right| left.path.cmp(&right.path));
        }
        (state.entries, state.errors)
    }

    /// Set in the child test process that owns the `RLIMIT_NOFILE` change.
    const LOWERED_DESCRIPTOR_LIMIT_CHILD: &str = "FERRALK_LOWERED_DESCRIPTOR_LIMIT_CHILD";

    /// A process that lowers `RLIMIT_NOFILE` between walks must walk with
    /// the budget that limit allows, not the ceiling measured earlier.
    ///
    /// The soft limit and the measured ceiling are process-wide, and libtest
    /// runs the other native tests on threads of this process: a concurrent
    /// walk would re-measure the ceiling inside the window, and the lowered
    /// limit could starve it of descriptors. The mutation therefore runs in
    /// a child test process that executes only this test, the way the
    /// cwd- and environment-sensitive tests in `lib.rs` do, and restores the
    /// limit before that child exits.
    #[test]
    fn the_next_walk_measures_a_lowered_descriptor_limit() {
        if std::env::var_os(LOWERED_DESCRIPTOR_LIMIT_CHILD).is_some() {
            lower_the_descriptor_limit_between_two_walks();
            return;
        }
        let status =
            std::process::Command::new(std::env::current_exe().expect("locate test binary"))
                .args([
                    "linux_native::tests::the_next_walk_measures_a_lowered_descriptor_limit",
                    "--exact",
                ])
                .env(LOWERED_DESCRIPTOR_LIMIT_CHILD, "1")
                .status()
                .expect("run the isolated descriptor-limit regression test");
        assert!(
            status.success(),
            "the isolated descriptor-limit child failed: {status}"
        );
    }

    /// The body of the regression, run only in the isolated child. The soft
    /// limit is lowered to at most 512, or half its current value, for the
    /// duration of one walk of an empty directory and restored on every
    /// exit path through the guard.
    #[allow(unsafe_code)]
    fn lower_the_descriptor_limit_between_two_walks() {
        struct RestoreLimit(libc::rlimit);

        impl Drop for RestoreLimit {
            fn drop(&mut self) {
                // SAFETY: `self.0` is the initialized limit pair `getrlimit`
                // returned for this process.
                let status = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.0) };
                assert_eq!(status, 0, "restore the descriptor limit");
                super::refresh_retained_directory_limit();
            }
        }

        let mut limits = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: `limits` points at writable storage for one `rlimit`; a
        // successful call initializes it before `assume_init` below.
        let status = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limits.as_mut_ptr()) };
        assert_eq!(status, 0, "read the descriptor limit");
        // SAFETY: `getrlimit` returned success and initialized the output.
        let original = unsafe { limits.assume_init() };
        let _restore = RestoreLimit(original);

        let root = fixture_root("lowered-descriptor-limit");
        fs::create_dir_all(&root).expect("create fixture");
        Walker::new(&root)
            .collect()
            .expect("walk under the original limit");
        let measured_before = super::retained_directory_limit();
        let expected_before = usize::try_from(original.rlim_cur)
            .map_or(256, |limit| limit.saturating_div(4).min(256));
        assert_eq!(measured_before, expected_before);

        let lowered = libc::rlimit {
            rlim_cur: (original.rlim_cur / 2).min(512),
            rlim_max: original.rlim_max,
        };
        // SAFETY: `lowered` is a fully initialized limit pair whose soft
        // limit stays below the unchanged hard limit.
        let status = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) };
        assert_eq!(status, 0, "lower the soft descriptor limit");
        assert_eq!(
            super::retained_directory_limit(),
            measured_before,
            "nothing re-measures the limit between walks"
        );

        Walker::new(&root)
            .collect()
            .expect("walk under the lowered limit");
        let expected_after =
            usize::try_from(lowered.rlim_cur).map_or(256, |limit| limit.saturating_div(4).min(256));
        assert_eq!(
            super::retained_directory_limit(),
            expected_after,
            "the walk start re-measures the limit"
        );
        assert!(
            expected_after < expected_before,
            "the lowered limit must be observable: {expected_before} -> {expected_after}"
        );
        fs::remove_dir_all(root).expect("remove fixture");
    }

    fn describe_walk_entries(entries: &[WalkEntry], root: &Path) -> Vec<DescribedWalkEntry> {
        entries
            .iter()
            .map(|entry| {
                (
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("entry belongs to fixture")
                        .to_path_buf(),
                    entry.is_dir(),
                    entry.is_symlink(),
                    entry.depth(),
                    entry.metadata().map(|metadata| {
                        (
                            metadata.len(),
                            metadata.file_type().is_dir(),
                            metadata.file_type().is_symlink(),
                        )
                    }),
                )
            })
            .collect()
    }
}
