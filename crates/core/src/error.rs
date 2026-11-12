//! Shared error classification (architecture §4.1, D31).

use std::fmt;
use std::path::Path;

/// The class every Pantheon error maps to. Frontends turn it into an exit code
/// or a message category; libraries never print.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Bad arguments (CLI only).
    Usage,
    /// Input violates the format.
    Malformed,
    /// Valid but outside the supported matrix.
    Unsupported,
    /// Exceeds an explicit resource policy.
    Limit,
    /// Required decryption material is missing or wrong.
    KeyUnavailable,
    /// Output path refused (exists, symlink, collision, traversal).
    UnsafeOutput,
    /// Operating-system I/O failure.
    Io,
    /// Hermes transport failure.
    Network,
    /// Some items completed, others failed (P13).
    Partial,
    /// The user cancelled.
    Cancelled,
}

impl ErrorKind {
    /// CLI exit code for this kind (D31).
    pub const fn exit_code(self) -> u8 {
        match self {
            ErrorKind::Usage => 2,
            ErrorKind::Malformed => 3,
            ErrorKind::Unsupported => 4,
            ErrorKind::Limit => 5,
            ErrorKind::KeyUnavailable => 6,
            ErrorKind::UnsafeOutput => 7,
            ErrorKind::Io => 8,
            ErrorKind::Network => 9,
            ErrorKind::Partial => 10,
            ErrorKind::Cancelled => 130,
        }
    }
}

/// Implemented by every crate's `Error` so frontends can classify it uniformly.
pub trait Classify: std::error::Error {
    /// The shared class of this error.
    fn kind(&self) -> ErrorKind;
    /// Where partial output was left behind, if any (`Incomplete { path }`).
    fn incomplete(&self) -> Option<&Path> {
        None
    }
}

/// An offset/length computation overflowed or left its bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeError {
    /// The arithmetic itself overflowed `u64`.
    Overflow,
    /// The result lies outside the containing range.
    OutOfBounds { end: u64, limit: u64 },
    /// An alignment of zero or a non-power of two was requested.
    BadAlignment(u64),
}

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RangeError::Overflow => f.write_str("offset arithmetic overflowed"),
            RangeError::OutOfBounds { end, limit } => {
                write!(f, "range ends at {end}, past the limit {limit}")
            }
            RangeError::BadAlignment(align) => write!(f, "invalid alignment {align}"),
        }
    }
}

impl std::error::Error for RangeError {}

impl Classify for RangeError {
    fn kind(&self) -> ErrorKind {
        match self {
            // A bad alignment is a caller bug, not hostile input, but it still must not panic.
            RangeError::BadAlignment(_) => ErrorKind::Unsupported,
            _ => ErrorKind::Malformed,
        }
    }
}

/// A value exceeded an explicit resource limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitError {
    /// Which limit (for messages), e.g. `"sfo size"`.
    pub what: &'static str,
    pub value: u64,
    pub max: u64,
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} exceeds the limit {}",
            self.what, self.value, self.max
        )
    }
}

impl std::error::Error for LimitError {}

impl Classify for LimitError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Limit
    }
}

/// Returned by [`crate::Cancel::check`] once cancellation was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

impl Classify for Cancelled {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Cancelled
    }
}

/// Errors from the core readers.
#[derive(Debug)]
pub enum Error {
    Range(RangeError),
    Limit(LimitError),
    Cancelled,
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Range(e) => e.fmt(f),
            Error::Limit(e) => e.fmt(f),
            Error::Cancelled => Cancelled.fmt(f),
            Error::Io(e) => write!(f, "I/O error: {e}"),
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
            Error::Range(e) => e.kind(),
            Error::Limit(e) => e.kind(),
            Error::Cancelled => ErrorKind::Cancelled,
            // A short read of a declared length means the input is truncated.
            Error::Io(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => ErrorKind::Malformed,
            Error::Io(_) => ErrorKind::Io,
        }
    }
}

impl From<RangeError> for Error {
    fn from(e: RangeError) -> Self {
        Error::Range(e)
    }
}

impl From<LimitError> for Error {
    fn from(e: LimitError) -> Self {
        Error::Limit(e)
    }
}

impl From<Cancelled> for Error {
    fn from(_: Cancelled) -> Self {
        Error::Cancelled
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_d31() {
        let expected = [
            (ErrorKind::Usage, 2),
            (ErrorKind::Malformed, 3),
            (ErrorKind::Unsupported, 4),
            (ErrorKind::Limit, 5),
            (ErrorKind::KeyUnavailable, 6),
            (ErrorKind::UnsafeOutput, 7),
            (ErrorKind::Io, 8),
            (ErrorKind::Network, 9),
            (ErrorKind::Partial, 10),
            (ErrorKind::Cancelled, 130),
        ];
        for (kind, code) in expected {
            assert_eq!(kind.exit_code(), code, "{kind:?}");
        }
    }

    #[test]
    fn truncated_read_is_malformed() {
        let eof = Error::Io(std::io::ErrorKind::UnexpectedEof.into());
        assert_eq!(eof.kind(), ErrorKind::Malformed);
        let other = Error::Io(std::io::ErrorKind::PermissionDenied.into());
        assert_eq!(other.kind(), ErrorKind::Io);
    }
}
