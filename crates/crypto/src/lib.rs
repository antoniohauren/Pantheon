//! pantheon-crypto: narrow, purpose-named crypto and compression (docs/03-foundation.md).
//!
//! The only crate allowed to depend on crypto/compression crates (P6). Key material
//! is never stored here: fake keys and curve parameters are passed in by callers.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

pub mod aes;
pub mod ecdsa_npdrm;
pub mod hash;
pub mod rsa;
pub mod zlib;
pub mod zstd;

use pantheon_core::{Classify, ErrorKind, LimitError};
use std::fmt;

/// Errors from the crypto and compression primitives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A buffer that must hold whole blocks or sectors does not.
    Unaligned { len: usize, unit: usize },
    /// Caller-supplied key or curve parameters are invalid.
    BadKey(&'static str),
    /// Decryption with the supplied key failed (wrong key or damaged data).
    WrongKey,
    /// The input violates its format.
    Malformed(&'static str),
    /// Output would exceed the caller's bound.
    Limit(LimitError),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unaligned { len, unit } => {
                write!(f, "length {len} is not a multiple of {unit}")
            }
            Error::BadKey(what) => write!(f, "invalid key parameters: {what}"),
            Error::WrongKey => f.write_str("decryption failed: wrong key or damaged data"),
            Error::Malformed(what) => write!(f, "malformed input: {what}"),
            Error::Limit(limit) => limit.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Unaligned { .. } | Error::Malformed(_) => ErrorKind::Malformed,
            Error::BadKey(_) => ErrorKind::Unsupported,
            Error::WrongKey => ErrorKind::KeyUnavailable,
            Error::Limit(_) => ErrorKind::Limit,
        }
    }
}

/// `Error::Limit` for an output of `value` bytes against a bound of `max`.
fn limit_error(what: &'static str, value: usize, max: usize) -> Error {
    let wide = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    Error::Limit(LimitError {
        what,
        value: wide(value),
        max: wide(max),
    })
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
pub(crate) mod test_util {
    /// Decodes a hex string (test vectors only).
    pub fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        assert!(s.len().is_multiple_of(2), "odd hex length");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
            .collect()
    }

    /// Decodes a hex string into a fixed-size array.
    pub fn arr<const N: usize>(s: &str) -> [u8; N] {
        hex(s).try_into().expect("hex length")
    }
}
