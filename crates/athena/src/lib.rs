//! Athena: inspect. Tells you what a package *is*, without changing anything
//! (docs/gods/athena.md).
//!
//! shortcut: no `model` module yet; the frontend-agnostic model lands with the
//! app runtime in Phase 9, when a GUI first needs it.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

pub mod cli;
pub mod ops;

pub use ops::{
    ContentKind, Details, Error, Inspection, SfoEntry, SfoStatus, SfoTable, SfoValue, Summary,
    artwork, displayable_png, inspect,
};
pub use pantheon_package::{Artwork, Kind};

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests;
