//! XML documents as the native parsers see them through Java's namespace-aware SAX parser: the
//! events of [`crate::xml`], which reads and fails as the JDK's Xerces does, handed to a
//! [`SaxHandler`] without the `xmlns` attributes and with character data in the pieces Xerces
//! reports it in.

use std::borrow::Cow;

use crate::errors::Error;
use crate::formats::Failure;
use crate::xml::{self, Event, Flow, TextKind, XmlError, ENTITY_EXPANSION_LIMIT};

pub(crate) use crate::xml::{Attribute, XMLNS};

/// The value of the attribute named `local_name` in `namespace`, as SAX's `getValue(uri, name)`.
pub(crate) fn attribute<'a>(
    attributes: &'a [Attribute],
    namespace: &str,
    local_name: &str,
) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.namespace == namespace && attribute.local_name == local_name)
        .map(|attribute| attribute.value.as_str())
}

/// The value of the attribute written `qualified_name`, as SAX's `getValue(qName)`.
pub(crate) fn attribute_named<'a>(
    attributes: &'a [Attribute],
    qualified_name: &str,
) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.qualified_name == qualified_name)
        .map(|attribute| attribute.value.as_str())
}

/// What receives the events of [`parse_sax`]: a port of a Java SAX `ContentHandler`. An element
/// without a namespace is in the namespace `""`.
pub(crate) trait SaxHandler {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute],
    ) -> Result<(), Failure>;
    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure>;
    fn characters(&mut self, text: &str) -> Result<(), Failure>;

    /// The start of an element with what SAX also reports of it: its qualified name, and with
    /// the `namespace-prefixes` feature, its namespace declarations. A handler that needs no
    /// more than [`SaxHandler::start_element`] leaves this to call it.
    fn start_tag(&mut self, tag: &StartTag<'_>) -> Result<(), Failure> {
        self.start_element(tag.namespace, tag.local_name, tag.attributes)
    }

    /// A document type declaration, which Tika's parsers skip and POI's refuse.
    fn document_type(&mut self) -> Result<(), Failure> {
        Ok(())
    }
}

/// A start tag as [`SaxHandler::start_tag`] receives it.
pub(crate) struct StartTag<'t> {
    pub(crate) namespace: &'t str,
    pub(crate) local_name: &'t str,
    pub(crate) qualified_name: &'t str,
    pub(crate) attributes: &'t [Attribute],
    /// The `xmlns` attributes, in the XMLNS namespace.
    pub(crate) declarations: &'t [Attribute],
}

/// Reads the XML document `input` as the JDK's namespace-aware SAX parser does, handing `handler`
/// its elements, without their `xmlns` attributes, and its character data in the pieces that
/// parser hands it over in: a run of character data split as its scanner splits it, a CDATA
/// section, or what a reference stands for. `what` names the document in errors. Fails where that
/// parser fails, after the events it would have reported before.
pub(crate) fn parse_sax(
    input: &[u8],
    what: &str,
    handler: &mut impl SaxHandler,
) -> Result<(), Failure> {
    let mut failure = None;
    let result = xml::parse(input, &mut |event| {
        let handled = match event {
            Event::DocumentType => handler.document_type(),
            Event::Start(name, attributes) => {
                let (declarations, attributes): (Cow<'_, [Attribute]>, Cow<'_, [Attribute]>) =
                    if attributes.iter().any(|a| a.namespace == XMLNS) {
                        let (declarations, attributes) = attributes
                            .iter()
                            .cloned()
                            .partition(|a| a.namespace == XMLNS);
                        (Cow::Owned(declarations), Cow::Owned(attributes))
                    } else {
                        (Cow::Borrowed(&[]), Cow::Borrowed(attributes))
                    };
                handler.start_tag(&StartTag {
                    namespace: name.namespace(),
                    local_name: name.local_name(),
                    qualified_name: name.qualified_name(),
                    attributes: &attributes,
                    declarations: &declarations,
                })
            }
            Event::End(name) => handler.end_element(name.namespace(), name.local_name()),
            Event::Text(text, TextKind::Data) => {
                for_each_sax_piece(text, |piece| handler.characters(piece))
            }
            Event::Text("", TextKind::CData)
            | Event::Comment(_)
            | Event::ProcessingInstruction(..) => Ok(()),
            Event::Text(text, TextKind::CData | TextKind::Reference) => handler.characters(text),
        };
        match handled {
            Ok(()) => Flow::Continue,
            Err(stopped) => {
                failure = Some(stopped);
                Flow::Stop
            }
        }
    });
    if let Some(failure) = failure {
        return Err(failure);
    }
    result.map_err(|error| match error {
        XmlError::CharConversion => Failure::Malformed(format!(
            "{what} is malformed: a byte sequence its encoding does not allow"
        )),
        XmlError::Malformed(reason) => Failure::Malformed(format!("{what} is malformed: {reason}")),
        XmlError::EntityExpansionLimit => Failure::Extraction(Error::LimitExceeded(format!(
            "{what} expands more than {ENTITY_EXPANSION_LIMIT} entities"
        ))),
    })
}

/// Whether the JDK's scanner reads on over `character` in a run of character data: anything but
/// a line break, `]` and a character outside the Basic Multilingual Plane.
fn is_content(character: char) -> bool {
    !matches!(character, '\n' | ']') && character < '\u{10000}'
}

/// The end of the run the JDK's scanner reads from `start`: line breaks, then content.
fn scanned_run(text: &str, start: usize) -> usize {
    let rest = &text[start..];
    let content = rest.trim_start_matches('\n');
    let end = content
        .char_indices()
        .find(|(_, character)| !is_content(*character))
        .map_or(content.len(), |(index, _)| index);
    start + (rest.len() - content.len()) + end
}

fn after_brackets(text: &str, start: usize) -> usize {
    let rest = &text[start..];
    start + rest.len() - rest.trim_start_matches(']').len()
}

/// Hands `write` the character data `text`, which lies between markup and references and whose
/// line ends are normalized, in the pieces the JDK's SAX parser reports it in: a run of line
/// breaks and content, any `]`s, a second such run and its `]`s; or a run up to and with a
/// character outside the Basic Multilingual Plane. A handler that looks at each piece on its own,
/// such as `AnchoredBlockHandler`, sees what it sees in Java.
fn for_each_sax_piece<'t, E>(
    text: &'t str,
    mut write: impl FnMut(&'t str) -> Result<(), E>,
) -> Result<(), E> {
    let mut position = 0;
    while position < text.len() {
        let start = position;
        position = scanned_run(text, position);
        if let Some(character) = text[position..].chars().next() {
            if character >= '\u{10000}' {
                position += character.len_utf8();
            } else {
                position = after_brackets(text, position);
                position = after_brackets(text, scanned_run(text, position));
            }
        }
        write(&text[start..position])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(text: &str) -> Vec<&str> {
        let mut pieces = Vec::new();
        for_each_sax_piece::<()>(text, |piece| {
            pieces.push(piece);
            Ok(())
        })
        .unwrap();
        pieces
    }

    #[test]
    fn splits_character_data_where_the_jdk_does() {
        assert_eq!(pieces("plain text"), ["plain text"]);
        assert_eq!(pieces("a\nb\nc\nd"), ["a\nb", "\nc\nd"]);
        assert_eq!(pieces(" \n \n x"), [" \n ", "\n x"]);
        assert_eq!(pieces("\n\n  a\nb"), ["\n\n  a\nb"]);
        assert_eq!(pieces("a]]b]c"), ["a]]b]", "c"]);
        assert_eq!(pieces("ab😀cd😀😀"), ["ab😀", "cd😀", "😀"]);
        assert!(pieces("").is_empty());
    }

    #[derive(Default)]
    struct Events(Vec<String>);

    impl SaxHandler for Events {
        fn start_element(
            &mut self,
            namespace: &str,
            local_name: &str,
            attributes: &[Attribute],
        ) -> Result<(), Failure> {
            let attributes: Vec<String> = attributes
                .iter()
                .map(|a| format!("{{{}}}{}={}", a.namespace, a.local_name, a.value))
                .collect();
            self.0.push(format!(
                "<{{{namespace}}}{local_name} {}>",
                attributes.join(" ")
            ));
            Ok(())
        }

        fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure> {
            self.0.push(format!("</{{{namespace}}}{local_name}>"));
            Ok(())
        }

        fn characters(&mut self, text: &str) -> Result<(), Failure> {
            self.0.push(text.to_owned());
            Ok(())
        }
    }

    fn events(document: &[u8]) -> Result<Vec<String>, Failure> {
        let mut events = Events::default();
        parse_sax(document, "x", &mut events)?;
        Ok(events.0)
    }

    #[test]
    fn reports_namespaces_attributes_and_references_as_sax_does() {
        let document = b"<?xml version=\"1.0\"?>\n<a xmlns=\"urn:a\" xmlns:b=\"urn:b\" \
                        b:c=\"x\r\n\ty&#10;&amp;\" d='e'><b:f/>1 &lt; 2<![CDATA[<]]></a>\n";
        assert_eq!(
            events(document).unwrap(),
            [
                "<{urn:a}a {urn:b}c=x  y\n& {}d=e>",
                "<{urn:b}f >",
                "</{urn:b}f>",
                "1 ",
                "<",
                " 2",
                "<",
                "</{urn:a}a>",
            ]
        );
    }

    #[test]
    fn fails_where_the_jdk_parser_fails() {
        for document in [
            "",
            "<a>",
            "<a></b>",
            "<a/><b/>",
            "x<a/>",
            "<a/>x",
            "<a>&nbsp;</a>",
            "<a>&#1;</a>",
            "<a>\u{1}</a>",
            "<a b='<'/>",
            "<a b='1' b='2'/>",
            "<x:a/>",
            "<a x:b='1'/>",
            "<a>]]></a>",
        ] {
            assert!(events(document.as_bytes()).is_err(), "{document:?}");
        }
    }

    #[test]
    fn decodes_in_the_declared_charset() {
        let cyrillic = b"<?xml version=\"1.0\" encoding='windows-1251'?><a>\xcc\xe0\xff\xea</a>";
        assert_eq!(
            events(cyrillic).unwrap(),
            ["<{}a >", "\u{41c}\u{430}\u{44f}\u{43a}", "</{}a>"]
        );
        assert_eq!(
            events(b"\xff\xfe<\0a\0/\0>\0").unwrap(),
            ["<{}a >", "</{}a>"]
        );
        assert!(events(b"<a>\xe6</a>").is_err());
        assert!(events(b"<?xml version=\"1.0\" encoding=\"no-such\"?><a/>").is_err());
    }
}
