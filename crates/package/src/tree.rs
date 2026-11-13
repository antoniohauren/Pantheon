//! The flat, index-addressed application tree and selection resolution
//! (pkg-tools `docs/explore-selection.md`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Error;

/// Source of listing generations; every opened tree gets a fresh one.
static GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    File,
    Directory,
}

impl NodeKind {
    /// Stable machine name, as used in JSON output.
    pub const fn name(self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Directory => "directory",
        }
    }
}

/// One entry of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Portable `/`-separated path relative to the application root.
    pub path: String,
    pub kind: NodeKind,
    /// Decoded bytes; zero for directories.
    pub size: u64,
    /// Index of the parent directory; `None` at the root.
    pub parent: Option<u32>,
}

impl Node {
    /// The last path component.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// The listing of one opened package. Parents precede their children.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    generation: u64,
    nodes: Vec<Node>,
}

/// What to extract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    All,
    /// Node indices of the listing stamped `generation`.
    Paths {
        generation: u64,
        indices: Vec<u32>,
    },
}

/// A selection resolved against its tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// Ascending node indices: selected entries, their descendants and their
    /// parents, each once.
    pub indices: Vec<u32>,
    pub files: u64,
    pub directories: u64,
    /// Decoded bytes of the included files.
    pub bytes: u64,
}

fn index(i: usize) -> Result<u32, Error> {
    u32::try_from(i).map_err(|_| Error::UnknownIndex(u32::MAX))
}

impl Tree {
    /// Builds a tree from `(path, kind, size)` sorted so that every parent
    /// directory is present and precedes its children.
    pub(crate) fn new(entries: Vec<(String, NodeKind, u64)>) -> Result<Tree, Error> {
        let mut at: HashMap<String, u32> = HashMap::with_capacity(entries.len());
        let mut nodes = Vec::with_capacity(entries.len());
        for (i, (path, kind, size)) in entries.into_iter().enumerate() {
            let parent = match path.rsplit_once('/') {
                None => None,
                Some((p, _)) => Some(
                    at.get(p)
                        .copied()
                        .ok_or_else(|| Error::UnknownPath(p.to_owned()))?,
                ),
            };
            at.insert(path.clone(), index(i)?);
            nodes.push(Node {
                path,
                kind,
                size,
                parent,
            });
        }
        Ok(Tree {
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
            nodes,
        })
    }

    /// The stamp a [`Selection::Paths`] must carry.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn node(&self, index: u32) -> Option<&Node> {
        self.nodes.get(usize::try_from(index).ok()?)
    }

    /// The index of the node at exactly `path` (same spelling and case).
    pub fn find(&self, path: &str) -> Option<u32> {
        self.nodes
            .iter()
            .position(|n| n.path == path)
            .and_then(|i| u32::try_from(i).ok())
    }

    /// A selection of exact listing paths. Unknown paths fail.
    pub fn select<S: AsRef<str>>(&self, paths: &[S]) -> Result<Selection, Error> {
        if paths.is_empty() {
            return Err(Error::EmptySelection);
        }
        let mut by_path: HashMap<&str, u32> = HashMap::with_capacity(self.nodes.len());
        for (i, n) in self.nodes.iter().enumerate() {
            by_path.insert(&n.path, index(i)?);
        }
        let indices = paths
            .iter()
            .map(|p| {
                let p = p.as_ref();
                by_path
                    .get(p)
                    .copied()
                    .ok_or_else(|| Error::UnknownPath(p.to_owned()))
            })
            .collect::<Result<_, _>>()?;
        Ok(Selection::Paths {
            generation: self.generation,
            indices,
        })
    }

    /// Deduplicates, adds parents, expands subtrees and totals the bytes.
    /// Empty, unknown and stale selections fail, so nothing is written for them.
    pub fn resolve(&self, selection: &Selection) -> Result<Resolved, Error> {
        let len = self.nodes.len();
        let mut included = vec![false; len];
        match selection {
            Selection::All => included.fill(true),
            Selection::Paths {
                generation,
                indices,
            } => {
                if *generation != self.generation {
                    return Err(Error::StaleSelection);
                }
                if indices.is_empty() {
                    return Err(Error::EmptySelection);
                }
                let mut selected = vec![false; len];
                let mut parents = HashSet::new();
                for &i in indices {
                    let at = usize::try_from(i)
                        .ok()
                        .filter(|at| *at < len)
                        .ok_or(Error::UnknownIndex(i))?;
                    if let Some(s) = selected.get_mut(at) {
                        *s = true;
                    }
                    let mut up = self.nodes.get(at).and_then(|n| n.parent);
                    while let Some(p) = up.filter(|p| parents.insert(*p)) {
                        up = self.node(p).and_then(|n| n.parent);
                    }
                }
                // Parents precede children, so one pass marks whole subtrees.
                for i in 0..len {
                    let inherited = self
                        .nodes
                        .get(i)
                        .and_then(|n| n.parent)
                        .and_then(|p| included.get(usize::try_from(p).ok()?).copied())
                        .unwrap_or(false);
                    let mark = selected.get(i).copied().unwrap_or(false) || inherited;
                    if let Some(slot) = included.get_mut(i) {
                        *slot = mark;
                    }
                }
                for p in parents {
                    if let Some(slot) = usize::try_from(p).ok().and_then(|p| included.get_mut(p)) {
                        *slot = true;
                    }
                }
            }
        }
        let mut out = Resolved::default();
        for (i, (node, keep)) in self.nodes.iter().zip(&included).enumerate() {
            if !keep {
                continue;
            }
            out.indices.push(index(i)?);
            match node.kind {
                NodeKind::File => {
                    out.files = out.files.saturating_add(1);
                    out.bytes = out.bytes.saturating_add(node.size);
                }
                NodeKind::Directory => out.directories = out.directories.saturating_add(1),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests {
    use super::*;

    /// The shape of pkg-tools `docs/explore-selection.md`.
    fn sample() -> Tree {
        let d = |p: &str| (p.to_owned(), NodeKind::Directory, 0);
        let f = |p: &str, n| (p.to_owned(), NodeKind::File, n);
        Tree::new(vec![
            f("a.bin", 1),
            d("data"),
            f("data/config.bin", 10),
            d("data/empty"),
            d("data/textures"),
            f("data/textures/x.png", 100),
            f("data/textures/y.png", 1000),
            d("other"),
            f("other/z", 5),
        ])
        .unwrap()
    }

    fn paths(t: &Tree, r: &Resolved) -> Vec<String> {
        r.indices
            .iter()
            .map(|i| t.node(*i).unwrap().path.clone())
            .collect()
    }

    #[test]
    fn selection_adds_parents_expands_subtrees_and_dedupes() {
        let t = sample();
        let sel = t
            .select(&["data/textures", "data/textures/x.png", "data/empty"])
            .unwrap();
        let r = t.resolve(&sel).unwrap();
        assert_eq!(
            paths(&t, &r),
            [
                "data",
                "data/empty",
                "data/textures",
                "data/textures/x.png",
                "data/textures/y.png"
            ]
        );
        assert_eq!((r.files, r.directories, r.bytes), (2, 3, 1100));
    }

    #[test]
    fn all_selects_everything() {
        let t = sample();
        let r = t.resolve(&Selection::All).unwrap();
        assert_eq!(r.indices.len(), t.nodes().len());
        assert_eq!(r.bytes, 1116);
    }

    #[test]
    fn empty_unknown_and_stale_selections_fail() {
        let t = sample();
        assert!(matches!(t.select::<&str>(&[]), Err(Error::EmptySelection)));
        assert!(matches!(t.select(&["DATA"]), Err(Error::UnknownPath(_))));
        assert!(matches!(t.select(&["data/"]), Err(Error::UnknownPath(_))));
        let stale = sample().select(&["data"]).unwrap();
        assert!(matches!(t.resolve(&stale), Err(Error::StaleSelection)));
        let bad = Selection::Paths {
            generation: t.generation(),
            indices: vec![99],
        };
        assert!(matches!(t.resolve(&bad), Err(Error::UnknownIndex(99))));
        let empty = Selection::Paths {
            generation: t.generation(),
            indices: vec![],
        };
        assert!(matches!(t.resolve(&empty), Err(Error::EmptySelection)));
    }

    #[test]
    fn missing_parent_is_rejected() {
        let r = Tree::new(vec![("a/b".into(), NodeKind::File, 0)]);
        assert!(matches!(r, Err(Error::UnknownPath(_))));
    }
}
