//! `cargo xtask deps`: audits `Cargo.lock` and the build graph against
//! `deps-allowlist.txt` (docs/02-dependencies.md §6).
//!
//! The audit has three inputs, all read as plain text:
//! - `Cargo.lock`: every package with a `source` is third-party and must be listed.
//! - `deps-allowlist.txt`: `name version owner[,owner...] reason...`.
//! - `cargo tree -e normal,build --prefix depth`: gives every dependency edge
//!   (build dependencies and proc-macros count as runtime). Each edge into a
//!   third-party crate must come from one of that crate's listed owners.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Policy failure: the lockfile or graph breaks the allowlist. D31 has no code for
/// this, so it uses the conventional generic failure code, which no D31 kind uses.
pub const EXIT_POLICY: u8 = 1;
/// Bad flags (D31 `Usage`).
pub const EXIT_USAGE: u8 = 2;
/// `Cargo.lock`, the allowlist or `cargo tree` output could not be parsed (D31 `Malformed`).
pub const EXIT_MALFORMED: u8 = 3;
/// Reading a file or running `cargo` failed (D31 `Io`).
pub const EXIT_IO: u8 = 8;

const ALLOWLIST_FILE: &str = "deps-allowlist.txt";
const LOCK_FILE: &str = "Cargo.lock";
/// Workspace members that are developer tooling: their graphs are audited for
/// ownership but are not part of the runtime graph or its crate count.
const DEV_TOOL_MEMBERS: &[&str] = &["xtask"];
/// Dev-only counting allocator (D14); must never appear in a normal/build graph.
const TESTALLOC: &str = "pantheon-testalloc";
/// Owner prefix for a crate reached only through another allowlisted crate.
const VIA_PREFIX: &str = "via:";
/// Owner prefix for a dev-only crate (dev-dependency or dev-tool dependency).
const DEV_PREFIX: &str = "dev:";
/// Allowlist version column meaning "any version" (transitive crates).
const ANY_VERSION: &str = "*";
/// Allowlist version column prefix for an exact pin.
const EXACT_PREFIX: char = '=';
/// Minimum columns on an allowlist line: name, version, owners, reason.
const MIN_COLUMNS: usize = 4;

/// Target family a `[unix]` / `[windows]` owner qualifier refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetFamily {
    Unix,
    Windows,
}

impl TargetFamily {
    /// The family `cargo tree` resolves for: it uses the host target by default.
    fn host() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// One `[[package]]` block of `Cargo.lock`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockPackage {
    pub name: String,
    pub version: String,
    /// `None` for workspace members (and other path packages).
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionReq {
    Exact(String),
    Any,
}

impl VersionReq {
    fn matches(&self, version: &str) -> bool {
        match self {
            Self::Exact(pinned) => pinned == version,
            Self::Any => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// A workspace crate that depends on it directly, optionally only on one target family.
    Member {
        name: String,
        target: Option<TargetFamily>,
    },
    /// Transitive through this allowlisted crate.
    Via(String),
    /// Dev-only: used by this workspace crate's tests or dev tooling, never at runtime.
    Dev(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub version: VersionReq,
    pub owners: Vec<Owner>,
    /// 1-based line in the allowlist file.
    pub line: usize,
}

impl Entry {
    fn is_dev_only(&self) -> bool {
        self.owners
            .iter()
            .any(|owner| matches!(owner, Owner::Dev(_)))
    }
}

/// A package in a `cargo tree` graph.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Node {
    pub name: String,
    /// Without the leading `v`.
    pub version: String,
}

impl std::fmt::Display for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} v{}", self.name, self.version)
    }
}

/// Dependency graph: every package below a root, mapped to its direct dependents.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Graph {
    pub dependents: BTreeMap<Node, BTreeSet<Node>>,
}

/// Result of the policy check.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub failures: Vec<String>,
    pub warnings: Vec<String>,
    /// Distinct third-party packages in the runtime graph.
    pub runtime_crates: usize,
}

/// Parses `Cargo.lock` without a TOML parser: only `[[package]]` blocks and their
/// `name`, `version` and `source` keys are read; everything else is ignored.
pub fn parse_lock(text: &str) -> Result<Vec<LockPackage>, String> {
    struct Block {
        line: usize,
        name: Option<String>,
        version: Option<String>,
        source: Option<String>,
    }
    fn finish(block: Block, out: &mut Vec<LockPackage>) -> Result<(), String> {
        match (block.name, block.version) {
            (Some(name), Some(version)) => {
                out.push(LockPackage {
                    name,
                    version,
                    source: block.source,
                });
                Ok(())
            }
            _ => Err(format!(
                "{LOCK_FILE}:{}: [[package]] without name or version",
                block.line
            )),
        }
    }

    let mut packages = Vec::new();
    let mut current: Option<Block> = None;
    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();
        if line.starts_with('[') {
            if let Some(block) = current.take() {
                finish(block, &mut packages)?;
            }
            if line == "[[package]]" {
                current = Some(Block {
                    line: line_no,
                    name: None,
                    version: None,
                    source: None,
                });
            }
            continue;
        }
        let Some(block) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let slot = match key.trim() {
            "name" => &mut block.name,
            "version" => &mut block.version,
            "source" => &mut block.source,
            _ => continue,
        };
        let value = value.trim();
        let unquoted = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .filter(|inner| !inner.is_empty() && !inner.contains('"'))
            .ok_or_else(|| format!("{LOCK_FILE}:{line_no}: expected a quoted string"))?;
        *slot = Some(unquoted.to_owned());
    }
    if let Some(block) = current {
        finish(block, &mut packages)?;
    }
    Ok(packages)
}

fn is_crate_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `x.y.z` with optional `-pre` / `+build` suffix.
fn is_version(version: &str) -> bool {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn parse_owner(text: &str) -> Option<Owner> {
    if let Some(name) = text.strip_prefix(VIA_PREFIX) {
        return is_crate_name(name).then(|| Owner::Via(name.to_owned()));
    }
    if let Some(name) = text.strip_prefix(DEV_PREFIX) {
        return is_crate_name(name).then(|| Owner::Dev(name.to_owned()));
    }
    let (name, target) = match text.split_once('[') {
        None => (text, None),
        Some((name, qualifier)) => {
            let target = match qualifier {
                "unix]" => TargetFamily::Unix,
                "windows]" => TargetFamily::Windows,
                _ => return None,
            };
            (name, Some(target))
        }
    };
    is_crate_name(name).then(|| Owner::Member {
        name: name.to_owned(),
        target,
    })
}

/// Parses the allowlist. Lines whose first non-blank character is `#` are comments.
pub fn parse_allowlist(text: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |what: &str| format!("{ALLOWLIST_FILE}:{line_no}: {what}: `{line}`");
        let columns: Vec<&str> = line.split_whitespace().collect();
        if columns.len() < MIN_COLUMNS {
            return Err(bad("expected `name version owner(s) reason`"));
        }
        let (name, version, owners) = (columns[0], columns[1], columns[2]);
        if !is_crate_name(name) {
            return Err(bad("invalid crate name"));
        }
        let version = if version == ANY_VERSION {
            VersionReq::Any
        } else {
            match version.strip_prefix(EXACT_PREFIX) {
                Some(exact) if is_version(exact) => VersionReq::Exact(exact.to_owned()),
                _ => return Err(bad("version must be `=x.y.z` or `*`")),
            }
        };
        let owners = owners
            .split(',')
            .map(parse_owner)
            .collect::<Option<Vec<Owner>>>()
            .ok_or_else(|| bad("invalid owner"))?;
        let dev = owners.iter().any(|owner| matches!(owner, Owner::Dev(_)));
        let member = owners
            .iter()
            .any(|owner| matches!(owner, Owner::Member { .. }));
        if dev && member {
            return Err(bad("an entry cannot have both dev: and runtime owners"));
        }
        if entries.iter().any(|other: &Entry| {
            other.name == name && (other.version == version || other.version == VersionReq::Any)
        }) {
            return Err(bad("duplicate entry"));
        }
        entries.push(Entry {
            name: name.to_owned(),
            version,
            owners,
            line: line_no,
        });
    }
    Ok(entries)
}

/// Parses the `name vX.Y.Z ...` part of a `cargo tree -f {p}` line.
fn parse_node(text: &str) -> Option<Node> {
    let mut tokens = text.split_whitespace();
    let name = tokens.next()?;
    let version = tokens.next()?.strip_prefix('v')?;
    Some(Node {
        name: name.to_owned(),
        version: version.to_owned(),
    })
}

/// Parses `cargo tree --prefix depth -f {p}` output into dependency edges.
/// A line's parent is the closest earlier line one level shallower; `(*)` lines
/// were expanded at their first occurrence, so every edge is still seen.
pub fn parse_tree(text: &str) -> Result<Graph, String> {
    let mut graph = Graph::default();
    let mut stack: Vec<Node> = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let bad = || format!("unexpected `cargo tree` line: `{line}`");
        let digits = line.chars().take_while(char::is_ascii_digit).count();
        let depth: usize = line[..digits].parse().map_err(|_| bad())?;
        let node = parse_node(&line[digits..]).ok_or_else(bad)?;
        if depth > stack.len() {
            return Err(bad());
        }
        stack.truncate(depth);
        if let Some(parent) = stack.last() {
            graph
                .dependents
                .entry(node.clone())
                .or_default()
                .insert(parent.clone());
        }
        stack.push(node);
    }
    Ok(graph)
}

/// Checks the lockfile and graphs against the allowlist.
///
/// `runtime` is the normal+build graph of the audited non-tooling members;
/// `tooling` is the normal+build graph of the dev-tool members (`xtask`).
pub fn check(
    lock: &[LockPackage],
    allowlist: &[Entry],
    runtime: &Graph,
    tooling: &Graph,
    host: TargetFamily,
) -> Report {
    let mut report = Report::default();
    let members: BTreeSet<&str> = lock
        .iter()
        .filter(|package| package.source.is_none())
        .map(|package| package.name.as_str())
        .collect();
    let find = |name: &str, version: &str| {
        allowlist
            .iter()
            .find(|entry| entry.name == name && entry.version.matches(version))
    };

    for package in lock.iter().filter(|package| package.source.is_some()) {
        if find(&package.name, &package.version).is_some() {
            continue;
        }
        let listed: Vec<String> = allowlist
            .iter()
            .filter(|entry| entry.name == package.name)
            .filter_map(|entry| match &entry.version {
                VersionReq::Exact(version) => Some(format!("={version}")),
                VersionReq::Any => None,
            })
            .collect();
        report.failures.push(if listed.is_empty() {
            format!(
                "{} v{}: in {LOCK_FILE} but not in {ALLOWLIST_FILE}",
                package.name, package.version
            )
        } else {
            format!(
                "{} v{}: version does not match {ALLOWLIST_FILE} ({})",
                package.name,
                package.version,
                listed.join(", ")
            )
        });
    }

    for entry in allowlist {
        let at = format!("{ALLOWLIST_FILE}:{} {}", entry.line, entry.name);
        for owner in &entry.owners {
            match owner {
                Owner::Member { name, .. } | Owner::Dev(name)
                    if !members.contains(name.as_str()) =>
                {
                    report
                        .failures
                        .push(format!("{at}: owner `{name}` is not a workspace crate"));
                }
                Owner::Via(name) if !allowlist.iter().any(|other| &other.name == name) => {
                    report.failures.push(format!(
                        "{at}: `{VIA_PREFIX}{name}` is not an allowlisted crate"
                    ));
                }
                _ => {}
            }
        }
        let locked = lock
            .iter()
            .any(|package| package.name == entry.name && entry.version.matches(&package.version));
        if !locked {
            // Expected for target-specific crates (e.g. windows-sys when auditing on Linux).
            report
                .warnings
                .push(format!("{at}: not in {LOCK_FILE} on this target"));
        }
    }

    for graph in [runtime, tooling] {
        for (node, dependents) in &graph.dependents {
            if node.name == TESTALLOC {
                report.failures.push(format!(
                    "{TESTALLOC}: in a normal/build graph (dependent: {}); it must stay a dev-dependency (D14)",
                    join(dependents)
                ));
                continue;
            }
            if members.contains(node.name.as_str()) {
                continue;
            }
            // A missing entry was already reported by the lockfile check.
            let Some(entry) = find(&node.name, &node.version) else {
                continue;
            };
            for dependent in dependents {
                let (allowed, needed) = if members.contains(dependent.name.as_str()) {
                    let allowed = entry.owners.iter().any(|owner| match owner {
                        Owner::Member { name, target } => {
                            *name == dependent.name && target.is_none_or(|target| target == host)
                        }
                        Owner::Dev(name) => *name == dependent.name,
                        Owner::Via(_) => false,
                    });
                    (allowed, dependent.name.clone())
                } else {
                    let via = Owner::Via(dependent.name.clone());
                    (
                        entry.owners.contains(&via),
                        format!("{VIA_PREFIX}{}", dependent.name),
                    )
                };
                if !allowed {
                    report.failures.push(format!(
                        "{node}: depended on by {dependent}, which is not an owner (needs `{needed}`)"
                    ));
                }
            }
        }
    }

    for node in runtime.dependents.keys() {
        if find(&node.name, &node.version).is_some_and(Entry::is_dev_only) {
            report.failures.push(format!(
                "{node}: dev-only crate in the runtime normal/build graph (dependent: {})",
                join(&runtime.dependents[node])
            ));
        }
    }

    report.runtime_crates = runtime
        .dependents
        .keys()
        .filter(|node| !members.contains(node.name.as_str()))
        .count();
    report
}

fn join(nodes: &BTreeSet<Node>) -> String {
    nodes
        .iter()
        .map(Node::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Which workspace members to audit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// Every workspace member (plain `deps`, and `--all` until frontends exist).
    Workspace,
    /// Default members only (L0–L3 + CLI).
    Core,
}

fn parse_args(args: &[String]) -> Result<Scope, String> {
    match args {
        [] => Ok(Scope::Workspace),
        // shortcut: `--all` equals plain `deps` while there are no frontend crates;
        // make it report frontend counts without failing once pantheon-gtk lands.
        [flag] if flag == "--all" => Ok(Scope::Workspace),
        [flag] if flag == "--core" => Ok(Scope::Core),
        _ => Err(format!(
            "usage: cargo xtask deps [--core | --all]\n  got: {}",
            args.join(" ")
        )),
    }
}

pub(crate) fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap_or(manifest_dir).to_path_buf()
}

/// Runs `cargo tree` (host target, `--locked`) and returns its stdout.
fn cargo_tree(root: &Path, extra: &[String]) -> Result<String, (u8, String)> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(root)
        .args(["tree", "--locked", "-e", "normal,build", "-f", "{p}"])
        .args(extra)
        .output()
        .map_err(|error| (EXIT_IO, format!("xtask deps: running cargo tree: {error}")))?;
    if !output.status.success() {
        return Err((
            EXIT_IO,
            format!(
                "xtask deps: cargo tree {} failed:\n{}",
                extra.join(" "),
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| {
        (
            EXIT_MALFORMED,
            "xtask deps: cargo tree output is not UTF-8".into(),
        )
    })
}

/// Graph of `members` (empty when there are none).
fn member_graph(root: &Path, members: &[String]) -> Result<Graph, (u8, String)> {
    if members.is_empty() {
        return Ok(Graph::default());
    }
    let mut args = vec!["--prefix".to_owned(), "depth".to_owned()];
    for member in members {
        args.push("-p".to_owned());
        args.push(member.clone());
    }
    parse_tree(&cargo_tree(root, &args)?).map_err(|error| (EXIT_MALFORMED, error))
}

fn read(path: &Path) -> Result<String, (u8, String)> {
    std::fs::read_to_string(path)
        .map_err(|error| (EXIT_IO, format!("xtask deps: {}: {error}", path.display())))
}

fn audit(scope: Scope) -> Result<(u8, String), (u8, String)> {
    let root = workspace_root();
    let malformed = |error: String| (EXIT_MALFORMED, format!("xtask deps: {error}"));
    let lock = parse_lock(&read(&root.join(LOCK_FILE))?).map_err(malformed)?;
    let allowlist = parse_allowlist(&read(&root.join(ALLOWLIST_FILE))?).map_err(malformed)?;

    let mut list_args = vec![
        "--depth".to_owned(),
        "0".to_owned(),
        "--prefix".to_owned(),
        "none".to_owned(),
    ];
    if scope == Scope::Workspace {
        list_args.push("--workspace".to_owned());
    }
    let listing = cargo_tree(&root, &list_args)?;
    let selected: Vec<String> = listing
        .lines()
        .filter_map(parse_node)
        .map(|node| node.name)
        .collect();
    let (tooling_members, runtime_members): (Vec<String>, Vec<String>) = selected
        .into_iter()
        // testalloc is a dev-dependency, never a root; any edge into it is a failure.
        .filter(|name| name != TESTALLOC)
        .partition(|name| DEV_TOOL_MEMBERS.contains(&name.as_str()));

    let runtime = member_graph(&root, &runtime_members)?;
    let tooling = member_graph(&root, &tooling_members)?;
    let report = check(&lock, &allowlist, &runtime, &tooling, TargetFamily::host());

    let mut text = String::new();
    for warning in &report.warnings {
        text.push_str(&format!("warning: {warning}\n"));
    }
    for failure in &report.failures {
        text.push_str(&format!("error: {failure}\n"));
    }
    text.push_str(&format!(
        "{} third-party crates in the normal+build graph of: {}\n",
        report.runtime_crates,
        runtime_members.join(", ")
    ));
    if report.failures.is_empty() {
        text.push_str("deps: ok");
        Ok((0, text))
    } else {
        text.push_str(&format!("deps: {} failure(s)", report.failures.len()));
        Ok((EXIT_POLICY, text))
    }
}

/// Entry point for `cargo xtask deps [--core | --all]`; returns exit code and message.
pub fn run(args: &[String]) -> (u8, String) {
    let scope = match parse_args(args) {
        Ok(scope) => scope,
        Err(message) => return (EXIT_USAGE, message),
    };
    audit(scope).unwrap_or_else(|failure| failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"
# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "aes"
version = "0.9.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"
dependencies = [
 "cipher",
]

[[package]]
name = "cipher"
version = "0.5.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "pantheon-crypto"
version = "0.0.0"
dependencies = [
 "aes",
]

[metadata]
foo = "bar"
"#;

    fn node(name: &str, version: &str) -> Node {
        Node {
            name: name.to_owned(),
            version: version.to_owned(),
        }
    }

    /// `(name, version)` of a graph node.
    type Pkg<'a> = (&'a str, &'a str);

    fn graph(edges: &[(Pkg, Pkg)]) -> Graph {
        let mut graph = Graph::default();
        for ((child, child_version), (parent, parent_version)) in edges {
            graph
                .dependents
                .entry(node(child, child_version))
                .or_default()
                .insert(node(parent, parent_version));
        }
        graph
    }

    fn lock_of(packages: &[(&str, &str, bool)]) -> Vec<LockPackage> {
        packages
            .iter()
            .map(|(name, version, third_party)| LockPackage {
                name: (*name).to_owned(),
                version: (*version).to_owned(),
                source: third_party.then(|| "registry+x".to_owned()),
            })
            .collect()
    }

    #[test]
    fn parses_lock() {
        let packages = parse_lock(LOCK).expect("valid lock");
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].name, "aes");
        assert_eq!(packages[0].version, "0.9.3");
        assert!(packages[0].source.is_some());
        assert_eq!(packages[2].name, "pantheon-crypto");
        assert_eq!(packages[2].source, None);
    }

    #[test]
    fn rejects_bad_lock() {
        assert!(parse_lock("[[package]]\nname = \"a\"\n").is_err());
        assert!(parse_lock("[[package]]\nname = a\nversion = \"1.0.0\"\n").is_err());
        assert_eq!(parse_lock("version = 4\n"), Ok(Vec::new()));
    }

    #[test]
    fn parses_allowlist() {
        let text = "\
# comment
  # indented comment

aes      =0.9.3  pantheon-crypto              AES block cipher (D8)
rustix   =1.1.5  pantheon-fs[unix]            no-follow openat (P5)
win      =0.59.0 pantheon-fs[windows],via:ctrlc  rename
cipher   *       via:aes,via:cbc              transitive
roxmltree =0.20.0 dev:pantheon-xml            differential oracle
";
        let entries = parse_allowlist(text).expect("valid allowlist");
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].version, VersionReq::Exact("0.9.3".into()));
        assert_eq!(entries[0].line, 4);
        assert_eq!(
            entries[1].owners,
            vec![Owner::Member {
                name: "pantheon-fs".into(),
                target: Some(TargetFamily::Unix)
            }]
        );
        assert_eq!(entries[2].owners[1], Owner::Via("ctrlc".into()));
        assert_eq!(entries[3].version, VersionReq::Any);
        assert_eq!(entries[4].owners, vec![Owner::Dev("pantheon-xml".into())]);
        assert!(entries[4].is_dev_only());
    }

    #[test]
    fn rejects_bad_allowlist_lines() {
        for line in [
            "aes =0.9.3 pantheon-crypto",                 // no reason
            "aes 0.9.3 pantheon-crypto reason",           // no `=`
            "aes =0.9 pantheon-crypto reason",            // not x.y.z
            "aes ^0.9.3 pantheon-crypto reason",          // range
            "a/b =0.9.3 pantheon-crypto reason",          // bad name
            "aes =0.9.3 pantheon-fs[mac] reason",         // bad qualifier
            "aes =0.9.3 via: reason",                     // empty via
            "aes =0.9.3 pantheon-crypto,, reason",        // empty owner
            "aes =0.9.3 dev:pantheon-xml,pantheon-xml r", // dev + runtime
        ] {
            assert!(parse_allowlist(line).is_err(), "accepted `{line}`");
        }
        assert!(parse_allowlist("aes =1.0.0 a r\naes =1.0.0 a r").is_err());
        assert!(parse_allowlist("aes =1.0.0 a r\naes =2.0.0 a r").is_ok());
        assert!(parse_allowlist("ok =1.0.0-rc.1+b2 a r").is_ok());
    }

    #[test]
    fn parses_tree() {
        let text = "\
0pantheon-crypto v0.0.0 (/w/crates/crypto)
1aes v0.9.3
2cipher v0.5.0
1cbc v0.2.0
2cipher v0.5.0 (*)
1pantheon-core v0.0.0 (/w/crates/core)

0pantheon-fs v0.0.0 (/w/crates/fs)
1serde_derive v1.0.0 (proc-macro)
";
        let graph = parse_tree(text).expect("valid tree");
        assert_eq!(
            graph.dependents[&node("cipher", "0.5.0")],
            [node("aes", "0.9.3"), node("cbc", "0.2.0")].into()
        );
        assert_eq!(
            graph.dependents[&node("serde_derive", "1.0.0")],
            [node("pantheon-fs", "0.0.0")].into()
        );
        assert!(
            !graph
                .dependents
                .contains_key(&node("pantheon-crypto", "0.0.0"))
        );
        assert!(parse_tree("0a v1.0.0\n2b v1.0.0\n").is_err());
        assert!(parse_tree("[build-dependencies]\n").is_err());
        assert!(parse_tree("0a 1.0.0\n").is_err());
    }

    fn allowlist() -> Vec<Entry> {
        parse_allowlist(
            "\
aes       =0.9.3  pantheon-crypto     r
cipher    *       via:aes             r
rustix    =1.1.5  pantheon-fs[unix]   r
winsys    =0.59.0 pantheon-fs[windows] r
roxmltree =0.20.0 dev:pantheon-xml    r
json      =1.0.0  dev:xtask           r
",
        )
        .expect("valid allowlist")
    }

    fn lock() -> Vec<LockPackage> {
        lock_of(&[
            ("aes", "0.9.3", true),
            ("cipher", "0.5.0", true),
            ("rustix", "1.1.5", true),
            ("roxmltree", "0.20.0", true),
            ("json", "1.0.0", true),
            ("pantheon-crypto", "0.0.0", false),
            ("pantheon-fs", "0.0.0", false),
            ("pantheon-xml", "0.0.0", false),
            ("pantheon-testalloc", "0.0.0", false),
            ("xtask", "0.0.0", false),
        ])
    }

    fn good_runtime() -> Graph {
        graph(&[
            (("aes", "0.9.3"), ("pantheon-crypto", "0.0.0")),
            (("cipher", "0.5.0"), ("aes", "0.9.3")),
            (("rustix", "1.1.5"), ("pantheon-fs", "0.0.0")),
        ])
    }

    fn good_tooling() -> Graph {
        graph(&[(("json", "1.0.0"), ("xtask", "0.0.0"))])
    }

    fn failures(lock: &[LockPackage], runtime: &Graph, tooling: &Graph) -> Vec<String> {
        check(lock, &allowlist(), runtime, tooling, TargetFamily::Unix).failures
    }

    #[test]
    fn clean_audit_passes_and_counts() {
        let report = check(
            &lock(),
            &allowlist(),
            &good_runtime(),
            &good_tooling(),
            TargetFamily::Unix,
        );
        assert_eq!(report.failures, Vec::<String>::new());
        assert_eq!(report.runtime_crates, 3);
        // winsys is Windows-only: a warning, not a failure.
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("winsys"));
    }

    #[test]
    fn flags_unlisted_and_mismatched_versions() {
        let mut lock = lock();
        lock.extend(lock_of(&[("zstd", "1.0.0", true), ("aes", "0.8.0", true)]));
        let found = failures(&lock, &good_runtime(), &good_tooling());
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("zstd v1.0.0: in Cargo.lock but not in"));
        assert!(
            found[1].contains("aes v0.8.0: version does not match deps-allowlist.txt (=0.9.3)")
        );
    }

    #[test]
    fn flags_wrong_owner_and_direct_use_of_transitive() {
        let mut runtime = good_runtime();
        for (child, parent) in [
            (node("aes", "0.9.3"), node("pantheon-fs", "0.0.0")),
            (node("cipher", "0.5.0"), node("pantheon-crypto", "0.0.0")),
            (node("cipher", "0.5.0"), node("rustix", "1.1.5")),
        ] {
            runtime.dependents.entry(child).or_default().insert(parent);
        }
        let found = failures(&lock(), &runtime, &good_tooling());
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].contains("aes v0.9.3: depended on by pantheon-fs v0.0.0"));
        assert!(found[1].contains("cipher v0.5.0: depended on by pantheon-crypto v0.0.0"));
        assert!(found[2].contains("`via:rustix`"));
    }

    #[test]
    fn target_qualifier_follows_host() {
        let found = check(
            &lock(),
            &allowlist(),
            &good_runtime(),
            &good_tooling(),
            TargetFamily::Windows,
        )
        .failures;
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("rustix v1.1.5: depended on by pantheon-fs"));
    }

    #[test]
    fn flags_dev_only_and_testalloc_in_normal_graph() {
        let mut runtime = good_runtime();
        runtime
            .dependents
            .entry(node("roxmltree", "0.20.0"))
            .or_default()
            .insert(node("pantheon-xml", "0.0.0"));
        runtime
            .dependents
            .entry(node("pantheon-testalloc", "0.0.0"))
            .or_default()
            .insert(node("pantheon-crypto", "0.0.0"));
        let mut tooling = good_tooling();
        tooling
            .dependents
            .entry(node("pantheon-testalloc", "0.0.0"))
            .or_default()
            .insert(node("xtask", "0.0.0"));
        let found = failures(&lock(), &runtime, &tooling);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(
            found
                .iter()
                .filter(|f| f.starts_with("pantheon-testalloc"))
                .count()
                == 2
        );
        assert!(
            found
                .iter()
                .any(|f| f.contains("roxmltree v0.20.0: dev-only crate"))
        );
    }

    #[test]
    fn flags_unknown_owners() {
        let entries = parse_allowlist("aes =0.9.3 pantheon-cryptoo,via:nope r").expect("syntax ok");
        let lock = lock_of(&[("aes", "0.9.3", true), ("pantheon-crypto", "0.0.0", false)]);
        let report = check(
            &lock,
            &entries,
            &Graph::default(),
            &Graph::default(),
            TargetFamily::Unix,
        );
        assert_eq!(report.failures.len(), 2, "{:?}", report.failures);
    }

    #[test]
    fn parses_flags() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_args(&args(&[])), Ok(Scope::Workspace));
        assert_eq!(parse_args(&args(&["--all"])), Ok(Scope::Workspace));
        assert_eq!(parse_args(&args(&["--core"])), Ok(Scope::Core));
        assert!(parse_args(&args(&["--core", "--all"])).is_err());
        assert_eq!(run(&args(&["--bogus"])).0, EXIT_USAGE);
    }
}
