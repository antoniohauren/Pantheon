//! pantheon-json: a streaming JSON writer (docs/03-foundation.md). No parser.
//!
//! ```
//! # fn main() -> Result<(), pantheon_json::Error> {
//! let mut out = Vec::new();
//! let mut w = pantheon_json::JsonWriter::new(&mut out);
//! w.begin_object()?;
//! w.key("title_id")?;
//! w.string("CUSA00001")?;
//! w.key("size")?;
//! w.u64(1234)?;
//! w.end()?;
//! w.finish()?;
//! assert_eq!(out, br#"{"title_id":"CUSA00001","size":1234}"#);
//! # Ok(())
//! # }
//! ```
//!
//! Output is compact, escaped per RFC 8259, and always valid UTF-8: strings come
//! in as `&str`, and byte strings go through [`JsonWriter::hex`].

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::fmt;
use std::io::{self, Write};

use pantheon_core::{Classify, ErrorKind};

/// Lowercase hex digits for [`JsonWriter::hex`] and `\u00XX` escapes.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Writer errors. Everything except `Io` is a caller bug (nesting misuse).
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// `key` called outside an object, or twice in a row.
    UnexpectedKey,
    /// A value written inside an object without a preceding `key`.
    MissingKey,
    /// `end` called on an object whose last key has no value.
    MissingValue,
    /// `end` called with no open object or array.
    NothingToEnd,
    /// A value written after the top-level value was complete.
    AlreadyComplete,
    /// `finish` called before the top-level value was complete.
    Incomplete,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::UnexpectedKey => f.write_str("JSON key outside an object or after another key"),
            Error::MissingKey => f.write_str("JSON object value without a key"),
            Error::MissingValue => f.write_str("JSON object key without a value"),
            Error::NothingToEnd => f.write_str("no open JSON object or array to end"),
            Error::AlreadyComplete => f.write_str("JSON document is already complete"),
            Error::Incomplete => f.write_str("JSON document is incomplete"),
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
            Error::Io(_) => ErrorKind::Io,
            // A caller bug, not hostile input (same choice as core's BadAlignment).
            _ => ErrorKind::Unsupported,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Object,
    Array,
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    kind: Kind,
    /// No member written yet, so no comma is due.
    first: bool,
    /// Object only: a key was written and its value is due.
    after_key: bool,
}

/// Streaming JSON writer with nesting checks. Writes compact output to `W`.
pub struct JsonWriter<W: Write> {
    out: W,
    stack: Vec<Frame>,
    complete: bool,
}

impl<W: Write> JsonWriter<W> {
    pub fn new(out: W) -> JsonWriter<W> {
        JsonWriter {
            out,
            stack: Vec::new(),
            complete: false,
        }
    }

    pub fn begin_object(&mut self) -> Result<(), Error> {
        self.begin(Kind::Object, b"{")
    }

    pub fn begin_array(&mut self) -> Result<(), Error> {
        self.begin(Kind::Array, b"[")
    }

    /// Closes the innermost object or array.
    pub fn end(&mut self) -> Result<(), Error> {
        let frame = self.stack.last().copied().ok_or(Error::NothingToEnd)?;
        if frame.after_key {
            return Err(Error::MissingValue);
        }
        self.stack.pop();
        self.out.write_all(match frame.kind {
            Kind::Object => b"}",
            Kind::Array => b"]",
        })?;
        self.value_done();
        Ok(())
    }

    /// Writes an object key; the next call must write its value.
    pub fn key(&mut self, key: &str) -> Result<(), Error> {
        let frame = match self.stack.last_mut() {
            Some(frame) if frame.kind == Kind::Object && !frame.after_key => frame,
            _ => return Err(Error::UnexpectedKey),
        };
        let first = std::mem::replace(&mut frame.first, false);
        frame.after_key = true;
        if !first {
            self.out.write_all(b",")?;
        }
        write_escaped(&mut self.out, key)?;
        self.out.write_all(b":")?;
        Ok(())
    }

    pub fn string(&mut self, value: &str) -> Result<(), Error> {
        self.before_value()?;
        write_escaped(&mut self.out, value)?;
        self.value_done();
        Ok(())
    }

    /// Writes a byte string as a lowercase hex JSON string.
    pub fn hex(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.before_value()?;
        self.out.write_all(b"\"")?;
        for &byte in bytes {
            let pair = [hex_digit(byte >> 4), hex_digit(byte)];
            self.out.write_all(&pair)?;
        }
        self.out.write_all(b"\"")?;
        self.value_done();
        Ok(())
    }

    pub fn u64(&mut self, value: u64) -> Result<(), Error> {
        self.raw(format_args!("{value}"))
    }

    pub fn i64(&mut self, value: i64) -> Result<(), Error> {
        self.raw(format_args!("{value}"))
    }

    pub fn bool(&mut self, value: bool) -> Result<(), Error> {
        self.raw(format_args!("{value}"))
    }

    pub fn null(&mut self) -> Result<(), Error> {
        self.raw(format_args!("null"))
    }

    /// Checks that exactly one complete top-level value was written, flushes,
    /// and returns the output.
    pub fn finish(mut self) -> Result<W, Error> {
        if !self.complete {
            return Err(Error::Incomplete);
        }
        self.out.flush()?;
        Ok(self.out)
    }

    fn begin(&mut self, kind: Kind, open: &[u8]) -> Result<(), Error> {
        self.before_value()?;
        self.out.write_all(open)?;
        self.stack.push(Frame {
            kind,
            first: true,
            after_key: false,
        });
        Ok(())
    }

    fn raw(&mut self, text: fmt::Arguments<'_>) -> Result<(), Error> {
        self.before_value()?;
        self.out.write_fmt(text)?;
        self.value_done();
        Ok(())
    }

    /// Validates that a value may start here and writes the separating comma.
    fn before_value(&mut self) -> Result<(), Error> {
        let Some(frame) = self.stack.last_mut() else {
            return if self.complete {
                Err(Error::AlreadyComplete)
            } else {
                Ok(())
            };
        };
        match frame.kind {
            Kind::Object if !frame.after_key => Err(Error::MissingKey),
            Kind::Object => {
                frame.after_key = false;
                Ok(())
            }
            Kind::Array => {
                if !std::mem::replace(&mut frame.first, false) {
                    self.out.write_all(b",")?;
                }
                Ok(())
            }
        }
    }

    fn value_done(&mut self) {
        if self.stack.is_empty() {
            self.complete = true;
        }
    }
}

fn hex_digit(nibble: u8) -> u8 {
    HEX_DIGITS
        .get(usize::from(nibble & 0x0F))
        .copied()
        .unwrap_or(b'0')
}

/// Writes `s` as a quoted JSON string (RFC 8259 §7). Only `"`, `\` and C0
/// controls are escaped; everything else, U+2028/U+2029 included, is literal.
fn write_escaped(out: &mut impl Write, s: &str) -> io::Result<()> {
    out.write_all(b"\"")?;
    let mut rest = s.as_bytes();
    // Escaped bytes are all ASCII, so splitting around them never cuts a UTF-8 sequence.
    while let Some(at) = rest
        .iter()
        .position(|&b| b == b'"' || b == b'\\' || b < 0x20)
    {
        let (plain, tail) = rest.split_at(at);
        out.write_all(plain)?;
        let Some((&byte, after)) = tail.split_first() else {
            break;
        };
        match byte {
            b'"' => out.write_all(b"\\\"")?,
            b'\\' => out.write_all(b"\\\\")?,
            b'\n' => out.write_all(b"\\n")?,
            b'\r' => out.write_all(b"\\r")?,
            b'\t' => out.write_all(b"\\t")?,
            0x08 => out.write_all(b"\\b")?,
            0x0C => out.write_all(b"\\f")?,
            _ => out.write_all(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                hex_digit(byte >> 4),
                hex_digit(byte),
            ])?,
        }
        rest = after;
    }
    out.write_all(rest)?;
    out.write_all(b"\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_string(s: &str) -> String {
        let mut w = JsonWriter::new(Vec::new());
        w.string(s).expect("string");
        String::from_utf8(w.finish().expect("finish")).expect("utf-8")
    }

    #[test]
    fn escapes_quotes_backslash_and_controls() {
        assert_eq!(one_string("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(
            one_string("\n\r\t\u{8}\u{c}\u{0}\u{1f}"),
            r#""\n\r\t\b\f\u0000\u001f""#
        );
        assert_eq!(one_string("\u{7f}"), "\"\u{7f}\"");
    }

    #[test]
    fn non_ascii_and_line_separators_are_literal() {
        assert_eq!(one_string("é😀\u{2028}\u{2029}"), "\"é😀\u{2028}\u{2029}\"");
        assert_eq!(one_string(""), r#""""#);
    }

    #[test]
    fn integers_are_exact() {
        let mut w = JsonWriter::new(Vec::new());
        w.begin_array().unwrap();
        w.u64(u64::MAX).unwrap();
        w.i64(i64::MIN).unwrap();
        w.u64(0).unwrap();
        w.end().unwrap();
        let out = w.finish().unwrap();
        assert_eq!(out, b"[18446744073709551615,-9223372036854775808,0]");
    }

    #[test]
    fn full_document() {
        let mut w = JsonWriter::new(Vec::new());
        w.begin_object().unwrap();
        w.key("schema").unwrap();
        w.string("pantheon.test.doc/1").unwrap();
        w.key("digest").unwrap();
        w.hex(&[0x00, 0xAB, 0xFF]).unwrap();
        w.key("flags").unwrap();
        w.begin_array().unwrap();
        w.bool(true).unwrap();
        w.bool(false).unwrap();
        w.null().unwrap();
        w.begin_object().unwrap();
        w.end().unwrap();
        w.begin_array().unwrap();
        w.end().unwrap();
        w.end().unwrap();
        w.key("delta").unwrap();
        w.i64(-5).unwrap();
        w.end().unwrap();
        let out = String::from_utf8(w.finish().unwrap()).unwrap();
        assert_eq!(
            out,
            r#"{"schema":"pantheon.test.doc/1","digest":"00abff","flags":[true,false,null,{},[]],"delta":-5}"#
        );
    }

    #[test]
    fn nesting_misuse_is_rejected() {
        let mut w = JsonWriter::new(Vec::new());
        assert!(matches!(w.key("k"), Err(Error::UnexpectedKey)));
        assert!(matches!(w.end(), Err(Error::NothingToEnd)));

        w.begin_object().unwrap();
        assert!(matches!(w.u64(1), Err(Error::MissingKey)));
        w.key("a").unwrap();
        assert!(matches!(w.key("b"), Err(Error::UnexpectedKey)));
        assert!(matches!(w.end(), Err(Error::MissingValue)));
        w.begin_array().unwrap();
        assert!(matches!(w.key("c"), Err(Error::UnexpectedKey)));
        w.end().unwrap();
        w.end().unwrap();
        assert!(matches!(w.null(), Err(Error::AlreadyComplete)));
        assert!(matches!(w.begin_array(), Err(Error::AlreadyComplete)));
        assert!(w.finish().is_ok());
    }

    #[test]
    fn finish_requires_a_complete_value() {
        assert!(matches!(
            JsonWriter::new(Vec::new()).finish(),
            Err(Error::Incomplete)
        ));
        let mut w = JsonWriter::new(Vec::new());
        w.begin_array().unwrap();
        assert!(matches!(w.finish(), Err(Error::Incomplete)));
    }

    #[test]
    fn classifies_misuse_and_io() {
        assert_eq!(Error::MissingKey.kind(), ErrorKind::Unsupported);
        let io = Error::Io(io::ErrorKind::BrokenPipe.into());
        assert_eq!(io.kind(), ErrorKind::Io);
    }
}
