//! `hades list` and `hades extract`: argument specs and text/JSON rendering.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use pantheon_args::{Command, Invocation, Io, JSON, Opt, Positional, Spec, UsageError, fail};
use pantheon_core::{Cancel, Classify, Ctx, ErrorKind, Limits, Progress, ProgressSink};
use pantheon_json::JsonWriter;
use pantheon_package::{NodeKind, Selection, Tree};

use crate::ops::{self, Report};

/// The `--json` schema of `hades list`.
pub const SCHEMA: &str = "pantheon.hades.listing/1";
const NAME: &str = "hades";
/// Decimal megabyte, as progress and reports use.
const MB: u64 = 1_000_000;
/// One decimal digit of a megabyte.
const TENTH_MB: u64 = MB / 10;
/// Progress resolution: redraw at most once per permille.
const PERMILLE: u64 = 1000;

static LIST: Spec = Spec {
    verb: "list",
    summary: "List the files and folders inside a PS3/PS4 package, without extracting",
    positionals: &[Positional {
        name: "INPUT",
        help: "PS3 or PS4 PKG",
    }],
    options: &[JSON],
    one_of: &[],
};

static EXTRACT: Spec = Spec {
    verb: "extract",
    summary: "Extract a package's files into a new folder",
    positionals: &[
        Positional {
            name: "INPUT",
            help: "PS3 or PS4 PKG",
        },
        Positional {
            name: "OUT",
            help: "new folder; its parent must exist; never merged or overwritten",
        },
    ],
    options: &[Opt {
        long: "select",
        short: None,
        value: Some("PATH"),
        help: "extract only this path from `hades list` (repeatable; folders include their contents)",
    }],
    one_of: &[],
};

pub static COMMAND: Command = Command {
    name: NAME,
    summary: "shows the tree inside a package and brings it out, whole or in part",
    verbs: &[&LIST, &EXTRACT],
};

/// Runs `hades` with `argv` (program name excluded); returns the exit code (D31).
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
    let file = match File::open(input) {
        Ok(f) => BufReader::new(f),
        Err(e) => return fail(io, NAME, &OpenError(input.display().to_string(), e)),
    };
    let mut contents = match ops::open(file, &Limits::DEFAULT) {
        Ok(c) => c,
        Err(e) => return fail(io, NAME, &e),
    };
    if std::ptr::eq(spec, &LIST) {
        let tree = contents.tree();
        return if args.flag("json") {
            write_or_fail(io, |out| write_json(out, tree, input).map_err(json_io))
        } else {
            write_or_fail(io, |out| write_text(out, tree))
        };
    }
    let out = Path::new(args.positional(1).unwrap_or_default());
    let Some(name) = out.file_name().and_then(|n| n.to_str()) else {
        let e = UsageError(format!(
            "OUT must end in a UTF-8 folder name: {}",
            out.display()
        ));
        return fail(io, NAME, &e);
    };
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut paths = Vec::new();
    for v in args.values("select") {
        match v.to_str() {
            Some(p) => paths.push(p),
            None => {
                let e = UsageError(format!("--select path is not UTF-8: {}", v.display()));
                return fail(io, NAME, &e);
            }
        }
    }
    let selection = if paths.is_empty() {
        Selection::All
    } else {
        match contents.tree().select(&paths) {
            Ok(s) => s,
            Err(e) => return fail(io, NAME, &e),
        }
    };
    let cancel = Cancel::new();
    let meter = Meter::default();
    let shown = std::io::stderr().is_terminal();
    let sink: &dyn ProgressSink = if shown {
        &meter
    } else {
        &pantheon_core::NoProgress
    };
    let result = ops::extract(
        &mut contents,
        &selection,
        parent,
        name,
        &Ctx::new(&cancel, sink),
    );
    if shown {
        eprintln!();
    }
    match result {
        Ok(report) => write_or_fail(io, |o| write_report(o, &report)),
        Err(e) => fail(io, NAME, &e),
    }
}

/// `12.3` for 12,345,678 bytes.
fn megabytes(bytes: u64) -> String {
    let tenths = bytes.checked_div(TENTH_MB).unwrap_or(0);
    format!("{}.{}", tenths / 10, tenths % 10)
}

fn write_report(out: &mut dyn Write, r: &Report) -> std::io::Result<()> {
    writeln!(
        out,
        "Extracted {} files and {} folders ({} MB) to {}",
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
struct OpenError(String, std::io::Error);

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot open {}: {}", self.0, self.1)
    }
}

impl std::error::Error for OpenError {}

impl Classify for OpenError {
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
        Err(e) => fail(io, NAME, &OpenError("stdout".into(), e)),
    }
}

/// Totals of a listing: files, directories, decoded file bytes.
fn totals(tree: &Tree) -> (u64, u64, u64) {
    tree.nodes()
        .iter()
        .fold((0u64, 0u64, 0u64), |(f, d, b), n| match n.kind {
            NodeKind::File => (f.saturating_add(1), d, b.saturating_add(n.size)),
            NodeKind::Directory => (f, d.saturating_add(1), b),
        })
}

/// One line per entry (`size  path`, folders as `<dir>  path/`), then totals.
pub fn write_text(out: &mut dyn Write, tree: &Tree) -> std::io::Result<()> {
    for n in tree.nodes() {
        match n.kind {
            NodeKind::File => writeln!(out, "{:>14}  {}", n.size, n.path)?,
            NodeKind::Directory => writeln!(out, "{:>14}  {}/", "<dir>", n.path)?,
        }
    }
    let (files, dirs, bytes) = totals(tree);
    writeln!(out, "{files} files, {dirs} folders, {bytes} bytes")
}

type JResult = Result<(), pantheon_json::Error>;

/// The `pantheon.hades.listing/1` document.
pub fn write_json(out: &mut dyn Write, tree: &Tree, source: &Path) -> JResult {
    let mut w = JsonWriter::new(out);
    w.begin_object()?;
    w.key("schema")?;
    w.string(SCHEMA)?;
    w.key("source")?;
    match source.to_str() {
        Some(s) => w.string(s)?,
        None => w.null()?,
    }
    let (files, dirs, bytes) = totals(tree);
    w.key("files")?;
    w.u64(files)?;
    w.key("directories")?;
    w.u64(dirs)?;
    w.key("bytes")?;
    w.u64(bytes)?;
    w.key("entries")?;
    w.begin_array()?;
    for n in tree.nodes() {
        w.begin_object()?;
        w.key("path")?;
        w.string(&n.path)?;
        w.key("kind")?;
        w.string(n.kind.name())?;
        w.key("size")?;
        w.u64(n.size)?;
        w.end()?;
    }
    w.end()?;
    w.end()?;
    w.finish()?.write_all(b"\n")?;
    Ok(())
}
