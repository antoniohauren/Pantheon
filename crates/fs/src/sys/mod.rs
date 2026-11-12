//! The platform layer: the only `cfg(unix)` / `cfg(windows)` code in the crate.
//!
//! Every function returns raw `io::Error`s; the callers attach paths and classify
//! them with [`crate::Error::at`]. Refusals the layer detects itself (a reparse
//! point, a special file, `..`) travel as [`crate::error::Rejected`].

use std::io;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

pub(crate) use imp::{Dir, OWNER_ONLY_SUPPORTED, file_id, is_symlink_error, os_random};

/// What a directory entry is, without following it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    File,
    Symlink,
    Other,
}

/// Hard link `from` to `to` in `dir`; fails if `to` exists.
pub(crate) fn link(dir: &Dir, from: &str, to: &str) -> io::Result<()> {
    #[cfg(test)]
    if crate::inject::armed(crate::inject::Fault::NoHardLinks) {
        return Err(imp::unsupported_error());
    }
    dir.link(from, to)
}

/// Rename `from` to `to` in `dir` without replacing an existing `to`.
pub(crate) fn rename_noreplace(dir: &Dir, from: &str, to: &str) -> io::Result<()> {
    #[cfg(test)]
    {
        use crate::inject::{Fault, armed};
        if armed(Fault::CrashBeforeRename) {
            return Err(io::Error::other("injected crash before rename"));
        }
        if armed(Fault::NoNoReplaceRename) {
            return Err(imp::unsupported_error());
        }
    }
    dir.rename_noreplace(from, to)
}

/// Whether a failed hard link means "this filesystem cannot", so a fallback may run.
pub(crate) fn link_unsupported(e: &io::Error) -> bool {
    imp::is_unsupported(e, false)
}

/// Whether a failed no-replace rename means "this filesystem cannot".
pub(crate) fn rename_unsupported(e: &io::Error) -> bool {
    imp::is_unsupported(e, true)
}
