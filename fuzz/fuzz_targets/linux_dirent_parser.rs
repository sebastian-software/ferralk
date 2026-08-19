#![no_main]

use ferralk::fuzz_validate_linux_dirent_records;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fuzz_validate_linux_dirent_records(data);
});
