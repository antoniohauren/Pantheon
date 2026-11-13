//! Proteus tests: pack and convert round trips through `pantheon-zar::Reader`,
//! identity and output checks, two-file publication, cancel and the CLI.

use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};

use pantheon_args::Io;
use pantheon_core::{Cancel, Classify, Ctx, ErrorKind, Limits, NoProgress, Progress, ProgressSink};
use pantheon_ps3::testing::{self as ps3t, Item};
use pantheon_ps4::testing::{self as ps4t, Record};
use pantheon_psf::{Entry, FORMAT_TEXT, Sfo, VERSION_1_1, Value};
use pantheon_zar::{EntryKind, Reader};

use crate::*;

const CONTENT_ID: &str = "UP0000-CUSA00001_00-0000000000000000";
const TITLE_ID: &str = "CUSA00001";

/// A fresh, canonical directory under the system temp dir; removed on success.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> TestDir {
        let mut bytes = [0u8; 8];
        pantheon_fs::os_random(&mut bytes).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = base.join(format!("pantheon-proteus-test-{hex}"));
        std::fs::create_dir(&path).unwrap();
        TestDir(path)
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
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

fn run<T>(f: impl FnOnce(&Ctx<'_>) -> T) -> T {
    let cancel = Cancel::new();
    f(&Ctx::new(&cancel, &NoProgress))
}

/// `(path, None)` for folders, `(path, Some(bytes))` for files, sorted.
type Listing = Vec<(String, Option<Vec<u8>>)>;

fn read_back(path: &Path) -> Listing {
    let mut r = Reader::open(File::open(path).unwrap(), &Limits::DEFAULT).unwrap();
    r.verify_digest().unwrap();
    let mut out = Vec::new();
    for e in r.entries().unwrap() {
        let bytes = match e.kind {
            EntryKind::Directory => None,
            EntryKind::File => {
                let mut buf = vec![0u8; e.size as usize];
                assert_eq!(r.read_at(e.node, 0, &mut buf).unwrap(), buf.len());
                Some(buf)
            }
        };
        out.push((e.path, bytes));
    }
    out.sort();
    out
}

fn sample_tree(dir: &Path) -> Listing {
    let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let listing: Listing = vec![
        ("data".into(), None),
        ("data/big.bin".into(), Some(big)),
        ("data/empty".into(), None),
        ("data/zero".into(), Some(Vec::new())),
        ("eboot.bin".into(), Some(b"ELF".to_vec())),
        ("sce_sys".into(), None),
        ("sce_sys/param.sfo".into(), Some(b"\0PSF".to_vec())),
    ];
    for (rel, data) in &listing {
        match data {
            None => std::fs::create_dir(dir.join(rel)).unwrap(),
            Some(d) => std::fs::write(dir.join(rel), d).unwrap(),
        }
    }
    listing
}

#[test]
fn pack_round_trips_a_folder() {
    let dir = TestDir::new();
    std::fs::create_dir(dir.join("src")).unwrap();
    let want = sample_tree(&dir.join("src"));
    let out = dir.join("game.zar");
    let report = run(|ctx| pack(&dir.join("src"), &out, ctx)).unwrap();
    assert_eq!(
        (report.files, report.directories, report.bytes),
        (4, 3, 200_007)
    );
    assert_eq!(report.path, out);
    assert_eq!(read_back(&out), want);
    assert_eq!(dir.names(), ["game.zar", "src"], "no scratch left");
}

#[test]
fn pack_refusals_leave_no_output() {
    let dir = TestDir::new();
    std::fs::create_dir(dir.join("src")).unwrap();
    sample_tree(&dir.join("src"));

    // Inside the source.
    let err = run(|ctx| pack(&dir.join("src"), &dir.join("src/data/x.zar"), ctx)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{err}");

    // Existing output is never overwritten.
    std::fs::write(dir.join("taken.zar"), b"keep").unwrap();
    let err = run(|ctx| pack(&dir.join("src"), &dir.join("taken.zar"), ctx)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{err}");
    assert_eq!(std::fs::read(dir.join("taken.zar")).unwrap(), b"keep");
    std::fs::remove_file(dir.join("taken.zar")).unwrap();

    // A 128-byte name is refused before any output exists.
    std::fs::write(dir.join("src").join("n".repeat(128)), b"x").unwrap();
    let err = run(|ctx| pack(&dir.join("src"), &dir.join("long.zar"), ctx)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unsupported, "{err}");
    std::fs::remove_file(dir.join("src").join("n".repeat(128))).unwrap();

    // Every file empty.
    std::fs::create_dir_all(dir.join("empty/sub")).unwrap();
    std::fs::write(dir.join("empty/sub/zero"), b"").unwrap();
    let err = run(|ctx| pack(&dir.join("empty"), &dir.join("empty.zar"), ctx)).unwrap_err();
    assert!(
        matches!(err, Error::Zar(pantheon_zar::Error::EmptyPayload)),
        "{err}"
    );

    // Symlinks in the source.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("eboot.bin", dir.join("src/link")).unwrap();
        let err = run(|ctx| pack(&dir.join("src"), &dir.join("link.zar"), ctx)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Unsupported, "{err}");
    }
    assert_eq!(dir.names(), ["empty", "src"]);
}

/// Cancels on the first progress event that passes `after` bytes.
struct CancelAt<'a> {
    cancel: &'a Cancel,
    after: u64,
}

impl ProgressSink for CancelAt<'_> {
    fn report(&self, p: &Progress<'_>) {
        if p.done > self.after {
            self.cancel.cancel();
        }
    }
}

#[test]
fn cancel_removes_unpublished_scratch() {
    let dir = TestDir::new();
    std::fs::create_dir(dir.join("src")).unwrap();
    sample_tree(&dir.join("src"));
    let cancel = Cancel::new();
    let sink = CancelAt {
        cancel: &cancel,
        after: 0,
    };
    let err = pack(
        &dir.join("src"),
        &dir.join("game.zar"),
        &Ctx::new(&cancel, &sink),
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Cancelled);
    assert_eq!(dir.names(), ["src"]);
}

fn sfo(category: &str, title: &str, content: &str) -> Vec<u8> {
    let text = |key: &'static [u8], value: &str| {
        let raw: &'static [u8] = Box::leak(format!("{value}\0").into_bytes().into_boxed_slice());
        Entry {
            key,
            format: FORMAT_TEXT,
            value: Value::Text(&raw[..raw.len() - 1]),
            raw,
            max_len: raw.len() as u32,
            padding: b"",
        }
    };
    pantheon_psf::serialize(
        &Sfo {
            version: VERSION_1_1,
            entries: vec![
                text(b"CATEGORY", category),
                text(b"CONTENT_ID", content),
                text(b"TITLE_ID", title),
            ],
        },
        &Limits::DEFAULT,
    )
    .unwrap()
}

fn ps4_pkg(category: &str, title: &str, content: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let nodes = ps4t::app_nodes(files, &["empty"]);
    let records = [Record::plain(
        pantheon_ps4::id::PARAM_SFO,
        &sfo(category, title, content),
    )];
    ps4t::game_package(content, &nodes, &records)
}

fn base_pkg() -> Vec<u8> {
    let big = vec![9u8; 70_000];
    ps4_pkg(
        "gd",
        TITLE_ID,
        CONTENT_ID,
        &[("eboot.bin", b"base"), ("data/big.bin", &big)],
    )
}

fn update_pkg() -> Vec<u8> {
    ps4_pkg(
        "gp",
        TITLE_ID,
        CONTENT_ID,
        &[("eboot.bin", b"patched"), ("data/new.bin", b"new")],
    )
}

fn write(dir: &TestDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// What `hades` would extract from `pkg`, for comparison.
fn package_listing(pkg: &[u8]) -> Listing {
    let mut contents = pantheon_package::Package::open(std::io::Cursor::new(pkg), &Limits::DEFAULT)
        .unwrap()
        .into_contents()
        .unwrap();
    let nodes: Vec<_> = contents.tree().nodes().to_vec();
    let mut out = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let bytes = match n.kind {
            pantheon_package::NodeKind::Directory => None,
            pantheon_package::NodeKind::File => {
                let mut buf = vec![0u8; n.size as usize];
                contents.read_at(i as u32, 0, &mut buf).unwrap();
                Some(buf)
            }
        };
        out.push((n.path.clone(), bytes));
    }
    out.sort();
    out
}

#[test]
fn convert_streams_base_and_update_to_sibling_archives() {
    let dir = TestDir::new();
    let (base, update) = (base_pkg(), update_pkg());
    let base_path = write(&dir, "base.pkg", &base);
    let update_path_in = write(&dir, "update.pkg", &update);
    let out = dir.join("game.zar");
    let report = run(|ctx| convert(&base_path, Some(&update_path_in), &out, ctx)).unwrap();
    let upd = report.update.unwrap();
    assert_eq!(report.base.path, out);
    assert_eq!(upd.path, dir.join("game-UPDATE.zar"));
    assert_eq!(read_back(&out), package_listing(&base));
    assert_eq!(read_back(&upd.path), package_listing(&update));
    assert_eq!(
        dir.names(),
        ["base.pkg", "game-UPDATE.zar", "game.zar", "update.pkg"]
    );

    let alone = dir.join("alone.zar");
    let report = run(|ctx| convert(&base_path, None, &alone, ctx)).unwrap();
    assert!(report.update.is_none());
    assert!(!dir.join("alone-UPDATE.zar").exists());
}

#[test]
fn update_path_needs_lowercase_zar() {
    assert_eq!(
        update_path(Path::new("/x/Game.zar")).unwrap(),
        Path::new("/x/Game-UPDATE.zar")
    );
    for bad in ["/x/Game.ZAR", "/x/Game", "/x/Game.zip", "/x/.zar"] {
        let err = update_path(Path::new(bad)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Usage, "{bad}");
    }
}

#[test]
fn convert_checks_identities_before_any_output() {
    let dir = TestDir::new();
    let base = write(&dir, "base.pkg", &base_pkg());
    let update = write(&dir, "update.pkg", &update_pkg());
    let other = write(
        &dir,
        "other.pkg",
        &ps4_pkg(
            "gp",
            "CUSA00002",
            "UP0000-CUSA00002_00-0000000000000000",
            &[("a", b"a")],
        ),
    );
    let badtitle = write(
        &dir,
        "badtitle.pkg",
        &ps4_pkg(
            "gd",
            "NPXS00001",
            "UP0000-NPXS00001_00-0000000000000000",
            &[("a", b"a")],
        ),
    );
    // The header says one content ID, PARAM.SFO another.
    let mismatch = write(
        &dir,
        "mismatch.pkg",
        &ps4t::game_package(
            "UP0000-CUSA00001_00-1111111111111111",
            &ps4t::app_nodes(&[("a", b"a")], &[]),
            &[Record::plain(
                pantheon_ps4::id::PARAM_SFO,
                &sfo("gd", TITLE_ID, CONTENT_ID),
            )],
        ),
    );
    let ps3 = write(
        &dir,
        "ps3.pkg",
        &ps3t::package(
            false,
            "UP0000-TEST00000_00-0000000000000000",
            &[],
            &[Item::file("USRDIR/EBOOT.BIN", b"x")],
        ),
    );
    let out = dir.join("out.zar");
    let missing = dir.join("missing.pkg");
    let cases: Vec<(&Path, Option<&Path>, ErrorKind)> = vec![
        (&update, None, ErrorKind::Unsupported),       // gp as base
        (&base, Some(&base), ErrorKind::Unsupported),  // gd as update
        (&base, Some(&other), ErrorKind::Unsupported), // IDs differ
        (&badtitle, None, ErrorKind::Unsupported),     // not CUSA
        (&mismatch, None, ErrorKind::Unsupported),     // header vs SFO content ID
        (&ps3, None, ErrorKind::Unsupported),          // PS3
        (&missing, None, ErrorKind::Io),               // cannot open
    ];
    for (b, u, kind) in cases {
        let err = run(|ctx| convert(b, u, &out, ctx)).unwrap_err();
        assert_eq!(err.kind(), kind, "{}: {err}", b.display());
    }
    assert!(!out.exists());
    assert!(!dir.join("out-UPDATE.zar").exists());
}

#[test]
fn existing_update_output_stops_the_conversion_before_streaming() {
    let dir = TestDir::new();
    let base = write(&dir, "base.pkg", &base_pkg());
    let update = write(&dir, "update.pkg", &update_pkg());
    write(&dir, "game-UPDATE.zar", b"keep");
    let err = run(|ctx| convert(&base, Some(&update), &dir.join("game.zar"), ctx)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnsafeOutput, "{err}");
    assert!(
        !dir.join("game.zar").exists(),
        "the base is not published either"
    );
    assert_eq!(std::fs::read(dir.join("game-UPDATE.zar")).unwrap(), b"keep");
}

#[test]
fn cancelled_conversion_removes_both_scratch_archives() {
    let dir = TestDir::new();
    let base_bytes = base_pkg();
    let base = write(&dir, "base.pkg", &base_bytes);
    let update = write(&dir, "update.pkg", &update_pkg());
    let cancel = Cancel::new();
    // Cancel during the update, after the base was fully streamed.
    let sink = CancelAt {
        cancel: &cancel,
        after: package_listing(&base_bytes)
            .iter()
            .filter_map(|(_, b)| b.as_ref().map(|b| b.len() as u64))
            .sum(),
    };
    let err = convert(
        &base,
        Some(&update),
        &dir.join("game.zar"),
        &Ctx::new(&cancel, &sink),
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Cancelled);
    assert_eq!(dir.names(), ["base.pkg", "update.pkg"]);
}

/// Creates `path` once progress passes `after` bytes (during the update).
struct Squat {
    path: PathBuf,
    after: u64,
}

impl ProgressSink for Squat {
    fn report(&self, p: &Progress<'_>) {
        if p.done > self.after && !self.path.exists() {
            std::fs::write(&self.path, b"squatter").unwrap();
        }
    }
}

#[test]
fn failed_update_publication_keeps_the_base_and_names_it() {
    let dir = TestDir::new();
    let base_bytes = base_pkg();
    let base = write(&dir, "base.pkg", &base_bytes);
    let update = write(&dir, "update.pkg", &update_pkg());
    let out = dir.join("game.zar");
    let total_base: u64 = package_listing(&base_bytes)
        .iter()
        .filter_map(|(_, b)| b.as_ref().map(|b| b.len() as u64))
        .sum();
    let cancel = Cancel::new();
    let sink = Squat {
        path: dir.join("game-UPDATE.zar"),
        after: total_base,
    };
    let err = convert(&base, Some(&update), &out, &Ctx::new(&cancel, &sink)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Partial, "{err}");
    assert!(
        err.to_string().contains(&out.display().to_string()),
        "{err}"
    );
    assert_eq!(read_back(&out), package_listing(&base_bytes));
    assert_eq!(
        std::fs::read(dir.join("game-UPDATE.zar")).unwrap(),
        b"squatter"
    );
}

fn cli(args: &[&str]) -> (u8, String, String) {
    let argv: Vec<OsString> = args.iter().map(OsString::from).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(
        &argv,
        &mut Io {
            out: &mut out,
            err: &mut err,
        },
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn cli_packs_converts_and_reports_both_paths() {
    let dir = TestDir::new();
    std::fs::create_dir(dir.join("src")).unwrap();
    sample_tree(&dir.join("src"));
    let s = |p: PathBuf| p.to_str().unwrap().to_owned();
    let (code, out, err) = cli(&["pack", &s(dir.join("src")), &s(dir.join("p.zar"))]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("Packed 4 files and 3 folders"), "{out}");

    let base = write(&dir, "base.pkg", &base_pkg());
    let update = write(&dir, "update.pkg", &update_pkg());
    let (code, out, err) = cli(&[
        "convert",
        &s(base),
        &s(dir.join("g.zar")),
        "--update",
        &s(update),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(&s(dir.join("g.zar"))), "{out}");
    assert!(out.contains(&s(dir.join("g-UPDATE.zar"))), "{out}");

    let (code, _, err) = cli(&["convert", "x.pkg", &s(dir.join("G.ZAR"))]);
    assert_eq!(code, 2, "{err}");
    let (code, out, _) = cli(&["--help"]);
    assert!(code == 0 && out.contains("proteus convert"), "{out}");
    assert_eq!(cli(&["pack"]).0, 2);
}
