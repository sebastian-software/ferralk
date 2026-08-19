//! macOS bulk-directory backend.
//!
//! The only unsafe operations are Darwin syscalls. Their buffers are owned byte
//! vectors that remain live for each call; every kernel record is bounds-checked
//! before its fields or name are read. `getattrlistbulk` is preferred because it
//! returns entry names and object types in batches. Filesystems that do not
//! support it report an unsupported operation so the safe adapter can use the
//! portable backend. The `getdirentries64` reader remains a separately tested
//! parser regression boundary.

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
const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const FSOPT_NOFOLLOW: u64 = 0x0000_0001;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;
const ATTRIBUTE_SET_SIZE: usize = 5 * std::mem::size_of::<u32>();
const ATTRIBUTE_RECORD_HEADER_SIZE: usize = std::mem::size_of::<u32>() + ATTRIBUTE_SET_SIZE;

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

pub(super) fn read_directory(path: &Path) -> io::Result<Vec<BackendEntry>> {
    read_bulk_directory(path)
}

fn read_bulk_directory(path: &Path) -> io::Result<Vec<BackendEntry>> {
    let directory = File::open(path)?;
    let mut buffer = vec![0_u8; BUFFER_SIZE];
    let mut entries = Vec::new();
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
        buffer: &mut buffer,
        attributes,
        remaining: 0,
        offset: 0,
        primed: false,
    };
    while let Some((name, object_type)) = reader.next()? {
        let path = path.join(OsString::from_vec(name));
        let (is_dir, is_symlink) = bulk_entry_kind(&path, object_type)?;
        entries.push(BackendEntry {
            path,
            is_dir,
            is_symlink,
        });
    }
    Ok(entries)
}

struct BulkReader<'buffer> {
    directory: File,
    buffer: &'buffer mut [u8],
    attributes: AttrList,
    remaining: usize,
    offset: usize,
    primed: bool,
}

impl<'buffer> BulkReader<'buffer> {
    fn next(&mut self) -> io::Result<Option<(Vec<u8>, u32)>> {
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
                return Ok(Some((name.to_vec(), object_type)));
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

fn parse_bulk_record(record: &[u8]) -> io::Result<Option<(&[u8], u32)>> {
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
        return Err(malformed_bulk_record());
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
    if name.is_empty() || name.contains(&0) || name.contains(&b'/') {
        return Err(malformed_bulk_record());
    }
    if common_attributes & ATTR_CMN_OBJTYPE == 0 {
        return Err(malformed_bulk_record());
    }
    let object_type = read_u32(record, offset)?;
    Ok(Some((name, object_type)))
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

fn bulk_entry_kind(path: &Path, object_type: u32) -> io::Result<(bool, bool)> {
    match object_type {
        VDIR => Ok((true, false)),
        VREG => Ok((false, false)),
        VLNK => Ok((false, true)),
        _ => entry_kind(path, 0),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn read_direntries_directory(path: &Path) -> io::Result<Vec<BackendEntry>> {
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

fn malformed_bulk_record() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed getattrlistbulk record",
    )
}

#[cfg_attr(not(test), allow(dead_code))]
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
        if name.contains(&0) || name.contains(&b'/') {
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
    io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed getdirentries64 record",
    )
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
        ATTR_CMN_ERROR, ATTR_CMN_NAME, ATTR_CMN_OBJTYPE, ATTR_CMN_RETURNED_ATTRS,
        ATTRIBUTE_RECORD_HEADER_SIZE, BackendEntry, DT_DIR, DT_REG, NAME_OFFSET, VDIR,
        is_unsupported_bulk_error, parse_bulk_record, parse_records, read_directory,
        read_direntries_directory,
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
    fn bulk_parser_validates_attribute_references_and_entry_errors() {
        let record = bulk_record(b"nested", VDIR);
        let (name, object_type) = parse_bulk_record(&record)
            .expect("valid bulk record")
            .expect("entry has no error");
        assert_eq!(name, b"nested");
        assert_eq!(object_type, VDIR);

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

        let mut bulk = read_directory(&root).expect("bulk reader succeeds");
        let mut direntries = read_direntries_directory(&root).expect("direntries reader succeeds");
        let mut portable = StdBackend
            .read_directory(&root)
            .expect("portable reader succeeds");
        bulk.sort_by(|left, right| left.path.cmp(&right.path));
        direntries.sort_by(|left, right| left.path.cmp(&right.path));
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
        let portable = describe(portable);
        assert_eq!(describe(bulk), portable);
        assert_eq!(describe(direntries), portable);
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
        let mut state = crate::WalkState::new(walker);
        state
            .walk_directory(&StdBackend, walker.root.clone())
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
