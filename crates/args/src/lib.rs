//! pantheon-args: the shared CLI framework for god crates (D36).
//!
//! A small argument parser (P4): long and short flags, `--key=value` and
//! `--key value`, positionals, `--`, and non-UTF-8 values through `OsString`.
//! Help text is generated from the same static [`Spec`] the parser reads, so the
//! two cannot drift apart. Also: the [`Io`] pair every god's `cli::run` writes
//! to, and the exit-code mapping (D31).

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as _};
use std::io::Write;

use pantheon_core::{Classify, ErrorKind};

/// One option of a verb.
#[derive(Clone, Copy, Debug)]
pub struct Opt {
    /// Without the leading `--`.
    pub long: &'static str,
    pub short: Option<char>,
    /// The value's placeholder (e.g. `"FILE"`); `None` for a plain flag.
    pub value: Option<&'static str>,
    pub help: &'static str,
}

impl Opt {
    pub const fn flag(long: &'static str, help: &'static str) -> Opt {
        Opt {
            long,
            short: None,
            value: None,
            help,
        }
    }
}

/// `--json`, shared by every verb with machine-readable output.
pub const JSON: Opt = Opt::flag("json", "Machine-readable output on stdout");

/// One required positional argument.
#[derive(Clone, Copy, Debug)]
pub struct Positional {
    pub name: &'static str,
    pub help: &'static str,
}

/// A verb: its usage, positionals and options.
#[derive(Debug)]
pub struct Spec {
    pub verb: &'static str,
    pub summary: &'static str,
    pub positionals: &'static [Positional],
    pub options: &'static [Opt],
    /// Options of which exactly one must be given (e.g. `--icon0 | --pic1`).
    pub one_of: &'static [&'static str],
}

/// A god's command set.
#[derive(Debug)]
pub struct Command {
    pub name: &'static str,
    pub summary: &'static str,
    pub verbs: &'static [&'static Spec],
}

/// Parsed arguments of one verb.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    positionals: Vec<OsString>,
    options: Vec<(&'static str, Option<OsString>)>,
}

impl Args {
    pub fn positional(&self, index: usize) -> Option<&OsStr> {
        self.positionals.get(index).map(OsString::as_os_str)
    }

    pub fn flag(&self, long: &str) -> bool {
        self.options.iter().any(|(name, _)| *name == long)
    }

    /// The last value given for `long`.
    pub fn value(&self, long: &str) -> Option<&OsStr> {
        self.options
            .iter()
            .rev()
            .find(|(name, _)| *name == long)
            .and_then(|(_, v)| v.as_deref())
    }

    /// Every value given for a repeatable option, in order.
    pub fn values(&self, long: &str) -> Vec<&OsStr> {
        self.options
            .iter()
            .filter(|(name, _)| *name == long)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }
}

/// What the user asked for.
#[derive(Debug)]
pub enum Invocation {
    Run(&'static Spec, Args),
    /// Print this help text and exit 0.
    Help(String),
}

/// Bad arguments (exit code 2). The message already names the fix.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

impl Classify for UsageError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Usage
    }
}

fn is_help(arg: &OsStr) -> bool {
    arg == "-h" || arg == "--help"
}

/// Parses `argv` (without the program name) against `cmd`.
/// `help`, `-h` and `--help` work at god level, and `help VERB` or `VERB --help`
/// at verb level.
pub fn parse(cmd: &Command, argv: &[OsString]) -> Result<Invocation, UsageError> {
    let Some((first, rest)) = argv.split_first() else {
        return Err(UsageError(format!("{}missing command", command_help(cmd))));
    };
    if is_help(first) || first == "help" {
        return match rest.first() {
            None => Ok(Invocation::Help(command_help(cmd))),
            Some(verb) => find(cmd, verb).map(|spec| Invocation::Help(verb_help(cmd, spec))),
        };
    }
    let spec = find(cmd, first)?;
    if rest.iter().take_while(|a| *a != "--").any(|a| is_help(a)) {
        return Ok(Invocation::Help(verb_help(cmd, spec)));
    }
    parse_verb(cmd, spec, rest).map(|args| Invocation::Run(spec, args))
}

fn find(cmd: &Command, verb: &OsStr) -> Result<&'static Spec, UsageError> {
    cmd.verbs
        .iter()
        .copied()
        .find(|s| verb == s.verb)
        .ok_or_else(|| {
            UsageError(format!(
                "{}: unknown command {:?}; try '{} --help'",
                cmd.name,
                verb.to_string_lossy(),
                cmd.name
            ))
        })
}

fn parse_verb(cmd: &Command, spec: &'static Spec, argv: &[OsString]) -> Result<Args, UsageError> {
    let usage = |what: String| {
        UsageError(format!(
            "{} {}: {what}; try '{} {} --help'",
            cmd.name, spec.verb, cmd.name, spec.verb
        ))
    };
    let mut args = Args::default();
    let mut rest = argv.iter();
    let mut only_positionals = false;
    while let Some(arg) = rest.next() {
        let text = arg.to_str();
        if only_positionals || !text.is_some_and(|t| t.starts_with('-') && t.len() > 1) {
            args.positionals.push(arg.clone());
            continue;
        }
        let text = text.unwrap_or_default();
        if text == "--" {
            only_positionals = true;
            continue;
        }
        let (opt, inline) = if let Some(long) = text.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(OsString::from(value))),
                None => (long, None),
            };
            let opt = spec.options.iter().find(|o| o.long == name);
            (
                opt.ok_or_else(|| usage(format!("unknown option --{name}")))?,
                inline,
            )
        } else {
            let mut chars = text.chars().skip(1);
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return Err(usage(format!("unknown option {text}")));
            };
            let opt = spec.options.iter().find(|o| o.short == Some(c));
            (
                opt.ok_or_else(|| usage(format!("unknown option -{c}")))?,
                None,
            )
        };
        let value = match (opt.value, inline) {
            (None, None) => None,
            (None, Some(_)) => return Err(usage(format!("--{} takes no value", opt.long))),
            (Some(_), Some(v)) => Some(v),
            (Some(meta), None) => Some(
                rest.next()
                    .cloned()
                    .ok_or_else(|| usage(format!("--{} needs a value {meta}", opt.long)))?,
            ),
        };
        args.options.push((opt.long, value));
    }
    let wanted = spec.positionals.len();
    if args.positionals.len() != wanted {
        let names: Vec<_> = spec.positionals.iter().map(|p| p.name).collect();
        return Err(usage(format!(
            "expected {wanted} argument(s) ({}), got {}",
            names.join(" "),
            args.positionals.len()
        )));
    }
    if !spec.one_of.is_empty() {
        let given = spec.one_of.iter().filter(|name| args.flag(name)).count();
        if given != 1 {
            let names: Vec<_> = spec.one_of.iter().map(|n| format!("--{n}")).collect();
            return Err(usage(format!("give exactly one of {}", names.join(", "))));
        }
    }
    Ok(args)
}

fn usage_line(cmd: &Command, spec: &Spec) -> String {
    let mut line = format!("{} {}", cmd.name, spec.verb);
    for p in spec.positionals {
        let _ = write!(line, " {}", p.name);
    }
    if !spec.one_of.is_empty() {
        let names: Vec<_> = spec.one_of.iter().map(|n| format!("--{n}")).collect();
        let _ = write!(line, " ({})", names.join("|"));
    }
    if spec.options.iter().any(|o| !spec.one_of.contains(&o.long)) {
        line.push_str(" [OPTIONS]");
    }
    line
}

/// God-level help: one usage line per verb.
pub fn command_help(cmd: &Command) -> String {
    let mut out = format!("{}: {}\n\nUsage:\n", cmd.name, cmd.summary);
    for spec in cmd.verbs {
        let _ = writeln!(out, "  {}\n      {}", usage_line(cmd, spec), spec.summary);
    }
    let _ = writeln!(out, "\nRun '{} help VERB' for a verb's options.", cmd.name);
    out
}

/// Verb-level help, generated from the spec the parser uses.
pub fn verb_help(cmd: &Command, spec: &Spec) -> String {
    let mut out = format!("{}\n\nUsage: {}\n", spec.summary, usage_line(cmd, spec));
    if !spec.positionals.is_empty() {
        out.push_str("\nArguments:\n");
        for p in spec.positionals {
            let _ = writeln!(out, "  {:<22}{}", p.name, p.help);
        }
    }
    out.push_str("\nOptions:\n");
    for o in spec.options {
        let mut name = match o.short {
            Some(c) => format!("-{c}, --{}", o.long),
            None => format!("--{}", o.long),
        };
        if let Some(meta) = o.value {
            let _ = write!(name, " {meta}");
        }
        let _ = writeln!(out, "  {name:<22}{}", o.help);
    }
    let _ = writeln!(out, "  {:<22}Show this help", "-h, --help");
    out
}

/// Where a god's CLI writes. Diagnostics go to `err`, results to `out`.
pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

/// Reports `error` on `io.err` as `"<name>: <error>"` and returns its exit code (D31).
pub fn fail(io: &mut Io<'_>, name: &str, error: &dyn Classify) -> u8 {
    // A failed diagnostic write has nowhere left to go; the exit code still reports the error.
    let _ = writeln!(io.err, "{name}: {error}");
    error.kind().exit_code()
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests;
