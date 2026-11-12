use pantheon_core::testing::Corruptor;
use pantheon_core::{Classify, ErrorKind, Limits};

use crate::*;

const L: &Limits = &Limits::DEFAULT;

fn entry<'a>(key: &'a [u8], format: u16, raw: &'a [u8], padding: &'a [u8]) -> Entry<'a> {
    Entry {
        key,
        format,
        value: value_from_raw(format, raw).expect("valid raw"),
        raw,
        max_len: u32::try_from(raw.len() + padding.len()).expect("small"),
        padding,
    }
}

fn sample() -> Vec<u8> {
    let sfo = Sfo {
        version: VERSION_1_1,
        entries: vec![
            entry(b"APP_VER", FORMAT_TEXT, b"01.00\0", b"\0\0"),
            entry(b"ATTRIBUTE", FORMAT_INTEGER, &[0x78, 0x56, 0x34, 0x12], b""),
            entry(b"BLOB", FORMAT_BINARY, b"\x00\xff\x10", b"\xaa"),
            entry(b"ODD", 0x1234, b"zz", b""),
            entry(b"TITLE", FORMAT_TEXT, b"T\xc3\xa9st\xff\0", b"\0\0\0\0"),
        ],
    };
    serialize(&sfo, L).expect("serialize")
}

#[test]
fn typed_values_and_unknown_bytes_survive() {
    let data = sample();
    let sfo = parse(&data, L).expect("parse");
    assert_eq!(sfo.version, VERSION_1_1);
    assert_eq!(sfo.text(b"APP_VER"), Some("01.00"));
    assert_eq!(sfo.get(b"APP_VER").map(|e| e.padding), Some(&b"\0\0"[..]));
    assert_eq!(
        sfo.get(b"ATTRIBUTE").map(|e| e.value),
        Some(Value::Integer(0x1234_5678))
    );
    assert_eq!(
        sfo.get(b"BLOB").map(|e| e.value),
        Some(Value::Binary(b"\x00\xff\x10"))
    );
    assert_eq!(
        sfo.get(b"ODD").map(|e| e.value),
        Some(Value::Unknown {
            format: 0x1234,
            bytes: b"zz"
        })
    );
    // Invalid UTF-8 text is kept as bytes and never decoded lossily.
    let title = sfo.get(b"TITLE").expect("title");
    assert_eq!(title.value, Value::Text(b"T\xc3\xa9st\xff"));
    assert_eq!(title.text(), None);
}

#[test]
fn serialize_reparse_is_identity_for_canonical_layout() {
    let data = sample();
    let sfo = parse(&data, L).expect("parse");
    assert_eq!(serialize(&sfo, L).expect("serialize"), data);
}

#[test]
fn wire_layout_is_aligned_with_zero_gaps() {
    let sfo = Sfo {
        version: VERSION_1_1,
        entries: vec![
            entry(b"A", FORMAT_BINARY, b"x", b""),
            entry(b"B", FORMAT_BINARY, b"y", b""),
        ],
    };
    let data = serialize(&sfo, L).expect("serialize");
    // Header 20 + index 32 = 52; keys "A\0B\0" end at 56; values at 56.
    assert_eq!(&data[8..12], &52u32.to_le_bytes());
    assert_eq!(&data[12..16], &56u32.to_le_bytes());
    // Second value starts at the next 4-byte boundary after a 1-byte value.
    assert_eq!(&data[20 + 16 + 12..20 + 16 + 16], &4u32.to_le_bytes());
    assert_eq!(&data[56..], b"x\0\0\0y");
}

#[test]
fn every_truncation_fails_without_panic() {
    let data = sample();
    for cut in Corruptor::truncations(&data) {
        if cut.len() < data.len() {
            assert!(parse(cut, L).is_err(), "truncated at {}", cut.len());
        }
    }
}

fn patched(at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut data = sample();
    data[at..at + bytes.len()].copy_from_slice(bytes);
    data
}

fn kind_of(data: &[u8]) -> ErrorKind {
    parse(data, L).expect_err("must fail").kind()
}

#[test]
fn invalid_headers_tables_and_aliases_fail() {
    assert_eq!(kind_of(&patched(0, b"\0PSG")), ErrorKind::Malformed);
    assert_eq!(
        kind_of(&patched(4, &0x102u32.to_le_bytes())),
        ErrorKind::Unsupported
    );
    // Key table before the end of the index.
    assert_eq!(
        kind_of(&patched(8, &4u32.to_le_bytes())),
        ErrorKind::Malformed
    );
    // Value table past the input.
    assert_eq!(
        kind_of(&patched(12, &u32::MAX.to_le_bytes())),
        ErrorKind::Malformed
    );
    // Used length above capacity (first record).
    assert_eq!(
        kind_of(&patched(24, &100u32.to_le_bytes())),
        ErrorKind::Malformed
    );
    // Second value aliasing the first.
    assert_eq!(
        kind_of(&patched(20 + 16 + 12, &0u32.to_le_bytes())),
        ErrorKind::Malformed
    );
}

#[test]
fn duplicate_empty_and_unterminated_keys_fail() {
    // Point the second record's key at the first key.
    assert!(matches!(
        parse(&patched(20 + 16, &0u16.to_le_bytes()), L),
        Err(Error::Malformed("empty or duplicate key"))
    ));
    // Point a key at the final NUL of the key table (an empty key).
    let data = sample();
    let keys = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    let values = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
    let last_nul = data[keys..values].iter().rposition(|b| *b == 0).unwrap() as u16;
    assert!(parse(&patched(20, &last_nul.to_le_bytes()), L).is_err());
    // Unterminated text value: overwrite APP_VER's terminator and padding.
    let at = values;
    assert!(matches!(
        parse(&patched(at, b"01.0000x"), L),
        Err(Error::Malformed("unterminated text"))
    ));
}

#[test]
fn hostile_counts_and_sizes_hit_limits() {
    let mut data = sample();
    data[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(kind_of(&data), ErrorKind::Limit);
    let small = Limits::DEFAULT.lowered(|l| l.max_sfo = 16);
    assert_eq!(
        parse(&sample(), &small).map(|_| ()).unwrap_err().kind(),
        ErrorKind::Limit
    );
    let few = Limits::DEFAULT.lowered(|l| l.max_records = 2);
    assert_eq!(
        parse(&sample(), &few).map(|_| ()).unwrap_err().kind(),
        ErrorKind::Limit
    );
}

#[test]
fn version_1_0_reads_but_must_be_upgraded_to_serialize() {
    let data = patched(4, &VERSION_1_0.to_le_bytes());
    let mut sfo = parse(&data, L).expect("1.0 parses");
    assert_eq!(
        serialize(&sfo, L),
        Err(Error::UnsupportedVersion(VERSION_1_0))
    );
    sfo.version = VERSION_1_1;
    assert!(serialize(&sfo, L).is_ok());
}

#[test]
fn inconsistent_models_are_rejected() {
    let ok = entry(b"K", FORMAT_TEXT, b"v\0", b"");
    let cases = [
        Entry { key: b"", ..ok },
        Entry { key: b"K\0", ..ok },
        Entry { max_len: 9, ..ok },
        Entry {
            value: Value::Text(b"w"),
            ..ok
        },
        Entry {
            format: FORMAT_INTEGER,
            ..ok
        },
        Entry {
            raw: b"v",
            value: Value::Text(b"v"),
            max_len: 1,
            ..ok
        },
    ];
    for bad in cases {
        let sfo = Sfo {
            version: VERSION_1_1,
            entries: vec![bad],
        };
        assert!(serialize(&sfo, L).is_err(), "{bad:?}");
    }
    let dup = Sfo {
        version: VERSION_1_1,
        entries: vec![ok, ok],
    };
    assert!(serialize(&dup, L).is_err());
}

#[test]
fn key_offsets_must_fit_u16() {
    let keys: Vec<Vec<u8>> = (0..70u32)
        .map(|i| format!("{i:03}{}", "k".repeat(1000)).into_bytes())
        .collect();
    let entries = keys
        .iter()
        .map(|k| entry(k, FORMAT_BINARY, b"", b""))
        .collect();
    let sfo = Sfo {
        version: VERSION_1_1,
        entries,
    };
    assert_eq!(
        serialize(&sfo, L),
        Err(Error::Malformed("key offset exceeds u16"))
    );
}

#[test]
fn output_size_limit_covers_tables_and_padding() {
    let data = sample();
    let sfo = parse(&data, L).expect("parse");
    let exact = Limits::DEFAULT.lowered(|l| l.max_sfo = data.len() as u64);
    assert!(serialize(&sfo, &exact).is_ok());
    let short = Limits::DEFAULT.lowered(|l| l.max_sfo = data.len() as u64 - 1);
    assert_eq!(
        serialize(&sfo, &short).unwrap_err().kind(),
        ErrorKind::Limit
    );
}

#[test]
fn mutations_never_panic_and_reserialize() {
    let data = sample();
    Corruptor::new(0x5f0).run(&data, 4000, |input| {
        if let Ok(mut sfo) = parse(input, L) {
            sfo.version = VERSION_1_1;
            // Any parsed model must serialize and parse back to the same entries.
            let out = serialize(&sfo, L).expect("parsed models serialize");
            let again = parse(&out, L).expect("reparse");
            assert_eq!(again.entries, sfo.entries);
        }
    });
}
