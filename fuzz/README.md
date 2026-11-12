# Fuzzing (D20, cargo-fuzz layer)

Standalone package (not a workspace member); needs nightly and `cargo install cargo-fuzz`.
Run from the repository root: `cargo +nightly fuzz run fuzz_xml fuzz/seeds/xml`.
`fuzz_xml` checks the one-sided roxmltree rule in `crates/xml/tests/oracle/mod.rs`.
`fuzz_sfo` checks that every parsed PARAM.SFO serializes and parses back to the same
entries; `fuzz_ps3_pkg` and `fuzz_ps4_pkg` open packages and read their SFO and icon
(no seeds yet; the synthetic builders behind each crate's `testing` feature make good ones).
Every crash becomes a committed regression input in `crates/xml/tests/regressions/`,
which `cargo test -p pantheon-xml` replays. `corpus/` and `artifacts/` are not committed.
