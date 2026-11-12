//! The pull parser.

use std::borrow::Cow;

use pantheon_core::{Limit, Limits};

use crate::chars::{is_name_char, is_name_start, is_plain_name, is_space, is_xml_char};
use crate::{Error, MAX_ATTRIBUTES, MAX_DEPTH, MAX_NAME_BYTES, MAX_NODES};

const BOM: char = '\u{FEFF}';
const UTF8_LABEL: &str = "utf-8";

/// One attribute, value decoded and normalised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute<'a> {
    pub name: &'a str,
    pub value: Cow<'a, str>,
}

/// One parse event. `<a/>` yields `Start` then `End`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    Start {
        name: &'a str,
        attributes: Vec<Attribute<'a>>,
    },
    End {
        name: &'a str,
    },
    /// A decoded text run or CDATA section inside the root element. Adjacent
    /// runs are not merged, and whitespace-only runs are reported too.
    Text(Cow<'a, str>),
}

/// How [`decode`] normalises literal whitespace.
#[derive(Clone, Copy)]
enum Mode {
    Text,
    Attribute,
}

/// Pull parser over a complete in-memory document.
pub struct Reader<'a> {
    input: &'a str,
    rest: &'a str,
    stack: Vec<&'a str>,
    nodes: u64,
    seen_root: bool,
    /// The element just started was self-closing; its `End` is due next.
    pending_end: bool,
}

impl<'a> Reader<'a> {
    /// Checks size, UTF-8 and characters, skips a BOM and parses the XML declaration.
    pub fn new(input: &'a [u8], limits: &Limits) -> Result<Reader<'a>, Error> {
        Limit::new("xml size", limits.max_xml)
            .check(u64::try_from(input.len()).unwrap_or(u64::MAX))?;
        let text = std::str::from_utf8(input).map_err(|e| Error::Utf8 {
            offset: e.valid_up_to(),
        })?;
        // Every place a character may appear requires an XML `Char`, so one pass covers them all.
        if let Some((offset, _)) = text.char_indices().find(|&(_, c)| !is_xml_char(c)) {
            return Err(Error::InvalidChar { offset });
        }
        let mut reader = Reader {
            input: text,
            rest: text.strip_prefix(BOM).unwrap_or(text),
            stack: Vec::new(),
            nodes: 0,
            seen_root: false,
            pending_end: false,
        };
        reader.declaration()?;
        Ok(reader)
    }

    /// The next event, or `None` once the root element is closed and only
    /// whitespace, comments and PIs follow.
    pub fn next_event(&mut self) -> Result<Option<Event<'a>>, Error> {
        if std::mem::take(&mut self.pending_end) {
            let name = self.stack.pop().ok_or(Error::Unbalanced {
                offset: self.offset(),
            })?;
            return Ok(Some(Event::End { name }));
        }
        loop {
            if self.stack.is_empty() {
                self.skip_space();
                if self.rest.is_empty() {
                    return if self.seen_root {
                        Ok(None)
                    } else {
                        Err(self.syntax("no root element"))
                    };
                }
                if !self.rest.starts_with('<') {
                    return Err(self.syntax("text outside the root element"));
                }
            } else if self.rest.is_empty() {
                return Err(Error::Unbalanced {
                    offset: self.offset(),
                });
            } else if !self.rest.starts_with('<') {
                return self.text().map(Some);
            }

            if self.rest.starts_with("<!--") {
                self.comment()?;
            } else if self.rest.starts_with("<?") {
                self.processing_instruction()?;
            } else if self.rest.starts_with("<![CDATA[") {
                return self.cdata().map(Some);
            } else if self.rest.starts_with("<!DOCTYPE") {
                return Err(Error::Doctype {
                    offset: self.offset(),
                });
            } else if self.rest.starts_with("</") {
                return self.end_tag().map(Some);
            } else {
                return self.start_tag().map(Some);
            }
        }
    }

    // --- markup -----------------------------------------------------------

    fn declaration(&mut self) -> Result<(), Error> {
        let start = self.offset();
        // `<?xmlfoo` is a PI; a bare `<?xml?>` is rejected later as a misplaced declaration.
        match self.rest.strip_prefix("<?xml") {
            Some(after) if after.starts_with(is_space) => self.rest = after,
            _ => return Ok(()),
        }
        let unsupported = |what| Error::Declaration {
            offset: start,
            what,
        };
        let malformed = unsupported("malformed XML declaration");
        let fields = self.declaration_fields().map_err(|_| malformed)?;
        let mut fields = fields.into_iter().peekable();
        let version = match fields.next() {
            Some(("version", value)) => value,
            _ => return Err(unsupported("malformed XML declaration")),
        };
        let encoding = fields
            .next_if(|&(name, _)| name == "encoding")
            .map(|(_, v)| v);
        let standalone = fields
            .next_if(|&(name, _)| name == "standalone")
            .map(|(_, v)| v);
        if fields.next().is_some() {
            return Err(unsupported("malformed XML declaration"));
        }
        let minor = version.strip_prefix("1.").unwrap_or_default();
        if minor.is_empty() || !minor.bytes().all(|b| b.is_ascii_digit()) {
            return Err(unsupported("XML version other than 1.x"));
        }
        if encoding.is_some_and(|e| !e.eq_ignore_ascii_case(UTF8_LABEL)) {
            return Err(unsupported("encoding other than UTF-8"));
        }
        if standalone.is_some_and(|s| s != "yes" && s != "no") {
            return Err(unsupported("standalone other than yes or no"));
        }
        Ok(())
    }

    /// The `name="value"` pairs of the XML declaration, through `?>`.
    fn declaration_fields(&mut self) -> Result<Vec<(&'a str, &'a str)>, Error> {
        let mut fields = Vec::new();
        loop {
            let spaced = self.skip_space();
            if self.eat("?>") {
                return Ok(fields);
            }
            if !spaced {
                return Err(self.syntax("expected whitespace in the XML declaration"));
            }
            let name = self.take_while(is_name_char);
            self.equals()?;
            let (value, _) = self.quoted()?;
            fields.push((name, value));
        }
    }

    fn start_tag(&mut self) -> Result<Event<'a>, Error> {
        if self.seen_root && self.stack.is_empty() {
            return Err(self.syntax("content after the root element"));
        }
        self.eat("<");
        let name = self.name()?;
        let mut attributes: Vec<Attribute<'a>> = Vec::new();
        loop {
            let spaced = self.skip_space();
            if self.eat("/>") {
                self.pending_end = true;
                break;
            }
            if self.eat(">") {
                break;
            }
            if !spaced {
                return Err(self.syntax("expected whitespace, '>' or '/>' in a start tag"));
            }
            let at = self.offset();
            let attr = self.name()?;
            if attr == "xmlns" {
                return Err(Error::Namespace { offset: at });
            }
            self.equals()?;
            let (raw, raw_at) = self.quoted()?;
            if raw.contains('<') {
                return Err(Error::Syntax {
                    offset: raw_at,
                    what: "'<' in an attribute value",
                });
            }
            let value = decode(raw, raw_at, Mode::Attribute)?;
            if attributes.iter().any(|a| a.name == attr) {
                return Err(Error::DuplicateAttribute { offset: at });
            }
            attributes.push(Attribute { name: attr, value });
            Limit::new("xml attributes per element", MAX_ATTRIBUTES)
                .check(len_u64(attributes.len()))?;
        }
        self.stack.push(name);
        Limit::new("xml depth", MAX_DEPTH).check(len_u64(self.stack.len()))?;
        self.seen_root = true;
        self.count_node()?;
        Ok(Event::Start { name, attributes })
    }

    fn end_tag(&mut self) -> Result<Event<'a>, Error> {
        let start = self.offset();
        self.eat("</");
        let name = self.name()?;
        self.skip_space();
        if !self.eat(">") {
            return Err(self.syntax("expected '>' in an end tag"));
        }
        match self.stack.pop() {
            Some(open) if open == name => Ok(Event::End { name }),
            _ => Err(Error::Unbalanced { offset: start }),
        }
    }

    fn text(&mut self) -> Result<Event<'a>, Error> {
        let start = self.offset();
        let raw = self.take_while(|c| c != '<');
        if let Some(at) = raw.find("]]>") {
            return Err(Error::Syntax {
                offset: start.saturating_add(at),
                what: "']]>' in text",
            });
        }
        let text = decode(raw, start, Mode::Text)?;
        self.count_node()?;
        Ok(Event::Text(text))
    }

    fn cdata(&mut self) -> Result<Event<'a>, Error> {
        if self.stack.is_empty() {
            return Err(self.syntax("CDATA outside the root element"));
        }
        self.eat("<![CDATA[");
        let raw = self
            .take_until("]]>")
            .ok_or_else(|| self.syntax("unterminated CDATA section"))?;
        self.count_node()?;
        Ok(Event::Text(normalize_newlines(raw)))
    }

    fn comment(&mut self) -> Result<(), Error> {
        self.eat("<!--");
        // `--` may only appear as part of the closing `-->`.
        self.take_until("--")
            .ok_or_else(|| self.syntax("unterminated comment"))?;
        if !self.eat(">") {
            return Err(self.syntax("'--' inside a comment"));
        }
        self.count_node()
    }

    fn processing_instruction(&mut self) -> Result<(), Error> {
        let start = self.offset();
        self.eat("<?");
        if self.name()? == "xml" && self.rest.starts_with(is_space) {
            return Err(Error::Declaration {
                offset: start,
                what: "XML declaration not at the start",
            });
        }
        // Like roxmltree, no whitespace is required after the target: PIs carry no meaning here.
        self.take_until("?>")
            .ok_or_else(|| self.syntax("unterminated processing instruction"))?;
        self.count_node()
    }

    // --- lexical helpers --------------------------------------------------

    /// A `Name`; prefixed names are rejected as namespaces.
    fn name(&mut self) -> Result<&'a str, Error> {
        let start = self.offset();
        let name = self.take_while(is_name_char);
        if !name.starts_with(is_name_start) {
            return Err(Error::Syntax {
                offset: start,
                what: "expected a name",
            });
        }
        Limit::new("xml name bytes", MAX_NAME_BYTES).check(len_u64(name.len()))?;
        if name.contains(':') {
            return Err(Error::Namespace { offset: start });
        }
        Ok(name)
    }

    /// `S? '=' S?`
    fn equals(&mut self) -> Result<(), Error> {
        self.skip_space();
        if !self.eat("=") {
            return Err(self.syntax("expected '='"));
        }
        self.skip_space();
        Ok(())
    }

    /// A quoted value, raw, with its offset.
    fn quoted(&mut self) -> Result<(&'a str, usize), Error> {
        let quote = match self.rest.chars().next() {
            Some(q @ ('"' | '\'')) => q,
            _ => return Err(self.syntax("expected a quoted value")),
        };
        self.eat(if quote == '"' { "\"" } else { "'" });
        let at = self.offset();
        let (value, after) = self
            .rest
            .split_once(quote)
            .ok_or_else(|| self.syntax("unterminated quoted value"))?;
        self.rest = after;
        Ok((value, at))
    }

    fn count_node(&mut self) -> Result<(), Error> {
        self.nodes = self.nodes.saturating_add(1);
        Limit::new("xml nodes", MAX_NODES).check(self.nodes)?;
        Ok(())
    }

    fn offset(&self) -> usize {
        self.input.len().saturating_sub(self.rest.len())
    }

    fn syntax(&self, what: &'static str) -> Error {
        Error::Syntax {
            offset: self.offset(),
            what,
        }
    }

    fn eat(&mut self, prefix: &str) -> bool {
        match self.rest.strip_prefix(prefix) {
            Some(after) => {
                self.rest = after;
                true
            }
            None => false,
        }
    }

    /// Skips `S`; returns whether anything was skipped.
    fn skip_space(&mut self) -> bool {
        let before = self.rest.len();
        self.rest = self.rest.trim_start_matches(is_space);
        self.rest.len() != before
    }

    fn take_while(&mut self, keep: impl Fn(char) -> bool) -> &'a str {
        let end = self.rest.find(|c| !keep(c)).unwrap_or(self.rest.len());
        let (head, tail) = self.rest.split_at_checked(end).unwrap_or((self.rest, ""));
        self.rest = tail;
        head
    }

    /// Everything before `end`, consuming `end` too.
    fn take_until(&mut self, end: &str) -> Option<&'a str> {
        let (head, tail) = self.rest.split_once(end)?;
        self.rest = tail;
        Some(head)
    }
}

fn len_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// `\r\n` and lone `\r` become `\n` (§2.11).
fn normalize_newlines(raw: &str) -> Cow<'_, str> {
    if raw.contains('\r') {
        Cow::Owned(raw.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(raw)
    }
}

/// Resolves references and normalises literal whitespace. `base` is the
/// offset of `raw` in the input, for error positions.
fn decode(raw: &str, base: usize, mode: Mode) -> Result<Cow<'_, str>, Error> {
    let special = |c: char| match mode {
        Mode::Text => matches!(c, '&' | '\r'),
        Mode::Attribute => matches!(c, '&' | '\r' | '\n' | '\t'),
    };
    if !raw.contains(special) {
        return Ok(Cow::Borrowed(raw));
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find(special) {
        let (plain, tail) = rest.split_at_checked(at).unwrap_or((rest, ""));
        out.push_str(plain);
        let here = base.saturating_add(raw.len().saturating_sub(tail.len()));
        // Every special character is one ASCII byte, so index 1 is a char boundary.
        let after = tail.get(1..).unwrap_or_default();
        rest = match tail.as_bytes().first() {
            Some(b'&') => {
                let (entity, after) = after.split_once(';').ok_or(Error::Syntax {
                    offset: here,
                    what: "unterminated entity reference",
                })?;
                out.push(resolve_entity(entity, here)?);
                after
            }
            Some(b'\r') => {
                out.push(if matches!(mode, Mode::Text) {
                    '\n'
                } else {
                    ' '
                });
                after.strip_prefix('\n').unwrap_or(after)
            }
            _ => {
                out.push(' ');
                after
            }
        };
    }
    out.push_str(rest);
    Ok(Cow::Owned(out))
}

fn resolve_entity(entity: &str, offset: usize) -> Result<char, Error> {
    match entity {
        "lt" => return Ok('<'),
        "gt" => return Ok('>'),
        "amp" => return Ok('&'),
        "apos" => return Ok('\''),
        "quot" => return Ok('"'),
        _ => {}
    }
    let Some(number) = entity.strip_prefix('#') else {
        return Err(if is_plain_name(entity) {
            Error::UnknownEntity { offset }
        } else {
            Error::Syntax {
                offset,
                what: "malformed entity reference",
            }
        });
    };
    let (digits, radix) = match number.strip_prefix('x') {
        Some(hex) => (hex, 16),
        None => (number, 10),
    };
    // `from_str_radix` alone would accept a leading '+'.
    let valid = !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix));
    valid
        .then(|| u32::from_str_radix(digits, radix).ok())
        .flatten()
        .and_then(char::from_u32)
        .filter(|&c| is_xml_char(c))
        .ok_or(Error::InvalidCharRef { offset })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(input: &str) -> Result<Vec<Event<'_>>, Error> {
        let mut reader = Reader::new(input.as_bytes(), &Limits::DEFAULT)?;
        let mut out = Vec::new();
        while let Some(event) = reader.next_event()? {
            out.push(event);
        }
        Ok(out)
    }

    fn text(s: &str) -> Event<'_> {
        Event::Text(Cow::Borrowed(s))
    }

    #[test]
    fn events_for_a_small_document() {
        let got = events("\u{FEFF}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<a x='1'>t<b/><!--c--><![CDATA[<&>]]></a>\n").unwrap();
        assert_eq!(
            got,
            vec![
                Event::Start {
                    name: "a",
                    attributes: vec![Attribute {
                        name: "x",
                        value: Cow::Borrowed("1")
                    }]
                },
                text("t"),
                Event::Start {
                    name: "b",
                    attributes: vec![]
                },
                Event::End { name: "b" },
                text("<&>"),
                Event::End { name: "a" },
            ]
        );
    }

    #[test]
    fn entities_and_char_refs() {
        let got = events("<a v='&lt;&#x41;&#66;\t'>&amp;&quot;&apos;&gt;&#x1F600;</a>").unwrap();
        let Event::Start { attributes, .. } = &got[0] else {
            panic!("start expected");
        };
        assert_eq!(attributes[0].value, "<AB ");
        assert_eq!(got[1], text("&\"'>😀"));
    }

    #[test]
    fn newline_normalisation() {
        let got = events("<a v='x\r\ny&#13;'>p\r\nq\rr</a>").unwrap();
        let Event::Start { attributes, .. } = &got[0] else {
            panic!("start expected");
        };
        assert_eq!(attributes[0].value, "x y\r");
        assert_eq!(got[1], text("p\nq\nr"));
    }

    #[test]
    fn reject_classes() {
        type IsExpected = fn(&Error) -> bool;
        let cases: &[(&str, IsExpected)] = &[
            ("<!DOCTYPE a><a/>", |e| matches!(e, Error::Doctype { .. })),
            ("<a>&nbsp;</a>", |e| {
                matches!(e, Error::UnknownEntity { .. })
            }),
            ("<a>&#0;</a>", |e| matches!(e, Error::InvalidCharRef { .. })),
            ("<a>&#xD800;</a>", |e| {
                matches!(e, Error::InvalidCharRef { .. })
            }),
            ("<a>&#+65;</a>", |e| {
                matches!(e, Error::InvalidCharRef { .. })
            }),
            ("<a>&#99999999999;</a>", |e| {
                matches!(e, Error::InvalidCharRef { .. })
            }),
            ("<a></b>", |e| matches!(e, Error::Unbalanced { .. })),
            ("<a>", |e| matches!(e, Error::Unbalanced { .. })),
            ("<a x='1' x='2'/>", |e| {
                matches!(e, Error::DuplicateAttribute { .. })
            }),
            ("<?xml version='1.0' encoding='ISO-8859-1'?><a/>", |e| {
                matches!(e, Error::Declaration { .. })
            }),
            ("<?xml version='2.0'?><a/>", |e| {
                matches!(e, Error::Declaration { .. })
            }),
            ("<p:a xmlns:p='u'/>", |e| {
                matches!(e, Error::Namespace { .. })
            }),
            ("<a xmlns='u'/>", |e| matches!(e, Error::Namespace { .. })),
            ("<a>\u{1}</a>", |e| matches!(e, Error::InvalidChar { .. })),
            ("<a/><b/>", |e| matches!(e, Error::Syntax { .. })),
            ("x<a/>", |e| matches!(e, Error::Syntax { .. })),
            ("<![CDATA[ ]]><a/>", |e| matches!(e, Error::Syntax { .. })),
            ("", |e| matches!(e, Error::Syntax { .. })),
            ("<a><!-- -- --></a>", |e| matches!(e, Error::Syntax { .. })),
            ("<a>]]></a>", |e| matches!(e, Error::Syntax { .. })),
            ("<a/><?xml version='1.0'?>", |e| {
                matches!(e, Error::Declaration { .. })
            }),
            ("<?xml encoding='utf-8'?><a/>", |e| {
                matches!(e, Error::Declaration { .. })
            }),
            ("<a b='1'c='2'/>", |e| matches!(e, Error::Syntax { .. })),
            ("<a b='<'/>", |e| matches!(e, Error::Syntax { .. })),
        ];
        for (input, expected) in cases {
            let err = events(input).expect_err(input);
            assert!(expected(&err), "{input:?}: {err:?}");
        }
        assert!(matches!(
            Reader::new(b"<a>\xFF</a>", &Limits::DEFAULT),
            Err(Error::Utf8 { offset: 3 })
        ));
    }

    #[test]
    fn limits() {
        let deep = format!("{}{}", "<a>".repeat(65), "</a>".repeat(65));
        assert!(matches!(events(&deep), Err(Error::Limit(_))));
        let ok_deep = format!("{}{}", "<a>".repeat(64), "</a>".repeat(64));
        assert!(events(&ok_deep).is_ok());

        let attrs: String = (0..65).map(|i| format!(" a{i}='x'")).collect();
        assert!(matches!(
            events(&format!("<e{attrs}/>")),
            Err(Error::Limit(_))
        ));

        let long = "n".repeat(257);
        assert!(matches!(
            events(&format!("<{long}/>")),
            Err(Error::Limit(_))
        ));
        assert!(events(&format!("<{}/>", "n".repeat(256))).is_ok());

        let small = Limits::DEFAULT.lowered(|l| l.max_xml = 4);
        assert!(matches!(
            Reader::new(b"<a/> ", &small),
            Err(Error::Limit(_))
        ));
        assert!(Reader::new(b"<a/>", &small).is_ok());
    }

    #[test]
    fn node_limit() {
        let many = format!("<r>{}</r>", "<a/>".repeat(1_000_000));
        assert!(matches!(events(&many), Err(Error::Limit(_))));
    }
}
