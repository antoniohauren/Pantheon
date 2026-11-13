//! Hermes: send PS4 packages to a console on the LAN, through our own payload
//! and wire protocol (docs/gods/hermes.md, D27).
//!
//! The host validates the whole batch, binds a control listener and an HTTP
//! range server to the one local IPv4 that routes to the console, sends the
//! patched payload to the loader, and then feeds the payload one `Job` per
//! package. Each package reports three separate outcomes: *transferred*,
//! *registered* and *installed* (the last only if spike S4 works).
//!
//! shortcut: no `model` module yet; the frontend-agnostic model lands with the
//! app runtime in Phase 9, when a GUI first needs it.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

mod batch;
pub mod cli;
mod http;
pub mod ops;
mod session;

pub use hermes_wire::{Stage, TaskState};
pub use ops::{
    ConsoleFailure, Error, ItemReport, Loader, Payload, ProbeReport, SendReport, Target, probe,
    send,
};

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests;
