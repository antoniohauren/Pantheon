//! Streaming XML writer.
//!
//! The compact layout with a declaration matches how pkg-tools writes GP4 and
//! PlayGo manifests: `<?xml version="1.0" encoding="utf-8"?>\n`, the whole
//! element tree on one line, empty elements self-closed (`<a x="1"/>`), and a
//! trailing newline. Text and attribute values get the same escaping:
//! `& < > " '` and `\n \r \t` as entity or character references.

use std::io::Write;

use crate::Error;
use crate::chars::{is_plain_name, is_xml_char};

/// Layout of the element tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    /// Everything on one line (the pkg-tools GP4 layout).
    Compact,
    /// Each child element on its own line, indented by this many spaces per level.
    /// Elements holding only text stay on one line.
    Spaces(usize),
}

struct Open {
    name: String,
    has_elements: bool,
}

/// Writes one XML document to `W`. Every call validates its input so the
/// output is always well-formed and parseable by [`crate::Reader`].
pub struct XmlWriter<W: Write> {
    out: W,
    indent: Indent,
    open: Vec<Open>,
    /// A start tag is still open for attributes (`<name a="1"` without `>`).
    in_tag: bool,
    started: bool,
    root_done: bool,
}

impl<W: Write> XmlWriter<W> {
    pub fn new(out: W, indent: Indent) -> XmlWriter<W> {
        XmlWriter {
            out,
            indent,
            open: Vec::new(),
            in_tag: false,
            started: false,
            root_done: false,
        }
    }

    /// `<?xml version="1.0" encoding="utf-8"?>` (plus `standalone="yes"`) and a newline.
    /// Only allowed first.
    pub fn declaration(&mut self, standalone: bool) -> Result<(), Error> {
        if self.started {
            return Err(Error::Misuse("XML declaration after content"));
        }
        self.started = true;
        self.out
            .write_all(b"<?xml version=\"1.0\" encoding=\"utf-8\"")?;
        if standalone {
            self.out.write_all(b" standalone=\"yes\"")?;
        }
        self.out.write_all(b"?>\n")?;
        Ok(())
    }

    pub fn start(&mut self, name: &str) -> Result<(), Error> {
        if self.root_done {
            return Err(Error::Misuse("second root element"));
        }
        check_name(name)?;
        self.close_tag()?;
        if let Some(parent) = self.open.last_mut() {
            parent.has_elements = true;
            self.newline()?;
        }
        self.started = true;
        self.out.write_all(b"<")?;
        self.out.write_all(name.as_bytes())?;
        self.open.push(Open {
            name: name.to_owned(),
            has_elements: false,
        });
        self.in_tag = true;
        Ok(())
    }

    /// Adds an attribute to the element just started.
    // shortcut: duplicate attribute names are not detected; track names here if a caller ever builds them dynamically.
    pub fn attribute(&mut self, name: &str, value: &str) -> Result<(), Error> {
        if !self.in_tag {
            return Err(Error::Misuse("attribute outside a start tag"));
        }
        check_name(name)?;
        self.out.write_all(b" ")?;
        self.out.write_all(name.as_bytes())?;
        self.out.write_all(b"=\"")?;
        write_escaped(&mut self.out, value)?;
        self.out.write_all(b"\"")?;
        Ok(())
    }

    pub fn text(&mut self, text: &str) -> Result<(), Error> {
        if self.open.is_empty() {
            return Err(Error::Misuse("text outside the root element"));
        }
        self.close_tag()?;
        write_escaped(&mut self.out, text)?;
        Ok(())
    }

    /// Closes the innermost element; an element without content is self-closed.
    pub fn end(&mut self) -> Result<(), Error> {
        let element = self
            .open
            .pop()
            .ok_or(Error::Misuse("no open element to end"))?;
        if std::mem::take(&mut self.in_tag) {
            self.out.write_all(b"/>")?;
        } else {
            if element.has_elements {
                self.newline()?;
            }
            self.out.write_all(b"</")?;
            self.out.write_all(element.name.as_bytes())?;
            self.out.write_all(b">")?;
        }
        self.root_done = self.open.is_empty();
        Ok(())
    }

    /// Checks the root element is closed, writes the trailing newline, flushes
    /// and returns the output.
    pub fn finish(mut self) -> Result<W, Error> {
        if !self.root_done {
            return Err(Error::Misuse("document incomplete"));
        }
        self.out.write_all(b"\n")?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn close_tag(&mut self) -> Result<(), Error> {
        if std::mem::take(&mut self.in_tag) {
            self.out.write_all(b">")?;
        }
        Ok(())
    }

    /// A newline and indentation for the current depth (indented layout only).
    fn newline(&mut self) -> Result<(), Error> {
        let Indent::Spaces(width) = self.indent else {
            return Ok(());
        };
        self.out.write_all(b"\n")?;
        for _ in 0..self.open.len() {
            for _ in 0..width {
                self.out.write_all(b" ")?;
            }
        }
        Ok(())
    }
}

fn check_name(name: &str) -> Result<(), Error> {
    if is_plain_name(name) {
        Ok(())
    } else {
        Err(Error::Misuse("invalid or prefixed XML name"))
    }
}

fn write_escaped(out: &mut impl Write, s: &str) -> Result<(), Error> {
    if !s.chars().all(is_xml_char) {
        return Err(Error::Misuse("character not representable in XML 1.0"));
    }
    let mut rest = s;
    // Escaped characters are all ASCII, so splitting around them keeps UTF-8 intact.
    while let Some(at) = rest.find(['&', '<', '>', '"', '\'', '\n', '\r', '\t']) {
        let (plain, tail) = rest.split_at_checked(at).unwrap_or((rest, ""));
        out.write_all(plain.as_bytes())?;
        let escaped: &[u8] = match tail.as_bytes().first() {
            Some(b'&') => b"&amp;",
            Some(b'<') => b"&lt;",
            Some(b'>') => b"&gt;",
            Some(b'"') => b"&quot;",
            Some(b'\'') => b"&apos;",
            Some(b'\n') => b"&#10;",
            Some(b'\r') => b"&#13;",
            _ => b"&#9;",
        };
        out.write_all(escaped)?;
        rest = tail.get(1..).unwrap_or_default();
    }
    out.write_all(rest.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Node, parse};
    use pantheon_core::Limits;

    fn sample(indent: Indent) -> String {
        let mut w = XmlWriter::new(Vec::new(), indent);
        w.declaration(false).unwrap();
        w.start("psproject").unwrap();
        w.attribute("fmt", "gp4").unwrap();
        w.start("volume").unwrap();
        w.start("volume_type").unwrap();
        w.text("pkg_ps4_app").unwrap();
        w.end().unwrap();
        w.end().unwrap();
        w.start("files").unwrap();
        w.start("file").unwrap();
        w.attribute("targ_path", "a&b <\"x\">\t'y'\n").unwrap();
        w.end().unwrap();
        w.end().unwrap();
        w.end().unwrap();
        String::from_utf8(w.finish().unwrap()).unwrap()
    }

    #[test]
    fn compact_layout_matches_pkg_tools_style() {
        assert_eq!(
            sample(Indent::Compact),
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<psproject fmt=\"gp4\"><volume><volume_type>pkg_ps4_app</volume_type></volume><files><file targ_path=\"a&amp;b &lt;&quot;x&quot;&gt;&#9;&apos;y&apos;&#10;\"/></files></psproject>\n"
        );
    }

    #[test]
    fn indented_layout() {
        assert_eq!(
            sample(Indent::Spaces(2)),
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<psproject fmt=\"gp4\">\n  <volume>\n    <volume_type>pkg_ps4_app</volume_type>\n  </volume>\n  <files>\n    <file targ_path=\"a&amp;b &lt;&quot;x&quot;&gt;&#9;&apos;y&apos;&#10;\"/>\n  </files>\n</psproject>\n"
        );
    }

    #[test]
    fn output_round_trips_through_the_reader() {
        let root = parse(sample(Indent::Compact).as_bytes(), &Limits::DEFAULT).unwrap();
        let Some(Node::Element(files)) = root.children.get(1) else {
            panic!("files expected");
        };
        let Some(Node::Element(file)) = files.children.first() else {
            panic!("file expected");
        };
        assert_eq!(file.attribute("targ_path"), Some("a&b <\"x\">\t'y'\n"));
    }

    #[test]
    fn misuse_is_rejected() {
        let mut w = XmlWriter::new(Vec::new(), Indent::Compact);
        assert!(matches!(w.text("x"), Err(Error::Misuse(_))));
        assert!(matches!(w.end(), Err(Error::Misuse(_))));
        assert!(matches!(w.start("bad name"), Err(Error::Misuse(_))));
        assert!(matches!(w.start("p:a"), Err(Error::Misuse(_))));
        w.start("a").unwrap();
        assert!(matches!(w.declaration(false), Err(Error::Misuse(_))));
        assert!(matches!(w.text("\u{1}"), Err(Error::Misuse(_))));
        w.text("t").unwrap();
        assert!(matches!(w.attribute("k", "v"), Err(Error::Misuse(_))));
        w.end().unwrap();
        assert!(matches!(w.start("b"), Err(Error::Misuse(_))));
        assert_eq!(w.finish().unwrap(), b"<a>t</a>\n");

        let w = XmlWriter::new(Vec::new(), Indent::Compact);
        assert!(matches!(w.finish(), Err(Error::Misuse(_))));
    }
}
