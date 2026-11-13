//! `cargo xtask diff proteus INPUT…`: reader-equivalence against pkg-tools (D16).
//!
//! An input is a PS4 base package, `BASE.pkg+UPDATE.pkg` for a pair, or a folder
//! (packed with `pack`). Both tools write their archives under
//! `target/xtask-diff-proteus/`; each archive pair is read back with
//! `pantheon-zar` and must hold the same paths, kinds, sizes and SHA-256 of
//! every file. Byte identity is not required. Reports print counts, never names.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use pantheon_core::Limits;
use pantheon_crypto::hash::{SHA256_LEN, Sha256};
use pantheon_zar::{EntryKind, Reader};

/// Bytes hashed per read.
const CHUNK: usize = 64 * 1024;
/// Separates a base package from its update in one input.
const PAIR: char = '+';

/// `None` for folders, `(size, SHA-256)` for files.
type Manifest = BTreeMap<String, Option<(u64, [u8; SHA256_LEN])>>;

fn manifest(path: &Path) -> Result<Manifest, String> {
    let fail = |e: pantheon_zar::Error| format!("reading back an archive: {e}");
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut r = Reader::open(file, &Limits::DEFAULT).map_err(fail)?;
    r.verify_digest().map_err(fail)?;
    let mut out = Manifest::new();
    let mut buf = vec![0u8; CHUNK];
    for e in r.entries().map_err(fail)? {
        let value = match e.kind {
            EntryKind::Directory => None,
            EntryKind::File => {
                let mut hash = Sha256::new();
                let mut at = 0u64;
                while at < e.size {
                    let n = r.read_at(e.node, at, &mut buf).map_err(fail)?;
                    let chunk = buf.get(..n).filter(|c| !c.is_empty());
                    let Some(chunk) = chunk else {
                        return Err("archive file ended early".to_owned());
                    };
                    hash.update(chunk);
                    at = at.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
                }
                Some((e.size, hash.finalize()))
            }
        };
        out.insert(e.path, value);
    }
    Ok(out)
}

/// Mismatch counts between two manifests, as report lines.
pub fn compare(oracle: &Manifest, ours: &Manifest, label: &str) -> Vec<String> {
    let only_oracle = oracle.keys().filter(|k| !ours.contains_key(*k)).count();
    let only_ours = ours.keys().filter(|k| !oracle.contains_key(*k)).count();
    let (mut kind, mut content) = (0usize, 0usize);
    for (path, a) in oracle {
        match (a, ours.get(path)) {
            (_, None) | (None, Some(None)) => {}
            (Some((x, h)), Some(Some((y, g)))) if x == y => {
                if h != g {
                    content = content.saturating_add(1);
                }
            }
            _ => kind = kind.saturating_add(1),
        }
    }
    let mut lines = Vec::new();
    for (n, what) in [
        (only_oracle, "paths only in the pkg-tools archive"),
        (only_ours, "paths only in the proteus archive"),
        (kind, "paths with a different kind or size"),
        (content, "files with a different SHA-256"),
    ] {
        if n != 0 {
            lines.push(format!("{label}: {n} {what}"));
        }
    }
    lines
}

fn summary(m: &Manifest) -> String {
    let files = m.values().filter(|v| v.is_some()).count();
    let bytes: u64 = m.values().flatten().map(|(s, _)| *s).sum();
    format!(
        "{files} files, {} folders, {bytes} bytes",
        m.len().saturating_sub(files)
    )
}

fn convert(program: &Path, input: &str, out: &Path) -> Result<bool, String> {
    let mut command = Command::new(program);
    if Path::new(input).is_dir() {
        command.arg("pack").arg(input).arg(out);
    } else if let Some((base, update)) = input
        .rsplit_once(PAIR)
        .filter(|_| !Path::new(input).exists())
    {
        command
            .arg("convert")
            .arg(base)
            .arg(out)
            .arg("--update")
            .arg(update);
    } else {
        command.arg("convert").arg(input).arg(out);
    }
    command
        .output()
        .map(|o| o.status.success())
        .map_err(|e| format!("running {}: {e}", program.display()))
}

/// The update archive both tools write next to `game.zar`.
fn sibling(out: &Path) -> PathBuf {
    out.with_file_name("game-UPDATE.zar")
}

/// Runs one input; `Ok(lines)` with `lines[0]` starting `ok:` on equivalence.
pub fn run_input(
    oracle: &Path,
    ours: &Path,
    input: &str,
    work: &Path,
) -> Result<Vec<String>, String> {
    let io = |e: std::io::Error| format!("{}: {e}", work.display());
    if work.exists() {
        fs::remove_dir_all(work).map_err(io)?;
    }
    let (a, b) = (work.join("oracle"), work.join("ours"));
    fs::create_dir_all(&a).map_err(io)?;
    fs::create_dir_all(&b).map_err(io)?;
    let (za, zb) = (a.join("game.zar"), b.join("game.zar"));
    let ra = convert(oracle, input, &za)?;
    let rb = convert(ours, input, &zb)?;
    let lines = match (ra, rb) {
        (false, false) => vec!["ok: both refused".to_owned()],
        (true, false) => vec!["pkg-tools succeeded, proteus failed".to_owned()],
        (false, true) => vec!["pkg-tools failed, proteus succeeded".to_owned()],
        (true, true) => {
            let mut lines = Vec::new();
            let mut done = Vec::new();
            for (label, x, y) in [("base", &za, &zb), ("update", &sibling(&za), &sibling(&zb))] {
                match (x.exists(), y.exists()) {
                    (false, false) => continue,
                    (true, true) => {}
                    _ => {
                        lines.push(format!("{label}: written by only one tool"));
                        continue;
                    }
                }
                let (ma, mb) = (manifest(x)?, manifest(y)?);
                let diff = compare(&ma, &mb, label);
                if diff.is_empty() {
                    done.push(format!("{label} {}", summary(&ma)));
                }
                lines.extend(diff);
            }
            if lines.is_empty() {
                vec![format!("ok: {}", done.join("; "))]
            } else {
                lines
            }
        }
    };
    fs::remove_dir_all(work).map_err(io)?;
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_compare_by_path_kind_size_and_hash() {
        let file = |n: u8| Some((1u64, [n; SHA256_LEN]));
        let a: Manifest = [
            ("d".into(), None),
            ("d/f".into(), file(1)),
            ("g".into(), file(2)),
        ]
        .into_iter()
        .collect();
        assert!(compare(&a, &a.clone(), "base").is_empty());
        let mut b = a.clone();
        b.insert("d/f".into(), file(9));
        b.insert("g".into(), None);
        b.insert("extra".into(), None);
        b.remove("d");
        let lines = compare(&a, &b, "base");
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(
            lines.iter().all(|l| !l.contains("d/f")),
            "names are never printed"
        );
    }
}
