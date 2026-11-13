//! The HTTP range server the console's installer downloads from
//! (hermes.md "Security"; behaviour as pkg-tools `ps4-send`).
//!
//! Only `GET`/`HEAD` of the current job's `/<token-hex>/<n>/package.pkg` and
//! `…/package.json` are served; every other path is `404`. One request per
//! connection (`Connection: close`), at most [`MAX_CONNECTIONS`] at once, one
//! thread each (architecture §5). When all are taken, the one that has sent
//! nothing for [`STALE_AFTER`] is closed to make room (D37): a half-open
//! connection must not lock the installer out of resuming. Connections from
//! any address but the console's are closed at once.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pantheon_core::CHUNK;

use crate::session::Event;

/// Concurrent HTTP connections (architecture §5).
pub(crate) const MAX_CONNECTIONS: usize = 4;
/// Request line plus headers.
pub(crate) const MAX_REQUEST_HEAD: usize = 8 * 1024;
/// The installer pauses while it validates metadata (pkg-tools: 10 min).
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// A full server closes a connection that has sent nothing for this long (D37).
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 60 });
/// How often the accept loop checks for a stop request.
const ACCEPT_POLL: Duration = Duration::from_millis(10);
const PACKAGE_FILE: &str = "package.pkg";
const MANIFEST_FILE: &str = "package.json";

/// What the server may serve right now: one job's package and manifest.
#[derive(Debug)]
pub(crate) struct Current {
    pub index: u32,
    pub path: PathBuf,
    pub size: u64,
    pub manifest: Vec<u8>,
}

/// Bytes `[start, end)` of `total`, or a request that cannot be satisfied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Range {
    Whole,
    Part { start: u64, end_inclusive: u64 },
    Unsatisfiable,
}

/// Parses a `Range` header value against `total` bytes. Single ranges only;
/// multi-range and malformed values are unsatisfiable (pkg-tools behaviour).
pub(crate) fn parse_range(value: Option<&str>, total: u64) -> Range {
    let Some(value) = value else {
        return Range::Whole;
    };
    let unsatisfiable = Range::Unsatisfiable;
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return unsatisfiable;
    };
    let Some(last) = total.checked_sub(1) else {
        return unsatisfiable;
    };
    if spec.contains(',') {
        return unsatisfiable;
    }
    let Some((start, end)) = spec.split_once('-') else {
        return unsatisfiable;
    };
    if start.is_empty() {
        // `bytes=-N`: the last N bytes.
        return match end.parse::<u64>() {
            Ok(suffix) if suffix > 0 => Range::Part {
                start: total.saturating_sub(suffix),
                end_inclusive: last,
            },
            _ => unsatisfiable,
        };
    }
    let Ok(start) = start.parse::<u64>() else {
        return unsatisfiable;
    };
    if start > last {
        return unsatisfiable;
    }
    let end = if end.is_empty() {
        last
    } else {
        match end.parse::<u64>() {
            Ok(end) => end.min(last),
            Err(_) => return unsatisfiable,
        }
    };
    if end < start {
        return unsatisfiable;
    }
    Range::Part {
        start,
        end_inclusive: end,
    }
}

/// Unique bytes served for one package, merged across requests. Bytes from
/// interrupted requests count: the installer resumes without asking again.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Coverage {
    /// Sorted, disjoint, non-adjacent `[start, end)` ranges.
    ranges: Vec<(u64, u64)>,
}

impl Coverage {
    pub(crate) fn add(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        self.ranges.push((start, end));
        self.ranges.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.ranges.len());
        for &(s, e) in &self.ranges {
            match merged.last_mut() {
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        self.ranges = merged;
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.ranges
            .iter()
            .fold(0u64, |sum, (s, e)| sum.saturating_add(e.saturating_sub(*s)))
    }

    pub(crate) fn is_complete(&self, size: u64) -> bool {
        self.ranges.as_slice() == [(0, size)]
    }
}

/// Server state shared with connection threads.
struct Shared {
    console: Ipv4Addr,
    token_hex: String,
    current: Mutex<Option<Arc<Current>>>,
    stop: AtomicBool,
    /// Live connections, oldest first, so `stop` and eviction can shut them down.
    live: Mutex<Vec<Live>>,
    events: Mutex<Sender<Event>>,
}

/// A running server; `stop` closes the listener and every connection.
pub(crate) struct Server {
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    pub(crate) fn start(
        listener: TcpListener,
        console: Ipv4Addr,
        token_hex: String,
        events: Sender<Event>,
    ) -> io::Result<Server> {
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            console,
            token_hex,
            current: Mutex::new(None),
            stop: AtomicBool::new(false),
            live: Mutex::new(Vec::new()),
            events: Mutex::new(events),
        });
        let s = Arc::clone(&shared);
        let accept = std::thread::spawn(move || accept_loop(&listener, &s));
        Ok(Server {
            shared,
            accept: Some(accept),
        })
    }

    pub(crate) fn set_current(&self, current: Option<Arc<Current>>) {
        *lock(&self.shared.current) = current;
    }

    pub(crate) fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for Live { stream, .. } in lock(&self.shared.live).iter() {
            // Already-closed sockets fail here; there is nothing left to close.
            let _ = stream.shutdown(Shutdown::Both);
        }
        if let Some(handle) = self.accept.take() {
            // A panicked accept thread has already stopped serving.
            let _ = handle.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.halt();
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    let mut next_id: u64 = 0;
    while !shared.stop.load(Ordering::Relaxed) {
        workers.retain(|w| !w.is_finished());
        match listener.accept() {
            Ok((stream, peer)) => {
                if peer.ip() != IpAddr::V4(shared.console) {
                    // Peer check: anyone but the console is closed at once.
                    drop(stream);
                    continue;
                }
                if !make_room(shared) {
                    let mut stream = stream;
                    let _ = write_status(&mut stream, "503 Service Unavailable");
                    continue;
                }
                next_id = next_id.wrapping_add(1);
                if let Some(worker) = spawn_worker(stream, next_id, shared) {
                    workers.push(worker);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => std::thread::sleep(ACCEPT_POLL),
        }
    }
    // A worker registered after `halt` shut the others down is closed here.
    for Live { stream, .. } in lock(&shared.live).iter() {
        let _ = stream.shutdown(Shutdown::Both);
    }
    for w in workers {
        let _ = w.join();
    }
}

fn spawn_worker(stream: TcpStream, id: u64, shared: &Arc<Shared>) -> Option<JoinHandle<()>> {
    // Accepted sockets may inherit non-blocking mode on some platforms.
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok()?;
    lock(&shared.live).push(Live {
        id,
        stream: stream.try_clone().ok()?,
        active: Instant::now(),
    });
    let shared = Arc::clone(shared);
    Some(std::thread::spawn(move || {
        let mut stream = stream;
        let outcome = serve(&mut stream, id, &shared);
        if let Err((index, error)) = outcome {
            let _ = lock(&shared.events).send(Event::HttpFailed { index, error });
        }
        lock(&shared.live).retain(|l| l.id != id);
    }))
}

/// One live connection.
struct Live {
    id: u64,
    stream: TcpStream,
    /// When it was accepted or last sent package bytes.
    active: Instant,
}

/// True if a new connection may be served: below the limit, or after closing
/// the stalest connection once it has sent nothing for [`STALE_AFTER`]. The
/// closed connection's thread sees a broken pipe and ends without failing
/// the batch; its bytes sent so far still count.
fn make_room(shared: &Shared) -> bool {
    let mut live = lock(&shared.live);
    if live.len() < MAX_CONNECTIONS {
        return true;
    }
    let Some(pos) = live
        .iter()
        .enumerate()
        .min_by_key(|(_, l)| l.active)
        .map(|(pos, _)| pos)
    else {
        return true;
    };
    if live
        .get(pos)
        .is_none_or(|l| l.active.elapsed() < STALE_AFTER)
    {
        return false;
    }
    let stale = live.remove(pos);
    // Already-closed sockets fail here; there is nothing left to close.
    let _ = stale.stream.shutdown(Shutdown::Both);
    true
}

/// Records that connection `id` made progress.
fn touch(shared: &Shared, id: u64) {
    if let Some(l) = lock(&shared.live).iter_mut().find(|l| l.id == id) {
        l.active = Instant::now();
    }
}

/// A parsed request head.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub head_only: bool,
    pub path: String,
    pub range: Option<String>,
}

/// Why a request head was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BadRequest {
    TooLarge,
    Malformed,
    Method,
}

/// Reads one request head (bounded by [`MAX_REQUEST_HEAD`]).
pub(crate) fn read_request(r: &mut impl Read) -> io::Result<Result<Request, BadRequest>> {
    let mut head = Vec::with_capacity(MAX_REQUEST_HEAD);
    let mut byte = [0u8; 1];
    // Byte at a time: the head is small and must not over-read past its end.
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_REQUEST_HEAD {
            return Ok(Err(BadRequest::TooLarge));
        }
        if r.read(&mut byte)? == 0 {
            return Ok(Err(BadRequest::Malformed));
        }
        head.extend_from_slice(&byte);
    }
    let Ok(text) = std::str::from_utf8(&head) else {
        return Ok(Err(BadRequest::Malformed));
    };
    let mut lines = text.split("\r\n");
    let mut parts = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(_version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Ok(Err(BadRequest::Malformed));
    };
    let head_only = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return Ok(Err(BadRequest::Method)),
    };
    let path = target.split('?').next().unwrap_or_default().to_owned();
    let range = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("range"))
        .map(|(_, v)| v.trim().to_owned());
    Ok(Ok(Request {
        head_only,
        path,
        range,
    }))
}

/// What a request path names, relative to the session token.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Package(u32),
    Manifest(u32),
    NotFound,
}

pub(crate) fn route(path: &str, token_hex: &str) -> Route {
    let mut parts = path.strip_prefix('/').unwrap_or_default().split('/');
    let (Some(token), Some(n), Some(file), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Route::NotFound;
    };
    // `parse` alone would also accept "+1"; job numbers are plain digits.
    let index = match n.parse::<u32>() {
        Ok(i) if token == token_hex && n.bytes().all(|b| b.is_ascii_digit()) => i,
        _ => return Route::NotFound,
    };
    match file {
        PACKAGE_FILE => Route::Package(index),
        MANIFEST_FILE => Route::Manifest(index),
        _ => Route::NotFound,
    }
}

/// `Err((job index, error))` is a host-side failure that stops the batch;
/// client disconnects are not errors (bytes sent so far still count).
fn serve(stream: &mut TcpStream, id: u64, shared: &Shared) -> Result<(), (u32, io::Error)> {
    let request = match read_request(stream) {
        Ok(Ok(r)) => r,
        Ok(Err(BadRequest::Method)) => {
            let _ = write_status(stream, "405 Method Not Allowed");
            return Ok(());
        }
        Ok(Err(BadRequest::TooLarge)) => {
            let _ = write_status(stream, "431 Request Header Fields Too Large");
            return Ok(());
        }
        Ok(Err(BadRequest::Malformed)) => {
            let _ = write_status(stream, "400 Bad Request");
            return Ok(());
        }
        Err(_) => return Ok(()),
    };
    let route = route(&request.path, &shared.token_hex);
    let current = lock(&shared.current).clone();
    let found = match route {
        Route::Manifest(i) | Route::Package(i) => current.filter(|c| c.index == i),
        Route::NotFound => None,
    };
    let Some(c) = found else {
        // A 404 that cannot be written is the client's problem, not the batch's.
        let _ = write_status(stream, "404 Not Found");
        return Ok(());
    };
    let result = match route {
        Route::Manifest(_) => {
            serve_manifest(stream, &c, request.head_only).map_err(|e| (c.index, e))
        }
        Route::Package(_) => serve_package(stream, id, &c, &request, shared),
        Route::NotFound => Ok(()),
    };
    match result {
        Err((_, e)) if disconnected(&e) || shared.stop.load(Ordering::Relaxed) => Ok(()),
        other => other,
    }
}

/// Client-side ends of a connection: recoverable, the installer retries.
fn disconnected(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::NotConnected
    )
}

fn serve_manifest(stream: &mut TcpStream, c: &Current, head_only: bool) -> io::Result<()> {
    let len = u64::try_from(c.manifest.len()).unwrap_or(u64::MAX);
    write_headers(stream, "200 OK", len, None, "application/json")?;
    if !head_only {
        stream.write_all(&c.manifest)?;
    }
    stream.flush()
}

fn serve_package(
    stream: &mut TcpStream,
    id: u64,
    c: &Current,
    request: &Request,
    shared: &Shared,
) -> Result<(), (u32, io::Error)> {
    let fail = |e| (c.index, e);
    let mut file = File::open(&c.path).map_err(fail)?;
    let total = file.metadata().map_err(fail)?.len();
    if total != c.size {
        return Err(fail(io::Error::other("package changed during transfer")));
    }
    let (start, len) = match parse_range(request.range.as_deref(), total) {
        Range::Whole => (0, total),
        Range::Part {
            start,
            end_inclusive,
        } => (start, end_inclusive.saturating_sub(start).saturating_add(1)),
        Range::Unsatisfiable => {
            let head = format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            return stream.write_all(head.as_bytes()).map_err(fail);
        }
    };
    let partial = start != 0 || len != total;
    let content_range = partial.then(|| {
        format!(
            "bytes {start}-{}/{total}",
            start.saturating_add(len).saturating_sub(1)
        )
    });
    let status = if partial {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    write_headers(
        stream,
        status,
        len,
        content_range.as_deref(),
        "application/octet-stream",
    )
    .map_err(fail)?;
    if request.head_only {
        return Ok(());
    }
    file.seek(SeekFrom::Start(start)).map_err(fail)?;
    let mut buf = vec![0u8; CHUNK];
    let mut at = start;
    let end = start.saturating_add(len);
    while at < end {
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let want = usize::try_from(end.saturating_sub(at))
            .unwrap_or(CHUNK)
            .min(CHUNK);
        let chunk = buf.get_mut(..want).unwrap_or_default();
        let n = file.read(chunk).map_err(fail)?;
        if n == 0 {
            return Err(fail(io::Error::other("package changed during transfer")));
        }
        stream
            .write_all(chunk.get(..n).unwrap_or_default())
            .map_err(fail)?;
        touch(shared, id);
        let next = at.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        let _ = lock(&shared.events).send(Event::Bytes {
            index: c.index,
            start: at,
            end: next,
        });
        at = next;
    }
    stream.flush().map_err(fail)
}

fn write_headers(
    stream: &mut impl Write,
    status: &str,
    len: u64,
    content_range: Option<&str>,
    content_type: &str,
) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nContent-Type: {content_type}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
    );
    if let Some(range) = content_range {
        head.push_str("Content-Range: ");
        head.push_str(range);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())
}

fn write_status(stream: &mut impl Write, status: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}
