//! Time-stamped data envelopes (RFC 5544), as Tika's `TSDParser` reads them: an empty document
//! into which the envelope's content is parsed as an embedded document, read as Bouncy Castle's
//! `CMSTimeStampedDataParser` streams it, up to where the input ends.
//!
//! `TSDParser` also reads the time stamps, but writes them into the metadata of the embedded
//! document, which neither the document nor its spooled copy shows, so the port does not read
//! them. An envelope Bouncy Castle cannot read its content from leaves the document empty, as
//! `TSDParser` logs the failure and goes on.

use crate::config::ParserConfigs;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Parser, Prepared};

/// 1.2.840.113549.1.9.16.1.31, `id-ct-timestampedData`.
const TIMESTAMPED_DATA: &[u8] = &[
    0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x10, 0x01, 0x1F,
];
const UNIVERSAL_INTEGER: u32 = 2;
const UNIVERSAL_OCTET_STRING: u32 = 4;
const UNIVERSAL_OBJECT_IDENTIFIER: u32 = 6;
const UNIVERSAL_SEQUENCE: u32 = 16;
const UNIVERSAL_IA5_STRING: u32 = 22;
const CLASS_UNIVERSAL: u8 = 0x00;
const CLASS_CONTEXT: u8 = 0x80;
/// The bytes `parseTSDContent` copies the content in at a time.
const COPY_BUFFER: usize = 8192;

/// A BER length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Length {
    Definite(usize),
    Indefinite,
}

/// An element's header, as `ASN1StreamParser` reads it.
#[derive(Debug, Clone, Copy)]
struct Header {
    class: u8,
    constructed: bool,
    number: u32,
    length: Length,
}

impl Header {
    fn is(&self, class: u8, number: u32) -> bool {
        self.class == class && self.number == number
    }
}

/// What reading the envelope met that Bouncy Castle throws on.
struct Thrown;

/// The envelope read front to back, as `ASN1StreamParser` streams it: a definite length is not
/// checked against what is left until the bytes it covers are read, and each open element of
/// indefinite length reads two bytes ahead of what it hands on (`IndefiniteLengthInputStream`
/// looks for the end-of-contents marker so), each of which the input must hold.
struct Stream<'a> {
    bytes: &'a [u8],
    at: usize,
    /// The elements of indefinite length open around the position.
    indefinite: usize,
}

impl<'a> Stream<'a> {
    /// Where the bytes the position's elements can hand on end: two bytes before the input
    /// ends for each element of indefinite length open around it.
    fn limit(&self) -> usize {
        self.bytes.len().saturating_sub(2 * self.indefinite)
    }

    fn byte(&mut self) -> Result<u8, Thrown> {
        if self.at >= self.limit() {
            return Err(Thrown);
        }
        let byte = self.bytes[self.at];
        self.at += 1;
        Ok(byte)
    }

    /// `ASN1StreamParser.readObject`'s header: the tag, its number in base 128 when it does not
    /// fit the first byte, and a length of at most four bytes. An element of indefinite length
    /// is opened, its two bytes read ahead.
    fn header(&mut self) -> Result<Header, Thrown> {
        let tag = self.byte()?;
        let mut number = u32::from(tag & 0x1F);
        if number == 0x1F {
            number = 0;
            loop {
                let byte = self.byte()?;
                if number == 0 && byte == 0x80 {
                    return Err(Thrown);
                }
                number = number.checked_mul(128).ok_or(Thrown)? | u32::from(byte & 0x7F);
                if byte & 0x80 == 0 {
                    break;
                }
            }
        }
        let constructed = tag & 0x20 != 0;
        let first = self.byte()?;
        let length = match first {
            0x80 if constructed => {
                if self.at + 2 > self.limit() {
                    return Err(Thrown);
                }
                self.indefinite += 1;
                Length::Indefinite
            }
            0x80 => return Err(Thrown),
            0x00..=0x7F => Length::Definite(usize::from(first)),
            0x81..=0x84 => {
                let mut length: u32 = 0;
                for _ in 0..first & 0x7F {
                    length = length << 8 | u32::from(self.byte()?);
                }
                let length = i32::try_from(length).map_err(|_| Thrown)?;
                Length::Definite(usize::try_from(length).map_err(|_| Thrown)?)
            }
            _ => return Err(Thrown),
        };
        Ok(Header {
            class: tag & 0xC0,
            constructed,
            number,
            length,
        })
    }

    /// The `length` bytes that follow, which must be there to hand on.
    fn take(&mut self, length: usize) -> Result<&'a [u8], Thrown> {
        let end = self.at.checked_add(length).ok_or(Thrown)?;
        if end > self.limit() {
            return Err(Thrown);
        }
        let bytes = &self.bytes[self.at..end];
        self.at = end;
        Ok(bytes)
    }

    /// Whether the end-of-contents marker of the innermost open element of indefinite length
    /// comes next, which closes it.
    fn end_of_contents(&mut self) -> bool {
        let ends = self.bytes.get(self.at..self.at + 2) == Some(&[0, 0]);
        if ends {
            self.at += 2;
            self.indefinite -= 1;
        }
        ends
    }

    /// Reads past the element of `header`, as `toASN1Primitive` loads it.
    fn skip(&mut self, header: Header) -> Result<(), Thrown> {
        match header.length {
            Length::Definite(length) => {
                self.take(length)?;
            }
            Length::Indefinite => {
                while !self.end_of_contents() {
                    let inner = self.header()?;
                    self.skip(inner)?;
                }
            }
        }
        Ok(())
    }

    /// Adds the octets of the OCTET STRING of `header`, primitive or constructed, to `content`,
    /// as its octet stream hands them on, up to where it throws. Whether it was read whole.
    fn octets(&mut self, header: Header, content: &mut Vec<u8>) -> bool {
        match (header.constructed, header.length) {
            (false, Length::Definite(length)) => {
                let end = self.at.saturating_add(length).min(self.limit());
                content.extend_from_slice(&self.bytes[self.at..end]);
                let whole = end - self.at == length;
                self.at = end;
                whole
            }
            (false, Length::Indefinite) => false,
            (true, length) => {
                let start = self.at;
                loop {
                    let more = match length {
                        Length::Definite(length) => self.at < start.saturating_add(length),
                        Length::Indefinite => !self.end_of_contents(),
                    };
                    if !more {
                        return true;
                    }
                    let Ok(inner) = self.header() else {
                        return false;
                    };
                    if !inner.is(CLASS_UNIVERSAL, UNIVERSAL_OCTET_STRING)
                        || !self.octets(inner, content)
                    {
                        return false;
                    }
                }
            }
        }
    }
}

/// `CMSTimeStampedDataParser(stream).getContent()` as `parseTSDContent` copies it: the
/// `ContentInfo` of time-stamped data, the version of its `TimeStampedData`, its data URI and
/// metadata if it has them, then the octets of its content. Bouncy Castle's octet stream fills
/// each read whole, and the read its stream throws in loses what it read: the copy keeps the
/// whole buffers before it. `None` where Bouncy Castle throws before the content, or the
/// envelope has none.
fn content(input: &[u8]) -> Option<Vec<u8>> {
    let mut stream = Stream {
        bytes: input,
        at: 0,
        indefinite: 0,
    };
    let mut read = || -> Result<Option<Vec<u8>>, Thrown> {
        let info = stream.header()?;
        if !info.is(CLASS_UNIVERSAL, UNIVERSAL_SEQUENCE) || !info.constructed {
            return Err(Thrown);
        }
        let kind = stream.header()?;
        if !kind.is(CLASS_UNIVERSAL, UNIVERSAL_OBJECT_IDENTIFIER) {
            return Err(Thrown);
        }
        let Length::Definite(length) = kind.length else {
            return Err(Thrown);
        };
        if stream.take(length)? != TIMESTAMPED_DATA {
            return Err(Thrown);
        }
        let explicit = stream.header()?;
        if !explicit.is(CLASS_CONTEXT, 0) || !explicit.constructed {
            return Err(Thrown);
        }
        let data = stream.header()?;
        if !data.is(CLASS_UNIVERSAL, UNIVERSAL_SEQUENCE) || !data.constructed {
            return Err(Thrown);
        }
        let version = stream.header()?;
        if !version.is(CLASS_UNIVERSAL, UNIVERSAL_INTEGER) || version.constructed {
            return Err(Thrown);
        }
        stream.skip(version)?;
        let mut next = stream.header()?;
        if next.is(CLASS_UNIVERSAL, UNIVERSAL_IA5_STRING) {
            stream.skip(next)?;
            next = stream.header()?;
        }
        if next.is(CLASS_UNIVERSAL, UNIVERSAL_SEQUENCE) {
            stream.skip(next)?;
            next = stream.header()?;
        }
        if !next.is(CLASS_UNIVERSAL, UNIVERSAL_OCTET_STRING) {
            return Ok(None);
        }
        let mut content = Vec::new();
        if !stream.octets(next, &mut content) {
            content.truncate(content.len() - content.len() % COPY_BUFFER);
        }
        Ok(Some(content))
    };
    read().ok().flatten()
}

/// Tika's `TSDParser`.
pub(crate) struct TsdParser;

impl Parser for TsdParser {
    fn name(&self) -> &'static str {
        "extractous::TSDParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.crypto.TSDParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/timestamped-data"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _: &TikaMetadata,
        _: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let content = content(input);
        Ok(Box::new(move |xhtml| {
            if let Some(content) = content {
                xhtml.parse_embedded(&content, &TikaMetadata::new(), Wrapping::PackageEntry)?;
            }
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};

    /// A time-stamped data envelope of `content`, every length indefinite, with no time stamps.
    fn envelope(content: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x30, 0x80, 0x06, 0x0B];
        bytes.extend_from_slice(TIMESTAMPED_DATA);
        bytes.extend_from_slice(&[0xA0, 0x80, 0x30, 0x80, 0x02, 0x01, 0x01, 0x24, 0x80]);
        bytes.push(0x04);
        bytes.push(u8::try_from(content.len()).unwrap());
        bytes.extend_from_slice(content);
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }

    #[test]
    fn loses_the_read_a_cut_envelope_throws_in() {
        let whole = envelope(b"hello");
        assert_eq!(content(&whole).unwrap(), b"hello");
        assert_eq!(content(&whole[..whole.len() - 1]).unwrap(), b"");
        assert_eq!(content(&whole[..10]), None);
        assert_eq!(content(b"not ASN.1"), None);
    }

    #[test]
    fn extracts_synthetic_envelopes_as_tikas_path_does() {
        let scratch = Scratch::new("tsd");
        let whole = envelope(b"Some text in an envelope.");
        let documents_written = [
            scratch.write("text.tsd", &whole),
            scratch.write("cut.tsd", &whole[..whole.len() - 12]),
            scratch.write("empty.tsd", &envelope(b"")),
            scratch.write("none.tsd", &whole[..30]),
        ];
        let paths: Vec<&Path> = documents_written.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TsdParser.tika_name());
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_envelopes_as_tikas_path_does() {
        let (parsed, deferred) = documents(&TsdParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 7, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TsdParser.tika_name());
    }
}
