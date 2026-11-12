//! Unix: every operation is relative to a directory descriptor opened with
//! `O_NOFOLLOW`, so a path component swapped for a symlink is never followed.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use rustix::fs::{self as rfs, AtFlags, FileType, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use super::Kind;
use crate::error::rejected;
use crate::{Access, Reason};

pub(crate) const OWNER_ONLY_SUPPORTED: bool = true;

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const CREATE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
// NONBLOCK keeps a FIFO raced in after the listing from hanging the open.
const READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
const DIR_MODE: Mode = Mode::RWXU.union(Mode::RWXG).union(Mode::RWXO);
const FILE_MODE: Mode = Mode::RUSR
    .union(Mode::WUSR)
    .union(Mode::RGRP)
    .union(Mode::WGRP)
    .union(Mode::ROTH)
    .union(Mode::WOTH);
const OWNER_ONLY_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
/// Errors meaning "this filesystem cannot do that", for both link and rename.
const UNSUPPORTED: [Errno; 5] = [
    Errno::PERM,
    Errno::OPNOTSUPP,
    Errno::NOTSUP,
    Errno::XDEV,
    Errno::NOSYS,
];

/// An open, checked directory.
pub(crate) struct Dir(OwnedFd);

impl Dir {
    /// Opens `path` one component at a time from `/` or `.`, never following a symlink.
    pub(crate) fn open_path(path: &Path) -> io::Result<Dir> {
        let start = if path.has_root() { "/" } else { "." };
        let mut dir = Dir(rfs::open(start, DIR_FLAGS, Mode::empty())?);
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(name) => dir = dir.open_dir(name)?,
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(rejected(Reason::Traversal));
                }
            }
        }
        Ok(dir)
    }

    pub(crate) fn open_dir(&self, name: impl AsRef<OsStr>) -> io::Result<Dir> {
        let name = name.as_ref();
        match rfs::openat(&self.0, name, DIR_FLAGS, Mode::empty()) {
            Ok(fd) => Ok(Dir(fd)),
            // Linux reports a symlink under O_DIRECTORY|O_NOFOLLOW as ENOTDIR.
            Err(Errno::NOTDIR) if self.kind(name)? == Some(Kind::Symlink) => {
                Err(rejected(Reason::Symlink))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn mkdir(&self, name: &str) -> io::Result<()> {
        Ok(rfs::mkdirat(&self.0, name, DIR_MODE)?)
    }

    /// Creates a new regular file; fails if anything exists under `name`.
    pub(crate) fn create_file(&self, name: &str, access: Access) -> io::Result<File> {
        let mode = match access {
            Access::Default => FILE_MODE,
            Access::OwnerOnly => OWNER_ONLY_MODE,
        };
        Ok(File::from(rfs::openat(&self.0, name, CREATE_FLAGS, mode)?))
    }

    /// Opens an existing regular file for reading, confirmed by `fstat`.
    pub(crate) fn open_file(&self, name: impl AsRef<OsStr>) -> io::Result<File> {
        let fd = rfs::openat(&self.0, name.as_ref(), READ_FLAGS, Mode::empty())?;
        if FileType::from_raw_mode(rfs::fstat(&fd)?.st_mode) != FileType::RegularFile {
            return Err(rejected(Reason::SpecialFile));
        }
        Ok(File::from(fd))
    }

    /// What `name` is, without following it; `None` if absent.
    pub(crate) fn kind(&self, name: impl AsRef<OsStr>) -> io::Result<Option<Kind>> {
        match rfs::statat(&self.0, name.as_ref(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => Kind::Dir,
                FileType::RegularFile => Kind::File,
                FileType::Symlink => Kind::Symlink,
                _ => Kind::Other,
            })),
            Err(Errno::NOENT) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Entry names other than `.` and `..`, in no particular order.
    pub(crate) fn list(&self) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        for entry in rfs::Dir::read_from(&self.0)? {
            let bytes = entry?.file_name().to_bytes().to_vec();
            if bytes != b"." && bytes != b".." {
                names.push(OsStr::from_bytes(&bytes).to_os_string());
            }
        }
        Ok(names)
    }

    pub(crate) fn unlink(&self, name: &str) -> io::Result<()> {
        Ok(rfs::unlinkat(&self.0, name, AtFlags::empty())?)
    }

    pub(crate) fn rmdir(&self, name: &str) -> io::Result<()> {
        Ok(rfs::unlinkat(&self.0, name, AtFlags::REMOVEDIR)?)
    }

    pub(super) fn link(&self, from: &str, to: &str) -> io::Result<()> {
        Ok(rfs::linkat(&self.0, from, &self.0, to, AtFlags::empty())?)
    }

    /// `renameat2(RENAME_NOREPLACE)` on Linux, `renameatx_np(RENAME_EXCL)` on macOS.
    pub(super) fn rename_noreplace(&self, from: &str, to: &str) -> io::Result<()> {
        Ok(rfs::renameat_with(
            &self.0,
            from,
            &self.0,
            to,
            RenameFlags::NOREPLACE,
        )?)
    }

    /// The handle for this directory after `parent` renamed it to `name`.
    /// The descriptor follows the directory, so no second lookup is needed.
    pub(crate) fn renamed(self, _parent: &Dir, _name: &str) -> io::Result<Dir> {
        Ok(self)
    }
}

pub(crate) fn is_symlink_error(e: &io::Error) -> bool {
    Errno::from_io_error(e) == Some(Errno::LOOP)
}

/// `EINVAL` is how Linux reports `RENAME_NOREPLACE` on a filesystem without it.
pub(super) fn is_unsupported(e: &io::Error, rename: bool) -> bool {
    Errno::from_io_error(e)
        .is_some_and(|errno| UNSUPPORTED.contains(&errno) || (rename && errno == Errno::INVAL))
}

#[cfg(test)]
pub(super) fn unsupported_error() -> io::Error {
    Errno::PERM.into()
}

pub(crate) fn file_id(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

pub(crate) fn os_random(buf: &mut [u8]) -> io::Result<()> {
    File::open("/dev/urandom")?.read_exact(buf)
}
