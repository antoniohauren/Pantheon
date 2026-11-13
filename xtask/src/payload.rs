//! `cargo xtask payload`: builds `console/hermes-payload` for
//! `x86_64-unknown-none` (D33), converts the ELF to a flat binary, checks it
//! and stamps its build ID (docs/gods/hermes.md "Payload").
//!
//! Checks: entry at offset 0, no relocations left for the loader, no dynamic
//! linker, size within the 64 KiB budget (D27), the config marker exactly
//! once, and a reproducible build (two clean builds, same bytes).
//!
//! The build ID is the first 16 bytes of the SHA-256 of the flat image as
//! built (config block unpatched, build-ID field zero); it is then stamped
//! into the config block. Output: `target/hermes-payload/hermes-payload.bin`,
//! which the `hermes` feature `embed-payload` includes (D21).

use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_wire::BUILD_ID_LEN;
use hermes_wire::config;
use pantheon_crypto::hash::Sha256;

use crate::deps::workspace_root;
use crate::elf::{self, Flat};

/// A check failed (no D31 code fits; the generic failure code, as `deps` uses).
const EXIT_CHECK: u8 = 1;
const EXIT_USAGE: u8 = 2;
/// Running cargo or reading/writing files failed (D31 `Io`).
const EXIT_IO: u8 = 8;
const TARGET: &str = "x86_64-unknown-none";
const MANIFEST: &str = "console/hermes-payload/Cargo.toml";
const BIN: &str = "hermes-payload";
/// Under the workspace target directory.
const OUT_DIR: &str = "hermes-payload";
const OUT_FILE: &str = "hermes-payload.bin";
/// D27: the payload's size budget.
pub const SIZE_BUDGET: usize = 64 * 1024;
/// How much of a failed build's stderr to show.
const STDERR_TAIL: usize = 2000;

pub fn run(args: &[String]) -> (u8, String) {
    if !args.is_empty() {
        return (EXIT_USAGE, "usage: cargo xtask payload".into());
    }
    match build_and_check() {
        Ok(message) => (0, message),
        Err(failure) => failure,
    }
}

fn build_and_check() -> Result<String, (u8, String)> {
    let root = workspace_root();
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| root.join("target"), PathBuf::from)
        .join(OUT_DIR);
    let first = flat_build(&root, &target_dir.join("build-a"))?;
    let second = flat_build(&root, &target_dir.join("build-b"))?;
    let mut failures = check(&first);
    if first.image != second.image {
        failures.push("not reproducible: two clean builds differ".into());
    }
    if !failures.is_empty() {
        return Err((
            EXIT_CHECK,
            format!("xtask payload: checks failed:\n  {}", failures.join("\n  ")),
        ));
    }
    let id = build_id(&first.image);
    let mut image = first.image;
    config::stamp_build_id(&mut image, &id)
        .map_err(|e| (EXIT_CHECK, format!("xtask payload: {e}")))?;
    let out = target_dir.join(OUT_FILE);
    std::fs::write(&out, &image).map_err(|e| {
        (
            EXIT_IO,
            format!("xtask payload: writing {}: {e}", out.display()),
        )
    })?;
    Ok(format!(
        "xtask payload: {} ({} of {SIZE_BUDGET} bytes), build ID {}\n  entry at offset 0, no relocations, marker once, reproducible",
        out.display(),
        image.len(),
        hex(&id)
    ))
}

/// One clean release build, flattened.
fn flat_build(root: &Path, target_dir: &Path) -> Result<Flat, (u8, String)> {
    if target_dir.exists() {
        std::fs::remove_dir_all(target_dir).map_err(|e| {
            (
                EXIT_IO,
                format!("xtask payload: cleaning {}: {e}", target_dir.display()),
            )
        })?;
    }
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(root)
        .args([
            "build",
            "--release",
            "--locked",
            "--manifest-path",
            MANIFEST,
            "--target",
            TARGET,
            "--bin",
            BIN,
            "--target-dir",
        ])
        .arg(target_dir)
        // The host's flags (e.g. -C target-cpu=native) must not leak into the payload.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .map_err(|e| (EXIT_IO, format!("xtask payload: running cargo build: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail_start = stderr.len().saturating_sub(STDERR_TAIL);
        let tail = stderr.get(tail_start..).unwrap_or(&stderr);
        return Err((
            EXIT_IO,
            format!(
                "xtask payload: cargo build for {TARGET} failed:\n{tail}\n\
                 The toolchain needs the {TARGET} target's precompiled `core` \
                 (rust-toolchain.toml lists it; a rustup toolchain installs it). \
                 See docs/hermes-provenance.md, S5."
            ),
        ));
    }
    let elf_path = target_dir.join(TARGET).join("release").join(BIN);
    let bytes = std::fs::read(&elf_path).map_err(|e| {
        (
            EXIT_IO,
            format!("xtask payload: reading {}: {e}", elf_path.display()),
        )
    })?;
    elf::flatten(&bytes).map_err(|e| (EXIT_CHECK, format!("xtask payload: {e}")))
}

/// Every check that fails, as one message each.
pub fn check(flat: &Flat) -> Vec<String> {
    let mut failures = Vec::new();
    if flat.entry_offset != 0 {
        failures.push(format!(
            "entry is at offset {:#x} of the flat binary, not 0",
            flat.entry_offset
        ));
    }
    if flat.relocations != 0 {
        failures.push(format!(
            "{} relocation(s) left for the loader to apply",
            flat.relocations
        ));
    }
    if flat.has_interpreter {
        failures.push("the ELF asks for a dynamic linker (PT_INTERP)".into());
    }
    if flat.image.len() > SIZE_BUDGET {
        failures.push(format!(
            "flat binary is {} bytes, over the {SIZE_BUDGET}-byte budget",
            flat.image.len()
        ));
    }
    if let Err(e) = config::find(&flat.image) {
        failures.push(format!("config block: {e}"));
    }
    failures
}

fn build_id(image: &[u8]) -> [u8; BUILD_ID_LEN] {
    let digest = Sha256::digest(image);
    let mut id = [0u8; BUILD_ID_LEN];
    id.copy_from_slice(&digest[..BUILD_ID_LEN]);
    id
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::tests::synthetic;

    fn image_with_marker(code_prefix: &[u8]) -> Vec<u8> {
        let mut code = code_prefix.to_vec();
        code.extend_from_slice(&config::template());
        code
    }

    #[test]
    fn a_good_image_passes() {
        let elf = synthetic(&image_with_marker(b"\xEB\xFE"), 0, 0, 0, false);
        assert!(check(&elf::flatten(&elf).unwrap()).is_empty());
    }

    #[test]
    fn each_check_reports_its_failure() {
        let code = image_with_marker(b"\xEB\xFE");
        let entry = check(&elf::flatten(&synthetic(&code, 0, 2, 0, false)).unwrap());
        assert!(entry[0].contains("offset 0x2"), "{entry:?}");
        let relocs = check(&elf::flatten(&synthetic(&code, 0, 0, 0, true)).unwrap());
        assert!(relocs[0].contains("1 relocation"), "{relocs:?}");
        let big = check(&elf::flatten(&synthetic(&code, 0, 0, SIZE_BUDGET as u64, false)).unwrap());
        assert!(big[0].contains("budget"), "{big:?}");
        let none = check(&elf::flatten(&synthetic(b"no marker", 0, 0, 0, false)).unwrap());
        assert!(none[0].contains("not found"), "{none:?}");
        let mut twice = code.clone();
        twice.extend_from_slice(&config::template());
        let twice = check(&elf::flatten(&synthetic(&twice, 0, 0, 0, false)).unwrap());
        assert!(twice[0].contains("more than once"), "{twice:?}");
    }

    #[test]
    fn build_id_is_a_stable_sha256_prefix() {
        let a = build_id(b"payload");
        assert_eq!(a, build_id(b"payload"));
        assert_ne!(a, build_id(b"payload2"));
        assert_eq!(a.len(), BUILD_ID_LEN);
    }
}
