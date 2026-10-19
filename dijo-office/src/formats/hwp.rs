//! Hancom Office (Hangul Word Processor) 5 documents, as Tika's `HwpV5Parser` reads them with
//! its `HwpTextExtractorV5`: the summary information of the `\u{5}HwpSummaryInformation`
//! stream as metadata, then the text of the sections' paragraph text records, a paragraph each.
//! A compressed document's sections are raw deflate streams; a distribution document's sections,
//! under `ViewText`, are decrypted with the AES key that their first record hides, as Tika
//! decrypts them. A document encrypted with a password fails, as it fails Tika's parse, which
//! has no means to decrypt it, and so does one whose file header or summary information Tika
//! cannot read.
//!
//! Where Tika slips, this port does not: it reads the sections in the order of their numbers,
//! where Tika reads them in the order POI keeps a directory's entries, which is not theirs; it
//! passes over a paragraph text record of no characters, on which Tika's stream reader throws
//! `IllegalArgumentException`, failing the parse; it passes over a summary property of another
//! type than its kind, on which Tika's cast throws `ClassCastException`, and writes a page
//! count POI reads as a `Long` as one it reads as an `Integer`; it leaves out an incomplete AES
//! block that ends a distribution document's section, on which Java's cipher stream throws; and
//! it spells `must` right in the exception on a paragraph too long, which Tika misspells.

use std::convert::Infallible;
use std::io::Read;

use aes::cipher::{BlockCipherDecrypt, KeyInit};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::crypt;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{read_property_set, Value};
use crate::formats::poi::{self, Thrown as PoiThrown};
use crate::formats::thrown::{Exception, Thrown, TIKA_EXCEPTION};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::java::time::format_instant;
use crate::ole2::{Content, EntryId, EntryKind, Ole2};

const MEDIA_TYPE: &str = "application/x-hwp-v5";
const TIKA_NAME: &str = "org.apache.tika.parser.hwp.HwpV5Parser";
const FILE_HEADER: &str = "FileHeader";
const SUMMARY_INFORMATION: &str = "\u{5}HwpSummaryInformation";
/// What the file header starts with, and how long `getHeader` reads it.
const SIGNATURE: &[u8] = b"HWP Document File";
const HEADER_LENGTH: usize = 256;
/// Where the file header holds its flags, and the flags `getHeader` reads.
const FLAGS_AT: usize = 36;
const COMPRESSED: u32 = 0x01;
const ENCRYPTED: u32 = 0x02;
const VIEW_TEXT: u32 = 0x04;
/// `HWPTAG_BEGIN + 51`: `HWPTAG_PARA_TEXT`.
const PARA_TEXT: u32 = 0x10 + 51;
/// The record length that says a 32-bit length follows the record header.
const EXTENDED_LENGTH: u32 = 0xFFF;
const MAX_TAG_LENGTH: u32 = 50 * 1024 * 1024;
/// How long the record header and the data of a distribution document's first record are.
const DISTRIBUTION_HEADER: usize = 4;
const DISTRIBUTION_DATA: usize = 256;
const AES_BLOCK: usize = 16;
/// The summary information's text and date properties that `populateMatadata` writes, by id,
/// with the metadata each sets, and the id of its page count.
const TEXTS: [(u32, &str); 6] = [
    (2, "dc:title"),
    (3, "dc:subject"),
    (4, "dc:creator"),
    (5, "meta:keyword"),
    (6, "w:Comments"),
    (8, "meta:last-author"),
];
const DATES: [(u32, &str); 2] = [(12, "dcterms:created"), (13, "dcterms:modified")];
const PAGE_COUNT: u32 = 14;
const PAGE_COUNT_NAME: &str = "meta:page-count";

const IO_EXCEPTION: &str = "java.io.IOException";
const EOF: &str = "java.io.EOFException";
const FILE_NOT_FOUND: &str = "java.io.FileNotFoundException";
const UNSUPPORTED_FORMAT: &str = "org.apache.tika.exception.UnsupportedFormatException";
const MEMORY_LIMIT: &str = "org.apache.tika.exception.TikaMemoryLimitException";
/// The message of the `TikaException` `extract` wraps an `IOException` in.
const NOT_HWP: &str = "error occurred when parsing HWP Format, It may not HWP Format.";

/// What a character below 32 is in a paragraph's text, as `HWP_CHAR_TYPE` lists them: an inline
/// or an extended control, which take eight characters, or a character control, which takes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Inline,
    Extended,
    Character,
}

use Control::{Character as C, Extended as X, Inline as I};

const CONTROLS: [Control; 32] = [
    C, X, X, X, I, I, I, I, I, I, C, X, X, C, X, X, X, X, X, I, I, X, X, X, C, C, C, C, C, C, C, C,
];

pub struct HwpV5Parser;

impl Parser for HwpV5Parser {
    fn name(&self) -> &'static str {
        "dijo::HwpV5Parser"
    }
    fn tika_name(&self) -> &'static str {
        TIKA_NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// It reads the file header and the summary information whole, and each section as it is
    /// decrypted and inflated, holding one paragraph text record at a time, each at most the 50
    /// megabytes Tika allows one.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Fails documents Tika's parse fails on, reading the sections once to tell, and again as
    /// their text is written.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let Opened {
            file,
            flags,
            metadata,
        } = open(input)?;
        walk(&file, flags, &mut |_| Ok::<(), Infallible>(())).map_err(|halt| match halt {
            Halt::Thrown(thrown) => Decline::Fails(failed(thrown)),
            Halt::Written(never) => match never {},
        })?;
        Ok(Box::new(move |xhtml| {
            xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
            for (name, value) in metadata {
                xhtml.metadata_mut().set(name, value);
            }
            walk(&file, flags, &mut |paragraph| {
                xhtml.start_element("p")?;
                xhtml.characters_utf16(paragraph)?;
                xhtml.end_element("p")?;
                Ok(())
            })
            .map_err(|halt| match halt {
                Halt::Thrown(thrown) => Failure::Extraction(failed(thrown)),
                Halt::Written(failure) => failure,
            })
        }))
    }
}

/// A document `extract` reads the sections of: its file, the flags of its file header and the
/// metadata it sets, by name.
struct Opened<'a> {
    file: Ole2<'a>,
    flags: u32,
    metadata: Vec<(&'static str, String)>,
}

/// `extract` as far as the sections: the file header and the summary information, or how
/// Tika's parse fails on the document.
fn open(input: &[u8]) -> Result<Opened<'_>, Decline<'static>> {
    let fails = |thrown| Decline::Fails(failed(thrown));
    let file = Ole2::open(input).map_err(|reason| fails(io(IO_EXCEPTION, reason.0)))?;
    let root = file.root();
    let flags = flags(&file, root).map_err(fails)?;
    if flags & ENCRYPTED != 0 {
        return Err(Decline::Fails(crypt::encrypted_document(
            "document is encrypted",
        )));
    }
    let metadata = summary(&file, root)?;
    Ok(Opened {
        file,
        flags,
        metadata,
    })
}

/// The error the parse fails with where `extract` throws `thrown`: an `IOException` wrapped in
/// the `TikaException` that `extract` throws for it, any other exception as it is.
fn failed(thrown: Thrown) -> Error {
    let thrown = match thrown {
        Thrown::Io(cause) => Thrown::Tika(Exception::new(TIKA_EXCEPTION, NOT_HWP).caused_by(cause)),
        other => other,
    };
    thrown.into_error(TIKA_NAME)
}

fn io(class: &'static str, message: impl Into<String>) -> Thrown {
    Thrown::Io(Exception::new(class, message))
}

fn unsupported(message: &str) -> Thrown {
    Thrown::Tika(Exception::new(UNSUPPORTED_FORMAT, message))
}

/// `DirectoryNode.getEntry` of an entry `directory` does not have.
fn no_such_entry(name: &str) -> Thrown {
    io(FILE_NOT_FOUND, format!("no such entry: \"{name}\""))
}

/// The stream `entry`, as `DocumentInputStream` reads it.
fn content<'a>(file: &Ole2<'a>, entry: EntryId) -> Result<Content<'a>, Thrown> {
    file.content(entry)
        .map_err(|reason| io(IO_EXCEPTION, reason.0))
}

/// `getHeader`: the flags of the file header, which must be a stream of at least 256 bytes that
/// starts with the signature.
fn flags(file: &Ole2<'_>, root: EntryId) -> Result<u32, Thrown> {
    let entry = file
        .child(root, FILE_HEADER)
        .ok_or_else(|| no_such_entry(FILE_HEADER))?;
    let unreadable = || unsupported("cannot parse the File Header");
    if file.kind(entry) != EntryKind::Document {
        return Err(unreadable());
    }
    let header = content(file, entry)?
        .range(0, HEADER_LENGTH)
        .filter(|header| header.starts_with(SIGNATURE))
        .ok_or_else(unreadable)?;
    let flags = header[FLAGS_AT..FLAGS_AT + 4]
        .try_into()
        .expect("the header holds four bytes of flags");
    Ok(u32::from_le_bytes(flags))
}

/// `parseSummaryInformation`: the properties of the summary information's first section that
/// Tika writes, as metadata names and values. A summary information that is missing, or that
/// POI does not read as a property set, fails the parse.
fn summary(
    file: &Ole2<'_>,
    root: EntryId,
) -> Result<Vec<(&'static str, String)>, Decline<'static>> {
    let unreadable = || Decline::Fails(failed(unsupported("cannot parse the Summary Information")));
    let entry = file
        .child(root, SUMMARY_INFORMATION)
        .filter(|&entry| file.kind(entry) == EntryKind::Document)
        .ok_or_else(unreadable)?;
    let stream = file.content(entry).map_err(|_| unreadable())?;
    let set = read_property_set(&stream.bytes())?.ok_or_else(unreadable)?;
    let section = set.sections.first().ok_or_else(unreadable)?;
    let named = |names: &[(u32, &'static str)], id| {
        names
            .iter()
            .find(|&&(known, _)| known == id)
            .map(|&(_, name)| name)
    };
    Ok(section
        .properties()
        .filter_map(|(id, value)| match value? {
            Value::String(text) => Some((named(&TEXTS, id)?, text.clone())),
            Value::Date(millis) => Some((named(&DATES, id)?, format_instant(*millis))),
            Value::Integer(count) if id == PAGE_COUNT => Some((PAGE_COUNT_NAME, count.to_string())),
            Value::Long(count) if id == PAGE_COUNT => Some((PAGE_COUNT_NAME, count.to_string())),
            _ => None,
        })
        .collect())
}

/// Why reading the sections stops: Tika's parse throws, or handing a paragraph on failed.
enum Halt<E> {
    Thrown(Thrown),
    Written(E),
}

impl<E> From<Thrown> for Halt<E> {
    fn from(thrown: Thrown) -> Self {
        Halt::Thrown(thrown)
    }
}

/// `parseBodyText`, or `parseViewText` for a distribution document: each paragraph of the
/// sections handed to `paragraph` as it is read, each section decrypted where the document is a
/// distribution document and inflated where it is compressed.
fn walk<E>(
    file: &Ole2<'_>,
    flags: u32,
    paragraph: &mut dyn FnMut(&[u16]) -> Result<(), E>,
) -> Result<(), Halt<E>> {
    let view_text = flags & VIEW_TEXT != 0;
    let name = if view_text { "ViewText" } else { "BodyText" };
    let directory = file
        .child(file.root(), name)
        .ok_or_else(|| no_such_entry(name))?;
    if file.kind(directory) == EntryKind::Document {
        return Err(io(IO_EXCEPTION, format!("Invalid {name}")).into());
    }
    let mut sections: Vec<(Option<u32>, EntryId)> = file
        .children(directory)
        .filter(|&entry| file.kind(entry) == EntryKind::Document)
        .filter_map(|entry| {
            let name = file.name(entry);
            let number = name.strip_prefix("Section")?;
            Some((number.parse().ok(), entry))
        })
        .collect();
    sections.sort_by_key(|&(number, _)| (number.is_none(), number));
    for (_, entry) in sections {
        let content = content(file, entry)?;
        let mut section: Box<dyn Read + '_> = Box::new(content.reader());
        if view_text {
            section = Box::new(Decrypting::new(section)?);
        }
        if flags & COMPRESSED != 0 {
            section = Box::new(poi::RawInflater::new(section));
        }
        records(&mut *section, paragraph)?;
    }
    Ok(())
}

/// The Java exception of what a section's reader fails with.
fn unread(error: std::io::Error) -> Thrown {
    match PoiThrown::of(error) {
        PoiThrown::Java(thrown) => thrown,
        PoiThrown::Told(reason) => io(IO_EXCEPTION, reason),
    }
}

/// What `IOUtils.readFully` reads into `buffer`: as many bytes as there are, up to its length.
fn fill(reader: &mut dyn Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// The `EOFException` `IOUtils.readFully` throws where it reads `actual` of `length` bytes.
fn short_read(length: impl std::fmt::Display, actual: impl std::fmt::Display) -> Thrown {
    io(EOF, format!("Length to read: {length} actual: {actual}"))
}

/// `IOUtils.readFully` of a whole `buffer`.
fn read_fully(reader: &mut dyn Read, buffer: &mut [u8]) -> Result<(), Thrown> {
    let read = fill(reader, buffer).map_err(unread)?;
    if read < buffer.len() {
        return Err(short_read(buffer.len(), read));
    }
    Ok(())
}

/// `SRand`: the C runtime's `rand`, which hides the key of a distribution document.
struct Random(i32);

impl Random {
    fn next(&mut self) -> i32 {
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        (self.0 >> 16) & 0x7FFF
    }
}

/// `readKey` and `createDecryptStream`: a distribution document's section after its first
/// record, the distribution document data, decrypted as it is read with AES-128 in ECB mode by
/// the key that the record's data hides, whole blocks only.
struct Decrypting<R> {
    input: R,
    cipher: aes::Aes128,
    block: [u8; AES_BLOCK],
    at: usize,
}

impl<R: Read> Decrypting<R> {
    fn new(mut input: R) -> Result<Self, Thrown> {
        read_fully(&mut input, &mut [0; DISTRIBUTION_HEADER])?;
        let mut data = [0u8; DISTRIBUTION_DATA];
        read_fully(&mut input, &mut data)?;
        let mut random = Random(i32::from_le_bytes(
            data[..4].try_into().expect("a seed of four bytes"),
        ));
        let (mut xor, mut left) = (0u8, 0);
        for (index, byte) in data.iter_mut().enumerate() {
            if left == 0 {
                xor = random.next() as u8;
                left = (random.next() & 0xF) + 1;
            }
            if index >= 4 {
                *byte ^= xor;
            }
            left -= 1;
        }
        let offset = 4 + usize::from(data[0] & 0xF);
        let cipher = aes::Aes128::new_from_slice(&data[offset..offset + AES_BLOCK])
            .expect("a key of sixteen bytes");
        Ok(Self {
            input,
            cipher,
            block: [0; AES_BLOCK],
            at: AES_BLOCK,
        })
    }
}

impl<R: Read> Read for Decrypting<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.at == AES_BLOCK {
            if fill(&mut self.input, &mut self.block)? < AES_BLOCK {
                return Ok(0);
            }
            self.cipher.decrypt_block((&mut self.block).into());
            self.at = 0;
        }
        let length = out.len().min(AES_BLOCK - self.at);
        out[..length].copy_from_slice(&self.block[self.at..self.at + length]);
        self.at += length;
        Ok(length)
    }
}

/// `parse`: the text of a section's paragraph text records handed to `paragraph`, each with a
/// line feed after it where it has any, the other records skipped; a record cut short throws
/// `EOFException`, as `HwpStreamReader` reads past the end of the section.
fn records<E>(
    section: &mut dyn Read,
    paragraph: &mut dyn FnMut(&[u16]) -> Result<(), E>,
) -> Result<(), Halt<E>> {
    let mut header = [0u8; 4];
    loop {
        match fill(section, &mut header).map_err(unread)? {
            0 => return Ok(()),
            4 => {}
            _ => return Err(Thrown::Io(Exception::bare(EOF)).into()),
        }
        let tag = u32::from_le_bytes(header) & 0x3FF;
        let mut length = u32::from_le_bytes(header) >> 20;
        if length == EXTENDED_LENGTH {
            if fill(section, &mut header).map_err(unread)? < 4 {
                return Err(Thrown::Io(Exception::bare(EOF)).into());
            }
            length = u32::from_le_bytes(header);
        }
        let mut record = Read::take(&mut *section, u64::from(length));
        if tag != PARA_TEXT {
            let skipped = std::io::copy(&mut record, &mut std::io::sink()).map_err(unread)?;
            if skipped < u64::from(length) {
                return Err(io(EOF, format!("Bytes to skip: {length} actual: {skipped}")).into());
            }
            continue;
        }
        if length % 2 != 0 {
            return Err(io(IO_EXCEPTION, "Invalid block size").into());
        }
        if length > MAX_TAG_LENGTH {
            return Err(Thrown::Tika(Exception::new(
                MEMORY_LIMIT,
                format!("Tags must be smaller than {MAX_TAG_LENGTH}"),
            ))
            .into());
        }
        let mut data = Vec::new();
        record.read_to_end(&mut data).map_err(unread)?;
        if data.len() < length as usize {
            return Err(short_read(length, data.len()).into());
        }
        let mut text = paragraph_text(&data);
        if !text.is_empty() {
            text.push(u16::from(b'\n'));
            paragraph(&text).map_err(Halt::Written)?;
        }
    }
}

/// `writeParaText`: the characters of a paragraph text record, a tab for a tab, a space for a
/// character control, and nothing for the other controls, each eight characters long.
fn paragraph_text(data: &[u8]) -> Vec<u16> {
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let mut text = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        match CONTROLS.get(usize::from(unit)) {
            None => text.push(unit),
            Some(Control::Character) => text.push(u16::from(b' ')),
            Some(Control::Inline | Control::Extended) => {
                if unit == u16::from(b'\t') {
                    text.push(unit);
                }
                index += 7;
            }
        }
        index += 1;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::ole2::{lpwstr, property_set, typed};
    use crate::ole2::{write, Node, Storage};

    /// `VT_I4`, which POI reads as an `Integer`.
    const VT_I4: i32 = 3;

    fn fixture(name: &str) -> (String, crate::Metadata) {
        crate::Extractor::new()
            .extract_file_to_string(&format!("tests/fixtures/hwp/{name}"))
            .unwrap()
    }

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn file_header(flags: u32) -> Vec<u8> {
        let mut header = vec![0u8; HEADER_LENGTH];
        header[..SIGNATURE.len()].copy_from_slice(SIGNATURE);
        header[32..36].copy_from_slice(&0x0500_0300u32.to_le_bytes());
        header[FLAGS_AT..FLAGS_AT + 4].copy_from_slice(&flags.to_le_bytes());
        header
    }

    /// A record of `tag` holding `data`, its length extended where it does not fit the header.
    fn record(tag: u32, data: &[u8]) -> Vec<u8> {
        let length = data.len() as u32;
        let mut record = Vec::new();
        if length < EXTENDED_LENGTH {
            record.extend((tag | length << 20).to_le_bytes());
        } else {
            record.extend((tag | EXTENDED_LENGTH << 20).to_le_bytes());
            record.extend(length.to_le_bytes());
        }
        record.extend(data);
        record
    }

    fn para_text(text: &[u16]) -> Vec<u8> {
        record(
            PARA_TEXT,
            &text
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect::<Vec<u8>>(),
        )
    }

    fn summary_information() -> Vec<u8> {
        property_set(
            [
                0x9F, 0xA2, 0xB6, 0x60, 0x10, 0x61, 0x11, 0xD4, 0xB4, 0xC6, 0x00, 0x60, 0x97, 0xC0,
                0x9D, 0x8C,
            ],
            &[
                (2, lpwstr("Havnen")),
                (4, lpwstr("Karen Blixen")),
                (5, typed(VT_I4, &3i32.to_le_bytes())),
                (14, typed(VT_I4, &7i32.to_le_bytes())),
            ],
        )
    }

    /// A document of `flags` with the summary information where given and the sections of
    /// `BodyText`, in the order given.
    fn document(flags: u32, summary: Option<Vec<u8>>, sections: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut entries = vec![(units(FILE_HEADER), Node::Owned(file_header(flags)))];
        entries.extend(summary.map(|summary| (units(SUMMARY_INFORMATION), Node::Owned(summary))));
        entries.push((
            units("BodyText"),
            Node::Storage(Storage {
                class_id: [0; 16],
                entries: sections
                    .into_iter()
                    .map(|(name, data)| (units(name), Node::Owned(data)))
                    .collect(),
            }),
        ));
        write(&Storage {
            class_id: [0; 16],
            entries,
        })
        .unwrap()
    }

    fn failure(input: &[u8]) -> crate::Error {
        let configs = ParserConfigs::default();
        let prepared = HwpV5Parser.prepare(input, &TikaMetadata::new(), &configs);
        let error = match prepared {
            Err(Decline::Fails(error)) => error,
            Err(other) => panic!("the document is declined otherwise: {other:?}"),
            Ok(_) => panic!("the document is read"),
        };
        error
    }

    /// A compressed document: its paragraphs, each with a line feed and its paragraph break a
    /// space, and the summary information Tika maps.
    #[test]
    fn reads_the_text_and_summary_of_a_compressed_document() {
        let (text, metadata) = fixture("testHWP_5.0.hwp");
        assert_eq!(text, "\u{d14c}\u{c2a4}\u{d2b8} \n\ntest \n\n");
        assert_eq!(metadata["Content-Type"], [MEDIA_TYPE]);
        assert_eq!(metadata["tk:parsed-by"], ["dijo::HwpV5Parser"]);
        assert_eq!(metadata["dc:title"], ["\u{d14c}\u{c2a4}\u{d2b8}"]);
        assert_eq!(metadata["dc:creator"], ["next1009"]);
        assert_eq!(metadata["meta:last-author"], ["next1009"]);
        assert_eq!(metadata["dcterms:created"], ["2015-09-03T07:12:23Z"]);
        assert_eq!(metadata["dcterms:modified"], ["2015-09-03T07:13:53Z"]);
        assert_eq!(metadata["meta:page-count"], ["0"]);
    }

    /// A distribution document's sections are decrypted with the key their first record hides.
    #[test]
    fn decrypts_the_sections_of_a_distribution_document() {
        let (text, metadata) = fixture("testHWP-v5-dist.hwp");
        assert!(
            text.starts_with("Apache Tika - 컨텐츠 분석 툴킷 \n\n"),
            "{text}"
        );
        assert!(text.contains("Apache Tika 1.19.1"), "{text}");
        assert_eq!(metadata["dc:title"], ["Apache Tika"]);
        assert_eq!(metadata["dc:creator"], ["SooMyung Lee"]);
        assert_eq!(metadata["meta:page-count"], ["1"]);
        assert_eq!(metadata["dcterms:modified"], ["2019-07-20T05:20:58Z"]);
    }

    /// An uncompressed document: sections in the order of their numbers, controls as
    /// `writeParaText` reads them, records of extended length skipped, empty paragraph text
    /// passed over, and a mistyped summary property left out.
    #[test]
    fn reads_an_uncompressed_document_section_by_section() {
        let mut controls = units("a\tb");
        controls.splice(2..2, [0; 7]);
        controls.extend([0x000B, 0, 0, 0, 0, 0, 0, 0x000B]);
        controls.extend(units("c\u{1e}d\r"));
        let mut first = record(0x42, &[0; 8]);
        first.extend(record(0x44, &vec![0; 5000]));
        first.extend(para_text(&controls));
        first.extend(para_text(&[]));
        let second = para_text(&units("Anden\r"));
        let input = document(
            0,
            Some(summary_information()),
            vec![("Section1", second), ("Section0", first)],
        );
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&input)
            .unwrap();
        assert_eq!(text, "a\tbc d \n\nAnden \n\n");
        assert_eq!(metadata["dc:title"], ["Havnen"]);
        assert_eq!(metadata["dc:creator"], ["Karen Blixen"]);
        assert_eq!(metadata["meta:page-count"], ["7"]);
        assert!(!metadata.contains_key("meta:keyword"), "{metadata:?}");
    }

    /// A document too large to read whole, its size in a section, is read as it is decrypted
    /// and inflated, in memory that does not grow with it.
    #[test]
    fn extracts_documents_too_large_to_read_whole_in_bounded_memory() {
        use crate::input::MAX_READ_WHOLE;
        use dijo_core::large::LargeFile;

        let filler = record(0x42, &vec![0; 1 << 20]);
        let mut section = para_text(&units("Havnen i \u{c5}lborg\r"));
        while section.len() as u64 <= MAX_READ_WHOLE {
            section.extend(&filler);
        }
        section.extend(para_text(&units("og fjorden\r")));
        let document = document(0, Some(summary_information()), vec![("Section0", section)]);
        let file = LargeFile::new("large.hwp", &document, 0, |_| Vec::new(), b"");
        drop(document);
        let (text, metadata) = file.extract("dijo::HwpV5Parser");
        assert_eq!(text, "Havnen i \u{c5}lborg \n\nog fjorden \n\n");
        assert_eq!(metadata["Content-Type"], [MEDIA_TYPE]);
    }

    #[test]
    fn fails_documents_tika_fails() {
        let encrypted = document(ENCRYPTED, Some(summary_information()), vec![]);
        let error = crate::Extractor::new()
            .extract_bytes_to_string(&encrypted)
            .unwrap_err();
        assert!(
            matches!(&error, crate::Error::Encrypted(message) if message.ends_with("EncryptedDocumentException: document is encrypted")),
            "{error:?}"
        );
        let unsummarized = document(0, None, vec![("Section0", para_text(&units("x")))]);
        assert!(failure(&unsummarized)
            .to_string()
            .ends_with("UnsupportedFormatException: cannot parse the Summary Information"));
        let mut truncated = para_text(&units("cut short"));
        truncated.truncate(10);
        let error = failure(&document(
            0,
            Some(summary_information()),
            vec![("Section0", truncated)],
        ));
        assert_eq!(
            error.to_string(),
            "Parse error occurred : org.apache.tika.exception.TikaException: error occurred when \
             parsing HWP Format, It may not HWP Format. <- caused by java.io.EOFException: Length \
             to read: 18 actual: 6"
        );
        let odd = record(PARA_TEXT, b"odd");
        assert!(failure(&document(
            0,
            Some(summary_information()),
            vec![("Section0", odd)]
        ))
        .to_string()
        .ends_with("java.io.IOException: Invalid block size"));
        let mut unsigned = document(0, Some(summary_information()), vec![]);
        let at = unsigned
            .windows(SIGNATURE.len())
            .position(|window| window == SIGNATURE)
            .unwrap();
        unsigned[at] = b'X';
        assert!(failure(&unsigned)
            .to_string()
            .ends_with("UnsupportedFormatException: cannot parse the File Header"));
    }

    #[test]
    fn takes_eight_characters_for_an_inline_or_extended_control() {
        let bytes = |units: &[u16]| {
            units
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect::<Vec<u8>>()
        };
        assert_eq!(paragraph_text(&bytes(&[0x41, 0x0A, 0x42])), units("A B"));
        assert_eq!(
            paragraph_text(&bytes(&[0x02, 1, 2, 3, 4, 5, 6, 0x02, 0x43])),
            units("C")
        );
        assert_eq!(paragraph_text(&bytes(&[0x41, 0x04])), units("A"));
    }
}
