//! The crate error (architecture §4.1).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use pantheon_core::{Classify, ErrorKind};

/// Why a name, path or filesystem object was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    Empty,
    /// `.` or `..`.
    DotName,
    /// A component longer than [`crate::MAX_COMPONENT_BYTES`].
    ComponentTooLong,
    /// A path longer than [`crate::MAX_PATH_BYTES`].
    PathTooLong,
    /// More than [`crate::MAX_PATH_COMPONENTS`] components.
    TooManyComponents,
    /// A separator, NUL, control character or one of `<>:"|?*`.
    ForbiddenChar,
    /// `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`.
    ReservedDevice,
    TrailingDotOrSpace,
    NotUtf8,
    /// `.pantheon-incomplete` or a Pantheon scratch name.
    ReservedName,
    /// Equal, ignoring ASCII case, to a sibling.
    CaseCollision,
    /// `..` or a drive/UNC prefix where only plain components are allowed.
    Traversal,
    /// A symlink, junction or other reparse point.
    Symlink,
    NotDirectory,
    /// Not a regular file or directory (FIFO, socket, device).
    SpecialFile,
    Exists,
    /// An incomplete marker is present (`.pantheon-incomplete` or `.pkg-tools-incomplete`).
    MarkedIncomplete,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Reason::Empty => "empty name",
            Reason::DotName => "`.` or `..` name",
            Reason::ComponentTooLong => "name longer than 255 bytes",
            Reason::PathTooLong => "path longer than 4096 bytes",
            Reason::TooManyComponents => "path deeper than 128 components",
            Reason::ForbiddenChar => "name contains a forbidden character",
            Reason::ReservedDevice => "name is a Windows reserved device name",
            Reason::TrailingDotOrSpace => "name ends in a dot or space",
            Reason::NotUtf8 => "name is not valid UTF-8",
            Reason::ReservedName => "name is reserved by Pantheon",
            Reason::CaseCollision => "name collides, ignoring case, with a sibling",
            Reason::Traversal => "path leaves its root",
            Reason::Symlink => "symlink or reparse point",
            Reason::NotDirectory => "not a directory",
            Reason::SpecialFile => "not a regular file or directory",
            Reason::Exists => "already exists",
            Reason::MarkedIncomplete => {
                "incomplete output from an earlier run; remove it before retrying"
            }
        })
    }
}

/// Errors from `pantheon-fs`.
#[derive(Debug)]
pub enum Error {
    /// An output path was refused.
    Unsafe {
        path: PathBuf,
        reason: Reason,
    },
    /// A source folder holds something Pantheon will not read.
    Source {
        path: PathBuf,
        reason: Reason,
    },
    /// A source file changed between its snapshot and the recheck.
    SourceChanged {
        path: PathBuf,
    },
    /// Not available on this platform or filesystem.
    Unsupported(&'static str),
    Cancelled,
    Io {
        path: PathBuf,
        source: io::Error,
    },
    /// The OS random source failed.
    Random(io::Error),
    /// `cause` happened after output was created; it remains at `path`.
    Incomplete {
        path: PathBuf,
        cause: Box<Error>,
    },
}

/// Carried inside an `io::Error` by the platform layer for refusals it detects itself.
#[derive(Debug)]
pub(crate) struct Rejected(pub(crate) Reason);

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Rejected {}

pub(crate) fn rejected(reason: Reason) -> io::Error {
    io::Error::other(Rejected(reason))
}

impl Error {
    /// Classifies an OS error for `path`: refusals become `Unsafe`, the rest `Io`.
    pub(crate) fn at(path: &Path, source: io::Error) -> Error {
        let reason = if let Some(Rejected(reason)) = source.get_ref().and_then(|e| e.downcast_ref())
        {
            Some(*reason)
        } else if crate::sys::is_symlink_error(&source) {
            Some(Reason::Symlink)
        } else {
            match source.kind() {
                io::ErrorKind::AlreadyExists => Some(Reason::Exists),
                io::ErrorKind::NotADirectory => Some(Reason::NotDirectory),
                _ => None,
            }
        };
        match reason {
            Some(reason) => Error::Unsafe {
                path: path.to_path_buf(),
                reason,
            },
            None => Error::Io {
                path: path.to_path_buf(),
                source,
            },
        }
    }

    pub(crate) fn unsafe_at(path: impl Into<PathBuf>, reason: Reason) -> Error {
        Error::Unsafe {
            path: path.into(),
            reason,
        }
    }

    /// Records that output now exists at `path`; never nests.
    pub(crate) fn left_at(self, path: &Path) -> Error {
        match self {
            Error::Incomplete { .. } => self,
            cause => Error::Incomplete {
                path: path.to_path_buf(),
                cause: Box::new(cause),
            },
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsafe { path, reason } => {
                write!(f, "refusing output {}: {reason}", path.display())
            }
            Error::Source { path, reason } => {
                write!(f, "refusing source {}: {reason}", path.display())
            }
            Error::SourceChanged { path } => {
                write!(f, "source {} changed during the operation", path.display())
            }
            Error::Unsupported(what) => write!(f, "unsupported: {what}"),
            Error::Cancelled => f.write_str("cancelled"),
            Error::Io { path, source } if path.as_os_str().is_empty() => {
                write!(f, "I/O error: {source}")
            }
            Error::Io { path, source } => write!(f, "I/O error at {}: {source}", path.display()),
            Error::Random(source) => write!(f, "OS random source failed: {source}"),
            Error::Incomplete { path, cause } => {
                write!(f, "{cause}; incomplete output left at {}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } | Error::Random(source) => Some(source),
            Error::Incomplete { cause, .. } => Some(cause.as_ref()),
            _ => None,
        }
    }
}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Unsafe { .. } => ErrorKind::UnsafeOutput,
            Error::Source { .. } | Error::Unsupported(_) => ErrorKind::Unsupported,
            Error::SourceChanged { .. } | Error::Io { .. } | Error::Random(_) => ErrorKind::Io,
            Error::Cancelled => ErrorKind::Cancelled,
            Error::Incomplete { cause, .. } => cause.kind(),
        }
    }

    fn incomplete(&self) -> Option<&Path> {
        match self {
            Error::Incomplete { path, .. } => Some(path),
            _ => None,
        }
    }
}

impl From<pantheon_core::Cancelled> for Error {
    fn from(_: pantheon_core::Cancelled) -> Error {
        Error::Cancelled
    }
}
