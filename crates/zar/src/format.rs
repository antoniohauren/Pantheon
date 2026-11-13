//! ZArchive v1 layout constants and fixed-size record codecs (upstream
//! `zarchivecommon.h` at the pinned revision).

use std::cmp::Ordering;

use pantheon_core::{RangeError, u32_be, u64_be};
use pantheon_crypto::hash::SHA256_LEN;

/// Uncompressed size of every block of the logical stream.
pub const BLOCK: usize = 64 * 1024;
/// [`BLOCK`] as a stream offset.
pub const BLOCK_U64: u64 = 64 * 1024;
/// Blocks described by one offset record.
pub const RECORD_BLOCKS: usize = 16;
/// [`RECORD_BLOCKS`] as a block count.
pub const RECORD_BLOCKS_U64: u64 = 16;
/// One offset record: a u64 base offset and 16 u16 `stored size - 1` fields.
pub const RECORD_LEN: usize = 8 + 2 * RECORD_BLOCKS;
/// [`RECORD_LEN`] as a section size.
pub const RECORD_LEN_U64: u64 = 40;
/// One file-tree node.
pub const NODE_LEN: usize = 16;
/// [`NODE_LEN`] as a section size.
pub const NODE_LEN_U64: u64 = 16;
/// Six (offset, size) sections, the digest, the total size, version and magic.
pub const FOOTER_LEN: usize = 6 * 16 + SHA256_LEN + 8 + 4 + 4;
/// [`FOOTER_LEN`] as a file offset.
pub const FOOTER_LEN_U64: u64 = 144;
/// Where the digest sits inside the footer.
pub const DIGEST_AT: usize = 96;
const DIGEST_END: usize = DIGEST_AT + SHA256_LEN;
const SECTION_OFFSETS: [u64; 6] = [0, 16, 32, 48, 64, 80];
const TOTAL_AT: u64 = 128;
const VERSION_AT: u64 = 136;
const MAGIC_AT: u64 = 140;
pub const MAGIC: u32 = 0x169f_52d6;
pub const VERSION_1: u32 = 0x61bf_3a01;
/// The offset records follow the compressed data at this alignment.
pub const DATA_ALIGN: u64 = 8;
/// File offsets and sizes are 48-bit.
pub const MAX_LOGICAL: u64 = (1 << 48) - 1;
/// Node flag: set for files, clear for directories.
pub const FILE_FLAG: u32 = 0x8000_0000;
/// Low 31 bits of a node's first word: its name-table offset.
pub const NAME_MASK: u32 = 0x7fff_ffff;
/// The root's name offset, meaning "no name".
pub const NO_NAME: u32 = NAME_MASK;
/// Largest name table the pinned reader accepts.
pub const MAX_NAMES: u64 = 0x7fff_ffff;
/// Largest offset-record or file-tree section the pinned reader accepts.
pub const MAX_TABLE: u64 = 0xffff_ffff;
/// Name-length prefix flag for the two-byte form (never written, see `MAX_NAME_BYTES`).
pub const LONG_NAME_FLAG: u8 = 0x80;
/// Low 7 bits of the first name-length byte.
pub const SHORT_NAME_MASK: u8 = 0x7f;
/// Bits of each 48-bit value in the low and high node words.
pub const LOW_BITS: u32 = 32;
pub const HIGH_SHIFT: u32 = 16;
pub const HIGH_MASK: u32 = 0xffff;

/// One `(offset, size)` footer section.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Section {
    pub offset: u64,
    pub size: u64,
}

/// The decoded footer. Section order is the on-disk order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footer {
    pub data: Section,
    pub records: Section,
    pub names: Section,
    pub tree: Section,
    pub meta_directory: Section,
    pub meta_data: Section,
    pub digest: [u8; SHA256_LEN],
    pub total: u64,
}

impl Footer {
    fn sections(&self) -> [Section; 6] {
        [
            self.data,
            self.records,
            self.names,
            self.tree,
            self.meta_directory,
            self.meta_data,
        ]
    }

    pub fn encode(&self) -> [u8; FOOTER_LEN] {
        let mut out = Vec::with_capacity(FOOTER_LEN);
        for s in self.sections() {
            out.extend_from_slice(&s.offset.to_be_bytes());
            out.extend_from_slice(&s.size.to_be_bytes());
        }
        out.extend_from_slice(&self.digest);
        out.extend_from_slice(&self.total.to_be_bytes());
        out.extend_from_slice(&VERSION_1.to_be_bytes());
        out.extend_from_slice(&MAGIC.to_be_bytes());
        let mut fixed = [0u8; FOOTER_LEN];
        fixed.copy_from_slice(&out);
        fixed
    }

    /// Decodes the fields; magic and version are returned for the caller to check.
    pub fn decode(b: &[u8; FOOTER_LEN]) -> Result<(Footer, u32, u32), RangeError> {
        let mut sections = [Section::default(); 6];
        for (s, at) in sections.iter_mut().zip(SECTION_OFFSETS) {
            s.offset = u64_be(b, at)?;
            s.size = u64_be(b, at.checked_add(8).ok_or(RangeError::Overflow)?)?;
        }
        let mut digest = [0u8; SHA256_LEN];
        digest.copy_from_slice(&b[DIGEST_AT..DIGEST_END]);
        let [data, records, names, tree, meta_directory, meta_data] = sections;
        let footer = Footer {
            data,
            records,
            names,
            tree,
            meta_directory,
            meta_data,
            digest,
            total: u64_be(b, TOTAL_AT)?,
        };
        Ok((footer, u32_be(b, VERSION_AT)?, u32_be(b, MAGIC_AT)?))
    }

    /// Every section lies inside a file of `len` bytes.
    pub fn sections_within(&self, len: u64) -> bool {
        self.sections()
            .iter()
            .all(|s| s.offset.checked_add(s.size).is_some_and(|end| end <= len))
    }
}

/// Upstream node order: bytewise after folding ASCII `A-Z` to lower case, a
/// prefix first. Siblings that compare equal are refused by the writer.
pub fn compare_names(a: &str, b: &str) -> Ordering {
    let fold = |s: &str| {
        s.bytes()
            .map(|c| c.to_ascii_lowercase())
            .collect::<Vec<u8>>()
    };
    fold(a).cmp(&fold(b))
}
