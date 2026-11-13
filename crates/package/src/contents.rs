//! The application tree of an opened package and reads of its files.

use std::io::{self, Read, Seek, SeekFrom};

use pantheon_core::Span;

use crate::{Error, Kind, NodeKind, Package, Tree, ps3, ps4};

// One value per opened package; boxing would only add an indirection.
#[allow(clippy::large_enum_variant)]
enum Inner<R> {
    Ps3(ps3::Package<R>),
    Ps4(ps4::Game<R>),
}

/// A package opened for listing and extraction. PS4 opening recovers the
/// public fake-package keys and walks the nested PFS.
pub struct Contents<R> {
    inner: Inner<R>,
    tree: Tree,
    /// Data-region offset of each PS3 node (unused for PS4).
    offsets: Vec<u64>,
}

impl<R: Read + Seek> Package<R> {
    /// Opens the application tree (see [`Contents`]).
    pub fn into_contents(self) -> Result<Contents<R>, Error> {
        Contents::open(self)
    }
}

impl<R: Read + Seek> Contents<R> {
    pub fn open(pkg: Package<R>) -> Result<Contents<R>, Error> {
        match pkg {
            Package::Ps3(p) => {
                let mut entries = Vec::with_capacity(p.entries().len());
                let mut offsets = Vec::with_capacity(p.entries().len());
                for e in p.entries() {
                    if !e.path.is_ascii() {
                        // Same rule as PS4: no Unicode normalization or case-fold aliases.
                        return Err(ps3::Error::Unsupported("non-ASCII entry path").into());
                    }
                    let kind = match e.kind() {
                        ps3::FileKind::File => NodeKind::File,
                        ps3::FileKind::Directory => NodeKind::Directory,
                    };
                    entries.push((e.path.clone(), kind, e.size));
                    offsets.push(e.offset);
                }
                Ok(Contents {
                    tree: Tree::new(entries)?,
                    inner: Inner::Ps3(p),
                    offsets,
                })
            }
            Package::Ps4(p) => {
                let game = p.into_game()?;
                let entries = game
                    .entries()
                    .iter()
                    .map(|e| {
                        let kind = match e.kind() {
                            ps4::FileKind::File => NodeKind::File,
                            ps4::FileKind::Directory => NodeKind::Directory,
                        };
                        (e.path.clone(), kind, e.size)
                    })
                    .collect();
                Ok(Contents {
                    tree: Tree::new(entries)?,
                    inner: Inner::Ps4(game),
                    offsets: Vec::new(),
                })
            }
        }
    }

    pub fn kind(&self) -> Kind {
        match self.inner {
            Inner::Ps3(_) => Kind::Ps3Pkg,
            Inner::Ps4(_) => Kind::Ps4Pkg,
        }
    }

    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Reads decoded bytes of file node `index` at `offset`; the whole range
    /// must be inside the file.
    pub fn read_at(&mut self, index: u32, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        let node = self.tree.node(index).ok_or(Error::UnknownIndex(index))?;
        let len = u64::try_from(buf.len()).map_err(|_| Error::UnknownIndex(index))?;
        if node.kind != NodeKind::File {
            return Err(Error::NotAFile(node.path.clone()));
        }
        if Span::new(offset, len, node.size).is_err() {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read outside the file",
            )));
        }
        let at = usize::try_from(index).map_err(|_| Error::UnknownIndex(index))?;
        match &mut self.inner {
            Inner::Ps3(p) => {
                let base = self
                    .offsets
                    .get(at)
                    .copied()
                    .ok_or(Error::UnknownIndex(index))?;
                let start = base
                    .checked_add(offset)
                    .ok_or(ps3::Error::Malformed("entry offset overflows"))?;
                Ok(p.read_at(start, buf)?)
            }
            Inner::Ps4(g) => Ok(g.read_at(at, offset, buf)?),
        }
    }

    /// A `Read + Seek` view of file node `index`.
    pub fn open_entry(&mut self, index: u32) -> Result<EntryReader<'_, R>, Error> {
        let node = self.tree.node(index).ok_or(Error::UnknownIndex(index))?;
        if node.kind != NodeKind::File {
            return Err(Error::NotAFile(node.path.clone()));
        }
        let size = node.size;
        Ok(EntryReader {
            contents: self,
            index,
            size,
            position: 0,
        })
    }
}

/// Streaming reader over one file node.
pub struct EntryReader<'a, R> {
    contents: &'a mut Contents<R>,
    index: u32,
    size: u64,
    position: u64,
}

impl<R: Read + Seek> EntryReader<'_, R> {
    pub fn len(&self) -> u64 {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Like [`Read::read`], keeping the typed error.
    pub fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let left = self.size.saturating_sub(self.position);
        let n = buf
            .len()
            .min(usize::try_from(left).unwrap_or(usize::MAX))
            .min(pantheon_core::CHUNK);
        let Some(now) = buf.get_mut(..n).filter(|b| !b.is_empty()) else {
            return Ok(0);
        };
        self.contents.read_at(self.index, self.position, now)?;
        self.position = self
            .position
            .saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        Ok(n)
    }
}

impl<R: Read + Seek> Read for EntryReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_chunk(buf).map_err(|e| match e {
            Error::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other),
        })
    }
}

impl<R: Read + Seek> Seek for EntryReader<'_, R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(n) => self.size.checked_add_signed(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
        };
        self.position = target
            .filter(|n| *n <= self.size)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek outside entry"))?;
        Ok(self.position)
    }
}
