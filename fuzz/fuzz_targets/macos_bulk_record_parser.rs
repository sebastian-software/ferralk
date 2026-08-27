#![no_main]

use ferralk::fuzz_validate_macos_bulk_record;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    fuzz_validate_macos_bulk_record(data);
});
