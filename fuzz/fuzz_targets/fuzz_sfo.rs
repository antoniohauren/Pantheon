//! Parses arbitrary bytes as PARAM.SFO; anything that parses must serialize
//! (as 1.1) and parse back to the same entries.

#![no_main]

use pantheon_core::Limits;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let limits = Limits::DEFAULT;
    if let Ok(mut sfo) = pantheon_psf::parse(data, &limits) {
        sfo.version = pantheon_psf::VERSION_1_1;
        let out = pantheon_psf::serialize(&sfo, &limits).expect("parsed SFO serializes");
        let again = pantheon_psf::parse(&out, &limits).expect("serialized SFO parses");
        assert_eq!(again.entries, sfo.entries);
    }
});
