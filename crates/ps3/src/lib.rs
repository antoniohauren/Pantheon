//! pantheon-ps3: PS3 retail/debug PKG reading (header, metadata, entry table,
//! decrypted entry payloads).
//!
//! Only the container layer is decoded; nested SELF/EDAT files stay as stored.
//! Signatures are never checked. Behavioural reference: pkg-tools
//! `crates/ps3-pkg` (`lib.rs`), `docs/ps3.md`.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

mod crypt;
mod error;
#[cfg(any(test, feature = "testing"))]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
pub mod testing;

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, SeekFrom};

use pantheon_core::{CHUNK, Limit, Limits, Span, read_exact_capped, u16_be, u32_be, u64_be};

pub use crypt::RETAIL_KEY;
pub use error::Error;

/// `\x7fPKG`.
pub const MAGIC: [u8; 4] = *b"\x7fPKG";
/// The fixed header; metadata and data start at or after it.
pub const HEADER_LEN: u64 = HEADER_BYTES as u64;
const HEADER_BYTES: usize = 0xc0;
pub const RELEASE_DEBUG: u16 = 0;
pub const RELEASE_RETAIL: u16 = 0x8000;
pub const PLATFORM_PS3: u16 = 1;
const RECORD_LEN: u64 = 32;
const METADATA_PACKET_LEN: u64 = 8;
const CONTENT_ID_LEN: usize = 48;
const DIGEST_LEN: usize = 16;
const KIND_MASK: u32 = 0xff;
const KIND_DIRECTORY: u32 = 4;
/// Flag bits allowed besides the kind byte.
const ALLOWED_FLAGS: u32 = 0x8000_0000;
/// Seen on NPDRM (kind 1) entries; RPCS3 keeps it without changing decoding.
const NPDRM_COMPAT_FLAG: u32 = 0x100;
/// Known entry kinds: NPDRM, NPDRMEDAT, regular, folder, SDAT.
const KNOWN_KINDS: [u32; 5] = [1, 2, 3, 4, 9];
const KIND_NPDRM: u32 = 1;

pub const PARAM_SFO: &str = "PARAM.SFO";
pub const ICON0: &str = "ICON0.PNG";

/// Header fields, numeric values kept as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub release_type: u16,
    pub platform: u16,
    pub metadata_offset: u32,
    pub metadata_count: u32,
    pub metadata_size: u32,
    pub entry_count: u32,
    pub package_size: u64,
    pub data_offset: u64,
    pub data_size: u64,
    pub content_id: [u8; CONTENT_ID_LEN],
    pub qa_digest: [u8; DIGEST_LEN],
    pub nonce: [u8; DIGEST_LEN],
}

impl Header {
    /// The content ID up to its first NUL.
    pub fn content_id_bytes(&self) -> &[u8] {
        self.content_id
            .split(|b| *b == 0)
            .next()
            .unwrap_or_default()
    }

    pub fn is_retail(&self) -> bool {
        self.release_type == RELEASE_RETAIL
    }
}

/// One raw metadata packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub id: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
}

/// One entry of the application tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    /// Offset in the data region; zero for implied parent directories.
    pub offset: u64,
    pub size: u64,
    pub flags: u32,
}

impl FileEntry {
    pub fn kind(&self) -> FileKind {
        if self.flags & KIND_MASK == KIND_DIRECTORY {
            FileKind::Directory
        } else {
            FileKind::File
        }
    }
}

/// An opened PS3 package. Memory scales with the entry table (capped by
/// `Limits`), never with payload size.
#[derive(Debug)]
pub struct Package<R> {
    reader: R,
    header: Header,
    metadata: Vec<Metadata>,
    entries: Vec<FileEntry>,
    limits: Limits,
}

fn outside(_: pantheon_core::RangeError) -> Error {
    Error::Malformed("range outside declared region (damaged input or unsupported keyset)")
}

impl<R: Read + Seek> Package<R> {
    /// Reads and validates the header, metadata and entry table. `src` must be
    /// the complete package (split packages are unsupported).
    pub fn open(mut src: R, limits: &Limits) -> Result<Package<R>, Error> {
        let total = src.seek(SeekFrom::End(0))?;
        src.seek(SeekFrom::Start(0))?;
        let mut h = [0; HEADER_BYTES];
        src.read_exact(&mut h)?;
        if h.get(..4) != Some(&MAGIC[..]) {
            return Err(Error::Malformed("bad magic"));
        }
        let field = |range: std::ops::Range<usize>| h.get(range).unwrap_or_default();
        let header = Header {
            release_type: u16_be(&h, 4)?,
            platform: u16_be(&h, 6)?,
            metadata_offset: u32_be(&h, 8)?,
            metadata_count: u32_be(&h, 12)?,
            metadata_size: u32_be(&h, 16)?,
            entry_count: u32_be(&h, 20)?,
            package_size: u64_be(&h, 24)?,
            data_offset: u64_be(&h, 32)?,
            data_size: u64_be(&h, 40)?,
            content_id: field(48..96)
                .try_into()
                .map_err(|_| Error::Malformed("header"))?,
            qa_digest: field(96..112)
                .try_into()
                .map_err(|_| Error::Malformed("header"))?,
            nonce: field(112..128)
                .try_into()
                .map_err(|_| Error::Malformed("header"))?,
        };
        if !matches!(header.release_type, RELEASE_DEBUG | RELEASE_RETAIL) {
            return Err(Error::Unsupported("release type"));
        }
        if header.platform != PLATFORM_PS3 {
            return Err(Error::Unsupported("platform (PSP/Vita/PSM are not PS3)"));
        }
        if header.package_size != total {
            return Err(Error::Malformed(
                "declared package size differs from input length; split packages are unsupported",
            ));
        }
        if u64::from(header.metadata_offset) < HEADER_LEN || header.data_offset < HEADER_LEN {
            return Err(Error::Malformed("metadata/data overlaps header"));
        }
        let meta = Span::new(
            u64::from(header.metadata_offset),
            u64::from(header.metadata_size),
            header.data_offset,
        )
        .map_err(outside)?;
        Span::new(header.data_offset, header.data_size, total).map_err(outside)?;
        let table_len = u64::from(header.entry_count).saturating_mul(RECORD_LEN);
        Span::new(0, table_len, header.data_size).map_err(outside)?;
        Limit::new("PS3 entries", u64::from(limits.max_entries))
            .check(u64::from(header.entry_count))?;
        Limit::new("PS3 metadata packets", u64::from(limits.max_records))
            .check(u64::from(header.metadata_count))?;

        src.seek(SeekFrom::Start(meta.offset))?;
        let block = read_exact_capped(
            &mut src,
            meta.len,
            Limit::new("PS3 metadata", limits.max_record),
        )?;
        let metadata = parse_metadata(&block, header.metadata_count)?;

        let mut pkg = Package {
            reader: src,
            header,
            metadata,
            entries: Vec::new(),
            limits: *limits,
        };
        pkg.entries = pkg.read_table(table_len)?;
        Ok(pkg)
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn metadata(&self) -> &[Metadata] {
        &self.metadata
    }

    /// The application tree sorted by ASCII-lowercased path, implied parent
    /// directories included.
    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    /// Reads and decrypts `buf.len()` bytes at `offset` in the data region.
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        let len = u64::try_from(buf.len()).map_err(|_| Error::Malformed("read too large"))?;
        Span::new(offset, len, self.header.data_size).map_err(outside)?;
        let absolute = self
            .header
            .data_offset
            .checked_add(offset)
            .ok_or(Error::Malformed("offset overflows"))?;
        self.reader.seek(SeekFrom::Start(absolute))?;
        self.reader.read_exact(buf)?;
        crypt::apply(&self.header, offset, buf);
        Ok(())
    }

    /// The decrypted bytes of the top-level file `name`, `None` if absent.
    pub fn read_named(&mut self, name: &str, limit: Limit) -> Result<Option<Vec<u8>>, Error> {
        let Some(entry) = self.entries.iter().find(|e| e.path == name) else {
            return Ok(None);
        };
        if entry.kind() != FileKind::File {
            return Err(Error::Unsupported("cannot read a directory as a file"));
        }
        let (offset, size) = (entry.offset, entry.size);
        let len = pantheon_core::usize_len(size, limit)?;
        let mut out = vec![0; len];
        let mut done = 0u64;
        for chunk in out.chunks_mut(CHUNK) {
            self.read_at(offset.saturating_add(done), chunk)?;
            done = done.saturating_add(u64::try_from(chunk.len()).unwrap_or(0));
        }
        Ok(Some(out))
    }

    /// Decrypted `PARAM.SFO`, bounded by `Limits::max_sfo`.
    pub fn read_param_sfo(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let limit = Limit::new("PARAM.SFO size", self.limits.max_sfo);
        self.read_named(PARAM_SFO, limit)
    }

    /// Decrypted `ICON0.PNG`, bounded by `Limits::max_artwork`.
    pub fn read_icon0(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let limit = Limit::new("ICON0.PNG size", self.limits.max_artwork);
        self.read_named(ICON0, limit)
    }

    fn read_table(&mut self, table_len: u64) -> Result<Vec<FileEntry>, Error> {
        let len = pantheon_core::usize_len(
            table_len,
            Limit::new("PS3 entry table", self.limits.max_dir_data),
        )?;
        let mut table = vec![0; len];
        self.read_at(0, &mut table)?;
        let mut tree = Tree::new(self.limits);
        let mut explicit = HashSet::new();
        for record in table.chunks_exact(usize::try_from(RECORD_LEN).unwrap_or(1)) {
            let name_offset = u64::from(u32_be(record, 0)?);
            let name_size = u32_be(record, 4)?;
            let offset = u64_be(record, 8)?;
            let size = u64_be(record, 16)?;
            let flags = u32_be(record, 24)?;
            let kind = flags & KIND_MASK;
            if name_size == 0
                || usize::try_from(name_size).unwrap_or(usize::MAX) > pantheon_fs::MAX_PATH_BYTES
            {
                return Err(Error::Malformed(
                    "entry name length (damaged input or unsupported keyset)",
                ));
            }
            let compat = if kind == KIND_NPDRM {
                NPDRM_COMPAT_FLAG
            } else {
                0
            };
            if flags & !(ALLOWED_FLAGS | KIND_MASK | compat) != 0
                || !KNOWN_KINDS.contains(&kind)
                || u32_be(record, 28)? != 0
            {
                return Err(Error::Unsupported(
                    "unknown entry kind/flags or nonzero padding (damaged input or unsupported variant)",
                ));
            }
            Span::new(offset, size, self.header.data_size).map_err(outside)?;
            if name_offset < table_len || (size != 0 && offset < table_len) {
                return Err(Error::Malformed("entry name/payload overlaps table"));
            }
            if kind == KIND_DIRECTORY && size != 0 {
                return Err(Error::Malformed("directory has file payload"));
            }
            let mut name = vec![0; usize::try_from(name_size).unwrap_or(0)];
            self.read_at(name_offset, &mut name)?;
            while name.last() == Some(&0) {
                name.pop();
            }
            let path =
                String::from_utf8(name).map_err(|_| Error::Unsupported("non-UTF-8 entry path"))?;
            if let Err(reason) = pantheon_fs::validate_path(&path) {
                return Err(Error::UnsafePath { path, reason });
            }
            if !explicit.insert(path.to_ascii_lowercase()) {
                return Err(Error::UnsafePath {
                    path,
                    reason: pantheon_fs::Reason::CaseCollision,
                });
            }
            let mut parent = path.as_str();
            let parents: Vec<String> = std::iter::from_fn(|| {
                let (prefix, _) = parent.rsplit_once('/')?;
                parent = prefix;
                Some(prefix.to_owned())
            })
            .collect();
            tree.insert(
                FileEntry {
                    path,
                    offset,
                    size,
                    flags,
                },
                false,
            )?;
            for prefix in parents {
                let dir = FileEntry {
                    path: prefix,
                    offset: 0,
                    size: 0,
                    flags: KIND_DIRECTORY,
                };
                tree.insert(dir, true)?;
            }
        }
        Ok(tree.map.into_values().collect())
    }
}

/// Parses `count` (id, size, data) packets that must fit inside `block`.
fn parse_metadata(block: &[u8], count: u32) -> Result<Vec<Metadata>, Error> {
    let total = u64::try_from(block.len()).map_err(|_| Error::Malformed("metadata"))?;
    let mut out = Vec::new();
    let mut ids = HashSet::new();
    let mut at = 0u64;
    for _ in 0..count {
        let packet = Span::new(at, METADATA_PACKET_LEN, total).map_err(outside)?;
        let id = u32_be(block, packet.offset)?;
        let size = u32_be(block, packet.offset.saturating_add(4))?;
        let data = Span::new(packet.end(), u64::from(size), total).map_err(outside)?;
        if !ids.insert(id) {
            return Err(Error::Malformed("duplicate metadata ID"));
        }
        let start = usize::try_from(data.offset).unwrap_or(usize::MAX);
        let len = usize::try_from(data.len).unwrap_or(usize::MAX);
        let bytes = block
            .get(start..)
            .and_then(|b| b.get(..len))
            .ok_or(Error::Malformed("metadata"))?;
        out.push(Metadata {
            id,
            data: bytes.to_vec(),
        });
        at = data.end();
    }
    Ok(out)
}

/// The sorted tree, keyed by ASCII-lowercased path so case collisions are caught.
struct Tree {
    map: BTreeMap<String, FileEntry>,
    path_bytes: u64,
    limits: Limits,
}

impl Tree {
    fn new(limits: Limits) -> Tree {
        Tree {
            map: BTreeMap::new(),
            path_bytes: 0,
            limits,
        }
    }

    fn insert(&mut self, entry: FileEntry, implied: bool) -> Result<(), Error> {
        let key = entry.path.to_ascii_lowercase();
        if let Some(old) = self.map.get(&key) {
            // Only a directory may repeat, with identical case, as an implied parent.
            if old.path != entry.path
                || old.kind() != FileKind::Directory
                || entry.kind() != FileKind::Directory
            {
                return Err(Error::UnsafePath {
                    path: entry.path,
                    reason: pantheon_fs::Reason::CaseCollision,
                });
            }
            if !implied {
                self.map.insert(key, entry);
            }
            return Ok(());
        }
        self.path_bytes = self
            .path_bytes
            .saturating_add(u64::try_from(entry.path.len()).unwrap_or(u64::MAX));
        let count = u64::try_from(self.map.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        Limit::new("PS3 tree entries", u64::from(self.limits.max_entries)).check(count)?;
        Limit::new("PS3 path bytes", self.limits.max_path_bytes).check(self.path_bytes)?;
        self.map.insert(key, entry);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests;
