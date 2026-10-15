//! Reading XML documents as Java's SAX parser reads them: decoded in the charset their byte order
//! mark or XML declaration names, by the JDK's own decoders, and failing on what it rejects;
//! [`parse_sax`] reports them as the JDK's namespace-aware SAX parser does.

use std::borrow::Cow;

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::attributes::Attribute as QuickAttribute;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::reader::NsReader;
use quick_xml::XmlVersion;

use crate::formats::Failure;
use crate::text::charset::Charset;

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];

/// The text of an XML document: what follows its byte order mark, decoded in the charset the
/// mark or the declaration names, else UTF-8. `what` names the document in errors.
pub(crate) fn decode(input: &[u8], what: &str) -> Result<String, Failure> {
    let malformed = |reason: String| Failure::Malformed(format!("{what} {reason}"));
    if let Some(rest) = input.strip_prefix(UTF8_BOM) {
        return utf8(rest).map_err(malformed);
    }
    for (bom, name) in [(UTF16_BE_BOM, "UTF-16BE"), (UTF16_LE_BOM, "UTF-16LE")] {
        if let Some(rest) = input.strip_prefix(bom) {
            return decode_in(Charset::named(name), rest).map_err(malformed);
        }
    }
    match declared_encoding(input) {
        None => utf8(input).map_err(malformed),
        Some(label) => {
            let charset = Charset::for_name(&label).ok_or_else(|| {
                malformed(format!(
                    "declares the encoding {label:?}, which Java does not know"
                ))
            })?;
            decode_in(charset, input).map_err(malformed)
        }
    }
}

fn utf8(input: &[u8]) -> Result<String, String> {
    String::from_utf8(input.to_vec()).map_err(|e| format!("is not valid UTF-8: {}", e.utf8_error()))
}

fn decode_in(charset: Charset, input: &[u8]) -> Result<String, String> {
    if charset.name() == "UTF-8" {
        return utf8(input);
    }
    charset
        .decode(input)
        .ok_or_else(|| format!("is in {}, which extractous cannot decode", charset.name()))
}

/// The `encoding` of an XML declaration at the very start of `input`, which an ASCII-compatible
/// charset spells in ASCII.
fn declared_encoding(input: &[u8]) -> Option<String> {
    let declaration = input.strip_prefix(b"<?xml")?;
    let end = declaration.windows(2).position(|pair| pair == b"?>")?;
    let declaration = std::str::from_utf8(&declaration[..end]).ok()?;
    let after = &declaration[declaration.find("encoding")? + "encoding".len()..];
    let after = after.trim_start().strip_prefix('=')?.trim_start();
    let quote = after
        .chars()
        .next()
        .filter(|quote| matches!(quote, '"' | '\''))?;
    let value = &after[1..];
    Some(value[..value.find(quote)?].to_owned())
}

/// The text an entity or character reference stands for: a character reference or one of XML's
/// five predefined entities. Any other entity was never declared, which Java's parser rejects.
pub(crate) fn resolve_reference(reference: &BytesRef, what: &str) -> Result<String, Failure> {
    let malformed = |reason: String| Failure::Malformed(format!("{what} {reason}"));
    if let Some(character) = reference
        .resolve_char_ref()
        .map_err(|e| malformed(format!("has a bad character reference: {e}")))?
    {
        return Ok(character.to_string());
    }
    let name = reference.xml10_content();
    resolve_predefined_entity(&name)
        .map(str::to_owned)
        .ok_or_else(|| malformed(format!("uses the entity &{name}; without declaring it")))
}

/// An attribute as Java's namespace-aware SAX parser reports it: in the namespace its prefix is
/// bound to, if it has one, with its value normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attribute<'a> {
    /// The namespace, or the empty string for an attribute without a prefix.
    pub(crate) namespace: &'a str,
    pub(crate) local_name: &'a str,
    /// The name as written, with its prefix.
    pub(crate) qualified_name: &'a str,
    pub(crate) value: Cow<'a, str>,
}

/// The value of the attribute named `local_name` in `namespace`, as SAX's `getValue(uri, name)`.
pub(crate) fn attribute<'a>(
    attributes: &'a [Attribute<'_>],
    namespace: &str,
    local_name: &str,
) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.namespace == namespace && attribute.local_name == local_name)
        .map(|attribute| attribute.value.as_ref())
}

/// What receives the events of [`parse_sax`]: a port of a Java SAX `ContentHandler`. An element
/// without a namespace is in the namespace `""`.
pub(crate) trait SaxHandler {
    fn start_element(
        &mut self,
        namespace: &str,
        local_name: &str,
        attributes: &[Attribute<'_>],
    ) -> Result<(), Failure>;
    fn end_element(&mut self, namespace: &str, local_name: &str) -> Result<(), Failure>;
    fn characters(&mut self, text: &str) -> Result<(), Failure>;
}

/// Reads the XML document `text` as the JDK's namespace-aware SAX parser does, handing `handler`
/// its elements, without their `xmlns` attributes, and its character data in the pieces that
/// parser hands it over in. `what` names the document in errors. Fails where that parser fails,
/// after the events it would have reported before. A DTD is skipped, as Tika's parsers skip it,
/// so an entity it declares counts as undeclared.
pub(crate) fn parse_sax(
    text: &str,
    what: &str,
    handler: &mut impl SaxHandler,
) -> Result<(), Failure> {
    let malformed = |reason: String| Failure::Malformed(format!("{what} is malformed: {reason}"));
    let mut reader = NsReader::from_str(text);
    let mut open: Vec<String> = Vec::new();
    let mut had_root = false;
    loop {
        match reader.read_event().map_err(|e| malformed(e.to_string()))? {
            Event::Start(start) | Event::Empty(start) if open.is_empty() && had_root => {
                return Err(malformed(format!(
                    "<{}> follows the root element",
                    start.name().into_inner()
                )));
            }
            Event::Start(start) => {
                start_element(&reader, &start, &malformed, handler)?;
                open.push(start.name().into_inner().to_owned());
                had_root = true;
            }
            Event::Empty(start) => {
                let (namespace, local_name) = start_element(&reader, &start, &malformed, handler)?;
                handler.end_element(&namespace, &local_name)?;
                had_root = true;
            }
            Event::End(end) => {
                let (namespace, local_name) = element_name(&reader, end.name(), &malformed)?;
                handler.end_element(&namespace, local_name)?;
                open.pop();
            }
            Event::Text(content) => {
                let content = content.xml10_content();
                if open.is_empty() {
                    if !content.chars().all(is_xml_whitespace) {
                        return Err(malformed("has text outside its root element".to_owned()));
                    }
                    continue;
                }
                if content.contains("]]>") {
                    return Err(malformed("has ]]> in its character data".to_owned()));
                }
                check_characters(&content, &malformed)?;
                for_each_sax_piece(&content, |piece| handler.characters(piece))?;
            }
            Event::CData(data) => {
                if open.is_empty() {
                    return Err(malformed("has CDATA outside its root element".to_owned()));
                }
                let content = data.xml10_content();
                check_characters(&content, &malformed)?;
                if !content.is_empty() {
                    handler.characters(&content)?;
                }
            }
            Event::GeneralRef(reference) => {
                if open.is_empty() {
                    return Err(malformed(
                        "has a reference outside its root element".to_owned(),
                    ));
                }
                let resolved = resolve_reference(&reference, what)?;
                check_characters(&resolved, &malformed)?;
                handler.characters(&resolved)?;
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
    }
    if let Some(element) = open.last() {
        return Err(malformed(format!("<{element}> is never closed")));
    }
    if !had_root {
        return Err(malformed("has no root element".to_owned()));
    }
    Ok(())
}

fn start_element(
    reader: &NsReader<&[u8]>,
    start: &BytesStart<'_>,
    malformed: &impl Fn(String) -> Failure,
    handler: &mut impl SaxHandler,
) -> Result<(String, String), Failure> {
    let (namespace, local_name) = element_name(reader, start.name(), malformed)?;
    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|e| malformed(e.to_string()))?;
        if let Some(attribute) = sax_attribute(reader, attribute, malformed)? {
            attributes.push(attribute);
        }
    }
    handler.start_element(&namespace, local_name, &attributes)?;
    Ok((namespace, local_name.to_owned()))
}

fn sax_attribute<'a>(
    reader: &'a NsReader<&[u8]>,
    attribute: QuickAttribute<'a>,
    malformed: &impl Fn(String) -> Failure,
) -> Result<Option<Attribute<'a>>, Failure> {
    let key = attribute.key;
    if key.as_namespace_binding().is_some() {
        return Ok(None);
    }
    let qualified_name = key.into_inner();
    if attribute.value.contains('<') {
        return Err(malformed(format!(
            "has a '<' in the value of its attribute {qualified_name}"
        )));
    }
    let value = attribute
        .normalized_value(XmlVersion::Implicit1_0)
        .map_err(|e| {
            malformed(format!(
                "has a bad value of its attribute {qualified_name}: {e}"
            ))
        })?;
    check_characters(&value, malformed)?;
    let (resolved, local_name) = reader.resolver().resolve_attribute(key);
    let namespace = match resolved {
        ResolveResult::Unbound => "",
        ResolveResult::Bound(namespace) => namespace.into_inner(),
        ResolveResult::Unknown(prefix) => {
            return Err(malformed(format!(
                "binds no namespace to the prefix {} of its attribute {qualified_name}",
                prefix
            )))
        }
    };
    Ok(Some(Attribute {
        namespace,
        local_name: local_name.into_inner(),
        qualified_name,
        value,
    }))
}

fn element_name<'n>(
    reader: &NsReader<&[u8]>,
    name: QName<'n>,
    malformed: &impl Fn(String) -> Failure,
) -> Result<(String, &'n str), Failure> {
    let (resolved, local_name) = reader.resolver().resolve_element(name);
    let namespace = match resolved {
        ResolveResult::Unbound => String::new(),
        ResolveResult::Bound(namespace) => namespace.into_inner().to_owned(),
        ResolveResult::Unknown(prefix) => {
            return Err(malformed(format!(
                "binds no namespace to the prefix {} of its element {}",
                prefix,
                name.into_inner()
            )))
        }
    };
    Ok((namespace, local_name.into_inner()))
}

fn is_xml_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r')
}

/// Whether XML 1.0 allows `character` in a document.
fn is_xml_char(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\r' | ' '..='\u{d7ff}' | '\u{e000}'..='\u{fffd}')
        || character >= '\u{10000}'
}

fn check_characters(text: &str, malformed: &impl Fn(String) -> Failure) -> Result<(), Failure> {
    match text.chars().find(|character| !is_xml_char(*character)) {
        Some(character) => Err(malformed(format!(
            "holds the character U+{:04X}, which XML does not allow",
            u32::from(character)
        ))),
        None => Ok(()),
    }
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
            attributes: &[Attribute<'_>],
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

    fn events(text: &str) -> Result<Vec<String>, Failure> {
        let mut events = Events::default();
        parse_sax(text, "x", &mut events)?;
        Ok(events.0)
    }

    #[test]
    fn reports_namespaces_attributes_and_references_as_sax_does() {
        let document = "<?xml version=\"1.0\"?>\n<a xmlns=\"urn:a\" xmlns:b=\"urn:b\" \
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
            assert!(events(document).is_err(), "{document:?}");
        }
    }

    #[test]
    fn decodes_in_the_declared_charset() {
        let cyrillic = b"<?xml version=\"1.0\" encoding='windows-1251'?><a>\xcc\xe0\xff\xea</a>";
        assert_eq!(
            decode(cyrillic, "x").unwrap(),
            "<?xml version=\"1.0\" encoding='windows-1251'?><a>Маяк</a>"
        );
        let latin1 = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\x80";
        assert!(decode(latin1, "x").unwrap().ends_with('\u{80}'));
    }

    #[test]
    fn decodes_byte_order_marks_and_defaults_to_strict_utf8() {
        assert_eq!(decode(b"\xef\xbb\xbf<a/>", "x").unwrap(), "<a/>");
        assert_eq!(decode(b"\xff\xfe<\0a\0/\0>\0", "x").unwrap(), "<a/>");
        assert_eq!(decode("<a>æ</a>".as_bytes(), "x").unwrap(), "<a>æ</a>");
        assert!(decode(b"<a>\xe6</a>", "x").is_err());
        assert!(decode(b"<?xml version=\"1.0\" encoding=\"no-such\"?><a/>", "x").is_err());
    }
}
