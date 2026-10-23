//! XML documents as the native parsers see them through Java's namespace-aware SAX parser: the
//! events of [`crate::xml`], which reads and fails as the JDK's Xerces does, handed to a
//! [`SaxHandler`] without the `xmlns` attributes and with character data in the pieces Xerces
//! reports it in.

use crate::errors::Error;
use crate::formats::Failure;
use crate::input::{Input, Pages, Paging};
use crate::xml::{
    self, AttributeOrder, DocumentTypes, Event, Flow, TextKind, XmlError, ENTITY_EXPANSION_LIMIT,
};

pub use crate::xml::{Attribute, XMLNS};

/// The value of the attribute named `local_name` in `namespace`, as SAX's `getValue(uri, name)`.
pub fn attribute<'a>(
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
pub fn attribute_named<'a>(attributes: &'a [Attribute], qualified_name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.qualified_name == qualified_name)
        .map(|attribute| attribute.value.as_str())
}

/// What receives the events of [`parse_sax`]: a port of a Java SAX `ContentHandler`. An element
/// without a namespace is in the namespace `""`.
pub trait SaxHandler {
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

    /// The end of an element with its qualified name, which SAX also reports. A handler that
    /// needs no more than [`SaxHandler::end_element`] leaves this to call it.
    fn end_tag(
        &mut self,
        namespace: &str,
        local_name: &str,
        _qualified_name: &str,
    ) -> Result<(), Failure> {
        self.end_element(namespace, local_name)
    }

    /// A document type declaration, which Tika's parsers skip and POI's refuse.
    fn document_type(&mut self) -> Result<(), Failure> {
        Ok(())
    }
}

/// A start tag as [`SaxHandler::start_tag`] receives it.
pub struct StartTag<'t> {
    pub namespace: &'t str,
    pub local_name: &'t str,
    pub qualified_name: &'t str,
    pub attributes: &'t [Attribute],
    /// The `xmlns` attributes, in the XMLNS namespace.
    pub declarations: &'t [Attribute],
}

/// Reads the XML document `input` as the JDK's namespace-aware SAX parser does, handing `handler`
/// its elements, without their `xmlns` attributes, and its character data in the pieces that
/// parser hands it over in: a run of character data split as its scanner splits it, a CDATA
/// section, or what a reference stands for. `what` names the document in errors. Fails where that
/// parser fails, after the events it would have reported before.
pub fn parse_sax(input: &[u8], what: &str, handler: &mut impl SaxHandler) -> Result<(), Failure> {
    parse_sax_paged(
        input,
        Pages::IN_MEMORY,
        DocumentTypes::Processed,
        what,
        handler,
    )
}

/// [`parse_sax`] as the JDK's StAX reader reads a document without DTD support, as Tika's
/// `XMLReaderUtils.getXMLInputFactory` makes it: a document type declaration is read past and
/// nothing of it applies, so an entity reference other than the predefined ones fails.
pub fn parse_without_dtd(
    input: &[u8],
    what: &str,
    handler: &mut impl SaxHandler,
) -> Result<(), Failure> {
    parse_sax_paged(
        input,
        Pages::IN_MEMORY,
        DocumentTypes::Skipped,
        what,
        handler,
    )
}

/// How many events a parse reports between releases of the pages of its input.
const RELEASE_EVERY_EVENTS: usize = 1 << 16;

/// [`parse_sax`] of a part read from an archive, which may be spooled to a file of its own and
/// mapped: its pages are released as the parse goes on.
pub fn parse_part(
    input: &Input<'_>,
    what: &str,
    handler: &mut impl SaxHandler,
) -> Result<(), Failure> {
    parse_paged(input, input.pages(), what, handler)
}

/// [`parse_sax`] of `input`, whose `pages` are released as the parse goes on: a mapped file
/// that a parser of bounded memory reads.
pub fn parse_paged(
    input: &[u8],
    pages: Pages<'_>,
    what: &str,
    handler: &mut impl SaxHandler,
) -> Result<(), Failure> {
    parse_sax_paged(input, pages, DocumentTypes::Processed, what, handler)
}

fn parse_sax_paged(
    input: &[u8],
    pages: Pages<'_>,
    document_types: DocumentTypes,
    what: &str,
    handler: &mut impl SaxHandler,
) -> Result<(), Failure> {
    let mut failure = None;
    let mut events = 0usize;
    let paging = Paging::new(pages);
    let result = xml::parse_paged(
        input,
        &paging,
        document_types,
        AttributeOrder::DeclarationsFirst,
        &mut |event| {
            events += 1;
            if events.is_multiple_of(RELEASE_EVERY_EVENTS) {
                paging.release();
            }
            let handled = match event {
                Event::DocumentType => handler.document_type(),
                Event::Start(name, attributes) => {
                    let (declarations, attributes) =
                        attributes.split_at(attributes.partition_point(|a| a.namespace == XMLNS));
                    handler.start_tag(&StartTag {
                        namespace: name.namespace(),
                        local_name: name.local_name(),
                        qualified_name: name.qualified_name(),
                        attributes,
                        declarations,
                    })
                }
                Event::End(name) => {
                    handler.end_tag(name.namespace(), name.local_name(), name.qualified_name())
                }
                Event::Text(text, TextKind::Data) => {
                    for_each_sax_piece(text, |piece| handler.characters(piece))
                }
                Event::Text("", TextKind::CData)
                | Event::Comment(_)
                | Event::ProcessingInstruction(..) => Ok(()),
                Event::Text(text, TextKind::CData | TextKind::Reference) => {
                    handler.characters(text)
                }
            };
            match handled {
                Ok(()) => Flow::Continue,
                Err(stopped) => {
                    failure = Some(stopped);
                    Flow::Stop
                }
            }
        },
    );
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

/// Whether the XML document `input` cannot hold an element or attribute named `name`, an ASCII
/// name, as a search of its bytes tells: it is read as UTF-8, and neither the name nor a
/// document type declaration, whose entities could spell the name, stands in its bytes. A
/// document read otherwise may hold the name whatever its bytes are.
pub fn lacks_name(input: &Input<'_>, name: &str) -> bool {
    let paging = Paging::new(input.pages());
    xml::is_read_as_utf8(input)
        && paging.find(input, name.as_bytes()).is_none()
        && paging.find(input, b"<!DOCTYPE").is_none()
}

/// Whether the XML document `input` declares a document type before its root element, under
/// which Xerces may report whitespace as ignorable, which Tika's handlers drop or keep as their
/// own `ignorableWhitespace` decides.
pub fn has_document_type(input: &[u8]) -> bool {
    /// Finds a document type declaration, and stops at it or at the root element.
    struct Finder(bool);

    impl SaxHandler for Finder {
        fn start_element(&mut self, _: &str, _: &str, _: &[Attribute]) -> Result<(), Failure> {
            Err(Failure::Malformed(
                "the root element comes first".to_owned(),
            ))
        }

        fn end_element(&mut self, _: &str, _: &str) -> Result<(), Failure> {
            Ok(())
        }

        fn characters(&mut self, _: &str) -> Result<(), Failure> {
            Ok(())
        }

        fn document_type(&mut self) -> Result<(), Failure> {
            self.0 = true;
            Err(Failure::Malformed("found".to_owned()))
        }
    }

    let mut finder = Finder(false);
    let _stopped = parse_sax(input, "the document", &mut finder);
    finder.0
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
        position = xml::sax_piece_end(text, position);
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

    #[test]
    fn tells_a_name_absent_only_from_bytes_read_as_utf8() {
        let lacks = |bytes: &[u8]| lacks_name(&Input::Borrowed(bytes), "lock");
        assert!(lacks(b"<a><b/></a>"));
        assert!(lacks(
            b"\xef\xbb\xbf<?xml version=\"1.0\" encoding=\"utf-8\"?><a/>"
        ));
        assert!(lacks(b"<?xml version='1.0' encoding = 'UTF-8' ?><a/>"));
        assert!(!lacks(b"<a><lock/></a>"));
        assert!(!lacks(b"<a><x:b lock=\"1\" xmlns:x=\"urn:x\"/></a>"));
        assert!(!lacks(
            b"<?xml version=\"1.0\" encoding=\"windows-1252\"?><a/>"
        ));
        assert!(!lacks(b"<?xml version=\"1.0\" encoding=\"UTF-8\"<a/>"));
        assert!(!lacks(b"\xff\xfe<\0a\0/\0>\0"));
        assert!(!lacks(
            b"<!DOCTYPE a [<!ENTITY e \"&#60;lo&#99;k/&#62;\">]><a>&e;</a>"
        ));
    }
}
