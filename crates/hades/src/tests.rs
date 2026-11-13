//! Hades tests: synthetic PS3/PS4 trees, selections, output safety, cancel,
//! I/O failure and the CLI.

use std::cell::Cell;
use std::ffi::OsString;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use pantheon_args::Io;
use pantheon_core::testing::VecSink;
use pantheon_core::{Cancel, Classify, Ctx, ErrorKind, Limits, NoProgress, Progress, ProgressSink};
use pantheon_ps3::testing::{self as ps3t, Item};
use pantheon_ps4::testing::{self as ps4t, Record};

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
        let path = base.join(format!("pantheon-hades-test-{hex}"));
        std::fs::create_dir(&path).unwrap();
        TestDir(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// The shared selection shape (pkg-tools `docs/explore-selection.md`).
fn ps3_items() -> Vec<Item> {
    vec![
        Item::file("a.bin", b"a"),
        Item::dir("data"),
        Item::file("data/config.bin", &[1; 10]),
        Item::dir("data/empty"),
        Item::file("data/textures/x.png", &[2; 100]),
        Item::file("data/textures/y.png", &[3; 70_000]),
        Item::file("other/z", b"zzzzz"),
    ]
}

fn ps3_pkg(retail: bool) -> Vec<u8> {
    ps3t::package(
        retail,
        "UP0000-TEST00000_00-0000000000000000",
        &[],
        &ps3_items(),
    )
}

fn ps4_pkg() -> Vec<u8> {
    let y = vec![3u8; 70_000];
    let nodes = ps4t::app_nodes(
        &[
            ("a.bin", b"a"),
            ("data/config.bin", &[1; 10]),
            ("data/textures/x.png", &[2; 100]),
            ("data/textures/y.png", &y),
            ("other/z", b"zzzzz"),
        ],
        &["data/empty"],
    );
    let records = [Record {
        id: 0x1000,
        encrypted: true,
        key_index: 3,
        data: b"\0PSF sealed".to_vec(),
    }];
    ps4t::game_package("UP0000-TEST00000_00-0000000000000000", &nodes, &records)
}

/// Every file and folder below `root`: (relative path, Some(bytes) for files).
fn snapshot(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = pending.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().into_string().unwrap();
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if e.file_type().unwrap().is_dir() {
                pending.push((e.path(), path.clone()));
                out.push((path, None));
            } else {
                out.push((path, Some(std::fs::read(e.path()).unwrap())));
            }
        }
    }
    out.sort();
    out
}

fn ctx<'a>(cancel: &'a Cancel, sink: &'a dyn ProgressSink) -> Ctx<'a> {
    Ctx::new(cancel, sink)
}

fn extract_all(pkg: &[u8], dir: &Path, name: &str) -> Result<Report, Error> {
    let mut c = open(Cursor::new(pkg), L)?;
    extract(
        &mut c,
        &Selection::All,
        dir,
        name,
        &ctx(&Cancel::new(), &NoProgress),
    )
}

#[test]
fn full_extraction_matches_the_tree_on_both_platforms() {
    let dir = TestDir::new();
    for (name, pkg) in [
        ("retail", ps3_pkg(true)),
        ("debug", ps3_pkg(false)),
        ("ps4", ps4_pkg()),
    ] {
        let r = extract_all(&pkg, &dir.0, name).unwrap();
        assert_eq!(r.path, dir.0.join(name));
        let got = snapshot(&r.path);
        let paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        let mut want = vec![
            "a.bin",
            "data",
            "data/config.bin",
            "data/empty",
            "data/textures",
            "data/textures/x.png",
            "data/textures/y.png",
            "other",
            "other/z",
        ];
        if name == "ps4" {
            want.extend(["sce_sys", "sce_sys/param.sfo"]);
        }
        want.sort();
        assert_eq!(paths, want, "{name}");
        let file = |p: &str| got.iter().find(|(q, _)| q == p).unwrap().1.clone().unwrap();
        assert_eq!(file("data/textures/y.png"), vec![3; 70_000]);
        assert_eq!(file("other/z"), b"zzzzz");
        if name == "ps4" {
            assert_eq!(file("sce_sys/param.sfo"), b"\0PSF sealed");
        }
        let bytes = if name == "ps4" { 70_127 } else { 70_116 };
        assert_eq!(r.bytes, bytes, "{name}");
    }
}

#[test]
fn selected_extraction_keeps_parents_subtrees_and_empty_folders() {
    let dir = TestDir::new();
    for (name, pkg) in [("ps3", ps3_pkg(true)), ("ps4", ps4_pkg())] {
        let mut c = open(Cursor::new(&pkg[..]), L).unwrap();
        let sel = c
            .tree()
            .select(&["data/textures", "data/textures/x.png", "data/empty"])
            .unwrap();
        let sink = VecSink::new();
        let r = extract(&mut c, &sel, &dir.0, name, &ctx(&Cancel::new(), &sink)).unwrap();
        let paths: Vec<String> = snapshot(&r.path).into_iter().map(|(p, _)| p).collect();
        assert_eq!(
            paths,
            [
                "data",
                "data/empty",
                "data/textures",
                "data/textures/x.png",
                "data/textures/y.png"
            ]
        );
        assert_eq!((r.files, r.directories, r.bytes), (2, 3, 70_100));
        let events = sink.events();
        assert_eq!(
            events.first().map(|e| (e.done, e.total)),
            Some((0, Some(70_100)))
        );
        assert_eq!(events.last().map(|e| e.done), Some(70_100));
        assert!(events.windows(2).all(|w| w[0].done <= w[1].done));
    }
}

#[test]
fn bad_selections_fail_before_any_output() {
    let dir = TestDir::new();
    let pkg = ps3_pkg(true);
    let mut c = open(Cursor::new(&pkg[..]), L).unwrap();
    let none = NoProgress;
    let cancel = Cancel::new();
    assert!(c.tree().select(&["data/missing"]).is_err());
    assert!(c.tree().select::<&str>(&[]).is_err());
    let stale = open(Cursor::new(&pkg[..]), L)
        .unwrap()
        .tree()
        .select(&["data"])
        .unwrap();
    let e = extract(&mut c, &stale, &dir.0, "out", &ctx(&cancel, &none)).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Usage);
    let empty = Selection::Paths {
        generation: c.tree().generation(),
        indices: vec![],
    };
    assert!(extract(&mut c, &empty, &dir.0, "out", &ctx(&cancel, &none)).is_err());
    cancel.cancel();
    let e = extract(&mut c, &Selection::All, &dir.0, "out", &ctx(&cancel, &none)).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Cancelled);
    assert!(e.incomplete().is_none());
    assert!(!dir.0.join("out").exists());
}

#[test]
fn existing_output_and_symlinked_parents_are_refused() {
    let dir = TestDir::new();
    let pkg = ps3_pkg(true);
    std::fs::create_dir(dir.0.join("taken")).unwrap();
    let e = extract_all(&pkg, &dir.0, "taken").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::UnsafeOutput);
    assert!(
        std::fs::read_dir(dir.0.join("taken"))
            .unwrap()
            .next()
            .is_none()
    );
    assert_eq!(
        extract_all(&pkg, &dir.0, "..").unwrap_err().kind(),
        ErrorKind::UnsafeOutput
    );
    #[cfg(unix)]
    {
        std::fs::create_dir(dir.0.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.0.join("real"), dir.0.join("link")).unwrap();
        let e = extract_all(&pkg, &dir.0.join("link"), "out").unwrap_err();
        assert_eq!(e.kind(), ErrorKind::UnsafeOutput);
        assert!(!dir.0.join("real/out").exists());
    }
}

/// Cancels once any file bytes have been written.
struct CancelMidFile<'a>(&'a Cancel, Mutex<u32>);

impl ProgressSink for CancelMidFile<'_> {
    fn report(&self, p: &Progress<'_>) {
        if p.done > 0 {
            *self.1.lock().unwrap() += 1;
            self.0.cancel();
        }
    }
}

#[test]
fn cancel_mid_extraction_leaves_a_marked_tree() {
    let dir = TestDir::new();
    let pkg = ps4_pkg();
    let mut c = open(Cursor::new(&pkg[..]), L).unwrap();
    let cancel = Cancel::new();
    let sink = CancelMidFile(&cancel, Mutex::new(0));
    let e = extract(&mut c, &Selection::All, &dir.0, "out", &ctx(&cancel, &sink)).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Cancelled);
    assert_eq!(e.incomplete(), Some(dir.0.join("out").as_path()));
    assert!(dir.0.join("out").join(pantheon_fs::MARKER).exists());
    assert_eq!(
        *sink.1.lock().unwrap(),
        1,
        "stops at the next chunk boundary"
    );
    // A marked tree is never reused.
    let e = extract_all(&pkg, &dir.0, "out").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::UnsafeOutput);
}

/// A reader that starts failing when `broken` is set.
struct Flaky {
    inner: Cursor<Vec<u8>>,
    broken: Rc<Cell<bool>>,
}

impl Read for Flaky {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.broken.get() {
            return Err(io::Error::other("injected read failure"));
        }
        self.inner.read(buf)
    }
}

impl Seek for Flaky {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn read_failures_after_creation_leave_a_marked_tree() {
    let dir = TestDir::new();
    let broken = Rc::new(Cell::new(false));
    let src = Flaky {
        inner: Cursor::new(ps3_pkg(true)),
        broken: Rc::clone(&broken),
    };
    let mut c = open(src, L).unwrap();
    broken.set(true);
    let e = extract(
        &mut c,
        &Selection::All,
        &dir.0,
        "out",
        &ctx(&Cancel::new(), &NoProgress),
    )
    .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Io);
    assert_eq!(e.incomplete(), Some(dir.0.join("out").as_path()));
    assert!(dir.0.join("out").join(pantheon_fs::MARKER).exists());
}

#[test]
fn folder_names_follow_the_rpcs3_cap() {
    assert_eq!(default_folder_name("game"), "game-extracted");
    let long = default_folder_name("UP0006-NPUB30892_00-SSXNAHDD00000000");
    assert_eq!(long, "UP0006-NPUB30892_00-S-extracted");
    assert_eq!(long.len(), 31);
    assert_eq!(default_folder_name("ééééééééééé"), "éééééééééé-extracted");
    assert!(default_folder_name("ééééééééééé").len() <= 31);
    assert_eq!(default_folder_name("abc.  "), "abc-extracted");
    assert_eq!(default_folder_name(""), "package-extracted");
}

fn cli(args: &[&OsString]) -> (u8, String, String) {
    let argv: Vec<OsString> = args.iter().map(|a| (*a).clone()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = crate::cli::run(
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
fn cli_lists_and_extracts() {
    let dir = TestDir::new();
    let input = dir.0.join("in.pkg");
    std::fs::write(&input, ps3_pkg(true)).unwrap();
    let os = |s: &str| OsString::from(s);
    let input = input.into_os_string();

    let (code, out, _) = cli(&[&os("list"), &input]);
    assert_eq!(code, 0);
    assert!(
        out.contains("           100  data/textures/x.png\n"),
        "{out}"
    );
    assert!(out.contains("         <dir>  data/empty/\n"), "{out}");
    assert!(out.ends_with("5 files, 4 folders, 70116 bytes\n"), "{out}");

    let (code, out, _) = cli(&[&os("list"), &input, &os("--json")]);
    assert_eq!(code, 0);
    assert!(
        out.starts_with("{\"schema\":\"pantheon.hades.listing/1\""),
        "{out}"
    );
    assert!(out.contains("{\"path\":\"data/empty\",\"kind\":\"directory\",\"size\":0}"));

    let target = dir.0.join("sel").into_os_string();
    let (code, out, err) = cli(&[
        &os("extract"),
        &input,
        &target,
        &os("--select"),
        &os("other"),
        &os("--select"),
        &os("a.bin"),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Extracted 2 files and 1 folders (0.0 MB) to "),
        "{out}"
    );
    let got: Vec<String> = snapshot(&dir.0.join("sel"))
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    assert_eq!(got, ["a.bin", "other", "other/z"]);

    let (code, _, err) = cli(&[&os("extract"), &input, &target]);
    assert_eq!(code, ErrorKind::UnsafeOutput.exit_code(), "{err}");
    let other = dir.0.join("x").into_os_string();
    let (code, _, err) = cli(&[&os("extract"), &input, &other, &os("--select"), &os("nope")]);
    assert_eq!(code, ErrorKind::Usage.exit_code(), "{err}");
    assert!(!dir.0.join("x").exists());
    let (code, _, _) = cli(&[&os("list"), &dir.0.join("missing").into_os_string()]);
    assert_eq!(code, ErrorKind::Io.exit_code());
    let sfo = dir.0.join("p.sfo");
    std::fs::write(&sfo, b"\0PSF\x01\x01\0\0").unwrap();
    let (code, _, _) = cli(&[&os("list"), &sfo.into_os_string()]);
    assert_eq!(code, ErrorKind::Unsupported.exit_code());
}
