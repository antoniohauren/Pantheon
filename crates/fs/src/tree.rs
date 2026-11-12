//! Output trees (foundation §2, P15).

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use pantheon_core::{CHUNK, Ctx, Phase, Progress};

use crate::name::{MARKER, PKG_TOOLS_MARKER, fold, marker_text, scratch_name};
use crate::sys::{self, Dir, Kind, OWNER_ONLY_SUPPORTED};
use crate::{Access, Error, Reason, validate_component, validate_path};

/// A new directory tree that carries the incomplete marker until [`OutputTree::finish`].
///
/// Every error after `create_new` returned is `Incomplete { path }`: the tree and
/// its marker stay for the user to inspect. Nothing is ever deleted on error.
pub struct OutputTree {
    root: Dir,
    path: PathBuf,
    /// ASCII-case-folded relative paths of everything created, for collisions.
    taken: HashSet<String>,
}

impl OutputTree {
    /// Creates `parent/name`, which must not exist; `parent` must exist and its
    /// whole chain must be free of symlinks.
    ///
    /// The tree is built as a marked scratch directory and renamed into place
    /// without replacing, so a crash never leaves an unmarked tree under `name`.
    /// Where that rename is unsupported, `name` is created directly and the
    /// marker is written immediately.
    pub fn create_new(parent: &Path, name: &str) -> Result<OutputTree, Error> {
        let path = parent.join(name);
        validate_component(name).map_err(|reason| Error::unsafe_at(&path, reason))?;
        let parent_dir = Dir::open_path(parent).map_err(|e| Error::at(parent, e))?;
        refuse_existing(&parent_dir, parent, name)?;

        let scratch = scratch_name(name)?;
        let scratch_path = parent.join(&scratch);
        parent_dir
            .mkdir(&scratch)
            .map_err(|e| Error::at(&scratch_path, e))?;
        // The scratch directory exists from here on; a failure leaves it, marked.
        let scratch_dir = parent_dir
            .open_dir(&scratch)
            .and_then(|dir| write_marker(&dir).map(|()| dir))
            .map_err(|e| Error::at(&scratch_path, e).left_at(&scratch_path))?;

        let root = match sys::rename_noreplace(&parent_dir, &scratch, name) {
            Ok(()) => scratch_dir
                .renamed(&parent_dir, name)
                .map_err(|e| Error::at(&path, e).left_at(&path))?,
            Err(e) if sys::rename_unsupported(&e) => {
                // Not an error path: the scratch holds only our marker, so tidy it.
                let _ = scratch_dir.unlink(MARKER);
                let _ = parent_dir.rmdir(&scratch);
                parent_dir.mkdir(name).map_err(|e| Error::at(&path, e))?;
                parent_dir
                    .open_dir(name)
                    .and_then(|dir| write_marker(&dir).map(|()| dir))
                    .map_err(|e| Error::at(&path, e).left_at(&path))?
            }
            Err(e) => return Err(Error::at(&path, e).left_at(&scratch_path)),
        };
        Ok(OutputTree {
            root,
            path,
            taken: HashSet::new(),
        })
    }

    /// The tree's final path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates the directory `rel` (`/`-separated). Parents must already exist:
    /// create them first, in order.
    pub fn mkdir(&mut self, rel: &str) -> Result<(), Error> {
        self.mkdir_inner(rel).map_err(|e| e.left_at(&self.path))
    }

    /// Creates the new file `rel` and returns its writer.
    ///
    /// `size_hint` is advisory and currently unused (preallocation is unmeasured).
    pub fn create_file(
        &mut self,
        rel: &str,
        size_hint: u64,
        access: Access,
    ) -> Result<TreeFile, Error> {
        let _ = size_hint;
        self.create_file_inner(rel, access)
            .map(|file| TreeFile {
                file,
                tree: self.path.clone(),
            })
            .map_err(|e| e.left_at(&self.path))
    }

    /// Removes the incomplete marker: the tree is done.
    pub fn finish(self) -> Result<(), Error> {
        self.root
            .unlink(MARKER)
            .map_err(|e| Error::at(&self.path.join(MARKER), e).left_at(&self.path))
    }

    fn mkdir_inner(&mut self, rel: &str) -> Result<(), Error> {
        self.claim(rel)?;
        let (dir, leaf) = self.parent_of(rel)?;
        dir.as_ref()
            .unwrap_or(&self.root)
            .mkdir(leaf)
            .map_err(|e| Error::at(&self.path.join(rel), e))
    }

    fn create_file_inner(&mut self, rel: &str, access: Access) -> Result<File, Error> {
        if access == Access::OwnerOnly && !OWNER_ONLY_SUPPORTED {
            return Err(Error::Unsupported(
                "owner-only file access on this platform",
            ));
        }
        self.claim(rel)?;
        let (dir, leaf) = self.parent_of(rel)?;
        dir.as_ref()
            .unwrap_or(&self.root)
            .create_file(leaf, access)
            .map_err(|e| Error::at(&self.path.join(rel), e))
    }

    /// Validates `rel` and reserves it against ASCII-case collisions.
    fn claim(&mut self, rel: &str) -> Result<(), Error> {
        let full = self.path.join(rel);
        validate_path(rel).map_err(|reason| Error::unsafe_at(&full, reason))?;
        if !self.taken.insert(fold(rel)) {
            return Err(Error::unsafe_at(full, Reason::CaseCollision));
        }
        Ok(())
    }

    /// Opens the parent of `rel` (`None` = the root) without following symlinks.
    fn parent_of<'a>(&self, rel: &'a str) -> Result<(Option<Dir>, &'a str), Error> {
        let (parents, leaf) = rel.rsplit_once('/').unwrap_or(("", rel));
        let mut dir: Option<Dir> = None;
        if !parents.is_empty() {
            for part in parents.split('/') {
                let next = dir.as_ref().unwrap_or(&self.root).open_dir(part);
                dir = Some(next.map_err(|e| Error::at(&self.path.join(parents), e))?);
            }
        }
        Ok((dir, leaf))
    }
}

/// A file inside an [`OutputTree`]. Every error is reported against the tree.
pub struct TreeFile {
    file: File,
    tree: PathBuf,
}

impl TreeFile {
    /// Copies `src` to this file in [`CHUNK`]s, checking cancellation and
    /// reporting `Phase::Writing` progress at every chunk boundary.
    pub fn write_from(&mut self, src: &mut dyn Read, ctx: &Ctx<'_>) -> Result<u64, Error> {
        copy(src, &mut self.file, ctx, &self.tree).map_err(|e| e.left_at(&self.tree))
    }

    /// Classifies a failed [`Write`] call: always `Incomplete` at the tree.
    pub fn error(&self, source: io::Error) -> Error {
        Error::at(&self.tree, source).left_at(&self.tree)
    }
}

impl Write for TreeFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Fails if `name` exists in `parent`, or if `parent` itself is an incomplete tree.
pub(crate) fn refuse_existing(parent: &Dir, parent_path: &Path, name: &str) -> Result<(), Error> {
    if has_marker(parent).map_err(|e| Error::at(parent_path, e))? {
        return Err(Error::unsafe_at(parent_path, Reason::MarkedIncomplete));
    }
    let path = parent_path.join(name);
    match parent.kind(name).map_err(|e| Error::at(&path, e))? {
        None => Ok(()),
        Some(Kind::Dir)
            if parent
                .open_dir(name)
                .and_then(|d| has_marker(&d))
                .unwrap_or(false) =>
        {
            Err(Error::unsafe_at(path, Reason::MarkedIncomplete))
        }
        Some(_) => Err(Error::unsafe_at(path, Reason::Exists)),
    }
}

fn has_marker(dir: &Dir) -> io::Result<bool> {
    Ok(dir.kind(MARKER)?.is_some() || dir.kind(PKG_TOOLS_MARKER)?.is_some())
}

fn write_marker(dir: &Dir) -> io::Result<()> {
    dir.create_file(MARKER, Access::Default)?
        .write_all(marker_text())
}

/// Chunked, cancellable copy. Read errors carry no path (the reader's is unknown).
pub(crate) fn copy(
    src: &mut dyn Read,
    dst: &mut File,
    ctx: &Ctx<'_>,
    dst_path: &Path,
) -> Result<u64, Error> {
    let mut buf = vec![0u8; CHUNK];
    let mut done: u64 = 0;
    loop {
        ctx.cancel.check()?;
        let read = match src.read(&mut buf) {
            Ok(0) => return Ok(done),
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(Error::Io {
                    path: PathBuf::new(),
                    source,
                });
            }
        };
        let chunk = buf.get(..read).ok_or_else(|| {
            Error::at(Path::new(""), io::Error::other("reader overran its buffer"))
        })?;
        dst.write_all(chunk).map_err(|e| Error::at(dst_path, e))?;
        done = done.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        ctx.progress.report(&Progress {
            phase: Phase::Writing,
            done,
            total: None,
            item: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use pantheon_core::testing::VecSink;
    use pantheon_core::{Cancel, Classify, ErrorKind, NoProgress, ProgressSink};

    use super::*;
    use crate::inject::{self, Fault};
    use crate::testdir::TestDir;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn build(parent: &Path) -> OutputTree {
        let mut tree = OutputTree::create_new(parent, "CUSA00001").unwrap();
        assert!(tree.path().join(MARKER).exists(), "marker from the start");
        tree.mkdir("sce_sys").unwrap();
        let mut f = tree
            .create_file("sce_sys/param.sfo", 4, Access::Default)
            .unwrap();
        f.write_all(b"PSF\0").unwrap();
        tree
    }

    #[test]
    fn builds_and_finishes_a_tree() {
        let dir = TestDir::new();
        let tree = build(dir.path());
        tree.finish().unwrap();
        assert_eq!(names(dir.path()), ["CUSA00001"], "no scratch left");
        let root = dir.path().join("CUSA00001");
        assert_eq!(names(&root), ["sce_sys"]);
        assert_eq!(
            std::fs::read(root.join("sce_sys/param.sfo")).unwrap(),
            b"PSF\0"
        );
    }

    #[test]
    fn no_replace_rename_unsupported_creates_directly_with_marker() {
        let dir = TestDir::new();
        let _faults = inject::arm(&[Fault::NoNoReplaceRename]);
        let tree = build(dir.path());
        assert_eq!(names(dir.path()), ["CUSA00001"], "scratch tidied");
        tree.finish().unwrap();
        assert!(!dir.path().join("CUSA00001").join(MARKER).exists());
    }

    #[test]
    fn crash_before_rename_leaves_only_a_marked_scratch() {
        let dir = TestDir::new();
        let _faults = inject::arm(&[Fault::CrashBeforeRename]);
        let err = OutputTree::create_new(dir.path(), "CUSA00001")
            .err()
            .unwrap();
        let scratch = err.incomplete().unwrap().to_path_buf();
        let listing = names(dir.path());
        assert_eq!(listing.len(), 1);
        assert!(crate::name::is_reserved(&listing[0]), "{listing:?}");
        assert_eq!(names(&scratch), [MARKER]);
        assert!(
            !dir.path().join("CUSA00001").exists(),
            "final name never unmarked"
        );
        // A reader refuses both the scratch and its parent.
        assert!(crate::SourceTree::open(&scratch).is_err());
        assert!(crate::SourceTree::open(dir.path()).is_err());
    }

    #[test]
    fn existing_output_is_refused() {
        let dir = TestDir::new();
        std::fs::create_dir(dir.path().join("out")).unwrap();
        std::fs::write(dir.path().join("file"), b"").unwrap();
        for name in ["out", "file"] {
            let err = OutputTree::create_new(dir.path(), name).err().unwrap();
            assert_eq!(err.kind(), ErrorKind::UnsafeOutput);
            assert_eq!(err.incomplete(), None);
        }
        assert_eq!(names(dir.path()), ["file", "out"]);
    }

    #[test]
    fn marker_present_is_refused_with_its_reason() {
        let dir = TestDir::new();
        for marker in [MARKER, PKG_TOOLS_MARKER] {
            let old = dir.path().join(marker.trim_start_matches('.'));
            std::fs::create_dir(&old).unwrap();
            std::fs::write(old.join(marker), b"").unwrap();
            let err = OutputTree::create_new(dir.path(), marker.trim_start_matches('.'))
                .err()
                .unwrap();
            assert!(
                matches!(
                    err,
                    Error::Unsafe {
                        reason: Reason::MarkedIncomplete,
                        ..
                    }
                ),
                "{err}"
            );
            // Writing into the marked tree is refused too.
            assert!(matches!(
                OutputTree::create_new(&old, "inner").err().unwrap(),
                Error::Unsafe {
                    reason: Reason::MarkedIncomplete,
                    ..
                }
            ));
            assert!(matches!(
                crate::OutputFile::create_new(&old.join("f"), Access::Default)
                    .err()
                    .unwrap(),
                Error::Unsafe {
                    reason: Reason::MarkedIncomplete,
                    ..
                }
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_in_destination_parent_is_refused() {
        let dir = TestDir::new();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        for parent in [dir.path().join("link"), dir.path().join("link/.")] {
            let err = OutputTree::create_new(&parent, "out").err().unwrap();
            assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{err}");
            let err = crate::OutputFile::create_new(&parent.join("f"), Access::Default)
                .err()
                .unwrap();
            assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{err}");
        }
        assert!(names(&real).is_empty(), "nothing written through the link");
        let err = OutputTree::create_new(&dir.path().join("real/../real"), "out")
            .err()
            .unwrap();
        assert!(matches!(
            err,
            Error::Unsafe {
                reason: Reason::Traversal,
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_inside_the_tree_is_not_followed() {
        let dir = TestDir::new();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let mut tree = OutputTree::create_new(dir.path(), "out").unwrap();
        // Something else plants a symlink where our directory would go.
        std::os::unix::fs::symlink(&outside, tree.path().join("d")).unwrap();
        let err = tree.mkdir("d").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::UnsafeOutput);
        assert_eq!(err.incomplete(), Some(tree.path()));
        tree.taken.clear();
        let err = tree.create_file("d/f", 0, Access::Default).err().unwrap();
        assert_eq!(err.kind(), ErrorKind::UnsafeOutput);
        assert!(names(&outside).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn junction_in_destination_parent_is_refused() {
        let dir = TestDir::new();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("junction");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .status()
            .unwrap();
        assert!(status.success());
        let err = OutputTree::create_new(&link, "out").err().unwrap();
        assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{err}");
        let err = crate::OutputFile::create_new(&link.join("f"), Access::Default)
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{err}");
        assert!(names(&real).is_empty());
    }

    #[test]
    fn case_collisions_and_reserved_names_inside_a_package() {
        let dir = TestDir::new();
        let mut tree = OutputTree::create_new(dir.path(), "out").unwrap();
        tree.mkdir("Dir").unwrap();
        tree.create_file("Dir/A.bin", 0, Access::Default).unwrap();
        let cases = [
            ("dir", Reason::CaseCollision),
            ("DIR/a.BIN", Reason::CaseCollision),
            (".pantheon-incomplete", Reason::ReservedName),
            ("Dir/.PANTHEON-INCOMPLETE", Reason::ReservedName),
            ("Dir/.a.bin.pantheon-00.partial", Reason::ReservedName),
            ("Dir/../x", Reason::DotName),
            ("aux", Reason::ReservedDevice),
        ];
        for (rel, reason) in cases {
            let err = tree.create_file(rel, 0, Access::Default).err().unwrap();
            assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{rel}");
            assert_eq!(err.incomplete(), Some(tree.path()), "{rel}");
            let Error::Incomplete { cause, .. } = err else {
                unreachable!()
            };
            assert!(
                matches!(*cause, Error::Unsafe { reason: r, .. } if r == reason),
                "{rel}"
            );
        }
        assert!(tree.path().join(MARKER).exists(), "marker never replaced");
    }

    #[test]
    fn cancellation_mid_write_is_incomplete_and_keeps_the_marker() {
        let dir = TestDir::new();
        let mut tree = OutputTree::create_new(dir.path(), "out").unwrap();
        let mut f = tree.create_file("big.bin", 0, Access::Default).unwrap();
        let cancel = Cancel::new();
        let sink = VecSink::default();
        let data = vec![0u8; CHUNK * 3];
        // Cancels from inside the progress callback after the first chunk.
        struct CancelOnReport<'a>(&'a Cancel, &'a VecSink);
        impl ProgressSink for CancelOnReport<'_> {
            fn report(&self, progress: &Progress<'_>) {
                self.1.report(progress);
                self.0.cancel();
            }
        }
        let progress = CancelOnReport(&cancel, &sink);
        let err = f
            .write_from(&mut data.as_slice(), &Ctx::new(&cancel, &progress))
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Cancelled);
        assert_eq!(err.incomplete(), Some(tree.path()));
        let written = std::fs::metadata(tree.path().join("big.bin"))
            .unwrap()
            .len();
        assert_eq!(
            written,
            u64::try_from(CHUNK).unwrap(),
            "stopped at a chunk boundary"
        );
        assert!(tree.path().join(MARKER).exists());
        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].phase, events[0].done), (Phase::Writing, written));

        let mut g = tree.create_file("small.bin", 0, Access::Default).unwrap();
        let fresh = Cancel::new();
        let n = g
            .write_from(&mut &b"abc"[..], &Ctx::new(&fresh, &NoProgress))
            .unwrap();
        assert_eq!(n, 3);
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_tree_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        let mut tree = OutputTree::create_new(dir.path(), "out").unwrap();
        tree.create_file("key", 0, Access::OwnerOnly).unwrap();
        let mode = std::fs::metadata(tree.path().join("key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
