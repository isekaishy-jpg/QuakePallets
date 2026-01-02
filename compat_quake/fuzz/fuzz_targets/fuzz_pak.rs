#![no_main]

use compat_quake::pak;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = pak::parse_pak(data.to_vec());
});
