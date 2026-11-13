//! `hermes send` and `hermes probe`: argument specs and text/JSON rendering.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use pantheon_args::{Command, Invocation, Io, JSON, Opt, Positional, Spec, UsageError, fail};
use pantheon_core::{Cancel, Classify, Ctx, ErrorKind, Progress, ProgressSink};
use pantheon_json::JsonWriter;

use crate::ops::{self, Error, Loader, Payload, SendReport, Target};
use crate::session::hex;

/// The `--json` schema of `hermes send`.
pub const SCHEMA: &str = "pantheon.hermes.send/1";
const NAME: &str = "hermes";
/// `--loader tcp:PORT`.
const TCP_PREFIX: &str = "tcp:";
/// Progress resolution: redraw at most once per permille.
const PERMILLE: u64 = 1000;
/// Decimal megabyte, as progress and reports use.
const MB: u64 = 1_000_000;
const TENTH_MB: u64 = MB / 10;

const PS4: Opt = Opt {
    long: "ps4",
    short: None,
    value: Some("IP"),
    help: "the console's LAN IPv4 address (required)",
};
const LOADER: Opt = Opt {
    long: "loader",
    short: None,
    value: Some("goldhen|tcp:PORT"),
    help: "GoldHEN BinLoader (ports 9090, 9021, 9020; default) or a raw TCP loader",
};
const PAYLOAD: Opt = Opt {
    long: "payload",
    short: None,
    value: Some("FILE"),
    help: "payload built by `cargo xtask payload` (required without embed-payload)",
};

static SEND: Spec = Spec {
    verb: "send",
    summary: "Send PS4 packages to a console, in order, with one payload",
    positionals: &[Positional {
        name: "PKG...",
        help: "PS4 PKG files; all are validated before anything is sent",
    }],
    options: &[PS4, LOADER, PAYLOAD, JSON],
    one_of: &[],
};

static PROBE: Spec = Spec {
    verb: "probe",
    summary: "Check that a loader is reachable and that our payload answers",
    positionals: &[],
    options: &[PS4, LOADER, PAYLOAD],
    one_of: &[],
};

pub static COMMAND: Command = Command {
    name: NAME,
    summary: "carries packages from this computer to a PS4 on the same network",
    verbs: &[&SEND, &PROBE],
};

/// Runs `hermes` with `argv` (program name excluded); returns the exit code (D31).
pub fn run(argv: &[OsString], io: &mut Io<'_>) -> u8 {
    let (spec, args) = match pantheon_args::parse(&COMMAND, argv) {
        Ok(Invocation::Run(spec, args)) => (spec, args),
        Ok(Invocation::Help(text)) => {
            return write_or_fail(io, |out| out.write_all(text.as_bytes()));
        }
        Err(e) => return fail(io, NAME, &e),
    };
    let target = match target(args.value("ps4"), args.value("loader")) {
        Ok(t) => t,
        Err(e) => return fail(io, NAME, &e),
    };
    let payload = match args.value("payload") {
        Some(path) => Payload::File(PathBuf::from(path)),
        None => match embedded() {
            Some(p) => p,
            None => {
                let e = UsageError(format!(
                    "hermes {}: --payload FILE is required (this build has no embedded payload)",
                    spec.verb
                ));
                return fail(io, NAME, &e);
            }
        },
    };
    if std::ptr::eq(spec, &PROBE) {
        return match ops::probe(&target, &payload) {
            Ok(r) => write_or_fail(io, |out| {
                writeln!(
                    out,
                    "Loader on port {} ran the payload; it answered as protocol version {}, build {}",
                    r.loader_port,
                    r.payload_version,
                    hex(&r.build_id)
                )
            }),
            Err(e) => fail(io, NAME, &e),
        };
    }
    let packages: Vec<PathBuf> = args.positionals_from(0).iter().map(PathBuf::from).collect();
    // shortcut: no Ctrl+C handler (`ctrlc`, D15) yet; Ctrl+C ends the process,
    // which closes both listeners. Wire it in pantheon-cli with the first
    // god that needs a clean cancel report.
    let cancel = Cancel::new();
    let meter = Meter::default();
    let shown = std::io::stderr().is_terminal();
    let sink: &dyn ProgressSink = if shown {
        &meter
    } else {
        &pantheon_core::NoProgress
    };
    let result = ops::send(&target, &payload, &packages, &Ctx::new(&cancel, sink));
    if shown {
        eprintln!();
    }
    let json = args.flag("json");
    match result {
        Ok(report) => write_or_fail(io, |out| write_report(out, &report, None, json)),
        Err(e) => {
            if let Some(report) = e.report() {
                // The report goes to stdout first; the error and its exit code follow.
                let code = write_or_fail(io, |out| write_report(out, report, Some(&e), json));
                if code != 0 {
                    return code;
                }
            }
            fail(io, NAME, &e)
        }
    }
}

#[cfg(feature = "embed-payload")]
fn embedded() -> Option<Payload> {
    Some(Payload::Embedded)
}

#[cfg(not(feature = "embed-payload"))]
fn embedded() -> Option<Payload> {
    None
}

fn target(
    ps4: Option<&std::ffi::OsStr>,
    loader: Option<&std::ffi::OsStr>,
) -> Result<Target, UsageError> {
    let ps4 = ps4.ok_or_else(|| UsageError("hermes: --ps4 IP is required".into()))?;
    let ps4: Ipv4Addr = ps4.to_str().and_then(|s| s.parse().ok()).ok_or_else(|| {
        UsageError(format!(
            "--ps4 needs an IPv4 address, got {}",
            ps4.display()
        ))
    })?;
    let loader = match loader.map(|l| l.to_str()) {
        None => Loader::GoldHen,
        Some(Some("goldhen")) => Loader::GoldHen,
        Some(Some(l)) if l.starts_with(TCP_PREFIX) => {
            match l
                .get(TCP_PREFIX.len()..)
                .and_then(|p| p.parse::<u16>().ok())
            {
                Some(port) if port != 0 => Loader::Tcp { port },
                _ => {
                    return Err(UsageError(format!("--loader {l}: PORT must be 1-65535")));
                }
            }
        }
        Some(_) => {
            return Err(UsageError(
                "--loader must be goldhen or tcp:PORT".to_owned(),
            ));
        }
    };
    Ok(Target { ps4, loader })
}

/// `12.3` for 12,345,678 bytes.
fn megabytes(bytes: u64) -> String {
    let tenths = bytes.checked_div(TENTH_MB).unwrap_or(0);
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn write_report(
    out: &mut dyn Write,
    r: &SendReport,
    error: Option<&Error>,
    json: bool,
) -> std::io::Result<()> {
    if json {
        return write_json(out, r, error).map_err(json_io);
    }
    let total = r.items.len();
    for (i, item) in r.items.iter().enumerate() {
        let mut states = Vec::new();
        if item.transferred {
            states.push("transferred".to_owned());
        } else {
            states.push(format!(
                "{} / {} MB transferred",
                megabytes(item.bytes_transferred),
                megabytes(item.size)
            ));
        }
        if let Some(task) = item.task_id {
            states.push(format!("registered (task {task:#x})"));
        }
        if item.installed {
            states.push("installed".to_owned());
        }
        if let Some(f) = item.failure {
            states.push(format!(
                "failed at {} with code {:#010x}",
                f.stage.name(),
                f.code
            ));
        }
        writeln!(
            out,
            "{}/{total} {}: {}",
            i.saturating_add(1),
            item.name,
            states.join(", ")
        )?;
    }
    if error.is_none() {
        let registered = r.items.iter().all(|i| i.registered());
        let installed = r.items.iter().all(|i| i.installed);
        let what = match (registered, installed) {
            (_, true) => "Transferred, registered and installed.",
            (true, false) => "Transferred and registered. Check the console for installation.",
            (false, false) => "Transferred. Check the console for installation.",
        };
        writeln!(out, "{what}")?;
    }
    Ok(())
}

type JResult = Result<(), pantheon_json::Error>;

/// The `pantheon.hermes.send/1` document.
pub fn write_json(out: &mut dyn Write, r: &SendReport, error: Option<&Error>) -> JResult {
    let mut w = JsonWriter::new(out);
    w.begin_object()?;
    w.key("schema")?;
    w.string(SCHEMA)?;
    w.key("loader_port")?;
    w.u64(u64::from(r.loader_port))?;
    w.key("payload_build_id")?;
    w.hex(&r.build_id)?;
    w.key("transferred")?;
    w.u64(u64::try_from(r.transferred()).unwrap_or(u64::MAX))?;
    w.key("items")?;
    w.begin_array()?;
    for item in &r.items {
        w.begin_object()?;
        w.key("path")?;
        match item.path.to_str() {
            Some(p) => w.string(p)?,
            None => w.null()?,
        }
        w.key("name")?;
        w.string(&item.name)?;
        w.key("size")?;
        w.u64(item.size)?;
        w.key("bytes_transferred")?;
        w.u64(item.bytes_transferred)?;
        w.key("transferred")?;
        w.bool(item.transferred)?;
        w.key("registered")?;
        w.bool(item.registered())?;
        w.key("task_id")?;
        match item.task_id {
            Some(t) => w.u64(t)?,
            None => w.null()?,
        }
        w.key("state")?;
        match item.state {
            Some(s) => w.string(s.name())?,
            None => w.null()?,
        }
        w.key("installed")?;
        w.bool(item.installed)?;
        w.key("failure")?;
        match item.failure {
            Some(f) => {
                w.begin_object()?;
                w.key("stage")?;
                w.string(f.stage.name())?;
                w.key("code")?;
                w.u64(u64::from(f.code))?;
                w.end()?;
            }
            None => w.null()?,
        }
        w.end()?;
    }
    w.end()?;
    w.key("error")?;
    match error {
        Some(e) => {
            w.begin_object()?;
            w.key("exit_code")?;
            w.u64(u64::from(e.kind().exit_code()))?;
            w.key("message")?;
            w.string(&e.to_string())?;
            w.end()?;
        }
        None => w.null()?,
    }
    w.end()?;
    w.finish()?.write_all(b"\n")?;
    Ok(())
}

/// Redraws `done / total MB` of the current package on stderr (a terminal).
#[derive(Default)]
struct Meter {
    last: AtomicU64,
}

impl ProgressSink for Meter {
    fn report(&self, p: &Progress<'_>) {
        let total = p.total.unwrap_or(0);
        let step = p
            .done
            .saturating_mul(PERMILLE)
            .checked_div(total)
            .unwrap_or(PERMILLE)
            .saturating_add(1);
        if self.last.swap(step, Ordering::Relaxed) != step {
            eprint!(
                "\r{}: {} / {} MB",
                p.item.unwrap_or_default(),
                megabytes(p.done),
                megabytes(total)
            );
        }
    }
}

#[derive(Debug)]
struct OutputError(std::io::Error);

impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot write stdout: {}", self.0)
    }
}

impl std::error::Error for OutputError {}

impl Classify for OutputError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Io
    }
}

fn json_io(e: pantheon_json::Error) -> std::io::Error {
    match e {
        pantheon_json::Error::Io(e) => e,
        other => std::io::Error::other(other.to_string()),
    }
}

fn write_or_fail(io: &mut Io<'_>, f: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) -> u8 {
    match f(&mut *io.out).and_then(|()| io.out.flush()) {
        Ok(()) => 0,
        Err(e) => fail(io, NAME, &OutputError(e)),
    }
}
