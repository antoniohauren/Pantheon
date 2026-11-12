//! Parses arbitrary bytes with pantheon-xml and checks the one-sided
//! differential rule against roxmltree (crates/xml/tests/oracle).

#![no_main]

#[path = "../../crates/xml/tests/oracle/mod.rs"]
mod oracle;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let Err(why) = oracle::check(data) {
        panic!("{why}");
    }
});
