#![no_main]

use compat_quake::lmp::{parse_lmp_image, parse_palette};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_lmp_image(data);
    let _ = parse_palette(data);
});
