//! Mobipocket and Kindle books (MOBI, AZW, AZW3) without DRM, as our Java `MobiParser` reads
//! them: the text records of the Palm database, decompressed and handed with their charset to
//! the HTML parser, which writes the document; then the title, authors and language of the
//! book's MOBI and EXTH headers as metadata. The book is read as Java's `ByteBuffer` reads it,
//! big-endian at `int` indexes, an index outside it failing the parse.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::html::JSoupParser;
use crate::formats::java::strip;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};
use crate::text::charset::Charset;

const PALM_DATABASE_HEADER: i32 = 78;
const RECORD_COUNT: i32 = 76;
const RECORD_ENTRY: i32 = 8;
const NO_COMPRESSION: i32 = 1;
const PALMDOC_COMPRESSION: i32 = 2;
const HUFF_CDIC_COMPRESSION: i32 = 17480;
const MOBI_HEADER: i32 = 16;
const MOBI_MAGIC: i32 = 0x4D4F_4249;
const UTF_8: i32 = 65001;
const EXTRA_FLAGS_MIN_HEADER_LENGTH: i32 = 0xE4;
const EXTRA_FLAGS: i32 = 0xF2;
const FULL_NAME_OFFSET: i32 = 0x54;
const FULL_NAME_LENGTH: i32 = 0x58;
const EXTH_FLAGS: i32 = MOBI_HEADER + 0x70;
const EXTH_PRESENT: i32 = 0x40;
const EXTH_MAGIC: i32 = 0x4558_5448;
const EXTH_AUTHOR: i32 = 100;
const EXTH_TITLE: i32 = 503;
const EXTH_LANGUAGE: i32 = 524;
const MAX_TEXT_BYTES: usize = 256 << 20;
/// The most a PalmDOC byte decompresses to: a two-byte back copy of ten.
const MAX_EXPANSION: usize = 5;

fn broken(reason: impl Into<String>) -> Error {
    Error::ParseError(reason.into())
}

/// The book as Java's `ByteBuffer` reads it: big-endian, at `int` indexes, an index outside it
/// an `IndexOutOfBoundsException`.
#[derive(Clone, Copy)]
struct Book<'a> {
    bytes: &'a [u8],
    limit: i32,
}

impl<'a> Book<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, Error> {
        let limit = i32::try_from(bytes.len())
            .map_err(|_| broken("The book is larger than a Java array holds"))?;
        Ok(Self { bytes, limit })
    }

    fn bytes(self, at: i32, length: i32) -> Result<&'a [u8], Error> {
        usize::try_from(at)
            .ok()
            .zip(usize::try_from(length).ok())
            .and_then(|(start, length)| self.bytes.get(start..start.checked_add(length)?))
            .ok_or_else(|| broken(format!("The book holds no {length} bytes at {at}")))
    }

    /// `getShort(at) & 0xffff`.
    fn u16_at(self, at: i32) -> Result<i32, Error> {
        let bytes = self.bytes(at, 2)?;
        Ok(i32::from(u16::from_be_bytes([bytes[0], bytes[1]])))
    }

    /// `getInt(at)`.
    fn i32_at(self, at: i32) -> Result<i32, Error> {
        let bytes = self.bytes(at, 4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

/// The book's HTML, with what the book's headers say about it.
struct Markup<'a> {
    book: Book<'a>,
    /// The offset of record 0, which holds the PalmDOC and MOBI headers.
    header: i32,
    /// The length of the MOBI header, when the book has one.
    mobi_length: Option<i32>,
    charset: Charset,
    html: Vec<u8>,
}

impl Markup<'_> {
    /// The metadata `MobiParser` hands the HTML parser.
    fn metadata(&self) -> TikaMetadata {
        let mut metadata = TikaMetadata::new();
        metadata.set(
            "Content-Type",
            format!("text/html; charset={}", self.charset.name()),
        );
        metadata
    }
}

/// `MobiParser.parse` up to the HTML: the failures it throws before any output, else the HTML.
fn read(input: &[u8]) -> Result<Markup<'_>, Error> {
    let book = Book::new(input)?;
    if book.limit < PALM_DATABASE_HEADER {
        return Err(broken("The book is shorter than a Palm database header"));
    }
    let records = book.u16_at(RECORD_COUNT)?;
    let mut offsets = (0..records)
        .map(|index| book.i32_at(PALM_DATABASE_HEADER + index * RECORD_ENTRY))
        .collect::<Result<Vec<i32>, Error>>()?;
    offsets.push(book.limit);
    if let Some(index) = offsets
        .windows(2)
        .position(|pair| pair[0] < 0 || pair[0] > pair[1])
    {
        return Err(broken(format!(
            "The book's record {index} lies outside the file"
        )));
    }
    let header = offsets[0];
    let compression = book.u16_at(header)?;
    let text_length = book.i32_at(header.wrapping_add(4))? as u32;
    let text_records = book.u16_at(header.wrapping_add(8))?;
    let encryption = book.u16_at(header.wrapping_add(12))?;
    if encryption != 0 {
        return Err(Error::Encrypted("The book is protected by DRM".to_owned()));
    }
    if compression == HUFF_CDIC_COMPRESSION {
        return Err(broken("HUFF/CDIC compressed books are not supported"));
    }
    if compression != NO_COMPRESSION && compression != PALMDOC_COMPRESSION {
        return Err(broken(format!("Unknown book compression {compression}")));
    }
    if text_records >= records || text_length as usize > MAX_TEXT_BYTES {
        return Err(broken(
            "The book's header names more text than the book holds",
        ));
    }
    let mobi = header.wrapping_add(MOBI_HEADER + 4) <= book.limit
        && book.i32_at(header.wrapping_add(MOBI_HEADER))? == MOBI_MAGIC;
    let mobi_length = if mobi {
        Some(book.i32_at(header.wrapping_add(MOBI_HEADER + 4))?)
    } else {
        None
    };
    let extra_flags = match mobi_length {
        Some(length) if length >= EXTRA_FLAGS_MIN_HEADER_LENGTH => {
            book.u16_at(header.wrapping_add(EXTRA_FLAGS))?
        }
        _ => 0,
    };
    let charset = if mobi && book.i32_at(header.wrapping_add(MOBI_HEADER + 12))? == UTF_8 {
        Charset::named("UTF-8")
    } else {
        Charset::named("windows-1252")
    };
    let mut html = Vec::with_capacity((text_length as usize).min(input.len() * MAX_EXPANSION));
    for index in 1..=text_records as usize {
        let record = book.bytes(offsets[index], offsets[index + 1] - offsets[index])?;
        let content = &record[..record.len() - trailing_bytes(record, extra_flags)?];
        if compression == PALMDOC_COMPRESSION {
            decompress(content, &mut html)?;
        } else {
            html.extend_from_slice(content);
        }
        if html.len() > MAX_TEXT_BYTES {
            return Err(broken(format!(
                "The book's text decompresses past {MAX_TEXT_BYTES} bytes"
            )));
        }
    }
    Ok(Markup {
        book,
        header,
        mobi_length,
        charset,
        html,
    })
}

/// `trailingBytes`: the size of the entries the book's extra data flags say end a text record,
/// one entry, sized by a backward variable-length integer, per flag from the second on, then
/// the bytes of a multibyte character cut by the record's end when the first flag is set.
fn trailing_bytes(record: &[u8], extra_flags: i32) -> Result<usize, Error> {
    let cut = || broken("A text record ends before its trailing entries");
    let length = record.len();
    let mut trailing = 0;
    let mut flags = extra_flags >> 1;
    while flags != 0 {
        if flags & 1 != 0 {
            trailing += length
                .checked_sub(trailing)
                .map_or(0, |end| entry_size(record, end));
        }
        flags >>= 1;
    }
    if extra_flags & 1 != 0 {
        let last = length
            .checked_sub(trailing)
            .and_then(|end| end.checked_sub(1))
            .ok_or_else(cut)?;
        trailing += usize::from(record[last] & 0x3) + 1;
    }
    if trailing > length {
        return Err(cut());
    }
    Ok(trailing)
}

/// `entrySize`: the backward variable-length integer of the four bytes before `end`.
fn entry_size(record: &[u8], end: usize) -> usize {
    record[end.saturating_sub(4)..end]
        .iter()
        .fold(0, |size, &byte| {
            let size = if byte & 0x80 != 0 { 0 } else { size };
            (size << 7) | usize::from(byte & 0x7f)
        })
}

/// `decompress`: PalmDOC's LZ77 of one record, its literals, runs of literals, space-prefixed
/// characters and back copies, appended to `text`.
fn decompress(record: &[u8], text: &mut Vec<u8>) -> Result<(), Error> {
    let start = text.len();
    let mut index = 0;
    while index < record.len() {
        let code = record[index];
        index += 1;
        match code {
            0x00 | 0x09..=0x7f => text.push(code),
            0x01..=0x08 => {
                let run = record
                    .get(index..index + usize::from(code))
                    .ok_or_else(|| broken("A literal run passes the end of a text record"))?;
                text.extend_from_slice(run);
                index += run.len();
            }
            0xc0..=0xff => text.extend_from_slice(&[b' ', code ^ 0x80]),
            0x80..=0xbf => {
                let next = *record
                    .get(index)
                    .ok_or_else(|| broken("A back copy passes the end of a text record"))?;
                index += 1;
                let pair = usize::from(u16::from_be_bytes([code, next]));
                let distance = (pair >> 3) & 0x7ff;
                let count = (pair & 0x7) + 3;
                if distance == 0 || distance > text.len() - start {
                    return Err(broken(
                        "A back copy reaches before the start of a text record",
                    ));
                }
                for _ in 0..count {
                    text.push(text[text.len() - distance]);
                }
            }
        }
    }
    Ok(())
}

/// `bookMetadata`: the title, authors and language of the book's MOBI and EXTH headers.
fn book_metadata(markup: &Markup, metadata: &mut TikaMetadata) -> Result<(), Error> {
    let Some(mobi_length) = markup.mobi_length else {
        return Ok(());
    };
    let (book, header) = (markup.book, markup.header);
    let decode = |bytes: &[u8]| {
        markup.charset.decode(bytes).ok_or_else(|| {
            broken(format!(
                "The native decoders do not decode {}",
                markup.charset.name()
            ))
        })
    };
    let name_offset = book.i32_at(header.wrapping_add(FULL_NAME_OFFSET))?;
    let name_length = book.i32_at(header.wrapping_add(FULL_NAME_LENGTH))?;
    let name_at = header.wrapping_add(name_offset);
    if name_offset > 0 && name_length > 0 && name_at.wrapping_add(name_length) <= book.limit {
        metadata.set("dc:title", decode(book.bytes(name_at, name_length)?)?);
    }
    let flags = book.i32_at(header.wrapping_add(EXTH_FLAGS))?;
    let exth = header.wrapping_add(MOBI_HEADER).wrapping_add(mobi_length);
    if flags & EXTH_PRESENT == 0
        || exth.wrapping_add(12) > book.limit
        || book.i32_at(exth)? != EXTH_MAGIC
    {
        return Ok(());
    }
    let count = book.i32_at(exth.wrapping_add(8))?;
    let mut offset = exth.wrapping_add(12);
    let mut entry = 0;
    while entry < count && offset.wrapping_add(8) <= book.limit {
        let kind = book.i32_at(offset)?;
        let length = book.i32_at(offset.wrapping_add(4))?;
        if length < 8 || offset.wrapping_add(length) > book.limit {
            return Ok(());
        }
        if matches!(kind, EXTH_AUTHOR | EXTH_TITLE | EXTH_LANGUAGE) {
            let value = decode(book.bytes(offset.wrapping_add(8), length - 8)?)?;
            let value = strip(&value);
            match kind {
                EXTH_AUTHOR => metadata.add("dc:creator", value),
                EXTH_TITLE => metadata.set("dc:title", value),
                _ => metadata.set("dc:language", value),
            }
        }
        offset = offset.wrapping_add(length);
        entry += 1;
    }
    Ok(())
}

/// Our Java `MobiParser`.
pub(crate) struct MobiParser;

impl Parser for MobiParser {
    fn name(&self) -> &'static str {
        "extractous::MobiParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "ai.yobix.MobiParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-mobipocket-ebook"]
    }

    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        configs: &ParserConfigs,
    ) -> Option<String> {
        let markup = read(input).ok()?;
        JSoupParser
            .defers(&markup.html, &markup.metadata(), configs)
            .map(|reason| format!("the book's HTML defers: {reason}"))
    }

    fn fails_before_starting(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<Error> {
        read(input).err()
    }

    fn parse(
        &self,
        input: &[u8],
        configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let markup = read(input).map_err(Failure::Extraction)?;
        xhtml.with_metadata(markup.metadata(), |xhtml| {
            JSoupParser.parse(&markup.html, configs, xhtml)?;
            xhtml.end_document().map_err(Failure::from)
        })?;
        book_metadata(&markup, xhtml.metadata_mut()).map_err(Failure::Extraction)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_fails_as_tika, assert_same_as_tika, documents, Scratch};

    const TIKA_NAME: &str = "ai.yobix.MobiParser";

    /// A book to write: its PalmDOC header's fields, a MOBI header when `encoding` is given,
    /// and its text records as they are stored.
    struct Spec {
        compression: u16,
        encryption: u16,
        encoding: Option<u32>,
        mobi_length: u32,
        extra_flags: u16,
        exth: Vec<(u32, &'static [u8])>,
        name: &'static [u8],
        header_bytes: Option<usize>,
        records: Vec<Vec<u8>>,
    }

    impl Default for Spec {
        fn default() -> Self {
            Self {
                compression: 1,
                encryption: 0,
                encoding: Some(65001),
                mobi_length: 0xE8,
                extra_flags: 0,
                exth: Vec::new(),
                name: b"",
                header_bytes: None,
                records: Vec::new(),
            }
        }
    }

    fn put(bytes: &mut Vec<u8>, at: usize, value: &[u8]) {
        if bytes.len() < at + value.len() {
            bytes.resize(at + value.len(), 0);
        }
        bytes[at..at + value.len()].copy_from_slice(value);
    }

    fn book(spec: &Spec) -> Vec<u8> {
        let mut header = vec![0; 16];
        put(&mut header, 0, &spec.compression.to_be_bytes());
        put(&mut header, 4, &4096u32.to_be_bytes());
        put(&mut header, 8, &(spec.records.len() as u16).to_be_bytes());
        put(&mut header, 12, &spec.encryption.to_be_bytes());
        if let Some(encoding) = spec.encoding {
            let mobi_end = 16 + spec.mobi_length as usize;
            header.resize(mobi_end.max(0xF4), 0);
            put(&mut header, 16, b"MOBI");
            put(&mut header, 20, &spec.mobi_length.to_be_bytes());
            put(&mut header, 28, &encoding.to_be_bytes());
            put(&mut header, 0xF2, &spec.extra_flags.to_be_bytes());
            if !spec.exth.is_empty() {
                put(&mut header, 0x80, &0x40u32.to_be_bytes());
                let mut exth = b"EXTH".to_vec();
                let entries: Vec<u8> = spec
                    .exth
                    .iter()
                    .flat_map(|(kind, value)| {
                        let mut entry = kind.to_be_bytes().to_vec();
                        entry.extend((value.len() as u32 + 8).to_be_bytes());
                        entry.extend(*value);
                        entry
                    })
                    .collect();
                exth.extend((entries.len() as u32 + 12).to_be_bytes());
                exth.extend((spec.exth.len() as u32).to_be_bytes());
                exth.extend(entries);
                put(&mut header, mobi_end, &exth);
            }
            if !spec.name.is_empty() {
                let at = header.len();
                put(&mut header, 0x54, &(at as u32).to_be_bytes());
                put(&mut header, 0x58, &(spec.name.len() as u32).to_be_bytes());
                put(&mut header, at, spec.name);
            }
        }
        if let Some(length) = spec.header_bytes {
            header.truncate(length);
        }
        let records: Vec<&[u8]> = std::iter::once(header.as_slice())
            .chain(spec.records.iter().map(Vec::as_slice))
            .collect();
        let mut palm = vec![0; 78];
        put(&mut palm, 0, b"synthetic");
        put(&mut palm, 60, b"BOOKMOBI");
        put(&mut palm, 76, &(records.len() as u16).to_be_bytes());
        let mut offset = 78 + 8 * records.len();
        for record in &records {
            palm.extend((offset as u32).to_be_bytes());
            palm.extend([0; 4]);
            offset += record.len();
        }
        for record in records {
            palm.extend(record);
        }
        palm
    }

    /// `<p>Hello hello café</p>`, PalmDOC-compressed: literals, a space pair, a back copy and a
    /// literal run.
    const COMPRESSED: &[u8] = b"<p>Hello\xE8\x80\x31\xE3af\x02\xC3\xA9</p>";

    #[test]
    fn decompresses_and_trims_records_as_java_does() {
        let mut text = Vec::new();
        decompress(COMPRESSED, &mut text).unwrap();
        assert_eq!(text, "<p>Hello hello café</p>".as_bytes());
        assert!(decompress(b"a\x80\x11", &mut Vec::new()).is_err());
        assert!(decompress(b"\x03ab", &mut Vec::new()).is_err());
        assert_eq!(trailing_bytes(b"text\x01\x01\x55\x82", 0b11).unwrap(), 4);
        assert_eq!(trailing_bytes(b"text\x81", 0b10).unwrap(), 1);
        assert!(trailing_bytes(b"\x83", 0b10).is_err());
        assert!(trailing_bytes(b"", 0b1).is_err());
    }

    #[test]
    fn extracts_books_as_tikas_path_does() {
        let scratch = Scratch::new("mobi");
        let mut first = COMPRESSED.to_vec();
        first.extend(b"\x01\x01\x55\x82");
        let mut second = b"<p>Two</p>".to_vec();
        second.extend(b"\x00\x81");
        let read = [
            scratch.write(
                "compressed.mobi",
                &book(&Spec {
                    compression: 2,
                    extra_flags: 0b11,
                    exth: vec![
                        (100, b" Ada Lovelace "),
                        (100, b"Charles Babbage"),
                        (524, b"en"),
                        (503, b"The Title"),
                        (101, b"Publisher"),
                    ],
                    name: b"Full Name",
                    records: vec![first, second],
                    ..Spec::default()
                }),
            ),
            scratch.write(
                "windows-1252.mobi",
                &book(&Spec {
                    encoding: Some(1252),
                    mobi_length: 0x18,
                    name: b"Caf\xE9 \x81",
                    records: vec![b"<html><head><title>Inner</title></head><body><p>Caf\xE9 cr\xE8me</p></body></html>".to_vec()],
                    ..Spec::default()
                }),
            ),
            scratch.write(
                "palmdoc.mobi",
                &book(&Spec {
                    encoding: None,
                    records: vec![b"<p>No MOBI header</p>".to_vec()],
                    ..Spec::default()
                }),
            ),
        ];
        let failing = [
            scratch.write(
                "drm.mobi",
                &book(&Spec {
                    encryption: 2,
                    records: vec![b"<p>Locked</p>".to_vec()],
                    ..Spec::default()
                }),
            ),
            scratch.write(
                "huff.mobi",
                &book(&Spec {
                    compression: 17480,
                    records: vec![b"<p>Huffman</p>".to_vec()],
                    ..Spec::default()
                }),
            ),
            scratch.write(
                "short-mobi-header.mobi",
                &book(&Spec {
                    mobi_length: 0x18,
                    header_bytes: Some(32),
                    records: vec![b"<p>Text before the failure</p>".to_vec()],
                    ..Spec::default()
                }),
            ),
            scratch.write(
                "no-extra-flags.mobi",
                &book(&Spec {
                    header_bytes: Some(32),
                    records: vec![b"<p>No text</p>".to_vec()],
                    ..Spec::default()
                }),
            ),
            scratch.write("truncated.mobi", &book(&Spec::default())[..70]),
        ];
        let (parsed, deferred) = documents(&MobiParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len() + failing.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
        let error = crate::Extractor::new()
            .extract_file_to_string(failing[0].to_str().unwrap())
            .unwrap_err();
        assert!(matches!(error, Error::Encrypted(_)), "{error:?}");
    }

    #[test]
    fn extracts_the_corpus_book_as_tikas_path_does() {
        let (parsed, deferred) = documents(&MobiParser, &[Path::new("../test_files/documents")]);
        assert!(deferred.is_empty());
        assert_eq!(parsed.len(), 1);
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
