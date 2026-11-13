//! Proteus: the shape-shifter. Turns a folder or a PS4 package into a ZArchive
//! (`.zar`) that shadPS4 can run (docs/gods/proteus.md).
//!
//! shortcut: no `model` module yet; the frontend-agnostic model lands with the
//! app runtime in Phase 9, when a GUI first needs it.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

pub mod cli;
pub mod ops;

pub use ops::{ConvertReport, Error, Report, convert, pack, update_path};

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests;
