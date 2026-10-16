//! Header fields as mime4j 0.8.15 reads them: `RawFieldParser`'s tokens, values, comments and
//! quoted strings over a field's bytes, and `RawField`, a field's name and unfolded body.

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

/// `RawFieldParser.parseToken`: the text up to a delimiter, runs of white space between words
/// collapsed to one space and comments left out.
pub(super) fn parse_token(buf: &[u8], cursor: &mut Cursor, delimiters: Delimiters) -> String {
    let mut dst = String::new();
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
            if !dst.is_empty() && whitespace {
                dst.push(' ');
            }
            copy_content(buf, cursor, delimiters, &mut dst, false);
            whitespace = false;
        }
    }
    dst
}

/// `RawFieldParser.parseValue`: as [`parse_token`], with quoted strings read as their content.
pub(super) fn parse_value(
    buf: &[u8],
    cursor: &mut Cursor,
    delimiters: Delimiters,
) -> Result<String, Unported> {
    let mut dst = String::new();
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
        } else if current == b'"' {
            if !dst.is_empty() && whitespace {
                dst.push(' ');
            }
            copy_quoted_content(buf, cursor, &mut dst)?;
            whitespace = false;
        } else {
            if !dst.is_empty() && whitespace {
                dst.push(' ');
            }
            copy_content(buf, cursor, delimiters, &mut dst, true);
            whitespace = false;
        }
    }
    Ok(dst)
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
/// backslash before a character other than a quote or a backslash, line breaks left out, and
/// encoded words decoded when the content starts with one.
pub(super) fn copy_quoted_content(
    buf: &[u8],
    cursor: &mut Cursor,
    dst: &mut String,
) -> Result<(), Unported> {
    if cursor.at_end() || buf[cursor.pos] != b'"' {
        return Ok(());
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
    let decoded = java::utf8(&raw);
    if decoded.starts_with("=?") {
        dst.push_str(&decode_encoded_words(&decoded)?);
    } else {
        dst.push_str(&decoded);
    }
    cursor.pos = pos;
    Ok(())
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
    /// `DefaultFieldBuilder.build` of the bytes of a field, its folded lines included.
    pub(super) fn parse(mut raw: Vec<u8>) -> Result<RawField, InvalidField> {
        if raw.last() == Some(&b'\n') {
            raw.pop();
        }
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        let mut cursor = Cursor::new(0, raw.len());
        let name = parse_token(&raw, &mut cursor, Delimiters::of(b":"));
        if cursor.at_end() {
            return Err(InvalidField::NoColon);
        }
        let name = crate::java::trim(&name).to_owned();
        if !name.chars().all(is_field_char) {
            return Err(InvalidField::IllegalName);
        }
        Ok(RawField {
            raw,
            delimiter: cursor.pos,
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
        assert_eq!(commented.name(), "XY");
        assert_eq!(commented.body(), "v");
        assert_eq!(
            RawField::parse(b"From someone Mon".to_vec()),
            Err(InvalidField::NoColon)
        );
        assert_eq!(
            RawField::parse(b"From someone: x".to_vec()),
            Err(InvalidField::IllegalName)
        );
        assert_eq!(RawField::parse(b"\x01To: x".to_vec()).unwrap().name(), "To");
    }

    #[test]
    fn reads_values_as_raw_field_parser_does() {
        let buf = b" a  (note (nested)) \"q\\\"x\\y\" b;rest";
        let mut cursor = Cursor::new(0, buf.len());
        let value = parse_value(buf, &mut cursor, Delimiters::of(b";")).unwrap();
        assert_eq!(value, "a q\"x\\y b");
        assert_eq!(buf[cursor.pos], b';');
    }
}
