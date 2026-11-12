//! Bounded readers over untrusted sources.

use std::io::{self, Read, Seek, SeekFrom};

use crate::{Error, Limit, Span, usize_len};

/// The bounded chunk size (64 KiB) for streaming, cancellation and progress.
pub const CHUNK: usize = 64 * 1024;

/// Wraps a `Read` and returns end-of-file after `len` bytes.
#[derive(Debug)]
pub struct BoundedRead<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> BoundedRead<R> {
    /// Reads at most `span.len` bytes; the caller has positioned `inner` at `span.offset`.
    pub fn new(inner: R, span: Span) -> BoundedRead<R> {
        BoundedRead {
            inner,
            remaining: span.len,
        }
    }

    /// Bytes still allowed.
    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for BoundedRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let allowed = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        let want = buf.len().min(allowed);
        let Some(window) = buf.get_mut(..want) else {
            return Ok(0);
        };
        if window.is_empty() {
            return Ok(0);
        }
        let got = self.inner.read(window)?;
        // got <= want <= remaining, so this cannot underflow; saturate to stay panic-free.
        self.remaining = self
            .remaining
            .saturating_sub(u64::try_from(got).unwrap_or(u64::MAX));
        Ok(got)
    }
}

/// Presents one `Span` of a seekable source as its own `Read + Seek`, positioned at 0.
#[derive(Debug)]
pub struct SpanReader<R> {
    inner: R,
    span: Span,
    pos: u64,
}

impl<R: Read + Seek> SpanReader<R> {
    pub fn new(inner: R, span: Span) -> SpanReader<R> {
        SpanReader {
            inner,
            span,
            pos: 0,
        }
    }

    pub fn len(&self) -> u64 {
        self.span.len
    }

    pub fn is_empty(&self) -> bool {
        self.span.len == 0
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

fn invalid_seek() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "seek outside the span")
}

impl<R: Read + Seek> Read for SpanReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.span.len.saturating_sub(self.pos);
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let Some(window) = buf.get_mut(..want) else {
            return Ok(0);
        };
        if window.is_empty() {
            return Ok(0);
        }
        // Seek every time, so several readers can share one underlying handle.
        let absolute = self
            .span
            .offset
            .checked_add(self.pos)
            .ok_or_else(invalid_seek)?;
        self.inner.seek(SeekFrom::Start(absolute))?;
        let got = self.inner.read(window)?;
        self.pos = self
            .pos
            .saturating_add(u64::try_from(got).unwrap_or(u64::MAX));
        Ok(got)
    }
}

impl<R: Read + Seek> Seek for SpanReader<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let next = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(delta) => self.span.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        // Past-the-end positions are allowed (reads return 0), as for files.
        self.pos = next.ok_or_else(invalid_seek)?;
        Ok(self.pos)
    }
}

/// Reads exactly `len` bytes after checking `len` against `limit`.
/// The only sanctioned way to buffer a whole metadata record; a truncated source
/// yields an `UnexpectedEof` I/O error without allocating `len` up front.
pub fn read_exact_capped<R: Read>(reader: R, len: u64, limit: Limit) -> Result<Vec<u8>, Error> {
    let wanted = usize_len(len, limit)?;
    let mut out = Vec::with_capacity(wanted.min(CHUNK));
    reader.take(len).read_to_end(&mut out)?;
    if out.len() != wanted {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Classify, ErrorKind};
    use std::io::Cursor;

    #[test]
    fn bounded_read_stops_at_len() {
        let mut reader = BoundedRead::new(&b"abcdef"[..], Span { offset: 0, len: 4 });
        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abcd");
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn span_reader_reads_and_seeks_inside() {
        let source = Cursor::new(b"0123456789".to_vec());
        let span = Span::new(2, 5, 10).unwrap();
        let mut reader = SpanReader::new(source, span);
        let mut out = String::new();
        reader.read_to_string(&mut out).unwrap();
        assert_eq!(out, "23456");
        assert_eq!(reader.seek(SeekFrom::End(-2)).unwrap(), 3);
        let mut two = [0u8; 4];
        assert_eq!(reader.read(&mut two).unwrap(), 2);
        assert_eq!(&two[..2], b"56");
        assert!(reader.seek(SeekFrom::Current(-10)).is_err());
        assert_eq!(reader.seek(SeekFrom::Start(100)).unwrap(), 100);
        assert_eq!(reader.read(&mut two).unwrap(), 0);
    }

    #[test]
    fn capped_read() {
        let limit = Limit::new("record", 4);
        assert_eq!(read_exact_capped(&b"abcd"[..], 4, limit).unwrap(), b"abcd");
        let over = read_exact_capped(&b"abcde"[..], 5, limit).unwrap_err();
        assert_eq!(over.kind(), ErrorKind::Limit);
        let short = read_exact_capped(&b"ab"[..], 4, limit).unwrap_err();
        assert_eq!(short.kind(), ErrorKind::Malformed);
    }
}
