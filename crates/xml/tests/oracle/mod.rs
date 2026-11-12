//! The one-sided differential rule against roxmltree (docs/02-dependencies.md §3).
//! Shared by `tests/differential.rs` and the cargo-fuzz target `fuzz_xml`.
//!
//! - pantheon-xml accepts => roxmltree accepts and yields the same tree.
//! - pantheon-xml rejects with a subset class (`Doctype`, `UnknownEntity`,
//!   `Declaration`, `Namespace`, `Limit`) => fine whatever roxmltree does.
//!   `InvalidCharRef` is explained too: roxmltree 0.21.1 turns references above
//!   U+10FFFF into U+FFFD instead of rejecting them (XML 1.0 §4.1 forbids them).
//! - pantheon-xml rejects as malformed (any other class) => roxmltree must reject too.
//!
//! Whitespace rule for comparing trees: within each element, adjacent text and
//! CDATA (also across comments and PIs) is concatenated, and the result is dropped
//! when it consists only of XML whitespace (space, tab, CR, LF). In the rest, CR LF
//! and lone CR are folded to LF on both sides (roxmltree 0.21.1 leaves a literal
//! lone CR in text that also contains references, against XML 1.0 §2.11); the text
//! is otherwise compared exactly. Attributes are compared exactly, in document order.

use pantheon_core::Limits;
use pantheon_xml::{Element, Error, Node};

#[derive(Debug, PartialEq, Eq)]
pub enum Tree {
    Element {
        name: String,
        attributes: Vec<(String, String)>,
        children: Vec<Tree>,
    },
    Text(String),
}

/// Rejections that are a subset choice (or a known roxmltree leniency), so
/// roxmltree may accept the input.
pub fn is_subset_reject(error: &Error) -> bool {
    matches!(
        error,
        Error::Doctype { .. }
            | Error::UnknownEntity { .. }
            | Error::Declaration { .. }
            | Error::Namespace { .. }
            | Error::Limit(_)
            | Error::InvalidCharRef { .. }
    )
}

/// `Err` describes a violation of the rule.
pub fn check(input: &[u8]) -> Result<(), String> {
    let ours = pantheon_xml::parse(input, &Limits::DEFAULT);
    let theirs = std::str::from_utf8(input)
        .map_err(|e| e.to_string())
        .and_then(|text| roxmltree::Document::parse(text).map_err(|e| e.to_string()));
    match (ours, theirs) {
        (Ok(root), Ok(document)) => {
            let ours = from_ours(&root);
            let theirs = from_roxmltree(document.root_element());
            if ours == theirs {
                Ok(())
            } else {
                Err(format!(
                    "trees differ:\n ours:   {ours:?}\n theirs: {theirs:?}"
                ))
            }
        }
        (Ok(_), Err(theirs)) => Err(format!("accepted, but roxmltree rejects: {theirs}")),
        (Err(ours), _) if is_subset_reject(&ours) => Ok(()),
        (Err(_), Err(_)) => Ok(()),
        (Err(ours), Ok(_)) => Err(format!("unexplained rejection, roxmltree accepts: {ours}")),
    }
}

fn from_ours(element: &Element) -> Tree {
    let mut children = Vec::new();
    let mut text = String::new();
    for child in &element.children {
        match child {
            Node::Text(t) => text.push_str(t),
            Node::Element(e) => {
                flush_text(&mut text, &mut children);
                children.push(from_ours(e));
            }
        }
    }
    flush_text(&mut text, &mut children);
    Tree::Element {
        name: element.name.clone(),
        attributes: element.attributes.clone(),
        children,
    }
}

fn from_roxmltree(node: roxmltree::Node<'_, '_>) -> Tree {
    let mut children = Vec::new();
    let mut text = String::new();
    for child in node.children() {
        if child.is_element() {
            flush_text(&mut text, &mut children);
            children.push(from_roxmltree(child));
        } else if child.is_text() {
            text.push_str(child.text().unwrap_or_default());
        }
    }
    flush_text(&mut text, &mut children);
    Tree::Element {
        name: node.tag_name().name().to_owned(),
        attributes: node
            .attributes()
            .map(|a| (a.name().to_owned(), a.value().to_owned()))
            .collect(),
        children,
    }
}

fn flush_text(text: &mut String, children: &mut Vec<Tree>) {
    let text = std::mem::take(text);
    if !text.chars().all(|c| matches!(c, ' ' | '\t' | '\r' | '\n')) {
        children.push(Tree::Text(text.replace("\r\n", "\n").replace('\r', "\n")));
    }
}
