//! pantheon-ps4: PS4 PKG reading: header, container records, the fake-key
//! chain and the nested PFS/PFSC application tree ([`Game`]).
//!
//! Nothing here authenticates a package beyond the outer PFS header HMAC.
//! Behavioural reference: pkg-tools `crates/ps4-pkg`, `docs/extraction.md`.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

mod error;
mod game;
mod keys;
mod pfs;
#[cfg(any(test, feature = "testing"))]
// Test-only code: a panic here is a broken fixture, never a user-facing failure.
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::unwrap_used
)]
pub mod testing;

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

use pantheon_core::{Limit, Limits, Span, read_exact_capped, u32_be, u64_be};

pub use error::Error;
pub use game::{FileEntry, FileKind, Game, record_name};

/// `\x7fCNT`.
pub const MAGIC: [u8; 4] = *b"\x7fCNT";
/// The fixed header region; record payloads never start inside it.
pub const HEADER_LEN: u64 = 0x1100;
/// Header bytes holding every field read here.
const HEADER_FIELDS_LEN: usize = 0x5a0;
const RECORD_LEN: u64 = 32;
const CONTENT_ID_LEN: usize = 48;
const ENCRYPTED_FLAG: u32 = 0x8000_0000;
const KEY_INDEX_SHIFT: u32 = 12;
const KEY_INDEX_MASK: u32 = 0xf;

/// Well-known container record IDs.
pub mod id {
    pub const PARAM_SFO: u32 = 0x1000;
    pub const PIC1: u32 = 0x1006;
    pub const ICON0: u32 = 0x1200;
}

/// Header fields, numeric values kept as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub flags: u32,
    pub entry_count: u32,
    pub entry_table_offset: u32,
    pub body_offset: u64,
    pub body_size: u64,
    pub content_id: [u8; CONTENT_ID_LEN],
    pub drm_type: u32,
    pub content_type: u32,
    pub content_flags: u32,
    pub pfs_flags: u64,
    pub pfs_offset: u64,
    pub pfs_size: u64,
    pub mount_image_offset: u64,
    pub mount_image_size: u64,
    pub package_size: u64,
}

impl Header {
    /// The content ID up to its first NUL.
    pub fn content_id_bytes(&self) -> &[u8] {
        until_nul(&self.content_id)
    }
}

/// The bytes of `buf` before its first NUL.
fn until_nul(buf: &[u8]) -> &[u8] {
    buf.split(|b| *b == 0).next().unwrap_or_default()
}

/// One container record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: u32,
    pub name_offset: u32,
    pub flags1: u32,
    pub flags2: u32,
    pub offset: u32,
    pub size: u32,
}

impl Entry {
    pub fn encrypted(&self) -> bool {
        self.flags1 & ENCRYPTED_FLAG != 0
    }

    pub fn key_index(&self) -> u8 {
        // Masked to four bits, so the narrowing cannot lose data.
        u8::try_from((self.flags2 >> KEY_INDEX_SHIFT) & KEY_INDEX_MASK).unwrap_or(0)
    }
}

/// An opened PS4 package. Memory scales with the record count (capped by
/// `Limits::max_records`), never with payload size.
#[derive(Debug)]
pub struct Package<R> {
    reader: R,
    header: Header,
    entries: Vec<Entry>,
    limits: Limits,
}

impl<R: Read + Seek> Package<R> {
    /// Reads and validates the header and the record table. `src` must be the
    /// complete package: its length must equal the declared package size.
    pub fn open(mut src: R, limits: &Limits) -> Result<Package<R>, Error> {
        let total = src.seek(SeekFrom::End(0))?;
        src.seek(SeekFrom::Start(0))?;
        let mut magic = [0; 4];
        src.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(Error::Malformed("bad magic"));
        }
        if total < HEADER_LEN {
            return Err(Error::Malformed("truncated header"));
        }
        src.seek(SeekFrom::Start(0))?;
        let mut h = [0; HEADER_FIELDS_LEN];
        src.read_exact(&mut h)?;
        let header = Header {
            flags: u32_be(&h, 0x04)?,
            entry_count: u32_be(&h, 0x10)?,
            entry_table_offset: u32_be(&h, 0x18)?,
            body_offset: u64_be(&h, 0x20)?,
            body_size: u64_be(&h, 0x28)?,
            content_id: h
                .get(0x40..0x70)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::Malformed("header"))?,
            drm_type: u32_be(&h, 0x70)?,
            content_type: u32_be(&h, 0x74)?,
            content_flags: u32_be(&h, 0x78)?,
            pfs_flags: u64_be(&h, 0x408)?,
            pfs_offset: u64_be(&h, 0x410)?,
            pfs_size: u64_be(&h, 0x418)?,
            mount_image_offset: u64_be(&h, 0x420)?,
            mount_image_size: u64_be(&h, 0x428)?,
            package_size: u64_be(&h, 0x430)?,
        };
        if header.package_size != total {
            return Err(Error::Malformed(
                "declared package size differs from input length",
            ));
        }
        let outside = |_| Error::Malformed("range outside package");
        Span::new(header.body_offset, header.body_size, total).map_err(outside)?;
        Span::new(header.pfs_offset, header.pfs_size, total).map_err(outside)?;
        Span::new(header.mount_image_offset, header.mount_image_size, total).map_err(outside)?;
        if u64::from(header.entry_table_offset) < HEADER_LEN {
            return Err(Error::Malformed("record table overlaps header"));
        }
        Limit::new("PS4 container records", u64::from(limits.max_records))
            .check(u64::from(header.entry_count))?;
        let table_len = u64::from(header.entry_count).saturating_mul(RECORD_LEN);
        let table =
            Span::new(u64::from(header.entry_table_offset), table_len, total).map_err(outside)?;
        src.seek(SeekFrom::Start(table.offset))?;
        let table = read_exact_capped(
            &mut src,
            table.len,
            Limit::new("PS4 record table", limits.max_dir_data),
        )?;

        let mut entries = Vec::with_capacity(usize::try_from(header.entry_count).unwrap_or(0));
        let mut ids = HashSet::with_capacity(entries.capacity());
        for record in table.chunks_exact(usize::try_from(RECORD_LEN).unwrap_or(1)) {
            let entry = Entry {
                id: u32_be(record, 0)?,
                name_offset: u32_be(record, 4)?,
                flags1: u32_be(record, 8)?,
                flags2: u32_be(record, 12)?,
                offset: u32_be(record, 16)?,
                size: u32_be(record, 20)?,
            };
            Span::new(u64::from(entry.offset), u64::from(entry.size), total).map_err(outside)?;
            if entry.size != 0 && u64::from(entry.offset) < HEADER_LEN {
                return Err(Error::Malformed("record data overlaps header"));
            }
            if !ids.insert(entry.id) {
                return Err(Error::Malformed("duplicate container record ID"));
            }
            entries.push(entry);
        }
        Ok(Package {
            reader: src,
            header,
            entries,
            limits: *limits,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry(&self, id: u32) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Plaintext PARAM.SFO bytes. Absent is `Ok(None)`; an encrypted record is
    /// `Error::DecryptionUnavailable`, never "missing" or "malformed".
    pub fn read_param_sfo(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let limit = Limit::new("PARAM.SFO size", self.limits.max_sfo);
        self.read_plaintext(id::PARAM_SFO, limit)
    }

    /// Plaintext `icon0.png` (bounded by `Limits::max_artwork`).
    pub fn read_icon0(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let limit = Limit::new("icon0.png size", self.limits.max_artwork);
        self.read_plaintext(id::ICON0, limit)
    }

    /// Plaintext `pic1.png` (bounded by `Limits::max_artwork`).
    pub fn read_pic1(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let limit = Limit::new("pic1.png size", self.limits.max_artwork);
        self.read_plaintext(id::PIC1, limit)
    }

    /// The payload of record `id` if it is stored in plaintext.
    pub fn read_plaintext(&mut self, id: u32, limit: Limit) -> Result<Option<Vec<u8>>, Error> {
        let Some(entry) = self.entry(id).copied() else {
            return Ok(None);
        };
        if entry.encrypted() {
            return Err(Error::DecryptionUnavailable {
                entry_id: entry.id,
                key_index: entry.key_index(),
            });
        }
        self.reader.seek(SeekFrom::Start(u64::from(entry.offset)))?;
        Ok(Some(read_exact_capped(
            &mut self.reader,
            u64::from(entry.size),
            limit,
        )?))
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests;

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod game_tests;
