use std::{fmt, io};

use pantheon_core::{Classify, ErrorKind, LimitError, RangeError};

#[derive(Debug)]
pub enum Error {
    Malformed(&'static str),
    Unsupported(&'static str),
    /// An entry path fails the portable name rules or collides with another.
    UnsafePath {
        path: String,
        reason: pantheon_fs::Reason,
    },
    Limit(LimitError),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Malformed(why) => write!(f, "malformed PS3 package: {why}"),
            Error::Unsupported(why) => write!(f, "unsupported PS3 package: {why}"),
            Error::UnsafePath { path, reason } => write!(f, "unsafe entry path {path:?}: {reason}"),
            Error::Limit(e) => write!(f, "PS3 package: {e}"),
            Error::Io(e) => write!(f, "PS3 package I/O failure: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Malformed(_) => ErrorKind::Malformed,
            Error::Unsupported(_) => ErrorKind::Unsupported,
            Error::UnsafePath { .. } => ErrorKind::UnsafeOutput,
            Error::Limit(_) => ErrorKind::Limit,
            Error::Io(_) => ErrorKind::Io,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::Malformed("truncated input")
        } else {
            Error::Io(e)
        }
    }
}

impl From<LimitError> for Error {
    fn from(e: LimitError) -> Self {
        Error::Limit(e)
    }
}

impl From<RangeError> for Error {
    fn from(_: RangeError) -> Self {
        Error::Malformed("field outside its record")
    }
}

impl From<pantheon_core::Error> for Error {
    fn from(e: pantheon_core::Error) -> Self {
        match e {
            pantheon_core::Error::Range(e) => e.into(),
            pantheon_core::Error::Limit(e) => e.into(),
            pantheon_core::Error::Io(e) => e.into(),
            // Package reads take no Cancel; kept total so no input can panic.
            pantheon_core::Error::Cancelled => Error::Malformed("cancelled"),
        }
    }
}
