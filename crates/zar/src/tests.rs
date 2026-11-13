//! Writer/reader round trips, random reads, a sparse > 4 GiB file, rejections,
//! cancel and corruption.

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use pantheon_core::testing::{Corruptor, SplitMix64};
use pantheon_core::{Cancel, Classify, Ctx, ErrorKind, Limits, NoProgress};
use pantheon_fs::{Access, OutputFile, Reason};

use crate::format::{BLOCK, FOOTER_LEN};
use crate::*;

const L: &Limits = &Limits::DEFAULT;

/// A fresh, canonical directory under the system temp dir; removed on success.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> TestDir {
        let mut bytes = [0u8; 8];
        pantheon_fs::os_random(&mut bytes).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = base.join(format!("pantheon-zar-test-{hex}"));
        std::fs::create_dir(&path).unwrap();
        TestDir(path)
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn noop() -> (Cancel, NoProgress) {
    (Cancel::new(), NoProgress)
}

fn writer(path: &Path) -> Writer {
    Writer::create_new(OutputFile::create_new(path, Access::Default).unwrap()).unwrap()
}

fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    SplitMix64::new(seed).fill(&mut out);
    out
}

fn repeat(len: usize) -> Vec<u8> {
    b"abc123".iter().copied().cycle().take(len).collect()
}

/// `(path, None)` for folders, `(path, Some(bytes))` for files; parents first.
type Tree = Vec<(String, Option<Vec<u8>>)>;

fn sample() -> Tree {
    let mut tree: Tree = vec![("Nested".into(), None), ("Nested/empty-dir".into(), None)];
    for (i, size) in [0, 1, 65_535, 65_536, 65_537, 19 * 65_536 + 97]
        .into_iter()
        .enumerate()
    {
        tree.push((format!("Nested/noise-{i}"), Some(noise(i as u64, size))));
        tree.push((format!("repeat-{i}"), Some(repeat(size))));
    }
    for name in ["a", "AA", "Z", "é", "Ω", &"n".repeat(MAX_NAME_BYTES)] {
        tree.push((name.to_owned(), Some(b"name test".to_vec())));
    }
    tree
}

fn pack(path: &Path, tree: &Tree) -> Summary {
    let (cancel, sink) = noop();
    let ctx = Ctx::new(&cancel, &sink);
    let mut w = writer(path);
    for (p, data) in tree {
        match data {
            None => w.add_directory(p).unwrap(),
            Some(d) => w
                .add_file(p, d.len() as u64, &mut Cursor::new(d), &ctx)
                .unwrap(),
        }
    }
    w.finish().unwrap()
}

fn open(path: &Path) -> Reader<File> {
    Reader::open(File::open(path).unwrap(), L).unwrap()
}

fn read_all(r: &mut Reader<File>, node: u32, size: u64) -> Vec<u8> {
    let mut out = vec![0u8; size as usize];
    assert_eq!(r.read_at(node, 0, &mut out).unwrap(), out.len());
    out
}

#[test]
fn round_trip_preserves_paths_kinds_sizes_and_bytes() {
    let dir = TestDir::new();
    let path = dir.0.join("test.zar");
    let tree = sample();
    let summary = pack(&path, &tree);
    assert_eq!(
        dir.names(),
        ["test.zar"],
        "no scratch or spill file is left"
    );
    assert_eq!((summary.files, summary.directories), (18, 2));
    assert_eq!(
        summary.archive_bytes,
        std::fs::metadata(&path).unwrap().len()
    );

    let mut r = open(&path);
    r.verify_digest().unwrap();
    let mut got: Tree = Vec::new();
    for e in r.entries().unwrap() {
        match e.kind {
            EntryKind::Directory => got.push((e.path, None)),
            EntryKind::File => {
                let bytes = read_all(&mut r, e.node, e.size);
                got.push((e.path, Some(bytes)));
            }
        }
    }
    let mut want = tree.clone();
    want.sort();
    got.sort();
    assert_eq!(got, want);

    // Lookup ignores ASCII case, as the upstream reader does.
    let node = r.lookup("NESTED/NOISE-5").unwrap();
    assert_eq!(r.file_size(node), Some(19 * 65_536 + 97));
    assert!(r.lookup("é").is_some(), "non-ASCII names match exactly");
    assert!(r.lookup("missing").is_none());
}

#[test]
fn archive_has_raw_and_compressed_blocks() {
    let dir = TestDir::new();
    let path = dir.0.join("test.zar");
    pack(&path, &sample());
    let bytes = std::fs::read(&path).unwrap();
    let footer: [u8; FOOTER_LEN] = bytes[bytes.len() - FOOTER_LEN..].try_into().unwrap();
    let (f, _, _) = format::Footer::decode(&footer).unwrap();
    let records = &bytes[f.records.offset as usize..(f.records.offset + f.records.size) as usize];
    let sizes: Vec<usize> = records
        .chunks(40)
        .flat_map(|r| {
            r[8..]
                .chunks(2)
                .map(|s| u16::from_be_bytes([s[0], s[1]]) as usize + 1)
        })
        .collect();
    assert!(sizes.contains(&BLOCK), "noise is stored raw");
    assert!(
        sizes.iter().any(|s| *s > 1 && *s < BLOCK),
        "repeats are compressed"
    );
    assert_eq!(f.data.offset, 0);
    assert_eq!(f.records.offset % 8, 0);
}

#[test]
fn random_reads_match_including_block_edges_and_eof() {
    let dir = TestDir::new();
    let path = dir.0.join("test.zar");
    let tree = sample();
    pack(&path, &tree);
    let mut r = open(&path);
    let mut rng = SplitMix64::new(0x1234_5678);
    for (p, data) in &tree {
        let Some(data) = data else { continue };
        let node = r.lookup(p).unwrap();
        let size = data.len();
        let mut checks = vec![(0, 131_079), (size, 1), (size.saturating_sub(1), 10)];
        checks.extend(
            [65_535, 65_536, 65_537, 2 * 65_536 - 1]
                .iter()
                .map(|&o| (o.min(size), 70_001)),
        );
        for _ in 0..40 {
            checks.push((rng.below(size + 1), 70_001));
        }
        for (offset, len) in checks {
            let mut buf = vec![0xeeu8; len];
            let n = r.read_at(node, offset as u64, &mut buf).unwrap();
            let want = &data[offset.min(size)..(offset + len).min(size)];
            assert_eq!(&buf[..n], want, "{p} at {offset}+{len}");
        }
    }
    let folder = r.lookup("Nested").unwrap();
    assert!(r.read_at(folder, 0, &mut [0; 4]).is_err());
}

#[test]
fn many_blocks_span_several_offset_records() {
    let dir = TestDir::new();
    let path = dir.0.join("big.zar");
    // 40 blocks of mixed content: three records, the last one partial.
    let data: Vec<u8> = (0..40)
        .flat_map(|i| {
            if i % 3 == 0 {
                noise(i, BLOCK)
            } else {
                repeat(BLOCK)
            }
        })
        .collect();
    let tree = vec![("x".to_owned(), Some(data.clone()))];
    pack(&path, &tree);
    let mut r = open(&path);
    let node = r.lookup("x").unwrap();
    assert_eq!(read_all(&mut r, node, data.len() as u64), data);
}

/// A sparse 4 GiB + 65,537-byte source, followed by a small file whose
/// payload starts beyond 4 GiB in the logical stream.
#[test]
fn file_over_4_gib_from_a_sparse_source() {
    const LARGE: u64 = (1 << 32) + 65_537;
    let dir = TestDir::new();
    let source = dir.0.join("sparse.bin");
    {
        let f = File::create(&source).unwrap();
        f.set_len(LARGE).unwrap();
    }
    let path = dir.0.join("large.zar");
    let (cancel, sink) = noop();
    let ctx = Ctx::new(&cancel, &sink);
    let mut w = writer(&path);
    let mut src = File::open(&source).unwrap();
    w.add_file("Large", LARGE, &mut src, &ctx).unwrap();
    w.add_file("after", 5, &mut &b"tail!"[..], &ctx).unwrap();
    let summary = w.finish().unwrap();
    assert_eq!(summary.bytes, LARGE + 5);

    let mut r = open(&path);
    r.verify_digest().unwrap();
    let entries = r.entries().unwrap();
    assert_eq!(entries.len(), 2);
    let large = r.lookup("Large").unwrap();
    assert_eq!(r.file_size(large), Some(LARGE));
    let mut buf = vec![1u8; 100_000];
    for offset in [0, (1 << 32) - 50_000, LARGE - 100_000] {
        assert_eq!(r.read_at(large, offset, &mut buf).unwrap(), buf.len());
        assert!(buf.iter().all(|b| *b == 0));
    }
    assert_eq!(r.read_at(large, LARGE - 3, &mut buf).unwrap(), 3);
    let after = r.lookup("after").unwrap();
    let mut tail = [0u8; 5];
    assert_eq!(r.read_at(after, 0, &mut tail).unwrap(), 5);
    assert_eq!(&tail, b"tail!");
}

fn assert_nothing_published(dir: &TestDir) {
    assert!(dir.names().is_empty(), "left behind: {:?}", dir.names());
}

#[test]
fn name_rules_are_enforced() {
    let dir = TestDir::new();
    let (cancel, sink) = noop();
    let ctx = Ctx::new(&cancel, &sink);
    let long = "n".repeat(MAX_NAME_BYTES + 1);
    let cases: Vec<(&str, ErrorKind)> = vec![
        (&long, ErrorKind::Unsupported),
        ("", ErrorKind::UnsafeOutput),
        ("..", ErrorKind::UnsafeOutput),
        ("a\\b", ErrorKind::UnsafeOutput),
        ("a/../b", ErrorKind::UnsafeOutput),
        ("nul\0", ErrorKind::UnsafeOutput),
        ("missing/child", ErrorKind::Unsupported),
    ];
    for (name, kind) in cases {
        let mut w = writer(&dir.0.join("x.zar"));
        let err = w.add_file(name, 1, &mut &b"x"[..], &ctx).unwrap_err();
        assert_eq!(err.kind(), kind, "{name:?}: {err}");
        assert!(matches!(w.finish(), Err(Error::Poisoned)), "{name:?}");
    }
    assert!(check_path(&"n".repeat(MAX_NAME_BYTES)).is_ok());
    assert_nothing_published(&dir);
}

#[test]
fn duplicates_and_case_collisions_are_refused() {
    let dir = TestDir::new();
    let (cancel, sink) = noop();
    let ctx = Ctx::new(&cancel, &sink);
    for (first, second, reason) in [
        ("dir", "dir", Reason::Exists),
        ("dir", "DIR", Reason::CaseCollision),
        ("dir", "Dir", Reason::CaseCollision),
    ] {
        let mut w = writer(&dir.0.join("x.zar"));
        w.add_directory(first).unwrap();
        let err = w.add_file(second, 1, &mut &b"x"[..], &ctx).unwrap_err();
        assert!(
            matches!(&err, Error::Name { reason: r, .. } if *r == reason),
            "{second}: {err}"
        );
    }
    // A file is not a parent.
    let mut w = writer(&dir.0.join("x.zar"));
    w.add_file("f", 1, &mut &b"x"[..], &ctx).unwrap();
    assert!(matches!(
        w.add_directory("f/sub"),
        Err(Error::MissingParent { .. })
    ));
    drop(w);
    assert_nothing_published(&dir);
}

#[test]
fn all_empty_payload_is_refused() {
    let dir = TestDir::new();
    let (cancel, sink) = noop();
    let ctx = Ctx::new(&cancel, &sink);
    let mut w = writer(&dir.0.join("x.zar"));
    w.add_directory("d").unwrap();
    w.add_file("d/empty", 0, &mut &b""[..], &ctx).unwrap();
    assert!(matches!(w.finish(), Err(Error::EmptyPayload)));
    let w = writer(&dir.0.join("y.zar"));
    assert_eq!(w.finish().unwrap_err().kind(), ErrorKind::Unsupported);
    assert_nothing_published(&dir);
}

#[test]
fn reader_yielding_the_wrong_size_poisons_the_writer() {
    let dir = TestDir::new();
    let (cancel, sink) = noop();
    let ctx = Ctx::new(&cancel, &sink);
    for data in [&b"shor"[..], &b"longer"[..]] {
        let mut w = writer(&dir.0.join("x.zar"));
        let err = w.add_file("f", 5, &mut &data[..], &ctx).unwrap_err();
        assert!(
            matches!(err, Error::SizeMismatch { expected: 5, .. }),
            "{err}"
        );
        assert!(matches!(
            w.add_file("g", 1, &mut &b"x"[..], &ctx),
            Err(Error::Poisoned)
        ));
        assert!(matches!(w.finish(), Err(Error::Poisoned)));
    }
    assert_nothing_published(&dir);
}

#[test]
fn cancel_stops_at_a_block_and_removes_scratch() {
    struct CancelAfter<'a>(&'a Cancel, usize);
    impl Read for CancelAfter<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.1 += buf.len();
            if self.1 > 3 * BLOCK {
                self.0.cancel();
            }
            buf.fill(7);
            Ok(buf.len())
        }
    }
    let dir = TestDir::new();
    let cancel = Cancel::new();
    let ctx = Ctx::new(&cancel, &NoProgress);
    let mut w = writer(&dir.0.join("x.zar"));
    let err = w
        .add_file("f", 100 * BLOCK as u64, &mut CancelAfter(&cancel, 0), &ctx)
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Cancelled);
    assert!(
        !dir.names().is_empty(),
        "scratch exists while the writer lives"
    );
    drop(w);
    assert_nothing_published(&dir);
}

#[test]
fn reader_rejects_damage_without_panicking() {
    let dir = TestDir::new();
    let path = dir.0.join("x.zar");
    pack(
        &path,
        &vec![
            ("d".to_owned(), None),
            ("d/f".to_owned(), Some(repeat(70_000))),
        ],
    );
    let bytes = std::fs::read(&path).unwrap();
    for prefix in Corruptor::truncations(&bytes).take(64) {
        assert!(Reader::open(Cursor::new(prefix), L).is_err());
    }
    let mut flipped = bytes.clone();
    flipped[10] ^= 1;
    let mut r = Reader::open(Cursor::new(flipped), L).unwrap();
    assert!(r.verify_digest().is_err());
    Corruptor::new(0x2a5).run(&bytes, 256, |input| {
        if let Ok(mut r) = Reader::open(Cursor::new(input), L) {
            if let Ok(entries) = r.entries() {
                for e in entries.iter().filter(|e| e.kind == EntryKind::File) {
                    let mut buf = vec![0u8; 1024];
                    let _ = r.read_at(e.node, 0, &mut buf);
                }
            }
            let _ = r.verify_digest();
        }
    });
}
