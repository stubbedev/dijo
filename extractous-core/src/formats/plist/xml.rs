//! XML property lists as dd-plist 1.30's `XMLPropertyListParser` reads them: the document parsed
//! by the JDK's namespace-unaware DOM builder, coalescing CDATA into text and leaving comments
//! out, then each element read as the object its name stands for, its text the first run of
//! character data among its children.

use std::rc::Rc;

use super::binary::{Dict, Key, Object, Value};
use super::Problem;
use crate::java;
use crate::java::time::{CalendarFields, TimeZone};
use crate::xml::{self, Event, Flow, XmlError};

/// `XMLPropertyListParser.MAX_NESTING_DEPTH`.
const MAX_NESTING_DEPTH: usize = 512;
/// Milliseconds from the Java epoch to dd-plist's, 2001-01-01.
const EPOCH: i64 = 978_307_200_000;

fn failed<T>(reason: impl Into<String>) -> Result<T, Problem> {
    Err(Problem::Failed(reason.into()))
}

/// A node of the DOM tree that matters to dd-plist: an element, a text node, or anything else
/// that ends a run of text, a processing instruction.
#[derive(Debug)]
enum Node {
    Element(Element),
    Text(String),
    Other,
}

#[derive(Debug)]
struct Element {
    name: String,
    children: Vec<Node>,
}

impl Element {
    fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    /// `getNodeTextContents`: the text of the first text node among the children, which the DOM
    /// builder has run together up to the next element or processing instruction.
    fn text(&self) -> &str {
        self.children
            .iter()
            .find_map(|node| match node {
                Node::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or("")
    }
}

/// `XMLPropertyListParser.parse(InputStream)`: the root object of an XML property list.
pub(super) fn parse(input: &[u8]) -> Result<Value, Problem> {
    check_supported(input)?;
    let (document_type, root) = build(input)?;
    let root = match document_type {
        Some(name) if name != "plist" => {
            return failed("The given XML document is not a property list.")
        }
        None if root.name != "plist" => {
            return failed("The given XML document is not a property list.")
        }
        _ => root,
    };
    if root.name != "plist" {
        return object(&root, 1);
    }
    let mut objects = root.elements();
    match (objects.next(), objects.next()) {
        (Some(only), None) => object(only, 1),
        (None, _) => failed("The given XML property list has no root element!"),
        (Some(_), Some(_)) => failed("The given XML property list has more than one root element!"),
    }
}

/// Refuses what the port does not read as the JDK's DOM builder does: a list in an encoding
/// that does not write ASCII as ASCII, which the checks below cannot scan, and entities beyond
/// the predefined ones, which the builder keeps as entity reference nodes.
fn check_supported(input: &[u8]) -> Result<(), Problem> {
    let ascii_compatible = !input.starts_with(&[0xFE, 0xFF])
        && !input.starts_with(&[0xFF, 0xFE])
        && !input.starts_with(&[0x4C, 0x6F, 0xA7, 0x94])
        && !input.contains(&0);
    if !ascii_compatible {
        return Err(Problem::Unread(
            "an XML property list in an encoding that does not write ASCII as ASCII",
        ));
    }
    if contains(input, b"<!ENTITY") {
        return Err(Problem::Unread(
            "an XML property list that declares entities",
        ));
    }
    let predefined: [&[u8]; 6] = [b"#", b"lt;", b"gt;", b"amp;", b"apos;", b"quot;"];
    let mut rest = input;
    while let Some(at) = rest.iter().position(|&byte| byte == b'&') {
        rest = &rest[at + 1..];
        if !predefined.iter().any(|name| rest.starts_with(name)) {
            return Err(Problem::Unread(
                "an XML property list that refers to an entity beyond the predefined ones",
            ));
        }
    }
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The name of the document type declaration, if the document has one, and the root element.
fn build(input: &[u8]) -> Result<(Option<String>, Element), Problem> {
    let mut open: Vec<Element> = Vec::new();
    let mut root = None;
    let mut document_type = false;
    let result = xml::parse(input, &mut |event| {
        match event {
            Event::DocumentType => document_type = true,
            Event::Start(name, _) => open.push(Element {
                name: name.qualified_name().to_owned(),
                children: Vec::new(),
            }),
            Event::End(_) => {
                let element = open.pop().expect("an end tag closes an open element");
                match open.last_mut() {
                    Some(parent) => parent.children.push(Node::Element(element)),
                    None => root = Some(element),
                }
            }
            Event::Text(text, _) => {
                if let Some(element) = open.last_mut() {
                    if !text.is_empty() {
                        match element.children.last_mut() {
                            Some(Node::Text(run)) => run.push_str(text),
                            _ => element.children.push(Node::Text(text.to_owned())),
                        }
                    }
                }
            }
            Event::ProcessingInstruction(..) => {
                if let Some(element) = open.last_mut() {
                    element.children.push(Node::Other);
                }
            }
            Event::Comment(_) => {}
        }
        Flow::Continue
    });
    if let Err(error) = result {
        if contains(input, b":") || contains(input, b"xmlns") {
            return Err(Problem::Unread(
                "a malformed XML property list with names a namespace-unaware parser may accept",
            ));
        }
        return failed(match error {
            XmlError::CharConversion => "a byte sequence its encoding does not allow".to_owned(),
            XmlError::Malformed(reason) => reason,
            XmlError::EntityExpansionLimit => "more entity expansions than allowed".to_owned(),
        });
    }
    let root = root.expect("a well-formed document has a root element");
    let document_type = if document_type {
        Some(document_type_name(input)?)
    } else {
        None
    };
    Ok((document_type, root))
}

/// The name a document's one `<!DOCTYPE` declares, read from its ASCII-compatible bytes.
fn document_type_name(input: &[u8]) -> Result<String, Problem> {
    const DOCTYPE: &[u8] = b"<!DOCTYPE";
    let mut declarations = input
        .windows(DOCTYPE.len())
        .enumerate()
        .filter(|(_, window)| *window == DOCTYPE)
        .map(|(at, _)| at);
    let (Some(at), None) = (declarations.next(), declarations.next()) else {
        return Err(Problem::Unread(
            "an XML property list that spells <!DOCTYPE more than once",
        ));
    };
    let rest = &input[at + DOCTYPE.len()..];
    let rest = &rest[rest
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count()..];
    let end = rest
        .iter()
        .position(|&byte| byte.is_ascii_whitespace() || byte == b'>' || byte == b'[')
        .unwrap_or(rest.len());
    std::str::from_utf8(&rest[..end])
        .map(str::to_owned)
        .map_err(|_| Problem::Unread("a document type name beyond ASCII"))
}

/// `parseObject`: the object an element stands for, `None` for an element of another name.
fn object(element: &Element, depth: usize) -> Result<Value, Problem> {
    if depth > MAX_NESTING_DEPTH {
        return failed(format!(
            "The nesting depth of the property list exceeds the maximum supported depth of \
             {MAX_NESTING_DEPTH}."
        ));
    }
    let unparsed = || Problem::Failed(format!("The {} node could not be parsed.", element.name));
    let object = match element.name.as_str() {
        "dict" => {
            let children: Vec<&Element> = element.elements().collect();
            let mut dict = Dict::default();
            for pair in children.chunks(2) {
                let [key, value] = pair else {
                    return Err(unparsed());
                };
                dict.put(
                    Key::String(key.text().to_owned()),
                    object(value, depth + 1)?,
                );
            }
            Object::Dict(dict)
        }
        "array" => Object::Array(
            element
                .elements()
                .map(|child| object(child, depth + 1))
                .collect::<Result<_, _>>()?,
        ),
        "true" => Object::Boolean(true),
        "false" => Object::Boolean(false),
        "integer" | "real" => number(element.text()).ok_or_else(unparsed)?,
        "string" => Object::String(element.text().to_owned()),
        "data" => Object::Data(data(element.text()).ok_or_else(unparsed)?),
        "date" => Object::Date(date(element.text())?),
        _ => return Ok(None),
    };
    Ok(Some(Rc::new(object)))
}

/// `new NSNumber(text)`: a boolean by name, a special real, a decimal or `0x` hexadecimal
/// `long`, else a `double`; `None` where it throws.
fn number(text: &str) -> Option<Object> {
    let named = |name: &str| java::equals_ignore_case(text, name);
    if named("true") || named("YES") {
        return Some(Object::Boolean(true));
    }
    if named("false") || named("NO") {
        return Some(Object::Boolean(false));
    }
    if named("nan") {
        return Some(Object::Real(f64::NAN));
    }
    if named("+infinity") {
        return Some(Object::Real(f64::INFINITY));
    }
    if named("-infinity") {
        return Some(Object::Real(f64::NEG_INFINITY));
    }
    let integer = match text.strip_prefix("0x") {
        Some(hexadecimal) => java::parse_hex_long(hexadecimal),
        None => java::parse_long(text),
    };
    integer
        .map(Object::Integer)
        .or_else(|| java::parse_double(text).map(Object::Real))
}

/// `new NSData(base64)`: the text without Java's `\s` characters, decoded by dd-plist's
/// `Base64.decode` from its US-ASCII bytes; `None` where it throws.
fn data(text: &str) -> Option<Vec<u8>> {
    let encoded: Vec<u8> = text
        .chars()
        .filter(|&character| !java::is_regex_space(character))
        .map(|character| {
            u8::try_from(character)
                .ok()
                .filter(u8::is_ascii)
                .unwrap_or(b'?')
        })
        .collect();
    if encoded.is_empty() {
        return Some(Vec::new());
    }
    if encoded.len() < 4 {
        return None;
    }
    let value = |byte: u8| -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0xFF),
            _ => None,
        }
    };
    let mut decoded = Vec::with_capacity(encoded.len() * 3 / 4);
    let mut quartet = [0u8; 4];
    let mut filled = 0;
    for &byte in &encoded {
        value(byte)?;
        quartet[filled] = byte;
        filled += 1;
        if filled < 4 {
            continue;
        }
        filled = 0;
        let bits = quartet.iter().fold(0u32, |bits, &byte| {
            bits << 6 | value(byte).unwrap_or_default()
        });
        let [_, high, middle, low] = bits.to_be_bytes();
        let width = if quartet[2] == b'=' {
            1
        } else if quartet[3] == b'=' {
            2
        } else {
            3
        };
        decoded.extend_from_slice(&[high, middle, low][..width]);
        if byte == b'=' {
            break;
        }
    }
    Some(decoded)
}

/// `new NSDate(text)`'s seconds since 2001 for a date in the canonical form
/// `yyyy-MM-dd'T'HH:mm:ss'Z'` of a year from 1 to 9999, each field in range. The lenient
/// `SimpleDateFormat` parse dd-plist makes of any other text is not ported.
fn date(text: &str) -> Result<f64, Problem> {
    let unread = || Problem::Unread("a date the port does not parse as SimpleDateFormat does");
    let bytes = text.as_bytes();
    let shape = b"dddd-dd-ddTdd:dd:ddZ";
    if bytes.len() != shape.len()
        || !bytes.iter().zip(shape).all(|(&byte, &expected)| {
            if expected == b'd' {
                byte.is_ascii_digit()
            } else {
                byte == expected
            }
        })
    {
        return Err(unread());
    }
    let field = |range: std::ops::Range<usize>| -> i32 {
        text[range].parse().expect("the field is ASCII digits")
    };
    let (year, month, day) = (field(0..4), field(5..7), field(8..10));
    let (hour, minute, second) = (field(11..13), field(14..16), field(17..19));
    let days_in_month = match month {
        2 if java::time::civil::is_leap_year(i64::from(year)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if year == 0
        || !(1..=12).contains(&month)
        || !(1..=days_in_month).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(unread());
    }
    let millis = CalendarFields {
        year,
        month: month - 1,
        day,
        hour,
        minute,
        second,
        millisecond: 0,
    }
    .epoch_millis(&TimeZone::fixed(0));
    Ok((millis - EPOCH) as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(xml: &str) -> Result<Value, Problem> {
        parse(xml.as_bytes())
    }

    #[test]
    fn reads_objects_as_dd_plist_does() {
        let value = root(
            "<?xml version='1.0'?><!DOCTYPE plist PUBLIC '-//Apple//DTD PLIST 1.0//EN' 'x.dtd'>\
             <plist><dict><key>a</key><string>x<!--c-->y<![CDATA[z]]><?p?>w</string>\
             <key>b</key><integer>0x1F</integer><key>c</key><real> 2</real><key>d</key><true/>\
             <key>a</key><unknown/><key>e</key><data>QU\nJD</data><key>f</key><date>2001-01-02T00:00:00Z</date>\
             </dict></plist>",
        )
        .unwrap()
        .unwrap();
        let Object::Dict(dict) = &*value else {
            panic!("{value:?}")
        };
        let entries: Vec<_> = dict
            .entries()
            .iter()
            .map(|(key, value)| (key.clone(), value.as_deref().cloned()))
            .collect();
        let entry = |key: &str, value: Object| (Key::String(key.to_owned()), Some(value));
        // The unknown element is a null, which takes the key's value: dd-plist drops it (#104).
        assert_eq!(
            entries,
            [
                (Key::String("a".to_owned()), None),
                entry("b", Object::Integer(31)),
                entry("c", Object::Real(2.0)),
                entry("d", Object::Boolean(true)),
                entry("e", Object::Data(b"ABC".to_vec())),
                entry("f", Object::Date(86_400.0)),
            ]
        );
    }

    #[test]
    fn fails_where_dd_plist_fails() {
        for xml in [
            "<plist/>",
            "<plist><a/><b/></plist>",
            "<root><string/></root>",
            "<!DOCTYPE other><plist><string/></plist>",
            "<plist><dict><key>a</key></dict></plist>",
            "<plist><integer>x</integer></plist>",
            "<plist><data>QQ</data></plist>",
            "<plist><data>QU*D</data></plist>",
        ] {
            assert!(matches!(root(xml), Err(Problem::Failed(_))), "{xml}");
        }
        assert!(root("<!DOCTYPE plist><dict/>").unwrap().is_some());
    }

    #[test]
    fn leaves_what_it_does_not_read_to_tika() {
        for xml in [
            "<!DOCTYPE plist [<!ENTITY e 'v'>]><plist><string>&e;</string></plist>",
            "<plist><string>&e;</string></plist>",
            "<plist><date>2001-1-02T00:00:00Z</date></plist>",
            "<p:plist><string/></plist>",
        ] {
            assert!(matches!(root(xml), Err(Problem::Unread(_))), "{xml}");
        }
    }
}
