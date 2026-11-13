//! Single-file publication without overwrite (foundation §3, D28).

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use pantheon_core::Ctx;

use crate::name::scratch_name;
use crate::sys::{self, Dir, OWNER_ONLY_SUPPORTED};
use crate::tree::{copy, refuse_existing};
use crate::{Access, Error, Reason, validate_component};

/// How the final name will appear, chosen by probing at creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Publication {
    /// Hard link scratch → final, then unlink scratch.
    Link,
    /// Fallback 1: no-replace rename scratch → final (no hard links here).
    Rename,
    /// Fallback 2 (D28): writing straight into `final`, opened `create_new`.
    Direct,
}

/// A new file that appears under its final name only once [`OutputFile::publish`]
/// succeeds (except under fallback 2, see D28).
///
/// Dropping it unpublished removes the scratch file (best effort). The final
/// name is never removed.
pub struct OutputFile {
    parent: Dir,
    name: String,
    path: PathBuf,
    file: File,
    /// The scratch name while one exists.
    scratch: Option<String>,
    publication: Publication,
}

impl OutputFile {
    /// Checks that `final_path` does not exist and creates a scratch sibling
    /// `.<name>.pantheon-<random>.partial`. The parent chain must be symlink-free.
    pub fn create_new(final_path: &Path, access: Access) -> Result<OutputFile, Error> {
        let (parent_path, name) = split_final(final_path)?;
        if access == Access::OwnerOnly && !OWNER_ONLY_SUPPORTED {
            return Err(Error::Unsupported(
                "owner-only file access on this platform",
            ));
        }
        let parent = Dir::open_path(parent_path).map_err(|e| Error::at(parent_path, e))?;
        refuse_existing(&parent, parent_path, name)?;

        let scratch = scratch_name(name)?;
        let file = parent
            .create_file(&scratch, access)
            .map_err(|e| Error::at(&parent_path.join(&scratch), e))?;
        let mut out = OutputFile {
            parent,
            name: name.to_owned(),
            path: final_path.to_path_buf(),
            file,
            scratch: Some(scratch),
            publication: Publication::Link,
        };
        // On error from here, `drop` removes the scratch.
        out.probe(access)?;
        Ok(out)
    }

    /// The final path this file will be published under.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Copies `src` in chunks, checking cancellation at every chunk boundary.
    pub fn write_from(&mut self, src: &mut dyn Read, ctx: &Ctx<'_>) -> Result<u64, Error> {
        let path = self.current_path();
        copy(src, &mut self.file, ctx, &path).map_err(|e| self.classify(e))
    }

    /// Flushes data and metadata to the device.
    pub fn sync(&mut self) -> Result<(), Error> {
        self.file
            .sync_all()
            .map_err(|e| self.classify(Error::at(&self.current_path(), e)))
    }

    /// Classifies a failed [`Write`] call; `Incomplete` under fallback 2.
    pub fn error(&self, source: io::Error) -> Error {
        self.classify(Error::at(&self.current_path(), source))
    }

    /// Makes the file appear under its final name, never replacing anything.
    pub fn publish(mut self) -> Result<(), Error> {
        let Some(scratch) = self.scratch.clone() else {
            return Ok(()); // fallback 2 already wrote to the final name
        };
        let linked = self.publication == Publication::Link
            && match sys::link(&self.parent, &scratch, &self.name) {
                Ok(()) => true,
                Err(e) if sys::link_unsupported(&e) => false,
                Err(e) => return Err(Error::at(&self.path, e)),
            };
        if linked {
            // The final name is complete; a leftover scratch is only clutter.
            let _ = self.parent.unlink(&scratch);
        } else {
            sys::rename_noreplace(&self.parent, &scratch, &self.name)
                .map_err(|e| Error::at(&self.path, e))?;
        }
        self.scratch = None;
        Ok(())
    }

    /// Picks the publication path while the scratch is still empty: try a hard
    /// link to a second scratch name, then a no-replace rename to it, else D28.
    fn probe(&mut self, access: Access) -> Result<(), Error> {
        let Some(scratch) = self.scratch.clone() else {
            return Ok(());
        };
        let probe = scratch_name(&self.name)?;
        let probe_path = self.path.with_file_name(&probe);
        match sys::link(&self.parent, &scratch, &probe) {
            Ok(()) => {
                let _ = self.parent.unlink(&probe);
                return Ok(());
            }
            Err(e) if sys::link_unsupported(&e) => {}
            Err(e) => return Err(Error::at(&probe_path, e)),
        }
        match sys::rename_noreplace(&self.parent, &scratch, &probe) {
            Ok(()) => {
                self.scratch = Some(probe);
                self.publication = Publication::Rename;
                Ok(())
            }
            Err(e) if sys::rename_unsupported(&e) => {
                self.file = self
                    .parent
                    .create_file(&self.name, access)
                    .map_err(|e| Error::at(&self.path, e))?;
                self.publication = Publication::Direct;
                let _ = self.parent.unlink(&scratch);
                self.scratch = None;
                Ok(())
            }
            Err(e) => Err(Error::at(&probe_path, e)),
        }
    }

    fn current_path(&self) -> PathBuf {
        match &self.scratch {
            Some(scratch) => self.path.with_file_name(scratch),
            None => self.path.clone(),
        }
    }

    fn classify(&self, e: Error) -> Error {
        match self.publication {
            Publication::Direct => e.left_at(&self.path),
            _ => e,
        }
    }
}

impl Write for OutputFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for OutputFile {
    fn drop(&mut self) {
        if let Some(scratch) = &self.scratch {
            let _ = self.parent.unlink(scratch);
        }
    }
}

/// The parent directory (`.` when bare) and the validated UTF-8 file name.
fn split_final(final_path: &Path) -> Result<(&Path, &str), Error> {
    let name = match final_path.file_name().map(OsStr::to_str) {
        None => return Err(Error::unsafe_at(final_path, Reason::Empty)),
        Some(None) => return Err(Error::unsafe_at(final_path, Reason::NotUtf8)),
        Some(Some(name)) => name,
    };
    validate_component(name).map_err(|reason| Error::unsafe_at(final_path, reason))?;
    let parent = match final_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    Ok((parent, name))
}

/// A private working file beside an output, for data that must spill to disk
/// before publication (e.g. ZArchive chunk indexes). It is never published;
/// dropping it removes it (best effort).
pub struct ScratchFile {
    parent: Dir,
    name: String,
    path: PathBuf,
    file: File,
}

impl ScratchFile {
    /// Creates `.<name>.pantheon-<random>.partial` next to `near`, which need not exist.
    pub fn create_beside(near: &Path) -> Result<ScratchFile, Error> {
        let (parent_path, name) = split_final(near)?;
        let parent = Dir::open_path(parent_path).map_err(|e| Error::at(parent_path, e))?;
        let scratch = scratch_name(name)?;
        let path = parent_path.join(&scratch);
        let file = parent
            .create_file(&scratch, Access::Default)
            .map_err(|e| Error::at(&path, e))?;
        Ok(ScratchFile {
            parent,
            name: scratch,
            path,
            file,
        })
    }

    /// Where the scratch file lives (for messages).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A new read handle positioned at the start; flush buffered writes first.
    pub fn reopen(&self) -> Result<File, Error> {
        self.parent
            .open_file(&self.name)
            .map_err(|e| Error::at(&self.path, e))
    }
}

impl Write for ScratchFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        let _ = self.parent.unlink(&self.name);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use pantheon_core::{Cancel, Classify, ErrorKind, NoProgress};

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

    fn publish_bytes(path: &Path) -> Publication {
        let mut out = OutputFile::create_new(path, Access::Default).unwrap();
        let publication = out.publication;
        out.write_all(b"payload").unwrap();
        out.sync().unwrap();
        out.publish().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"payload");
        publication
    }

    #[test]
    fn publishes_by_hard_link_and_leaves_no_scratch() {
        let dir = TestDir::new();
        let path = dir.path().join("out.pkg");
        assert_eq!(publish_bytes(&path), Publication::Link);
        assert_eq!(names(dir.path()), ["out.pkg"]);
    }

    #[test]
    fn hard_link_less_filesystem_falls_back_to_rename() {
        let dir = TestDir::new();
        let _faults = inject::arm(&[Fault::NoHardLinks]);
        let path = dir.path().join("out.pkg");
        assert_eq!(publish_bytes(&path), Publication::Rename);
        assert_eq!(names(dir.path()), ["out.pkg"]);
    }

    #[test]
    fn neither_link_nor_rename_writes_directly_and_reports_incomplete() {
        let dir = TestDir::new();
        let _faults = inject::arm(&[Fault::NoHardLinks, Fault::NoNoReplaceRename]);
        let path = dir.path().join("out.pkg");
        assert_eq!(publish_bytes(&path), Publication::Direct);
        assert_eq!(names(dir.path()), ["out.pkg"]);

        // A failure under fallback 2 names the partial final file.
        let path = dir.path().join("partial.pkg");
        let mut out = OutputFile::create_new(&path, Access::Default).unwrap();
        let cancel = Cancel::new();
        cancel.cancel();
        let err = out
            .write_from(&mut &b"data"[..], &Ctx::new(&cancel, &NoProgress))
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Cancelled);
        assert_eq!(err.incomplete(), Some(path.as_path()));
        drop(out);
        assert!(path.exists(), "the final name is never removed");
    }

    #[test]
    fn existing_output_is_refused_and_untouched() {
        let dir = TestDir::new();
        let path = dir.path().join("out.pkg");
        std::fs::write(&path, b"theirs").unwrap();
        let err = OutputFile::create_new(&path, Access::Default)
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::UnsafeOutput);
        assert_eq!(std::fs::read(&path).unwrap(), b"theirs");
        assert_eq!(names(dir.path()), ["out.pkg"]);
    }

    #[test]
    fn output_created_after_the_check_is_never_replaced() {
        let dir = TestDir::new();
        let path = dir.path().join("out.pkg");
        for faults in [&[][..], &[Fault::NoHardLinks]] {
            let _faults = inject::arm(faults);
            let out = OutputFile::create_new(&path, Access::Default).unwrap();
            std::fs::write(&path, b"theirs").unwrap();
            let err = out.publish().unwrap_err();
            assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{faults:?}");
            assert_eq!(std::fs::read(&path).unwrap(), b"theirs");
            assert_eq!(names(dir.path()), ["out.pkg"], "scratch removed on drop");
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    fn drop_without_publish_removes_only_the_scratch() {
        let dir = TestDir::new();
        let mut out = OutputFile::create_new(&dir.path().join("out.pkg"), Access::Default).unwrap();
        out.write_all(b"half").unwrap();
        assert_eq!(names(dir.path()).len(), 1);
        drop(out);
        assert!(names(dir.path()).is_empty());
    }

    #[test]
    fn cancellation_mid_write_stops_and_leaves_nothing() {
        struct CancelAfterFirstChunk<'a>(&'a Cancel, u32);
        impl Read for CancelAfterFirstChunk<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.1 = self.1.saturating_add(1);
                if self.1 == 2 {
                    self.0.cancel();
                }
                buf.fill(0xAA);
                Ok(buf.len())
            }
        }
        let dir = TestDir::new();
        let path = dir.path().join("big.bin");
        let cancel = Cancel::new();
        let mut out = OutputFile::create_new(&path, Access::Default).unwrap();
        let err = out
            .write_from(
                &mut CancelAfterFirstChunk(&cancel, 0),
                &Ctx::new(&cancel, &NoProgress),
            )
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Cancelled);
        assert_eq!(err.incomplete(), None);
        drop(out);
        assert!(names(dir.path()).is_empty());
    }

    #[test]
    fn reserved_and_unsafe_final_names_are_refused() {
        let dir = TestDir::new();
        for name in [
            ".pantheon-incomplete",
            ".x.pantheon-1.partial",
            "CON",
            "a:b",
            "x.",
        ] {
            let err = OutputFile::create_new(&dir.path().join(name), Access::Default)
                .err()
                .unwrap();
            assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{name}");
        }
        assert!(names(dir.path()).is_empty());
    }

    /// Spike probe (docs/spikes/phase1-fs.md): which path does this filesystem take?
    #[test]
    #[ignore = "spike probe: run with TMPDIR set to a directory on the filesystem to test"]
    fn probe_publication_path() {
        let dir = TestDir::new();
        let path = dir.path().join("probe.bin");
        let out = OutputFile::create_new(&path, Access::Default).unwrap();
        eprintln!(
            "{}: file publication {:?}",
            dir.path().display(),
            out.publication
        );
        out.publish().unwrap();
        let parent = Dir::open_path(dir.path()).unwrap();
        let rename = sys::rename_noreplace(&parent, "probe.bin", "renamed.bin");
        eprintln!("no-replace rename (tree path): {rename:?}");
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_is_mode_0600_from_creation() {
        use std::os::unix::fs::PermissionsExt;
        const PERMISSION_BITS: u32 = 0o777;
        const OWNER_RW: u32 = 0o600;
        let dir = TestDir::new();
        let path = dir.path().join("key.gp4");
        let out = OutputFile::create_new(&path, Access::OwnerOnly).unwrap();
        let scratch = dir.path().join(out.scratch.clone().unwrap());
        let mode = std::fs::metadata(&scratch).unwrap().permissions().mode();
        assert_eq!(
            mode & PERMISSION_BITS,
            OWNER_RW,
            "scratch is 0600 before any write"
        );
        out.publish().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & PERMISSION_BITS, OWNER_RW);
    }

    #[cfg(windows)]
    #[test]
    fn owner_only_is_unsupported_on_windows() {
        let dir = TestDir::new();
        let err = OutputFile::create_new(&dir.path().join("key.gp4"), Access::OwnerOnly)
            .err()
            .unwrap();
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn scratch_file_reads_back_and_is_removed_on_drop() {
        let dir = TestDir::new();
        let mut scratch = ScratchFile::create_beside(&dir.path().join("out.zar")).unwrap();
        scratch.write_all(b"records").unwrap();
        scratch.flush().unwrap();
        let mut back = Vec::new();
        scratch.reopen().unwrap().read_to_end(&mut back).unwrap();
        assert_eq!(back, b"records");
        assert!(scratch.path().exists());
        drop(scratch);
        assert!(names(dir.path()).is_empty());
    }
}
