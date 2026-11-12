//! Synthetic PS4 container builder for tests. Produces structurally valid
//! headers and record tables only; no keys, signatures or PFS images.

use crate::{ENCRYPTED_FLAG, HEADER_LEN, MAGIC};

/// One record to place in a synthetic package.
#[derive(Clone, Debug)]
pub struct Record {
    pub id: u32,
    pub encrypted: bool,
    pub key_index: u8,
    pub data: Vec<u8>,
}

impl Record {
    pub fn plain(id: u32, data: &[u8]) -> Record {
        Record {
            id,
            encrypted: false,
            key_index: 0,
            data: data.to_vec(),
        }
    }
}

const TABLE_OFFSET: usize = HEADER_LEN as usize;
const RECORD_LEN: usize = 32;
const ALIGN: usize = 16;

fn put32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

fn put64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

/// A package with `content_id`, the given records (in order) after the record
/// table, and a 16-byte dummy PFS region at the end.
pub fn package(content_id: &str, records: &[Record]) -> Vec<u8> {
    let mut out = vec![0u8; TABLE_OFFSET + records.len() * RECORD_LEN];
    let mut placed = Vec::new();
    for r in records {
        out.resize(out.len().next_multiple_of(ALIGN), 0);
        placed.push(out.len());
        out.extend_from_slice(&r.data);
    }
    out.resize(out.len().next_multiple_of(ALIGN), 0);
    let pfs_offset = out.len();
    out.extend_from_slice(&[0u8; ALIGN]);
    let total = out.len();

    out[..4].copy_from_slice(&MAGIC);
    put32(&mut out, 0x04, 0x8000_0000);
    put32(&mut out, 0x10, records.len() as u32);
    put32(&mut out, 0x18, TABLE_OFFSET as u32);
    put64(&mut out, 0x20, TABLE_OFFSET as u64);
    put64(&mut out, 0x28, (pfs_offset - TABLE_OFFSET) as u64);
    let id = content_id.as_bytes();
    out[0x40..0x40 + id.len()].copy_from_slice(id);
    put32(&mut out, 0x70, 0xf);
    put32(&mut out, 0x74, 0x1a);
    put32(&mut out, 0x78, 0x0a00_0000);
    put64(&mut out, 0x408, 0);
    put64(&mut out, 0x410, pfs_offset as u64);
    put64(&mut out, 0x418, ALIGN as u64);
    put64(&mut out, 0x430, total as u64);
    for (i, (r, at)) in records.iter().zip(placed).enumerate() {
        let rec = TABLE_OFFSET + i * RECORD_LEN;
        put32(&mut out, rec, r.id);
        put32(
            &mut out,
            rec + 8,
            if r.encrypted { ENCRYPTED_FLAG } else { 0 },
        );
        put32(&mut out, rec + 12, u32::from(r.key_index) << 12);
        put32(&mut out, rec + 16, at as u32);
        put32(&mut out, rec + 20, r.data.len() as u32);
    }
    out
}
