//! Differential test against roxmltree over the seed corpus, committed
//! regression inputs and `Corruptor` mutations of every seed (D20, in-`cargo test` layer).
//!
//! Seeds named `reject-*.xml` must be rejected by pantheon-xml; all others accepted.

mod oracle;

use std::fs;
use std::path::{Path, PathBuf};

use pantheon_core::Limits;
use pantheon_core::testing::Corruptor;

const SEEDS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fuzz/seeds/xml");
const REGRESSIONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/regressions");
/// Random mutations per seed, on top of every truncation.
const ROUNDS: usize = 300;
const SEED_BASE: u64 = 0x5EED_0000;
const MIN_SEEDS: usize = 15;

fn inputs(dir: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.map(|e| e.expect("dir entry").path()).collect();
    paths.sort();
    paths
}

fn name(path: &Path) -> &str {
    path.file_name()
        .and_then(|n| n.to_str())
        .expect("utf-8 name")
}

#[test]
fn seeds_follow_their_expectation() {
    let seeds = inputs(SEEDS);
    assert!(seeds.len() >= MIN_SEEDS, "only {} seeds", seeds.len());
    for path in &seeds {
        let bytes = fs::read(path).expect("read seed");
        let result = pantheon_xml::parse(&bytes, &Limits::DEFAULT);
        if name(path).starts_with("reject-") {
            // Whether the class is explained is the oracle's job (`seeds_and_regressions_match_roxmltree`).
            result.expect_err(name(path));
        } else {
            result.unwrap_or_else(|e| panic!("{}: {e}", name(path)));
        }
    }
}

#[test]
fn seeds_and_regressions_match_roxmltree() {
    for path in inputs(SEEDS).iter().chain(&inputs(REGRESSIONS)) {
        let bytes = fs::read(path).expect("read input");
        if let Err(why) = oracle::check(&bytes) {
            panic!("{}: {why}", name(path));
        }
    }
}

#[test]
fn mutations_match_roxmltree() {
    let mut failures = Vec::new();
    for (index, path) in (0u64..).zip(inputs(SEEDS)) {
        let bytes = fs::read(&path).expect("read seed");
        let seed = SEED_BASE.wrapping_add(index);
        Corruptor::new(seed).run(&bytes, ROUNDS, |mutated| {
            if let Err(why) = oracle::check(mutated) {
                failures.push(format!(
                    "{} (seed {seed:#x}): {why}\n input: {:?}",
                    name(&path),
                    String::from_utf8_lossy(mutated)
                ));
            }
        });
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
