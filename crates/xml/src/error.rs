use std::{fmt, io};

use pantheon_core::{Classify, ErrorKind, LimitError};

/// Reader and writer errors. Offsets are byte offsets into the input.
#[derive(Debug)]
pub enum Error {
    // Malformed: the input is not well-formed XML.
    /// The input is not valid UTF-8.
    Utf8 {
        offset: usize,
    },
    /// A character outside the XML 1.0 `Char` production.
    InvalidChar {
        offset: usize,
    },
    /// Malformed markup.
    Syntax {
        offset: usize,
        what: &'static str,
    },
    /// An end tag does not match its start tag, or the input ends inside an element.
    Unbalanced {
        offset: usize,
    },
    DuplicateAttribute {
        offset: usize,
    },
    /// A character reference to a code point that is not an XML `Char`.
    InvalidCharRef {
        offset: usize,
    },

    // Outside the supported subset.
    /// Any `<!DOCTYPE`: no DTDs, so no external or custom entities.
    Doctype {
        offset: usize,
    },
    /// An entity other than the five predefined ones.
    UnknownEntity {
        offset: usize,
    },
    /// A malformed or misplaced XML declaration, or one with a non-UTF-8 encoding,
    /// a version other than 1.x or a standalone value other than yes/no.
    Declaration {
        offset: usize,
        what: &'static str,
    },
    /// A prefixed name or an `xmlns` attribute.
    Namespace {
        offset: usize,
    },
    Limit(LimitError),

    // Writer.
    Io(io::Error),
    /// The writer was called out of order or given unrepresentable content (a caller bug).
    Misuse(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Utf8 { offset } => write!(f, "XML is not valid UTF-8 at byte {offset}"),
            Error::InvalidChar { offset } => write!(f, "invalid XML character at byte {offset}"),
            Error::Syntax { offset, what } => write!(f, "malformed XML at byte {offset}: {what}"),
            Error::Unbalanced { offset } => write!(f, "unbalanced XML tags at byte {offset}"),
            Error::DuplicateAttribute { offset } => {
                write!(f, "duplicate XML attribute at byte {offset}")
            }
            Error::InvalidCharRef { offset } => {
                write!(f, "invalid XML character reference at byte {offset}")
            }
            Error::Doctype { offset } => write!(f, "XML DOCTYPE at byte {offset} is not supported"),
            Error::UnknownEntity { offset } => write!(f, "unknown XML entity at byte {offset}"),
            Error::Declaration { offset, what } => {
                write!(f, "unsupported XML declaration at byte {offset}: {what}")
            }
            Error::Namespace { offset } => {
                write!(f, "XML namespaces at byte {offset} are not supported")
            }
            Error::Limit(e) => e.fmt(f),
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Misuse(what) => write!(f, "XML writer misuse: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Limit(e) => Some(e),
            _ => None,
        }
    }
}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Utf8 { .. }
            | Error::InvalidChar { .. }
            | Error::Syntax { .. }
            | Error::Unbalanced { .. }
            | Error::DuplicateAttribute { .. }
            | Error::InvalidCharRef { .. }
            | Error::UnknownEntity { .. } => ErrorKind::Malformed,
            Error::Doctype { .. } | Error::Declaration { .. } | Error::Namespace { .. } => {
                ErrorKind::Unsupported
            }
            Error::Limit(_) => ErrorKind::Limit,
            Error::Io(_) => ErrorKind::Io,
            // A caller bug, not hostile input (same choice as core's BadAlignment).
            Error::Misuse(_) => ErrorKind::Unsupported,
        }
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
