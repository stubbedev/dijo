//! The content lines of the directory formats: iCalendar (RFC 5545), vCard 3.0 and 4.0 (RFC 2425,
//! RFC 6350) and vCard 2.1. A content line is `[group.]NAME *(;PARAM=VALUE[,VALUE]):value`,
//! folded onto lines that continue with a space or a tab, or, in a vCard 2.1 quoted-printable
//! value, onto lines after a soft line break (`=` at the end of a line). Names compare without
//! regard to case. Parameter values may be quoted and hold RFC 6868 caret escapes; vCard 2.1's
//! bare parameters are read as the `ENCODING` or `TYPE` values they stand for. Text values hold
//! the escapes `\n`, `\N`, `\,`, `\;` and `\\`, and in vCard 2.1 may be quoted-printable bytes of
//! the `CHARSET` parameter's charset.

use std::borrow::Cow;

use crate::formats::xhtml::{within_budget, Stop};
use crate::text::charset::Charset;
use crate::text::quoted_printable::decode_escapes;

/// The bare vCard 2.1 parameters that name an encoding rather than a type.
const BARE_ENCODINGS: [&str; 5] = ["7BIT", "8BIT", "QUOTED-PRINTABLE", "BASE64", "B"];

/// A logical line: the physical lines of one content line unfolded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logical {
    /// The number, from 1, of the physical line it starts on.
    pub number: usize,
    pub text: String,
}

/// The logical lines of physical lines, unfolded, without the empty lines between them.
pub struct Unfolded<I> {
    lines: I,
    /// The physical line read after the logical line returned last, and its number.
    ahead: Option<(usize, String)>,
    /// The number of physical lines read.
    read: usize,
}

/// The logical lines of `lines`, the physical lines of a directory file.
pub fn unfold<I: Iterator<Item = String>>(lines: I) -> Unfolded<I> {
    Unfolded {
        lines,
        ahead: None,
        read: 0,
    }
}

impl<I: Iterator<Item = String>> Unfolded<I> {
    fn next_physical(&mut self) -> Option<(usize, String)> {
        if let Some(ahead) = self.ahead.take() {
            return Some(ahead);
        }
        let line = self.lines.next()?;
        self.read += 1;
        Some((self.read, line))
    }
}

impl<I: Iterator<Item = String>> Iterator for Unfolded<I> {
    type Item = Result<Logical, Stop>;

    fn next(&mut self) -> Option<Self::Item> {
        let (number, mut text) = loop {
            let (number, line) = self.next_physical()?;
            if !line.is_empty() {
                break (number, line);
            }
        };
        while let Some((next_number, next)) = self.next_physical() {
            if text.ends_with('=') && is_quoted_printable(&text) {
                text.pop();
                text.push_str(&next);
            } else if let Some(continued) = next.strip_prefix([' ', '\t']) {
                text.push_str(continued);
            } else {
                self.ahead = Some((next_number, next));
                break;
            }
            if let Err(stop) = within_budget() {
                return Some(Err(stop));
            }
        }
        Some(Ok(Logical { number, text }))
    }
}

/// Whether the name and parameters of the logical line `text` declare its value
/// quoted-printable, so that an `=` ending a physical line of it is a soft line break.
fn is_quoted_printable(text: &str) -> bool {
    let head = &text[..value_start(text).unwrap_or(text.len())];
    head.as_bytes()
        .windows(b"QUOTED-PRINTABLE".len())
        .any(|window| window.eq_ignore_ascii_case(b"QUOTED-PRINTABLE"))
}

/// Where the value of the logical line `text` starts: after the first colon outside a quoted
/// parameter value.
fn value_start(text: &str) -> Option<usize> {
    let mut quoted = false;
    for (at, byte) in text.bytes().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b':' if !quoted => return Some(at + 1),
            _ => {}
        }
    }
    None
}

/// How a value's characters encode it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// The characters are the value, as text escapes them.
    Text,
    /// vCard 2.1's quoted-printable bytes, of the `CHARSET` parameter's charset.
    QuotedPrintable,
    /// Base64 bytes of a binary value.
    Base64,
}

/// A parameter, its values unquoted and their caret escapes decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter<'a> {
    pub name: &'a str,
    pub values: Vec<Cow<'a, str>>,
}

/// A content line, borrowed from its logical line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentLine<'a> {
    pub group: Option<&'a str>,
    pub name: &'a str,
    pub parameters: Vec<Parameter<'a>>,
    /// The value as written, escapes and encoding kept.
    pub raw: &'a str,
}

impl<'a> ContentLine<'a> {
    /// The content line the logical line `text` holds, or what is wrong with it.
    pub fn parse(text: &'a str) -> Result<Self, String> {
        let name_end = text
            .find([';', ':'])
            .ok_or_else(|| format!("the content line {text:?} has no colon before its value"))?;
        let (group, name) = match text[..name_end].rsplit_once('.') {
            Some((group, name)) => (Some(group), name),
            None => (None, &text[..name_end]),
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(format!("the content line {text:?} has no name"));
        }
        let mut parameters = Vec::new();
        let mut rest = &text[name_end..];
        while let Some(after) = rest.strip_prefix(';') {
            let (parameter, remaining) = parse_parameter(after)
                .ok_or_else(|| format!("the content line {text:?} has a malformed parameter"))?;
            parameters.push(parameter);
            rest = remaining;
        }
        let raw = rest
            .strip_prefix(':')
            .ok_or_else(|| format!("the content line {text:?} has no colon before its value"))?;
        Ok(Self {
            group,
            name,
            parameters,
            raw,
        })
    }

    /// Whether the line's name is `name`, regardless of case.
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// The first value of the parameter named `name`, regardless of case.
    pub fn parameter<'s>(&'s self, name: &'s str) -> Option<&'s str> {
        self.parameter_values(name).next()
    }

    /// Every value of every parameter named `name`, regardless of case.
    pub fn parameter_values<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'s str> + 's {
        self.parameters
            .iter()
            .filter(move |parameter| parameter.name.eq_ignore_ascii_case(name))
            .flat_map(|parameter| parameter.values.iter().map(|value| value.as_ref()))
    }

    /// How the value is encoded, by its `ENCODING` parameter.
    pub fn encoding(&self) -> Result<Encoding, String> {
        let Some(encoding) = self.parameter("ENCODING") else {
            return Ok(Encoding::Text);
        };
        if ["7BIT", "8BIT"]
            .iter()
            .any(|name| encoding.eq_ignore_ascii_case(name))
        {
            Ok(Encoding::Text)
        } else if encoding.eq_ignore_ascii_case("QUOTED-PRINTABLE") {
            Ok(Encoding::QuotedPrintable)
        } else if encoding.eq_ignore_ascii_case("BASE64") || encoding.eq_ignore_ascii_case("B") {
            Ok(Encoding::Base64)
        } else {
            Err(format!(
                "the {} line has the unknown encoding {encoding:?}",
                self.name
            ))
        }
    }

    /// The value as text, decoded and unescaped.
    pub fn text(&self) -> Result<Cow<'a, str>, String> {
        Ok(match self.decoded()? {
            Cow::Borrowed(raw) => unescape(raw),
            Cow::Owned(decoded) => Cow::Owned(unescape(&decoded).into_owned()),
        })
    }

    /// The values of a list or structured value, split at each `separator` that no backslash
    /// escapes, each decoded and unescaped.
    pub fn texts(&self, separator: char) -> Result<Vec<String>, String> {
        let decoded = self.decoded()?;
        Ok(split_unescaped(&decoded, separator)
            .map(|part| unescape(part).into_owned())
            .collect())
    }

    /// The bytes of a base64 value.
    pub fn binary(&self) -> Result<Vec<u8>, String> {
        match self.encoding()? {
            Encoding::Base64 => crate::java::base64_mime_decode(self.raw)
                .ok_or_else(|| format!("the {} line's base64 value is malformed", self.name)),
            _ => Err(format!("the {} line's value is not base64", self.name)),
        }
    }

    /// The value's characters as its encoding decodes them, escapes kept.
    fn decoded(&self) -> Result<Cow<'a, str>, String> {
        match self.encoding()? {
            Encoding::Text => Ok(Cow::Borrowed(self.raw)),
            Encoding::QuotedPrintable => {
                let charset = match self.parameter("CHARSET") {
                    Some(name) => Charset::for_name(name).ok_or_else(|| {
                        format!("the {} line names the unknown charset {name:?}", self.name)
                    })?,
                    None => Charset::named("UTF-8"),
                };
                let mut bytes = Vec::with_capacity(self.raw.len());
                decode_escapes(self.raw.as_bytes(), &mut bytes);
                charset.decode(&bytes).map(Cow::Owned).ok_or_else(|| {
                    format!(
                        "the {} line's charset {} is not one the port decodes",
                        self.name,
                        charset.name()
                    )
                })
            }
            Encoding::Base64 => Err(format!(
                "the {} line's value is base64 bytes, not text",
                self.name
            )),
        }
    }
}

/// Someone a directory entry names, as mail writes a mailbox: a calendar entry's organiser or
/// attendee, or a card's holder at one of its email addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub name: Option<String>,
    pub address: String,
}

impl Person {
    /// The person `name` at `uri`, a `mailto:` URI as the email address it names.
    pub fn new(name: Option<String>, uri: &str) -> Self {
        let uri = uri.trim();
        let address = match uri.get(..7) {
            Some(scheme) if scheme.eq_ignore_ascii_case("mailto:") => &uri[7..],
            _ => uri,
        };
        Self {
            name,
            address: address.to_owned(),
        }
    }
}

impl std::fmt::Display for Person {
    /// `Name <address>`, as mail writes a mailbox, or the address alone.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.name, self.address.is_empty()) {
            (Some(name), true) => f.write_str(name),
            (Some(name), false) => write!(f, "{name} <{}>", self.address),
            (None, _) => f.write_str(&self.address),
        }
    }
}

/// The parameter at the start of `text`, after its semicolon, and what follows it, or `None`
/// when it is malformed: an empty name or an unclosed quote.
fn parse_parameter(text: &str) -> Option<(Parameter<'_>, &str)> {
    let name_end = text.find(['=', ';', ':']).unwrap_or(text.len());
    let name = &text[..name_end];
    if name.is_empty() {
        return None;
    }
    let Some(mut rest) = text[name_end..].strip_prefix('=') else {
        let bare = Parameter {
            name: if BARE_ENCODINGS.iter().any(|e| name.eq_ignore_ascii_case(e)) {
                "ENCODING"
            } else {
                "TYPE"
            },
            values: vec![Cow::Borrowed(name)],
        };
        return Some((bare, &text[name_end..]));
    };
    let mut values = Vec::new();
    loop {
        let (value, remaining) = match rest.strip_prefix('"') {
            Some(quoted) => {
                let end = quoted.find('"')?;
                (&quoted[..end], &quoted[end + 1..])
            }
            None => {
                let end = rest.find([',', ';', ':']).unwrap_or(rest.len());
                (&rest[..end], &rest[end..])
            }
        };
        values.push(decode_carets(value));
        match remaining.strip_prefix(',') {
            Some(next) => rest = next,
            None => return Some((Parameter { name, values }, remaining)),
        }
    }
}

/// A parameter value with its RFC 6868 escapes decoded: `^n` a line break, `^'` a double quote
/// and `^^` a caret, any other caret kept.
fn decode_carets(value: &str) -> Cow<'_, str> {
    if !value.contains('^') {
        return Cow::Borrowed(value);
    }
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '^' {
            decoded.push(c);
            continue;
        }
        match chars.clone().next() {
            Some('n') => decoded.push('\n'),
            Some('\'') => decoded.push('"'),
            Some('^') => decoded.push('^'),
            _ => {
                decoded.push('^');
                continue;
            }
        }
        chars.next();
    }
    Cow::Owned(decoded)
}

/// A text value with its escapes decoded: `\n` and `\N` a line break, `\,`, `\;`, `\:` and `\\`
/// the character escaped, any other backslash kept.
pub fn unescape(value: &str) -> Cow<'_, str> {
    if !value.contains('\\') {
        return Cow::Borrowed(value);
    }
    let mut unescaped = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            unescaped.push(c);
            continue;
        }
        match chars.clone().next() {
            Some('n' | 'N') => unescaped.push('\n'),
            Some(escaped @ (',' | ';' | ':' | '\\')) => unescaped.push(escaped),
            _ => {
                unescaped.push('\\');
                continue;
            }
        }
        chars.next();
    }
    Cow::Owned(unescaped)
}

/// The parts of `value` between the `separator`s no backslash escapes, escapes kept.
fn split_unescaped(value: &str, separator: char) -> impl Iterator<Item = &str> {
    let mut rest = Some(value);
    std::iter::from_fn(move || {
        let text = rest?;
        let mut escaped = false;
        for (at, c) in text.char_indices() {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == separator {
                rest = Some(&text[at + c.len_utf8()..]);
                return Some(&text[..at]);
            }
        }
        rest = None;
        Some(text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logical(text: &str) -> Vec<(usize, String)> {
        unfold(text.lines().map(str::to_owned))
            .map(|line| line.map(|line| (line.number, line.text)).unwrap())
            .collect()
    }

    #[test]
    fn unfolds_lines_continued_with_a_space_or_a_tab() {
        assert_eq!(
            logical("SUMMARY:Lamp \n  check\n\tdone\n\nLOCATION:Tower"),
            [
                (1, "SUMMARY:Lamp  checkdone".to_owned()),
                (5, "LOCATION:Tower".to_owned())
            ]
        );
    }

    #[test]
    fn unfolds_soft_line_breaks_of_quoted_printable_values() {
        assert_eq!(
            logical("NOTE;ENCODING=QUOTED-PRINTABLE:Caf=C3=A9 =\n au phare=\nX\nFN:Keeper\n"),
            [
                (
                    1,
                    "NOTE;ENCODING=QUOTED-PRINTABLE:Caf=C3=A9  au phareX".to_owned()
                ),
                (4, "FN:Keeper".to_owned())
            ]
        );
        assert_eq!(
            logical("NOTE:ends with =\nFN:Keeper"),
            [
                (1, "NOTE:ends with =".to_owned()),
                (2, "FN:Keeper".to_owned())
            ]
        );
    }

    #[test]
    fn parses_groups_names_parameters_and_values() {
        let line = ContentLine::parse(
            "item1.ATTENDEE;CN=\"Doe, Jane\";ROLE=REQ-PARTICIPANT;MEMBER=\"a:b\",c;X-N=x^'y^n^^:mailto:jane@example.org",
        )
        .unwrap();
        assert_eq!(line.group, Some("item1"));
        assert!(line.is("attendee"));
        assert_eq!(line.parameter("cn"), Some("Doe, Jane"));
        assert_eq!(
            line.parameter_values("MEMBER").collect::<Vec<_>>(),
            ["a:b", "c"]
        );
        assert_eq!(line.parameter("X-N"), Some("x\"y\n^"));
        assert_eq!(line.raw, "mailto:jane@example.org");
    }

    #[test]
    fn reads_bare_vcard_2_1_parameters_as_types_and_encodings() {
        let line = ContentLine::parse("TEL;WORK;VOICE;QUOTED-PRINTABLE:1=32").unwrap();
        assert_eq!(
            line.parameter_values("TYPE").collect::<Vec<_>>(),
            ["WORK", "VOICE"]
        );
        assert_eq!(line.encoding(), Ok(Encoding::QuotedPrintable));
        assert_eq!(line.text().unwrap(), "12");
    }

    #[test]
    fn unescapes_text_values() {
        let line = ContentLine::parse(r"DESCRIPTION:One\, two\; three\nfour\\five\x").unwrap();
        assert_eq!(line.text().unwrap(), "One, two; three\nfour\\five\\x");
        let list = ContentLine::parse(r"CATEGORIES:Lamp\, oil,Tower,").unwrap();
        assert_eq!(list.texts(',').unwrap(), ["Lamp, oil", "Tower", ""]);
    }

    #[test]
    fn decodes_quoted_printable_values_in_their_charset() {
        let line =
            ContentLine::parse("N;CHARSET=ISO-8859-1;ENCODING=QUOTED-PRINTABLE:Gr=F8n;Ove\\;=3D")
                .unwrap();
        assert_eq!(line.texts(';').unwrap(), ["Grøn", "Ove;="]);
        let unknown = ContentLine::parse("N;CHARSET=X-NONE;ENCODING=QUOTED-PRINTABLE:a").unwrap();
        assert!(unknown.text().unwrap_err().contains("X-NONE"));
    }

    #[test]
    fn decodes_base64_values_as_bytes() {
        let line = ContentLine::parse("PHOTO;ENCODING=b;TYPE=png:aGk=").unwrap();
        assert_eq!(line.binary().unwrap(), b"hi");
        assert!(line.text().is_err());
    }

    #[test]
    fn writes_people_as_mail_writes_mailboxes() {
        let named = Person::new(Some("Keeper, The".to_owned()), " MAILTO:keeper@example.org");
        assert_eq!(named.to_string(), "Keeper, The <keeper@example.org>");
        assert_eq!(
            Person::new(None, "bo@example.org").to_string(),
            "bo@example.org"
        );
        assert_eq!(Person::new(Some("Ada".to_owned()), "").to_string(), "Ada");
    }

    #[test]
    fn rejects_lines_without_a_name_or_a_value() {
        assert!(ContentLine::parse("no colon here").is_err());
        assert!(ContentLine::parse(":value").is_err());
        assert!(ContentLine::parse("X;CN=\"open:value").is_err());
        assert!(ContentLine::parse("X;=a:value").is_err());
    }
}
