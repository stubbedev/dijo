//! Visio drawings before Visio 2013 (VSD) as Tika's `OfficeParser` reads them with POI's HDGF:
//! the summary information as metadata, then a paragraph for each text `VisioTextExtractor`
//! finds. The document stream's trailer points to streams, compressed or not, which point to
//! more streams or hold chunks, among them the text chunks. A drawing POI throws on is left to
//! Tika, as are macros extracted when asked to.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{office, Summaries};
use crate::formats::poi::{i16_at, i32_at, safely_clone, thrown, u8_at, Lzw, Read, Thrown};
use crate::formats::{Declined, Parser, Prepared, Unported};
use crate::ole2::{EntryId, EntryKind, Ole2};

const MEDIA_TYPE: &str = "application/vnd.visio";
const DOCUMENT_STREAM: &str = "VisioDocument";
const VISIO_HEADER: &[u8; 20] = b"Visio (TM) Drawing\r\n";
/// Where the trailer's pointer is in the document stream.
const TRAILER_POINTER: usize = 0x24;
/// The pointer type of a trailer.
const TRAILER: i32 = 20;
/// `StreamStore.MAX_RECORD_LENGTH`.
const MAX_STREAM: i64 = 10_000_000;
/// `CompressedStreamStore.MAX_RECORD_LENGTH`.
const MAX_COMPRESSED_STREAM: i64 = 64_000_000;
/// `ChunkFactory.MAX_RECORD_LENGTH`.
const MAX_CHUNK: i64 = 1_000_000;
/// `PointerFactory.MAX_NUMBER_OF_POINTERS`.
const MAX_POINTERS: i64 = 100_000;
/// `PointerContainingStream.MAX_CHILDREN_NESTING`.
const MAX_NESTING: usize = 500;
/// The type of the chunks the command table names `Text`.
const TEXT_CHUNK: i32 = 14;
/// Where a text chunk's text starts.
const TEXT_START: usize = 8;
/// `HDGFLZW`: a set flag bit marks a byte, and a reference's position is relative.
const HDGF_LZW: Lzw = Lzw {
    mask_means_compressed: false,
    code_length_increase: 3,
    position_is_big_endian: false,
    preload: &[],
    adjust: |pointer| {
        if pointer > 4078 {
            pointer - 4078
        } else {
            pointer + 18
        }
    },
};

pub(crate) struct VisioParser;

impl Parser for VisioParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Defers drawings POI throws on and macros extracted when asked to.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let (summaries, texts) = read(input, configs)?;
        Ok(Box::new(move |xhtml| {
            summaries.write(xhtml.metadata_mut());
            xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
            for text in &texts {
                xhtml.element("p", text)?;
            }
            Ok(())
        }))
    }
}

/// The summary information and the texts `VisioTextExtractor.getAllText` gives.
fn read(input: &[u8], configs: &ParserConfigs) -> Result<(Summaries, Vec<String>), Unported> {
    let file = office::open(input, configs, MEDIA_TYPE, "Visio drawing")?;
    let root = file.root();
    let summaries = Summaries::read(&file, root)?;
    let texts = texts(&file, root)
        .map_err(|Thrown(reason)| Unported(format!("POI throws reading the drawing: {reason}")))?;
    Ok((summaries, texts))
}

/// `new HDGFDiagram(root)` and `getAllText`: the streams the trailer points to, read in order,
/// and the text of each of their text chunks.
fn texts(file: &Ole2<'_>, root: EntryId) -> Read<Vec<String>> {
    let Some(entry) = file.child_ignoring_case(root, DOCUMENT_STREAM) else {
        return thrown(format!("no document entry {DOCUMENT_STREAM}"));
    };
    if file.kind(entry) != EntryKind::Document {
        return thrown(format!("Entry '{DOCUMENT_STREAM}' is not a DocumentEntry"));
    }
    let document = file
        .read(entry)
        .map_err(|reason| Thrown(format!("cannot read {DOCUMENT_STREAM}: {}", reason.0)))?;
    if document.get(..VISIO_HEADER.len()) != Some(VISIO_HEADER.as_slice()) {
        return thrown("Wasn't a valid visio document");
    }
    let version = Version::of(i16_at(&document, 0x1a)?)?;
    i32_at(&document, 0x1c)?;
    let trailer = version.pointer(&document, TRAILER_POINTER)?;
    let reader = Reader {
        document: &document,
        version,
    };
    let store = reader.store(&trailer)?;
    if trailer.kind != TRAILER {
        return thrown("Stream is not a TrailerStream");
    }
    let mut texts = Vec::new();
    reader.children(&trailer, &store, 0, &mut texts)?;
    Ok(texts)
}

/// The version of a drawing, which sets how its pointers and chunks are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    /// Visio 5.
    V5,
    /// Visio 6.
    V6,
    /// After Visio 6: chunks may have separators, and their text is UTF-16.
    V11,
}

impl Version {
    /// `PointerFactory`'s and `ChunkFactory`'s versions: from 5 on.
    fn of(version: i16) -> Read<Self> {
        match version {
            ..5 => thrown(format!(
                "Visio files with versions below 5 are not supported, yours was {version}"
            )),
            5 => Ok(Self::V5),
            6 => Ok(Self::V6),
            _ => Ok(Self::V11),
        }
    }

    /// `PointerFactory.createPointer`.
    fn pointer(self, data: &[u8], at: usize) -> Read<Pointer> {
        Ok(match self {
            Self::V5 => Pointer {
                kind: i32::from(i16_at(data, at)?),
                format: i16_at(data, at + 2)?,
                offset: i32_at(data, at + 8)?,
                length: i32_at(data, at + 12)?,
                v6: false,
            },
            Self::V6 | Self::V11 => Pointer {
                kind: i32_at(data, at)?,
                offset: i32_at(data, at + 8)?,
                length: i32_at(data, at + 12)?,
                format: i16_at(data, at + 16)?,
                v6: true,
            },
        })
    }

    /// `ChunkHeader.getHeaderSize`.
    fn header_size(self) -> usize {
        match self {
            Self::V5 => 12,
            Self::V6 | Self::V11 => 19,
        }
    }
}

/// A pointer to a stream of the document.
#[derive(Debug, Clone, Copy)]
struct Pointer {
    kind: i32,
    offset: i32,
    length: i32,
    format: i16,
    /// Whether it is a `PointerV6`.
    v6: bool,
}

impl Pointer {
    fn format_between(self, min: i16, max: i16) -> bool {
        min <= self.format && self.format < max
    }

    fn has_pointers(self) -> bool {
        match (self.v6, self.kind) {
            (_, TRAILER) => true,
            (false, 22) => false,
            _ => self.format_between(0x1d, 0x1f) || self.format_between(0x50, 0x60),
        }
    }

    fn has_chunks(self) -> bool {
        match (self.v6, self.kind) {
            (false, 21 | 24) => true,
            _ => self.format_between(0xd0, 0xdf),
        }
    }

    fn compressed(self) -> bool {
        self.format & 2 > 0
    }

    fn size(self) -> usize {
        if self.v6 {
            18
        } else {
            16
        }
    }
}

/// A stream's contents, as its `StreamStore` holds them, and the block header a compressed
/// stream starts with.
struct Store {
    contents: Vec<u8>,
    block_header: Option<[u8; 4]>,
}

impl Store {
    /// `copyBlockHeaderToContents`: the contents of a chunk stream, a compressed one's after its
    /// block header.
    fn with_block_header(self) -> Read<Vec<u8>> {
        let Some(header) = self.block_header else {
            return Ok(self.contents);
        };
        if self.contents.len() as i64 + 4 > MAX_STREAM {
            return thrown(format!(
                "Tried to allocate an array of length {}",
                self.contents.len() + 4
            ));
        }
        Ok([header.as_slice(), &self.contents].concat())
    }
}

/// The document stream, read as streams of a drawing of `version`.
struct Reader<'d> {
    document: &'d [u8],
    version: Version,
}

impl Reader<'_> {
    /// `Stream.createStream`'s store: the bytes the pointer points to, decompressed where they
    /// are compressed.
    fn store(&self, pointer: &Pointer) -> Read<Store> {
        let (offset, length) = (i64::from(pointer.offset), i64::from(pointer.length));
        if !pointer.compressed() {
            return Ok(Store {
                contents: safely_clone(self.document, offset, length, MAX_STREAM)?,
                block_header: None,
            });
        }
        if offset < 0 || length < 0 {
            return thrown(format!(
                "offset {offset} or length {length} of a compressed stream is negative"
            ));
        }
        let size = self.document.len();
        let start = (offset as usize).min(size);
        let end = usize::try_from((start as i32).wrapping_add(pointer.length))
            .map_or(start, |end| end.min(size));
        let decompressed = HDGF_LZW.decompress(&self.document[start..end.max(start)]);
        let Some((header, contents)) = decompressed.split_first_chunk::<4>() else {
            return thrown(format!(
                "Could not read enough data to decompress: {}",
                decompressed.len()
            ));
        };
        let contents = safely_clone(contents, 0, contents.len() as i64, MAX_STREAM)?;
        safely_clone(self.document, offset, length, MAX_COMPRESSED_STREAM)?;
        Ok(Store {
            contents,
            block_header: Some(*header),
        })
    }

    /// `PointerContainingStream.findChildren`: the streams `parent`'s `store` points to, each
    /// read, its chunks found or its own children, and the texts of their text chunks added to
    /// `texts` in order.
    fn children(
        &self,
        parent: &Pointer,
        store: &Store,
        nesting: usize,
        texts: &mut Vec<String>,
    ) -> Read<()> {
        let pointers = self.container_pointers(parent, &store.contents)?;
        if nesting > MAX_NESTING {
            return thrown(format!(
                "Encountered too deep nesting, cannot process stream with more than \
                 {MAX_NESTING} nested children."
            ));
        }
        for pointer in &pointers {
            let store = self.store(pointer)?;
            if pointer.has_pointers() {
                self.children(pointer, &store, nesting + 1, texts)?;
            } else if pointer.has_chunks() {
                self.chunk_texts(&store.with_block_header()?, texts);
            }
        }
        Ok(())
    }

    /// `PointerFactory.createContainerPointers`.
    fn container_pointers(&self, parent: &Pointer, data: &[u8]) -> Read<Vec<Pointer>> {
        let (count_at, count, skip) = if parent.v6 {
            let count_at = exact_int(i32_at(data, 0)?)?;
            (count_at, exact_int(int_at(data, count_at)?)?, 8)
        } else {
            let count_at = match parent.kind {
                0x1d | 0x4e => 30,
                0x1e => 54,
                0x14 => 130,
                _ => 10,
            };
            (count_at, i32::from(short_at(data, count_at)?), 2)
        };
        if count < 0 {
            return thrown(format!(
                "Cannot create container pointers with negative count: {count}"
            ));
        }
        if i64::from(count) > MAX_POINTERS {
            return thrown(format!("Tried to allocate an array of length {count}"));
        }
        let mut position = i64::from(count_at) + skip;
        let mut pointers = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let at = usize::try_from(position)
                .map_err(|_| Thrown(format!("Index {position} out of bounds")))?;
            let pointer = self.version.pointer(data, at)?;
            position += pointer.size() as i64;
            pointers.push(pointer);
        }
        Ok(pointers)
    }

    /// `ChunkStream.findChunks` and `findText`: the texts of the chunks of `contents`, read
    /// until one fails, which ends the stream's chunks.
    fn chunk_texts(&self, contents: &[u8], texts: &mut Vec<String>) {
        let header_size = self.version.header_size();
        let mut position = 0;
        while position + header_size <= contents.len() {
            let Some(chunk) = self.chunk(contents, position) else {
                return;
            };
            if let Some(text) = chunk.text {
                if !(text.is_empty() || text == "\n") {
                    texts.push(text);
                }
            }
            position += chunk.on_disk_size;
        }
    }

    /// `ChunkFactory.createChunk` at `offset`, or `None` where it throws.
    fn chunk(&self, data: &[u8], offset: usize) -> Option<Chunk> {
        let header = ChunkHeader::read(self.version, data, offset)?;
        let header_size = self.version.header_size();
        let mut length = i64::from(header.length);
        if length < 0 {
            return None;
        }
        let mut end = offset as i64 + length + header_size as i64;
        if end > i64::from(i32::MAX) {
            return None;
        }
        let size = data.len() as i64;
        let (has_trailer, has_separator) = (header.has_trailer(), header.has_separator());
        if end > size {
            end = size;
            length = size - offset as i64 - header_size as i64;
            if has_trailer {
                length -= 8;
                end -= 8;
            }
            if has_separator {
                length -= 4;
                end -= 4;
            }
        }
        let mut extra = 0;
        if has_trailer && end <= size - 8 {
            if end < 0 {
                return None;
            }
            end += 8;
            extra += 8;
        }
        if has_separator && end <= size - 4 {
            if end < 0 {
                return None;
            }
            extra += 4;
        }
        let contents = safely_clone(data, (offset + header_size) as i64, length, MAX_CHUNK).ok()?;
        let text = (header.kind == TEXT_CHUNK && TEXT_START < contents.len())
            .then(|| self.text(&contents));
        Some(Chunk {
            on_disk_size: header_size + contents.len() + extra,
            text,
        })
    }

    /// The value of a text chunk's text command: from its eighth byte to the first two zero
    /// bytes after it, or to its end, in the chunk's charset.
    fn text(&self, contents: &[u8]) -> String {
        let end = (TEXT_START + 1..contents.len().saturating_sub(1))
            .find(|&at| contents[at] == 0 && contents[at + 1] == 0)
            .unwrap_or(contents.len());
        let bytes = &contents[TEXT_START..end];
        match self.version {
            Version::V11 => {
                let (units, rest) = bytes.as_chunks::<2>();
                let mut text = String::from_utf16_lossy(
                    &units
                        .iter()
                        .map(|pair| u16::from_le_bytes(*pair))
                        .collect::<Vec<_>>(),
                );
                if !rest.is_empty() {
                    text.push(char::REPLACEMENT_CHARACTER);
                }
                text
            }
            Version::V5 | Version::V6 => bytes
                .iter()
                .map(|&byte| {
                    if byte.is_ascii() {
                        char::from(byte)
                    } else {
                        char::REPLACEMENT_CHARACTER
                    }
                })
                .collect(),
        }
    }
}

/// A chunk read: how many bytes it takes, and the text of a text chunk that has one.
struct Chunk {
    on_disk_size: usize,
    text: Option<String>,
}

/// What a chunk's header says.
struct ChunkHeader {
    version: Version,
    kind: i32,
    length: i32,
    unknown1: i32,
    unknown2: i16,
    unknown3: u8,
}

impl ChunkHeader {
    /// `ChunkHeader.createChunkHeader`, or `None` where it throws.
    fn read(version: Version, data: &[u8], at: usize) -> Option<Self> {
        Some(match version {
            Version::V5 => Self {
                version,
                kind: i32::from(i16_at(data, at).ok()?),
                unknown3: u8_at(data, at + 5).ok()?,
                unknown1: i32::from(i16_at(data, at + 6).ok()?),
                length: i32::try_from(i32_at(data, at + 8).ok()? as u32).ok()?,
                unknown2: 0,
            },
            Version::V6 | Version::V11 => Self {
                version,
                kind: i32_at(data, at).ok()?,
                unknown1: i32_at(data, at + 8).ok()?,
                length: i32_at(data, at + 12).ok()?,
                unknown2: i16_at(data, at + 16).ok()?,
                unknown3: u8_at(data, at + 18).ok()?,
            },
        })
    }

    fn has_trailer(&self) -> bool {
        match self.version {
            Version::V5 => false,
            Version::V6 | Version::V11 => {
                matches!(
                    self.kind,
                    0x2c | 0x65 | 0x66 | 0x69 | 0x6a | 0x6b | 0x70 | 0x71
                ) || self.unknown1 != 0
            }
        }
    }

    fn has_separator(&self) -> bool {
        if self.version != Version::V11 {
            return false;
        }
        match self.kind {
            0x1f | 0xc9 => return false,
            0x69 => return true,
            0xa9 | 0xaa | 0xb4 | 0xb6 if self.unknown2 == 2 && self.unknown3 == 0x54 => {
                return true
            }
            _ => {}
        }
        if (self.unknown2 == 2 && self.unknown3 == 0x55)
            || (self.unknown2 == 3 && self.unknown3 != 0x50)
        {
            return true;
        }
        self.has_trailer()
    }
}

/// `LittleEndian.getShort` at a position Java holds as an `int`.
fn short_at(data: &[u8], at: i32) -> Read<i16> {
    let index = usize::try_from(at).map_err(|_| Thrown(format!("Index {at} out of bounds")))?;
    i16_at(data, index)
}

/// `LittleEndian.getUInt` at a position Java holds as an `int`.
fn int_at(data: &[u8], at: i32) -> Read<i32> {
    let index = usize::try_from(at).map_err(|_| Thrown(format!("Index {at} out of bounds")))?;
    i32_at(data, index)
}

/// `Math.toIntExact` of an unsigned 32-bit value read as `value`.
fn exact_int(value: i32) -> Read<i32> {
    if value < 0 {
        return thrown("integer overflow");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents};
    use std::path::{Path, PathBuf};

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_the_conformance_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&VisioParser, &[&conformance_corpus()]);
        assert_eq!(deferred, Vec::<PathBuf>::new());
        assert_eq!(parsed.len(), 1, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, VisioParser.tika_name());
    }

    /// `data` compressed as HDGF reads it: every byte a literal.
    fn literally(data: &[u8]) -> Vec<u8> {
        data.chunks(8)
            .flat_map(|chunk| std::iter::once(0xFF).chain(chunk.iter().copied()))
            .collect()
    }

    /// A Visio 2003 pointer: its type, offset, length and format.
    fn pointer(kind: u32, offset: usize, length: usize, format: u16) -> Vec<u8> {
        let mut bytes = kind.to_le_bytes().to_vec();
        bytes.extend(0u32.to_le_bytes());
        bytes.extend((offset as u32).to_le_bytes());
        bytes.extend((length as u32).to_le_bytes());
        bytes.extend(format.to_le_bytes());
        bytes
    }

    /// A text chunk of Visio 2003 holding `text`, and a chunk of type 0x2c with a trailer.
    fn chunks(text: &str) -> Vec<u8> {
        let mut contents = vec![0u8; TEXT_START];
        contents.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let header = |kind: u32, length: usize| {
            let mut bytes = kind.to_le_bytes().to_vec();
            bytes.extend([0; 8]);
            bytes.extend((length as u32).to_le_bytes());
            bytes.extend([0; 3]);
            bytes
        };
        let mut data = header(0x2c, 4);
        data.extend([1, 2, 3, 4]);
        data.extend([0; 12]);
        data.extend(header(TEXT_CHUNK as u32, contents.len()));
        data.extend(contents);
        data
    }

    /// A Visio 2003 drawing whose trailer points to a compressed stream of chunks holding
    /// `first` and an uncompressed stream of pointers to one holding `second`.
    fn drawing(first: &str, second: &str) -> Vec<u8> {
        use crate::ole2::{write, Node, Storage};
        let mut document = VISIO_HEADER.to_vec();
        document.resize(0x1a, 0);
        document.extend(11i16.to_le_bytes());
        document.extend(0u32.to_le_bytes());
        document.resize(0x24 + 18, 0);
        let compressed = literally(&chunks(first));
        let compressed_at = document.len();
        document.extend(&compressed);
        let second_chunks = chunks(second);
        let second_at = document.len();
        document.extend(&second_chunks);
        let mut inner = 4u32.to_le_bytes().to_vec();
        inner.extend(1u32.to_le_bytes());
        inner.extend([0; 4]);
        inner.extend(pointer(0x15, second_at, second_chunks.len(), 0xd0));
        let inner_at = document.len();
        document.extend(&inner);
        let mut trailer = 4u32.to_le_bytes().to_vec();
        trailer.extend(2u32.to_le_bytes());
        trailer.extend([0; 4]);
        trailer.extend(pointer(0x15, compressed_at, compressed.len(), 0xd2));
        trailer.extend(pointer(0x16, inner_at, inner.len(), 0x54));
        let trailer_at = document.len();
        document.extend(&trailer);
        let trailer_pointer = pointer(20, trailer_at, trailer.len(), 0);
        document[0x24..0x24 + 18].copy_from_slice(&trailer_pointer);
        write(&Storage {
            class_id: [0; 16],
            entries: vec![(
                DOCUMENT_STREAM.encode_utf16().collect(),
                Node::Document(document),
            )],
        })
        .unwrap()
    }

    #[test]
    fn extracts_the_texts_of_compressed_and_nested_streams_as_tikas_path_does() {
        let scratch = crate::formats::parity::Scratch::new("visio");
        let path = scratch.write("havn.vsd", &drawing("Havnen i \u{c5}lborg", "Fjorden"));
        assert_eq!(crate::formats::parity::deferral(&VisioParser, &path), None);
        assert_same_as_tika(&[&path], VisioParser.tika_name());
        let (text, _) = crate::Extractor::new()
            .extract_file_to_string(path.to_str().unwrap())
            .unwrap();
        assert!(
            text.contains("Havnen i \u{c5}lborg") && text.contains("Fjorden"),
            "{text}"
        );
    }

    #[test]
    fn decompresses_as_hdgf_does() {
        let literal = [0xFF, b'a', b'b', b'c', b'd', b'e', b'f', b'g', b'h'];
        assert_eq!(HDGF_LZW.decompress(&literal), b"abcdefgh");
        assert_eq!(
            HDGF_LZW.decompress(&[0x03, b'a', b'b', 0xEE, 0xF0]),
            b"ababa"
        );
    }

    #[test]
    fn reads_a_text_chunk_to_its_first_two_zero_bytes_after_its_start() {
        let reader = Reader {
            document: &[],
            version: Version::V11,
        };
        let mut contents = vec![0u8; TEXT_START];
        contents.extend("Rum".encode_utf16().flat_map(u16::to_le_bytes));
        contents.extend([0, 0, b'x', 0]);
        assert_eq!(reader.text(&contents), "Ru\u{fffd}");
        let mut leading = vec![0u8; TEXT_START + 2];
        leading.extend([b'a', 0]);
        assert_eq!(reader.text(&leading), "\0a");
    }
}
