//! Synthetic PS3 package builder for tests. Uses only the public retail key or
//! the debug digest scheme; no signatures, no game data.

use crate::{HEADER_LEN, Header, MAGIC, PLATFORM_PS3, RELEASE_DEBUG, RELEASE_RETAIL, crypt};

/// One explicit entry: path, kind byte (3 = file, 4 = folder) and payload.
#[derive(Clone, Debug)]
pub struct Item {
    pub path: String,
    pub flags: u32,
    pub data: Vec<u8>,
}

impl Item {
    pub fn file(path: &str, data: &[u8]) -> Item {
        Item {
            path: path.into(),
            flags: 3,
            data: data.to_vec(),
        }
    }

    pub fn dir(path: &str) -> Item {
        Item {
            path: path.into(),
            flags: 4,
            data: Vec::new(),
        }
    }
}

const ALIGN: usize = 16;
const RECORD_LEN: usize = 32;

fn put32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

fn put64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

/// A complete package. `metadata` packets are `(id, data)`.
pub fn package(
    retail: bool,
    content_id: &str,
    metadata: &[(u32, &[u8])],
    items: &[Item],
) -> Vec<u8> {
    let mut meta = Vec::new();
    for (id, data) in metadata {
        meta.extend_from_slice(&id.to_be_bytes());
        meta.extend_from_slice(&(data.len() as u32).to_be_bytes());
        meta.extend_from_slice(data);
    }
    let data_offset = (HEADER_LEN as usize + meta.len()).next_multiple_of(ALIGN);

    let mut data = vec![0u8; items.len() * RECORD_LEN];
    let mut places = Vec::new();
    for item in items {
        data.resize(data.len().next_multiple_of(ALIGN), 0);
        let name_at = data.len();
        data.extend_from_slice(item.path.as_bytes());
        data.resize(data.len().next_multiple_of(ALIGN), 0);
        let body_at = data.len();
        data.extend_from_slice(&item.data);
        places.push((name_at, body_at));
    }
    data.resize(data.len().next_multiple_of(ALIGN), 0);
    for (i, (item, (name_at, body_at))) in items.iter().zip(&places).enumerate() {
        let rec = i * RECORD_LEN;
        put32(&mut data, rec, *name_at as u32);
        put32(&mut data, rec + 4, item.path.len() as u32);
        put64(
            &mut data,
            rec + 8,
            if item.data.is_empty() {
                0
            } else {
                *body_at as u64
            },
        );
        put64(&mut data, rec + 16, item.data.len() as u64);
        put32(&mut data, rec + 24, item.flags);
    }

    let mut header = Header {
        release_type: if retail {
            RELEASE_RETAIL
        } else {
            RELEASE_DEBUG
        },
        platform: PLATFORM_PS3,
        metadata_offset: HEADER_LEN as u32,
        metadata_count: metadata.len() as u32,
        metadata_size: meta.len() as u32,
        entry_count: items.len() as u32,
        package_size: (data_offset + data.len()) as u64,
        data_offset: data_offset as u64,
        data_size: data.len() as u64,
        content_id: [0; 48],
        qa_digest: *b"0123456789abcdef",
        nonce: *b"fedcba9876543210",
    };
    header.content_id[..content_id.len()].copy_from_slice(content_id.as_bytes());
    crypt::apply(&header, 0, &mut data);

    let mut out = vec![0u8; data_offset];
    out[..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&header.release_type.to_be_bytes());
    out[6..8].copy_from_slice(&header.platform.to_be_bytes());
    put32(&mut out, 8, header.metadata_offset);
    put32(&mut out, 12, header.metadata_count);
    put32(&mut out, 16, header.metadata_size);
    put32(&mut out, 20, header.entry_count);
    put64(&mut out, 24, header.package_size);
    put64(&mut out, 32, header.data_offset);
    put64(&mut out, 40, header.data_size);
    out[48..96].copy_from_slice(&header.content_id);
    out[96..112].copy_from_slice(&header.qa_digest);
    out[112..128].copy_from_slice(&header.nonce);
    out[HEADER_LEN as usize..HEADER_LEN as usize + meta.len()].copy_from_slice(&meta);
    out.extend_from_slice(&data);
    out
}
