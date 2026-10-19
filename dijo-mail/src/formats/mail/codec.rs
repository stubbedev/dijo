//! The decoders of mail: base64 and quoted-printable bodies, `DecoderUtil`'s encoded words,
//! and the percent-encoded values of RFC 2231 parameters.

use super::Unported;
use crate::text::charset::Charset;

const BASE64_PAD: u8 = b'=';
const CR: u8 = b'\r';
const LF: u8 = b'\n';
const EQ: u8 = b'=';

/// The text of `bytes` in `charset`, or the charset when the port does not decode it.
pub(super) fn decode(charset: Charset, bytes: &[u8]) -> Result<String, Unported> {
    charset.decode(bytes).ok_or(Unported::Charset)
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

/// A body's transfer encoding, decoded a chunk of the body at a time, as [`decode_base64`]
/// decodes a whole one.
#[derive(Debug)]
pub(super) enum BodyDecoder {
    /// Any encoding but these two, which the body is read as it is.
    Identity,
    /// Base64, with the sextets of the quantum the chunks so far end in.
    Base64 { data: u32, sextets: u32 },
    /// Quoted-printable, with the line the chunks so far end in, decoded a line at a time as
    /// RFC 2045 says: `=XX` an escaped byte (in either case), `=` ending a line a soft line
    /// break, the space and tab before a line break or the end dropped as transport padding,
    /// every line break kept as it is, and a malformed escape kept as it is. mime4j's
    /// `QuotedPrintableInputStream` makes every line break a CRLF, reads `= x` as a soft line
    /// break that keeps `= `, and decides `==` before a line break by looking ahead within its
    /// buffer alone (#73).
    QuotedPrintable { line: Vec<u8> },
}

impl BodyDecoder {
    /// The decoder of the transfer encoding `encoding`.
    pub(super) fn of(encoding: &str) -> Self {
        if encoding.eq_ignore_ascii_case("base64") {
            BodyDecoder::Base64 {
                data: 0,
                sextets: 0,
            }
        } else if encoding.eq_ignore_ascii_case("quoted-printable") {
            BodyDecoder::QuotedPrintable { line: Vec::new() }
        } else {
            BodyDecoder::Identity
        }
    }

    /// About the most bytes a body of `encoded` bytes decodes into.
    pub(super) fn decoded_length(&self, encoded: u64) -> u64 {
        match self {
            BodyDecoder::Base64 { .. } => encoded / 4 * 3,
            BodyDecoder::Identity | BodyDecoder::QuotedPrintable { .. } => encoded,
        }
    }

    /// Decodes the next `chunk` of the body into `out`.
    pub(super) fn feed(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        match self {
            BodyDecoder::Identity => out.extend_from_slice(chunk),
            BodyDecoder::Base64 { data, sextets } => {
                for &byte in chunk {
                    if byte == BASE64_PAD {
                        flush_base64(out, *data, *sextets);
                        *data = 0;
                        *sextets = 0;
                        continue;
                    }
                    let Some(value) = base64_value(byte) else {
                        continue;
                    };
                    *data = (*data << 6) | value;
                    *sextets += 1;
                    if *sextets == 4 {
                        out.extend_from_slice(&[
                            (*data >> 16) as u8,
                            (*data >> 8) as u8,
                            *data as u8,
                        ]);
                        *data = 0;
                        *sextets = 0;
                    }
                }
            }
            BodyDecoder::QuotedPrintable { line } => {
                for piece in chunk.split_inclusive(|&byte| byte == LF) {
                    line.extend_from_slice(piece);
                    if piece.ends_with(&[LF]) {
                        decode_quoted_printable_line(line, out);
                        line.clear();
                    }
                }
            }
        }
    }

    /// Decodes into `out` what the body ends in.
    pub(super) fn finish(self, out: &mut Vec<u8>) {
        match self {
            BodyDecoder::Identity => {}
            BodyDecoder::Base64 { data, sextets } => flush_base64(out, data, sextets),
            BodyDecoder::QuotedPrintable { line } => decode_quoted_printable_line(&line, out),
        }
    }
}

/// The bytes of a base64 quantum of `sextets` sextets in `data` that ends early.
fn flush_base64(out: &mut Vec<u8>, data: u32, sextets: u32) {
    match sextets {
        2 => out.push((data >> 4) as u8),
        3 => {
            out.push((data >> 10) as u8);
            out.push((data >> 2) as u8);
        }
        _ => {}
    }
}

/// `input` decoded whole by `decoder`.
fn decode_whole(mut decoder: BodyDecoder, input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    decoder.feed(input, &mut out);
    decoder.finish(&mut out);
    out
}

/// Base64 decoded leniently: every byte outside the alphabet skipped, a pad ending the quantum
/// before it, so that concatenated blocks all decode, and the sextets of a short last quantum
/// decoded as far as they go. mime4j's `Base64InputStream` drops everything from the first pad
/// on and a short last quantum (#72).
pub(super) fn decode_base64(input: &[u8]) -> Vec<u8> {
    decode_whole(BodyDecoder::of("base64"), input)
}

/// An encoded word's base64 decoded as [`decode_base64`] decodes it, up to the first pad: an
/// encoded word holds one block, so what follows its padding is not text.
fn decode_base64_word(input: &[u8]) -> Vec<u8> {
    let end = input
        .iter()
        .position(|&byte| byte == BASE64_PAD)
        .unwrap_or(input.len());
    decode_base64(&input[..end])
}

fn hex_value(byte: Option<u8>) -> Option<u8> {
    match byte? {
        c @ b'0'..=b'9' => Some(c - b'0'),
        c @ b'A'..=b'F' => Some(c - b'A' + 10),
        c @ b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// A line of quoted-printable text, with the line break that ends it if any, decoded into
/// `out`.
fn decode_quoted_printable_line(line: &[u8], out: &mut Vec<u8>) {
    let (content, line_break) = match line {
        [content @ .., CR, LF] => (content, &line[line.len() - 2..]),
        [content @ .., LF] => (content, &line[line.len() - 1..]),
        content => (content, &[][..]),
    };
    let end = content
        .iter()
        .rposition(|&byte| byte != b' ' && byte != b'\t')
        .map_or(0, |last| last + 1);
    let content = &content[..end];
    match content.strip_suffix(&[EQ]) {
        Some(soft) => decode_escapes(soft, out),
        None => {
            decode_escapes(content, out);
            out.extend_from_slice(line_break);
        }
    }
}

/// The escapes of a line of quoted-printable text decoded, an `=` not followed by two
/// hexadecimal digits kept as it is.
fn decode_escapes(line: &[u8], out: &mut Vec<u8>) {
    let mut at = 0;
    while at < line.len() {
        let byte = line[at];
        if byte == EQ {
            if let (Some(upper), Some(lower)) = (
                hex_value(line.get(at + 1).copied()),
                hex_value(line.get(at + 2).copied()),
            ) {
                out.push((upper << 4) | lower);
                at += 3;
                continue;
            }
        }
        out.push(byte);
        at += 1;
    }
}

/// `DecoderUtil.decodeEncodedWords` with a silent monitor and no fallback charset: every
/// `=?charset?encoding?text?=` whose charset the JDK knows decoded, adjacent words of one
/// charset and encoding decoded together, and white space between encoded words dropped.
pub fn decode_encoded_words(body: &str) -> Result<String, Unported> {
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
            None => {
                // DecoderUtil writes the word before the bytes still pending (#74).
                words.flush()?;
                words.text.push_str(&body[start..text_end + 2]);
            }
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
        // An encoded word has no line breaks, so a `=` that ends it is kept, as malformed.
        let mut out = Vec::with_capacity(encoded.len());
        decode_escapes(&ascii(&encoded.replace('_', "=20")), &mut out);
        Some(out)
    } else if encoding.eq_ignore_ascii_case("B") {
        Some(decode_base64_word(&ascii(encoded)))
    } else {
        None
    }
}

/// Why a percent-encoded value does not decode: an unknown charset or a malformed escape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UrlDecodeError;

/// An RFC 2231 extended value's runs of `%XX` escapes decoded as bytes of `charset`, every
/// other character, `+` included, kept as it is. mime4j decodes with `URLDecoder`, which makes
/// `+` a space and reads a sign in an escape (#76).
pub(super) fn percent_decode(
    value: &str,
    charset: &str,
) -> Result<Result<String, UrlDecodeError>, Unported> {
    let Some(charset) = lookup(charset) else {
        return Ok(Err(UrlDecodeError));
    };
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let start = value.len() - rest.len() + at;
        let mut end = start;
        let mut decoded = Vec::new();
        while bytes.get(end) == Some(&b'%') {
            let (Some(high), Some(low)) = (
                hex_value(bytes.get(end + 1).copied()),
                hex_value(bytes.get(end + 2).copied()),
            ) else {
                return Ok(Err(UrlDecodeError));
            };
            decoded.push(high * 16 + low);
            end += 3;
        }
        out.push_str(&decode(charset, &decoded)?);
        rest = &value[end..];
    }
    out.push_str(rest);
    Ok(Ok(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_quoted_printable(input: &[u8]) -> Vec<u8> {
        decode_whole(BodyDecoder::of("quoted-printable"), input)
    }

    #[test]
    fn decodes_bodies_a_chunk_at_a_time_as_whole() {
        let bodies: [(&str, &[u8]); 3] = [
            ("base64", b"aGVs\r\nbG8=IGlnbm9yZWQ=aGk"),
            (
                "quoted-printable",
                b"a=3Db \r\nsoft=\r\nbreak  \nx=4\r\ntail=",
            ),
            ("7bit", b"plain\r\ntext"),
        ];
        for (encoding, body) in bodies {
            let whole = decode_whole(BodyDecoder::of(encoding), body);
            for size in 1..body.len() {
                let mut decoder = BodyDecoder::of(encoding);
                let mut out = Vec::new();
                for chunk in body.chunks(size) {
                    decoder.feed(chunk, &mut out);
                }
                decoder.finish(&mut out);
                assert_eq!(out, whole, "{encoding} in chunks of {size}");
            }
        }
    }

    #[test]
    fn decodes_base64_leniently() {
        assert_eq!(decode_base64(b"aGVs\r\nbG8="), b"hello");
        assert_eq!(decode_base64(b"aGVsbG8=IGlnbm9yZWQ="), b"hello ignored");
        assert_eq!(decode_base64(b"aGVsbA"), b"hell");
        assert_eq!(decode_base64(b"a*G=Vs"), b"hV");
        assert_eq!(decode_base64(b"aGk=\r\naGk="), b"hihi");
    }

    #[test]
    fn decodes_quoted_printable_as_rfc_2045_says() {
        assert_eq!(decode_quoted_printable(b"a=3Db \nc"), b"a=b\nc");
        assert_eq!(decode_quoted_printable(b"a=3db\r\nc\r\n"), b"a=b\r\nc\r\n");
        assert_eq!(decode_quoted_printable(b"soft=\r\nbreak  "), b"softbreak");
        assert_eq!(decode_quoted_printable(b"soft= \t\nbreak"), b"softbreak");
        assert_eq!(decode_quoted_printable(b"x=4"), b"x=4");
        assert_eq!(decode_quoted_printable(b"x=4\r\ny"), b"x=4\r\ny");
        assert_eq!(decode_quoted_printable(b"x=ZZ"), b"x=ZZ");
        assert_eq!(decode_quoted_printable(b"x=\n= y"), b"x= y");
        assert_eq!(decode_quoted_printable(b"a\rb"), b"a\rb");
        assert_eq!(decode_quoted_printable(b"a==\r\nb"), b"a=b");
        assert_eq!(decode_quoted_printable(b"a==3D\nb"), b"a==\nb");
        assert_eq!(decode_quoted_printable(b"tail="), b"tail");
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
        assert_eq!(decode("=?utf-8?b?w6k?="), "é");
        assert_eq!(decode("=?utf-8?b?w6k=extra?="), "é");
    }

    #[test]
    fn percent_decodes_extended_values() {
        let decode = |text: &str| percent_decode(text, "UTF-8").unwrap();
        assert_eq!(decode("a+b%C3%A9"), Ok("a+bé".to_owned()));
        assert_eq!(decode("%4"), Err(UrlDecodeError));
        assert_eq!(decode("%+1"), Err(UrlDecodeError));
        assert_eq!(
            percent_decode("x", "no-such-charset").unwrap(),
            Err(UrlDecodeError)
        );
    }
}
