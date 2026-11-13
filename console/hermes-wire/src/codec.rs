//! Bounded little-endian cursors over byte slices. Every read and write checks
//! its bounds and returns an error instead of panicking, so the payload links
//! no panic paths from here.

use crate::Error;

/// Reads fields front to back from one frame body.
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(body: &'a [u8]) -> Reader<'a> {
        Reader { rest: body }
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let (head, rest) = self
            .rest
            .split_at_checked(len)
            .ok_or(Error::Malformed("truncated body"))?;
        self.rest = rest;
        Ok(head)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let bytes = self.bytes(N)?;
        <[u8; N]>::try_from(bytes).map_err(|_| Error::Malformed("truncated body"))
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        let [b] = self.array::<1>()?;
        Ok(b)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    /// `u8` flag: 0 = absent, 1 = present, anything else is malformed.
    pub(crate) fn flag(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Malformed("flag is neither 0 nor 1")),
        }
    }

    /// `u32 length + bytes`; the length is checked against `max` before slicing.
    pub(crate) fn blob(&mut self, field: &'static str, max: u32) -> Result<&'a [u8], Error> {
        let len = self.u32()?;
        if len > max {
            return Err(Error::FieldTooLong { field, len, max });
        }
        let len = usize::try_from(len).map_err(|_| Error::Malformed("length"))?;
        self.bytes(len)
    }

    /// A non-empty UTF-8 string of at most `max` bytes.
    pub(crate) fn str(&mut self, field: &'static str, max: u32) -> Result<&'a str, Error> {
        let bytes = self.blob(field, max)?;
        if bytes.is_empty() {
            return Err(Error::EmptyField(field));
        }
        core::str::from_utf8(bytes).map_err(|_| Error::Malformed("string is not UTF-8"))
    }

    /// Trailing bytes after the last field are malformed.
    pub(crate) fn finish(self) -> Result<(), Error> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(Error::Malformed("trailing bytes"))
        }
    }
}

/// Writes fields front to back into a caller's buffer.
pub(crate) struct Writer<'a> {
    rest: &'a mut [u8],
    written: usize,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(out: &'a mut [u8]) -> Writer<'a> {
        Writer {
            rest: out,
            written: 0,
        }
    }

    pub(crate) fn bytes(&mut self, data: &[u8]) -> Result<(), Error> {
        let rest = core::mem::take(&mut self.rest);
        let (head, tail) = rest
            .split_at_mut_checked(data.len())
            .ok_or(Error::BufferTooSmall)?;
        head.copy_from_slice(data);
        self.rest = tail;
        self.written = self
            .written
            .checked_add(data.len())
            .ok_or(Error::BufferTooSmall)?;
        Ok(())
    }

    pub(crate) fn u8(&mut self, v: u8) -> Result<(), Error> {
        self.bytes(&[v])
    }

    pub(crate) fn u32(&mut self, v: u32) -> Result<(), Error> {
        self.bytes(&v.to_le_bytes())
    }

    pub(crate) fn u64(&mut self, v: u64) -> Result<(), Error> {
        self.bytes(&v.to_le_bytes())
    }

    pub(crate) fn flag(&mut self, v: bool) -> Result<(), Error> {
        self.u8(u8::from(v))
    }

    pub(crate) fn blob(&mut self, data: &[u8]) -> Result<(), Error> {
        let len = u32::try_from(data.len()).map_err(|_| Error::BufferTooSmall)?;
        self.u32(len)?;
        self.bytes(data)
    }

    pub(crate) fn written(&self) -> usize {
        self.written
    }
}
