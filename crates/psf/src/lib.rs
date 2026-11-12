//! pantheon-psf: PARAM.SFO 1.0/1.1 parsing and SFO 1.1 serialization.
//!
//! Parsing borrows from the input and keeps every entry's raw used bytes,
//! capacity and padding, so nothing is lost or decoded lossily. Text stays as
//! bytes; frontends decide how to show non-UTF-8 values.
//! Behavioural reference: pkg-tools `crates/psf`, `docs/compatibility.md` §3.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

mod error;
mod parse;
mod serialize;

pub use error::Error;
pub use parse::parse;
pub use serialize::serialize;

/// `\0PSF`.
pub const MAGIC: [u8; 4] = *b"\0PSF";
pub const VERSION_1_0: u32 = 0x100;
pub const VERSION_1_1: u32 = 0x101;
/// Raw bytes (`utf8-S` in Sony's tools).
pub const FORMAT_BINARY: u16 = 0x0004;
/// NUL-terminated text.
pub const FORMAT_TEXT: u16 = 0x0204;
/// Little-endian `u32`.
pub const FORMAT_INTEGER: u16 = 0x0404;
/// Longest key accepted, excluding its NUL. A format bound, not a resource policy.
pub const MAX_KEY_BYTES: usize = 1024;

/// One entry's value, interpreted by its format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    Binary(&'a [u8]),
    /// Bytes before the first NUL. Not decoded: use `std::str::from_utf8`.
    Text(&'a [u8]),
    Integer(u32),
    Unknown {
        format: u16,
        bytes: &'a [u8],
    },
}

/// One index record with its key and value bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub key: &'a [u8],
    pub format: u16,
    pub value: Value<'a>,
    /// Exactly the used bytes, text terminators included. Authoritative for
    /// serialization: `value` and `format` must agree with them.
    pub raw: &'a [u8],
    /// Declared capacity: `raw.len() + padding.len()`.
    pub max_len: u32,
    pub padding: &'a [u8],
}

impl Entry<'_> {
    /// The text value as UTF-8, or `None` for non-text or invalid UTF-8.
    pub fn text(&self) -> Option<&str> {
        match self.value {
            Value::Text(bytes) => std::str::from_utf8(bytes).ok(),
            _ => None,
        }
    }
}

/// A parsed PARAM.SFO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sfo<'a> {
    pub version: u32,
    pub entries: Vec<Entry<'a>>,
}

impl Sfo<'_> {
    pub fn get(&self, key: &[u8]) -> Option<&Entry<'_>> {
        self.entries.iter().find(|entry| entry.key == key)
    }

    /// The UTF-8 text value of `key`, if present and valid.
    pub fn text(&self, key: &[u8]) -> Option<&str> {
        self.get(key).and_then(Entry::text)
    }
}

/// Interprets `raw` for `format`; shared by parse and serialize so both agree.
fn value_from_raw(format: u16, raw: &[u8]) -> Result<Value<'_>, Error> {
    Ok(match format {
        FORMAT_BINARY => Value::Binary(raw),
        FORMAT_TEXT => {
            let end = raw
                .iter()
                .position(|b| *b == 0)
                .ok_or(Error::Malformed("unterminated text"))?;
            Value::Text(raw.get(..end).unwrap_or_default())
        }
        FORMAT_INTEGER => {
            let bytes: [u8; 4] = raw
                .try_into()
                .map_err(|_| Error::Malformed("integer is not 4 bytes"))?;
            Value::Integer(u32::from_le_bytes(bytes))
        }
        _ => Value::Unknown { format, bytes: raw },
    })
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests;
