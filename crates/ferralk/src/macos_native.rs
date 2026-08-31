//! macOS native directory backend.
//!
//! The only unsafe operations are Darwin syscalls. Per-thread owned buffers
//! remain live for each call; every kernel record is bounds-checked before its
//! fields or name are read.
//!
//! `getdirentries64` is the reader the walk uses. It is the core Darwin dirent
//! syscall — every local filesystem serves it — and one record carries exactly
//! what [`Listing::push`] stores: the entry name and its `d_type`. Darwin libc
//! uses the private `__getdirentries64` stub linked below too; consumers that
//! need App Store compatibility should leave the `native-macos` feature off,
//! because private-symbol scans can reject that linkage.
//!
//! `getattrlistbulk` was the reader until 2026-08-20. It returns the same two
//! facts in a richer per-entry record, and assembling that record is work the
//! walk then throws away: nothing downstream of `Listing::push` harvests a bulk
//! attribute, because a listing carries name, is-dir and is-symlink and nothing
//! else. Measured on an M1 Pro over the 53k-file Palamedes fixture, routing to
//! `getdirentries64` took the serial walk from 81.0 ms to 57.1 ms and the
//! four-thread walk from 36.6 ms to 31.5 ms; see `docs/benchmark-evidence.md`.
//!
//! The bulk reader and its parser stay in the module as a regression boundary:
//! `parse_bulk_record` is a fuzz target and both readers are checked against the
//! portable backend by the parity test below. Nothing in the walk calls them.
//!
//! When the private reader is unavailable, the already-open directory handle
//! is transferred to the public `fdopendir`/`readdir` API. That keeps an
//! `O_NOFOLLOW` descendant protected during the portable degradation: no
//! fallback reopens its original path.

use std::{
    cell::RefCell,
    ffi::{CString, OsStr, OsString, c_int, c_void},
    fs::{self, File, OpenOptions},
    io,
    ops::Range,
    os::unix::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawFd, FromRawFd, IntoRawFd},
    },
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use super::{Listing, defer_entry_stat_error};

const BUFFER_SIZE: usize = 32 * 1024;
const GETDIRENTRIES64_EXTENDED_BUFFER_MINIMUM: usize = 1024;
const GETDIRENTRIES64_EOF: u32 = 0x1;
const RECORD_LENGTH_OFFSET: usize = 16;
const NAME_LENGTH_OFFSET: usize = 18;
const TYPE_OFFSET: usize = 20;
const NAME_OFFSET: usize = 21;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;
// Types that are neither a directory nor a symlink, so they need no stat.
const DT_FIFO: u8 = 1;
const DT_CHR: u8 = 2;
const DT_BLK: u8 = 6;
const DT_SOCK: u8 = 12;
const O_DIRECTORY: i32 = 0x0010_0000;
const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const FSOPT_NOFOLLOW: u64 = 0x0000_0001;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VBLK: u32 = 3;
const VCHR: u32 = 4;
const VLNK: u32 = 5;
const VSOCK: u32 = 6;
const VFIFO: u32 = 7;
const ATTRIBUTE_SET_SIZE: usize = 5 * std::mem::size_of::<u32>();
const ATTRIBUTE_RECORD_HEADER_SIZE: usize = std::mem::size_of::<u32>() + ATTRIBUTE_SET_SIZE;

/// Set once the kernel reports that native directory reads are unavailable.
///
/// Support is a property of the filesystem driver rather than of one
/// directory, so probing again for every directory only pays a failed open, a
/// failed syscall, and a second `opendir`. The latch is per process: a walk
/// that crosses from a filesystem without native support onto one with it keeps
/// using the portable reader, which costs speed and never correctness. Keying
/// it by `st_dev` instead would need the device id before the first read, and
/// obtaining that costs the very syscall this avoids.
///
/// `getdirentries64` refusing a filesystem outright is far less likely than
/// `getattrlistbulk` was — it is the syscall `readdir` itself is built on — so
/// this latch is expected to stay clear. It is kept because the fallback it
/// guards is what makes an exotic filesystem a slow walk rather than a failed
/// one, which is the semantics the bulk reader established.
static NATIVE_UNSUPPORTED: AtomicBool = AtomicBool::new(false);

/// Process-wide ceiling for descriptors retained beyond one directory read.
///
/// Relative opens are an optimization, so reaching the ceiling falls back to
/// the existing full-path open instead of turning a wide tree into `EMFILE`.
const MAX_RETAINED_DIRECTORIES: usize = 256;
static RETAINED_DIRECTORIES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static RETAINED_DIRECTORY_LIMIT: OnceLock<usize> = OnceLock::new();

fn retained_directory_limit() -> usize {
    *RETAINED_DIRECTORY_LIMIT.get_or_init(|| {
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
    })
}

std::thread_local! {
    static DIRENTRIES_BUFFER: RefCell<Box<[u8; BUFFER_SIZE]>> = RefCell::new(Box::new([0; BUFFER_SIZE]));
}

/// Distinguishes a rejected private reader from an error while using it.
///
/// A capability result is produced only by `__getdirentries64` returning raw
/// `EINVAL` or `ENOTSUP` before the first accepted batch. In particular, an
/// entry's metadata error can also have `ErrorKind::Unsupported`, but its
/// directory descriptor has already advanced and must never be reread through
/// the portable stream.
#[derive(Debug)]
pub(super) enum NativeDirectoryReadError {
    CapabilityUnavailable(io::Error),
    Io(io::Error),
}

/// Parent capability and basename retained by a queued child directory.
#[derive(Debug, Clone)]
pub(super) struct RelativeDirectoryOpen {
    pub(super) parent: Arc<RetainedDirectory>,
    pub(super) name: OsString,
}

#[derive(Debug)]
pub(super) struct RetainedDirectory(File);

impl RetainedDirectory {
    fn retain(directory: File) -> Option<Arc<Self>> {
        RETAINED_DIRECTORIES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |retained| {
                (retained < retained_directory_limit()).then_some(retained + 1)
            })
            .ok()
            .map(|_| Arc::new(Self(directory)))
    }
}

impl Drop for RetainedDirectory {
    fn drop(&mut self) {
        RETAINED_DIRECTORIES.fetch_sub(1, Ordering::AcqRel);
    }
}

impl From<io::Error> for NativeDirectoryReadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl NativeDirectoryReadError {
    pub(super) fn into_io_error(self) -> io::Error {
        match self {
            Self::CapabilityUnavailable(error) | Self::Io(error) => error,
        }
    }
}

type NativeDirectoryReadResult = Result<(), NativeDirectoryReadError>;

#[repr(C)]
struct AttrList {
    bitmap_count: u16,
    reserved: u16,
    common_attributes: u32,
    volume_attributes: u32,
    directory_attributes: u32,
    file_attributes: u32,
    fork_attributes: u32,
}

unsafe extern "C" {
    fn getattrlistbulk(
        fd: c_int,
        attributes: *const AttrList,
        buffer: *mut c_void,
        buffer_size: usize,
        options: u64,
    ) -> c_int;

    fn __getdirentries64(
        fd: c_int,
        buffer: *mut c_void,
        buffer_size: usize,
        base: *mut u64,
    ) -> isize;
}

pub(super) fn read_directory(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
    listing: &mut Listing,
) -> NativeDirectoryReadResult {
    let result = if NATIVE_UNSUPPORTED.load(Ordering::Relaxed) {
        read_portable_directory_from_path(path, relative, refuse_final_symlink, listing)?;
        Ok(())
    } else {
        read_direntries_directory(path, relative, refuse_final_symlink, listing)
    };
    if result.is_ok() {
        reject_path_limited_child_directories(path, listing);
    }
    result
}

/// Makes descriptor-relative traversal stop where ordinary pathname traversal
/// would. A parallel worker may otherwise emit a directory before its queued
/// child discovers `ENAMETOOLONG`, while the depth-first frontend encounters
/// that error before emitting the same directory.
fn reject_path_limited_child_directories(path: &Path, listing: &mut Listing) {
    let mut index = 0;
    while index < listing.entries().len() {
        let rejected = listing.entries()[index].is_dir()
            && path
                .join(listing.entries()[index].name())
                .as_os_str()
                .as_bytes()
                .len()
                >= libc::PATH_MAX as usize;
        if rejected {
            let child = path.join(listing.entries()[index].name());
            listing.remove_entry(index);
            listing.defer_error(child, io::Error::from_raw_os_error(libc::ENAMETOOLONG));
        } else {
            index += 1;
        }
    }
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

/// Opens a directory for reading, refusing anything that is not one.
///
/// `O_DIRECTORY` makes the open fail with `ENOTDIR` instead of blocking when
/// the scheduled path was replaced by a FIFO between scheduling and open.
/// For a scheduled non-root directory in a no-follow walk, `O_NOFOLLOW` makes
/// the final component check and open one operation. A directory exchanged for
/// a symlink is therefore refused (`ENOTDIR` or `ELOOP`, depending on the
/// Darwin open path) instead of escaping through that link. User-supplied
/// symlink roots retain portable semantics.
fn open_directory(path: &Path, refuse_final_symlink: bool) -> io::Result<File> {
    let flags = O_DIRECTORY
        | if refuse_final_symlink {
            libc::O_NOFOLLOW
        } else {
            0
        };
    OpenOptions::new().read(true).custom_flags(flags).open(path)
}

/// Opens a queued child relative to the still-open parent that named it.
fn open_scheduled_directory(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
) -> io::Result<File> {
    // Relative `openat` can reach a directory whose reported path has already
    // crossed Darwin's path-only limit. Do not let retained descriptors make
    // that an accidental frontend-dependent extension: callers receive the
    // full path and their later metadata calls cannot use it either. Keeping
    // the portable `ENAMETOOLONG` boundary makes serial, parallel, and stream
    // agree regardless of how many parent descriptors happen to be retained.
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
        | O_DIRECTORY
        | if refuse_final_symlink {
            libc::O_NOFOLLOW
        } else {
            0
        };
    // SAFETY: the parent `Arc<File>` keeps a live directory descriptor for the
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

/// The `getattrlistbulk` reader, kept as the regression boundary the module
/// documentation describes. The walk reads directories through
/// [`read_direntries_directory`]; this one is exercised by the parity test and
/// keeps [`parse_bulk_record`] reachable from something that runs it against a
/// real filesystem rather than from the fuzz target alone.
///
/// It owns its buffer per call instead of borrowing a thread-local one. Nothing
/// on a hot path calls it any more, so the buffer that existed to keep the walk
/// allocation-free would only be a live 32 KiB per thread.
#[cfg_attr(not(test), allow(dead_code))]
fn read_bulk_directory(path: &Path, listing: &mut Listing) -> io::Result<()> {
    let mut buffer = Box::new([0_u8; BUFFER_SIZE]);
    read_bulk_directory_with_buffer(path, &mut buffer[..], listing)
}

#[cfg_attr(not(test), allow(dead_code))]
fn read_bulk_directory_with_buffer(
    path: &Path,
    buffer: &mut [u8],
    listing: &mut Listing,
) -> io::Result<()> {
    let directory = open_directory(path, false)?;
    listing.clear();
    let attributes = AttrList {
        bitmap_count: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        common_attributes: ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | ATTR_CMN_NAME
            | ATTR_CMN_OBJTYPE,
        volume_attributes: 0,
        directory_attributes: 0,
        file_attributes: 0,
        fork_attributes: 0,
    };
    let mut reader = BulkReader {
        directory,
        buffer,
        attributes,
        remaining: 0,
        offset: 0,
        primed: false,
    };
    while let Some((name, object_type)) = reader.next()? {
        let name = OsStr::from_bytes(&reader.buffer[name]);
        // An entry that vanished between the read and its stat costs that one
        // entry; the rest of the listing is still valid and is returned.
        match bulk_entry_kind(path, name, object_type) {
            Ok(Some((is_dir, is_symlink))) => listing.push(name, is_dir, is_symlink),
            Ok(None) => {}
            Err(error) => defer_entry_stat_error(listing, path.join(name), error)?,
        }
    }
    Ok(())
}

struct BulkReader<'buffer> {
    directory: File,
    buffer: &'buffer mut [u8],
    attributes: AttrList,
    remaining: usize,
    offset: usize,
    primed: bool,
}

impl BulkReader<'_> {
    /// Advances to the next record and reports where its name lies in the
    /// buffer.
    ///
    /// A range rather than a slice: a returned slice would keep the reader
    /// borrowed, so the caller could not ask for the next record — and copying
    /// the name out to sidestep that is an allocation for every entry the walk
    /// reads, most of which it is about to discard.
    fn next(&mut self) -> io::Result<Option<(Range<usize>, Option<u32>)>> {
        loop {
            if self.remaining == 0 && !self.refill()? {
                return Ok(None);
            }
            let record_length = read_u32(self.buffer, self.offset)? as usize;
            if record_length < ATTRIBUTE_RECORD_HEADER_SIZE
                || record_length > self.buffer.len().saturating_sub(self.offset)
            {
                return Err(malformed_bulk_record());
            }
            let record_end = self
                .offset
                .checked_add(record_length)
                .ok_or_else(malformed_bulk_record)?;
            let record = &self.buffer[self.offset..record_end];
            self.offset = record_end;
            self.remaining -= 1;
            if let Some((name, object_type)) = parse_bulk_record(record)? {
                let start = record_end - record_length;
                return Ok(Some((start + name.start..start + name.end, object_type)));
            }
        }
    }

    fn refill(&mut self) -> io::Result<bool> {
        loop {
            // SAFETY: `buffer` is owned, writable, and lives through the call;
            // its length is passed verbatim. `attributes` has the Darwin C
            // layout, and `directory` owns the descriptor for the call.
            let count = unsafe {
                getattrlistbulk(
                    self.directory.as_raw_fd(),
                    &self.attributes,
                    self.buffer.as_mut_ptr().cast(),
                    self.buffer.len(),
                    FSOPT_NOFOLLOW,
                )
            };
            if count >= 0 {
                self.primed = true;
                self.remaining = count as usize;
                self.offset = 0;
                return Ok(count != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // Darwin returns EINVAL or EOPNOTSUPP for an unsupported bulk
            // operation. EINVAL is a capability fallback only before a batch
            // has been accepted; after that it is an ordinary read failure.
            if is_unsupported_bulk_error(&error, self.primed) {
                return Err(io::Error::new(io::ErrorKind::Unsupported, error));
            }
            return Err(error);
        }
    }
}

fn is_unsupported_bulk_error(error: &io::Error, primed: bool) -> bool {
    !primed && matches!(error.raw_os_error(), Some(22 | 45))
}

/// Parses one bulk record into its name and, when the filesystem returned it,
/// its object type.
///
/// A record without `ATTR_CMN_OBJTYPE` yields `None` for the type so the
/// caller resolves it with one stat. A record without `ATTR_CMN_NAME` cannot
/// be used at all and is reported as unsupported, which makes the adapter read
/// the directory portably instead of losing it: network filesystems are
/// allowed to omit attributes, and doing so is not a malformed record.
fn parse_bulk_record(record: &[u8]) -> io::Result<Option<(Range<usize>, Option<u32>)>> {
    if record.len() < ATTRIBUTE_RECORD_HEADER_SIZE || read_u32(record, 0)? as usize != record.len()
    {
        return Err(malformed_bulk_record());
    }
    let common_attributes = read_u32(record, std::mem::size_of::<u32>())?;
    if common_attributes & ATTR_CMN_RETURNED_ATTRS == 0 {
        return Err(malformed_bulk_record());
    }
    let mut offset = ATTRIBUTE_RECORD_HEADER_SIZE;
    if common_attributes & ATTR_CMN_ERROR != 0 {
        let error = read_u32(record, offset)?;
        offset = offset
            .checked_add(std::mem::size_of::<u32>())
            .ok_or_else(malformed_bulk_record)?;
        if error != 0 {
            return Ok(None);
        }
    }
    if common_attributes & ATTR_CMN_NAME == 0 {
        return Err(unsupported(
            "getattrlistbulk did not return entry names on this filesystem",
        ));
    }
    let reference_offset = read_i32(record, offset)?;
    let name_length = read_u32(record, offset + std::mem::size_of::<i32>())? as usize;
    let reference_position = offset;
    offset = offset
        .checked_add(std::mem::size_of::<i32>() + std::mem::size_of::<u32>())
        .ok_or_else(malformed_bulk_record)?;
    let name_start = if reference_offset >= 0 {
        reference_position.checked_add(reference_offset as usize)
    } else {
        reference_position.checked_sub(reference_offset.unsigned_abs() as usize)
    }
    .ok_or_else(malformed_bulk_record)?;
    let name_end = name_start
        .checked_add(name_length)
        .filter(|&end| end <= record.len())
        .ok_or_else(malformed_bulk_record)?;
    let name_with_nul = record
        .get(name_start..name_end)
        .ok_or_else(malformed_bulk_record)?;
    let Some((&0, name)) = name_with_nul.split_last() else {
        return Err(malformed_bulk_record());
    };
    if name.is_empty() || memchr::memchr2(0, b'/', name).is_some() {
        return Err(malformed_bulk_record());
    }
    // The name without its terminating nul, as a range into `record`.
    let name = name_start..name_end - 1;
    if common_attributes & ATTR_CMN_OBJTYPE == 0 {
        return Ok(Some((name, None)));
    }
    let object_type = read_u32(record, offset)?;
    Ok(Some((name, Some(object_type))))
}

/// Validates one raw Darwin bulk-attribute record without touching the
/// filesystem. This is exposed only for the feature-gated cargo-fuzz target.
#[doc(hidden)]
pub fn fuzz_validate_bulk_record(record: &[u8]) {
    let _ = parse_bulk_record(record);
}

fn read_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let value = bytes
        .get(offset..offset + std::mem::size_of::<u32>())
        .ok_or_else(malformed_bulk_record)?;
    Ok(u32::from_ne_bytes(
        value
            .try_into()
            .expect("u32 slice has fixed width after bounds check"),
    ))
}

fn read_i32(bytes: &[u8], offset: usize) -> io::Result<i32> {
    let value = bytes
        .get(offset..offset + std::mem::size_of::<i32>())
        .ok_or_else(malformed_bulk_record)?;
    Ok(i32::from_ne_bytes(
        value
            .try_into()
            .expect("i32 slice has fixed width after bounds check"),
    ))
}

fn bulk_entry_kind(
    directory: &Path,
    name: &OsStr,
    object_type: Option<u32>,
) -> io::Result<Option<(bool, bool)>> {
    match object_type {
        Some(VDIR) => Ok(Some((true, false))),
        Some(VREG | VBLK | VCHR | VSOCK | VFIFO) => Ok(Some((false, false))),
        Some(VLNK) => Ok(Some((false, true))),
        // `VNON`, `VBAD`, and a record the filesystem left without an object
        // type need one stat, and a whole path to stat with. Building one here
        // is what keeps the common cases above from needing it.
        _ => stat_entry_kind(&directory.join(name)),
    }
}

fn read_direntries_directory(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
    listing: &mut Listing,
) -> NativeDirectoryReadResult {
    DIRENTRIES_BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        read_direntries_directory_with_buffer(
            path,
            relative,
            refuse_final_symlink,
            &mut buffer[..],
            listing,
        )
    })
}

fn read_direntries_directory_with_buffer(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
    buffer: &mut [u8],
    listing: &mut Listing,
) -> NativeDirectoryReadResult {
    let used_portable_fallback = read_open_directory_with_portable_fallback(
        path,
        relative,
        refuse_final_symlink,
        buffer,
        listing,
        read_direntries_from_open_directory,
    )?;
    if used_portable_fallback {
        NATIVE_UNSUPPORTED.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Reads an already-open directory with the private reader, degrading through
/// the same descriptor if that reader is unavailable before its first batch.
///
/// The boolean reports whether the portable descriptor reader ran, allowing
/// the caller to latch the native capability without making the descriptor
/// escape this function.
fn read_open_directory_with_portable_fallback(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
    buffer: &mut [u8],
    listing: &mut Listing,
    native: impl FnOnce(&File, &Path, &mut [u8], &mut Listing) -> NativeDirectoryReadResult,
) -> Result<bool, NativeDirectoryReadError> {
    let directory = open_scheduled_directory(path, relative, refuse_final_symlink)?;
    match native(&directory, path, buffer, listing) {
        Ok(()) => {
            listing.native_directory = RetainedDirectory::retain(directory);
            Ok(false)
        }
        Err(NativeDirectoryReadError::CapabilityUnavailable(_)) => {
            read_portable_directory_from_open_file(directory, path, listing)?;
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

/// Opens a directory once with the native walk's flags, then uses the public
/// descriptor reader. This is the latched capability path: it must not call
/// `std::fs::read_dir(path)`, because a scheduled no-follow descendant may
/// have changed between scheduling and this open.
fn read_portable_directory_from_path(
    path: &Path,
    relative: Option<&RelativeDirectoryOpen>,
    refuse_final_symlink: bool,
    listing: &mut Listing,
) -> io::Result<()> {
    let directory = open_scheduled_directory(path, relative, refuse_final_symlink)?;
    read_portable_directory_from_open_file(directory, path, listing)
}

/// Enumerates exactly the directory a `File` already refers to.
///
/// `fdopendir` takes ownership of the descriptor on success; `DirectoryStream`
/// then closes it exactly once. `readdir` supplies names relative to that
/// descriptor, and `fstatat` resolves its rare `DT_UNKNOWN` records relative
/// to the same descriptor too, so neither operation reopens `path`.
fn read_portable_directory_from_open_file(
    directory: File,
    reported_path: &Path,
    listing: &mut Listing,
) -> io::Result<()> {
    let descriptor = directory.into_raw_fd();
    // SAFETY: `descriptor` is a live directory descriptor transferred from
    // `directory`. On success `fdopendir` owns it; on failure we reconstruct
    // the `File` below so it is still closed exactly once.
    let stream = unsafe { libc::fdopendir(descriptor) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: `fdopendir` did not take ownership on failure, and this is
        // the sole reconstruction of the descriptor transferred above.
        unsafe { drop(File::from_raw_fd(descriptor)) };
        return Err(error);
    }
    let stream = DirectoryStream(stream);
    listing.clear();
    loop {
        // SAFETY: `__error` returns this thread's writable errno slot. Clearing
        // it lets a null `readdir` result distinguish EOF from a read failure.
        unsafe { *libc::__error() = 0 };
        // SAFETY: `stream` owns a valid `DIR` until its Drop calls `closedir`;
        // the returned pointer is consumed before the next `readdir` call.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(error)
            };
        }
        // SAFETY: a successful `readdir` result points to a valid Darwin
        // `dirent`; `d_namlen` bounds the name within its fixed d_name buffer.
        let entry = unsafe { &*entry };
        let name = unsafe {
            std::slice::from_raw_parts(entry.d_name.as_ptr().cast::<u8>(), entry.d_namlen as usize)
        };
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

/// Owns the `DIR` returned by `fdopendir`, including its directory descriptor.
struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this is the one `closedir` corresponding to successful
        // `fdopendir`; libc closes the transferred descriptor with the stream.
        unsafe { libc::closedir(self.0) };
    }
}

/// Resolves an unknown `d_type` relative to the protected directory descriptor.
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
            // SAFETY: zeroed `stat` is valid output storage, `directory` is a
            // live descriptor held by `DirectoryStream`, and `name` remains
            // NUL-terminated and live for the call.
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

fn read_direntries_from_open_directory(
    directory: &File,
    path: &Path,
    buffer: &mut [u8],
    listing: &mut Listing,
) -> NativeDirectoryReadResult {
    let mut base = 0_u64;
    let mut primed = false;
    listing.clear();
    loop {
        let byte_count = loop {
            clear_direntries_eof_tail(buffer);
            // SAFETY: `buffer` is owned, writable, and lives through the call;
            // its length is passed verbatim. `base` is a valid mutable u64, and
            // `directory` owns the open descriptor for the duration of reads.
            // The extended EOF tail is reset immediately before every call so
            // pre-10.15 kernels, which never write that extension, cannot
            // leave a stale bit claiming EOF.
            let byte_count = unsafe {
                __getdirentries64(
                    directory.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut base,
                )
            };
            if byte_count >= 0 {
                break byte_count as usize;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                if is_unsupported_direntries_error(&error, primed) {
                    return Err(NativeDirectoryReadError::CapabilityUnavailable(error));
                }
                return Err(error.into());
            }
        };
        if byte_count == 0 {
            return Ok(());
        }
        primed = true;
        parse_records(path, &buffer[..byte_count], listing)?;
        if has_direntries_eof_flag(buffer) {
            return Ok(());
        }
    }
}

fn malformed_bulk_record() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed getattrlistbulk record",
    )
}

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
        match classify(directory, name, directory_type) {
            Ok(Some((is_dir, is_symlink))) => listing.push(name, is_dir, is_symlink),
            Ok(None) => {}
            Err(error) => defer_entry_stat_error(listing, directory.join(name), error)?,
        }
        Ok(())
    })
}

/// Validates raw Darwin directory records without touching the filesystem.
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
        if record.len() < NAME_OFFSET {
            return Err(malformed_record());
        }
        let record_length = u16::from_ne_bytes(
            record[RECORD_LENGTH_OFFSET..NAME_LENGTH_OFFSET]
                .try_into()
                .expect("record length slice has fixed width"),
        ) as usize;
        let name_length = u16::from_ne_bytes(
            record[NAME_LENGTH_OFFSET..TYPE_OFFSET]
                .try_into()
                .expect("name length slice has fixed width"),
        ) as usize;
        if record_length < NAME_OFFSET || record_length > record.len() {
            return Err(malformed_record());
        }
        let name_end = NAME_OFFSET
            .checked_add(name_length)
            .filter(|&end| end <= record_length)
            .ok_or_else(malformed_record)?;
        let name = &record[NAME_OFFSET..name_end];
        offset = offset
            .checked_add(record_length)
            .ok_or_else(malformed_record)?;
        if name.is_empty() || name == b"." || name == b".." {
            continue;
        }
        // One vectorised pass for both rejected bytes: this runs for every
        // entry of every directory the walk reads.
        if memchr::memchr2(0, b'/', name).is_some() {
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
        // and a whole path to stat with.
        _ => stat_entry_kind(&directory.join(name)),
    }
}

fn stat_entry_kind(path: &Path) -> io::Result<Option<(bool, bool)>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            Ok(Some((file_type.is_dir(), file_type.is_symlink())))
        }
        Err(error) if is_vanished_entry(&error) => Ok(None),
        Err(error) => Err(error),
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

/// Darwin uses raw errno 45 (`ENOTSUP`) for a filesystem that refuses this
/// private syscall. `std::io` calls it `Uncategorized`, so preserve the native
/// backend's documented portable-fallback signal ourselves. `EINVAL` is a
/// capability result only before the first accepted batch, matching the bulk
/// reader's established contract.
fn is_unsupported_direntries_error(error: &io::Error, primed: bool) -> bool {
    !primed && matches!(error.raw_os_error(), Some(22 | 45))
}

/// XNU reserves the final word of an extended (at least 1024-byte) buffer for
/// `getdirentries64_flags_t`. It lies outside `byte_count`, so inspecting it
/// after parsing cannot relax the parser's bounds.
fn has_direntries_eof_flag(buffer: &[u8]) -> bool {
    if buffer.len() < GETDIRENTRIES64_EXTENDED_BUFFER_MINIMUM {
        return false;
    }
    let tail = &buffer[buffer.len() - std::mem::size_of::<u32>()..];
    u32::from_ne_bytes(tail.try_into().expect("tail has u32 width")) & GETDIRENTRIES64_EOF != 0
}

/// Clears exactly XNU's optional extended EOF word before a syscall that may
/// expose it. Older kernels leave this out-of-band word untouched, so zero is
/// the conservative "no EOF" value while supported kernels retain their fast
/// final-batch signal.
fn clear_direntries_eof_tail(buffer: &mut [u8]) {
    if buffer.len() < GETDIRENTRIES64_EXTENDED_BUFFER_MINIMUM {
        return;
    }
    let tail_start = buffer.len() - std::mem::size_of::<u32>();
    buffer[tail_start..].fill(0);
}

fn malformed_record() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed getdirentries64 record",
    )
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::{
        DirectoryBackend, ErrorPolicy, StdBackend, SystemBackend, WalkEntry, WalkOptions, Walker,
    };

    use super::{
        ATTR_CMN_ERROR, ATTR_CMN_NAME, ATTR_CMN_OBJTYPE, ATTR_CMN_RETURNED_ATTRS,
        ATTRIBUTE_RECORD_HEADER_SIZE, BUFFER_SIZE, DT_BLK, DT_CHR, DT_DIR, DT_FIFO, DT_REG,
        DT_SOCK, GETDIRENTRIES64_EOF, Listing, NAME_OFFSET, NativeDirectoryReadError,
        RelativeDirectoryOpen, RetainedDirectory, VBLK, VCHR, VDIR, VFIFO, VSOCK, bulk_entry_kind,
        clear_direntries_eof_tail, entry_kind, for_each_record, has_direntries_eof_flag,
        is_unsupported_bulk_error, is_unsupported_direntries_error, open_directory,
        parse_bulk_record, parse_records, parse_records_with_entry_kind, read_bulk_directory,
        read_directory, read_direntries_directory, read_open_directory_with_portable_fallback,
    };

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
    type DescribedWalkEntry = (PathBuf, bool, bool, usize, Option<(u64, bool, bool)>);

    fn record(name: &[u8], directory_type: u8) -> Vec<u8> {
        let length = (NAME_OFFSET + name.len() + 3) & !3;
        let mut record = vec![0_u8; length];
        record[16..18].copy_from_slice(&(length as u16).to_ne_bytes());
        record[18..20].copy_from_slice(&(name.len() as u16).to_ne_bytes());
        record[20] = directory_type;
        record[NAME_OFFSET..NAME_OFFSET + name.len()].copy_from_slice(name);
        record
    }

    fn bulk_record(name: &[u8], object_type: u32) -> Vec<u8> {
        let attribute_error_offset = ATTRIBUTE_RECORD_HEADER_SIZE;
        let name_reference_offset = attribute_error_offset + 4;
        let object_type_offset = name_reference_offset + 8;
        let name_offset = object_type_offset + 4;
        let length = name_offset + name.len() + 1;
        let mut record = vec![0_u8; length];
        record[0..4].copy_from_slice(&(length as u32).to_ne_bytes());
        record[4..8].copy_from_slice(
            &(ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_ERROR | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE)
                .to_ne_bytes(),
        );
        record[name_reference_offset..name_reference_offset + 4]
            .copy_from_slice(&((name_offset - name_reference_offset) as i32).to_ne_bytes());
        record[name_reference_offset + 4..object_type_offset]
            .copy_from_slice(&((name.len() + 1) as u32).to_ne_bytes());
        record[object_type_offset..name_offset].copy_from_slice(&object_type.to_ne_bytes());
        record[name_offset..name_offset + name.len()].copy_from_slice(name);
        record
    }

    #[test]
    fn parser_skips_dot_entries_and_rejects_truncated_records() {
        let mut records = record(b".", DT_DIR);
        records.extend(record(b"..", DT_DIR));
        records.extend(record(b"regular", DT_REG));
        let mut listing = Listing::default();
        parse_records(Path::new("/tmp"), &records, &mut listing).expect("dot records parse");
        assert_eq!(listing.entries().len(), 1);
        assert!(!listing.entries()[0].is_dir());
        assert!(!listing.entries()[0].is_symlink());
        assert!(parse_records(Path::new("/tmp"), &[0_u8; NAME_OFFSET - 1], &mut listing).is_err());

        let mut zero_length = vec![0_u8; NAME_OFFSET];
        zero_length[18..20].copy_from_slice(&1_u16.to_ne_bytes());
        assert!(parse_records(Path::new("/tmp"), &zero_length, &mut listing).is_err());

        let mut oversized_name = record(b"x", DT_REG);
        let oversized_length = oversized_name.len() as u16;
        oversized_name[18..20].copy_from_slice(&oversized_length.to_ne_bytes());
        assert!(parse_records(Path::new("/tmp"), &oversized_name, &mut listing).is_err());
    }

    #[test]
    fn bulk_parser_validates_attribute_references_and_entry_errors() {
        let record = bulk_record(b"nested", VDIR);
        let (name, object_type) = parse_bulk_record(&record)
            .expect("valid bulk record")
            .expect("entry has no error");
        assert_eq!(&record[name], b"nested");
        assert_eq!(object_type, Some(VDIR));

        let mut missing_nul = bulk_record(b"file", VDIR);
        *missing_nul
            .last_mut()
            .expect("bulk record has a terminator") = b'x';
        assert!(parse_bulk_record(&missing_nul).is_err());

        let mut invalid_reference = bulk_record(b"file", VDIR);
        invalid_reference[ATTRIBUTE_RECORD_HEADER_SIZE + 4..ATTRIBUTE_RECORD_HEADER_SIZE + 8]
            .copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(parse_bulk_record(&invalid_reference).is_err());

        let mut entry_error = bulk_record(b"file", VDIR);
        entry_error[ATTRIBUTE_RECORD_HEADER_SIZE..ATTRIBUTE_RECORD_HEADER_SIZE + 4]
            .copy_from_slice(&1_u32.to_ne_bytes());
        assert!(
            parse_bulk_record(&entry_error)
                .expect("entry errors are skipped")
                .is_none()
        );
    }

    /// Builds a bulk record whose returned-attribute mask omits one attribute.
    fn bulk_record_without(name: &[u8], missing: u32) -> Vec<u8> {
        let attribute_error_offset = ATTRIBUTE_RECORD_HEADER_SIZE;
        let name_reference_offset = attribute_error_offset + 4;
        let name_offset = name_reference_offset + 8;
        let length = name_offset + name.len() + 1;
        let mut record = vec![0_u8; length];
        record[0..4].copy_from_slice(&(length as u32).to_ne_bytes());
        let mask = (ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_ERROR | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE)
            & !missing;
        record[4..8].copy_from_slice(&mask.to_ne_bytes());
        record[name_reference_offset..name_reference_offset + 4]
            .copy_from_slice(&((name_offset - name_reference_offset) as i32).to_ne_bytes());
        record[name_reference_offset + 4..name_offset]
            .copy_from_slice(&((name.len() + 1) as u32).to_ne_bytes());
        record[name_offset..name_offset + name.len()].copy_from_slice(name);
        record
    }

    #[test]
    fn missing_object_type_defers_to_a_stat_instead_of_failing() {
        let record = bulk_record_without(b"file", ATTR_CMN_OBJTYPE);
        let (name, object_type) = parse_bulk_record(&record)
            .expect("a record without an object type is usable")
            .expect("entry has no error");
        assert_eq!(&record[name], b"file");
        assert_eq!(object_type, None, "the caller resolves the type by stat");
    }

    #[test]
    fn missing_name_is_unsupported_rather_than_malformed() {
        // A network filesystem may return fewer attributes than requested.
        // Without a name the record is unusable, but the directory is not:
        // reporting `Unsupported` makes the adapter read it portably, while
        // `InvalidData` would surface as a lost directory.
        let record = bulk_record_without(b"file", ATTR_CMN_NAME);
        let error = parse_bulk_record(&record).expect_err("a nameless record is unusable");
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
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
        for object_type in [VBLK, VCHR, VSOCK, VFIFO] {
            assert_eq!(
                bulk_entry_kind(absent, "entry".as_ref(), Some(object_type))
                    .expect("no stat is attempted"),
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
        assert!(
            matches!(
                error.raw_os_error(),
                Some(libc::ELOOP) | Some(libc::ENOTDIR)
            ),
            "a no-follow directory open rejects the changed path: {error}"
        );
        let mut listing = Listing::default();
        read_directory(&link, None, false, &mut listing)
            .expect("follow mode still opens through a link");
        assert!(listing.contains("inside"));
        fs::remove_dir_all(root).expect("remove no-follow fixture");
    }

    #[test]
    fn no_follow_fallback_preserves_the_boundary_and_roots_stay_compatible() {
        let root = fixture_root("no-follow-fallback");
        let target = root.join("target");
        let link = root.join("scheduled");
        fs::create_dir_all(&target).expect("create target directory");
        fs::write(target.join("inside"), b"fixture").expect("write target entry");
        symlink(&target, &link).expect("replace scheduled directory with a link");

        let mut listing = Listing::default();
        let error = crate::read_native_or_portable(
            &mut listing,
            |_| Err(std::io::Error::from(std::io::ErrorKind::Unsupported)),
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "portable fallback would lose no-follow",
                ))
            },
        )
        .expect_err("a safe fallback never reopens the changed path");
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);

        // A user-provided root is not a scheduled descendant. It keeps the
        // portable reader's long-standing ability to start at a directory
        // symlink while only scheduled no-follow descents get O_NOFOLLOW.
        SystemBackend
            .read_directory(&link, false, false, &mut listing)
            .expect("a directory symlink remains a valid root");
        assert!(listing.contains("inside"));
        fs::remove_dir_all(root).expect("remove no-follow fallback fixture");
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
                // The native reader has already opened `descendant` with
                // O_NOFOLLOW. Replacing its path here makes a path-based
                // fallback escape to `target`, while the descriptor fallback
                // still enumerates the opened subtree.
                fs::rename(&descendant, &moved).expect("move opened directory");
                symlink(&target, &descendant).expect("replace path with a link");
                Err(NativeDirectoryReadError::CapabilityUnavailable(
                    std::io::Error::from_raw_os_error(45),
                ))
            },
        )
        .expect("unsupported native read degrades through the descriptor");

        assert!(used_portable_fallback);
        assert!(listing.contains("nested"), "the descendant is not lost");
        assert!(
            !listing.contains("escaped"),
            "the fallback never reopens the swapped path"
        );
        fs::remove_dir_all(root).expect("remove descriptor fallback fixture");
    }

    #[test]
    fn entry_unsupported_after_a_batch_never_restarts_the_descriptor_reader() {
        let root = fixture_root("entry-unsupported-after-batch");
        fs::create_dir_all(&root).expect("create fixture directory");
        fs::write(root.join("from-portable-fallback"), b"fixture")
            .expect("write portable fallback marker");
        let mut records = record(b"sibling", DT_REG);
        records.extend(record(b"unknown", 0));

        let mut buffer = vec![0_u8; BUFFER_SIZE];
        let mut listing = Listing::default();
        let error = read_open_directory_with_portable_fallback(
            &root,
            None,
            true,
            &mut buffer,
            &mut listing,
            |_, path, _, listing| {
                // This models a native call that accepted `records` as its
                // first batch. The DT_UNKNOWN classification then reports an
                // ordinary metadata Unsupported error, which is not a reader
                // capability signal even though it has the same ErrorKind.
                listing.clear();
                parse_records_with_entry_kind(path, &records, listing, |_, _, directory_type| {
                    if directory_type == 0 {
                        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
                    } else {
                        Ok(Some((false, false)))
                    }
                })?;
                Ok(())
            },
        )
        .expect_err("an entry metadata error is not a capability fallback");

        assert_eq!(
            error.into_io_error().kind(),
            std::io::ErrorKind::Unsupported,
            "the entry failure still reaches the walker error policy"
        );
        assert!(
            listing.contains("sibling"),
            "the accepted batch is retained"
        );
        assert!(
            !listing.contains("from-portable-fallback"),
            "the advanced descriptor was never resumed through fdopendir"
        );
        fs::remove_dir_all(root).expect("remove fixture directory");
    }

    #[test]
    fn checked_in_macos_native_fuzz_seeds_are_valid_and_reproducible() {
        let long_name = [b'x'; 255];
        let mut multi = record(b"nested", DT_DIR);
        multi.extend(record(b"file", DT_REG));
        let dirent_seeds: [(&[u8], Vec<u8>); 4] = [
            (
                include_bytes!("../../../fuzz/corpus/macos_dirent_parser/single-regular"),
                record(b"one", DT_REG),
            ),
            (
                include_bytes!("../../../fuzz/corpus/macos_dirent_parser/minimal-name"),
                record(b"a", DT_REG),
            ),
            (
                include_bytes!("../../../fuzz/corpus/macos_dirent_parser/multi-directory-regular"),
                multi,
            ),
            (
                include_bytes!("../../../fuzz/corpus/macos_dirent_parser/long-name"),
                record(&long_name, DT_REG),
            ),
        ];
        for (seed, expected) in dirent_seeds {
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
            .expect("dirent seed reaches the parser visitor");
            assert!(records > 0);
        }

        let bulk_seeds: [(&[u8], Vec<u8>); 3] = [
            (
                include_bytes!("../../../fuzz/corpus/macos_bulk_record_parser/name-and-type"),
                bulk_record(b"entry", VDIR),
            ),
            (
                include_bytes!("../../../fuzz/corpus/macos_bulk_record_parser/no-object-type"),
                bulk_record_without(b"unknown", ATTR_CMN_OBJTYPE),
            ),
            (
                include_bytes!("../../../fuzz/corpus/macos_bulk_record_parser/long-name"),
                bulk_record(&long_name, VDIR),
            ),
        ];
        for (seed, expected) in bulk_seeds {
            assert_eq!(
                seed,
                expected.as_slice(),
                "seed matches the generator record"
            );
            assert!(
                parse_bulk_record(seed)
                    .expect("bulk seed passes structural validation")
                    .is_some(),
                "bulk seed reaches a usable parser record"
            );
        }
    }

    #[test]
    fn bulk_capability_errors_fall_back_only_before_the_first_batch() {
        for error in [
            std::io::Error::from_raw_os_error(22),
            std::io::Error::from_raw_os_error(45),
        ] {
            assert!(is_unsupported_bulk_error(&error, false));
            assert!(!is_unsupported_bulk_error(&error, true));
        }
        assert!(!is_unsupported_bulk_error(
            &std::io::Error::from_raw_os_error(13),
            false
        ));
    }

    #[test]
    fn direntries_capability_errors_fall_back_only_before_the_first_batch() {
        for error in [
            std::io::Error::from_raw_os_error(22),
            std::io::Error::from_raw_os_error(45),
        ] {
            assert!(is_unsupported_direntries_error(&error, false));
            assert!(!is_unsupported_direntries_error(&error, true));
        }
        assert!(!is_unsupported_direntries_error(
            &std::io::Error::from_raw_os_error(13),
            false
        ));
    }

    #[test]
    fn unsupported_native_reader_uses_the_portable_system_fallback() {
        let root = fixture_root("fallback");
        fs::create_dir_all(&root).expect("create fallback fixture");
        fs::write(root.join("survivor"), b"fixture").expect("write fallback entry");
        let mut listing = Listing::default();
        // This is the same `Unsupported` result the reader emits after a raw
        // ENOTSUP or pre-first-batch EINVAL. Exercise the adapter without
        // mutating its process-wide capability latch during parallel tests.
        crate::read_native_or_portable(
            &mut listing,
            |_| Err(std::io::Error::from(std::io::ErrorKind::Unsupported)),
            |listing| StdBackend.read_directory(&root, false, false, listing),
        )
        .expect("unsupported native read falls back portably");
        assert!(listing.contains("survivor"));
        fs::remove_dir_all(root).expect("remove fallback fixture");
    }

    #[test]
    fn direntries_extended_tail_marks_the_final_data_batch_without_expanding_parse_bounds() {
        let mut buffer = vec![0_u8; 1024];
        let record = record(b"entry", DT_REG);
        buffer[..record.len()].copy_from_slice(&record);
        let tail_start = buffer.len() - 4;
        buffer[tail_start..].copy_from_slice(&1_u32.to_ne_bytes());
        let mut listing = Listing::default();
        parse_records(Path::new("/tmp"), &buffer[..record.len()], &mut listing)
            .expect("the parser sees only returned dirent bytes");
        assert_eq!(listing.entries()[0].name(), "entry");
        assert!(has_direntries_eof_flag(&buffer));
        assert!(!has_direntries_eof_flag(&buffer[..1023]));
    }

    #[test]
    fn direntries_nonwriting_extended_tail_cannot_end_a_full_batch_early() {
        // Simulate a pre-10.15 successful syscall: it fills every returned
        // dirent byte but does not know about, and therefore does not touch,
        // the optional EOF word at the end of the extended buffer.
        let mut buffer = vec![0_u8; 1040];
        let record = record(b"entry", DT_REG);
        let byte_count = buffer.len() - std::mem::size_of::<u32>();
        assert_eq!(byte_count % record.len(), 0, "the mock fills a whole batch");
        buffer[byte_count..].copy_from_slice(&GETDIRENTRIES64_EOF.to_ne_bytes());

        // This is the pre-syscall operation in
        // `read_direntries_from_open_directory`; the mocked syscall below
        // deliberately leaves the tail unchanged.
        clear_direntries_eof_tail(&mut buffer);
        for slot in buffer[..byte_count].chunks_exact_mut(record.len()) {
            slot.copy_from_slice(&record);
        }

        let mut listing = Listing::default();
        parse_records(Path::new("/tmp"), &buffer[..byte_count], &mut listing)
            .expect("all returned records still parse");
        assert_eq!(listing.entries().len(), byte_count / record.len());
        assert!(
            !has_direntries_eof_flag(&buffer),
            "a kernel that does not write the extension means no EOF"
        );
    }

    #[test]
    fn native_readers_match_the_portable_reader() {
        let root = std::env::temp_dir().join(format!(
            "ferralk-macos-native-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        ));
        fs::create_dir_all(root.join("nested")).expect("create native fixture");
        fs::write(root.join("file.txt"), b"fixture").expect("write native fixture");
        symlink("file.txt", root.join("link.txt")).expect("create native fixture symlink");

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
        let portable = describe(&|listing| {
            StdBackend
                .read_directory(&root, false, false, listing)
                .expect("portable reader succeeds");
        });
        // What the walk actually reads through.
        assert_eq!(
            describe(&|listing| {
                read_directory(&root, None, false, listing).expect("native reader succeeds");
            }),
            portable
        );
        assert_eq!(
            describe(&|listing| {
                read_direntries_directory(&root, None, false, listing)
                    .expect("direntries reader succeeds");
            }),
            portable
        );
        // The bulk reader is no longer on any walk's path. It is still held to
        // the same answer, which is what makes it a regression boundary rather
        // than dead code kept for sentiment.
        assert_eq!(
            describe(&|listing| {
                read_bulk_directory(&root, listing).expect("bulk reader succeeds");
            }),
            portable
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

    fn fixture_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ferralk-macos-native-{label}-{}-{}",
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
