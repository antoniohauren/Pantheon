use std::io::Cursor;

use pantheon_core::testing::Corruptor;
use pantheon_core::{Classify, ErrorKind, Limits};

use crate::testing::{Item, package};
use crate::*;

const L: &Limits = &Limits::DEFAULT;
const CID: &str = "UP0001-NPUB00000_00-0000000000000001";

fn items() -> Vec<Item> {
    vec![
        Item::file("PARAM.SFO", b"\0PSF fake sfo bytes"),
        Item::file("ICON0.PNG", &[0x89; 100_000]),
        Item::file("USRDIR/EBOOT.BIN", b"self"),
        Item::file("USRDIR/data/a.dat", b"aaaa"),
        Item::dir("USRDIR"),
    ]
}

fn open(data: &[u8]) -> Result<Package<Cursor<&[u8]>>, Error> {
    Package::open(Cursor::new(data), L)
}

#[test]
fn retail_and_debug_trees_metadata_and_reads() {
    for retail in [true, false] {
        let data = package(retail, CID, &[(1, &[0, 0, 0, 3]), (7, &[7; 48])], &items());
        let mut pkg = open(&data).expect("open");
        assert_eq!(pkg.header().content_id_bytes(), CID.as_bytes());
        assert_eq!(pkg.header().is_retail(), retail);
        assert_eq!(
            pkg.metadata(),
            &[
                Metadata {
                    id: 1,
                    data: vec![0, 0, 0, 3]
                },
                Metadata {
                    id: 7,
                    data: vec![7; 48]
                },
            ]
        );
        let paths: Vec<_> = pkg
            .entries()
            .iter()
            .map(|e| (e.path.as_str(), e.kind()))
            .collect();
        assert_eq!(
            paths,
            [
                ("ICON0.PNG", FileKind::File),
                ("PARAM.SFO", FileKind::File),
                ("USRDIR", FileKind::Directory),
                ("USRDIR/data", FileKind::Directory),
                ("USRDIR/data/a.dat", FileKind::File),
                ("USRDIR/EBOOT.BIN", FileKind::File),
            ]
        );
        // The explicit USRDIR record (offset 0, flags 4) wins over the implied one.
        assert_eq!(
            pkg.read_param_sfo().expect("sfo").as_deref(),
            Some(&b"\0PSF fake sfo bytes"[..])
        );
        // Spans several 64 KiB chunks at an unaligned start.
        assert_eq!(pkg.read_icon0().expect("icon"), Some(vec![0x89; 100_000]));
        let mut buf = [0; 3];
        let at = pkg.entries()[5].offset + 1;
        pkg.read_at(at, &mut buf).expect("unaligned");
        assert_eq!(&buf, b"elf");
    }
}

#[test]
fn absent_sfo_and_artwork_limit() {
    let data = package(true, CID, &[], &[Item::file("ICON0.PNG", &[1; 64])]);
    let small = Limits::DEFAULT.lowered(|l| l.max_artwork = 63);
    let mut pkg = Package::open(Cursor::new(&data[..]), &small).expect("open");
    assert_eq!(pkg.read_param_sfo().expect("read"), None);
    assert_eq!(pkg.read_icon0().unwrap_err().kind(), ErrorKind::Limit);
}

#[test]
fn rejects_variants_and_unsafe_paths() {
    let kind = |items: &[Item]| open(&package(true, CID, &[], items)).unwrap_err().kind();
    assert_eq!(kind(&[Item::file("a/../b", b"x")]), ErrorKind::UnsafeOutput);
    assert_eq!(
        kind(&[Item::file(".pantheon-incomplete", b"x")]),
        ErrorKind::UnsafeOutput
    );
    assert_eq!(
        kind(&[Item::file("A", b"x"), Item::file("a", b"y")]),
        ErrorKind::UnsafeOutput
    );
    assert_eq!(
        kind(&[Item::file("d/x", b"x"), Item::file("D", b"y")]),
        ErrorKind::UnsafeOutput
    );
    assert_eq!(
        kind(&[Item {
            flags: 5,
            ..Item::file("x", b"x")
        }]),
        ErrorKind::Unsupported
    );
    assert_eq!(
        kind(&[Item {
            flags: 4,
            ..Item::file("x", b"x")
        }]),
        ErrorKind::Malformed
    );
    // The NPDRM compatibility flag is accepted on kind 1 only.
    assert!(
        open(&package(
            true,
            CID,
            &[],
            &[Item {
                flags: 0x101,
                ..Item::file("x", b"x")
            }]
        ))
        .is_ok()
    );
    assert_eq!(
        kind(&[Item {
            flags: 0x103,
            ..Item::file("x", b"x")
        }]),
        ErrorKind::Unsupported
    );
}

fn patched(at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut data = package(true, CID, &[(1, &[1; 4])], &items());
    data[at..at + bytes.len()].copy_from_slice(bytes);
    data
}

#[test]
fn rejects_bad_headers() {
    let kind = |d: &[u8]| open(d).unwrap_err().kind();
    assert_eq!(kind(&patched(0, b"\x7fCNT")), ErrorKind::Malformed);
    assert_eq!(
        kind(&patched(4, &1u16.to_be_bytes())),
        ErrorKind::Unsupported
    );
    assert_eq!(
        kind(&patched(6, &2u16.to_be_bytes())),
        ErrorKind::Unsupported
    );
    assert_eq!(
        kind(&patched(24, &1u64.to_be_bytes())),
        ErrorKind::Malformed
    );
    assert_eq!(
        kind(&patched(32, &0x10u64.to_be_bytes())),
        ErrorKind::Malformed
    );
    assert_eq!(
        kind(&patched(20, &u32::MAX.to_be_bytes())),
        ErrorKind::Malformed
    );
    assert_eq!(
        kind(&patched(12, &u32::MAX.to_be_bytes())),
        ErrorKind::Limit
    );
    // A second metadata packet that does not fit.
    assert_eq!(
        kind(&patched(12, &2u32.to_be_bytes())),
        ErrorKind::Malformed
    );
}

#[test]
fn truncations_and_mutations_never_panic() {
    // Small payloads: every mutation round decrypts them with the debug SHA-1 scheme.
    let small = [
        Item::file("PARAM.SFO", b"\0PSF"),
        Item::file("ICON0.PNG", &[0x89; 64]),
        Item::file("USRDIR/EBOOT.BIN", b"self"),
    ];
    let data = package(false, CID, &[(1, &[1; 4])], &small);
    for cut in Corruptor::truncations(&data) {
        if cut.len() < data.len() {
            assert!(open(cut).is_err());
        }
    }
    Corruptor::new(0x953).run(&data, 3000, |input| {
        if let Ok(mut pkg) = open(input) {
            let _ = pkg.read_param_sfo();
            let _ = pkg.read_icon0();
        }
    });
}
