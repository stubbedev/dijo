//! mime4j's decoders: `Base64InputStream` and `QuotedPrintableInputStream` with a silent
//! monitor, `DecoderUtil`'s encoded words, and the `URLDecoder` its parameter mapping uses.

use super::Unported;
use crate::text::charset::Charset;
use crate::unicode::is_whitespace;

const BASE64_PAD: u8 = b'=';
const CR: u8 = b'\r';
const LF: u8 = b'\n';
const EQ: u8 = b'=';

/// `new String(bytes, charset)`, or the charset when the port does not decode it as the JDK
/// does.
pub(super) fn decode(charset: Charset, bytes: &[u8]) -> Result<String, Unported> {
    charset.decode_exactly(bytes).ok_or(Unported::Charset)
}

/// `CharsetUtil.lookup`: the JDK charset of a name or alias.
pub(super) fn lookup(name: &str) -> Option<Charset> {
    Charset::for_name(name)
}

fn base64_value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// What `Base64InputStream` reads from `input`: every byte outside the alphabet skipped, and
/// everything from the first pad on dropped, with one or two bytes from the sextets before it;
/// sextets that end the input short of a quantum are dropped.
pub(super) fn decode_base64(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut data = 0u32;
    let mut sextets = 0;
    for &byte in input {
        if byte == BASE64_PAD {
            match sextets {
                2 => out.push((data >> 4) as u8),
                3 => {
                    out.push((data >> 10) as u8);
                    out.push((data >> 2) as u8);
                }
                _ => {}
            }
            return out;
        }
        let Some(value) = base64_value(byte) else {
            continue;
        };
        data = (data << 6) | value;
        sextets += 1;
        if sextets == 4 {
            out.extend_from_slice(&[(data >> 16) as u8, (data >> 8) as u8, data as u8]);
            data = 0;
            sextets = 0;
        }
    }
    out
}

fn hex_value(byte: Option<u8>) -> Option<u8> {
    match byte? {
        c @ b'0'..=b'9' => Some(c - b'0'),
        c @ b'A'..=b'F' => Some(c - b'A' + 10),
        c @ b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// The size of `QuotedPrintableInputStream`'s buffer of encoded bytes: the recycled buffers it
/// gets are at least this large.
const QP_BUFFER_SIZE: usize = 4000;

/// `QuotedPrintableInputStream` with a silent monitor: every line break a CRLF, white space
/// before a line break or the end dropped, soft line breaks joined, and malformed escapes kept
/// as they are. It looks ahead within its buffer of encoded bytes only, so what it makes of
/// `==` before a line break depends on where its reads from the source end.
pub(super) struct QuotedPrintableReader {
    encoded: Vec<u8>,
    pos: usize,
    limit: usize,
    last_was_cr: bool,
    /// Decoded bytes a read had no room for.
    decoded: Vec<u8>,
    blanks: Vec<u8>,
}

/// Where a read puts what it decodes: the caller's buffer until it is full, then the
/// reader's own.
struct Output<'a> {
    buffer: &'a mut [u8],
    index: usize,
    overflow: &'a mut Vec<u8>,
}

impl Output<'_> {
    fn push(&mut self, byte: u8) {
        if self.index < self.buffer.len() {
            self.buffer[self.index] = byte;
            self.index += 1;
        } else {
            self.overflow.push(byte);
        }
    }
}

impl QuotedPrintableReader {
    pub(super) fn new() -> Self {
        QuotedPrintableReader {
            encoded: vec![0; QP_BUFFER_SIZE],
            pos: 0,
            limit: 0,
            last_was_cr: false,
            decoded: Vec::new(),
            blanks: Vec::new(),
        }
    }

    /// `fillBuffer`: compacts the buffer and reads once into its room, `None` at the end.
    fn fill(&mut self, source: &mut dyn FnMut(&mut [u8]) -> Option<usize>) -> Option<usize> {
        if self.pos < self.limit {
            self.encoded.copy_within(self.pos..self.limit, 0);
            self.limit -= self.pos;
        } else {
            self.limit = 0;
        }
        self.pos = 0;
        if self.limit == self.encoded.len() {
            return Some(0);
        }
        let read = source(&mut self.encoded[self.limit..])?;
        self.limit += read;
        Some(read)
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        (self.pos + offset < self.limit).then(|| self.encoded[self.pos + offset])
    }

    fn next_byte(&mut self) -> Option<u8> {
        let byte = self.peek(0)?;
        self.pos += 1;
        Some(byte)
    }

    /// `transfer`: the pending white space first when `keep_blanks`, then the byte.
    fn transfer(&mut self, out: &mut Output, byte: Option<u8>, keep_blanks: bool) {
        if keep_blanks {
            for blank in std::mem::take(&mut self.blanks) {
                out.push(blank);
            }
        }
        if let Some(byte) = byte {
            out.push(byte);
        }
    }

    /// `read(b, off, len)`: decodes into `buffer`, `None` at the end.
    pub(super) fn read(
        &mut self,
        buffer: &mut [u8],
        source: &mut dyn FnMut(&mut [u8]) -> Option<usize>,
    ) -> Option<usize> {
        let to = buffer.len();
        let mut leftover = std::mem::take(&mut self.decoded);
        let chunk = leftover.len().min(to);
        buffer[..chunk].copy_from_slice(&leftover[..chunk]);
        leftover.drain(..chunk);
        let mut out = Output {
            buffer,
            index: chunk,
            overflow: &mut leftover,
        };
        let mut eof = false;
        while out.index < to {
            if self.limit - self.pos < 3 {
                eof = self.fill(source).is_none();
            }
            if self.limit == self.pos && eof {
                let read = out.index;
                self.decoded = leftover;
                return (read > 0).then_some(read);
            }
            while self.pos < self.limit && out.index < to {
                let b = self.encoded[self.pos];
                self.pos += 1;
                if self.last_was_cr && b != LF {
                    self.transfer(&mut out, Some(CR), false);
                }
                if b == CR {
                    self.last_was_cr = true;
                    continue;
                }
                self.last_was_cr = false;
                if b == LF {
                    if self.blanks.first() != Some(&EQ) {
                        self.transfer(&mut out, Some(CR), false);
                        self.transfer(&mut out, Some(LF), false);
                    }
                    self.blanks.clear();
                } else if b == EQ {
                    if self.limit - self.pos < 2 && !eof {
                        self.pos -= 1;
                        break;
                    }
                    self.escape(&mut out);
                } else if is_whitespace(char::from(b)) {
                    self.blanks.push(b);
                } else {
                    self.transfer(&mut out, Some(b), true);
                }
            }
        }
        self.decoded = leftover;
        Some(to)
    }

    /// What follows an `=`: a soft line break, an escaped byte, or bytes kept as they are.
    fn escape(&mut self, out: &mut Output) {
        let b2 = self.next_byte();
        match b2 {
            Some(EQ) => {
                self.transfer(out, Some(EQ), true);
                let (bb1, bb2) = (self.peek(0), self.peek(1));
                if bb1 == Some(LF) || (bb1 == Some(CR) && bb2 == Some(LF)) {
                    self.blanks.push(EQ);
                }
            }
            Some(b2) if is_whitespace(char::from(b2)) => {
                if self.peek(0) == Some(LF) {
                    self.last_was_cr = b2 == CR;
                }
                self.transfer(out, None, true);
                if b2 != LF {
                    self.blanks.extend_from_slice(&[EQ, b2]);
                }
            }
            _ => {
                let b3 = self.next_byte();
                match (hex_value(b2), hex_value(b3)) {
                    (Some(upper), Some(lower)) => {
                        self.transfer(out, Some((upper << 4) | lower), true)
                    }
                    _ => {
                        self.transfer(out, Some(EQ), true);
                        self.transfer(out, b2, false);
                        self.transfer(out, b3, false);
                    }
                }
            }
        }
    }
}

/// What `DecoderUtil` decodes from quoted-printable text: the bytes a
/// `QuotedPrintableInputStream` reads one at a time from the whole text.
pub(super) fn decode_quoted_printable(input: &[u8]) -> Vec<u8> {
    let mut reader = QuotedPrintableReader::new();
    let mut position = 0;
    let mut source = |into: &mut [u8]| {
        if position >= input.len() {
            return None;
        }
        let count = into.len().min(input.len() - position);
        into[..count].copy_from_slice(&input[position..position + count]);
        position += count;
        Some(count)
    };
    let mut out = Vec::with_capacity(input.len());
    let mut byte = [0u8];
    while reader.read(&mut byte, &mut source).is_some() {
        out.push(byte[0]);
    }
    out
}

/// `DecoderUtil.decodeEncodedWords` with a silent monitor and no fallback charset: every
/// `=?charset?encoding?text?=` whose charset the JDK knows decoded, adjacent words of one
/// charset and encoding decoded together, and white space between encoded words dropped.
pub(super) fn decode_encoded_words(body: &str) -> Result<String, Unported> {
    let mut words = EncodedWords {
        out: Vec::new(),
        text: String::new(),
        charset: String::new(),
    };
    let mut encoding = String::new();
    let mut position = 0;
    while position < body.len() {
        let Some(start) = find(body, "=?", position) else {
            if position == 0 {
                return Ok(body.to_owned());
            }
            words.flush()?;
            words.text.push_str(&body[position..]);
            break;
        };
        let charset_end = find(body, "?", start + 2);
        let encoding_end = charset_end.and_then(|end| find(body, "?", end + 1));
        let text_end = encoding_end.and_then(|end| find(body, "?=", end + 1));
        let (Some(charset_end), Some(encoding_end), Some(text_end)) =
            (charset_end, encoding_end, text_end)
        else {
            words.flush()?;
            words.text.push_str(&body[position..start + 2]);
            position = start + 2;
            continue;
        };
        let separator = &body[position..start];
        let blank = separator
            .bytes()
            .all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'));
        if (!blank || position == 0) && !separator.is_empty() {
            words.flush()?;
            words.text.push_str(separator);
        }
        let charset = &body[start + 2..charset_end];
        let current_encoding = &body[charset_end + 1..encoding_end];
        let encoded = &body[encoding_end + 1..text_end];
        if !words.charset.is_empty() && charset != words.charset {
            words.flush()?;
        }
        if !encoding.is_empty() && current_encoding != encoding {
            words.flush()?;
        }
        charset.clone_into(&mut words.charset);
        current_encoding.clone_into(&mut encoding);
        position = text_end + 2;
        if encoded.is_empty() {
            continue;
        }
        match decode_word(charset, current_encoding, encoded) {
            Some(bytes) => words.out.extend_from_slice(&bytes),
            None => words.text.push_str(&body[start..text_end + 2]),
        }
    }
    words.flush()?;
    Ok(words.text)
}

fn find(text: &str, pattern: &str, from: usize) -> Option<usize> {
    text.get(from..)?.find(pattern).map(|index| index + from)
}

struct EncodedWords {
    /// The decoded bytes of the words not yet turned into text.
    out: Vec<u8>,
    text: String,
    charset: String,
}

impl EncodedWords {
    /// `appendStringBuffer`: the pending bytes decoded in the charset of the last word.
    fn flush(&mut self) -> Result<(), Unported> {
        if self.out.is_empty() {
            return Ok(());
        }
        let charset = lookup(&self.charset)
            .expect("bytes are only pending for a word whose charset the JDK knows");
        self.text.push_str(&decode(charset, &self.out)?);
        self.out.clear();
        Ok(())
    }
}

/// `tryDecodeEncodedWord`: the bytes of an encoded word, or `None` when its charset or encoding
/// is unknown.
fn decode_word(charset: &str, encoding: &str, encoded: &str) -> Option<Vec<u8>> {
    lookup(charset)?;
    let ascii = |text: &str| -> Vec<u8> {
        text.chars()
            .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
            .collect()
    };
    if encoding.eq_ignore_ascii_case("Q") {
        Some(decode_quoted_printable(&ascii(
            &encoded.replace('_', "=20"),
        )))
    } else if encoding.eq_ignore_ascii_case("B") {
        Some(decode_base64(&ascii(encoded)))
    } else {
        None
    }
}

/// Why `URLDecoder.decode` throws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UrlDecodeError;

/// `URLDecoder.decode(value, charset)`: `+` a space and runs of `%XX` escapes decoded as bytes
/// of `charset`. A hexadecimal pair may carry a sign, as `Integer.parseInt` reads it.
pub(super) fn url_decode(
    value: &str,
    charset: &str,
) -> Result<Result<String, UrlDecodeError>, Unported> {
    let Some(charset) = (!charset.is_empty()).then(|| lookup(charset)).flatten() else {
        return Ok(Err(UrlDecodeError));
    };
    let chars: Vec<char> = value.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '+' => {
                out.push(' ');
                i += 1;
            }
            '%' => {
                let mut bytes = Vec::new();
                while i + 2 < chars.len() && chars[i] == '%' {
                    let Some(byte) = parse_hex_pair(chars[i + 1], chars[i + 2]) else {
                        return Ok(Err(UrlDecodeError));
                    };
                    bytes.push(byte);
                    i += 3;
                }
                if i < chars.len() && chars[i] == '%' {
                    return Ok(Err(UrlDecodeError));
                }
                out.push_str(&decode(charset, &bytes)?);
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(Ok(out))
}

/// `Integer.parseInt` of two characters in base 16, as a byte, or `None` for what it rejects
/// or a negative value.
fn parse_hex_pair(first: char, second: char) -> Option<u8> {
    let digit = |c: char| c.to_digit(16);
    match first {
        '+' => digit(second).map(|d| d as u8),
        '-' => match digit(second)? {
            0 => Some(0),
            _ => None,
        },
        _ => Some((digit(first)? * 16 + digit(second)?) as u8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64_as_mime4j_does() {
        assert_eq!(decode_base64(b"aGVs\r\nbG8="), b"hello");
        assert_eq!(decode_base64(b"aGVsbG8=IGlnbm9yZWQ="), b"hello");
        assert_eq!(decode_base64(b"aGVsbA"), b"hel");
        assert_eq!(decode_base64(b"a*G=Vs"), b"h");
    }

    #[test]
    fn decodes_quoted_printable_as_mime4j_does() {
        assert_eq!(decode_quoted_printable(b"a=3Db \nc"), b"a=b\r\nc");
        assert_eq!(decode_quoted_printable(b"soft=\r\nbreak  "), b"softbreak");
        assert_eq!(decode_quoted_printable(b"x=4"), b"x=4");
        assert_eq!(decode_quoted_printable(b"x=\n= y"), b"x= y");
        assert_eq!(decode_quoted_printable(b"a\rb"), b"a\rb");
        assert_eq!(decode_quoted_printable(b"a==\r\nb"), b"a=b");
    }

    #[test]
    fn decodes_encoded_words_as_decoder_util_does() {
        let decode = |text: &str| decode_encoded_words(text).unwrap();
        assert_eq!(decode("plain"), "plain");
        assert_eq!(decode("=?utf-8?q?caf=C3=A9?= =?utf-8?b?w6k=?="), "caféé");
        assert_eq!(decode("a =?iso-8859-1?q?=E9?= b"), "a é b");
        assert_eq!(decode("=?utf-8?q?=C3?= =?utf-8?q?=A9?="), "é");
        assert_eq!(decode("=?x-unknown?q?a?= z"), "=?x-unknown?q?a?= z");
        assert_eq!(decode("=?utf-8?x?a?="), "=?utf-8?x?a?=");
        assert_eq!(decode("broken =?utf-8 x"), "broken =?utf-8 x");
    }

    #[test]
    fn url_decodes_as_java_does() {
        let decode = |text: &str| url_decode(text, "UTF-8").unwrap();
        assert_eq!(decode("a+b%C3%A9"), Ok("a bé".to_owned()));
        assert_eq!(decode("%+1%41"), Ok("\u{1}A".to_owned()));
        assert_eq!(decode("%4"), Err(UrlDecodeError));
        assert_eq!(decode("%-1"), Err(UrlDecodeError));
        assert_eq!(
            url_decode("x", "no-such-charset").unwrap(),
            Err(UrlDecodeError)
        );
    }
}
