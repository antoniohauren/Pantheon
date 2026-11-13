//! The Hermes payload's session loop (docs/gods/hermes.md "Payload").
//!
//! Connect to the host, send `Hello`, then loop: receive `Job`, register the
//! install task, report `Registered`/`Failed`, poll status if the console
//! supports it, and exit on `Bye`, on connection loss or after the idle
//! timeout (5 min, D27).
//!
//! Everything console-specific is behind [`Console`] and [`Conn`]. The
//! binary's `sys.rs` implements them; until spikes S1–S4 are done every call
//! there returns [`SysError::NotImplemented`]. This library is safe code only
//! and runs on the host in tests.

#![cfg_attr(not(test), no_std)]

use hermes_wire::config::Config;
use hermes_wire::{
    Failed, HEADER_LEN, Hello, Job, Kind, Message, Registered, Stage, Status, TaskState, VERSION,
    decode_header,
};

/// D27: the payload exits after this long without host frames or task progress.
/// The host's `Keepalive` (D37) keeps a long download's session open.
pub const IDLE_TIMEOUT_MS: u64 = 5 * 60 * 1000;
/// One receive wait; between waits the loop checks the idle timer and polls status.
pub const RECV_WAIT_MS: u32 = 1_000;
/// Receive arena for one frame (D37): fits a `Job` with a full-size icon.
/// `sys.rs` provides it, because S1 decides where the payload's memory comes from.
pub const RECV_ARENA_LEN: usize = hermes_wire::MAX_JOB_FRAME as usize;
/// Every console → host frame is small.
const SEND_BUF_LEN: usize = 64;

/// The open console questions (hermes.md "Spikes"). A stub names the one it waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spike {
    /// Loader entry contract, memory and exit.
    S1,
    /// Finding system functions: sockets, time.
    S2,
    /// The installer call that registers a download task.
    S3,
    /// Polling a task's status.
    S4,
}

/// A failure of the console layer. Raw console codes are never invented here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SysError {
    /// Waits for this spike; nothing was attempted.
    NotImplemented(Spike),
    /// `Conn::recv` saw no data within its wait.
    TimedOut,
    /// A system call failed; its code is not interpreted.
    Failed(u32),
}

/// Why registration failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// The installer refused the task with this raw code (sent as `Failed`).
    Console(u32),
    /// The call itself is unavailable or failed.
    Sys(SysError),
}

/// One status poll result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Polled {
    pub state: TaskState,
    pub bytes_done: Option<u64>,
    pub error_code: u32,
}

/// A connected TCP stream to the host (S2).
pub trait Conn {
    /// Reads at most `buf.len()` bytes, waiting up to `wait_ms`. `Ok(0)` means
    /// the host closed the connection; `Err(TimedOut)` means nothing arrived.
    fn recv(&mut self, buf: &mut [u8], wait_ms: u32) -> Result<usize, SysError>;
    fn send_all(&mut self, data: &[u8]) -> Result<(), SysError>;
}

/// What the session needs from the console (S2–S4). Dropping the `Conn`
/// closes it: the payload must not leave a socket behind (hermes.md).
pub trait Console {
    type Conn: Conn;
    fn connect(&mut self, host: [u8; 4], port: u16) -> Result<Self::Conn, SysError>;
    /// A monotonic clock in milliseconds.
    fn now_ms(&mut self) -> Result<u64, SysError>;
    /// Registers a download task for `job`; returns the installer's task ID (S3).
    fn register(&mut self, job: &Job<'_>) -> Result<u64, RegisterError>;
    /// The task's current state (S4). `NotImplemented` turns polling off.
    fn poll(&mut self, task_id: u64) -> Result<Polled, SysError>;
}

/// Why the session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The host said `Bye`.
    Bye,
    /// Nothing happened for `IDLE_TIMEOUT_MS`.
    Idle,
    /// The host closed the connection.
    Closed,
    /// The host broke the protocol (or sent a frame larger than our buffer).
    Malformed(hermes_wire::Error),
    Sys(SysError),
}

/// A registered task whose status is being polled.
struct Watch {
    index: u32,
    task_id: u64,
    last: Option<TaskState>,
}

/// Runs one session to its end, receiving into `arena` (normally
/// [`RECV_ARENA_LEN`] bytes; a larger frame is refused). The connection is
/// closed when this returns.
pub fn run<C: Console>(console: &mut C, cfg: &Config, arena: &mut [u8]) -> Exit {
    match session(console, cfg, arena) {
        Ok(exit) | Err(exit) => exit,
    }
}

fn session<C: Console>(console: &mut C, cfg: &Config, buf: &mut [u8]) -> Result<Exit, Exit> {
    let mut conn = console.connect(cfg.host, cfg.port).map_err(Exit::Sys)?;
    send(
        &mut conn,
        &Message::Hello(Hello {
            version: VERSION,
            token: cfg.token,
            build_id: cfg.build_id,
        }),
    )?;
    let mut last = console.now_ms().map_err(Exit::Sys)?;
    let mut watch: Option<Watch> = None;
    let mut polling = true;
    loop {
        let Some((kind, len)) = receive(console, &mut conn, buf, &mut last)? else {
            if polling && let Some(w) = watch.as_mut() {
                match poll(console, &mut conn, w, &mut last)? {
                    Polling::Continue => {}
                    Polling::Finished => watch = None,
                    Polling::Unavailable => polling = false,
                }
            }
            continue;
        };
        let body = buf
            .get(..len)
            .ok_or(Exit::Malformed(hermes_wire::Error::BufferTooSmall))?;
        match Message::decode(kind, body).map_err(Exit::Malformed)? {
            Message::Job(job) => match console.register(&job) {
                Ok(task_id) => {
                    send(
                        &mut conn,
                        &Message::Registered(Registered {
                            index: job.index,
                            task_id,
                        }),
                    )?;
                    watch = Some(Watch {
                        index: job.index,
                        task_id,
                        last: None,
                    });
                }
                Err(RegisterError::Console(code)) => send(
                    &mut conn,
                    &Message::Failed(Failed {
                        index: job.index,
                        stage: Stage::Register,
                        code,
                    }),
                )?,
                // No console code to report: end the session; the host sees it close.
                Err(RegisterError::Sys(e)) => return Err(Exit::Sys(e)),
            },
            Message::Bye => return Ok(Exit::Bye),
            // Receiving it already reset the idle timer.
            Message::Keepalive => {}
            _ => {
                return Err(Exit::Malformed(hermes_wire::Error::Malformed(
                    "unexpected frame type",
                )));
            }
        }
    }
}

enum Polling {
    Continue,
    Finished,
    Unavailable,
}

/// Polls `w` once and reports a changed state as `Status`.
fn poll<C: Console>(
    console: &mut C,
    conn: &mut C::Conn,
    w: &mut Watch,
    last: &mut u64,
) -> Result<Polling, Exit> {
    let p = match console.poll(w.task_id) {
        Ok(p) => p,
        Err(SysError::NotImplemented(_)) => return Ok(Polling::Unavailable),
        Err(e) => return Err(Exit::Sys(e)),
    };
    if w.last != Some(p.state) {
        w.last = Some(p.state);
        *last = console.now_ms().map_err(Exit::Sys)?;
        send(
            conn,
            &Message::Status(Status {
                index: w.index,
                state: p.state,
                bytes_done: p.bytes_done,
                error_code: p.error_code,
            }),
        )?;
    }
    Ok(match p.state {
        TaskState::Done | TaskState::Error => Polling::Finished,
        TaskState::Queued | TaskState::Downloading | TaskState::Installing => Polling::Continue,
    })
}

/// The next frame's type and body length (body in `buf`), or `None` when
/// nothing arrived within one wait.
fn receive<C: Console>(
    console: &mut C,
    conn: &mut C::Conn,
    buf: &mut [u8],
    last: &mut u64,
) -> Result<Option<(Kind, usize)>, Exit> {
    let mut head = [0u8; HEADER_LEN];
    if !fill(console, conn, &mut head, last, true)? {
        return Ok(None);
    }
    let header = decode_header(head).map_err(Exit::Malformed)?;
    let len = usize::try_from(header.body_len)
        .map_err(|_| Exit::Malformed(hermes_wire::Error::Malformed("length")))?;
    let max = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    let body = buf
        .get_mut(..len)
        .ok_or(Exit::Malformed(hermes_wire::Error::FrameTooLarge {
            kind: header.kind,
            len: header.body_len,
            max,
        }))?;
    fill(console, conn, body, last, false)?;
    Ok(Some((header.kind, len)))
}

/// Fills `out` completely. At a frame boundary (`may_wait`), returns
/// `Ok(false)` if nothing at all arrived within one wait.
fn fill<C: Console>(
    console: &mut C,
    conn: &mut C::Conn,
    out: &mut [u8],
    last: &mut u64,
    may_wait: bool,
) -> Result<bool, Exit> {
    let mut got = 0usize;
    while got < out.len() {
        let rest = out.get_mut(got..).unwrap_or_default();
        match conn.recv(rest, RECV_WAIT_MS) {
            Ok(0) => return Err(Exit::Closed),
            Ok(n) => {
                got = got.saturating_add(n);
                *last = console.now_ms().map_err(Exit::Sys)?;
            }
            Err(SysError::TimedOut) => {
                let now = console.now_ms().map_err(Exit::Sys)?;
                if now.saturating_sub(*last) >= IDLE_TIMEOUT_MS {
                    return Err(Exit::Idle);
                }
                if may_wait && got == 0 {
                    return Ok(false);
                }
            }
            Err(e) => return Err(Exit::Sys(e)),
        }
    }
    Ok(true)
}

fn send(conn: &mut impl Conn, m: &Message<'_>) -> Result<(), Exit> {
    let mut out = [0u8; SEND_BUF_LEN];
    let n = m.encode(&mut out).map_err(Exit::Malformed)?;
    conn.send_all(out.get(..n).unwrap_or_default())
        .map_err(Exit::Sys)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)] // test-only code
mod tests;
