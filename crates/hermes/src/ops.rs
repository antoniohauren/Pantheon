//! The public library API (hermes.md "Library").

use std::fmt;
use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use hermes_wire::{BUILD_ID_LEN, Stage, TaskState};
use pantheon_core::{Classify, Ctx, ErrorKind, Limits};

use crate::{batch, session};

/// How the payload reaches the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Loader {
    /// GoldHEN BinLoader: ports 9090, 9021 and 9020, in that order (pkg-tools).
    GoldHen,
    /// A generic loader that runs raw bytes received on `port`.
    Tcp { port: u16 },
}

/// GoldHEN BinLoader ports, tried in this order (pkg-tools behaviour).
pub const GOLDHEN_PORTS: [u16; 3] = [9090, 9021, 9020];

impl Loader {
    pub fn ports(&self) -> &[u16] {
        match self {
            Loader::GoldHen => &GOLDHEN_PORTS,
            Loader::Tcp { port } => std::slice::from_ref(port),
        }
    }
}

/// The console and how to reach its loader. IPv4 only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub ps4: Ipv4Addr,
    pub loader: Loader,
}

/// The payload binary to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    /// The payload built into this binary (feature `embed-payload`, D21).
    #[cfg(feature = "embed-payload")]
    Embedded,
    /// A flat payload binary built by `cargo xtask payload`.
    File(PathBuf),
}

#[cfg(feature = "embed-payload")]
const EMBEDDED: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../target/hermes-payload/hermes-payload.elf"
));

impl Payload {
    pub(crate) fn bytes(&self) -> Result<Vec<u8>, Error> {
        match self {
            #[cfg(feature = "embed-payload")]
            Payload::Embedded => Ok(EMBEDDED.to_vec()),
            Payload::File(path) => std::fs::read(path).map_err(|source| Error::Io {
                what: "read the payload file",
                source,
            }),
        }
    }
}

/// What `probe` learned: the loader ran our payload and it answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeReport {
    /// The loader port that accepted the payload.
    pub loader_port: u16,
    pub payload_version: u32,
    pub build_id: [u8; BUILD_ID_LEN],
}

/// A console-side failure, with its raw code (never interpreted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConsoleFailure {
    pub stage: Stage,
    pub code: u32,
}

/// One package of a batch. The three outcomes are separate and never merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemReport {
    pub path: PathBuf,
    /// The title sent to the installer.
    pub name: String,
    pub size: u64,
    /// Unique package bytes the console downloaded, interrupted requests included.
    pub bytes_transferred: u64,
    /// Every byte was served.
    pub transferred: bool,
    /// The installer accepted the task and returned this ID (`Registered`).
    pub task_id: Option<u64>,
    /// The last task state the console reported (spike S4), if any.
    pub state: Option<TaskState>,
    /// The console reported the task `done` (spike S4). Never inferred.
    pub installed: bool,
    pub failure: Option<ConsoleFailure>,
}

impl ItemReport {
    pub fn registered(&self) -> bool {
        self.task_id.is_some()
    }
}

/// The outcome of a batch, one item per package, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendReport {
    pub loader_port: u16,
    pub build_id: [u8; BUILD_ID_LEN],
    pub items: Vec<ItemReport>,
}

impl SendReport {
    /// How many packages were fully transferred.
    pub fn transferred(&self) -> usize {
        self.items.iter().filter(|i| i.transferred).count()
    }
}

/// Is a supported loader reachable, and does our payload answer?
pub fn probe(target: &Target, payload: &Payload) -> Result<ProbeReport, Error> {
    let image = payload.bytes()?;
    let s = session::Session::open(target, image, &pantheon_core::Cancel::new())?;
    let report = ProbeReport {
        loader_port: s.loader_port,
        payload_version: s.hello.version,
        build_id: s.hello.build_id,
    };
    s.close();
    Ok(report)
}

/// Sends `packages` to the console, in order, one payload per batch. The whole
/// batch is validated first: one bad package means nothing is sent.
pub fn send(
    target: &Target,
    payload: &Payload,
    packages: &[PathBuf],
    ctx: &Ctx<'_>,
) -> Result<SendReport, Error> {
    let items = batch::validate(packages, &Limits::DEFAULT)?;
    let image = payload.bytes()?;
    ctx.cancel.check().map_err(|_| Error::Cancelled)?;
    let mut s = session::Session::open(target, image, ctx.cancel)?;
    let mut report = SendReport {
        loader_port: s.loader_port,
        build_id: s.hello.build_id,
        items: items.iter().map(batch::Item::report).collect(),
    };
    let result = s.run(&items, &mut report.items, ctx);
    s.close();
    match result {
        Ok(()) => Ok(report),
        Err(cause) => Err(Error::Batch {
            report,
            cause: Box::new(cause),
        }),
    }
}

#[derive(Debug)]
pub enum Error {
    /// `send` with no packages.
    EmptyBatch,
    /// A package failed batch validation; nothing was sent.
    Package {
        path: PathBuf,
        source: pantheon_package::Error,
    },
    /// The package's PARAM.SFO is malformed; nothing was sent.
    Sfo {
        path: PathBuf,
        source: pantheon_psf::Error,
    },
    /// A PS3 package (or another kind) where a PS4 package is needed.
    NotPs4 {
        path: PathBuf,
    },
    /// A package field the installer cannot take; nothing was sent.
    Field {
        path: PathBuf,
        field: &'static str,
        problem: &'static str,
    },
    /// The payload file has no usable config block.
    Payload(hermes_wire::Error),
    /// A local operating-system failure.
    Io {
        what: &'static str,
        source: io::Error,
    },
    /// The OS random source failed (session token, D26).
    Random(pantheon_fs::Error),
    /// No local IPv4 address routes to the console.
    NoRoute(io::Error),
    /// No loader port accepted a connection.
    LoaderUnreachable {
        attempts: Vec<(u16, io::Error)>,
    },
    /// A loader accepted the connection but the payload could not be sent.
    LoaderSend {
        port: u16,
        source: io::Error,
    },
    /// The payload never connected back with our token.
    NoConnectBack,
    /// The console broke the protocol (malformed or unexpected frame).
    Wire(hermes_wire::Error),
    /// The control connection failed or closed early.
    Control(io::Error),
    /// The console's installer reported a failure for package `index`.
    Console {
        index: u32,
        failure: ConsoleFailure,
    },
    /// Serving package bytes failed on the host side.
    Http(io::Error),
    Cancelled,
    /// The session started and then stopped: `report` lists every item.
    Batch {
        report: SendReport,
        cause: Box<Error>,
    },
}

impl Error {
    /// The report of a batch that started, if this error stopped one.
    pub fn report(&self) -> Option<&SendReport> {
        match self {
            Error::Batch { report, .. } => Some(report),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nothing = "No packages were sent.";
        match self {
            Error::EmptyBatch => f.write_str("choose at least one PS4 package"),
            Error::Package { path, source } => {
                write!(f, "{}: {source}. {nothing}", path.display())
            }
            Error::Sfo { path, source } => {
                write!(f, "{}: PARAM.SFO: {source}. {nothing}", path.display())
            }
            Error::NotPs4 { path } => {
                write!(f, "{}: not a PS4 package. {nothing}", path.display())
            }
            Error::Field {
                path,
                field,
                problem,
            } => write!(f, "{}: {field} {problem}. {nothing}", path.display()),
            Error::Payload(e) => write!(f, "unusable payload file: {e}"),
            Error::Io { what, source } => write!(f, "could not {what}: {source}"),
            Error::Random(e) => write!(f, "could not create a session token: {e}"),
            Error::NoRoute(e) => write!(f, "no local IPv4 address routes to the PS4: {e}"),
            Error::LoaderUnreachable { attempts } => {
                f.write_str("could not reach the loader (")?;
                for (i, (port, e)) in attempts.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{port}: {e}")?;
                }
                f.write_str(")")
            }
            Error::LoaderSend { port, source } => {
                write!(f, "could not send the payload on port {port}: {source}")
            }
            Error::NoConnectBack => f.write_str(
                "the payload did not connect back; check the loader and firewall settings",
            ),
            Error::Wire(e) => write!(f, "console protocol error: {e}"),
            Error::Control(e) => write!(f, "control connection failed: {e}"),
            Error::Console { index, failure } => write!(
                f,
                "package {}: the console's installer failed at {} with code {:#010x}",
                u64::from(*index).saturating_add(1),
                failure.stage.name(),
                failure.code
            ),
            Error::Http(e) => write!(f, "HTTP transfer failed: {e}"),
            Error::Cancelled => f.write_str("sending cancelled"),
            Error::Batch { report, cause } => write!(
                f,
                "{cause}\n{} of {} package(s) transferred; remaining packages were not sent",
                report.transferred(),
                report.items.len()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Package { source, .. } => Some(source),
            Error::Sfo { source, .. } => Some(source),
            Error::Payload(e) | Error::Wire(e) => Some(e),
            Error::Io { source, .. }
            | Error::LoaderSend { source, .. }
            | Error::NoRoute(source)
            | Error::Control(source)
            | Error::Http(source) => Some(source),
            Error::Random(e) => Some(e),
            Error::Batch { cause, .. } => Some(cause.as_ref()),
            Error::EmptyBatch
            | Error::NotPs4 { .. }
            | Error::Field { .. }
            | Error::LoaderUnreachable { .. }
            | Error::NoConnectBack
            | Error::Console { .. }
            | Error::Cancelled => None,
        }
    }
}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::EmptyBatch => ErrorKind::Usage,
            Error::Package { source, .. } => source.kind(),
            Error::Sfo { source, .. } => source.kind(),
            Error::NotPs4 { .. } => ErrorKind::Unsupported,
            Error::Field { .. } => ErrorKind::Limit,
            Error::Payload(hermes_wire::Error::VersionMismatch { .. }) => ErrorKind::Unsupported,
            Error::Payload(_) => ErrorKind::Malformed,
            Error::Io { .. } | Error::Random(_) => ErrorKind::Io,
            Error::Wire(hermes_wire::Error::VersionMismatch { .. }) => ErrorKind::Unsupported,
            Error::Wire(_) => ErrorKind::Malformed,
            Error::NoRoute(_)
            | Error::LoaderUnreachable { .. }
            | Error::LoaderSend { .. }
            | Error::NoConnectBack
            | Error::Control(_)
            | Error::Console { .. }
            | Error::Http(_) => ErrorKind::Network,
            Error::Cancelled => ErrorKind::Cancelled,
            // P13: once one package made it, the batch is partial, unless cancelled.
            Error::Batch { report, cause } => match cause.kind() {
                ErrorKind::Cancelled => ErrorKind::Cancelled,
                _ if report.transferred() > 0 => ErrorKind::Partial,
                kind => kind,
            },
        }
    }
}
