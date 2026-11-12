//! Opens arbitrary bytes as a PS4 PKG and reads its metadata; must never panic.

#![no_main]

use std::io::Cursor;

use pantheon_core::Limits;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let Ok(mut pkg) = pantheon_ps4::Package::open(Cursor::new(data), &Limits::DEFAULT) {
        let _ = pkg.read_param_sfo();
        let _ = pkg.read_icon0();
    }
});
