//! `proteus pack` and `proteus convert`: argument specs and text rendering.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use pantheon_args::{Command, Invocation, Io, Opt, Positional, Spec, fail};
use pantheon_core::{Cancel, Classify, Ctx, ErrorKind, Progress, ProgressSink};

use crate::ops::{self, Report};

const NAME: &str = "proteus";
/// Decimal megabyte, as progress and reports use.
const MB: u64 = 1_000_000;
/// One decimal digit of a megabyte.
const TENTH_MB: u64 = MB / 10;
/// Progress resolution: redraw at most once per permille.
const PERMILLE: u64 = 1000;

static PACK: Spec = Spec {
    verb: "pack",
    summary: "Pack a folder into a new ZArchive (.zar)",
    positionals: &[
        Positional {
            name: "DIR",
            help: "folder whose contents become the archive root; must not change while packing",
        },
        Positional {
            name: "OUT",
            help: "new .zar file outside DIR; never overwritten",
        },
    ],
    options: &[],
    one_of: &[],
};

static CONVERT: Spec = Spec {
    verb: "convert",
    summary: "Convert a PS4 game package into a ZArchive (.zar) for shadPS4",
    positionals: &[
        Positional {
            name: "GAME",
            help: "PS4 base game PKG (CATEGORY gd)",
        },
        Positional {
            name: "OUT",
            help: "new lowercase .zar file; never overwritten",
        },
    ],
    options: &[Opt {
        long: "update",
        short: None,
        value: Some("PKG"),
        help: "also convert this full update into OUT's sibling <name>-UPDATE.zar",
    }],
    one_of: &[],
};

pub static COMMAND: Command = Command {
    name: NAME,
    summary: "turns a folder or a PS4 package into a ZArchive that shadPS4 can run",
    verbs: &[&PACK, &CONVERT],
};

/// Runs `proteus` with `argv` (program name excluded); returns the exit code (D31).
pub fn run(argv: &[OsString], io: &mut Io<'_>) -> u8 {
    let (spec, args) = match pantheon_args::parse(&COMMAND, argv) {
        Ok(Invocation::Run(spec, args)) => (spec, args),
        Ok(Invocation::Help(text)) => {
            return write_or_fail(io, |out| out.write_all(text.as_bytes()));
        }
        Err(e) => return fail(io, NAME, &e),
    };
    // The parser checked the positional count.
    let input = Path::new(args.positional(0).unwrap_or_default());
    let out = Path::new(args.positional(1).unwrap_or_default());
    let cancel = Cancel::new();
    let meter = Meter::default();
    let shown = std::io::stderr().is_terminal();
    let sink: &dyn ProgressSink = if shown {
        &meter
    } else {
        &pantheon_core::NoProgress
    };
    let ctx = Ctx::new(&cancel, sink);
    let result = if std::ptr::eq(spec, &PACK) {
        ops::pack(input, out, &ctx).map(|r| vec![("Packed", r)])
    } else {
        let update = args.value("update").map(Path::new);
        ops::convert(input, update, out, &ctx).map(|r| {
            std::iter::once(("Converted base:", r.base))
                .chain(r.update.map(|u| ("Converted update:", u)))
                .collect()
        })
    };
    if shown {
        eprintln!();
    }
    match result {
        Ok(reports) => write_or_fail(io, |o| {
            reports
                .iter()
                .try_for_each(|(what, r)| write_report(o, what, r))
        }),
        Err(e) => fail(io, NAME, &e),
    }
}

/// `12.3` for 12,345,678 bytes.
fn megabytes(bytes: u64) -> String {
    let tenths = bytes.checked_div(TENTH_MB).unwrap_or(0);
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn write_report(out: &mut dyn Write, what: &str, r: &Report) -> std::io::Result<()> {
    writeln!(
        out,
        "{what} {} files and {} folders ({} MB) to {}",
        r.files,
        r.directories,
        megabytes(r.bytes),
        r.path.display()
    )
}

/// Redraws `done / total MB` on stderr (a terminal) at most once per permille.
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
            eprint!("\r{} / {} MB", megabytes(p.done), megabytes(total));
        }
    }
}

#[derive(Debug)]
struct StdoutError(std::io::Error);

impl std::fmt::Display for StdoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot write to stdout: {}", self.0)
    }
}

impl std::error::Error for StdoutError {}

impl Classify for StdoutError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Io
    }
}

fn write_or_fail(io: &mut Io<'_>, f: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) -> u8 {
    match f(&mut *io.out).and_then(|()| io.out.flush()) {
        Ok(()) => 0,
        Err(e) => fail(io, NAME, &StdoutError(e)),
    }
}
