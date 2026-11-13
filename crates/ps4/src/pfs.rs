//! PFS and PFSC images, read side (little-endian on-disk fields).
//!
//! Layers stack by ownership: package region → outer PFS (XTS) → `pfs_image.dat`
//! → PFSC → inner PFS. Every layer has one bounded block cache; nothing scales
//! with payload size. Behavioural reference: pkg-tools `docs/extraction.md`.

use std::io::{Read, Seek, SeekFrom};

use pantheon_core::{Limit, Limits, Span, u16_le, u32_le, u64_le};
use pantheon_crypto::aes::Xts128;
use pantheon_crypto::hash::hmac_sha256;
use pantheon_crypto::zlib::Inflater;

use crate::Error;

/// PFS filesystem block size; the only one supported.
pub(crate) const BLOCK: u64 = 0x1_0000;
const BLOCK_BYTES: usize = 0x1_0000;
const HEADER_BYTES: usize = 0x400;

const PFS_VERSION: u64 = 1;
/// The format date stamp `20130315`, used as the magic.
const PFS_MAGIC: u64 = 20_130_315;
const MODE_SIGNED: u16 = 0x1;
const MODE_ENCRYPTED: u16 = 0x4;
/// Signed, encrypted and case-insensitive; `0x2` (64-bit inodes) is unsupported.
const KNOWN_MODES: u16 = 0xd;
const AT_VERSION: u64 = 0x00;
const AT_MAGIC: u64 = 0x08;
const AT_MODE: u64 = 0x1c;
const AT_BLOCK_SIZE: u64 = 0x20;
const AT_INODE_COUNT: u64 = 0x30;
const AT_INODE_TABLE_BLOCKS: u64 = 0x40;
const SEED: std::ops::Range<usize> = 0x370..0x380;
const HEADER_HMAC: std::ops::Range<usize> = 0x380..0x3a0;
/// The header HMAC covers bytes `0..0x380`, zero-extended to this length.
const SIGNED_HEADER_LEN: usize = 0x5a0;
const KEY_INDEX_ENCRYPTION: u32 = 1;
const KEY_INDEX_SIGNING: u32 = 2;
const XTS_SECTOR: usize = 0x1000;
const SECTORS_PER_BLOCK: u64 = BLOCK / 0x1000;

const INODE_SIGNED_LEN: u64 = 0x2c8;
const INODE_PLAIN_LEN: u64 = 0xa8;
const INODE_SIGNED_BYTES: usize = 0x2c8;
const AT_INODE_MODE: u64 = 0;
const AT_INODE_FLAGS: u64 = 4;
const AT_INODE_SIZE: u64 = 8;
const AT_INODE_COMPRESSED: u64 = 16;
const AT_INODE_BLOCKS: u64 = 96;
const AT_INODE_REFS: u64 = 100;
/// A signed block reference: a 32-byte signature, then the block number.
const SIGNED_REF_LEN: u64 = 36;
const SIGNED_REF_NUMBER: u64 = 32;
const PLAIN_REF_LEN: u64 = 4;
const REF_SLOTS: usize = 17;
const DIRECT_REFS: u64 = 12;
const INDIRECT_SLOT: usize = 12;
const DOUBLE_INDIRECT_SLOT: usize = 13;
/// Signed references per indirect block.
const REFS_PER_BLOCK: u64 = BLOCK / SIGNED_REF_LEN;
/// `refs[1]` of a contiguous (unfragmented) file.
const NO_REF: u32 = u32::MAX;
const TYPE_MASK: u16 = 0xf000;
const TYPE_DIR: u16 = 0x4000;
const TYPE_FILE: u16 = 0x8000;
/// Inode flag bit for a per-file compressed file.
pub(crate) const INODE_COMPRESSED: u32 = 0x1;

const DIRENT_HEADER: usize = 16;
const DIRENT_MIN: usize = 24;
const DIRENT_ALIGN: usize = 8;
const MAX_NAME: usize = 255;
const DIRENT_FILE: u32 = 2;
const DIRENT_DIR: u32 = 3;
const DIRENT_DOT: u32 = 4;
const DIRENT_DOTDOT: u32 = 5;
const UROOT: &str = "uroot";

const PFSC_MAGIC: &[u8; 4] = b"PFSC";
const PFSC_HEADER: usize = 48;
const PFSC_MIN_BLOCK: u64 = 0x1000;
const PFSC_OFFSET_LEN: u64 = 8;
/// Offsets validated per batch when a PFSC image opens.
const PFSC_BATCH: u64 = 1024;
const PFSC_BATCH_BYTES: usize = 8192;

fn overflow() -> Error {
    Error::Malformed("offset arithmetic overflows")
}

fn to_usize(v: u64) -> Result<usize, Error> {
    usize::try_from(v).map_err(|_| overflow())
}

fn to_u64(v: usize) -> Result<u64, Error> {
    u64::try_from(v).map_err(|_| overflow())
}

/// Positional reads from a bounded image.
pub(crate) trait ReadAt {
    fn len(&self) -> u64;
    /// Fills `buf` from `offset`; the whole range must be inside the image.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error>;
}

fn check_range(offset: u64, len: usize, total: u64) -> Result<(), Error> {
    Span::new(offset, to_u64(len)?, total)
        .map(|_| ())
        .map_err(|_| Error::Malformed("read outside image"))
}

/// A byte range of the package reader.
pub(crate) struct Region<R> {
    pub(crate) reader: R,
    pub(crate) span: Span,
}

impl<R: Read + Seek> ReadAt for Region<R> {
    fn len(&self) -> u64 {
        self.span.len
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        check_range(offset, buf.len(), self.span.len)?;
        let at = self.span.offset.checked_add(offset).ok_or_else(overflow)?;
        self.reader.seek(SeekFrom::Start(at))?;
        self.reader.read_exact(buf)?;
        Ok(())
    }
}

/// HMAC-SHA256(key, LE32(index) || seed): the PFS key derivation.
pub(crate) fn pfs_key(ekpfs: &[u8; 32], seed: &[u8], index: u32) -> [u8; 32] {
    let mut input = [0u8; 20];
    let (head, tail) = input.split_at_mut(4);
    head.copy_from_slice(&index.to_le_bytes());
    if tail.len() == seed.len() {
        tail.copy_from_slice(seed);
    }
    hmac_sha256(ekpfs, &input)
}

/// One decoded inode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Inode {
    pub(crate) mode: u16,
    pub(crate) flags: u32,
    pub(crate) size: u64,
    pub(crate) compressed_size: u64,
    blocks: u32,
    refs: [u32; REF_SLOTS],
}

impl Inode {
    pub(crate) fn is_dir(&self) -> bool {
        self.mode & TYPE_MASK == TYPE_DIR
    }

    fn contiguous(&self) -> bool {
        self.blocks <= 1 || self.refs.get(1) == Some(&NO_REF)
    }

    fn slot(&self, i: usize) -> Result<u32, Error> {
        self.refs
            .get(i)
            .copied()
            .ok_or(Error::Malformed("PFS block reference"))
    }
}

/// A directory entry: name, inode number, is-directory.
pub(crate) type Child = (String, u32, bool);

/// An opened PFS image over `source`.
pub(crate) struct Pfs<T> {
    source: T,
    signed: bool,
    inode_count: u64,
    inode_blocks: u64,
    block_count: u64,
    cipher: Option<Xts128>,
    cache: Vec<u8>,
    cached: Option<u64>,
    limits: Limits,
}

impl<T: ReadAt> Pfs<T> {
    /// Opens the image; `ekpfs` unlocks a signed/encrypted image.
    pub(crate) fn open(
        mut source: T,
        ekpfs: Option<&[u8; 32]>,
        limits: &Limits,
    ) -> Result<Pfs<T>, Error> {
        let mut h = [0u8; HEADER_BYTES];
        source.read_at(0, &mut h)?;
        if u64_le(&h, AT_VERSION)? != PFS_VERSION {
            return Err(Error::Unsupported("PFS version"));
        }
        if u64_le(&h, AT_MAGIC)? != PFS_MAGIC {
            return Err(Error::Malformed("PFS magic"));
        }
        let mode = u16_le(&h, AT_MODE)?;
        if mode & !KNOWN_MODES != 0 {
            return Err(Error::Unsupported(
                "PFS mode (64-bit inodes or unknown flags)",
            ));
        }
        if u64::from(u32_le(&h, AT_BLOCK_SIZE)?) != BLOCK {
            return Err(Error::Unsupported("PFS block size other than 64 KiB"));
        }
        if !source.len().is_multiple_of(BLOCK) {
            return Err(Error::Malformed("PFS image is not whole blocks"));
        }
        let seed = h.get(SEED).ok_or_else(overflow)?;
        let signed = mode & MODE_SIGNED != 0;
        if signed && let Some(key) = ekpfs {
            let signing = pfs_key(key, seed, KEY_INDEX_SIGNING);
            let mut covered = [0u8; SIGNED_HEADER_LEN];
            let head = h.get(..HEADER_HMAC.start).ok_or_else(overflow)?;
            covered
                .get_mut(..HEADER_HMAC.start)
                .ok_or_else(overflow)?
                .copy_from_slice(head);
            if h.get(HEADER_HMAC) != Some(&hmac_sha256(&signing, &covered)[..]) {
                return Err(Error::Malformed("PFS header HMAC mismatch"));
            }
        }
        let inode_count = u64_le(&h, AT_INODE_COUNT)?;
        let inode_blocks = u64_le(&h, AT_INODE_TABLE_BLOCKS)?;
        let block_count = source.len() / BLOCK;
        let per_block = if signed {
            BLOCK / INODE_SIGNED_LEN
        } else {
            BLOCK / INODE_PLAIN_LEN
        };
        if inode_count == 0
            || inode_blocks == 0
            || inode_blocks >= block_count
            || inode_count.div_ceil(per_block) > inode_blocks
        {
            return Err(Error::Malformed("PFS inode table range"));
        }
        Limit::new("PFS inodes", u64::from(limits.max_entries)).check(inode_count)?;
        let cipher = if mode & MODE_ENCRYPTED != 0 {
            let key = ekpfs.ok_or(Error::Unsupported("encrypted inner PFS"))?;
            let k = pfs_key(key, seed, KEY_INDEX_ENCRYPTION);
            let (tweak, data) = k.split_at(16);
            let tweak: &[u8; 16] = tweak.try_into().map_err(|_| overflow())?;
            let data: &[u8; 16] = data.try_into().map_err(|_| overflow())?;
            Some(Xts128::new(data, tweak))
        } else {
            None
        };
        Ok(Pfs {
            source,
            signed,
            inode_count,
            inode_blocks,
            block_count,
            cipher,
            cache: vec![0; BLOCK_BYTES],
            cached: None,
            limits: *limits,
        })
    }

    pub(crate) fn source_mut(&mut self) -> &mut T {
        &mut self.source
    }

    fn width(&self) -> u64 {
        if self.signed {
            INODE_SIGNED_LEN
        } else {
            INODE_PLAIN_LEN
        }
    }

    pub(crate) fn inode(&mut self, number: u32) -> Result<Inode, Error> {
        let number = u64::from(number);
        if number >= self.inode_count {
            return Err(Error::Malformed("PFS inode number"));
        }
        let width = self.width();
        let per_block = BLOCK.checked_div(width).ok_or_else(overflow)?;
        let offset = number
            .checked_div(per_block)
            .and_then(|b| b.checked_add(1))
            .and_then(|b| b.checked_mul(BLOCK))
            .and_then(|b| b.checked_add(number.checked_rem(per_block)?.checked_mul(width)?))
            .ok_or_else(overflow)?;
        let mut b = [0u8; INODE_SIGNED_BYTES];
        let raw = b.get_mut(..to_usize(width)?).ok_or_else(overflow)?;
        self.read_at(offset, raw)?;
        let (stride, at) = if self.signed {
            (SIGNED_REF_LEN, SIGNED_REF_NUMBER)
        } else {
            (PLAIN_REF_LEN, 0)
        };
        let mut refs = [0u32; REF_SLOTS];
        let mut pos = AT_INODE_REFS.checked_add(at).ok_or_else(overflow)?;
        for r in &mut refs {
            *r = u32_le(raw, pos)?;
            pos = pos.checked_add(stride).ok_or_else(overflow)?;
        }
        let inode = Inode {
            mode: u16_le(raw, AT_INODE_MODE)?,
            flags: u32_le(raw, AT_INODE_FLAGS)?,
            size: u64_le(raw, AT_INODE_SIZE)?,
            compressed_size: u64_le(raw, AT_INODE_COMPRESSED)?,
            blocks: u32_le(raw, AT_INODE_BLOCKS)?,
            refs,
        };
        if inode.size > u64::from(inode.blocks).saturating_mul(BLOCK) {
            return Err(Error::Malformed("PFS file exceeds its allocated blocks"));
        }
        if !matches!(inode.mode & TYPE_MASK, TYPE_DIR | TYPE_FILE) {
            return Err(Error::Unsupported("PFS symlink or special inode"));
        }
        if !inode.contiguous() {
            if !self.signed {
                return Err(Error::Unsupported("fragmented unsigned PFS file"));
            }
            let mapped = DIRECT_REFS
                .saturating_add(REFS_PER_BLOCK)
                .saturating_add(REFS_PER_BLOCK.saturating_mul(REFS_PER_BLOCK));
            if u64::from(inode.blocks) > mapped {
                return Err(Error::Unsupported("PFS triple indirection"));
            }
        } else if inode.blocks > 0 {
            let first = inode.slot(0)?;
            self.block(first)?;
            let last = first
                .checked_add(inode.blocks.saturating_sub(1))
                .ok_or(Error::Malformed("PFS extent overflows"))?;
            self.block(last)?;
        }
        Ok(inode)
    }

    /// The byte offset of data block `block`; inode-table and header blocks are refused.
    fn block(&self, block: u32) -> Result<u64, Error> {
        let block = u64::from(block);
        if block <= self.inode_blocks || block >= self.block_count {
            return Err(Error::Malformed("PFS data block out of range"));
        }
        block.checked_mul(BLOCK).ok_or_else(overflow)
    }

    fn indirect(&mut self, block: u32, index: u64) -> Result<u32, Error> {
        if index >= REFS_PER_BLOCK {
            return Err(Error::Malformed("PFS indirect index"));
        }
        let at = index
            .checked_mul(SIGNED_REF_LEN)
            .and_then(|v| v.checked_add(SIGNED_REF_NUMBER))
            .and_then(|v| v.checked_add(self.block(block).ok()?))
            .ok_or(Error::Malformed("PFS indirect block"))?;
        let mut b = [0u8; 4];
        self.read_at(at, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// The byte offset of file block `index`.
    fn mapped(&mut self, inode: &Inode, index: u64) -> Result<u64, Error> {
        if index >= u64::from(inode.blocks) {
            return Err(Error::Malformed("PFS file block index"));
        }
        let block = if inode.contiguous() {
            u32::try_from(index)
                .ok()
                .and_then(|i| inode.slot(0).ok()?.checked_add(i))
                .ok_or(Error::Malformed("PFS extent overflows"))?
        } else if index < DIRECT_REFS {
            inode.slot(to_usize(index)?)?
        } else if let Some(i) = index
            .checked_sub(DIRECT_REFS)
            .filter(|i| *i < REFS_PER_BLOCK)
        {
            self.indirect(inode.slot(INDIRECT_SLOT)?, i)?
        } else {
            let i = index
                .checked_sub(DIRECT_REFS.saturating_add(REFS_PER_BLOCK))
                .ok_or_else(overflow)?;
            let table = self.indirect(inode.slot(DOUBLE_INDIRECT_SLOT)?, i / REFS_PER_BLOCK)?;
            self.indirect(table, i % REFS_PER_BLOCK)?
        };
        self.block(block)
    }

    /// Reads file bytes; the range must lie within the file.
    pub(crate) fn read_file(
        &mut self,
        inode: &Inode,
        mut offset: u64,
        mut buf: &mut [u8],
    ) -> Result<(), Error> {
        check_range(offset, buf.len(), inode.size)?;
        while !buf.is_empty() {
            let within = offset % BLOCK;
            let n = buf.len().min(to_usize(BLOCK.saturating_sub(within))?);
            let pos = self
                .mapped(inode, offset / BLOCK)?
                .checked_add(within)
                .ok_or_else(overflow)?;
            let (now, rest) = buf.split_at_mut(n);
            self.read_at(pos, now)?;
            offset = offset.checked_add(to_u64(n)?).ok_or_else(overflow)?;
            buf = rest;
        }
        Ok(())
    }

    /// The named entries of a directory, without `.` and `..`.
    pub(crate) fn children(&mut self, dir: &Inode) -> Result<Vec<Child>, Error> {
        if !dir.is_dir() {
            return Err(Error::Malformed("PFS directory inode type"));
        }
        Limit::new("PFS directory data", self.limits.max_dir_data).check(dir.size)?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; BLOCK_BYTES];
        let mut offset = 0u64;
        while offset < dir.size {
            let n = to_usize(dir.size.saturating_sub(offset).min(BLOCK))?;
            let block = buf.get_mut(..n).ok_or_else(overflow)?;
            self.read_file(dir, offset, block)?;
            parse_dirents(block, &mut out, self.limits.max_entries)?;
            offset = offset.saturating_add(BLOCK);
        }
        Ok(out)
    }

    /// The inode number of the single `uroot` directory under the super-root.
    pub(crate) fn uroot(&mut self) -> Result<u32, Error> {
        let root = self.inode(0)?;
        let mut roots = self
            .children(&root)?
            .into_iter()
            .filter(|(name, _, dir)| name == UROOT && *dir);
        match (roots.next(), roots.next()) {
            (Some((_, number, _)), None) => Ok(number),
            _ => Err(Error::Malformed("PFS needs exactly one uroot directory")),
        }
    }
}

/// Parses one directory block's entries into `out`.
fn parse_dirents(block: &[u8], out: &mut Vec<Child>, max_entries: u32) -> Result<(), Error> {
    let mut pos = 0usize;
    while let Some(b) = block.get(pos..).filter(|b| !b.is_empty()) {
        if b.len() < DIRENT_HEADER {
            return Err(Error::Malformed("truncated PFS directory entry"));
        }
        let ino = u32_le(b, 0)?;
        let kind = u32_le(b, 4)?;
        let name_len = to_usize(u64::from(u32_le(b, 8)?))?;
        let size = to_usize(u64::from(u32_le(b, 12)?))?;
        if size == 0 {
            if b.iter().any(|v| *v != 0) {
                return Err(Error::Malformed("nonzero PFS directory tail"));
            }
            break;
        }
        let name_end = DIRENT_HEADER.saturating_add(name_len);
        if size < DIRENT_MIN
            || size > b.len()
            || !size.is_multiple_of(DIRENT_ALIGN)
            || name_len == 0
            || name_len > MAX_NAME
            || name_end >= size
            || b.get(name_end) != Some(&0)
        {
            return Err(Error::Malformed("PFS directory entry size or name"));
        }
        let name = b
            .get(DIRENT_HEADER..name_end)
            .and_then(|n| std::str::from_utf8(n).ok())
            .ok_or(Error::Unsupported("non-UTF-8 PFS name"))?;
        match kind {
            DIRENT_DOT if name == "." => {}
            DIRENT_DOTDOT if name == ".." => {}
            DIRENT_FILE | DIRENT_DIR => {
                Limit::new("PFS directory entries", u64::from(max_entries))
                    .check(to_u64(out.len())?.saturating_add(1))?;
                out.push((name.to_owned(), ino, kind == DIRENT_DIR));
            }
            _ => return Err(Error::Unsupported("PFS directory entry kind")),
        }
        pos = pos.checked_add(size).ok_or_else(overflow)?;
    }
    Ok(())
}

impl<T: ReadAt> ReadAt for Pfs<T> {
    fn len(&self) -> u64 {
        self.source.len()
    }

    fn read_at(&mut self, mut offset: u64, mut buf: &mut [u8]) -> Result<(), Error> {
        check_range(offset, buf.len(), self.len())?;
        let Some(cipher) = &self.cipher else {
            return self.source.read_at(offset, buf);
        };
        while !buf.is_empty() {
            let block = offset / BLOCK;
            let within = to_usize(offset % BLOCK)?;
            let n = buf.len().min(BLOCK_BYTES.saturating_sub(within));
            if self.cached != Some(block) {
                self.cached = None;
                let at = block.checked_mul(BLOCK).ok_or_else(overflow)?;
                self.source.read_at(at, &mut self.cache)?;
                // Block 0 (the header) is stored in the clear.
                if block != 0 {
                    let sector = block.checked_mul(SECTORS_PER_BLOCK).ok_or_else(overflow)?;
                    cipher
                        .decrypt_sectors(&mut self.cache, XTS_SECTOR, u128::from(sector))
                        .map_err(|_| Error::Malformed("PFS XTS block"))?;
                }
                self.cached = Some(block);
            }
            let end = within.checked_add(n).ok_or_else(overflow)?;
            let (now, rest) = buf.split_at_mut(n);
            now.copy_from_slice(self.cache.get(within..end).ok_or_else(overflow)?);
            offset = offset.checked_add(to_u64(n)?).ok_or_else(overflow)?;
            buf = rest;
        }
        Ok(())
    }
}

/// One file of a PFS image, as an image of its own.
pub(crate) struct PfsFile<T> {
    pub(crate) pfs: Pfs<T>,
    pub(crate) inode: Inode,
}

impl<T: ReadAt> ReadAt for PfsFile<T> {
    fn len(&self) -> u64 {
        self.inode.size
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        self.pfs.read_file(&self.inode, offset, buf)
    }
}

/// A PFSC image: an indexed sequence of zlib or raw blocks.
pub(crate) struct Pfsc<T> {
    source: T,
    size: u64,
    block: u64,
    table: u64,
    cached: Option<u64>,
    decoded: Vec<u8>,
    stored: Vec<u8>,
}

impl<T: ReadAt> Pfsc<T> {
    pub(crate) fn open(mut source: T) -> Result<Pfsc<T>, Error> {
        let mut h = [0u8; PFSC_HEADER];
        source.read_at(0, &mut h)?;
        if h.get(..4) != Some(&PFSC_MAGIC[..]) || u32_le(&h, 4)? != 0 {
            return Err(Error::Malformed("PFSC header"));
        }
        let block = u64_le(&h, 16)?;
        if block != u64::from(u32_le(&h, 12)?)
            || !block.is_power_of_two()
            || !(PFSC_MIN_BLOCK..=BLOCK).contains(&block)
        {
            return Err(Error::Unsupported("PFSC block size"));
        }
        let table = u64_le(&h, 24)?;
        let start = u64_le(&h, 32)?;
        let size = u64_le(&h, 40)?;
        if size == 0 || !size.is_multiple_of(block) {
            return Err(Error::Malformed("PFSC logical size"));
        }
        let count = size.checked_div(block).ok_or_else(overflow)?;
        let table_len = count
            .checked_add(1)
            .and_then(|n| n.checked_mul(PFSC_OFFSET_LEN))
            .ok_or_else(overflow)?;
        let table_span = Span::new(table, table_len, source.len())
            .map_err(|_| Error::Malformed("PFSC offset table outside image"))?;
        if table < to_u64(PFSC_HEADER)? || start < table_span.end() || start > source.len() {
            return Err(Error::Malformed("PFSC table and data overlap"));
        }
        // Validate every offset with a fixed buffer; the index never scales RAM with payload.
        let mut buf = [0u8; PFSC_BATCH_BYTES];
        let mut previous: Option<u64> = None;
        let mut index = 0u64;
        while index <= count {
            let n = count
                .saturating_add(1)
                .saturating_sub(index)
                .min(PFSC_BATCH);
            let bytes = buf
                .get_mut(..to_usize(n.saturating_mul(PFSC_OFFSET_LEN))?)
                .ok_or_else(overflow)?;
            let at = index
                .checked_mul(PFSC_OFFSET_LEN)
                .and_then(|v| v.checked_add(table))
                .ok_or_else(overflow)?;
            source.read_at(at, bytes)?;
            for pair in bytes.as_chunks::<8>().0 {
                let offset = u64_le(pair, 0)?;
                if offset < start || offset > source.len() {
                    return Err(Error::Malformed("PFSC block offset"));
                }
                if let Some(p) = previous
                    && (offset <= p || offset.saturating_sub(p) > block)
                {
                    return Err(Error::Unsupported("PFSC empty or oversized block"));
                }
                previous = Some(offset);
            }
            index = index.saturating_add(n);
        }
        let block_bytes = to_usize(block)?;
        Ok(Pfsc {
            source,
            size,
            block,
            table,
            cached: None,
            decoded: Vec::with_capacity(block_bytes),
            stored: vec![0; block_bytes],
        })
    }

    pub(crate) fn source_mut(&mut self) -> &mut T {
        &mut self.source
    }

    fn load(&mut self, index: u64) -> Result<(), Error> {
        self.cached = None;
        let mut pair = [0u8; 16];
        let at = index
            .checked_mul(PFSC_OFFSET_LEN)
            .and_then(|v| v.checked_add(self.table))
            .ok_or_else(overflow)?;
        self.source.read_at(at, &mut pair)?;
        let (start, end) = (u64_le(&pair, 0)?, u64_le(&pair, 8)?);
        let len = end
            .checked_sub(start)
            .filter(|v| *v > 0 && *v <= self.block)
            .ok_or(Error::Malformed("PFSC block range changed"))?;
        let stored = self.stored.get_mut(..to_usize(len)?).ok_or_else(overflow)?;
        self.source.read_at(start, stored)?;
        self.decoded.clear();
        if len == self.block {
            self.decoded.extend_from_slice(stored);
        } else {
            let step = Inflater::new(to_usize(self.block)?)
                .push(stored, &mut self.decoded)
                .map_err(|_| Error::Malformed("PFSC zlib data or checksum"))?;
            if !step.finished
                || step.consumed != stored.len()
                || to_u64(self.decoded.len())? != self.block
            {
                return Err(Error::Malformed("PFSC decoded length or trailing data"));
            }
        }
        self.cached = Some(index);
        Ok(())
    }
}

impl<T: ReadAt> ReadAt for Pfsc<T> {
    fn len(&self) -> u64 {
        self.size
    }

    fn read_at(&mut self, mut offset: u64, mut buf: &mut [u8]) -> Result<(), Error> {
        check_range(offset, buf.len(), self.size)?;
        while !buf.is_empty() {
            let index = offset.checked_div(self.block).ok_or_else(overflow)?;
            let within = to_usize(offset.checked_rem(self.block).ok_or_else(overflow)?)?;
            let n = buf.len().min(to_usize(self.block)?.saturating_sub(within));
            if self.cached != Some(index) {
                self.load(index)?;
            }
            let end = within.checked_add(n).ok_or_else(overflow)?;
            let (now, rest) = buf.split_at_mut(n);
            now.copy_from_slice(self.decoded.get(within..end).ok_or_else(overflow)?);
            offset = offset.checked_add(to_u64(n)?).ok_or_else(overflow)?;
            buf = rest;
        }
        Ok(())
    }
}
