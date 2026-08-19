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
    ffi::{OsString, c_long, c_void},
    fs::{self, File},
    io,
    os::unix::{ffi::OsStringExt, io::AsRawFd},
    path::Path,
};

use super::BackendEntry;

const BUFFER_SIZE: usize = 32 * 1024;
const RECORD_LENGTH_OFFSET: usize = 16;
const TYPE_OFFSET: usize = 18;
const NAME_OFFSET: usize = 19;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

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

pub(super) fn read_directory(path: &Path) -> io::Result<Vec<BackendEntry>> {
    DIRECTORY_BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();
        read_directory_with_buffer(path, &mut buffer[..])
    })
}

fn read_directory_with_buffer(path: &Path, buffer: &mut [u8]) -> io::Result<Vec<BackendEntry>> {
    let directory = File::open(path)?;
    let mut entries = Vec::new();
    loop {
        let byte_count = read_batch(&directory, buffer)?;
        if byte_count == 0 {
            return Ok(entries);
        }
        parse_records(path, &buffer[..byte_count], &mut entries)?;
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

fn parse_records(
    directory: &Path,
    records: &[u8],
    entries: &mut Vec<BackendEntry>,
) -> io::Result<()> {
    for_each_record(records, |name, directory_type| {
        let path = directory.join(OsString::from_vec(name.to_vec()));
        let (is_dir, is_symlink) = entry_kind(&path, directory_type)?;
        entries.push(BackendEntry {
            path,
            is_dir,
            is_symlink,
        });
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
        let Some(name_length) = name_and_padding.iter().position(|&byte| byte == 0) else {
            return Err(malformed_record());
        };
        let name = &name_and_padding[..name_length];
        offset = offset
            .checked_add(record_length)
            .ok_or_else(malformed_record)?;
        if name.is_empty() || name == b"." || name == b".." {
            continue;
        }
        if name.contains(&b'/') {
            return Err(malformed_record());
        }
        visit(name, record[TYPE_OFFSET])?;
    }
    Ok(())
}

fn entry_kind(path: &Path, directory_type: u8) -> io::Result<(bool, bool)> {
    match directory_type {
        DT_DIR => Ok((true, false)),
        DT_REG => Ok((false, false)),
        DT_LNK => Ok((false, true)),
        _ => {
            let file_type = fs::symlink_metadata(path)?.file_type();
            Ok((file_type.is_dir(), file_type.is_symlink()))
        }
    }
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
        BackendEntry, DT_DIR, DT_REG, NAME_OFFSET, TYPE_OFFSET, parse_records, read_directory,
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
        let mut entries: Vec<BackendEntry> = Vec::new();
        parse_records(Path::new("/tmp"), &records, &mut entries).expect("dot records parse");
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_dir);
        assert!(!entries[0].is_symlink);

        assert!(parse_records(Path::new("/tmp"), &[0_u8; NAME_OFFSET], &mut entries).is_err());

        let mut zero_length = vec![0_u8; NAME_OFFSET + 1];
        zero_length[NAME_OFFSET] = DT_REG;
        assert!(parse_records(Path::new("/tmp"), &zero_length, &mut entries).is_err());

        let mut missing_nul = record(b"name", DT_REG);
        for byte in &mut missing_nul[NAME_OFFSET..] {
            *byte = b'x';
        }
        assert!(parse_records(Path::new("/tmp"), &missing_nul, &mut entries).is_err());
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

        let mut native = read_directory(&root).expect("native reader succeeds");
        let mut portable = StdBackend
            .read_directory(&root)
            .expect("portable reader succeeds");
        native.sort_by(|left, right| left.path.cmp(&right.path));
        portable.sort_by(|left, right| left.path.cmp(&right.path));
        let describe = |entries: Vec<BackendEntry>| {
            entries
                .into_iter()
                .map(|entry| {
                    (
                        entry
                            .path
                            .strip_prefix(&root)
                            .expect("entry belongs to fixture")
                            .to_path_buf(),
                        entry.is_dir,
                        entry.is_symlink,
                    )
                })
                .collect::<Vec<(PathBuf, bool, bool)>>()
        };
        assert_eq!(describe(native), describe(portable));
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
        let mut state = crate::WalkState::new(walker);
        let task = crate::DirectoryTask {
            path: walker.root.clone(),
            ignores: crate::IgnoreScope::root(walker, &StdBackend),
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
