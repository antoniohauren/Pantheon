//! Hades: explore and extract. Shows the tree inside a package and brings it
//! out, whole or in part (docs/gods/hades.md).
//!
//! shortcut: no `model` module yet; the frontend-agnostic model lands with the
//! app runtime in Phase 9, when a GUI first needs it.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

pub mod cli;
pub mod ops;

pub use ops::{Error, Report, default_folder_name, extract, open};
pub use pantheon_package::{Contents, Node, NodeKind, Resolved, Selection, Tree};

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests;
