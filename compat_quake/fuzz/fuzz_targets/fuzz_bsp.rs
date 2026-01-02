#![no_main]

use compat_quake::bsp;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = bsp::parse_bsp(data);
});
