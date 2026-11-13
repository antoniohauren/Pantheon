//! pantheon-zar: ZArchive v1, the read-only archive format shadPS4 mounts
//! (docs/gods/proteus.md).
//!
//! The format, as read by the upstream reader pinned at ZArchive
//! `965b66c8d67b6b7e30fd63b3b75aa91a99ff303b`: one logical stream of every file's
//! bytes, cut into 64 KiB blocks, each stored raw or as an independent zstd frame;
//! then 40-byte offset records (one per 16 blocks), a length-prefixed name table,
//! a breadth-first table of 16-byte nodes, two empty metadata sections and a
//! 144-byte footer carrying a SHA-256 of the whole archive. Integers are big-endian.
//!
//! [`Writer`] streams an archive into an [`pantheon_fs::OutputFile`]; [`Reader`]
//! reads one back with random access, for verification and tests.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

mod error;
mod format;
mod reader;
mod writer;

pub use error::Error;
pub use reader::{Entry, EntryKind, Reader};
pub use writer::{Summary, Writer};

/// Longest path component, in UTF-8 bytes, the pinned reader decodes. The format
/// allows 32,767, but that reader misdecodes the two-byte length prefix, so
/// 128-byte and longer names fail enumeration (pkg-tools `docs/zarchive.md`).
pub const MAX_NAME_BYTES: usize = 127;

/// Checks an archive path (`/`-separated, relative) against the portable name
/// rules and [`MAX_NAME_BYTES`]. Sibling collisions are checked by the writer.
pub fn check_path(path: &str) -> Result<(), Error> {
    pantheon_fs::validate_path(path).map_err(|reason| Error::Name {
        path: path.to_owned(),
        reason,
    })?;
    if path.split('/').any(|c| c.len() > MAX_NAME_BYTES) {
        return Err(Error::NameTooLong {
            path: path.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests;
