use std::fmt;

use pantheon_core::{Classify, ErrorKind, LimitError, RangeError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Malformed(&'static str),
    UnsupportedVersion(u32),
    Limit(LimitError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Malformed(why) => write!(f, "malformed PARAM.SFO: {why}"),
            Error::UnsupportedVersion(v) => write!(f, "unsupported PARAM.SFO version {v:#x}"),
            Error::Limit(e) => write!(f, "PARAM.SFO: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Malformed(_) => ErrorKind::Malformed,
            Error::UnsupportedVersion(_) => ErrorKind::Unsupported,
            Error::Limit(_) => ErrorKind::Limit,
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
        Error::Malformed("range outside input")
    }
}
