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

// ---------------------------------------------------------------------------
// Synthetic GD fake packages: the full key chain and nested PFS/PFSC images.

use crate::keys::{self, Keyset};
use crate::pfs::{self, BLOCK};
use pantheon_crypto::{aes, rsa, zlib};

const B: usize = BLOCK as usize;
const PFS_MAGIC: u64 = 20_130_315;
const SIGNED_INODE: usize = 0x2c8;
const PLAIN_INODE: usize = 0xa8;
const REFS_PER_BLOCK: usize = B / 36;

/// One node of a synthetic PFS image; the node index is its inode number.
#[derive(Clone, Debug)]
pub enum PfsNode {
    /// Children: name and node index.
    Dir(Vec<(String, u32)>),
    File {
        data: Vec<u8>,
        /// The inode's "compressed size" field (normally `data.len()`).
        compressed_size: u64,
        flags: u32,
    },
}

impl PfsNode {
    pub fn file(data: &[u8]) -> PfsNode {
        PfsNode::File {
            data: data.to_vec(),
            compressed_size: data.len() as u64,
            flags: 0,
        }
    }
}

/// Super-root (0) → `uroot` (1) → the given files and folders (paths
/// `/`-separated; parents are created as needed, `dirs` may be empty folders).
pub fn app_nodes(files: &[(&str, &[u8])], dirs: &[&str]) -> Vec<PfsNode> {
    let mut nodes = vec![
        PfsNode::Dir(vec![("uroot".into(), 1)]),
        PfsNode::Dir(Vec::new()),
    ];
    let mut by_path = std::collections::HashMap::new();
    fn dir_of(
        nodes: &mut Vec<PfsNode>,
        by_path: &mut std::collections::HashMap<String, u32>,
        path: &str,
    ) -> u32 {
        if path.is_empty() {
            return 1;
        }
        if let Some(i) = by_path.get(path) {
            return *i;
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = dir_of(nodes, by_path, parent);
        let me = nodes.len() as u32;
        nodes.push(PfsNode::Dir(Vec::new()));
        if let PfsNode::Dir(c) = &mut nodes[parent as usize] {
            c.push((name.into(), me));
        }
        by_path.insert(path.into(), me);
        me
    }
    for d in dirs {
        dir_of(&mut nodes, &mut by_path, d);
    }
    for (path, data) in files {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = dir_of(&mut nodes, &mut by_path, parent);
        let me = nodes.len() as u32;
        nodes.push(PfsNode::file(data));
        if let PfsNode::Dir(c) = &mut nodes[parent as usize] {
            c.push(((*name).into(), me));
        }
    }
    nodes
}

fn le16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn le32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn le64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn dirent(out: &mut Vec<u8>, ino: u32, kind: u32, name: &str) {
    let size = (16 + name.len() + 1).next_multiple_of(8);
    let at = out.len();
    out.resize(at + size, 0);
    le32(out, at, ino);
    le32(out, at + 4, kind);
    le32(out, at + 8, name.len() as u32);
    le32(out, at + 12, size as u32);
    out[at + 16..at + 16 + name.len()].copy_from_slice(name.as_bytes());
}

/// A clear PFS image of `nodes` with mode `mode` (bit 0 = signed inodes).
/// Signed multi-block files use direct and single-indirect references.
pub fn pfs_image(nodes: &[PfsNode], mode: u16) -> Vec<u8> {
    let signed = mode & 1 != 0;
    let width = if signed { SIGNED_INODE } else { PLAIN_INODE };
    let per_block = B / width;
    let inode_blocks = nodes.len().div_ceil(per_block);
    let mut parents = vec![0u32; nodes.len()];
    for (i, n) in nodes.iter().enumerate() {
        if let PfsNode::Dir(c) = n {
            for (_, child) in c {
                parents[*child as usize] = i as u32;
            }
        }
    }
    let mut blocks: Vec<Vec<u8>> = vec![vec![0; B]; 1 + inode_blocks];
    for (i, node) in nodes.iter().enumerate() {
        let (data, mode_bits, size, compressed, flags) = match node {
            PfsNode::Dir(children) => {
                let mut d = Vec::new();
                dirent(&mut d, i as u32, 4, ".");
                dirent(&mut d, parents[i], 5, "..");
                for (name, child) in children {
                    let kind = if matches!(nodes[*child as usize], PfsNode::Dir(_)) {
                        3
                    } else {
                        2
                    };
                    dirent(&mut d, *child, kind, name);
                }
                let len = d.len() as u64;
                (d, 0x41ed, len, len, 0)
            }
            PfsNode::File {
                data,
                compressed_size,
                flags,
            } => (
                data.clone(),
                0x81a4,
                data.len() as u64,
                *compressed_size,
                *flags,
            ),
        };
        let count = data.len().div_ceil(B);
        let start = blocks.len();
        for chunk in data.chunks(B) {
            let mut b = vec![0; B];
            b[..chunk.len()].copy_from_slice(chunk);
            blocks.push(b);
        }
        let mut refs = [0u32; 17];
        if signed && count > 1 {
            for (k, r) in refs.iter_mut().enumerate().take(count.min(12)) {
                *r = (start + k) as u32;
            }
            if count > 12 {
                assert!(
                    count <= 12 + REFS_PER_BLOCK,
                    "synthetic files use one indirect block"
                );
                let mut ind = vec![0; B];
                for k in 12..count {
                    let at = (k - 12) * 36 + 32;
                    le32(&mut ind, at, (start + k) as u32);
                }
                refs[12] = blocks.len() as u32;
                blocks.push(ind);
            }
        } else if count > 0 {
            refs[0] = start as u32;
            if count > 1 {
                refs[1] = u32::MAX;
            }
        }
        let at = (1 + i / per_block, (i % per_block) * width);
        let ib = &mut blocks[at.0];
        let o = at.1;
        le16(ib, o, mode_bits);
        le32(ib, o + 4, flags);
        le64(ib, o + 8, size);
        le64(ib, o + 16, compressed);
        le32(ib, o + 96, count as u32);
        for (k, r) in refs.iter().enumerate() {
            let pos = if signed {
                100 + k * 36 + 32
            } else {
                100 + k * 4
            };
            le32(ib, o + pos, *r);
        }
    }
    let h = &mut blocks[0];
    le64(h, 0, 1);
    le64(h, 8, PFS_MAGIC);
    le16(h, 0x1c, mode);
    le32(h, 0x20, BLOCK as u32);
    le64(h, 0x30, nodes.len() as u64);
    le64(h, 0x40, inode_blocks as u64);
    blocks.concat()
}

/// PFSC-wraps `image` in 64 KiB blocks: zlib where it saves space, raw otherwise.
pub fn pfsc_image(image: &[u8]) -> Vec<u8> {
    assert!(image.len().is_multiple_of(B));
    let count = image.len() / B;
    let table = 48;
    let start = table + (count + 1) * 8;
    let mut out = vec![0u8; start];
    let mut offsets = vec![start as u64];
    for block in image.chunks(B) {
        let z = zlib::deflate(block);
        out.extend_from_slice(if z.len() < B { &z } else { block });
        offsets.push(out.len() as u64);
    }
    out[..4].copy_from_slice(b"PFSC");
    le32(&mut out, 12, BLOCK as u32);
    le64(&mut out, 16, BLOCK);
    le64(&mut out, 24, table as u64);
    le64(&mut out, 32, start as u64);
    le64(&mut out, 40, image.len() as u64);
    for (i, o) in offsets.iter().enumerate() {
        le64(&mut out, table + i * 8, *o);
    }
    out.resize(out.len().next_multiple_of(16), 0);
    out
}

/// Signs the header and XTS-encrypts every block but the first, in place.
pub fn seal_pfs(image: &mut [u8], ekpfs: &[u8; 32], seed: &[u8; 16]) {
    image[0x370..0x380].copy_from_slice(seed);
    let signing = pfs::pfs_key(ekpfs, seed, 2);
    let mut covered = [0u8; 0x5a0];
    covered[..0x380].copy_from_slice(&image[..0x380]);
    let mac = pantheon_crypto::hash::hmac_sha256(&signing, &covered);
    image[0x380..0x3a0].copy_from_slice(&mac);
    let k = pfs::pfs_key(ekpfs, seed, 1);
    let xts = aes::Xts128::new(k[16..].try_into().unwrap(), k[..16].try_into().unwrap());
    for (i, block) in image.chunks_mut(B).enumerate().skip(1) {
        xts.encrypt_sectors(block, 0x1000, (i * 16) as u128)
            .unwrap();
    }
}

/// PKCS#1 v1.5 type-2 RSA wrap of a 32-byte key with fixed non-zero padding.
fn rsa_wrap(which: Keyset, key: &[u8; 32]) -> [u8; 256] {
    let mut block = [0xa5u8; 256];
    block[0] = 0;
    block[1] = 2;
    block[256 - 33] = 0;
    block[256 - 32..].copy_from_slice(key);
    let n = keys::private_key(which).unwrap().modulus();
    rsa::public_wrap(&n, 65_537, &block).unwrap()
}

/// CBC over full blocks, CFB over the final partial block (the format's tail).
pub fn encrypt_record(plain: &[u8], material: &[u8; 32]) -> Vec<u8> {
    let (iv, key) = keys::split_material(material);
    let full = plain.len() / 16 * 16;
    let mut out = plain.to_vec();
    aes::cbc_encrypt_in_place(&key, &iv, &mut out[..full]).unwrap();
    if full < out.len() {
        let mut stream: [u8; 16] = if full == 0 {
            iv
        } else {
            out[full - 16..full].try_into().unwrap()
        };
        aes::encrypt_block(&key, &mut stream);
        for (b, s) in out[full..].iter_mut().zip(stream) {
            *b ^= s;
        }
    }
    out
}

/// A GD fake package whose application tree is `inner` (see [`app_nodes`])
/// plus `records` (plaintext; `encrypted` ones are sealed with key index 3).
pub fn game_package(content_id: &str, inner: &[PfsNode], records: &[Record]) -> Vec<u8> {
    let dk3 = [0x3du8; 32];
    let ekpfs = [0x5eu8; 32];
    let inner_image = pfs_image(inner, 0x8);
    let pfsc = pfsc_image(&inner_image);
    let outer_nodes = vec![
        PfsNode::Dir(vec![("uroot".into(), 1)]),
        PfsNode::Dir(vec![("pfs_image.dat".into(), 2)]),
        PfsNode::File {
            data: pfsc,
            compressed_size: inner_image.len() as u64,
            flags: 0xd,
        },
    ];
    let mut outer = pfs_image(&outer_nodes, 0xd);
    seal_pfs(&mut outer, &ekpfs, &[0x77; 16]);

    let mut keyset = vec![0u8; keys::KEYS_LEN];
    keyset[32 + 3 * 32..32 + 4 * 32].copy_from_slice(&keys::key_digest(&dk3));
    keyset[256 + 3 * 256..256 + 4 * 256].copy_from_slice(&rsa_wrap(Keyset::Dk3, &dk3));
    let mut all = vec![
        Record::plain(keys::KEYS_ID, &keyset),
        Record {
            id: keys::IMAGE_KEY_ID,
            encrypted: true,
            key_index: 3,
            data: rsa_wrap(Keyset::Fake, &ekpfs).to_vec(),
        },
    ];
    all.extend_from_slice(records);

    let mut out = package(content_id, &all);
    // Replace the dummy PFS region with the sealed outer image (64 KiB aligned).
    let table = TABLE_OFFSET;
    let pfs_offset = (out.len() - ALIGN).next_multiple_of(B);
    out.truncate(out.len() - ALIGN);
    out.resize(pfs_offset, 0);
    out.extend_from_slice(&outer);
    let total = out.len() as u64;
    put64(&mut out, 0x28, (pfs_offset - table) as u64);
    put64(&mut out, 0x408, 0x8000_0000_0000_0000);
    put64(&mut out, 0x410, pfs_offset as u64);
    put64(&mut out, 0x418, outer.len() as u64);
    put64(&mut out, 0x430, total);
    put32(&mut out, 0x78, 0);
    // Seal encrypted records in place now that offsets are final.
    for i in 0..all.len() {
        let rec = table + i * RECORD_LEN;
        let entry = crate::Entry {
            id: u32::from_be_bytes(out[rec..rec + 4].try_into().unwrap()),
            name_offset: 0,
            flags1: u32::from_be_bytes(out[rec + 8..rec + 12].try_into().unwrap()),
            flags2: u32::from_be_bytes(out[rec + 12..rec + 16].try_into().unwrap()),
            offset: u32::from_be_bytes(out[rec + 16..rec + 20].try_into().unwrap()),
            size: u32::from_be_bytes(out[rec + 20..rec + 24].try_into().unwrap()),
        };
        if entry.encrypted() {
            let at = entry.offset as usize;
            let len = entry.size as usize;
            let sealed = encrypt_record(&out[at..at + len], &keys::entry_material(&entry, &dk3));
            out[at..at + len].copy_from_slice(&sealed);
        }
    }
    out
}
