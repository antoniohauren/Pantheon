//! One payload session: route, listeners, loader push, `Hello`, the job loop
//! and `Bye` (hermes.md "Components" and "Security").

use std::io::{self, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket,
};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hermes_wire::{
    Failed, HEADER_LEN, Hello, Job, Kind, Message, Registered, Stage, Status, TOKEN_LEN, TaskState,
    config, decode_header,
};
use pantheon_core::{Cancel, Ctx, Phase, Progress};
use pantheon_json::JsonWriter;

use crate::batch::Item;
use crate::http::{self, Coverage, Current};
use crate::ops::{ConsoleFailure, Error, ItemReport, Target};

/// How long the payload has to connect back after the loader took it
/// (pkg-tools: 15 s). Tests that expect no connection wait less.
pub(crate) const CONNECT_BACK_TIMEOUT: Duration =
    Duration::from_secs(if cfg!(test) { 3 } else { 15 });
/// How long a connected peer has to send `Hello`.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// Loader connect and write timeouts (pkg-tools).
const LOADER_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const LOADER_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// A `Job` carries up to 4 MiB of icon.
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// After a package is transferred, how long to wait for its `Registered`.
const REGISTER_GRACE: Duration = Duration::from_secs(2);
/// How often the host tells the payload it is still there (D37): well inside
/// the payload's 5-minute idle timeout (D27), so a long download keeps its
/// session and the next `Job` can still be sent.
pub(crate) const KEEPALIVE_INTERVAL: Duration =
    Duration::from_millis(if cfg!(test) { 200 } else { 60_000 });
/// Event-loop tick: cancellation is checked at least this often.
const POLL: Duration = Duration::from_millis(10);
/// UDP "discard" port: `connect` on a UDP socket only picks a route, no packet is sent.
const ROUTE_PROBE_PORT: u16 = 9;
/// `pieces[].hashValue` in the manifest: pkg-tools sends zeros (no per-piece hash).
const ZERO_PIECE_HASH: &str = "0000000000000000000000000000000000000000";

/// What the session loop reacts to.
#[derive(Debug)]
pub(crate) enum Event {
    /// Package bytes `[start, end)` of job `index` were written to the console.
    Bytes { index: u32, start: u64, end: u64 },
    /// A host-side failure while serving job `index`.
    HttpFailed { index: u32, error: io::Error },
    /// A frame from the payload, or why the control connection ended.
    Console(Result<FromConsole, Error>),
}

/// The frames a payload may send after `Hello`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FromConsole {
    Registered(Registered),
    Failed(Failed),
    Status(Status),
}

pub(crate) struct Session {
    pub loader_port: u16,
    pub hello: Hello,
    control: TcpStream,
    reader: Option<JoinHandle<()>>,
    server: Option<http::Server>,
    events: Receiver<Event>,
    /// `http://<local>:<port>/<token-hex>`; never logged.
    base_url: String,
}

impl Session {
    /// Binds both listeners, sends the patched payload and waits for its `Hello`.
    pub(crate) fn open(
        target: &Target,
        mut image: Vec<u8>,
        cancel: &Cancel,
    ) -> Result<Session, Error> {
        config::find(&image).map_err(Error::Payload)?;
        let mut token = [0u8; TOKEN_LEN];
        pantheon_fs::os_random(&mut token).map_err(Error::Random)?;
        let local = local_address(target.ps4).map_err(Error::NoRoute)?;
        let bind = |what| {
            TcpListener::bind(SocketAddrV4::new(local, 0))
                .map_err(|source| Error::Io { what, source })
        };
        let control_listener = bind("bind the control listener")?;
        let http_listener = bind("bind the HTTP listener")?;
        let port_of = |l: &TcpListener| {
            l.local_addr()
                .map(|a| a.port())
                .map_err(|source| Error::Io {
                    what: "read a listener address",
                    source,
                })
        };
        let control_port = port_of(&control_listener)?;
        let http_port = port_of(&http_listener)?;
        config::patch(&mut image, local.octets(), control_port, &token).map_err(Error::Payload)?;

        let loader_port = push(target, &image)?;
        let (control, hello) = accept_payload(&control_listener, target.ps4, &token, cancel)?;
        // One payload connection per batch: nobody else is accepted from here on.
        drop(control_listener);

        let token_hex = hex(&token);
        let (tx, events) = mpsc::channel();
        let server = http::Server::start(http_listener, target.ps4, token_hex.clone(), tx.clone())
            .map_err(|source| Error::Io {
                what: "start the HTTP server",
                source,
            })?;
        let reader = spawn_reader(&control, tx)?;
        Ok(Session {
            loader_port,
            hello,
            control,
            reader: Some(reader),
            server: Some(server),
            events,
            base_url: format!("http://{local}:{http_port}/{token_hex}"),
        })
    }

    /// Sends every item in order; stops at the first error or cancellation.
    pub(crate) fn run(
        &mut self,
        items: &[Item],
        reports: &mut [ItemReport],
        ctx: &Ctx<'_>,
    ) -> Result<(), Error> {
        // `Keepalive` holds the payload's session open during long downloads
        // (D37). If the control connection is still lost once the installer
        // has registered the job, the current package is still served to the
        // end, but no further `Job` can be sent.
        let mut control_lost: Option<Error> = None;
        for (i, item) in items.iter().enumerate() {
            if let Some(e) = control_lost.take() {
                return Err(e);
            }
            let index = u32::try_from(i).map_err(|_| Error::Field {
                path: item.path.clone(),
                field: "batch position",
                problem: "exceeds the protocol's job index",
            })?;
            self.send_job(index, item)?;
            let mut last_frame = Instant::now();
            let mut coverage = Coverage::default();
            let mut transferred_at = None;
            loop {
                ctx.cancel.check().map_err(|_| Error::Cancelled)?;
                let registered = reports.get(i).is_some_and(|r| r.registered());
                if transferred_at
                    .is_some_and(|t: Instant| registered || t.elapsed() >= REGISTER_GRACE)
                {
                    break;
                }
                if control_lost.is_none() && last_frame.elapsed() >= KEEPALIVE_INTERVAL {
                    last_frame = Instant::now();
                    if let Err(e) = write_message(&mut self.control, &Message::Keepalive) {
                        if !registered {
                            return Err(e);
                        }
                        control_lost = Some(e);
                    }
                }
                match self.events.recv_timeout(POLL) {
                    Ok(Event::Bytes {
                        index: j,
                        start,
                        end,
                    }) if j == index => {
                        coverage.add(start, end);
                        let done = coverage.bytes();
                        if let Some(r) = reports.get_mut(i) {
                            r.bytes_transferred = done;
                        }
                        ctx.progress.report(&Progress {
                            phase: Phase::Sending,
                            done,
                            total: Some(item.size),
                            item: Some(&item.name),
                        });
                        if transferred_at.is_none() && coverage.is_complete(item.size) {
                            if let Some(r) = reports.get_mut(i) {
                                r.transferred = true;
                            }
                            transferred_at = Some(Instant::now());
                        }
                    }
                    Ok(Event::HttpFailed { index: j, error }) if j == index => {
                        return Err(Error::Http(error));
                    }
                    // Late events of an earlier, already transferred job.
                    Ok(Event::Bytes { .. } | Event::HttpFailed { .. }) => {}
                    Ok(Event::Console(Err(e @ Error::Control(_))))
                        if control_lost.is_none()
                            && reports.get(i).is_some_and(ItemReport::registered) =>
                    {
                        control_lost = Some(e);
                    }
                    // Already lost (a failed `Keepalive` came first): keep serving.
                    Ok(Event::Console(Err(Error::Control(_)))) if control_lost.is_some() => {}
                    Ok(Event::Console(frame)) => apply(frame?, reports)?,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(Error::Control(io::Error::other(
                            "the session's event channel closed",
                        )));
                    }
                }
            }
        }
        if let Some(server) = &self.server {
            server.set_current(None);
        }
        // Frames already received (late `Status`) still belong in the report.
        while let Ok(event) = self.events.try_recv() {
            if let Event::Console(Ok(frame)) = event {
                apply(frame, reports)?;
            }
        }
        Ok(())
    }

    fn send_job(&mut self, index: u32, item: &Item) -> Result<(), Error> {
        let package_url = format!("{}/{index}/package.pkg", self.base_url);
        let manifest_url = format!("{}/{index}/package.json", self.base_url);
        if let Some(server) = &self.server {
            server.set_current(Some(Arc::new(Current {
                index,
                path: item.path.clone(),
                size: item.size,
                manifest: manifest(&package_url, item),
            })));
        }
        let job = Message::Job(Job {
            index,
            manifest_url: &manifest_url,
            name: &item.name,
            content_id: &item.content_id,
            package_type: &item.package_type,
            size: item.size,
            digest: item.digest,
            icon: item.icon.as_deref(),
        });
        write_message(&mut self.control, &job)
    }

    /// Sends `Bye`, closes the control connection and both listeners.
    pub(crate) fn close(mut self) {
        // The payload may already be gone; closing is best effort.
        let _ = write_message(&mut self.control, &Message::Bye);
        let _ = self.control.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(server) = self.server.take() {
            server.stop();
        }
    }
}

/// Applies one console frame to the report; console failures stop the batch.
fn apply(frame: FromConsole, reports: &mut [ItemReport]) -> Result<(), Error> {
    let index = match frame {
        FromConsole::Registered(r) => r.index,
        FromConsole::Failed(f) => f.index,
        FromConsole::Status(s) => s.index,
    };
    let report = usize::try_from(index)
        .ok()
        .and_then(|i| reports.get_mut(i))
        .ok_or(Error::Wire(hermes_wire::Error::Malformed(
            "job index outside the batch",
        )))?;
    let failure = match frame {
        FromConsole::Registered(r) => {
            report.task_id = Some(r.task_id);
            None
        }
        FromConsole::Failed(f) => Some(ConsoleFailure {
            stage: f.stage,
            code: f.code,
        }),
        FromConsole::Status(s) => {
            report.state = Some(s.state);
            match s.state {
                TaskState::Done => {
                    report.installed = true;
                    None
                }
                TaskState::Error => Some(ConsoleFailure {
                    stage: Stage::Status,
                    code: s.error_code,
                }),
                TaskState::Queued | TaskState::Downloading | TaskState::Installing => None,
            }
        }
    };
    match failure {
        Some(failure) => {
            report.failure = Some(failure);
            Err(Error::Console { index, failure })
        }
        None => Ok(()),
    }
}

/// The local IPv4 address that routes to `ps4`. Loopback only for a loopback
/// target (tests); a LAN target must route over a LAN interface.
fn local_address(ps4: Ipv4Addr) -> io::Result<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect((ps4, ROUTE_PROBE_PORT))?;
    match socket.local_addr()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() && ip.is_loopback() == ps4.is_loopback() => Ok(ip),
        _ => Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no IPv4 LAN route to the PS4",
        )),
    }
}

/// Sends `image` to the first loader port that accepts; returns that port.
fn push(target: &Target, image: &[u8]) -> Result<u16, Error> {
    let mut attempts = Vec::new();
    for &port in target.loader.ports() {
        let addr = SocketAddr::from((target.ps4, port));
        match TcpStream::connect_timeout(&addr, LOADER_CONNECT_TIMEOUT) {
            Ok(mut stream) => {
                let sent = stream
                    .set_write_timeout(Some(LOADER_WRITE_TIMEOUT))
                    .and_then(|()| stream.write_all(image))
                    .and_then(|()| stream.flush());
                sent.map_err(|source| Error::LoaderSend { port, source })?;
                // Signals the end of the payload; a loader that already closed is fine.
                let _ = stream.shutdown(Shutdown::Write);
                return Ok(port);
            }
            Err(e) => attempts.push((port, e)),
        }
    }
    Err(Error::LoaderUnreachable { attempts })
}

/// Equal without an early exit, so timing does not reveal the matching prefix.
fn same_token(a: &[u8; TOKEN_LEN], b: &[u8; TOKEN_LEN]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Waits for the payload's connection: the console's address and our token.
/// Other peers and wrong tokens are closed, and waiting continues.
fn accept_payload(
    listener: &TcpListener,
    console: Ipv4Addr,
    token: &[u8; TOKEN_LEN],
    cancel: &Cancel,
) -> Result<(TcpStream, Hello), Error> {
    let io_error = |what| move |source| Error::Io { what, source };
    listener
        .set_nonblocking(true)
        .map_err(io_error("poll the control listener"))?;
    let deadline = Instant::now()
        .checked_add(CONNECT_BACK_TIMEOUT)
        .unwrap_or_else(Instant::now);
    while Instant::now() < deadline {
        cancel.check().map_err(|_| Error::Cancelled)?;
        let (mut stream, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(POLL);
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_error("accept the payload's connection")(e)),
        };
        if peer.ip() != IpAddr::V4(console) {
            continue;
        }
        let ready = stream
            .set_nonblocking(false)
            .and_then(|()| stream.set_read_timeout(Some(HELLO_TIMEOUT)))
            .and_then(|()| stream.set_write_timeout(Some(CONTROL_WRITE_TIMEOUT)));
        if ready.is_err() {
            continue;
        }
        match read_hello(&mut stream) {
            Ok(hello) if same_token(&hello.token, token) => {
                stream
                    .set_read_timeout(None)
                    .map_err(io_error("configure the control connection"))?;
                return Ok((stream, hello));
            }
            // Our payload, but another protocol version: say so, never guess.
            Err(e @ Error::Wire(hermes_wire::Error::VersionMismatch { .. })) => return Err(e),
            // A wrong token or garbage is not our payload.
            Ok(_) | Err(_) => {}
        }
    }
    Err(Error::NoConnectBack)
}

/// Reads one frame of an allowed type; the body is allocated only after
/// `decode_header` has bounded it and the type is known to be acceptable.
pub(crate) fn read_frame(r: &mut impl Read, allowed: &[Kind]) -> Result<(Kind, Vec<u8>), Error> {
    let mut head = [0u8; HEADER_LEN];
    r.read_exact(&mut head).map_err(|e| {
        Error::Control(if e.kind() == io::ErrorKind::UnexpectedEof {
            io::Error::new(e.kind(), "the payload closed the connection")
        } else {
            e
        })
    })?;
    let header = decode_header(head).map_err(Error::Wire)?;
    if !allowed.contains(&header.kind) {
        return Err(Error::Wire(hermes_wire::Error::Malformed(
            "unexpected frame type",
        )));
    }
    let len = usize::try_from(header.body_len)
        .map_err(|_| Error::Wire(hermes_wire::Error::Malformed("length")))?;
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).map_err(Error::Control)?;
    Ok((header.kind, body))
}

fn read_hello(r: &mut impl Read) -> Result<Hello, Error> {
    let (kind, body) = read_frame(r, &[Kind::Hello])?;
    match Message::decode(kind, &body).map_err(Error::Wire)? {
        Message::Hello(h) => Ok(h),
        _ => Err(Error::Wire(hermes_wire::Error::Malformed("expected Hello"))),
    }
}

fn read_console_frame(r: &mut impl Read) -> Result<FromConsole, Error> {
    let (kind, body) = read_frame(r, &[Kind::Registered, Kind::Failed, Kind::Status])?;
    match Message::decode(kind, &body).map_err(Error::Wire)? {
        Message::Registered(m) => Ok(FromConsole::Registered(m)),
        Message::Failed(m) => Ok(FromConsole::Failed(m)),
        Message::Status(m) => Ok(FromConsole::Status(m)),
        _ => Err(Error::Wire(hermes_wire::Error::Malformed(
            "unexpected frame type",
        ))),
    }
}

/// Forwards console frames until the connection ends (the first error ends it).
fn spawn_reader(control: &TcpStream, events: Sender<Event>) -> Result<JoinHandle<()>, Error> {
    let mut stream = control.try_clone().map_err(|source| Error::Io {
        what: "clone the control connection",
        source,
    })?;
    Ok(std::thread::spawn(move || {
        loop {
            let frame = read_console_frame(&mut stream);
            let last = frame.is_err();
            if events.send(Event::Console(frame)).is_err() || last {
                break;
            }
        }
    }))
}

pub(crate) fn write_message(w: &mut impl Write, m: &Message<'_>) -> Result<(), Error> {
    let len = m.encoded_len().map_err(Error::Wire)?;
    let mut frame = vec![0u8; len];
    m.encode(&mut frame).map_err(Error::Wire)?;
    w.write_all(&frame)
        .and_then(|()| w.flush())
        .map_err(Error::Control)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The download manifest the installer reads (pkg-tools' format, one piece).
fn manifest(package_url: &str, item: &Item) -> Vec<u8> {
    let digest: String = item.digest.iter().map(|b| format!("{b:02X}")).collect();
    let mut w = JsonWriter::new(Vec::new());
    let built = (|| -> Result<Vec<u8>, pantheon_json::Error> {
        w.begin_object()?;
        w.key("originalFileSize")?;
        w.u64(item.size)?;
        w.key("packageDigest")?;
        w.string(&digest)?;
        w.key("numberOfSplitFiles")?;
        w.u64(1)?;
        w.key("pieces")?;
        w.begin_array()?;
        w.begin_object()?;
        w.key("url")?;
        w.string(package_url)?;
        w.key("fileOffset")?;
        w.u64(0)?;
        w.key("fileSize")?;
        w.u64(item.size)?;
        w.key("hashValue")?;
        w.string(ZERO_PIECE_HASH)?;
        w.end()?;
        w.end()?;
        w.end()?;
        w.finish()
    })();
    // Writing JSON into a Vec cannot fail: no I/O, and the nesting above is balanced.
    built.unwrap_or_default()
}
