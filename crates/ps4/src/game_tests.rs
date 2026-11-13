//! Application-tree tests over synthetic GD fake packages and sparse PFS images.

use std::collections::HashMap;
use std::io::Cursor;

use pantheon_core::testing::{Corruptor, SplitMix64};
use pantheon_core::{Classify, ErrorKind, Limits};

use crate::pfs::{BLOCK, Pfs, ReadAt};
use crate::testing::{PfsNode, Record, app_nodes, game_package};
use crate::*;

const L: &Limits = &Limits::DEFAULT;
const CID: &str = "UP0000-TEST00000_00-0000000000000000";

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut v = vec![0; len];
    SplitMix64::new(seed).fill(&mut v);
    v
}

/// Big enough that the outer `pfs_image.dat` needs an indirect block.
fn big() -> Vec<u8> {
    noise(1_100_000, 7)
}

fn records() -> Vec<Record> {
    vec![
        Record::plain(0x1000, b"\0PSF synthetic"),
        Record {
            id: 0x1200,
            encrypted: true,
            key_index: 3,
            data: noise(37, 1), // CBC block pairs plus a 5-byte CFB tail
        },
        Record {
            id: 0x1400,
            encrypted: true,
            key_index: 3,
            data: noise(15, 2), // shorter than one block: CFB over the IV
        },
        Record::plain(0x400, &[0xee; 64]), // container internal, never listed
    ]
}

fn sample() -> Vec<u8> {
    let nodes = app_nodes(
        &[
            ("eboot.bin", b"ELF?"),
            ("data/big.bin", &big()),
            ("data/empty.bin", b""),
            ("Data2/x", b"x"),
        ],
        &["data/void"],
    );
    game_package(CID, &nodes, &records())
}

fn game(data: &[u8]) -> Result<Game<Cursor<&[u8]>>, Error> {
    Package::open(Cursor::new(data), L)?.into_game()
}

fn read_all(g: &mut Game<Cursor<&[u8]>>, path: &str) -> Vec<u8> {
    let i = g.entries().iter().position(|e| e.path == path).unwrap();
    let mut out = vec![0; g.entries()[i].size as usize];
    g.read_at(i, 0, &mut out).unwrap();
    out
}

#[test]
fn full_key_chain_tree_and_reads() {
    let data = sample();
    let mut g = game(&data).unwrap();
    let listed: Vec<(&str, FileKind, u64)> = g
        .entries()
        .iter()
        .map(|e| (e.path.as_str(), e.kind(), e.size))
        .collect();
    use FileKind::{Directory as D, File as F};
    assert_eq!(
        listed,
        [
            ("data", D, 0),
            ("data/big.bin", F, 1_100_000),
            ("data/empty.bin", F, 0),
            ("data/void", D, 0),
            ("Data2", D, 0),
            ("Data2/x", F, 1),
            ("eboot.bin", F, 4),
            ("sce_sys", D, 0),
            ("sce_sys/icon0.png", F, 37),
            ("sce_sys/param.sfo", F, 14),
            ("sce_sys/trophy", D, 0),
            ("sce_sys/trophy/trophy00.trp", F, 15),
        ]
    );
    assert_eq!(read_all(&mut g, "data/big.bin"), big());
    assert_eq!(read_all(&mut g, "eboot.bin"), b"ELF?");
    assert_eq!(read_all(&mut g, "sce_sys/param.sfo"), b"\0PSF synthetic");
    assert_eq!(read_all(&mut g, "sce_sys/icon0.png"), noise(37, 1));
    assert_eq!(
        read_all(&mut g, "sce_sys/trophy/trophy00.trp"),
        noise(15, 2)
    );
    assert_eq!(g.entries()[8].record_id(), Some(0x1200));
}

#[test]
fn every_subrange_of_an_encrypted_record_decrypts() {
    let data = sample();
    let mut g = game(&data).unwrap();
    let i = g
        .entries()
        .iter()
        .position(|e| e.path == "sce_sys/icon0.png")
        .unwrap();
    let plain = noise(37, 1);
    for start in 0..plain.len() {
        for end in start..=plain.len() {
            let mut buf = vec![0; end - start];
            g.read_at(i, start as u64, &mut buf).unwrap();
            assert_eq!(buf, plain[start..end], "{start}..{end}");
        }
    }
}

#[test]
fn cross_block_reads_of_inner_files() {
    let data = sample();
    let mut g = game(&data).unwrap();
    let i = g
        .entries()
        .iter()
        .position(|e| e.path == "data/big.bin")
        .unwrap();
    let want = big();
    for start in [0usize, 65_530, 65_536, 131_000, 1_099_990] {
        let mut buf = vec![0; 10.min(want.len() - start)];
        g.read_at(i, start as u64, &mut buf).unwrap();
        assert_eq!(buf, want[start..start + buf.len()]);
    }
    let mut over = [0u8; 11];
    assert!(g.read_at(i, 1_099_990, &mut over).is_err());
    assert!(
        g.read_at(0, 0, &mut [0u8; 1]).is_err(),
        "directories are not files"
    );
}

fn kind_of(data: &[u8]) -> ErrorKind {
    match game(data) {
        Ok(_) => panic!("accepted"),
        Err(e) => e.kind(),
    }
}

fn with_nodes(nodes: &[PfsNode], records: &[Record]) -> Vec<u8> {
    game_package(CID, nodes, records)
}

#[test]
fn rejects_unsafe_and_unsupported_trees() {
    let n = |files: &[(&str, &[u8])]| app_nodes(files, &[]);
    let collide = with_nodes(&n(&[("a/x", b""), ("A/y", b"")]), &[]);
    assert_eq!(kind_of(&collide), ErrorKind::UnsafeOutput);
    let ascii = with_nodes(&n(&[("é", b"")]), &[]);
    assert_eq!(kind_of(&ascii), ErrorKind::Unsupported);
    for bad in [
        "CON",
        "end.",
        ".pantheon-incomplete",
        ".PKG-TOOLS-INCOMPLETE",
        "a:b",
    ] {
        let pkg = with_nodes(&n(&[(bad, b"")]), &[]);
        assert_eq!(kind_of(&pkg), ErrorKind::UnsafeOutput, "{bad}");
    }
    // A PFS file that collides with a reconstructed sce_sys record.
    let dup = with_nodes(&n(&[("sce_sys/param.sfo", b"")]), &records());
    assert_eq!(kind_of(&dup), ErrorKind::UnsafeOutput);
    // Per-file compression.
    let mut nodes = n(&[("z", b"abc")]);
    if let PfsNode::File { flags, .. } = nodes.last_mut().unwrap() {
        *flags = 1;
    }
    assert_eq!(kind_of(&with_nodes(&nodes, &[])), ErrorKind::Unsupported);
    // A directory cycle: uroot lists itself.
    let mut nodes = n(&[]);
    nodes[1] = PfsNode::Dir(vec![("loop".into(), 1)]);
    assert_eq!(kind_of(&with_nodes(&nodes, &[])), ErrorKind::Malformed);
}

#[test]
fn rejects_unknown_records_and_foreign_keys() {
    let nodes = app_nodes(&[], &[]);
    let unknown = with_nodes(&nodes, &[Record::plain(0x1fff, b"?")]);
    assert_eq!(kind_of(&unknown), ErrorKind::Unsupported);
    let other_key = Record {
        id: 0x1000,
        encrypted: true,
        key_index: 2,
        data: vec![0; 16],
    };
    assert_eq!(
        kind_of(&with_nodes(&nodes, &[other_key])),
        ErrorKind::KeyUnavailable
    );
}

/// Offset of record `id`'s payload in a synthetic package.
fn record_offset(data: &[u8], id: u32) -> usize {
    let pkg = Package::open(Cursor::new(data), L).unwrap();
    pkg.entry(id).unwrap().offset as usize
}

#[test]
fn rejects_damaged_keys_images_and_headers() {
    let good = sample();
    // DK3 digest.
    let mut bad = good.clone();
    bad[record_offset(&good, 0x10) + 32 + 3 * 32] ^= 1;
    assert_eq!(kind_of(&bad), ErrorKind::Malformed);
    // Wrapped DK3.
    let mut bad = good.clone();
    bad[record_offset(&good, 0x10) + 256 + 3 * 256 + 7] ^= 1;
    assert_eq!(kind_of(&bad), ErrorKind::KeyUnavailable);
    // Image key ciphertext.
    let mut bad = good.clone();
    bad[record_offset(&good, 0x20) + 200] ^= 1;
    assert_eq!(kind_of(&bad), ErrorKind::KeyUnavailable);
    // Outer header (HMAC-covered).
    let pfs = Package::open(Cursor::new(&good[..]), L)
        .unwrap()
        .header()
        .pfs_offset as usize;
    let mut bad = good.clone();
    bad[pfs + 0x100] ^= 1;
    assert_eq!(kind_of(&bad), ErrorKind::Malformed);
    // Non-GD content.
    let mut bad = good.clone();
    bad[0x74..0x78].copy_from_slice(&0x1bu32.to_be_bytes());
    assert_eq!(kind_of(&bad), ErrorKind::Unsupported);
}

#[test]
fn corrupted_images_never_panic() {
    let nodes = app_nodes(&[("a/b", &noise(70_000, 3)), ("c", b"c")], &["e"]);
    let data = game_package(CID, &nodes, &records());
    let pfs = Package::open(Cursor::new(&data[..]), L)
        .unwrap()
        .header()
        .pfs_offset as usize;
    let mut rng = SplitMix64::new(0xbad);
    for _ in 0..32 {
        let mut bad = data.clone();
        let at = pfs + rng.below(data.len() - pfs);
        bad[at] ^= 1 << rng.below(8);
        if let Ok(mut g) = game(&bad) {
            for i in 0..g.entries().len() {
                let mut buf = vec![0; g.entries()[i].size.min(1 << 20) as usize];
                let _ = g.read_at(i, 0, &mut buf);
            }
        }
    }
    // Header and record-table mutations; truncation is covered by `tests.rs`.
    let mut c = Corruptor::new(0x5eed);
    for _ in 0..16 {
        let head = c.mutate(&data[..0x2000]);
        let n = head.len().min(0x2000);
        let mut bad = data.clone();
        bad[..n].copy_from_slice(&head[..n]);
        let _ = game(&bad);
    }
}

/// A sparse image: absent blocks read as zeros.
struct Sparse {
    len: u64,
    blocks: HashMap<u64, Vec<u8>>,
}

impl ReadAt for Sparse {
    fn len(&self) -> u64 {
        self.len
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        assert!(offset + buf.len() as u64 <= self.len);
        for (k, b) in buf.iter_mut().enumerate() {
            let at = offset + k as u64;
            *b = self
                .blocks
                .get(&(at / BLOCK))
                .map_or(0, |v| v[(at % BLOCK) as usize]);
        }
        Ok(())
    }
}

fn put32(s: &mut Sparse, block: u64, at: usize, v: u32) {
    let b = s
        .blocks
        .entry(block)
        .or_insert_with(|| vec![0; BLOCK as usize]);
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(s: &mut Sparse, block: u64, at: usize, v: u64) {
    let b = s
        .blocks
        .entry(block)
        .or_insert_with(|| vec![0; BLOCK as usize]);
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

const R: u64 = BLOCK / 36;

/// Signed PFS whose inode 0 is a file of `blocks` blocks (references unset).
fn sparse_file(blocks: u32) -> Sparse {
    let mut s = Sparse {
        len: 200_000 * BLOCK,
        blocks: HashMap::new(),
    };
    put64(&mut s, 0, 0, 1);
    put64(&mut s, 0, 8, 20_130_315);
    put32(&mut s, 0, 0x1c, 1);
    put32(&mut s, 0, 0x20, BLOCK as u32);
    put64(&mut s, 0, 0x30, 1);
    put64(&mut s, 0, 0x40, 1);
    put32(&mut s, 1, 0, 0x81a4);
    put64(&mut s, 1, 8, u64::from(blocks) * BLOCK);
    put64(&mut s, 1, 16, u64::from(blocks) * BLOCK);
    put32(&mut s, 1, 96, blocks);
    s
}

fn slot(i: usize) -> usize {
    100 + i * 36 + 32
}

#[test]
fn double_indirect_mapping_beyond_4_gib() {
    let blocks = 65_537u32;
    let mut s = sparse_file(blocks);
    // Direct 0, single indirect 12, double indirect for the last two blocks.
    put32(&mut s, 1, slot(0), 10);
    put32(&mut s, 1, slot(1), 11);
    put32(&mut s, 1, slot(12), 20);
    put32(&mut s, 20, 32, 21);
    put32(&mut s, 1, slot(13), 30);
    let (j, k) = ((65_535 - 12 - R) / R, (65_535 - 12 - R) % R);
    put32(&mut s, 30, (j * 36 + 32) as usize, 31);
    put32(&mut s, 31, (k * 36 + 32) as usize, 150_000);
    put32(&mut s, 31, ((k + 1) * 36 + 32) as usize, 150_001);
    s.blocks.insert(10, vec![0xa1; BLOCK as usize]);
    s.blocks.insert(21, vec![0xb2; BLOCK as usize]);
    s.blocks.insert(150_000, vec![0xc3; BLOCK as usize]);
    s.blocks.insert(150_001, vec![0xd4; BLOCK as usize]);
    let mut pfs = Pfs::open(s, None, L).unwrap();
    let inode = pfs.inode(0).unwrap();
    let mut b = [0u8; 2];
    pfs.read_file(&inode, 0, &mut b).unwrap();
    assert_eq!(b, [0xa1; 2]);
    pfs.read_file(&inode, 12 * BLOCK, &mut b).unwrap();
    assert_eq!(b, [0xb2; 2]);
    let past_4_gib = 65_536 * BLOCK;
    assert!(past_4_gib > u64::from(u32::MAX));
    pfs.read_file(&inode, past_4_gib - 1, &mut b).unwrap();
    assert_eq!(b, [0xc3, 0xd4]);
    // An unset reference (block 0) is refused, never read.
    assert!(pfs.read_file(&inode, 2 * BLOCK, &mut b).is_err());
}

#[test]
fn triple_indirection_and_unsigned_fragments_are_unsupported() {
    let too_many = (12 + R + R * R + 1) as u32;
    let mut s = sparse_file(too_many);
    put32(&mut s, 1, slot(1), 11);
    let mut pfs = Pfs::open(s, None, L).unwrap();
    assert!(matches!(pfs.inode(0), Err(Error::Unsupported(_))));

    let mut s = sparse_file(3);
    put32(&mut s, 0, 0x1c, 0); // unsigned
    put32(&mut s, 1, 100, 10);
    put32(&mut s, 1, 104, 12); // refs[1] set: fragmented
    let mut pfs = Pfs::open(s, None, L).unwrap();
    assert!(matches!(pfs.inode(0), Err(Error::Unsupported(_))));
}

#[test]
fn record_names_follow_the_map() {
    assert_eq!(record_name(0x1000).as_deref(), Some("param.sfo"));
    assert_eq!(record_name(0x1201).as_deref(), Some("icon0_00.png"));
    assert_eq!(record_name(0x121f).as_deref(), Some("icon0_30.png"));
    assert_eq!(record_name(0x1220).as_deref(), Some("pic0.png"));
    assert_eq!(record_name(0x1463).as_deref(), Some("trophy/trophy99.trp"));
    assert_eq!(record_name(0x1609).as_deref(), Some("keymap_rp/010.png"));
    assert_eq!(record_name(0x1610).as_deref(), Some("keymap_rp/00/001.png"));
    assert_eq!(record_name(0x160a), None);
    assert_eq!(record_name(0x400), None);
    assert_eq!(record_name(0x10), None);
}
