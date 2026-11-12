# Fuzzing (D20, cargo-fuzz layer)

Standalone package (not a workspace member); needs nightly and `cargo install cargo-fuzz`.
Run from the repository root: `cargo +nightly fuzz run fuzz_xml fuzz/seeds/xml`.
`fuzz_xml` checks the one-sided roxmltree rule in `crates/xml/tests/oracle/mod.rs`.
Every crash becomes a committed regression input in `crates/xml/tests/regressions/`,
which `cargo test -p pantheon-xml` replays. `corpus/` and `artifacts/` are not committed.
