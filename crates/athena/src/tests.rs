use std::ffi::OsString;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use pantheon_args::Io;
use pantheon_core::testing::Corruptor;
use pantheon_core::{Classify, ErrorKind, Limits};
use pantheon_ps3::testing::{self as ps3t, Item};
use pantheon_ps4::testing::{self as ps4t, Record};
use pantheon_psf::{Entry, FORMAT_BINARY, FORMAT_INTEGER, FORMAT_TEXT, Sfo, VERSION_1_1, Value};

use crate::*;

const L: &Limits = &Limits::DEFAULT;

fn sfo(category: &[u8]) -> Vec<u8> {
    let mut cat = category.to_vec();
    cat.push(0);
    let e = |key: &'static [u8], format, raw: &'static [u8], value| Entry {
        key,
        format,
        value,
        raw,
        max_len: raw.len() as u32,
        padding: b"",
    };
    let cat: &'static [u8] = Box::leak(cat.into_boxed_slice());
    let entries = vec![
        e(b"APP_VER", FORMAT_TEXT, b"01.02\0", Value::Text(b"01.02")),
        e(
            b"ATTRIBUTE",
            FORMAT_INTEGER,
            &[1, 0, 0, 0],
            Value::Integer(1),
        ),
        e(
            b"CATEGORY",
            FORMAT_TEXT,
            cat,
            Value::Text(&cat[..cat.len() - 1]),
        ),
        e(
            b"PUBTOOLINFO",
            FORMAT_BINARY,
            &[0xde, 0xad],
            Value::Binary(&[0xde, 0xad]),
        ),
        e(
            b"TITLE",
            FORMAT_TEXT,
            b"Bad \xff\0",
            Value::Text(b"Bad \xff"),
        ),
        e(
            b"TITLE_ID",
            FORMAT_TEXT,
            b"CUSA00001\0",
            Value::Text(b"CUSA00001"),
        ),
    ];
    pantheon_psf::serialize(
        &Sfo {
            version: VERSION_1_1,
            entries,
        },
        L,
    )
    .expect("sfo")
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\x02\0\0\0\x02\0";

fn ps4_pkg(encrypted_sfo: bool) -> Vec<u8> {
    let mut sfo_rec = Record::plain(pantheon_ps4::id::PARAM_SFO, &sfo(b"gp"));
    sfo_rec.encrypted = encrypted_sfo;
    sfo_rec.key_index = 3;
    ps4t::package(
        "UP0000-CUSA00001_00-0000000000000000",
        &[
            Record::plain(0x1, &[0; 32]),
            sfo_rec,
            Record::plain(pantheon_ps4::id::ICON0, PNG),
        ],
    )
}

fn ps3_pkg(with_sfo: bool) -> Vec<u8> {
    let mut items = vec![
        Item::file("ICON0.PNG", PNG),
        Item::file("USRDIR/EBOOT.BIN", b"x"),
    ];
    if with_sfo {
        items.push(Item::file("PARAM.SFO", &sfo(b"HG")));
    }
    ps3t::package(
        true,
        "UP0001-NPUB00001_00-0000000000000001",
        &[(1, &[0, 0, 0, 2])],
        &items,
    )
}

#[test]
fn inspects_ps4_summary_and_details() {
    let r = inspect(Cursor::new(ps4_pkg(false)), L).expect("inspect");
    assert_eq!(r.kind, Kind::Ps4Pkg);
    assert_eq!(
        r.summary.title, None,
        "invalid UTF-8 is unknown, never lossy"
    );
    assert_eq!(r.summary.title_id.as_deref(), Some("CUSA00001"));
    assert_eq!(r.summary.app_version.as_deref(), Some("01.02"));
    assert_eq!(r.summary.content, ContentKind::Patch);
    assert_eq!(
        r.summary.content_id.as_deref(),
        Some("UP0000-CUSA00001_00-0000000000000000")
    );
    let SfoStatus::Available(table) = &r.sfo else {
        panic!("{:?}", r.sfo)
    };
    assert_eq!(table.entries.len(), 6);
    assert!(matches!(&r.details, Details::Ps4 { entries, .. } if entries.len() == 3));
}

#[test]
fn encrypted_and_missing_sfo_are_statuses_not_errors() {
    let r = inspect(Cursor::new(ps4_pkg(true)), L).expect("inspect");
    assert_eq!(
        r.sfo,
        SfoStatus::Encrypted {
            entry_id: 0x1000,
            key_index: 3
        }
    );
    assert_eq!(r.summary.content, ContentKind::Unknown);
    let r = inspect(Cursor::new(ps3_pkg(false)), L).expect("inspect");
    assert_eq!(r.sfo, SfoStatus::Missing);
}

#[test]
fn inspects_ps3_and_bare_sfo() {
    let r = inspect(Cursor::new(ps3_pkg(true)), L).expect("inspect");
    assert_eq!(r.summary.content, ContentKind::Game);
    let Details::Ps3 {
        metadata, entries, ..
    } = &r.details
    else {
        panic!()
    };
    assert_eq!(metadata.len(), 1);
    // ICON0.PNG, PARAM.SFO, USRDIR (implied), USRDIR/EBOOT.BIN.
    assert_eq!(entries.len(), 4);

    let r = inspect(Cursor::new(sfo(b"ac")), L).expect("inspect");
    assert_eq!(
        (r.kind, r.details, r.summary.content),
        (Kind::Sfo, Details::Sfo, ContentKind::AddOn)
    );
    assert_eq!(r.summary.package_size, None);
}

#[test]
fn unknown_input_is_unsupported() {
    let err = inspect(Cursor::new(b"hello".to_vec()), L).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unsupported);
}

#[test]
fn artwork_reads_and_png_check() {
    let icon = artwork(Cursor::new(ps4_pkg(false)), Artwork::Icon0, L).expect("art");
    assert_eq!(icon.as_deref(), Some(PNG));
    assert!(displayable_png(PNG));
    assert_eq!(
        artwork(Cursor::new(ps4_pkg(false)), Artwork::Pic1, L).expect("art"),
        None
    );
    let mut huge = PNG.to_vec();
    huge[16..20].copy_from_slice(&4096u32.to_be_bytes());
    assert!(!displayable_png(&huge));
    assert!(!displayable_png(&PNG[..20]));
}

#[test]
fn corruption_sweep_gives_typed_errors_never_panics() {
    for (seed, data) in [(1, ps4_pkg(false)), (2, ps3_pkg(true)), (3, sfo(b"gd"))] {
        Corruptor::new(seed).run(&data, 2000, |input| {
            if let Err(e) = inspect(Cursor::new(input), L) {
                // Classification is total; Io never happens on an in-memory source.
                assert_ne!(e.kind(), ErrorKind::Io, "{e}");
            }
        });
        for cut in Corruptor::truncations(&data) {
            let _ = inspect(Cursor::new(cut), L);
        }
    }
}

struct Run {
    code: u8,
    out: String,
    err: String,
}

fn run(args: &[&std::ffi::OsStr]) -> Run {
    let argv: Vec<OsString> = args.iter().map(|a| a.to_os_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(
        &argv,
        &mut Io {
            out: &mut out,
            err: &mut err,
        },
    );
    Run {
        code,
        out: String::from_utf8(out).expect("utf8"),
        err: String::from_utf8(err).expect("utf8"),
    }
}

struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!("athena-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Temp(dir)
    }

    fn file(&self, name: &str, data: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, data).expect("write");
        path
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn os(p: &Path) -> &std::ffi::OsStr {
    p.as_os_str()
}

#[test]
fn cli_inspect_text_json_and_exit_codes() {
    let t = Temp::new("inspect");
    let pkg = t.file("in.pkg", &ps4_pkg(false));
    let r = run(&["inspect".as_ref(), os(&pkg), "--json".as_ref()]);
    assert_eq!((r.code, r.err.as_str()), (0, ""));
    assert!(
        r.out
            .starts_with(r#"{"schema":"pantheon.athena.inspection/1","format":"ps4-pkg""#)
    );
    assert!(r.out.contains(
        r#""key":"TITLE","key_hex":"5449544c45","format":516,"kind":"text","value":null"#
    ));
    assert!(r.out.contains(r#""fpkg_status":"unverified""#));
    assert!(r.out.ends_with("}\n"));

    let r = run(&["inspect".as_ref(), os(&pkg)]);
    assert_eq!(r.code, 0);
    assert!(
        r.out
            .contains("Title ID: CUSA00001\nVersion: 01.02\nType: patch (gp)\n"),
        "{}",
        r.out
    );
    assert!(r.out.contains("\"PUBTOOLINFO\": hex:dead (binary)"));

    let enc = t.file("enc.pkg", &ps4_pkg(true));
    let r = run(&["inspect".as_ref(), os(&enc), "--json".as_ref()]);
    assert_eq!(r.code, 0, "encrypted SFO still exits 0");
    assert!(
        r.out
            .contains(r#""sfo":{"status":"decryption_unavailable","entry_id":4096,"key_index":3}"#)
    );

    let bad = t.file("bad.pkg", b"\x7fCNT short");
    assert_eq!(run(&["inspect".as_ref(), os(&bad)]).code, 3);
    assert_eq!(run(&["inspect".as_ref(), os(&t.0.join("nope"))]).code, 8);
    assert_eq!(run(&["inspect".as_ref()]).code, 2);
    let help = run(&["--help".as_ref()]);
    assert_eq!(help.code, 0);
    assert!(
        help.out
            .contains("athena artwork INPUT OUT.png (--icon0|--pic1)")
    );
}

#[test]
fn cli_artwork_never_overwrites() {
    let t = Temp::new("artwork");
    let pkg = t.file("in.pkg", &ps4_pkg(false));
    let out = t.0.join("icon0.png");
    let r = run(&["artwork".as_ref(), os(&pkg), os(&out), "--icon0".as_ref()]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(std::fs::read(&out).expect("saved"), PNG);
    let again = run(&["artwork".as_ref(), os(&pkg), os(&out), "--icon0".as_ref()]);
    assert_eq!(again.code, 7, "{}", again.err);
    let none = run(&[
        "artwork".as_ref(),
        os(&pkg),
        os(&t.0.join("p.png")),
        "--pic1".as_ref(),
    ]);
    assert_eq!(none.code, 4);
    assert!(!t.0.join("p.png").exists());
}
