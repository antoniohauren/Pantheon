//! Windows: UNVERIFIED (no Windows target in Phase 1; docs/spikes/phase1-fs.md).
//!
//! `std` has no handle-relative open, so this layer is path-based and weaker
//! than Unix: paths are `\\?\` verbatim, every open uses
//! `FILE_FLAG_OPEN_REPARSE_POINT` and rejects reparse points afterwards, and the
//! directory itself is re-checked before each operation. A parent swapped between
//! the check and the use is caught only if it became a reparse point.
//!
//! The crate's only two `unsafe` blocks live here (D22, architecture §9).

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf, Prefix};

use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    MoveFileExW,
};

use super::Kind;
use crate::error::rejected;
use crate::{Access, Reason};

/// Owner-only ACLs are pending their spike; `Access::OwnerOnly` is refused.
pub(crate) const OWNER_ONLY_SUPPORTED: bool = false;

const ERROR_INVALID_FUNCTION: i32 = 1;
const ERROR_NOT_SAME_DEVICE: i32 = 17;
const ERROR_NOT_SUPPORTED: i32 = 50;
const ERROR_CANT_RESOLVE_FILENAME: i32 = 1921;
const STATUS_SUCCESS: i32 = 0;
/// No `MOVEFILE_REPLACE_EXISTING`, no `MOVEFILE_COPY_ALLOWED`.
const MOVE_NOREPLACE: u32 = 0;
/// Keeps every `BCryptGenRandom` length well inside `u32`.
const RANDOM_CHUNK: usize = 1 << 16;
/// Open the object itself (not a reparse target); allow directory handles.
const NOFOLLOW_DIR: u32 = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;

/// A checked directory, held as a `\\?\` path.
pub(crate) struct Dir {
    path: PathBuf,
}

impl Dir {
    pub(crate) fn open_path(path: &Path) -> io::Result<Dir> {
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(rejected(Reason::Traversal));
        }
        let full = verbatim(path)?;
        // Check every ancestor, root first.
        let mut ancestors: Vec<&Path> = full.ancestors().collect();
        ancestors.reverse();
        for ancestor in ancestors {
            check_dir(ancestor)?;
        }
        Ok(Dir { path: full })
    }

    pub(crate) fn open_dir(&self, name: impl AsRef<OsStr>) -> io::Result<Dir> {
        let path = self.child(name.as_ref())?;
        check_dir(&path)?;
        Ok(Dir { path })
    }

    pub(crate) fn mkdir(&self, name: &str) -> io::Result<()> {
        std::fs::create_dir(self.child(name.as_ref())?)
    }

    pub(crate) fn create_file(&self, name: &str, access: Access) -> io::Result<File> {
        if access != Access::Default {
            return Err(io::ErrorKind::Unsupported.into());
        }
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.child(name.as_ref())?)
    }

    pub(crate) fn open_file(&self, name: impl AsRef<OsStr>) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.child(name.as_ref())?)?;
        check(&file.metadata()?, Kind::File)?;
        Ok(file)
    }

    pub(crate) fn kind(&self, name: impl AsRef<OsStr>) -> io::Result<Option<Kind>> {
        match std::fs::symlink_metadata(self.child(name.as_ref())?) {
            Ok(meta) => Ok(Some(kind_of(&meta))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn list(&self) -> io::Result<Vec<OsString>> {
        check_dir(&self.path)?;
        std::fs::read_dir(&self.path)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect()
    }

    pub(crate) fn unlink(&self, name: &str) -> io::Result<()> {
        std::fs::remove_file(self.child(name.as_ref())?)
    }

    pub(crate) fn rmdir(&self, name: &str) -> io::Result<()> {
        std::fs::remove_dir(self.child(name.as_ref())?)
    }

    pub(super) fn link(&self, from: &str, to: &str) -> io::Result<()> {
        std::fs::hard_link(self.child(from.as_ref())?, self.child(to.as_ref())?)
    }

    pub(super) fn rename_noreplace(&self, from: &str, to: &str) -> io::Result<()> {
        move_file_noreplace(&self.child(from.as_ref())?, &self.child(to.as_ref())?)
    }

    /// Paths do not follow a rename, so the new name is opened and checked again.
    pub(crate) fn renamed(self, parent: &Dir, name: &str) -> io::Result<Dir> {
        parent.open_dir(name)
    }

    /// `self/name`, after re-checking that `self` is still a plain directory.
    fn child(&self, name: &OsStr) -> io::Result<PathBuf> {
        check_dir(&self.path)?;
        Ok(self.path.join(name))
    }
}

/// `path` made absolute and `\\?\`-prefixed, so no Win32 name rewriting applies.
fn verbatim(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let Some(Component::Prefix(prefix)) = absolute.components().next() else {
        return Err(rejected(Reason::Traversal));
    };
    let mut out = match prefix.kind() {
        Prefix::Verbatim(_) | Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(..) => {
            return Ok(absolute);
        }
        Prefix::Disk(drive) => PathBuf::from(format!(r"\\?\{}:\", char::from(drive))),
        Prefix::UNC(server, share) => {
            let mut root = OsString::from(r"\\?\UNC\");
            root.push(server);
            root.push(r"\");
            root.push(share);
            root.push(r"\");
            PathBuf::from(root)
        }
        Prefix::DeviceNS(_) => return Err(rejected(Reason::Traversal)),
    };
    for component in absolute.components() {
        if let Component::Normal(name) = component {
            out.push(name);
        }
    }
    Ok(out)
}

fn check_dir(path: &Path) -> io::Result<()> {
    let handle = OpenOptions::new()
        .access_mode(0)
        .custom_flags(NOFOLLOW_DIR)
        .open(path)?;
    check(&handle.metadata()?, Kind::Dir)
}

fn kind_of(meta: &Metadata) -> Kind {
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Kind::Symlink
    } else if meta.is_dir() {
        Kind::Dir
    } else if meta.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

fn check(meta: &Metadata, want: Kind) -> io::Result<()> {
    match (kind_of(meta), want) {
        (Kind::Symlink, _) => Err(rejected(Reason::Symlink)),
        (found, wanted) if found == wanted => Ok(()),
        (_, Kind::Dir) => Err(rejected(Reason::NotDirectory)),
        _ => Err(rejected(Reason::SpecialFile)),
    }
}

/// NUL-terminated UTF-16 for a Win32 call.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut units: Vec<u16> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(rejected(Reason::ForbiddenChar));
    }
    units.push(0);
    Ok(units)
}

#[allow(unsafe_code)]
fn move_file_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let from = wide(from)?;
    let to = wide(to)?;
    // SAFETY: `from` and `to` are owned, NUL-terminated UTF-16 buffers with no
    // interior NUL (checked by `wide`), alive until the call returns; MoveFileExW
    // only reads them and keeps no pointer. The flags are plain integers.
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVE_NOREPLACE) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[allow(unsafe_code)]
pub(crate) fn os_random(buf: &mut [u8]) -> io::Result<()> {
    for chunk in buf.chunks_mut(RANDOM_CHUNK) {
        let len = u32::try_from(chunk.len()).map_err(io::Error::other)?;
        // SAFETY: `chunk` is an exclusively borrowed, writable buffer of exactly
        // `len` bytes for the whole call. With BCRYPT_USE_SYSTEM_PREFERRED_RNG the
        // algorithm handle must be null, which it is. BCryptGenRandom keeps no pointer.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                chunk.as_mut_ptr(),
                len,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != STATUS_SUCCESS {
            return Err(io::Error::other(format!(
                "BCryptGenRandom failed with NTSTATUS {status:#x}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn is_symlink_error(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_CANT_RESOLVE_FILENAME)
}

pub(super) fn is_unsupported(e: &io::Error, _rename: bool) -> bool {
    matches!(
        e.raw_os_error(),
        Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_NOT_SAME_DEVICE)
    )
}

#[cfg(test)]
pub(super) fn unsupported_error() -> io::Error {
    io::Error::from_raw_os_error(ERROR_NOT_SUPPORTED)
}

/// Windows `std` has no stable file index; the creation time stands in (unverified).
pub(crate) fn file_id(meta: &Metadata) -> (u64, u64) {
    (0, meta.creation_time())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_file_noreplace_refuses_to_replace() {
        let dir = crate::testdir::TestDir::new();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        assert!(move_file_noreplace(&a, &b).is_err());
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        std::fs::remove_file(&b).unwrap();
        move_file_noreplace(&a, &b).unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"a");
    }
}
