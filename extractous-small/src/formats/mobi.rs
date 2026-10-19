//! Mobipocket and Kindle books (MOBI, AZW, AZW3) without DRM, as our Java `MobiParser` reads
//! them: the title, authors and language of the book's MOBI and EXTH headers as metadata, then
//! the text records of the Palm database, decompressed and handed with their charset to the
//! HTML parser, which writes the document under the book's metadata. The book is read
//! big-endian, an index outside it failing the parse. Positions are summed without overflow, so
//! a header naming metadata past the book's end is passed over, where Java's `int` sums wrap
//! around to an index that fails the parse (#105).
//!
//! Our Java parser has the HTML parser write the head from the HTML's own metadata, and adds the
//! book's after the document has ended, where the head never shows it (#105).
//!
//! The text is decompressed a record at a time into a spool, which a large book's goes to a
//! file of, and the HTML parser builds a large page from it a piece at a time, so that the
//! memory a book takes is bounded whatever its size.

use std::io::Write;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::html;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{noted, unless_different, Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Paging, Spool};
use crate::java::strip;
use crate::text::charset::Charset;

const PALM_DATABASE_HEADER: i64 = 78;
const RECORD_COUNT: i64 = 76;
const RECORD_ENTRY: i64 = 8;
const NO_COMPRESSION: i64 = 1;
const PALMDOC_COMPRESSION: i64 = 2;
const HUFF_CDIC_COMPRESSION: i64 = 17480;
const MOBI_HEADER: i64 = 16;
const MOBI_MAGIC: i64 = 0x4D4F_4249;
const UTF_8: i64 = 65001;
const EXTRA_FLAGS_MIN_HEADER_LENGTH: i64 = 0xE4;
const EXTRA_FLAGS: i64 = 0xF2;
const FULL_NAME_OFFSET: i64 = 0x54;
const FULL_NAME_LENGTH: i64 = 0x58;
const EXTH_FLAGS: i64 = MOBI_HEADER + 0x70;
const EXTH_PRESENT: i64 = 0x40;
const EXTH_MAGIC: i64 = 0x4558_5448;
const EXTH_AUTHOR: i64 = 100;
const EXTH_TITLE: i64 = 503;
const EXTH_LANGUAGE: i64 = 524;
const MAX_TEXT_BYTES: usize = 256 << 20;
/// The most a PalmDOC byte decompresses to: a two-byte back copy of ten.
const MAX_EXPANSION: u64 = 5;

fn broken(reason: impl Into<String>) -> Error {
    Error::ParseError(reason.into())
}

/// The book, read big-endian at indexes wide enough that no sum of a position and a length
/// read from it overflows.
#[derive(Clone, Copy)]
struct Book<'a> {
    bytes: &'a [u8],
    limit: i64,
}

impl<'a> Book<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, Error> {
        let limit = i64::try_from(bytes.len())
            .map_err(|_| broken("The book is larger than an index addresses"))?;
        Ok(Self { bytes, limit })
    }

    fn bytes(self, at: i64, length: i64) -> Result<&'a [u8], Error> {
        usize::try_from(at)
            .ok()
            .zip(usize::try_from(length).ok())
            .and_then(|(start, length)| self.bytes.get(start..start.checked_add(length)?))
            .ok_or_else(|| broken(format!("The book holds no {length} bytes at {at}")))
    }

    /// `getShort(at) & 0xffff`.
    fn u16_at(self, at: i64) -> Result<i64, Error> {
        let bytes = self.bytes(at, 2)?;
        Ok(i64::from(u16::from_be_bytes([bytes[0], bytes[1]])))
    }

    /// The signed 32-bit integer at `at`.
    fn i32_at(self, at: i64) -> Result<i64, Error> {
        let bytes = self.bytes(at, 4)?;
        Ok(i64::from(i32::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ])))
    }
}

/// What the book's headers say about its HTML.
struct Headers<'a> {
    book: Book<'a>,
    /// The offset of record 0, which holds the PalmDOC and MOBI headers.
    header: i64,
    /// The length of the MOBI header, when the book has one.
    mobi_length: Option<i64>,
    charset: Charset,
}

impl Headers<'_> {
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

/// `MobiParser.parse` up to the HTML of `input`, whose pages are `pages`: the failures it
/// throws before any output, else the headers and the HTML, spooled.
fn read<'a>(input: &'a [u8], pages: Pages<'a>) -> Result<(Headers<'a>, Input<'static>), Error> {
    let book = Book::new(input)?;
    if book.limit < PALM_DATABASE_HEADER {
        return Err(broken("The book is shorter than a Palm database header"));
    }
    let records = book.u16_at(RECORD_COUNT)?;
    let mut offsets = (0..records)
        .map(|index| book.i32_at(PALM_DATABASE_HEADER + index * RECORD_ENTRY))
        .collect::<Result<Vec<i64>, Error>>()?;
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
    let text_length = book.i32_at(header + 4)? as u32;
    let text_records = book.u16_at(header + 8)?;
    let encryption = book.u16_at(header + 12)?;
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
    let mobi =
        header + MOBI_HEADER + 4 <= book.limit && book.i32_at(header + MOBI_HEADER)? == MOBI_MAGIC;
    let mobi_length = if mobi {
        Some(book.i32_at(header + MOBI_HEADER + 4)?)
    } else {
        None
    };
    let extra_flags = match mobi_length {
        Some(length) if length >= EXTRA_FLAGS_MIN_HEADER_LENGTH => {
            book.u16_at(header + EXTRA_FLAGS)?
        }
        _ => 0,
    };
    let charset = if mobi && book.i32_at(header + MOBI_HEADER + 12)? == UTF_8 {
        Charset::named("UTF-8")
    } else {
        Charset::named("windows-1252")
    };
    let spooled =
        |error: std::io::Error| Error::IoError(format!("Could not spool the book's text: {error}"));
    let declared = u64::from(text_length).min(input.len() as u64 * MAX_EXPANSION);
    let mut html = Spool::new(Some(declared));
    let paging = Paging::new(pages);
    let mut decompressed = Vec::new();
    let mut length = 0;
    for index in 1..=text_records as usize {
        let record = book.bytes(offsets[index], offsets[index + 1] - offsets[index])?;
        paging.read(record.len());
        let content = &record[..record.len() - trailing_bytes(record, extra_flags)?];
        let text = if compression == PALMDOC_COMPRESSION {
            decompressed.clear();
            decompress(content, &mut decompressed)?;
            &decompressed
        } else {
            content
        };
        length += text.len();
        if length > MAX_TEXT_BYTES {
            return Err(broken(format!(
                "The book's text decompresses past {MAX_TEXT_BYTES} bytes"
            )));
        }
        html.write_all(text).map_err(spooled)?;
    }
    let headers = Headers {
        book,
        header,
        mobi_length,
        charset,
    };
    Ok((headers, html.finish().map_err(spooled)?))
}

/// `trailingBytes`: the size of the entries the book's extra data flags say end a text record,
/// one entry, sized by a backward variable-length integer, per flag from the second on, then
/// the bytes of a multibyte character cut by the record's end when the first flag is set.
fn trailing_bytes(record: &[u8], extra_flags: i64) -> Result<usize, Error> {
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
fn book_metadata(headers: &Headers, metadata: &mut TikaMetadata) -> Result<(), Error> {
    let Some(mobi_length) = headers.mobi_length else {
        return Ok(());
    };
    let (book, header) = (headers.book, headers.header);
    let decode = |bytes: &[u8]| {
        headers.charset.decode(bytes).ok_or_else(|| {
            broken(format!(
                "The native decoders do not decode {}",
                headers.charset.name()
            ))
        })
    };
    let name_offset = book.i32_at(header + FULL_NAME_OFFSET)?;
    let name_length = book.i32_at(header + FULL_NAME_LENGTH)?;
    let name_at = header + name_offset;
    if name_offset > 0 && name_length > 0 && name_at + name_length <= book.limit {
        metadata.set("dc:title", decode(book.bytes(name_at, name_length)?)?);
    }
    let flags = book.i32_at(header + EXTH_FLAGS)?;
    let exth = header + MOBI_HEADER + mobi_length;
    if flags & EXTH_PRESENT == 0
        || mobi_length < 0
        || exth + 12 > book.limit
        || book.i32_at(exth)? != EXTH_MAGIC
    {
        return Ok(());
    }
    let count = book.i32_at(exth + 8)?;
    let mut offset = exth + 12;
    let mut entry = 0;
    while entry < count && offset + 8 <= book.limit {
        let kind = book.i32_at(offset)?;
        let length = book.i32_at(offset + 4)?;
        if length < 8 || offset + length > book.limit {
            return Ok(());
        }
        if matches!(kind, EXTH_AUTHOR | EXTH_TITLE | EXTH_LANGUAGE) {
            let value = decode(book.bytes(offset + 8, length - 8)?)?;
            let value = strip(&value);
            match kind {
                EXTH_AUTHOR => metadata.add("dc:creator", value),
                EXTH_TITLE => metadata.set("dc:title", value),
                _ => metadata.set("dc:language", value),
            }
        }
        offset += length;
        entry += 1;
    }
    Ok(())
}

/// Our Java `MobiParser`.
pub struct MobiParser;

impl Parser for MobiParser {
    fn name(&self) -> &'static str {
        "extractous::MobiParser"
    }
    fn tika_name(&self) -> &'static str {
        "ai.yobix.MobiParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-mobipocket-ebook"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let (headers, text) = read(input, pages).map_err(Decline::Fails)?;
        let (html, difference) = noted(html::prepare_input(text, &headers.metadata()))
            .map_err(|reason| Decline::Deferred(format!("the book's HTML defers: {reason}")))?;
        unless_different(
            Box::new(move |xhtml| {
                book_metadata(&headers, xhtml.metadata_mut()).map_err(Failure::Extraction)?;
                // The HTML parser writes into a copy, so what it reads from the HTML stays out of
                // the book's metadata, as in Tika's path.
                let book = xhtml.metadata_mut().clone();
                xhtml.with_metadata(book, |xhtml| {
                    html(xhtml)?;
                    xhtml.end_document().map_err(Failure::from)
                })
            }),
            difference,
        )
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn shows_the_books_metadata_in_the_head() {
        let (xml, metadata) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(&book(&Spec {
                exth: vec![(100, b"Ada Lovelace"), (524, b"en")],
                name: b"Full Name",
                ..Spec::default()
            }))
            .unwrap();
        let head = &xml[..xml.find("</head>").unwrap()];
        for (name, values) in &metadata {
            if name.starts_with("dc:") {
                for value in values {
                    assert!(
                        head.contains(&format!(r#"<meta name="{name}" content="{value}" />"#)),
                        "{xml}"
                    );
                }
            }
        }
        assert!(metadata.contains_key("dc:title"), "{metadata:?}");
    }

    #[test]
    fn passes_over_metadata_a_header_places_past_the_book() {
        let mut bytes = book(&Spec {
            exth: vec![(100, b"Ada Lovelace")],
            name: b"Full Name",
            records: vec!["<p>Bølgerne</p>".as_bytes().to_vec()],
            ..Spec::default()
        });
        let header = 78 + 8 * 2;
        put(&mut bytes, header + 0x14, &0x7FFF_FFF0u32.to_be_bytes());
        put(&mut bytes, header + 0x54, &0x7FFF_FFF0u32.to_be_bytes());
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&bytes)
            .unwrap();
        assert_eq!(text.trim(), "Bølgerne");
        assert!(!metadata.contains_key("dc:title"), "{metadata:?}");
        assert!(!metadata.contains_key("dc:creator"), "{metadata:?}");
        put(&mut bytes, header + 0x14, &(-8i32).to_be_bytes());
        let (text, _) = crate::Extractor::new()
            .extract_bytes_to_string(&bytes)
            .unwrap();
        assert_eq!(text.trim(), "Bølgerne");
    }

    /// A book to write: its PalmDOC header's fields, a MOBI header when `encoding` is given,
    /// and its text records as they are stored.
    struct Spec {
        compression: u16,
        text_length: u32,
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
                text_length: 4096,
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

    /// The Palm database header and record 0 of a book of `spec` whose text records are
    /// `lengths` long: all of the book before its text.
    fn head(spec: &Spec, lengths: &[usize]) -> Vec<u8> {
        let mut header = vec![0; 16];
        put(&mut header, 0, &spec.compression.to_be_bytes());
        put(&mut header, 4, &spec.text_length.to_be_bytes());
        put(&mut header, 8, &(lengths.len() as u16).to_be_bytes());
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
        let mut palm = vec![0; 78];
        put(&mut palm, 0, b"synthetic");
        put(&mut palm, 60, b"BOOKMOBI");
        put(&mut palm, 76, &(lengths.len() as u16 + 1).to_be_bytes());
        let mut offset = 78 + 8 * (lengths.len() + 1);
        for length in std::iter::once(header.len()).chain(lengths.iter().copied()) {
            palm.extend((offset as u32).to_be_bytes());
            palm.extend([0; 4]);
            offset += length;
        }
        palm.extend(header);
        palm
    }

    fn book(spec: &Spec) -> Vec<u8> {
        let lengths: Vec<usize> = spec.records.iter().map(Vec::len).collect();
        let mut palm = head(spec, &lengths);
        for record in &spec.records {
            palm.extend(record);
        }
        palm
    }

    /// A book too large to read whole, of uncompressed records of paragraphs, each numbered:
    /// its text is spooled to a file and its HTML built a piece at a time, under a budget far
    /// below the book.
    #[test]
    fn reads_books_too_large_to_read_whole_a_piece_at_a_time() {
        use extractous_core::large::{bodies_past_limit, LargeFile};

        const PARAGRAPHS: u64 = 320;
        let paragraph = |index: u64| format!("<p>Afsnit {index:010}</p>\n");
        let record = |record: u64| {
            (record * PARAGRAPHS..(record + 1) * PARAGRAPHS)
                .map(paragraph)
                .collect::<String>()
                .into_bytes()
        };
        let length = record(0).len();
        let records = bodies_past_limit(length);
        let lengths = vec![length; usize::try_from(records).unwrap()];
        let spec = Spec {
            text_length: u32::try_from(records * length as u64).unwrap(),
            ..Spec::default()
        };
        let file = LargeFile::new("large.mobi", &head(&spec, &lengths), records, record, b"");
        let mut paragraphs = 0u64;
        file.extract_lines(MobiParser.name(), |line| {
            if let Some(number) = line.trim().strip_prefix("Afsnit ") {
                assert_eq!(number, format!("{paragraphs:010}"));
                paragraphs += 1;
            }
        });
        assert_eq!(paragraphs, records * PARAGRAPHS);
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
}
