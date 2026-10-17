//! Pages, Numbers and Keynote documents from iWork 2013 on, as our Java `IworkArchiveParser`
//! reads them: the protobuf archives (`.iwa`) of the package, stored directly or in an
//! `Index.zip`, each a series of Snappy-compressed chunks; the text of their text storages and
//! the strings of their tables' string lists as paragraphs, the document first, then the
//! slides, then the rest; then the files of the package's `Data` folders handed to the embedded
//! document extractor.
//!
//! The Java parser opens the package with the JDK's `ZipFile` and an `Index.zip` with its
//! `ZipInputStream`, which the port reads as [`jdk_zip`] does. Snappy blocks the snap crate
//! rejects, which commons-compress may still read, stay on Tika's path.

use std::collections::BTreeMap;

use crate::compress::zip::file::{ZipEntry, ZipFile};
use crate::compress::zip::Content;
use crate::config::ParserConfigs;
use crate::formats::embedded::{self, Wrapping};
use crate::formats::jdk_zip::{self, failed, Refusal};
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::java::is_blank;
use crate::java::strip;
use crate::java::{is_line_terminator, JavaString};
use crate::text::charset::Charset;

const TYPES: [&str; 7] = [
    "application/vnd.apple.pages.13",
    "application/vnd.apple.numbers.13",
    "application/vnd.apple.keynote.13",
    "application/vnd.apple.unknown.13",
    "application/vnd.apple.pages.18",
    "application/vnd.apple.numbers.18",
    "application/vnd.apple.keynote.18",
];

/// `TSWP.StorageArchive`: a text storage, its text in field 3.
const STORAGE: i64 = 2001;
/// `TST.TableDataList`: a table's list of values, its type in field 1, entries in field 3.
const TABLE_DATA_LIST: i64 = 6005;
const STRING_LIST: i64 = 1;
const MAX_DECOMPRESSED_BYTES: i64 = 256 << 20;
const CHUNK_HEADER_BYTES: usize = 4;
const ATTACHMENT: char = '\u{FFFC}';
const LINE_BREAKS: [char; 3] = ['\n', '\u{2028}', '\u{2029}'];
const ARCHIVE: &str = ".iwa";
const INDEX: &str = "Index.zip";
const DATA: &str = "Data/";

/// What `IworkArchiveParser.parse` writes: the paragraphs, then the failure that ends it or
/// the `Data` files it hands the extractor.
struct Extraction<'a> {
    paragraphs: Vec<String>,
    end: End<'a>,
}

enum End<'a> {
    Failed(String),
    /// The package and the indexes of its `Data` files among its entries.
    Embedded {
        zip: ZipFile<'a>,
        files: Vec<usize>,
    },
}

/// The extraction of `input`, or why it is left to Tika's path.
fn extract(input: &[u8]) -> Result<Extraction<'_>, &'static str> {
    let mut paragraphs = Vec::new();
    let end = match read_package(input, &mut paragraphs) {
        Ok(end) => end,
        Err(Refusal::Failed(reason)) => End::Failed(reason),
        Err(Refusal::Unread(why)) => return Err(why),
    };
    Ok(Extraction { paragraphs, end })
}

/// The package's paragraphs, added to `paragraphs` archive by archive, then its `Data` files.
fn read_package<'a>(input: &'a [u8], paragraphs: &mut Vec<String>) -> Result<End<'a>, Refusal> {
    let zip = jdk_zip::open(input)?;
    let mut budget = Budget(MAX_DECOMPRESSED_BYTES);
    let archives = archives(&zip, &mut budget)?;
    for (name, archive) in in_reading_order(archives) {
        if file_name(&name).starts_with("MasterSlide") {
            continue;
        }
        let stream = decompress(&archive, &mut budget)?;
        paragraphs.extend(text(&stream)?);
    }
    let files = data_files(&zip)?;
    Ok(End::Embedded { zip, files })
}

/// Caps how much the archives may decompress to, against decompression bombs.
struct Budget(i64);

impl Budget {
    fn spend(&mut self, bytes: usize) -> Result<(), Refusal> {
        self.0 -= i64::try_from(bytes).unwrap_or(i64::MAX);
        if self.0 < 0 {
            return Err(failed(format!(
                "The iWork archives decompress to more than {MAX_DECOMPRESSED_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

/// `zip.getInputStream(entry)` read to its end, an entry past the budget failing as it does.
fn content(zip: &ZipFile<'_>, entry: &ZipEntry) -> Result<Vec<u8>, Refusal> {
    match jdk_zip::read(zip, entry)? {
        Content::Complete(bytes) => Ok(bytes.into_owned()),
        _ => Err(failed(format!(
            "The iWork archives decompress to more than {MAX_DECOMPRESSED_BYTES} bytes"
        ))),
    }
}

/// `archives`: the `.iwa` archives of the package, stored directly or in an `Index.zip`, by
/// name in Java's order, a later one of a name replacing an earlier.
fn archives(
    zip: &ZipFile<'_>,
    budget: &mut Budget,
) -> Result<BTreeMap<JavaString, Vec<u8>>, Refusal> {
    let mut archives = BTreeMap::new();
    for entry in zip.entries() {
        if entry.name.ends_with(ARCHIVE) {
            let archive = content(zip, entry)?;
            budget.spend(archive.len())?;
            archives.insert(JavaString(entry.name.clone()), archive);
        } else if entry.name.ends_with(INDEX) {
            let index = jdk_zip::local_entries(&content(zip, entry)?)?;
            for (name, archive) in index
                .into_iter()
                .filter(|(name, _)| name.ends_with(ARCHIVE))
            {
                budget.spend(archive.len())?;
                archives.insert(JavaString(format!("{}/{name}", entry.name)), archive);
            }
        }
    }
    Ok(archives)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Where an archive comes in the reading order: its group, then its name without the number,
/// then the number, compared as a number.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReadingOrder {
    group: u8,
    name: JavaString,
    /// The number's digits without leading zeros, compared by length, then digit by digit.
    number: Option<(usize, String)>,
}

/// `readingOrder`: the document first, then the slides, then everything else (such as tables'
/// lists), each group by name and then by the archives' numbers. Our Java parser sorts keys of
/// numbers padded to ten digits in Java's string order, so an eleven-digit number goes before
/// a two-digit one, and fails on a number longer than a `long` when there are two or more
/// archives (#105).
fn reading_order(name: &str) -> ReadingOrder {
    if name == "Document.iwa" {
        return ReadingOrder {
            group: 0,
            name: JavaString(String::new()),
            number: None,
        };
    }
    let group = if name.starts_with("Slide") { 1 } else { 2 };
    if let Some((prefix, digits)) = numbered(name) {
        let digits = digits.trim_start_matches('0');
        return ReadingOrder {
            group,
            name: JavaString(prefix.to_owned()),
            number: Some((digits.len(), digits.to_owned())),
        };
    }
    ReadingOrder {
        group,
        name: JavaString(name.replace(ARCHIVE, "")),
        number: None,
    }
}

/// The groups of `(.*?)-(\d+)\.iwa` matching all of `name`.
fn numbered(name: &str) -> Option<(&str, &str)> {
    let (prefix, digits) = name.strip_suffix(ARCHIVE)?.rsplit_once('-')?;
    let numbered = !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && !prefix.contains(is_line_terminator);
    numbered.then_some((prefix, digits))
}

/// The archives sorted by their reading order, stably, as `List.sort` sorts them.
fn in_reading_order(archives: BTreeMap<JavaString, Vec<u8>>) -> Vec<(String, Vec<u8>)> {
    let mut keyed: Vec<(ReadingOrder, String, Vec<u8>)> = archives
        .into_iter()
        .map(|(JavaString(name), archive)| (reading_order(file_name(&name)), name, archive))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed
        .into_iter()
        .map(|(_, name, archive)| (name, archive))
        .collect()
}

/// `decompress`: an archive's protobuf stream, chunks of a zero byte, a three-byte
/// little-endian length and a raw Snappy block.
fn decompress(archive: &[u8], budget: &mut Budget) -> Result<Vec<u8>, Refusal> {
    let mut stream = Vec::new();
    let mut offset = 0;
    while offset < archive.len() {
        if archive[offset] != 0 || offset + CHUNK_HEADER_BYTES > archive.len() {
            return Err(failed(format!(
                "Corrupt iWork archive chunk at byte {offset}"
            )));
        }
        let length = usize::from(archive[offset + 1])
            | usize::from(archive[offset + 2]) << 8
            | usize::from(archive[offset + 3]) << 16;
        offset += CHUNK_HEADER_BYTES;
        let block = archive
            .get(offset..offset + length)
            .ok_or_else(|| failed(format!("Truncated iWork archive chunk at byte {offset}")))?;
        let declared = snap::raw::decompress_len(block)
            .map_err(|_| Refusal::Unread("a Snappy block the snap crate rejects"))?;
        budget.spend(declared)?;
        let decoded = snap::raw::Decoder::new()
            .decompress_vec(block)
            .map_err(|_| Refusal::Unread("a Snappy block the snap crate rejects"))?;
        stream.extend_from_slice(&decoded);
        offset += length;
    }
    Ok(stream)
}

/// `text`: the paragraphs of the text storages and string tables in an archive's messages.
fn text(stream: &[u8]) -> Result<Vec<String>, Refusal> {
    let mut paragraphs = Vec::new();
    let mut reader = Protobuf::new(stream);
    while reader.has_more() {
        let length = reader.varint()?;
        let mut info = reader.message(length)?;
        let mut messages = Vec::new();
        while info.has_more() {
            if info.field()? != 2 || info.wire_type != LENGTH_DELIMITED {
                info.skip()?;
                continue;
            }
            let length = info.varint()?;
            let mut message_info = info.message(length)?;
            let (mut kind, mut length) = (-1, 0);
            while message_info.has_more() {
                match message_info.varint_field()? {
                    Some(1) => kind = message_info.varint()?,
                    Some(3) => length = message_info.varint()?,
                    _ => message_info.skip()?,
                }
            }
            messages.push((kind, length));
        }
        for (kind, length) in messages {
            let payload = reader.message(length)?;
            match kind {
                STORAGE => storage(payload, &mut paragraphs)?,
                TABLE_DATA_LIST => strings(payload, &mut paragraphs)?,
                _ => {}
            }
        }
    }
    Ok(paragraphs)
}

/// `storage`: the lines of a text storage, without attachment characters.
fn storage(mut storage: Protobuf, paragraphs: &mut Vec<String>) -> Result<(), Refusal> {
    while storage.has_more() {
        if storage.field()? != 3 || storage.wire_type != LENGTH_DELIMITED {
            storage.skip()?;
            continue;
        }
        let text = storage.string()?.replace(ATTACHMENT, "");
        paragraphs.extend(
            text.split(LINE_BREAKS)
                .filter(|line| !is_blank(line))
                .map(|line| strip(line).to_owned()),
        );
    }
    Ok(())
}

/// `strings`: the strings of a table's string list, in the order of their keys.
fn strings(mut list: Protobuf, paragraphs: &mut Vec<String>) -> Result<(), Refusal> {
    let mut list_type = -1;
    let mut strings = BTreeMap::new();
    while list.has_more() {
        let field = list.field()?;
        match (field, list.wire_type) {
            (1, VARINT) => list_type = list.varint()?,
            (3, LENGTH_DELIMITED) => {
                let length = list.varint()?;
                let mut entry = list.message(length)?;
                let (mut key, mut string) = (-1, None);
                while entry.has_more() {
                    let field = entry.field()?;
                    match (field, entry.wire_type) {
                        (1, VARINT) => key = entry.varint()?,
                        (3, LENGTH_DELIMITED) => string = Some(entry.string()?),
                        _ => entry.skip()?,
                    }
                }
                if let Some(string) = string.filter(|string| !is_blank(string)) {
                    strings.insert(key, strip(&string).to_owned());
                }
            }
            _ => list.skip()?,
        }
    }
    if list_type == STRING_LIST {
        paragraphs.extend(strings.into_values());
    }
    Ok(())
}

/// `embedded`'s entries: the files of the package's `Data` folders, in the order of the central
/// directory, each read whole.
fn data_files(zip: &ZipFile<'_>) -> Result<Vec<usize>, Refusal> {
    let mut files = Vec::new();
    for (index, entry) in zip.entries().iter().enumerate() {
        if entry.is_directory() || !entry.name.contains(DATA) {
            continue;
        }
        jdk_zip::read_whole(zip, entry)?;
        files.push(index);
    }
    Ok(files)
}

/// The wire type of a varint.
const VARINT: i64 = 0;
/// The wire type of a length-delimited field.
const LENGTH_DELIMITED: i64 = 2;

/// Reads protobuf wire format: varints, length-delimited fields, and skips the rest. A field is
/// read only when its wire type is the one it is read as: our Java parser reads fields 1 and 3
/// whatever their wire type, so a fixed-width field misreads as a varint or a length (#105).
struct Protobuf<'a> {
    bytes: &'a [u8],
    offset: usize,
    end: usize,
    wire_type: i64,
}

impl<'a> Protobuf<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            offset: 0,
            end: bytes.len(),
            wire_type: 0,
        }
    }

    fn has_more(&self) -> bool {
        self.offset < self.end
    }

    /// The field number of the next key, as Java's `(int) (key >>> 3)` truncates it.
    fn field(&mut self) -> Result<i32, Refusal> {
        let key = self.varint()?;
        self.wire_type = key & 7;
        Ok(((key as u64) >> 3) as i32)
    }

    /// The field number of the next key when it is a varint's, else none.
    fn varint_field(&mut self) -> Result<Option<i32>, Refusal> {
        let field = self.field()?;
        Ok((self.wire_type == VARINT).then_some(field))
    }

    fn varint(&mut self) -> Result<i64, Refusal> {
        let mut value: i64 = 0;
        for shift in (0..64).step_by(7) {
            if self.offset >= self.end {
                return Err(failed("Truncated varint in an iWork archive"));
            }
            let next = self.bytes[self.offset];
            self.offset += 1;
            value |= i64::from(next & 0x7f) << shift;
            if next < 0x80 {
                return Ok(value);
            }
        }
        Err(failed("Overlong varint in an iWork archive"))
    }

    /// The next `length` bytes as a message, or `None` where they run past the end.
    fn take(&mut self, length: i64) -> Option<usize> {
        let length = usize::try_from(length).ok()?;
        (length <= self.end - self.offset).then(|| {
            let start = self.offset;
            self.offset += length;
            start
        })
    }

    fn message(&mut self, length: i64) -> Result<Protobuf<'a>, Refusal> {
        let start = self
            .take(length)
            .ok_or_else(|| failed("A message runs past the end of an iWork archive"))?;
        Ok(Protobuf {
            bytes: self.bytes,
            offset: start,
            end: self.offset,
            wire_type: 0,
        })
    }

    /// A string, decoded as Java's `new String(bytes, UTF_8)` decodes it.
    fn string(&mut self) -> Result<String, Refusal> {
        let length = self.varint()?;
        let start = self
            .take(length)
            .ok_or_else(|| failed("A string runs past the end of an iWork archive"))?;
        Charset::named("UTF-8")
            .decode(&self.bytes[start..self.offset])
            .ok_or(Refusal::Unread("UTF-8 the native decoders do not decode"))
    }

    fn skip(&mut self) -> Result<(), Refusal> {
        match self.wire_type {
            0 => {
                self.varint()?;
            }
            1 => self.offset += 8,
            2 => {
                let length = self.varint()?;
                self.message(length)?;
            }
            5 => self.offset += 4,
            other => {
                return Err(failed(format!(
                    "Unsupported protobuf wire type {other} in an iWork archive"
                )))
            }
        }
        if self.offset > self.end {
            return Err(failed("A field runs past the end of an iWork archive"));
        }
        Ok(())
    }
}

/// Our Java `IworkArchiveParser`.
pub(crate) struct IworkArchiveParser;

impl Parser for IworkArchiveParser {
    fn name(&self) -> &'static str {
        "extractous::IworkArchiveParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "ai.yobix.IworkArchiveParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &TYPES
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let extraction = extract(input).map_err(|why| Declined::Deferred(why.to_owned()))?;
        Ok(Box::new(move |xhtml| {
            for paragraph in &extraction.paragraphs {
                xhtml.element("p", paragraph)?;
            }
            let (zip, files) = match extraction.end {
                End::Failed(reason) => return Err(Failure::Malformed(reason)),
                End::Embedded { zip, files } => (zip, files),
            };
            for entry in files.into_iter().map(|index| &zip.entries()[index]) {
                let mut metadata = TikaMetadata::new();
                metadata.set(embedded::RESOURCE_NAME, file_name(&entry.name));
                metadata.set(embedded::RELATIONSHIP_ID, entry.name.as_str());
                let data = jdk_zip::read_whole(&zip, entry).map_err(|_| {
                    Failure::Malformed(format!("{} cannot be read again", entry.name))
                })?;
                xhtml.parse_embedded(&data, &metadata, Wrapping::PackageEntry)?;
            }
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::compress::zip::file::tests::zip;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };

    const TIKA_NAME: &str = "ai.yobix.IworkArchiveParser";
    const BUILD_HISTORY: &[u8] = b"<plist version=\"1.0\"><array/></plist>";

    fn varint(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        while value >= 0x80 {
            bytes.push(value as u8 | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
        bytes
    }

    fn key(field: u64, wire_type: u64) -> Vec<u8> {
        varint(field << 3 | wire_type)
    }

    fn varint_field(field: u64, value: u64) -> Vec<u8> {
        [key(field, 0), varint(value)].concat()
    }

    fn bytes_field(field: u64, bytes: &[u8]) -> Vec<u8> {
        [key(field, 2), varint(bytes.len() as u64), bytes.to_vec()].concat()
    }

    /// An archive record: its `ArchiveInfo`, with fields the parser skips, then its payload.
    fn record(kind: u64, payload: &[u8]) -> Vec<u8> {
        let message_info = [
            varint_field(1, kind),
            key(2, 5),
            vec![1, 2, 3, 4],
            varint_field(3, payload.len() as u64),
        ]
        .concat();
        let info = [
            varint_field(1, 7),
            bytes_field(2, &message_info),
            key(4, 1),
            vec![0; 8],
        ]
        .concat();
        [varint(info.len() as u64), info, payload.to_vec()].concat()
    }

    fn storage(text: &str) -> Vec<u8> {
        record(
            2001,
            &[varint_field(1, 1), bytes_field(3, text.as_bytes())].concat(),
        )
    }

    fn string_list(list_type: u64, strings: &[(u64, &str)]) -> Vec<u8> {
        let mut payload = varint_field(1, list_type);
        for (key, string) in strings {
            let entry = [varint_field(1, *key), bytes_field(3, string.as_bytes())].concat();
            payload.extend(bytes_field(3, &entry));
        }
        record(6005, &payload)
    }

    /// The records in Snappy chunks of at most 40 bytes of protobuf each.
    fn archive(records: &[Vec<u8>]) -> Vec<u8> {
        let mut archive = Vec::new();
        for chunk in records.concat().chunks(40) {
            let block = snap::raw::Encoder::new().compress_vec(chunk).unwrap();
            archive.push(0);
            archive.extend(&(block.len() as u32).to_le_bytes()[..3]);
            archive.extend(block);
        }
        archive
    }

    fn package(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let borrowed: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(name, content)| (*name, content.as_slice()))
            .collect();
        zip(&borrowed)
    }

    #[test]
    fn orders_archives_by_group_name_and_number() {
        // Our Java parser sorts numbers padded to ten digits as strings, and fails on one
        // longer than a `long` (#105).
        let mut names = [
            "Slide-12345678901.iwa",
            "DataList.iwa",
            "Slide-2.iwa",
            "Document.iwa",
            "Slide-99999999999999999999.iwa",
            "Slide-010.iwa",
            "a-1-2.iwa",
            "x-.iwa",
        ];
        names.sort_by_key(|name| reading_order(name));
        assert_eq!(
            names,
            [
                "Document.iwa",
                "Slide-2.iwa",
                "Slide-010.iwa",
                "Slide-12345678901.iwa",
                "Slide-99999999999999999999.iwa",
                "DataList.iwa",
                "a-1-2.iwa",
                "x-.iwa",
            ]
        );
    }

    #[test]
    fn reads_fields_only_by_their_wire_type() {
        // Field 3 of a storage as a fixed 32-bit value: our Java parser reads it as a string's
        // length (#105).
        let mut fixed = key(3, 5);
        fixed.extend_from_slice(&[4, 0, 0, 0]);
        let stream = decompress(
            &archive(&[record(2001, &[fixed, bytes_field(3, b"Kept")].concat())]),
            &mut Budget(MAX_DECOMPRESSED_BYTES),
        )
        .unwrap();
        assert_eq!(text(&stream).unwrap(), ["Kept"]);
    }

    #[test]
    fn extracts_packages_as_tikas_path_does() {
        let scratch = Scratch::new("iwork");
        let document = archive(&[
            storage("Title\nFirst line\u{2028}Second \u{FFFC}line\u{2029}  \n\n Last "),
            record(42, b"ignored"),
        ]);
        let pages = package(&[
            ("Metadata/BuildVersionHistory.plist", BUILD_HISTORY.to_vec()),
            ("Index/Document.iwa", document.clone()),
            (
                "Index/Tables/DataList-10.iwa",
                archive(&[string_list(
                    1,
                    &[(5, "five"), (2, "two"), (5, " five again "), (3, "  ")],
                )]),
            ),
            (
                "Index/Tables/DataList-9.iwa",
                archive(&[string_list(1, &[(1, "nine")])]),
            ),
            (
                "Index/Tables/DataList.iwa",
                archive(&[string_list(2, &[(1, "not a string list")])]),
            ),
            ("Data/note.txt", b"An embedded note.".to_vec()),
            ("Data/", Vec::new()),
        ]);
        let keynote = package(&[
            ("Metadata/BuildVersionHistory.plist", BUILD_HISTORY.to_vec()),
            ("Index/Slide-10.iwa", archive(&[storage("Slide ten")])),
            ("Index/Slide-2.iwa", archive(&[storage("Slide two")])),
            ("Index/MasterSlide.iwa", archive(&[storage("Placeholder")])),
            ("Index/Document.iwa", archive(&[storage("The deck")])),
        ]);
        let index = package(&[
            (
                "Index/Document.iwa",
                archive(&[storage("Inside Index.zip")]),
            ),
            ("Index/Thing.bin", b"not an archive".to_vec()),
            ("Index/Slide.iwa", archive(&[storage("A slide")])),
        ]);
        let keynote18 = package(&[
            (
                "Deck.key/Metadata/BuildVersionHistory.plist",
                BUILD_HISTORY.to_vec(),
            ),
            ("Deck.key/Index.zip", index),
        ]);
        let bad_snappy = package(&[
            ("Metadata/BuildVersionHistory.plist", BUILD_HISTORY.to_vec()),
            ("Index/Document.iwa", vec![0, 3, 0, 0, 0xFF, 0xFF, 0xFF]),
        ]);
        let failing_archive = |archive: Vec<u8>| {
            package(&[
                ("Metadata/BuildVersionHistory.plist", BUILD_HISTORY.to_vec()),
                ("Index/Document.iwa", document.clone()),
                ("Index/Other.iwa", archive),
            ])
        };
        let read = [
            scratch.write("text.pages", &pages),
            scratch.write("slides.key", &keynote),
            scratch.write("index.key", &keynote18),
        ];
        let failing = [
            scratch.write("corrupt-chunk.pages", &failing_archive(b"\x01abc".to_vec())),
            scratch.write(
                "wire-type.pages",
                &failing_archive(archive(&[record(2001, &key(4, 3))])),
            ),
            scratch.write(
                "truncated-message.pages",
                &failing_archive(archive(&[
                    record(2001, &bytes_field(3, b"text"))[..20].to_vec()
                ])),
            ),
        ];
        scratch.write("bad-snappy.pages", &bad_snappy);
        let (parsed, deferred) = documents(&IworkArchiveParser, &[scratch.directory()]);
        assert_eq!(deferred, [scratch.directory().join("bad-snappy.pages")]);
        assert_eq!(parsed.len(), read.len() + failing.len());
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
    }

    #[test]
    fn extracts_the_corpus_packages_as_tikas_path_does() {
        let (parsed, deferred) =
            documents(&IworkArchiveParser, &[Path::new("../test_files/documents")]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 2);
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_packages_as_tikas_path_does() {
        let (parsed, deferred) = documents(&IworkArchiveParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 5);
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
