//! Header fields as mime4j 0.8.15 reads them: `RawFieldParser`'s values, comments and quoted
//! strings over a field's bytes, and `RawField`, a field's name and unfolded body.

use super::codec::decode_encoded_words;
use super::Unported;
use crate::java;

/// `CharsetUtil.isWhitespace`.
pub(super) fn is_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

/// A set of delimiter bytes, as mime4j's `BitSet`s of ASCII characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Delimiters(u128);

impl Delimiters {
    pub(super) const fn of(bytes: &[u8]) -> Self {
        let mut set = 0u128;
        let mut i = 0;
        while i < bytes.len() {
            set |= 1 << bytes[i];
            i += 1;
        }
        Delimiters(set)
    }

    pub(super) const fn with(self, other: Delimiters) -> Self {
        Delimiters(self.0 | other.0)
    }

    pub(super) fn contains(self, byte: u8) -> bool {
        byte < 128 && self.0 & (1 << byte) != 0
    }
}

/// A position in a field's bytes and the end it may not pass, as `ParserCursor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cursor {
    pub(super) pos: usize,
    pub(super) end: usize,
}

impl Cursor {
    pub(super) fn new(pos: usize, end: usize) -> Self {
        Cursor { pos, end }
    }

    pub(super) fn at_end(self) -> bool {
        self.pos >= self.end
    }
}

/// A word of a value: an atom, or a quoted string's content, and whether white space comes
/// before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Word {
    text: String,
    quoted: bool,
    spaced: bool,
}

/// The words of a value as `RawFieldParser.parseValue` reads them, up to a delimiter: atoms and
/// quoted strings, comments left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Words(Vec<Word>);

impl Words {
    /// `RawFieldParser.parseValue`: the words joined, a space where white space separates two,
    /// and a quoted string that starts with an encoded word decoded.
    pub(super) fn value(&self) -> Result<String, Unported> {
        self.join(|word| decode_quoted(&word.text))
    }

    /// The words as a phrase, a display name, shows them: encoded words decoded once, in the
    /// atoms and, leniently, in a quoted string that starts with one. mime4j decodes the
    /// quoted string as it reads it and the phrase again after, and Tika the display name a
    /// third time, so text that decodes to an encoded word is decoded again (#77).
    pub(super) fn phrase(&self) -> Result<String, Unported> {
        let mut out = String::new();
        let mut atoms = String::new();
        let mut atoms_spaced = false;
        let flush = |atoms: &mut String, spaced: bool, out: &mut String| -> Result<(), Unported> {
            if atoms.is_empty() {
                return Ok(());
            }
            if spaced && !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&decode_encoded_words(atoms)?);
            atoms.clear();
            Ok(())
        };
        for word in &self.0 {
            if word.quoted {
                flush(&mut atoms, atoms_spaced, &mut out)?;
                if word.spaced && !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&decode_quoted(&word.text)?);
            } else if atoms.is_empty() {
                atoms.push_str(&word.text);
                atoms_spaced = word.spaced;
            } else {
                if word.spaced {
                    atoms.push(' ');
                }
                atoms.push_str(&word.text);
            }
        }
        flush(&mut atoms, atoms_spaced, &mut out)?;
        Ok(out)
    }

    fn join(&self, text: impl Fn(&Word) -> Result<String, Unported>) -> Result<String, Unported> {
        let mut out = String::new();
        for word in &self.0 {
            if word.spaced && !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&text(word)?);
        }
        Ok(out)
    }
}

/// A quoted string's content, its encoded words decoded when it starts with one, as mime4j
/// decodes it leniently.
fn decode_quoted(text: &str) -> Result<String, Unported> {
    if text.starts_with("=?") {
        decode_encoded_words(text)
    } else {
        Ok(text.to_owned())
    }
}

/// The words of a value up to a delimiter, as `RawFieldParser.parseValue` reads them.
pub(super) fn parse_words(buf: &[u8], cursor: &mut Cursor, delimiters: Delimiters) -> Words {
    let mut words = Vec::new();
    let mut whitespace = false;
    while !cursor.at_end() {
        let current = buf[cursor.pos];
        if delimiters.contains(current) {
            break;
        } else if is_whitespace(current) {
            skip_whitespace(buf, cursor);
            whitespace = true;
        } else if current == b'(' {
            skip_comment(buf, cursor);
        } else {
            let quoted = current == b'"';
            let mut text = String::new();
            if quoted {
                copy_quoted_content(buf, cursor, &mut text);
            } else {
                copy_content(buf, cursor, delimiters, &mut text, true);
            }
            words.push(Word {
                text,
                quoted,
                spaced: whitespace,
            });
            whitespace = false;
        }
    }
    Words(words)
}

/// `RawFieldParser.parseValue`: the words up to a delimiter joined, as [`Words::value`].
pub(super) fn parse_value(
    buf: &[u8],
    cursor: &mut Cursor,
    delimiters: Delimiters,
) -> Result<String, Unported> {
    parse_words(buf, cursor, delimiters).value()
}

pub(super) fn skip_whitespace(buf: &[u8], cursor: &mut Cursor) {
    while !cursor.at_end() && is_whitespace(buf[cursor.pos]) {
        cursor.pos += 1;
    }
}

/// `RawFieldParser.skipComment`: a comment with its nested comments and escaped characters,
/// or the rest of the field when it does not close.
pub(super) fn skip_comment(buf: &[u8], cursor: &mut Cursor) {
    if cursor.at_end() || buf[cursor.pos] != b'(' {
        return;
    }
    let mut pos = cursor.pos + 1;
    let mut level = 1;
    let mut escaped = false;
    while pos < cursor.end {
        let current = buf[pos];
        if escaped {
            escaped = false;
        } else if current == b'\\' {
            escaped = true;
        } else if current == b'(' {
            level += 1;
        } else if current == b')' {
            level -= 1;
        }
        pos += 1;
        if level <= 0 {
            break;
        }
    }
    cursor.pos = pos;
}

/// `RawFieldParser.skipAllWhiteSpace`: white space and comments.
pub(super) fn skip_all_whitespace(buf: &[u8], cursor: &mut Cursor) {
    while !cursor.at_end() {
        let current = buf[cursor.pos];
        if is_whitespace(current) {
            skip_whitespace(buf, cursor);
        } else if current == b'(' {
            skip_comment(buf, cursor);
        } else {
            break;
        }
    }
}

/// `RawFieldParser.copyContent`, or with `unquoted` `copyUnquotedContent`, which also stops at a
/// quote: the bytes up to a delimiter, white space or a comment, decoded as UTF-8 when any is
/// not ASCII.
pub(super) fn copy_content(
    buf: &[u8],
    cursor: &mut Cursor,
    delimiters: Delimiters,
    dst: &mut String,
    unquoted: bool,
) {
    let start = cursor.pos;
    let mut pos = start;
    while pos < cursor.end {
        let current = buf[pos];
        if delimiters.contains(current)
            || is_whitespace(current)
            || current == b'('
            || (unquoted && current == b'"')
        {
            break;
        }
        pos += 1;
    }
    cursor.pos = pos;
    dst.push_str(&java::utf8(&buf[start..pos]));
}

/// `RawFieldParser.copyQuotedContent`: a quoted string's content, escapes resolved but for a
/// backslash before a character other than a quote or a backslash, and line breaks left out.
pub(super) fn copy_quoted_content(buf: &[u8], cursor: &mut Cursor, dst: &mut String) {
    if cursor.at_end() || buf[cursor.pos] != b'"' {
        return;
    }
    let mut pos = cursor.pos + 1;
    let mut raw = Vec::new();
    let mut escaped = false;
    while pos < cursor.end {
        let current = buf[pos];
        pos += 1;
        if escaped {
            if current != b'"' && current != b'\\' {
                raw.push(b'\\');
            }
            raw.push(current);
            escaped = false;
        } else if current == b'"' {
            break;
        } else if current == b'\\' {
            escaped = true;
        } else if current != b'\r' && current != b'\n' {
            raw.push(current);
        }
    }
    dst.push_str(&java::utf8(&raw));
    cursor.pos = pos;
}

/// The characters mime4j allows in a field name: printable ASCII but the colon.
fn is_field_char(c: char) -> bool {
    matches!(c, '\u{21}'..='\u{39}' | '\u{3B}'..='\u{7E}')
}

/// A header field as `DefaultFieldBuilder.build` makes it: its bytes without the line break that
/// ends it, its name, and where the colon after the name is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawField {
    raw: Vec<u8>,
    delimiter: usize,
    name: String,
}

/// Why `DefaultFieldBuilder.build` rejects a header line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InvalidField {
    NoColon,
    IllegalName,
}

impl RawField {
    /// A field of its bytes, its folded lines included: its name the bytes up to the first
    /// colon, less the spaces and tabs RFC 5322's obsolete syntax allows before the colon, a
    /// field whose name is empty or holds anything but printable ASCII invalid. mime4j's
    /// `DefaultFieldBuilder` reads the name as a token, which leaves comments out, and trims it
    /// of control characters, so `\x01To` names `To` (#78).
    pub(super) fn parse(mut raw: Vec<u8>) -> Result<RawField, InvalidField> {
        if raw.last() == Some(&b'\n') {
            raw.pop();
        }
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        let Some(colon) = raw.iter().position(|&byte| byte == b':') else {
            return Err(InvalidField::NoColon);
        };
        let name = java::utf8(&raw[..colon]);
        let name = name.trim_end_matches([' ', '\t']);
        if name.is_empty() || !name.chars().all(is_field_char) {
            return Err(InvalidField::IllegalName);
        }
        let name = name.to_owned();
        Ok(RawField {
            raw,
            delimiter: colon,
            name,
        })
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// The field's bytes, as `getRaw`.
    pub(super) fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// `RawField.getBody`: the text after the colon and one white space character when more
    /// follows, decoded as UTF-8 when any byte is not ASCII, and unfolded.
    pub(super) fn body(&self) -> String {
        let mut start = self.delimiter + 1;
        if self.raw.len() > start + 1 && is_whitespace(self.raw[start]) {
            start += 1;
        }
        unfold(&java::utf8(&self.raw[start.min(self.raw.len())..]))
    }
}

/// `MimeUtil.unfold`: the text without its carriage returns and line feeds.
pub(super) fn unfold(text: &str) -> String {
    text.chars().filter(|&c| c != '\r' && c != '\n').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_names_and_bodies_as_mime4j_does() {
        let field = RawField::parse(b"Subject:  two\r\n\tlines\r\n".to_vec()).unwrap();
        assert_eq!(field.name(), "Subject");
        assert_eq!(field.body(), " two\tlines");
        let short = RawField::parse(b"X-A: ".to_vec()).unwrap();
        assert_eq!(short.body(), " ");
        let commented = RawField::parse(b"X(c)Y : v".to_vec()).unwrap();
        assert_eq!(commented.name(), "X(c)Y");
        assert_eq!(commented.body(), "v");
        assert_eq!(
            RawField::parse(b"X (c) : v".to_vec()),
            Err(InvalidField::IllegalName)
        );
        assert_eq!(
            RawField::parse(b": v".to_vec()),
            Err(InvalidField::IllegalName)
        );
        assert_eq!(
            RawField::parse(b"From someone Mon".to_vec()),
            Err(InvalidField::NoColon)
        );
        assert_eq!(
            RawField::parse(b"From someone: x".to_vec()),
            Err(InvalidField::IllegalName)
        );
        assert_eq!(
            RawField::parse(b"\x01To: x".to_vec()),
            Err(InvalidField::IllegalName)
        );
        assert_eq!(
            RawField::parse(b" To: x".to_vec()),
            Err(InvalidField::IllegalName)
        );
    }

    #[test]
    fn reads_values_as_raw_field_parser_does() {
        let buf = b" a  (note (nested)) \"q\\\"x\\y\" b;rest";
        let mut cursor = Cursor::new(0, buf.len());
        let value = parse_value(buf, &mut cursor, Delimiters::of(b";")).unwrap();
        assert_eq!(value, "a q\"x\\y b");
        assert_eq!(buf[cursor.pos], b';');
    }

    #[test]
    fn decodes_a_phrase_once() {
        let phrase = |text: &str| {
            let mut cursor = Cursor::new(0, text.len());
            parse_words(text.as_bytes(), &mut cursor, Delimiters::of(b"<"))
                .phrase()
                .unwrap()
        };
        assert_eq!(phrase("=?utf-8?q?Ren=C3=A9?= =?utf-8?q?_D?="), "René D");
        assert_eq!(phrase("a \"=?utf-8?q?b?=\" c"), "a b c");
        assert_eq!(
            phrase("\"=?utf-8?q?=3D=3Fus-ascii=3Fq=3Fx=3F=3D?=\""),
            "=?us-ascii?q?x?="
        );
        assert_eq!(phrase("\"a =?utf-8?q?b?=\""), "a =?utf-8?q?b?=");
    }
}
