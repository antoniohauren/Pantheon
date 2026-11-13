//! The crate error (architecture §4.1).

use std::fmt;
use std::io;
use std::path::Path;

use pantheon_core::{Classify, ErrorKind, LimitError};

#[derive(Debug)]
pub enum Error {
    /// A path refused by the portable name rules, or a sibling that already
    /// exists (`Exists`) or differs only in ASCII case (`CaseCollision`).
    Name {
        path: String,
        reason: pantheon_fs::Reason,
    },
    /// A component longer than [`crate::MAX_NAME_BYTES`].
    NameTooLong {
        path: String,
    },
    /// The parent directory was not added before its child.
    MissingParent {
        path: String,
    },
    /// The file's reader did not yield exactly the declared number of bytes.
    SizeMismatch {
        path: String,
        expected: u64,
    },
    /// No file has any payload. The pinned reader rejects an archive with an
    /// empty chunk table (carried limit).
    EmptyPayload,
    /// More than the format can address.
    TooLarge(&'static str),
    /// An earlier addition failed, so the archive cannot be finished.
    Poisoned,
    /// The archive being read violates the format.
    Malformed(&'static str),
    Limit(LimitError),
    Compression(pantheon_crypto::Error),
    Fs(pantheon_fs::Error),
    /// Reading a source or the archive failed.
    Io(io::Error),
    Cancelled,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Name { path, reason } => write!(f, "archive path {path:?}: {reason}"),
            Error::NameTooLong { path } => write!(
                f,
                "archive path {path:?} has a component over {} bytes, which the ZArchive reader cannot read",
                crate::MAX_NAME_BYTES
            ),
            Error::MissingParent { path } => {
                write!(f, "archive path {path:?}: its parent folder was not added")
            }
            Error::SizeMismatch { path, expected } => write!(
                f,
                "{path:?} did not yield exactly {expected} bytes (did the source change?)"
            ),
            Error::EmptyPayload => f.write_str(
                "every file is empty; ZArchive readers reject an archive without payload",
            ),
            Error::TooLarge(what) => write!(f, "too large for ZArchive: {what}"),
            Error::Poisoned => f.write_str("an earlier addition failed; the archive is unusable"),
            Error::Malformed(what) => write!(f, "malformed ZArchive: {what}"),
            Error::Limit(e) => e.fmt(f),
            Error::Compression(e) => e.fmt(f),
            Error::Fs(e) => e.fmt(f),
            Error::Io(e) => write!(f, "I/O failure: {e}"),
            Error::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Limit(e) => Some(e),
            Error::Compression(e) => Some(e),
            Error::Fs(e) => Some(e),
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Name { .. } => ErrorKind::UnsafeOutput,
            Error::NameTooLong { .. }
            | Error::MissingParent { .. }
            | Error::EmptyPayload
            | Error::Poisoned => ErrorKind::Unsupported,
            Error::TooLarge(_) | Error::Limit(_) => ErrorKind::Limit,
            Error::Malformed(_) => ErrorKind::Malformed,
            Error::Compression(e) => e.kind(),
            Error::Fs(e) => e.kind(),
            Error::SizeMismatch { .. } | Error::Io(_) => ErrorKind::Io,
            Error::Cancelled => ErrorKind::Cancelled,
        }
    }

    fn incomplete(&self) -> Option<&Path> {
        match self {
            Error::Fs(e) => e.incomplete(),
            _ => None,
        }
    }
}

impl From<pantheon_fs::Error> for Error {
    fn from(e: pantheon_fs::Error) -> Self {
        match e {
            pantheon_fs::Error::Cancelled => Error::Cancelled,
            e => Error::Fs(e),
        }
    }
}

impl From<pantheon_core::Cancelled> for Error {
    fn from(_: pantheon_core::Cancelled) -> Self {
        Error::Cancelled
    }
}

impl From<pantheon_core::RangeError> for Error {
    fn from(_: pantheon_core::RangeError) -> Self {
        Error::Malformed("offset outside its table")
    }
}

impl From<LimitError> for Error {
    fn from(e: LimitError) -> Self {
        Error::Limit(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}
