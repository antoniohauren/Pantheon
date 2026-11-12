//! `cargo xtask diff athena INPUT…`: differential inspect against pkg-tools
//! (docs/05-testing.md §3). The oracle binary comes from `PANTHEON_PKG_TOOLS`.
//!
//! Field mapping (docs/gods/athena.md): identity. Every field pkg-tools emits
//! must appear in Pantheon's output with an equal value; Pantheon may only add
//! the fields in [`PANTHEON_ONLY`]. Reports print input numbers and counts,
//! never names or hashes of private content.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::EXIT_USAGE;
use crate::deps::workspace_root;

/// Differential mismatch.
const EXIT_DIFF: u8 = 1;
const EXIT_IO: u8 = 8;
const ORACLE_ENV: &str = "PANTHEON_PKG_TOOLS";

/// Pantheon-only fields, as JSON-pointer-like paths with `*` for any array index.
const PANTHEON_ONLY: &[&str] = &[
    "/schema",
    "/source",
    "/summary",
    "/header/mount_image_offset",
    "/header/mount_image_size",
];

/// Compares `oracle` against `ours`; returns one line per mismatch.
pub fn compare(oracle: &Value, ours: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(oracle, ours, "", &mut out);
    out
}

fn generic(path: &str) -> String {
    // Array indices become `*` so PANTHEON_ONLY can name fields inside lists.
    path.split('/')
        .map(|part| {
            if part.parse::<usize>().is_ok() {
                "*"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn walk(oracle: &Value, ours: &Value, path: &str, out: &mut Vec<String>) {
    match (oracle, ours) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                let at = format!("{path}/{key}");
                match b.get(key) {
                    Some(mine) => walk(value, mine, &at, out),
                    None => out.push(format!("{at}: missing")),
                }
            }
            for key in b.keys().filter(|k| !a.contains_key(*k)) {
                let at = format!("{path}/{key}");
                if !PANTHEON_ONLY.contains(&generic(&at).as_str()) {
                    out.push(format!(
                        "{at}: not in pkg-tools output and not in the documented mapping"
                    ));
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                walk(x, y, &format!("{path}/{i}"), out);
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            out.push(format!("{path}: {} items vs {}", a.len(), b.len()));
        }
        (a, b) if a == b => {}
        // Values are deliberately not printed: they may be private content.
        _ => out.push(format!("{path}: value differs")),
    }
}

fn run_json(program: &Path, args: &[&str], input: &Path) -> Result<Option<Value>, String> {
    let output = Command::new(program)
        .args(args)
        .arg(input)
        .arg("--json")
        .output()
        .map_err(|e| format!("running {}: {e}", program.display()))?;
    if !output.status.success() {
        return Ok(None);
    }
    serde_json::from_slice(&output.stdout)
        .map(Some)
        .map_err(|e| format!("{} printed invalid JSON: {e}", program.display()))
}

fn build_athena(root: &Path) -> Result<PathBuf, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(root)
        .args([
            "build",
            "--quiet",
            "--release",
            "-p",
            "pantheon-cli",
            "--bin",
            "athena",
        ])
        .status()
        .map_err(|e| format!("running cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build of athena failed".into());
    }
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    Ok(target
        .join("release")
        .join(format!("athena{}", std::env::consts::EXE_SUFFIX)))
}

/// Entry point for `cargo xtask diff <god> INPUT…`.
pub fn run(args: &[String]) -> (u8, String) {
    let usage = || {
        (
            EXIT_USAGE,
            format!(
                "usage: {ORACLE_ENV}=/path/to/pkg-tools cargo xtask diff athena INPUT...\n  (only `athena` is implemented)"
            ),
        )
    };
    let Some((god, inputs)) = args.split_first() else {
        return usage();
    };
    if god != "athena" || inputs.is_empty() {
        return usage();
    }
    let Some(oracle) = std::env::var_os(ORACLE_ENV).map(PathBuf::from) else {
        return (
            EXIT_USAGE,
            format!(
                "xtask diff: set {ORACLE_ENV} to the pkg-tools binary (frozen at 20e3c3a, D19)"
            ),
        );
    };
    let ours = match build_athena(&workspace_root()) {
        Ok(path) => path,
        Err(e) => return (EXIT_IO, format!("xtask diff: {e}")),
    };
    let mut report = String::new();
    let mut failed = 0usize;
    for (n, input) in inputs.iter().enumerate() {
        let number = n.saturating_add(1);
        let input = Path::new(input);
        let result = run_json(&oracle, &["inspect"], input)
            .and_then(|a| run_json(&ours, &["inspect"], input).map(|b| (a, b)));
        let lines = match result {
            Err(e) => return (EXIT_IO, format!("xtask diff: input {number}: {e}")),
            Ok((None, None)) => Vec::new(),
            Ok((Some(_), None)) => vec!["pkg-tools succeeded, athena failed".to_owned()],
            Ok((None, Some(_))) => vec!["pkg-tools failed, athena succeeded".to_owned()],
            Ok((Some(a), Some(b))) => compare(&a, &b),
        };
        if lines.is_empty() {
            report.push_str(&format!("input {number}: pass\n"));
        } else {
            failed = failed.saturating_add(1);
            report.push_str(&format!(
                "input {number}: FAIL ({} mismatches)\n",
                lines.len()
            ));
            for line in lines.iter().take(20) {
                report.push_str(&format!("  {line}\n"));
            }
        }
    }
    let total = inputs.len();
    report.push_str(&format!(
        "diff athena: {} of {total} inputs pass",
        total.saturating_sub(failed)
    ));
    (if failed == 0 { 0 } else { EXIT_DIFF }, report)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn subset_with_documented_additions_passes() {
        let oracle = json!({"format": "ps4-pkg", "header": {"flags": 1}, "entries": [{"id": 1}]});
        let ours = json!({"schema": "x", "format": "ps4-pkg", "summary": {},
            "header": {"flags": 1, "mount_image_size": 0}, "entries": [{"id": 1}]});
        assert!(compare(&oracle, &ours).is_empty());
    }

    #[test]
    fn missing_changed_and_undocumented_fields_fail() {
        let oracle = json!({"a": 1, "b": [1, 2], "c": "secret"});
        let ours = json!({"a": 2, "b": [1], "extra": true});
        let lines = compare(&oracle, &ours);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(
            lines.iter().all(|l| !l.contains("secret")),
            "values are never printed: {lines:?}"
        );
    }
}
