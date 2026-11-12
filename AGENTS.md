# Pantheon

> Pure-Rust toolkit for PlayStation 3 and PlayStation 4 packages: small CLI tools named after Greek gods, plus a hub app (GTK first, then GPUI).

Instructions for AI coding agents working in this repository.

## Setup

- Toolchain is pinned in `rust-toolchain.toml` (edition 2024; MSRV = that toolchain).
- Build: `cargo build`
- Run tests: `cargo test`
- Lint / format: `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings`
- Dependency audit: `cargo xtask deps` (and `cargo xtask deps --core` for L0–L3)

## Code style

- Enforce strict typing across the codebase.
- Keep code human-readable; use named constants instead of magic numbers.
- Run `cargo fmt`; `cargo clippy` must pass with no warnings.
- Prefer `Result` over panics in library code. Errors map to the CLI exit codes in D31.
- No `unwrap()` in non-test code without a justifying comment.

## Project structure

- `crates/`: libraries, god crates, `cli`, `app`, `gtk`, `gpui`, dev-only `testalloc`.
- `console/`: `hermes-wire`, `hermes-payload` (built explicitly, not a default member).
- `xtask/`: payload builder, dependency audit, fixtures, differential runs.
- `fixtures/`: synthetic generators + tiny committed synthetic fixtures.
- `reference/`: independent reference checks.
- `docs/`: design docs. Read `docs/00-decisions.md` and `docs/06-roadmap.md` first;
  the decision log overrides other docs.
- Full layout: `docs/01-architecture.md` §8.

## Guardrails

Things agents get wrong here. Follow these strictly:

- Don't edit `Cargo.lock` by hand.
- `unsafe_code` is forbidden workspace-wide. The only exceptions are `hermes-payload`
  (`sys.rs`), `pantheon-testalloc` (D14) and `pantheon-fs` on Windows (D22); see
  `docs/01-architecture.md` §9. Each `unsafe` block needs a comment proving its soundness.
- Run the full test suite and fix any failures before marking a task complete.
- New dependencies need a PR that updates `docs/02-dependencies.md`, `deps-allowlist.txt`
  and the reason; `cargo xtask deps` fails CI otherwise. Only `pantheon-crypto`
  depends on crypto crates directly (P6).
- Implement simple functionality directly instead of adding a dependency.
- pkg-tools (`~/Projects/pkg-tools` @ `20e3c3a`) is the behavioural reference. Don't port it line by line.
- Clean-room rule for Hermes (D27): never read DPI's GPL source.
- Fixtures use only public or test keys. No game data, ever.
- Support claims name their evidence kind (`docs/05-testing.md`); real-target results go in
  `docs/verification-log.md`.
- Never commit secrets, API keys, or `.env` files.
- Match existing patterns. Don't introduce a new library for something the codebase already solves.
- Keep changes scoped to the task. Don't reformat or refactor unrelated files.
- Ask before large refactors, renaming public APIs, or changing a recorded decision.

## Commits & pull requests

- Use Conventional Commits: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`.
- Keep PRs focused on a single concern; describe what changed and why.
