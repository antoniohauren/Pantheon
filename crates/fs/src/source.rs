//! Reading source folders (foundation §6) and source stability (§4).

use std::collections::HashSet;
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::name::{MARKER, PKG_TOOLS_MARKER, fold};
use crate::sys::{self, Dir, Kind};
use crate::{Error, Reason, validate_component, validate_path};

/// `(len, mtime, file id)` of a source file, recorded at the start of a long
/// operation and compared before publication.
///
/// This detects ordinary changes; it is **not** a guarantee against hostile
/// concurrent mutation. Content re-verification is the format layer's job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSnapshot {
    len: u64,
    modified: Option<SystemTime>,
    id: (u64, u64),
}

impl SourceSnapshot {
    /// From an open file's metadata (inode and device on Unix).
    pub fn of(meta: &Metadata) -> SourceSnapshot {
        SourceSnapshot {
            len: meta.len(),
            modified: meta.modified().ok(),
            id: sys::file_id(meta),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceEntry {
    /// `/`-separated, relative to the root.
    pub path: String,
    pub kind: EntryKind,
}

/// A validated source folder: no symlinks, special files, incomplete markers,
/// unsafe names or case collisions anywhere below the root.
pub struct SourceTree {
    root: Dir,
    path: PathBuf,
    entries: Vec<SourceEntry>,
}

impl SourceTree {
    /// Walks `root` with no-follow opens. The root's own parent chain must be
    /// symlink-free too.
    pub fn open(root: &Path) -> Result<SourceTree, Error> {
        let dir = Dir::open_path(root).map_err(|e| as_source(Error::at(root, e)))?;
        let mut tree = SourceTree {
            root: dir,
            path: root.to_path_buf(),
            entries: Vec::new(),
        };
        let mut entries = Vec::new();
        tree.walk(&tree.root, "", &mut entries)?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        tree.entries = entries;
        Ok(tree)
    }

    /// Every directory and file, byte-wise sorted by path.
    pub fn entries(&self) -> &[SourceEntry] {
        &self.entries
    }

    /// Opens the regular file `rel` for reading, without following symlinks.
    pub fn open_file(&self, rel: &str) -> Result<File, Error> {
        let full = self.path.join(rel);
        validate_path(rel).map_err(|reason| Error::Source {
            path: full.clone(),
            reason,
        })?;
        let (parents, leaf) = rel.rsplit_once('/').unwrap_or(("", rel));
        let mut dir: Option<Dir> = None;
        if !parents.is_empty() {
            for part in parents.split('/') {
                let next = dir.as_ref().unwrap_or(&self.root).open_dir(part);
                dir = Some(next.map_err(|e| as_source(Error::at(&full, e)))?);
            }
        }
        dir.as_ref()
            .unwrap_or(&self.root)
            .open_file(leaf)
            .map_err(|e| as_source(Error::at(&full, e)))
    }

    pub fn snapshot(&self, rel: &str) -> Result<SourceSnapshot, Error> {
        let file = self.open_file(rel)?;
        let meta = file
            .metadata()
            .map_err(|e| Error::at(&self.path.join(rel), e))?;
        Ok(SourceSnapshot::of(&meta))
    }

    /// Fails with `SourceChanged` unless `rel` still matches `snapshot`.
    pub fn recheck(&self, rel: &str, snapshot: &SourceSnapshot) -> Result<(), Error> {
        if self.snapshot(rel)? == *snapshot {
            Ok(())
        } else {
            Err(Error::SourceChanged {
                path: self.path.join(rel),
            })
        }
    }

    fn walk(&self, dir: &Dir, prefix: &str, out: &mut Vec<SourceEntry>) -> Result<(), Error> {
        let here = self.path.join(prefix);
        let refuse = |path: PathBuf, reason| Err(Error::Source { path, reason });
        let mut seen = HashSet::new();
        for os_name in dir.list().map_err(|e| as_source(Error::at(&here, e)))? {
            let Some(name) = os_name.to_str() else {
                return refuse(here.join(&os_name), Reason::NotUtf8);
            };
            let folded = fold(name);
            if folded == MARKER || folded == PKG_TOOLS_MARKER {
                return refuse(here, Reason::MarkedIncomplete);
            }
            let rel = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let full = self.path.join(&rel);
            if let Err(reason) = validate_component(name).and_then(|()| validate_path(&rel)) {
                return refuse(full, reason);
            }
            if !seen.insert(folded) {
                return refuse(full, Reason::CaseCollision);
            }
            match dir.kind(name).map_err(|e| as_source(Error::at(&full, e)))? {
                Some(Kind::Dir) => {
                    let child = dir
                        .open_dir(name)
                        .map_err(|e| as_source(Error::at(&full, e)))?;
                    self.walk(&child, &rel, out)?;
                    out.push(SourceEntry {
                        path: rel,
                        kind: EntryKind::Dir,
                    });
                }
                Some(Kind::File) => out.push(SourceEntry {
                    path: rel,
                    kind: EntryKind::File,
                }),
                Some(Kind::Symlink) => return refuse(full, Reason::Symlink),
                Some(Kind::Other) => return refuse(full, Reason::SpecialFile),
                None => return Err(Error::SourceChanged { path: full }),
            }
        }
        Ok(())
    }
}

/// Refusals while reading are about input, not output.
fn as_source(e: Error) -> Error {
    match e {
        Error::Unsafe { path, reason } => Error::Source { path, reason },
        e => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    fn reason(root: &Path) -> Reason {
        match SourceTree::open(root).err().unwrap() {
            Error::Source { reason, .. } => reason,
            other => panic!("unexpected {other}"),
        }
    }

    #[test]
    fn lists_sorted_entries_and_rechecks_snapshots() {
        let dir = TestDir::new();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a.txt"), b"1").unwrap();
        std::fs::write(root.join("a/b/c"), b"22").unwrap();
        std::fs::write(root.join("Z"), b"").unwrap();
        let tree = SourceTree::open(root).unwrap();
        let paths: Vec<&str> = tree.entries().iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["Z", "a", "a.txt", "a/b", "a/b/c"]);
        assert_eq!(tree.entries()[1].kind, EntryKind::Dir);

        let snap = tree.snapshot("a/b/c").unwrap();
        tree.recheck("a/b/c", &snap).unwrap();
        std::fs::write(root.join("a/b/c"), b"333").unwrap();
        let err = tree.recheck("a/b/c", &snap).unwrap_err();
        assert!(matches!(err, Error::SourceChanged { .. }));
        // Replaced by a different file of the same length.
        let snap = tree.snapshot("a.txt").unwrap();
        std::fs::remove_file(root.join("a.txt")).unwrap();
        std::fs::write(root.join("a.txt.new"), b"9").unwrap();
        std::fs::rename(root.join("a.txt.new"), root.join("a.txt")).unwrap();
        assert!(tree.recheck("a.txt", &snap).is_err());
    }

    #[test]
    fn rejects_incomplete_markers_of_both_tools() {
        for marker in [MARKER, PKG_TOOLS_MARKER, ".PKG-TOOLS-INCOMPLETE"] {
            let dir = TestDir::new();
            std::fs::create_dir(dir.path().join("sub")).unwrap();
            std::fs::write(dir.path().join("sub").join(marker), b"").unwrap();
            assert_eq!(reason(dir.path()), Reason::MarkedIncomplete, "{marker}");
        }
    }

    #[test]
    fn rejects_unsafe_and_reserved_names() {
        for (name, want) in [
            ("aux.txt", Reason::ReservedDevice),
            ("a:b", Reason::ForbiddenChar),
            ("x.", Reason::TrailingDotOrSpace),
            (".f.pantheon-ab.partial", Reason::ReservedName),
        ] {
            let dir = TestDir::new();
            std::fs::write(dir.path().join(name), b"").unwrap();
            assert_eq!(reason(dir.path()), want, "{name}");
        }
    }

    #[test]
    fn rejects_case_collisions() {
        let dir = TestDir::new();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        std::fs::write(dir.path().join("d/File"), b"").unwrap();
        if dir.path().join("d/fILE").exists() {
            // Case-insensitive filesystem (macOS, Windows): a collision cannot be created.
            return;
        }
        std::fs::write(dir.path().join("d/fILE"), b"").unwrap();
        assert_eq!(reason(dir.path()), Reason::CaseCollision);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_and_special_files() {
        use pantheon_core::{Classify, ErrorKind};

        let dir = TestDir::new();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("link")).unwrap();
        assert_eq!(reason(&root), Reason::Symlink);
        std::fs::remove_file(root.join("link")).unwrap();

        let socket = std::os::unix::net::UnixListener::bind(root.join("sock")).unwrap();
        assert_eq!(reason(&root), Reason::SpecialFile);
        drop(socket);
        std::fs::remove_file(root.join("sock")).unwrap();

        // A symlinked root and a symlink swapped in after the walk are refused too.
        std::os::unix::fs::symlink(&root, dir.path().join("alias")).unwrap();
        assert_eq!(reason(&dir.path().join("alias")), Reason::Symlink);
        std::fs::write(root.join("f"), b"").unwrap();
        let tree = SourceTree::open(&root).unwrap();
        std::fs::remove_file(root.join("f")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("f")).unwrap();
        let err = tree.open_file("f").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported, "{err}");
    }
}
