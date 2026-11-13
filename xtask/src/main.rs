//! Developer tasks for the Pantheon workspace, run with `cargo xtask <task>`.

use std::process::ExitCode;

mod deps;
mod diff;
mod diff_hades;
mod diff_proteus;

/// Exit code for bad usage (D31).
const EXIT_USAGE: u8 = 2;
/// Exit code for a task that exists but is not implemented yet (D31 "unsupported").
const EXIT_UNSUPPORTED: u8 = 4;

/// Known tasks and, for tasks not implemented yet, the roadmap phase that implements them.
const TASKS: &[(&str, &str, Option<&str>)] = &[
    ("deps", "audit Cargo.lock against deps-allowlist.txt", None),
    ("fixtures", "generate synthetic fixtures", Some("Phase 1")),
    ("diff", "differential run against pkg-tools", None),
    (
        "payload",
        "build the Hermes console payload",
        Some("Phase 4"),
    ),
];

fn usage() -> String {
    let mut text = String::from("usage: cargo xtask <task> [args]\n\ntasks:\n");
    for (name, about, _) in TASKS {
        text.push_str(&format!("  {name:<10} {about}\n"));
    }
    text
}

/// Returns the exit code and the message to print for `args` (without argv[0]).
fn run(args: &[String]) -> (u8, String) {
    let Some(task) = args.first() else {
        return (EXIT_USAGE, usage());
    };
    match TASKS.iter().find(|(name, _, _)| name == task) {
        Some(("deps", _, _)) => deps::run(&args[1..]),
        Some(("diff", _, _)) => diff::run(&args[1..]),
        Some((name, _, Some(phase))) => (
            EXIT_UNSUPPORTED,
            format!("xtask {name}: not implemented yet ({phase})"),
        ),
        Some((name, _, None)) => (EXIT_UNSUPPORTED, format!("xtask {name}: no handler")),
        None if task == "help" || task == "--help" || task == "-h" => (0, usage()),
        None => (
            EXIT_USAGE,
            format!("xtask: unknown task `{task}`\n\n{}", usage()),
        ),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (code, message) = run(&args);
    if code == 0 {
        println!("{message}");
    } else {
        eprintln!("{message}");
    }
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(args: &[&str]) -> u8 {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        run(&args).0
    }

    #[test]
    fn dispatches_tasks() {
        assert_eq!(code(&[]), EXIT_USAGE);
        assert_eq!(code(&["help"]), 0);
        assert_eq!(code(&["nope"]), EXIT_USAGE);
        assert_eq!(code(&["deps", "--bogus"]), EXIT_USAGE);
        for (name, _, phase) in TASKS {
            if phase.is_some() {
                assert_eq!(code(&[name]), EXIT_UNSUPPORTED);
            }
        }
    }
}
