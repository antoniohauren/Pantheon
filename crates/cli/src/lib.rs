//! pantheon-cli: the `pantheon` multiplexer and the per-god binaries (D5, D17, P11).
//!
//! shortcut: no Ctrl+C handler (`ctrlc`, D15) yet; Athena's reads are short.
//! Add it with the first long-running god (Hades, Phase 3).

use std::ffi::OsString;
use std::process::ExitCode;

use pantheon_args::{Io, UsageError, fail};

/// A god the multiplexer can dispatch to.
struct God {
    name: &'static str,
    summary: &'static str,
    run: fn(&[OsString], &mut Io<'_>) -> u8,
}

const GODS: &[God] = &[God {
    name: "athena",
    summary: athena::cli::COMMAND.summary,
    run: athena::cli::run,
}];

const NAME: &str = "pantheon";

fn hub_help() -> String {
    let mut text = String::from(
        "pantheon: PS3 and PS4 package tools\n\nUsage:\n  pantheon <god> <verb> [ARGS] [OPTIONS]\n  pantheon help [<god> [<verb>]]\n  pantheon --version\n\nGods:\n",
    );
    for god in GODS {
        text.push_str(&format!("  {:<10}{}\n", god.name, god.summary));
    }
    text
}

/// `pantheon ARGS…` (program name excluded). The god's own `run` does the work;
/// the multiplexer only strips the first argument.
pub fn pantheon(argv: &[OsString], io: &mut Io<'_>) -> u8 {
    let Some((first, rest)) = argv.split_first() else {
        return fail(io, NAME, &UsageError(format!("{}missing god", hub_help())));
    };
    if first == "--version" || first == "-V" {
        let _ = writeln!(io.out, "pantheon {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let (god_name, god_args): (&OsString, Vec<OsString>) =
        if first == "help" || first == "-h" || first == "--help" {
            match rest.split_first() {
                None => {
                    let _ = io.out.write_all(hub_help().as_bytes());
                    return 0;
                }
                // `pantheon help athena [verb]` is `athena help [verb]`.
                Some((god, verb)) => (
                    god,
                    std::iter::once("help".into())
                        .chain(verb.iter().cloned())
                        .collect(),
                ),
            }
        } else {
            (first, rest.to_vec())
        };
    match GODS.iter().find(|g| god_name == g.name) {
        Some(god) => (god.run)(&god_args, io),
        None => fail(
            io,
            NAME,
            &UsageError(format!(
                "unknown god {:?}; try 'pantheon help'",
                god_name.to_string_lossy()
            )),
        ),
    }
}

/// Runs `run` with the process arguments and real stdout/stderr.
pub fn main_with(run: fn(&[OsString], &mut Io<'_>) -> u8) -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let (stdout, stderr) = (std::io::stdout(), std::io::stderr());
    let (mut out, mut err) = (stdout.lock(), stderr.lock());
    ExitCode::from(run(
        &argv,
        &mut Io {
            out: &mut out,
            err: &mut err,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> (u8, String, String) {
        let argv: Vec<OsString> = args.iter().map(OsString::from).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = pantheon(
            &argv,
            &mut Io {
                out: &mut out,
                err: &mut err,
            },
        );
        (
            code,
            String::from_utf8(out).unwrap_or_default(),
            String::from_utf8(err).unwrap_or_default(),
        )
    }

    #[test]
    fn dispatches_and_helps() {
        assert_eq!(run(&[]).0, 2);
        let (code, out, _) = run(&["--version"]);
        assert_eq!((code, out.starts_with("pantheon ")), (0, true));
        let (code, out, _) = run(&["help"]);
        assert!(code == 0 && out.contains("athena"));
        let (code, out, _) = run(&["help", "athena", "inspect"]);
        assert!(code == 0 && out.contains("athena inspect INPUT"), "{out}");
        let (code, out, _) = run(&["athena", "--help"]);
        assert!(code == 0 && out.contains("athena artwork"));
        let (code, _, err) = run(&["zeus"]);
        assert!(code == 2 && err.contains("unknown god"));
        assert_eq!(run(&["athena", "inspect"]).0, 2);
    }
}
