//! An owned tree built from the pull parser, for callers that want random access.

use pantheon_core::Limits;

use crate::{Error, Event, Reader};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    /// In document order.
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Element(Element),
    /// Adjacent text, CDATA and character data separated only by comments or
    /// PIs are merged into one node. Whitespace is kept as is.
    Text(String),
}

impl Element {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Parses a whole document and returns its root element.
pub fn parse(input: &[u8], limits: &Limits) -> Result<Element, Error> {
    let mut reader = Reader::new(input, limits)?;
    let mut open: Vec<Element> = Vec::new();
    let mut root = None;
    while let Some(event) = reader.next_event()? {
        match event {
            Event::Start { name, attributes } => open.push(Element {
                name: name.to_owned(),
                attributes: attributes
                    .into_iter()
                    .map(|a| (a.name.to_owned(), a.value.into_owned()))
                    .collect(),
                children: Vec::new(),
            }),
            Event::Text(text) => {
                let Some(parent) = open.last_mut() else {
                    continue;
                };
                match parent.children.last_mut() {
                    Some(Node::Text(previous)) => previous.push_str(&text),
                    _ => parent.children.push(Node::Text(text.into_owned())),
                }
            }
            Event::End { .. } => {
                let Some(done) = open.pop() else {
                    continue;
                };
                match open.last_mut() {
                    Some(parent) => parent.children.push(Node::Element(done)),
                    None => root = Some(done),
                }
            }
        }
    }
    // The reader only finishes after closing a root element, so this is a fallback.
    root.ok_or(Error::Syntax {
        offset: input.len(),
        what: "no root element",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_merged_tree() {
        let root = parse(
            b"<r k='v'>a<!--c-->b<![CDATA[c]]><e/> </r>",
            &Limits::DEFAULT,
        )
        .unwrap();
        assert_eq!(root.attribute("k"), Some("v"));
        assert_eq!(root.attribute("missing"), None);
        assert_eq!(
            root.children,
            vec![
                Node::Text("abc".into()),
                Node::Element(Element {
                    name: "e".into(),
                    attributes: vec![],
                    children: vec![]
                }),
                Node::Text(" ".into()),
            ]
        );
    }
}
