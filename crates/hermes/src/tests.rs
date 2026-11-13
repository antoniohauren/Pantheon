//! Host session against a fake console: an in-process client built on
//! `hermes-wire` that plays the loader and the payload over loopback
//! (hermes.md "Tests").

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread::JoinHandle;

use hermes_wire::config::{self, Config};
use hermes_wire::{
    BUILD_ID_LEN, DIGEST_LEN, Failed, Hello, Kind, Message, Registered, Stage, Status, TOKEN_LEN,
    TaskState, VERSION,
};
use pantheon_args::Io;
use pantheon_core::testing::VecSink;
use pantheon_core::{Cancel, Classify, Ctx, ErrorKind, Limits};
use pantheon_ps4::testing::{self as ps4t, Record};
use pantheon_psf::{Entry, FORMAT_TEXT, Sfo, VERSION_1_1, Value};

use crate::http::{self, Coverage, Range, Route, parse_range, route};
use crate::ops::{Error, Loader, Payload, SendReport, Target};
use crate::session::{read_frame, write_message};
use crate::{probe, send};

const LOCALHOST: Ipv4Addr = Ipv4Addr::LOCALHOST;
const BUILD_ID: [u8; BUILD_ID_LEN] = [0xB1; BUILD_ID_LEN];
const ICON: &[u8] = b"\x89PNG\r\n\x1a\nnot a real image";
const CONTENT_ID: &str = "UP0000-TEST00000_00-0000000000000000";
/// Large enough to need several 64 KiB chunks.
const FILLER: usize = 200_000;
const FILLER_RECORD: u32 = 0x2000;
const CONSOLE_ERROR: u32 = 0x8099_0001;

/// A fresh, canonical directory under the system temp dir; removed on success.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> TestDir {
        let mut bytes = [0u8; 8];
        pantheon_fs::os_random(&mut bytes).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = base.join(format!("pantheon-hermes-test-{hex}"));
        std::fs::create_dir(&path).unwrap();
        TestDir(path)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn text(key: &'static [u8], value: &str) -> Entry<'static> {
    let mut raw = value.as_bytes().to_vec();
    raw.push(0);
    let raw: &'static [u8] = Box::leak(raw.into_boxed_slice());
    Entry {
        key,
        format: FORMAT_TEXT,
        value: Value::Text(&raw[..raw.len() - 1]),
        raw,
        max_len: raw.len() as u32,
        padding: b"",
    }
}

fn sfo(title: &str, category: &str) -> Vec<u8> {
    let entries = vec![text(b"CATEGORY", category), text(b"TITLE", title)];
    pantheon_psf::serialize(
        &Sfo {
            version: VERSION_1_1,
            entries,
        },
        &Limits::DEFAULT,
    )
    .unwrap()
}

fn digest(seed: u8) -> [u8; DIGEST_LEN] {
    [seed; DIGEST_LEN]
}

/// A synthetic PS4 package with a title, category, icon and digest.
fn package(title: &str, category: &str, seed: u8) -> Vec<u8> {
    let mut pkg = ps4t::package(
        CONTENT_ID,
        &[
            Record::plain(pantheon_ps4::id::PARAM_SFO, &sfo(title, category)),
            Record::plain(pantheon_ps4::id::ICON0, ICON),
            Record::plain(FILLER_RECORD, &vec![seed; FILLER]),
        ],
    );
    let at = pantheon_ps4::DIGEST_OFFSET as usize;
    pkg[at..at + DIGEST_LEN].copy_from_slice(&digest(seed));
    pkg
}

/// A stand-in payload: some code bytes and a stamped config block.
fn payload_image() -> Vec<u8> {
    let mut image = b"\xEB\xFE fake payload ".to_vec();
    image.extend_from_slice(&config::template());
    image.extend_from_slice(&[0x90; 32]);
    config::stamp_build_id(&mut image, &BUILD_ID).unwrap();
    image
}

/// The console's loader: a listener the host pushes the payload to.
struct Loader_ {
    listener: TcpListener,
}

impl Loader_ {
    fn on(ip: Ipv4Addr) -> Loader_ {
        Loader_ {
            listener: TcpListener::bind((ip, 0)).unwrap(),
        }
    }

    fn target(&self) -> Target {
        let addr = self.listener.local_addr().unwrap();
        let std::net::IpAddr::V4(ps4) = addr.ip() else {
            unreachable!("bound to IPv4")
        };
        Target {
            ps4,
            loader: Loader::Tcp { port: addr.port() },
        }
    }

    /// Accepts the payload push and returns its patched config.
    fn receive(&self) -> Config {
        let (mut s, _) = self.listener.accept().unwrap();
        let mut image = Vec::new();
        s.read_to_end(&mut image).unwrap();
        let at = config::find(&image).unwrap();
        let fields: [u8; config::FIELDS_LEN] = image[at + config::MARKER_LEN..at + config::LEN]
            .try_into()
            .unwrap();
        Config::decode_fields(&fields).unwrap()
    }
}

/// A job as the fake payload received it.
#[derive(Debug)]
struct GotJob {
    index: u32,
    manifest_url: String,
    name: String,
    package_type: String,
    size: u64,
    digest: [u8; DIGEST_LEN],
    icon: Option<Vec<u8>>,
}

/// The fake payload's control connection.
struct Control(TcpStream);

impl Control {
    fn connect(cfg: &Config) -> Control {
        Control(TcpStream::connect((Ipv4Addr::from(cfg.host), cfg.port)).unwrap())
    }

    fn hello(cfg: &Config) -> Control {
        let mut c = Control::connect(cfg);
        c.hello_with(VERSION, cfg.token);
        c
    }

    fn hello_with(&mut self, version: u32, token: [u8; TOKEN_LEN]) {
        self.send(&Message::Hello(Hello {
            version,
            token,
            build_id: BUILD_ID,
        }));
    }

    fn send(&mut self, m: &Message<'_>) {
        write_message(&mut self.0, m).unwrap();
    }

    fn registered(&mut self, index: u32) {
        self.send(&Message::Registered(Registered {
            index,
            task_id: 0x100 + u64::from(index),
        }));
    }

    /// The next frame: `Some(job)`, or `None` for `Bye`.
    fn next(&mut self) -> Option<GotJob> {
        let (kind, body) = read_frame(&mut self.0, &[Kind::Job, Kind::Bye]).unwrap();
        match Message::decode(kind, &body).unwrap() {
            Message::Job(j) => Some(GotJob {
                index: j.index,
                manifest_url: j.manifest_url.to_owned(),
                name: j.name.to_owned(),
                package_type: j.package_type.to_owned(),
                size: j.size,
                digest: j.digest,
                icon: j.icon.map(<[u8]>::to_vec),
            }),
            Message::Bye => None,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn job(&mut self) -> GotJob {
        self.next().expect("a Job, not Bye")
    }

    fn bye(&mut self) {
        assert!(self.next().is_none(), "expected Bye");
    }

    /// True once the host has closed this connection.
    fn closed_by_host(&mut self) -> bool {
        let mut b = [0u8; 1];
        matches!(self.0.read(&mut b), Ok(0) | Err(_))
    }
}

/// One HTTP exchange: (status, head, body).
fn http(url: &str, method: &str, range: Option<&str>) -> (u16, String, Vec<u8>) {
    http_limited(url, method, range, usize::MAX)
}

/// Like `http`, but reads at most `limit` body bytes, then drops the connection.
fn http_limited(
    url: &str,
    method: &str,
    range: Option<&str>,
    limit: usize,
) -> (u16, String, Vec<u8>) {
    let rest = url.strip_prefix("http://").unwrap();
    let (authority, path) = rest.split_at(rest.find('/').unwrap());
    let mut s = TcpStream::connect(authority).unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(r) = range {
        request.push_str(&format!("Range: {r}\r\n"));
    }
    request.push_str("\r\n");
    s.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let mut body_start = None;
    loop {
        let n = s.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if body_start.is_none() {
            body_start = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
        }
        if body_start.is_some_and(|b| raw.len() - b >= limit) {
            break;
        }
    }
    let start = body_start.expect("a response head");
    let head = String::from_utf8(raw[..start].to_vec()).unwrap();
    let status = head[9..12].parse().unwrap();
    let mut body = raw[start..].to_vec();
    body.truncate(limit);
    (status, head, body)
}

fn package_url(job: &GotJob) -> String {
    job.manifest_url.replace("package.json", "package.pkg")
}

/// The fake console's per-job behaviour after `Hello`.
fn download(job: &GotJob, expected: &[u8]) {
    let (status, head, manifest) = http(&job.manifest_url, "GET", None);
    assert_eq!(status, 200);
    assert!(head.contains("application/json"));
    let manifest = String::from_utf8(manifest).unwrap();
    let hex: String = job.digest.iter().map(|b| format!("{b:02X}")).collect();
    assert!(
        manifest.contains(&format!("\"packageDigest\":\"{hex}\"")),
        "{manifest}"
    );
    assert!(manifest.contains(&format!("\"url\":\"{}\"", package_url(job))));
    assert!(manifest.contains(&format!("\"originalFileSize\":{}", expected.len())));
    let (status, _, body) = http(&package_url(job), "GET", None);
    assert_eq!(status, 200);
    assert_eq!(body, expected);
}

/// Starts the console side on a thread; it reports the loader's target first.
fn console<T: Send + 'static>(
    ip: Ipv4Addr,
    script: impl FnOnce(Loader_) -> T + Send + 'static,
) -> (Target, JoinHandle<T>) {
    let loader = Loader_::on(ip);
    let target = loader.target();
    (target, std::thread::spawn(move || script(loader)))
}

fn payload_file(dir: &TestDir) -> Payload {
    Payload::File(dir.write("payload.bin", &payload_image()))
}

fn run_send(
    target: &Target,
    payload: &Payload,
    pkgs: &[PathBuf],
    cancel: &Cancel,
) -> (Result<SendReport, Error>, VecSink) {
    let sink = VecSink::new();
    let result = send(target, payload, pkgs, &Ctx::new(cancel, &sink));
    (result, sink)
}

#[test]
fn batch_sends_in_order_with_three_outcomes() {
    let dir = TestDir::new();
    let a = package("First Title", "gd", 1);
    let b = package("Second", "gp", 2);
    let pkgs = vec![dir.write("a.pkg", &a), dir.write("b.pkg", &b)];
    let (a2, b2) = (a.clone(), b.clone());
    let (target, fake) = console(LOCALHOST, move |loader| {
        let cfg = loader.receive();
        assert_eq!(cfg.build_id, BUILD_ID, "host keeps the stamped build ID");
        let mut c = Control::hello(&cfg);
        for (i, bytes) in [a2, b2].iter().enumerate() {
            let job = c.job();
            assert_eq!(job.index, i as u32);
            assert_eq!(job.size, bytes.len() as u64);
            assert_eq!(job.icon.as_deref(), Some(ICON));
            c.registered(job.index);
            download(&job, bytes);
            if i == 0 {
                c.send(&Message::Status(Status {
                    index: 0,
                    state: TaskState::Done,
                    bytes_done: None,
                    error_code: 0,
                }));
            }
            assert_eq!(job.digest, digest(i as u8 + 1));
            let names = ["First Title", "Second"];
            let types = ["PS4GD", "PS4GP"];
            assert_eq!(
                (job.name.as_str(), job.package_type.as_str()),
                (names[i], types[i])
            );
        }
        c.bye();
    });
    let (result, sink) = run_send(&target, &payload_file(&dir), &pkgs, &Cancel::new());
    fake.join().unwrap();
    let report = result.unwrap();
    assert_eq!(report.build_id, BUILD_ID);
    assert_eq!(report.transferred(), 2);
    let [first, second] = report.items.as_slice() else {
        panic!("two items")
    };
    assert!(first.transferred && second.transferred);
    assert_eq!((first.task_id, second.task_id), (Some(0x100), Some(0x101)));
    assert!(first.installed, "Status done reported");
    assert!(!second.installed, "installed is never inferred");
    assert_eq!(first.bytes_transferred, a.len() as u64);
    let last = sink.events().into_iter().last().unwrap();
    assert_eq!(
        (last.done, last.total),
        (b.len() as u64, Some(b.len() as u64))
    );
}

#[test]
fn resume_after_interrupted_range_counts_unique_bytes() {
    let dir = TestDir::new();
    let pkg = package("Resume", "gd", 3);
    let path = dir.write("r.pkg", &pkg);
    let expected = pkg.clone();
    let (target, fake) = console(LOCALHOST, move |loader| {
        let cfg = loader.receive();
        let mut c = Control::hello(&cfg);
        let job = c.job();
        c.registered(0);
        let url = package_url(&job);
        // HEAD: headers only.
        let (status, head, body) = http(&url, "HEAD", None);
        assert_eq!(status, 200);
        assert!(head.contains(&format!("Content-Length: {}", expected.len())));
        assert!(head.contains("Accept-Ranges: bytes"));
        assert!(body.is_empty());
        // Unsatisfiable and foreign paths.
        let (status, head, _) = http(&url, "GET", Some(&format!("bytes={}-", expected.len())));
        assert_eq!(status, 416);
        assert!(head.contains(&format!("Content-Range: bytes */{}", expected.len())));
        let token = url.split('/').nth(3).unwrap();
        let wrong_token = url.replace(token, &"0".repeat(token.len()));
        assert_eq!(http(&wrong_token, "GET", None).0, 404);
        assert_eq!(http(&url.replace("/0/", "/1/"), "GET", None).0, 404);
        assert_eq!(http(&url, "POST", None).0, 405);
        // The first download stops after 1000 bytes; the resume asks for the rest.
        // pkg-tools: a range covering the whole file is a plain 200.
        let (status, _, first) = http_limited(&url, "GET", Some("bytes=0-"), 1000);
        assert_eq!(status, 200);
        let (status, head, rest) = http(&url, "GET", Some("bytes=1000-"));
        assert_eq!(status, 206);
        let last = expected.len() - 1;
        assert!(head.contains(&format!(
            "Content-Range: bytes 1000-{last}/{}",
            expected.len()
        )));
        assert_eq!([first, rest].concat(), expected);
        c.bye();
    });
    let (result, sink) = run_send(&target, &payload_file(&dir), &[path], &Cancel::new());
    fake.join().unwrap();
    let report = result.unwrap();
    assert!(report.items[0].transferred);
    assert_eq!(report.items[0].bytes_transferred, pkg.len() as u64);
    // Progress never exceeds the package size, however often bytes were re-sent.
    assert!(sink.events().iter().all(|e| e.done <= pkg.len() as u64));
}

#[test]
fn cancel_mid_batch_keeps_transferred_items() {
    let dir = TestDir::new();
    let pkgs = vec![
        dir.write("a.pkg", &package("A", "gd", 4)),
        dir.write("b.pkg", &package("B", "gd", 5)),
    ];
    let first = std::fs::read(&pkgs[0]).unwrap();
    let cancel = Cancel::new();
    let remote = cancel.clone();
    let (target, fake) = console(LOCALHOST, move |loader| {
        let cfg = loader.receive();
        let mut c = Control::hello(&cfg);
        let job = c.job();
        c.registered(0);
        download(&job, &first);
        let second = c.job();
        assert_eq!(second.index, 1);
        remote.cancel();
        c.bye();
    });
    let (result, _) = run_send(&target, &payload_file(&dir), &pkgs, &cancel);
    fake.join().unwrap();
    let e = result.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Cancelled);
    let report = e.report().unwrap();
    assert_eq!(report.transferred(), 1);
    assert!(report.items[0].registered() && !report.items[1].transferred);
    assert!(
        e.to_string().contains("1 of 2 package(s) transferred"),
        "{e}"
    );
}

#[test]
fn console_failed_frames_stop_the_batch() {
    // Failure on the first package: the cause's own kind (network).
    let dir = TestDir::new();
    let pkgs = vec![
        dir.write("a.pkg", &package("A", "gd", 6)),
        dir.write("b.pkg", &package("B", "gd", 7)),
    ];
    let (target, fake) = console(LOCALHOST, |loader| {
        let cfg = loader.receive();
        let mut c = Control::hello(&cfg);
        let job = c.job();
        c.send(&Message::Failed(Failed {
            index: job.index,
            stage: Stage::Register,
            code: CONSOLE_ERROR,
        }));
        c.bye();
    });
    let (result, _) = run_send(&target, &payload_file(&dir), &pkgs, &Cancel::new());
    fake.join().unwrap();
    let e = result.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Network);
    let item = &e.report().unwrap().items[0];
    assert_eq!(item.failure.unwrap().code, CONSOLE_ERROR);
    assert!(e.to_string().contains("0x80990001"), "{e}");

    // Failure on the second package, after the first made it: partial (P13).
    let first = std::fs::read(&pkgs[0]).unwrap();
    let (target, fake) = console(LOCALHOST, move |loader| {
        let cfg = loader.receive();
        let mut c = Control::hello(&cfg);
        let job = c.job();
        c.registered(0);
        download(&job, &first);
        let job = c.job();
        c.send(&Message::Status(Status {
            index: job.index,
            state: TaskState::Error,
            bytes_done: Some(0),
            error_code: CONSOLE_ERROR,
        }));
        c.bye();
    });
    let (result, _) = run_send(&target, &payload_file(&dir), &pkgs, &Cancel::new());
    fake.join().unwrap();
    let e = result.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Partial);
    assert_eq!(e.kind().exit_code(), 10);
    let report = e.report().unwrap();
    assert_eq!(report.items[1].failure.unwrap().stage, Stage::Status);
}

#[test]
fn payload_leaving_after_registration_still_completes_the_current_package() {
    let dir = TestDir::new();
    let pkgs = vec![
        dir.write("a.pkg", &package("A", "gd", 17)),
        dir.write("b.pkg", &package("B", "gd", 18)),
    ];
    let first = std::fs::read(&pkgs[0]).unwrap();
    let (target, fake) = console(LOCALHOST, move |loader| {
        let cfg = loader.receive();
        let mut c = Control::hello(&cfg);
        let job = c.job();
        c.registered(0);
        // The payload ends (e.g. its idle timeout); the installer keeps downloading.
        drop(c);
        std::thread::sleep(std::time::Duration::from_millis(100));
        download(&job, &first);
    });
    let (result, _) = run_send(&target, &payload_file(&dir), &pkgs, &Cancel::new());
    fake.join().unwrap();
    let e = result.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Partial, "{e}");
    let report = e.report().unwrap();
    assert!(report.items[0].transferred && !report.items[1].transferred);
    assert!(
        e.to_string().contains("the payload closed the connection"),
        "{e}"
    );
}

#[test]
fn wrong_token_is_closed_and_the_real_payload_still_connects() {
    let dir = TestDir::new();
    let pkg = package("Token", "gd", 8);
    let path = dir.write("t.pkg", &pkg);
    let (target, fake) = console(LOCALHOST, move |loader| {
        let cfg = loader.receive();
        let mut impostor = Control::connect(&cfg);
        impostor.hello_with(VERSION, [0; TOKEN_LEN]);
        assert!(impostor.closed_by_host(), "a wrong token is dropped");
        let mut c = Control::hello(&cfg);
        let job = c.job();
        c.registered(0);
        download(&job, &pkg);
        c.bye();
    });
    let (result, _) = run_send(&target, &payload_file(&dir), &[path], &Cancel::new());
    fake.join().unwrap();
    assert_eq!(result.unwrap().transferred(), 1);
}

#[test]
fn version_mismatch_is_a_clear_error() {
    let dir = TestDir::new();
    let path = dir.write("v.pkg", &package("Version", "gd", 9));
    let (target, fake) = console(LOCALHOST, |loader| {
        let cfg = loader.receive();
        let mut c = Control::connect(&cfg);
        c.hello_with(VERSION + 1, cfg.token);
        assert!(c.closed_by_host());
    });
    let (result, _) = run_send(&target, &payload_file(&dir), &[path], &Cancel::new());
    fake.join().unwrap();
    let e = result.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Unsupported);
    assert!(
        e.to_string()
            .contains("payload is version 2, host expects 1"),
        "{e}"
    );
}

#[test]
fn malformed_console_frame_closes_the_session() {
    let dir = TestDir::new();
    let path = dir.write("m.pkg", &package("Malformed", "gd", 10));
    let (target, fake) = console(LOCALHOST, |loader| {
        let cfg = loader.receive();
        let mut c = Control::hello(&cfg);
        let _ = c.job();
        // An unknown frame type.
        c.0.write_all(&[1, 0, 0, 0, 0xEE]).unwrap();
        // The host ends the session (a best-effort Bye, then close).
        let mut rest = Vec::new();
        let _ = c.0.read_to_end(&mut rest);
    });
    let (result, _) = run_send(&target, &payload_file(&dir), &[path], &Cancel::new());
    fake.join().unwrap();
    let e = result.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Malformed, "{e}");
}

/// Linux routes all of 127.0.0.0/8 to loopback, so 127.0.0.2 can be "the console"
/// while our fake's connections come from 127.0.0.1: a foreign peer.
#[cfg(target_os = "linux")]
#[test]
fn foreign_peer_never_counts_as_the_payload() {
    let console_ip = Ipv4Addr::new(127, 0, 0, 2);
    let dir = TestDir::new();
    let path = dir.write("p.pkg", &package("Peer", "gd", 11));
    let (target, fake) = console(console_ip, |loader| {
        let cfg = loader.receive();
        // Right token, wrong source address (127.0.0.1).
        let mut c = Control::hello(&cfg);
        assert!(c.closed_by_host());
    });
    let (result, _) = run_send(&target, &payload_file(&dir), &[path], &Cancel::new());
    fake.join().unwrap();
    let e = result.unwrap_err();
    assert!(matches!(e, Error::NoConnectBack), "{e}");
    assert_eq!(e.kind(), ErrorKind::Network);
}

#[cfg(target_os = "linux")]
#[test]
fn http_server_closes_foreign_peers_and_bounds_connections() {
    let (tx, _rx) = mpsc::channel();
    let listener = TcpListener::bind((LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    // The console is 127.0.0.2; our connections come from 127.0.0.1.
    let server =
        http::Server::start(listener, Ipv4Addr::new(127, 0, 0, 2), "ab".into(), tx).unwrap();
    let mut s = TcpStream::connect(addr).unwrap();
    let _ = s.write_all(b"GET /ab/0/package.json HTTP/1.1\r\n\r\n");
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    assert!(out.is_empty(), "a foreign peer gets no response");
    server.stop();

    // At most MAX_CONNECTIONS at once: the next one gets 503.
    let (tx, _rx) = mpsc::channel();
    let listener = TcpListener::bind((LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = http::Server::start(listener, LOCALHOST, "ab".into(), tx).unwrap();
    let idle: Vec<TcpStream> = (0..http::MAX_CONNECTIONS)
        .map(|_| TcpStream::connect(addr).unwrap())
        .collect();
    // Let the accept loop register all four.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut extra = TcpStream::connect(addr).unwrap();
    let mut out = Vec::new();
    let _ = extra.read_to_end(&mut out);
    assert!(
        out.starts_with(b"HTTP/1.1 503"),
        "{}",
        String::from_utf8_lossy(&out)
    );
    // Stopping closes idle connections instead of waiting out their timeouts.
    server.stop();
    for mut s in idle {
        let mut b = [0u8; 1];
        assert!(matches!(s.read(&mut b), Ok(0) | Err(_)));
        let _ = s.shutdown(Shutdown::Both);
    }
}

#[test]
fn invalid_batch_sends_nothing() {
    let dir = TestDir::new();
    let good = dir.write("good.pkg", &package("Good", "gd", 12));
    let bad = dir.write("bad.pkg", b"not a package at all");
    let long_title = "T".repeat(hermes_wire::limits::NAME as usize + 1);
    let long = dir.write("long.pkg", &package(&long_title, "gd", 13));
    let loader = Loader_::on(LOCALHOST);
    let target = loader.target();
    let payload = payload_file(&dir);
    for (pkgs, kind) in [
        (vec![good.clone(), bad], ErrorKind::Unsupported),
        (vec![long, good], ErrorKind::Limit),
        (vec![], ErrorKind::Usage),
    ] {
        let (result, _) = run_send(&target, &payload, &pkgs, &Cancel::new());
        let e = result.unwrap_err();
        assert_eq!(e.kind(), kind, "{e}");
        assert!(e.report().is_none());
    }
    loader.listener.set_nonblocking(true).unwrap();
    assert!(
        loader.listener.accept().is_err(),
        "the loader was never contacted"
    );
}

#[test]
fn payload_without_config_block_is_refused_before_the_network() {
    let dir = TestDir::new();
    let pkg = dir.write("a.pkg", &package("A", "gd", 14));
    let payload = Payload::File(dir.write("bogus.bin", b"no marker here"));
    let loader = Loader_::on(LOCALHOST);
    let (result, _) = run_send(&loader.target(), &payload, &[pkg], &Cancel::new());
    let e = result.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Malformed, "{e}");
    loader.listener.set_nonblocking(true).unwrap();
    assert!(loader.listener.accept().is_err());
}

#[test]
fn probe_reports_the_payload_build() {
    let dir = TestDir::new();
    let (target, fake) = console(LOCALHOST, |loader| {
        let cfg = loader.receive();
        let mut c = Control::hello(&cfg);
        c.bye();
    });
    let report = probe(&target, &payload_file(&dir)).unwrap();
    fake.join().unwrap();
    assert_eq!(report.build_id, BUILD_ID);
    assert_eq!(report.payload_version, VERSION);
    assert_eq!(
        Some(report.loader_port),
        target.loader.ports().first().copied()
    );
}

#[test]
fn unreachable_loader_is_a_network_error() {
    let dir = TestDir::new();
    // Bind and drop: nothing listens on this port any more.
    let port = TcpListener::bind((LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let target = Target {
        ps4: LOCALHOST,
        loader: Loader::Tcp { port },
    };
    let e = probe(&target, &payload_file(&dir)).unwrap_err();
    assert!(matches!(e, Error::LoaderUnreachable { .. }), "{e}");
    assert_eq!(e.kind(), ErrorKind::Network);
}

#[test]
fn range_parsing_matches_pkg_tools() {
    let part = |start, end_inclusive| Range::Part {
        start,
        end_inclusive,
    };
    assert_eq!(parse_range(None, 100), Range::Whole);
    assert_eq!(parse_range(Some("bytes=0-9"), 100), part(0, 9));
    assert_eq!(parse_range(Some("bytes=90-"), 100), part(90, 99));
    assert_eq!(parse_range(Some("bytes=-10"), 100), part(90, 99));
    assert_eq!(parse_range(Some("bytes=-500"), 100), part(0, 99));
    assert_eq!(parse_range(Some("bytes=50-5000"), 100), part(50, 99));
    for bad in [
        "bytes=100-",
        "bytes=0-1,4-5",
        "bytes=5-2",
        "bytes=-0",
        "bytes=x-",
        "items=0-1",
        "bytes=0",
    ] {
        assert_eq!(parse_range(Some(bad), 100), Range::Unsatisfiable, "{bad}");
    }
    assert_eq!(parse_range(Some("bytes=0-"), 0), Range::Unsatisfiable);
}

#[test]
fn coverage_merges_and_counts_unique_bytes() {
    let mut c = Coverage::default();
    c.add(50, 100);
    c.add(0, 25);
    assert!(!c.is_complete(100));
    c.add(20, 60);
    assert!(c.is_complete(100));
    assert_eq!(c.bytes(), 100);
    let mut c = Coverage::default();
    c.add(50, 100);
    c.add(75, 100);
    c.add(10, 10);
    assert_eq!(c.bytes(), 50);
}

#[test]
fn routes_need_the_token_and_a_plain_job_number() {
    assert_eq!(route("/ab/3/package.pkg", "ab"), Route::Package(3));
    assert_eq!(route("/ab/0/package.json", "ab"), Route::Manifest(0));
    for bad in [
        "/ac/3/package.pkg",
        "/ab/+3/package.pkg",
        "/ab/3/other",
        "/ab/3/package.pkg/x",
        "ab/3/package.pkg",
        "/",
    ] {
        assert_eq!(route(bad, "ab"), Route::NotFound, "{bad}");
    }
}

#[test]
fn request_heads_are_bounded() {
    let mut big = b"GET / HTTP/1.1\r\nX: ".to_vec();
    big.extend(std::iter::repeat_n(b'a', http::MAX_REQUEST_HEAD));
    assert_eq!(
        http::read_request(&mut big.as_slice()).unwrap(),
        Err(http::BadRequest::TooLarge)
    );
    let ok = b"HEAD /a?b=c HTTP/1.1\r\nrange: bytes=1-2\r\n\r\n";
    let r = http::read_request(&mut ok.as_slice()).unwrap().unwrap();
    assert_eq!(
        (r.head_only, r.path.as_str(), r.range.as_deref()),
        (true, "/a", Some("bytes=1-2"))
    );
}

fn cli(args: &[&str]) -> (u8, String, String) {
    let argv: Vec<OsString> = args.iter().map(OsString::from).collect();
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
fn cli_usage_errors() {
    assert_eq!(cli(&[]).0, 2);
    let (code, out, _) = cli(&["--help"]);
    assert!(code == 0 && out.contains("hermes send PKG..."), "{out}");
    assert_eq!(cli(&["send", "a.pkg"]).0, 2, "--ps4 is required");
    assert_eq!(cli(&["send", "--ps4", "1.2.3", "a.pkg"]).0, 2);
    assert_eq!(cli(&["send", "--ps4", "::1", "a.pkg"]).0, 2, "IPv4 only");
    assert_eq!(cli(&["send", "--ps4", "10.0.0.2"]).0, 2, "at least one PKG");
    let (code, _, err) = cli(&["probe", "--ps4", "10.0.0.2", "--loader", "tcp:0"]);
    assert_eq!(code, 2, "{err}");
    assert_eq!(cli(&["probe", "--ps4", "10.0.0.2", "--loader", "ftp"]).0, 2);
    #[cfg(not(feature = "embed-payload"))]
    {
        let (code, _, err) = cli(&["probe", "--ps4", "10.0.0.2"]);
        assert!(
            code == 2 && err.contains("--payload FILE is required"),
            "{err}"
        );
    }
}

#[test]
fn cli_send_json_report() {
    let dir = TestDir::new();
    let pkg = package("Cli", "gd", 15);
    let path = dir.write("c.pkg", &pkg);
    let payload = dir.write("payload.bin", &payload_image());
    let (target, fake) = console(LOCALHOST, move |loader| {
        let cfg = loader.receive();
        let mut c = Control::hello(&cfg);
        let job = c.job();
        c.registered(0);
        download(&job, &pkg);
        c.bye();
    });
    let Loader::Tcp { port } = target.loader else {
        unreachable!()
    };
    let loader = format!("tcp:{port}");
    let (code, out, err) = cli(&[
        "send",
        "--ps4",
        "127.0.0.1",
        "--loader",
        &loader,
        "--payload",
        payload.to_str().unwrap(),
        "--json",
        path.to_str().unwrap(),
    ]);
    fake.join().unwrap();
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("{\"schema\":\"pantheon.hermes.send/1\""),
        "{out}"
    );
    assert!(
        out.contains("\"transferred\":true,\"registered\":true,\"task_id\":256"),
        "{out}"
    );
    assert!(out.contains("\"installed\":false"));
    assert!(out.contains("\"error\":null"));
}

#[test]
fn cli_failure_prints_the_report_then_the_error() {
    let dir = TestDir::new();
    let path = dir.write("f.pkg", &package("Fails", "gd", 16));
    let payload = dir.write("payload.bin", &payload_image());
    let (target, fake) = console(LOCALHOST, |loader| {
        let cfg = loader.receive();
        let mut c = Control::hello(&cfg);
        let job = c.job();
        c.send(&Message::Failed(Failed {
            index: job.index,
            stage: Stage::Register,
            code: CONSOLE_ERROR,
        }));
        c.bye();
    });
    let Loader::Tcp { port } = target.loader else {
        unreachable!()
    };
    let loader = format!("tcp:{port}");
    let (code, out, err) = cli(&[
        "send",
        "--ps4",
        "127.0.0.1",
        "--loader",
        &loader,
        "--payload",
        payload.to_str().unwrap(),
        path.to_str().unwrap(),
    ]);
    fake.join().unwrap();
    assert_eq!(code, 9, "{err}");
    assert!(
        out.contains(
            "1/1 Fails: 0.0 / 0.2 MB transferred, failed at register with code 0x80990001"
        ),
        "{out}"
    );
    assert!(err.contains("0 of 1 package(s) transferred"), "{err}");
}
