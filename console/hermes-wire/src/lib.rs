//! hermes-wire: the Hermes protocol, version 1 (docs/gods/hermes.md, D34).
//!
//! The same code runs on the host and inside the console payload, so it is
//! `no_std`, allocation-free and panic-free: decoding borrows from the caller's
//! buffer, encoding writes into one.
//!
//! A frame is `u32 length (LE) + u8 type + body`. `length` counts the type byte
//! and the body, so it is never 0. [`decode_header`] checks it against the
//! type's limit **before** the caller reads (or allocates for) the body.

#![no_std]
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used
)]

#[cfg(test)]
extern crate std;

mod codec;
pub mod config;

use core::fmt;

use codec::{Reader, Writer};

/// Protocol version carried in `Hello` and in the payload's config block.
pub const VERSION: u32 = 1;
/// Session token length: 128 bits (hermes.md "Security").
pub const TOKEN_LEN: usize = 16;
/// Payload build ID length: a SHA-256 prefix (stamped by `xtask payload`).
pub const BUILD_ID_LEN: usize = 16;
/// PS4 package digest length (header offset `0xFE0`).
pub const DIGEST_LEN: usize = 32;
/// `u32 length + u8 type`.
pub const HEADER_LEN: usize = 5;
/// Largest `length` of every frame type except `Job` (64 KiB).
pub const MAX_FRAME: u32 = 64 * 1024;
/// Largest icon a `Job` carries (4 MiB, `Limits::max_artwork`).
pub const MAX_ICON: u32 = 4 * 1024 * 1024;
/// Largest `Job` frame: the icon plus the regular frame budget.
pub const MAX_JOB_FRAME: u32 = MAX_ICON + MAX_FRAME;

/// Batch-validation limits (hermes.md "Scope"), shared by both ends.
pub mod limits {
    /// Package name (title), bytes.
    pub const NAME: u32 = 0x259;
    /// Content ID, bytes.
    pub const CONTENT_ID: u32 = 0x30;
    /// SFO category, bytes.
    pub const CATEGORY: u32 = 12;
    /// `"PS4"` (3 bytes) + category.
    pub const PACKAGE_TYPE: u32 = 3 + CATEGORY;
    /// Manifest URL: `http://a.b.c.d:port/<token-hex>/<n>/package.json` fits easily.
    pub const URL: u32 = 512;
    /// The fixed prefix of a package type.
    pub const PACKAGE_TYPE_PREFIX: &str = "PS4";
}

/// Frame types. The numbers are part of the wire format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
    Hello = 1,
    Job = 2,
    Registered = 3,
    Failed = 4,
    Status = 5,
    Bye = 6,
    Keepalive = 7,
}

impl Kind {
    pub const fn from_u8(b: u8) -> Option<Kind> {
        Some(match b {
            1 => Kind::Hello,
            2 => Kind::Job,
            3 => Kind::Registered,
            4 => Kind::Failed,
            5 => Kind::Status,
            6 => Kind::Bye,
            7 => Kind::Keepalive,
            _ => return None,
        })
    }

    /// The largest `length` field accepted for this type.
    pub const fn max_frame(self) -> u32 {
        match self {
            Kind::Job => MAX_JOB_FRAME,
            _ => MAX_FRAME,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Kind::Hello => "Hello",
            Kind::Job => "Job",
            Kind::Registered => "Registered",
            Kind::Failed => "Failed",
            Kind::Status => "Status",
            Kind::Bye => "Bye",
            Kind::Keepalive => "Keepalive",
        }
    }
}

/// Everything that can go wrong on the wire. Every variant closes the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The bytes violate the format (truncated, trailing bytes, bad flag, bad UTF-8).
    Malformed(&'static str),
    /// A frame type this version does not know.
    UnknownType(u8),
    /// A frame `length` above its type's limit.
    FrameTooLarge { kind: Kind, len: u32, max: u32 },
    /// A length-prefixed field above its limit.
    FieldTooLong {
        field: &'static str,
        len: u32,
        max: u32,
    },
    /// A required string field is empty.
    EmptyField(&'static str),
    /// `Hello` (or the config block) from another protocol version.
    VersionMismatch { got: u32, expected: u32 },
    /// The output buffer cannot hold the encoded frame.
    BufferTooSmall,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Malformed(what) => write!(f, "malformed frame: {what}"),
            Error::UnknownType(t) => write!(f, "malformed frame: unknown type {t}"),
            Error::FrameTooLarge { kind, len, max } => write!(
                f,
                "malformed frame: {} of {len} bytes exceeds {max}",
                kind.name()
            ),
            Error::FieldTooLong { field, len, max } => {
                write!(f, "malformed frame: {field} of {len} bytes exceeds {max}")
            }
            Error::EmptyField(field) => write!(f, "malformed frame: empty {field}"),
            Error::VersionMismatch { got, expected } => {
                write!(f, "payload is version {got}, host expects {expected}")
            }
            Error::BufferTooSmall => f.write_str("frame buffer too small"),
        }
    }
}

impl core::error::Error for Error {}

/// A checked frame header: the type and how many body bytes follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: Kind,
    pub body_len: u32,
}

/// Validates the 5 header bytes. The body length is bounded by the type's
/// limit, so a caller may allocate `body_len` bytes after this returns `Ok`.
pub fn decode_header(bytes: [u8; HEADER_LEN]) -> Result<Header, Error> {
    let [l0, l1, l2, l3, t] = bytes;
    let len = u32::from_le_bytes([l0, l1, l2, l3]);
    let kind = Kind::from_u8(t).ok_or(Error::UnknownType(t))?;
    let max = kind.max_frame();
    if len > max {
        return Err(Error::FrameTooLarge { kind, len, max });
    }
    let body_len = len
        .checked_sub(1)
        .ok_or(Error::Malformed("length 0 has no type byte"))?;
    Ok(Header { kind, body_len })
}

/// `Hello`: console → host, first frame of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello {
    pub version: u32,
    pub token: [u8; TOKEN_LEN],
    pub build_id: [u8; BUILD_ID_LEN],
}

/// `Job`: host → console, one package to register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job<'a> {
    /// 0-based position in the batch.
    pub index: u32,
    pub manifest_url: &'a str,
    pub name: &'a str,
    pub content_id: &'a str,
    /// `"PS4"` + the upper-cased SFO category, e.g. `"PS4GD"`.
    pub package_type: &'a str,
    pub size: u64,
    pub digest: [u8; DIGEST_LEN],
    pub icon: Option<&'a [u8]>,
}

/// `Registered`: the console's installer accepted the task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registered {
    pub index: u32,
    /// The installer's task ID, widened to `u64` (its native width is spike S3).
    pub task_id: u64,
}

/// Where on the console a job failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Stage {
    Register = 0,
    Status = 1,
}

impl Stage {
    pub const fn name(self) -> &'static str {
        match self {
            Stage::Register => "register",
            Stage::Status => "status",
        }
    }
}

/// `Failed`: a console call returned an error; the code is shown raw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failed {
    pub index: u32,
    pub stage: Stage,
    pub code: u32,
}

/// Installer task states (spike S4 decides whether they are ever reported).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TaskState {
    Queued = 0,
    Downloading = 1,
    Installing = 2,
    Done = 3,
    Error = 4,
}

impl TaskState {
    pub const fn name(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Downloading => "downloading",
            TaskState::Installing => "installing",
            TaskState::Done => "done",
            TaskState::Error => "error",
        }
    }
}

/// `Status`: a polled task state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub index: u32,
    pub state: TaskState,
    pub bytes_done: Option<u64>,
    /// Raw console code; meaningful when `state` is `Error`.
    pub error_code: u32,
}

/// One decoded frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message<'a> {
    Hello(Hello),
    Job(Job<'a>),
    Registered(Registered),
    Failed(Failed),
    Status(Status),
    /// Host → console: the payload exits cleanly.
    Bye,
    /// Host → console: the host is still there; resets the payload's idle
    /// timer while the installer downloads (D37).
    Keepalive,
}

const U8: usize = 1;
const U32: usize = 4;
const U64: usize = 8;
const HELLO_BODY: usize = U32 + TOKEN_LEN + BUILD_ID_LEN;
const REGISTERED_BODY: usize = U32 + U64;
const FAILED_BODY: usize = U32 + U8 + U32;
/// `Status` without `bytes_done`.
const STATUS_BODY: usize = U32 + U8 + U8 + U32;

impl<'a> Message<'a> {
    pub const fn kind(&self) -> Kind {
        match self {
            Message::Hello(_) => Kind::Hello,
            Message::Job(_) => Kind::Job,
            Message::Registered(_) => Kind::Registered,
            Message::Failed(_) => Kind::Failed,
            Message::Status(_) => Kind::Status,
            Message::Bye => Kind::Bye,
            Message::Keepalive => Kind::Keepalive,
        }
    }

    /// Decodes one body whose header [`decode_header`] already accepted.
    /// Unknown values, missing bytes and trailing bytes are all errors.
    pub fn decode(kind: Kind, body: &'a [u8]) -> Result<Message<'a>, Error> {
        let mut r = Reader::new(body);
        let message = match kind {
            Kind::Hello => {
                // The version comes first, so a newer layout still reports a clear mismatch.
                let version = r.u32()?;
                if version != VERSION {
                    return Err(Error::VersionMismatch {
                        got: version,
                        expected: VERSION,
                    });
                }
                Message::Hello(Hello {
                    version,
                    token: r.array()?,
                    build_id: r.array()?,
                })
            }
            Kind::Job => Message::Job(Job {
                index: r.u32()?,
                manifest_url: r.str("manifest URL", limits::URL)?,
                name: r.str("package name", limits::NAME)?,
                content_id: r.str("content ID", limits::CONTENT_ID)?,
                package_type: r.str("package type", limits::PACKAGE_TYPE)?,
                size: r.u64()?,
                digest: r.array()?,
                icon: if r.flag()? {
                    Some(r.blob("icon", MAX_ICON)?)
                } else {
                    None
                },
            }),
            Kind::Registered => Message::Registered(Registered {
                index: r.u32()?,
                task_id: r.u64()?,
            }),
            Kind::Failed => Message::Failed(Failed {
                index: r.u32()?,
                stage: match r.u8()? {
                    0 => Stage::Register,
                    1 => Stage::Status,
                    _ => return Err(Error::Malformed("unknown failure stage")),
                },
                code: r.u32()?,
            }),
            Kind::Status => Message::Status(Status {
                index: r.u32()?,
                state: match r.u8()? {
                    0 => TaskState::Queued,
                    1 => TaskState::Downloading,
                    2 => TaskState::Installing,
                    3 => TaskState::Done,
                    4 => TaskState::Error,
                    _ => return Err(Error::Malformed("unknown task state")),
                },
                bytes_done: if r.flag()? { Some(r.u64()?) } else { None },
                error_code: r.u32()?,
            }),
            Kind::Bye => Message::Bye,
            Kind::Keepalive => Message::Keepalive,
        };
        r.finish()?;
        Ok(message)
    }

    /// Body length in bytes, after checking every field against its limit.
    fn body_len(&self) -> Result<usize, Error> {
        Ok(match self {
            Message::Hello(_) => HELLO_BODY,
            Message::Job(job) => job.body_len()?,
            Message::Registered(_) => REGISTERED_BODY,
            Message::Failed(_) => FAILED_BODY,
            Message::Status(s) if s.bytes_done.is_some() => STATUS_BODY + U64,
            Message::Status(_) => STATUS_BODY,
            Message::Bye | Message::Keepalive => 0,
        })
    }

    /// The whole frame's length (header included).
    pub fn encoded_len(&self) -> Result<usize, Error> {
        self.body_len()?
            .checked_add(HEADER_LEN)
            .ok_or(Error::BufferTooSmall)
    }

    /// Encodes the whole frame into `out`; returns the bytes written. The same
    /// limits as [`Message::decode`] apply, so a host never sends what the
    /// payload would reject.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, Error> {
        let kind = self.kind();
        let body_len = self.body_len()?;
        let len = body_len
            .checked_add(U8)
            .and_then(|l| u32::try_from(l).ok())
            .ok_or(Error::BufferTooSmall)?;
        let max = kind.max_frame();
        if len > max {
            return Err(Error::FrameTooLarge { kind, len, max });
        }
        let mut w = Writer::new(out);
        w.u32(len)?;
        w.u8(kind as u8)?;
        match self {
            Message::Hello(h) => {
                w.u32(h.version)?;
                w.bytes(&h.token)?;
                w.bytes(&h.build_id)?;
            }
            Message::Job(j) => {
                w.u32(j.index)?;
                for s in [j.manifest_url, j.name, j.content_id, j.package_type] {
                    w.blob(s.as_bytes())?;
                }
                w.u64(j.size)?;
                w.bytes(&j.digest)?;
                w.flag(j.icon.is_some())?;
                if let Some(icon) = j.icon {
                    w.blob(icon)?;
                }
            }
            Message::Registered(r) => {
                w.u32(r.index)?;
                w.u64(r.task_id)?;
            }
            Message::Failed(f) => {
                w.u32(f.index)?;
                w.u8(f.stage as u8)?;
                w.u32(f.code)?;
            }
            Message::Status(s) => {
                w.u32(s.index)?;
                w.u8(s.state as u8)?;
                w.flag(s.bytes_done.is_some())?;
                if let Some(done) = s.bytes_done {
                    w.u64(done)?;
                }
                w.u32(s.error_code)?;
            }
            Message::Bye | Message::Keepalive => {}
        }
        Ok(w.written())
    }
}

impl Job<'_> {
    fn body_len(&self) -> Result<usize, Error> {
        let mut total = U32 + U64 + DIGEST_LEN + U8;
        for (field, value, max) in [
            ("manifest URL", self.manifest_url, limits::URL),
            ("package name", self.name, limits::NAME),
            ("content ID", self.content_id, limits::CONTENT_ID),
            ("package type", self.package_type, limits::PACKAGE_TYPE),
        ] {
            total = total
                .checked_add(checked_field(field, value.as_bytes(), max)?)
                .ok_or(Error::BufferTooSmall)?;
            if value.is_empty() {
                return Err(Error::EmptyField(field));
            }
        }
        if let Some(icon) = self.icon {
            total = total
                .checked_add(checked_field("icon", icon, MAX_ICON)?)
                .ok_or(Error::BufferTooSmall)?;
        }
        Ok(total)
    }
}

/// `u32 length + bytes` size of a field, or `FieldTooLong`.
fn checked_field(field: &'static str, value: &[u8], max: u32) -> Result<usize, Error> {
    let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
    if len > max {
        return Err(Error::FieldTooLong { field, len, max });
    }
    value.len().checked_add(U32).ok_or(Error::BufferTooSmall)
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used
)] // test-only code
mod tests;
