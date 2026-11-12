//! pantheon-xml: a bounded XML subset reader and writer (docs/03-foundation.md).
//!
//! **Reader** ([`Reader`], or [`parse`] for an owned tree): UTF-8 only; the XML
//! declaration, elements, attributes, text, CDATA, the five predefined entities
//! and numeric character references. Comments and processing instructions are
//! skipped. Line endings are normalised and attribute whitespace is replaced
//! by spaces, as XML 1.0 §2.11 and §3.3.3 require.
//!
//! Reject classes (see [`Error`]): malformed input (`Utf8`, `InvalidChar`,
//! `Syntax`, `Unbalanced`, `DuplicateAttribute`, `InvalidCharRef`) and inputs that
//! are valid XML but outside the subset: `Doctype`, `UnknownEntity`, `Declaration`
//! (non-UTF-8 encoding, version other than 1.x, or a malformed or misplaced
//! declaration), `Namespace` (prefixed names or `xmlns` attributes) and `Limit`.
//!
//! **Writer** ([`XmlWriter`]): compact or indented output with escaping.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

mod chars;
mod error;
mod reader;
mod tree;
mod writer;

pub use error::Error;
pub use reader::{Attribute, Event, Reader};
pub use tree::{Element, Node, parse};
pub use writer::{Indent, XmlWriter};

/// Maximum element nesting depth.
pub const MAX_DEPTH: u64 = 64;
/// Maximum attributes on one element.
pub const MAX_ATTRIBUTES: u64 = 64;
/// Maximum length of one element, attribute or PI name, in bytes.
pub const MAX_NAME_BYTES: u64 = 256;
/// Maximum nodes (elements, text runs, CDATA sections, comments, PIs) in one document.
pub const MAX_NODES: u64 = 1_000_000;
