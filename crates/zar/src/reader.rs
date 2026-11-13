//! Random-access ZArchive reading, for verification and tests.
//!
//! Validation follows the pinned upstream reader (footer magic, version, total
//! size, section bounds, table sizes, an unnamed root directory), plus Pantheon's
//! resource limits and checked tree walks. The SHA-256 is checked only by
//! [`Reader::verify_digest`], since it needs a pass over the whole archive.

use std::io::{Read, Seek, SeekFrom};

use pantheon_core::{CHUNK, Limit, Limits, u16_be, u32_be, u64_be, usize_len};
use pantheon_crypto::hash::Sha256;
use pantheon_crypto::zstd;

use crate::Error;
use crate::format::{
    BLOCK, BLOCK_U64, DIGEST_AT, FILE_FLAG, FOOTER_LEN, FOOTER_LEN_U64, Footer, HIGH_MASK,
    HIGH_SHIFT, LONG_NAME_FLAG, LOW_BITS, MAGIC, MAX_NAMES, MAX_TABLE, NAME_MASK, NO_NAME,
    NODE_LEN, NODE_LEN_U64, RECORD_BLOCKS_U64, RECORD_LEN_U64, SHORT_NAME_MASK, Section, VERSION_1,
};

/// Bits the high byte of a two-byte name length is shifted by.
const LONG_NAME_SHIFT: u32 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Directory,
    File,
}

/// One archive entry from [`Reader::entries`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// `/`-separated, relative to the root.
    pub path: String,
    pub kind: EntryKind,
    /// File size; zero for directories.
    pub size: u64,
    /// Node handle for [`Reader::read_at`].
    pub node: u32,
}

#[derive(Clone, Copy, Debug)]
struct Node {
    file: bool,
    name: u32,
    /// Directory: first child, child count. File: offset, size.
    a: u64,
    b: u64,
}

/// An opened archive.
pub struct Reader<R> {
    src: R,
    data: Section,
    records: Vec<u8>,
    blocks: u64,
    names: Vec<u8>,
    nodes: Vec<Node>,
    /// The most recently decoded block.
    cached: Option<(u64, Vec<u8>)>,
}

impl<R: Read + Seek> Reader<R> {
    /// Reads and checks the footer and loads the tables (bounded by `limits`).
    pub fn open(mut src: R, limits: &Limits) -> Result<Reader<R>, Error> {
        let len = src.seek(SeekFrom::End(0))?;
        if len <= FOOTER_LEN_U64 {
            return Err(Error::Malformed("shorter than its footer"));
        }
        let mut raw = [0u8; FOOTER_LEN];
        src.seek(SeekFrom::Start(len.saturating_sub(FOOTER_LEN_U64)))?;
        src.read_exact(&mut raw)?;
        let (footer, version, magic) = Footer::decode(&raw)?;
        if magic != MAGIC {
            return Err(Error::Malformed("bad footer magic"));
        }
        if version != VERSION_1 {
            return Err(Error::Malformed("unknown version"));
        }
        if footer.total != len {
            return Err(Error::Malformed("footer size differs from the file size"));
        }
        if !footer.sections_within(len) {
            return Err(Error::Malformed("section outside the file"));
        }
        if footer.records.size > MAX_TABLE
            || footer.names.size > MAX_NAMES
            || footer.tree.size > MAX_TABLE
        {
            return Err(Error::Malformed("table larger than the format allows"));
        }
        let records = read_table(
            &mut src,
            footer.records,
            Limit::new("ZArchive offset records", limits.max_dir_data),
        )?;
        if records.is_empty() || footer.records.size.checked_rem(RECORD_LEN_U64) != Some(0) {
            return Err(Error::Malformed("empty or partial offset records"));
        }
        let names = read_table(
            &mut src,
            footer.names,
            Limit::new("ZArchive name table", limits.max_path_bytes),
        )?;
        let tree = read_table(
            &mut src,
            footer.tree,
            Limit::new("ZArchive file tree", limits.max_dir_data),
        )?;
        if tree.is_empty() || footer.tree.size.checked_rem(NODE_LEN_U64) != Some(0) {
            return Err(Error::Malformed("empty or partial file tree"));
        }
        let count = footer.tree.size.checked_div(NODE_LEN_U64).unwrap_or(0);
        if count > u64::from(limits.max_entries) {
            return Err(Error::Limit(pantheon_core::LimitError {
                what: "ZArchive entries",
                value: count,
                max: u64::from(limits.max_entries),
            }));
        }
        let nodes = tree
            .as_chunks::<NODE_LEN>()
            .0
            .iter()
            .map(|n| decode_node(n))
            .collect::<Result<Vec<Node>, Error>>()?;
        let reader = Reader {
            src,
            data: footer.data,
            blocks: footer
                .records
                .size
                .checked_div(RECORD_LEN_U64)
                .unwrap_or(0)
                .saturating_mul(RECORD_BLOCKS_U64),
            records,
            names,
            nodes,
            cached: None,
        };
        match reader.nodes.first() {
            Some(root) if !root.file && reader.name(root.name)?.is_empty() => Ok(reader),
            _ => Err(Error::Malformed(
                "the first node is not an unnamed root folder",
            )),
        }
    }

    /// Every entry below the root, breadth-first (parents before children).
    pub fn entries(&self) -> Result<Vec<Entry>, Error> {
        let mut out: Vec<Entry> = Vec::new();
        let mut seen = vec![false; self.nodes.len()];
        // (node, index of its entry in `out`, or None for the root)
        let mut queue = vec![(0u32, None::<usize>)];
        let mut next = 0usize;
        while let Some(&(dir, at)) = queue.get(next) {
            next = next.saturating_add(1);
            let node = self.node(dir)?;
            if node.file {
                continue;
            }
            let prefix = at.and_then(|i| out.get(i)).map(|e| e.path.clone());
            for child in node.a..node.a.saturating_add(node.b) {
                let index = u32::try_from(child).map_err(|_| Error::Malformed("child index"))?;
                let slot = seen
                    .get_mut(usize::try_from(index).unwrap_or(usize::MAX))
                    .ok_or(Error::Malformed("child index outside the tree"))?;
                if index == 0 || *slot {
                    return Err(Error::Malformed("a node is listed twice"));
                }
                *slot = true;
                let n = self.node(index)?;
                let name = self.name(n.name)?;
                if name.is_empty() || name.contains(['/', '\\']) {
                    return Err(Error::Malformed("bad entry name"));
                }
                let path = match &prefix {
                    Some(p) => format!("{p}/{name}"),
                    None => name.to_owned(),
                };
                let (kind, size) = if n.file {
                    (EntryKind::File, n.b)
                } else {
                    queue.push((index, Some(out.len())));
                    (EntryKind::Directory, 0)
                };
                out.push(Entry {
                    path,
                    kind,
                    size,
                    node: index,
                });
            }
        }
        Ok(out)
    }

    /// The node at `path`, comparing names ignoring ASCII case as upstream does.
    pub fn lookup(&self, path: &str) -> Option<u32> {
        let mut at = 0u32;
        for part in path.split(['/', '\\']).filter(|p| !p.is_empty()) {
            let dir = self.node(at).ok().filter(|n| !n.file)?;
            at = (dir.a..dir.a.saturating_add(dir.b))
                .filter_map(|c| u32::try_from(c).ok())
                .find(|&c| {
                    self.node(c)
                        .and_then(|n| self.name(n.name))
                        .is_ok_and(|name| name.eq_ignore_ascii_case(part))
                })?;
        }
        Some(at)
    }

    /// Size of file `node`, `None` for directories and unknown nodes.
    pub fn file_size(&self, node: u32) -> Option<u64> {
        self.node(node).ok().filter(|n| n.file).map(|n| n.b)
    }

    /// Reads file `node` from `offset` into `buf`; returns the bytes read,
    /// fewer at the end of the file and zero past it.
    pub fn read_at(&mut self, node: u32, offset: u64, buf: &mut [u8]) -> Result<usize, Error> {
        let n = self.node(node)?;
        if !n.file {
            return Err(Error::Malformed("read of a folder"));
        }
        let left = n.b.saturating_sub(offset);
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let mut stream =
            n.a.checked_add(offset)
                .ok_or(Error::Malformed("file offset overflows"))?;
        let mut done = 0usize;
        while done < want {
            let index = stream.checked_div(BLOCK_U64).unwrap_or(0);
            let within = usize::try_from(stream.checked_rem(BLOCK_U64).unwrap_or(0)).unwrap_or(0);
            let block = self.block(index)?;
            let step = BLOCK.saturating_sub(within).min(want.saturating_sub(done));
            let from = block
                .get(within..within.saturating_add(step))
                .ok_or(Error::Malformed("short block"))?;
            buf.get_mut(done..done.saturating_add(step))
                .ok_or(Error::Malformed("read buffer"))?
                .copy_from_slice(from);
            done = done.saturating_add(step);
            stream = stream.saturating_add(u64::try_from(step).unwrap_or(u64::MAX));
        }
        Ok(want)
    }

    /// Recomputes the footer SHA-256 over the whole archive.
    pub fn verify_digest(&mut self) -> Result<(), Error> {
        let len = self.src.seek(SeekFrom::End(0))?;
        let body = len.saturating_sub(FOOTER_LEN_U64);
        self.src.seek(SeekFrom::Start(0))?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let mut left = body;
        while left > 0 {
            let n = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let chunk = buf.get_mut(..n).ok_or(Error::Malformed("chunk"))?;
            self.src.read_exact(chunk)?;
            hash.update(chunk);
            left = left.saturating_sub(u64::try_from(n).unwrap_or(u64::MAX));
        }
        let mut footer = [0u8; FOOTER_LEN];
        self.src.read_exact(&mut footer)?;
        let stored = Footer::decode(&footer)?.0.digest;
        footer
            .get_mut(DIGEST_AT..DIGEST_AT.saturating_add(stored.len()))
            .ok_or(Error::Malformed("footer"))?
            .fill(0);
        hash.update(&footer);
        if hash.finalize() == stored {
            Ok(())
        } else {
            Err(Error::Malformed("SHA-256 does not match"))
        }
    }

    fn node(&self, index: u32) -> Result<Node, Error> {
        self.nodes
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .copied()
            .ok_or(Error::Malformed("node index outside the tree"))
    }

    /// The name at `offset` in the name table; the root's marker is empty.
    fn name(&self, offset: u32) -> Result<&str, Error> {
        if offset == NO_NAME {
            return Ok("");
        }
        let at = u64::from(offset);
        let first = pantheon_core::u8_at(&self.names, at)?;
        let (len, start) = if first & LONG_NAME_FLAG != 0 {
            let second = pantheon_core::u8_at(&self.names, at.saturating_add(1))?;
            let len = u16::from(first & SHORT_NAME_MASK) | (u16::from(second) << LONG_NAME_SHIFT);
            (len, at.saturating_add(2))
        } else {
            (u16::from(first), at.saturating_add(1))
        };
        let start = usize::try_from(start).map_err(|_| Error::Malformed("name offset"))?;
        let bytes = self
            .names
            .get(start..start.saturating_add(usize::from(len)))
            .ok_or(Error::Malformed("name outside the name table"))?;
        std::str::from_utf8(bytes).map_err(|_| Error::Malformed("name is not UTF-8"))
    }

    /// Decodes block `index`, reusing the last one.
    fn block(&mut self, index: u64) -> Result<&[u8], Error> {
        if self.cached.as_ref().is_none_or(|(i, _)| *i != index) {
            let data = self.load(index)?;
            self.cached = Some((index, data));
        }
        Ok(self.cached.as_ref().map_or(&[][..], |(_, d)| d.as_slice()))
    }

    fn load(&mut self, index: u64) -> Result<Vec<u8>, Error> {
        if index >= self.blocks {
            return Err(Error::Malformed("block outside the offset records"));
        }
        let record = index
            .checked_div(RECORD_BLOCKS_U64)
            .unwrap_or(0)
            .saturating_mul(RECORD_LEN_U64);
        let slot = index.checked_rem(RECORD_BLOCKS_U64).unwrap_or(0);
        let mut offset = u64_be(&self.records, record)?;
        let field = |i: u64| {
            u16_be(
                &self.records,
                record.saturating_add(8).saturating_add(i.saturating_mul(2)),
            )
        };
        for i in 0..slot {
            offset = offset
                .saturating_add(u64::from(field(i)?))
                .saturating_add(1);
        }
        let stored = u64::from(field(slot)?).saturating_add(1);
        if offset.saturating_add(stored) > self.data.size {
            return Err(Error::Malformed("block outside the compressed data"));
        }
        self.src
            .seek(SeekFrom::Start(self.data.offset.saturating_add(offset)))?;
        let mut raw = vec![0u8; usize::try_from(stored).unwrap_or(BLOCK)];
        self.src.read_exact(&mut raw)?;
        if stored == BLOCK_U64 {
            return Ok(raw);
        }
        let plain = zstd::decompress(&raw, BLOCK).map_err(Error::Compression)?;
        if plain.len() != BLOCK {
            return Err(Error::Malformed("block does not decompress to 64 KiB"));
        }
        Ok(plain)
    }
}

fn read_table<R: Read + Seek>(src: &mut R, s: Section, limit: Limit) -> Result<Vec<u8>, Error> {
    let len = usize_len(s.size, limit)?;
    let mut out = vec![0u8; len];
    src.seek(SeekFrom::Start(s.offset))?;
    src.read_exact(&mut out)?;
    Ok(out)
}

fn decode_node(b: &[u8]) -> Result<Node, Error> {
    let w0 = u32_be(b, 0)?;
    let (w1, w2, w3) = (u32_be(b, 4)?, u32_be(b, 8)?, u32_be(b, 12)?);
    let file = w0 & FILE_FLAG != 0;
    let (a, b) = if file {
        (
            u64::from(w1) | (u64::from(w3 & HIGH_MASK) << LOW_BITS),
            u64::from(w2) | (u64::from(w3 >> HIGH_SHIFT) << LOW_BITS),
        )
    } else {
        (u64::from(w1), u64::from(w2))
    };
    Ok(Node {
        file,
        name: w0 & NAME_MASK,
        a,
        b,
    })
}
