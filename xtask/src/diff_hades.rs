//! `cargo xtask diff hades INPUT…`: extraction manifest parity against pkg-tools
//! (docs/gods/hades.md "Tests"): the same paths, kinds, sizes and file bytes.
//!
//! Both trees are written under `target/xtask-diff-hades/` and removed after
//! each input. Reports print input numbers and counts, never names.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Bytes compared per read.
const CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Dir,
    File(u64),
    Other,
}

/// Every entry below `root`, keyed by `/`-separated relative path.
fn manifest(root: &Path) -> io::Result<BTreeMap<String, Kind>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let meta = entry.path().symlink_metadata()?;
            let kind = if meta.is_dir() {
                pending.push((entry.path(), path.clone()));
                Kind::Dir
            } else if meta.is_file() {
                Kind::File(meta.len())
            } else {
                Kind::Other
            };
            out.insert(path, kind);
        }
    }
    Ok(out)
}

fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    let (mut x, mut y) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        let n = read_full(&mut a, &mut x)?;
        let m = read_full(&mut b, &mut y)?;
        if n != m || x.get(..n) != y.get(..m) {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(r: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0usize;
    while let Some(rest) = buf.get_mut(got..).filter(|r| !r.is_empty()) {
        match r.read(rest)? {
            0 => break,
            n => got = got.saturating_add(n),
        }
    }
    Ok(got)
}

/// Mismatch counts between the oracle tree and ours, as report lines.
pub fn compare_trees(oracle: &Path, ours: &Path) -> io::Result<Vec<String>> {
    let a = manifest(oracle)?;
    let b = manifest(ours)?;
    let only_oracle = a.keys().filter(|k| !b.contains_key(*k)).count();
    let only_ours = b.keys().filter(|k| !a.contains_key(*k)).count();
    let (mut kind, mut content, mut files, mut bytes) = (0usize, 0usize, 0usize, 0u64);
    for (path, ka) in &a {
        let Some(kb) = b.get(path) else { continue };
        match (ka, kb) {
            (Kind::Dir, Kind::Dir) => {}
            (Kind::File(x), Kind::File(y)) if x == y => {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(*x);
                if !same_bytes(&oracle.join(path), &ours.join(path))? {
                    content = content.saturating_add(1);
                }
            }
            _ => kind = kind.saturating_add(1),
        }
    }
    let mut lines = Vec::new();
    for (n, what) in [
        (only_oracle, "paths only in pkg-tools output"),
        (only_ours, "paths only in hades output"),
        (kind, "paths with a different kind or size"),
        (content, "files with different bytes"),
    ] {
        if n != 0 {
            lines.push(format!("{n} {what}"));
        }
    }
    if lines.is_empty() {
        let dirs = a.values().filter(|k| **k == Kind::Dir).count();
        lines.push(format!("ok: {files} files, {dirs} folders, {bytes} bytes"));
    }
    Ok(lines)
}

fn extract(program: &Path, args: &[&str], input: &Path, out: &Path) -> Result<bool, String> {
    Command::new(program)
        .args(args)
        .arg(input)
        .arg(out)
        .output()
        .map(|o| o.status.success())
        .map_err(|e| format!("running {}: {e}", program.display()))
}

/// Runs one input; `Ok(lines)` with `lines[0]` starting `ok:` on parity.
pub fn run_input(
    oracle: &Path,
    ours: &Path,
    input: &Path,
    work: &Path,
) -> Result<Vec<String>, String> {
    let io = |e: io::Error| format!("{}: {e}", work.display());
    if work.exists() {
        fs::remove_dir_all(work).map_err(io)?;
    }
    fs::create_dir_all(work).map_err(io)?;
    let (a, b): (PathBuf, PathBuf) = (work.join("oracle"), work.join("ours"));
    let ra = extract(oracle, &["extract"], input, &a)?;
    let rb = extract(ours, &["extract"], input, &b)?;
    let lines = match (ra, rb) {
        (false, false) => vec!["ok: both refused".to_owned()],
        (true, false) => vec!["pkg-tools succeeded, hades failed".to_owned()],
        (false, true) => vec!["pkg-tools failed, hades succeeded".to_owned()],
        (true, true) => compare_trees(&a, &b).map_err(io)?,
    };
    fs::remove_dir_all(work).map_err(io)?;
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trees_compare_by_path_kind_size_and_bytes() {
        let root = std::env::temp_dir().join(format!("xtask-diff-hades-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        for t in [&a, &b] {
            fs::create_dir_all(t.join("d/e")).unwrap();
            fs::write(t.join("d/f"), b"same").unwrap();
        }
        assert!(compare_trees(&a, &b).unwrap()[0].starts_with("ok: 1 files, 2 folders"));
        fs::write(b.join("d/f"), b"diff").unwrap();
        fs::write(b.join("extra"), b"").unwrap();
        let lines = compare_trees(&a, &b).unwrap();
        assert_eq!(
            lines,
            [
                "1 paths only in hades output",
                "1 files with different bytes"
            ]
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
