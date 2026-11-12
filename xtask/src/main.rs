//! Developer tasks for the Pantheon workspace, run with `cargo xtask <task>`.

use std::process::ExitCode;

/// Exit code for bad usage (D31).
const EXIT_USAGE: u8 = 2;
/// Exit code for a task that exists but is not implemented yet (D31 "unsupported").
const EXIT_UNSUPPORTED: u8 = 4;

/// Known tasks and the roadmap phase that implements each one.
const TASKS: &[(&str, &str, &str)] = &[
    (
        "deps",
        "audit Cargo.lock against deps-allowlist.txt",
        "Phase 1",
    ),
    ("fixtures", "generate synthetic fixtures", "Phase 1"),
    ("diff", "differential run against pkg-tools", "Phase 2"),
    ("payload", "build the Hermes console payload", "Phase 4"),
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
        Some((name, _, phase)) => (
            EXIT_UNSUPPORTED,
            format!("xtask {name}: not implemented yet ({phase})"),
        ),
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
        for (name, _, _) in TASKS {
            assert_eq!(code(&[name]), EXIT_UNSUPPORTED);
        }
    }
}
