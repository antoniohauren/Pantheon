use std::io::Cursor;

use pantheon_core::testing::Corruptor;
use pantheon_core::{Classify, ErrorKind, Limits};

use crate::testing::{Record, package};
use crate::*;

const L: &Limits = &Limits::DEFAULT;

fn sample() -> Vec<u8> {
    package(
        "UP0000-TEST00000_00-0000000000000000",
        &[
            Record::plain(0x1, &[1; 40]),
            Record::plain(id::PARAM_SFO, b"\0PSF-not-really"),
            Record::plain(id::ICON0, b"\x89PNG"),
            Record {
                id: id::PIC1,
                encrypted: true,
                key_index: 3,
                data: vec![9; 8],
            },
        ],
    )
}

fn open(data: &[u8]) -> Result<Package<Cursor<&[u8]>>, Error> {
    Package::open(Cursor::new(data), L)
}

#[test]
fn header_records_and_plaintext_reads() {
    let data = sample();
    let mut pkg = open(&data).expect("open");
    let h = pkg.header().clone();
    assert_eq!(
        h.content_id_bytes(),
        b"UP0000-TEST00000_00-0000000000000000"
    );
    assert_eq!(h.package_size, data.len() as u64);
    assert_eq!(h.entry_count, 4);
    assert_eq!(pkg.entries().len(), 4);
    assert_eq!(
        pkg.read_param_sfo().expect("sfo").as_deref(),
        Some(&b"\0PSF-not-really"[..])
    );
    assert_eq!(
        pkg.read_icon0().expect("icon").as_deref(),
        Some(&b"\x89PNG"[..])
    );
    match pkg.read_pic1() {
        Err(Error::DecryptionUnavailable {
            entry_id,
            key_index,
        }) => {
            assert_eq!((entry_id, key_index), (id::PIC1, 3));
        }
        other => panic!("expected DecryptionUnavailable, got {other:?}"),
    }
    assert!(
        pkg.read_plaintext(0x9999, pantheon_core::Limit::new("x", 1))
            .expect("absent")
            .is_none()
    );
}

#[test]
fn missing_sfo_is_none() {
    let data = package("X", &[Record::plain(0x1, b"a")]);
    assert_eq!(
        open(&data).expect("open").read_param_sfo().expect("read"),
        None
    );
}

#[test]
fn artwork_respects_limit() {
    let data = package("X", &[Record::plain(id::ICON0, &[0; 64])]);
    let small = Limits::DEFAULT.lowered(|l| l.max_artwork = 63);
    let mut pkg = Package::open(Cursor::new(&data[..]), &small).expect("open");
    assert_eq!(pkg.read_icon0().unwrap_err().kind(), ErrorKind::Limit);
}

fn patched(at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut data = sample();
    data[at..at + bytes.len()].copy_from_slice(bytes);
    data
}

#[test]
fn rejects_bad_layouts() {
    let kind = |d: &[u8]| open(d).unwrap_err().kind();
    assert_eq!(kind(&patched(0, b"\x7fPKG")), ErrorKind::Malformed);
    assert_eq!(
        kind(&patched(0x430, &1u64.to_be_bytes())),
        ErrorKind::Malformed
    );
    assert_eq!(
        kind(&patched(0x18, &0x100u32.to_be_bytes())),
        ErrorKind::Malformed
    );
    assert_eq!(
        kind(&patched(0x10, &u32::MAX.to_be_bytes())),
        ErrorKind::Limit
    );
    assert_eq!(
        kind(&patched(0x410, &u64::MAX.to_be_bytes())),
        ErrorKind::Malformed
    );
    // Duplicate record ID: second record takes the first one's ID.
    assert_eq!(
        kind(&patched(0x1100 + 32, &1u32.to_be_bytes())),
        ErrorKind::Malformed
    );
    // Record payload inside the header.
    assert_eq!(
        kind(&patched(0x1100 + 16, &0x10u32.to_be_bytes())),
        ErrorKind::Malformed
    );
    // Record payload past the end.
    assert_eq!(
        kind(&patched(0x1100 + 20, &u32::MAX.to_be_bytes())),
        ErrorKind::Malformed
    );
}

#[test]
fn truncations_and_mutations_never_panic() {
    let data = sample();
    for cut in Corruptor::truncations(&data) {
        if cut.len() < data.len() {
            assert!(open(cut).is_err());
        }
    }
    Corruptor::new(0x954).run(&data, 3000, |input| {
        if let Ok(mut pkg) = open(input) {
            let _ = pkg.read_param_sfo();
            let _ = pkg.read_icon0();
            let _ = pkg.read_pic1();
        }
    });
}
