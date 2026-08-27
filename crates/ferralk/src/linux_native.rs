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
    ffi::{OsStr, c_long, c_void},
    fs::{self, File, OpenOptions},
    io,
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawFd},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use super::{Listing, defer_entry_stat_error};

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

/// `O_DIRECTORY`. Most architectures take it from `asm-generic`; 32-bit ARM
/// defines its own value.
#[cfg(not(target_arch = "arm"))]
const O_DIRECTORY: i32 = 0o200_000;
#[cfg(target_arch = "arm")]
const O_DIRECTORY: i32 = 0o40_000;

/// Set once the kernel reports that `getdents64` is unavailable.
///
/// Whether the syscall exists is a property of the kernel and the build, not
/// of one directory, so probing again for every directory only pays a failed
/// syscall and a second open. A filesystem-specific refusal is rarer than a
/// missing syscall; per-device memoization would need the device id before the
/// first read, which costs the stat this latch exists to avoid.
static GETDENTS_UNSUPPORTED: AtomicBool = AtomicBool::new(false);

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
    refuse_final_symlink: bool,
    listing: &mut Listing,
) -> io::Result<()> {
    if GETDENTS_UNSUPPORTED.load(Ordering::Relaxed) {
        return Err(unsupported("getdents64 is unavailable on this system"));
    }
    let result = DIRECTORY_BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        read_directory_with_buffer(path, refuse_final_symlink, &mut buffer[..], listing)
    });
    if result
        .as_ref()
        .is_err_and(|error| error.kind() == io::ErrorKind::Unsupported)
    {
        GETDENTS_UNSUPPORTED.store(true, Ordering::Relaxed);
    }
    result
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
/// a symlink is therefore refused (`ELOOP` or `ENOTDIR`, according to the
/// kernel's flag handling) instead of escaping through that link. User-supplied
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

fn read_directory_with_buffer(
    path: &Path,
    refuse_final_symlink: bool,
    buffer: &mut [u8],
    listing: &mut Listing,
) -> io::Result<()> {
    let directory = open_directory(path, refuse_final_symlink)?;
    listing.clear();
    loop {
        let byte_count = read_batch(&directory, buffer)?;
        if byte_count == 0 {
            return Ok(());
        }
        parse_records(path, &buffer[..byte_count], listing)?;
    }
}

fn read_batch(directory: &File, buffer: &mut [u8]) -> io::Result<usize> {
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
        if error.raw_os_error() == Some(38) {
            return Err(io::Error::new(io::ErrorKind::Unsupported, error));
        }
        return Err(error);
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
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "getdents64 has no reviewed syscall number for this architecture",
        ))
    }
}

fn parse_records(directory: &Path, records: &[u8], listing: &mut Listing) -> io::Result<()> {
    for_each_record(records, |name, directory_type| {
        let name = OsStr::from_bytes(name);
        // An entry that vanished between the read and its stat costs that one
        // entry; the rest of the listing is still valid and is returned.
        match entry_kind(directory, name, directory_type) {
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
        fs,
        os::unix::fs::symlink,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::{DirectoryBackend, ErrorPolicy, StdBackend, WalkEntry, WalkOptions, Walker};

    use super::{
        DT_BLK, DT_CHR, DT_DIR, DT_FIFO, DT_REG, DT_SOCK, Listing, NAME_OFFSET, TYPE_OFFSET,
        entry_kind, open_directory, parse_records, read_directory,
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
        read_directory(&link, false, &mut listing).expect("follow mode still opens through a link");
        assert!(listing.contains("inside"));
        fs::remove_dir_all(root).expect("remove no-follow fixture");
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
                read_directory(&root, false, listing).expect("native reader succeeds");
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
        let task = crate::DirectoryTask {
            path: root.clone(),
            depth: 0,
            root: 0,
            cycle_guard: std::sync::Arc::new(crate::CycleGuard::default()),
            ignores: crate::IgnoreScope::for_root(walker, &StdBackend, &root),
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
