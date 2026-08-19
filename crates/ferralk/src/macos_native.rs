//! macOS `getdirentries64` directory backend.
//!
//! The only unsafe operation is the Darwin syscall. Its buffer is an owned
//! byte vector that remains live for the call; every kernel record is
//! bounds-checked before its fields or name are read. Unsupported operations
//! are reported as ordinary I/O errors so the safe adapter can use the
//! portable backend instead.

use std::{
    ffi::{OsString, c_int, c_void},
    fs::{self, File},
    io,
    os::unix::{ffi::OsStringExt, io::AsRawFd},
    path::Path,
};

use super::BackendEntry;

const BUFFER_SIZE: usize = 32 * 1024;
const RECORD_LENGTH_OFFSET: usize = 16;
const NAME_LENGTH_OFFSET: usize = 18;
const TYPE_OFFSET: usize = 20;
const NAME_OFFSET: usize = 21;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

unsafe extern "C" {
    fn __getdirentries64(
        fd: c_int,
        buffer: *mut c_void,
        buffer_size: usize,
        base: *mut u64,
    ) -> isize;
}

pub(super) fn read_directory(path: &Path) -> io::Result<Vec<BackendEntry>> {
    let directory = File::open(path)?;
    let mut base = 0_u64;
    let mut buffer = vec![0_u8; BUFFER_SIZE];
    let mut entries = Vec::new();
    loop {
        let byte_count = loop {
            // SAFETY: `buffer` is owned, writable, and lives through the call;
            // its length is passed verbatim. `base` is a valid mutable u64, and
            // `directory` owns the open descriptor for the duration of reads.
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
                return Err(error);
            }
        };
        if byte_count == 0 {
            return Ok(entries);
        }
        parse_records(path, &buffer[..byte_count], &mut entries)?;
    }
}

fn parse_records(
    directory: &Path,
    records: &[u8],
    entries: &mut Vec<BackendEntry>,
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
        if name.is_empty() || name == b"." || name == b".." || name.contains(&0) {
            if name.contains(&0) {
                return Err(malformed_record());
            }
            continue;
        }
        let path = directory.join(OsString::from_vec(name.to_vec()));
        let (is_dir, is_symlink) = entry_kind(&path, record[TYPE_OFFSET])?;
        entries.push(BackendEntry {
            path,
            is_dir,
            is_symlink,
        });
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
    io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed getdirentries64 record",
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::{DirectoryBackend, StdBackend};

    use super::{BackendEntry, DT_DIR, DT_REG, NAME_OFFSET, parse_records, read_directory};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    fn record(name: &[u8], directory_type: u8) -> Vec<u8> {
        let length = (NAME_OFFSET + name.len() + 3) & !3;
        let mut record = vec![0_u8; length];
        record[16..18].copy_from_slice(&(length as u16).to_ne_bytes());
        record[18..20].copy_from_slice(&(name.len() as u16).to_ne_bytes());
        record[20] = directory_type;
        record[NAME_OFFSET..NAME_OFFSET + name.len()].copy_from_slice(name);
        record
    }

    #[test]
    fn parser_skips_dot_entries_and_rejects_truncated_records() {
        let mut records = record(b".", DT_DIR);
        records.extend(record(b"..", DT_DIR));
        records.extend(record(b"regular", DT_REG));
        let mut entries: Vec<BackendEntry> = Vec::new();
        parse_records(Path::new("/tmp"), &records, &mut entries).expect("dot records parse");
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_dir);
        assert!(!entries[0].is_symlink);
        assert!(parse_records(Path::new("/tmp"), &[0_u8; NAME_OFFSET - 1], &mut entries).is_err());

        let mut zero_length = vec![0_u8; NAME_OFFSET];
        zero_length[18..20].copy_from_slice(&1_u16.to_ne_bytes());
        assert!(parse_records(Path::new("/tmp"), &zero_length, &mut entries).is_err());

        let mut oversized_name = record(b"x", DT_REG);
        let oversized_length = oversized_name.len() as u16;
        oversized_name[18..20].copy_from_slice(&oversized_length.to_ne_bytes());
        assert!(parse_records(Path::new("/tmp"), &oversized_name, &mut entries).is_err());
    }

    #[test]
    fn native_reader_matches_the_portable_reader() {
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
}
