//! Publisher documents (PUB) as Tika's `OfficeParser` reads them with POI's HPBF: the summary
//! information as metadata, then the text of the Quill contents' text bits, every carriage
//! return a line break, in one paragraph, as `PublisherTextExtractor` gives it, and the macros
//! after it when asked for. A document POI throws on, in its Quill contents or its main
//! contents, fails, as it fails Tika's parse.
//!
//! HPBF also reads the Escher streams, which hold no text, and reads each of their records at
//! the start of the stream rather than where it lies, so that a document whose streams are
//! missing or whose first record it cannot read fails Tika's parse; the port does not read them
//! (#129).

use std::borrow::Cow;

use crate::config::ParserConfigs;
use crate::formats::compound::detected_otherwise;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::office;
use crate::formats::ole2::vba::Macros;
use crate::formats::ole2::Summaries;
use crate::formats::poi::{i32_at, safely_borrow, thrown, u16_at, u8_at, Read, Thrown, Unread};
use crate::formats::{Decline, Footprint, Parser, Prepared};
use crate::ole2::{EntryId, EntryKind, Ole2};

const MEDIA_TYPE: &str = "application/x-mspublisher";
/// `QuillContents`' path to its stream.
const QUILL_CONTENTS: [&str; 3] = ["Quill", "QuillSub", "CONTENTS"];
const MAIN_CONTENTS: [&str; 1] = ["Contents"];
/// What the Quill contents start with.
const QUILL_SIGNATURE: &[u8; 8] = b"CHNKINK ";
/// How many bits the Quill contents' index lists.
const BITS: usize = 20;
/// Where the index starts, and how long each of its entries is.
const INDEX_START: usize = 0x20;
const INDEX_ENTRY: usize = 24;
/// `EscherPart.getMaxRecordLength`, which HPBF also bounds a bit's data by.
const MAX_RECORD_LENGTH: i64 = 1_000_000;
/// `QCPLCBit.MAX_NUMBER_OF_PLCS`.
const MAX_PLCS: i64 = 1000;

pub struct PublisherParser;

impl Parser for PublisherParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }
    fn tika_name(&self) -> &'static str {
        crate::formats::poi::OFFICE_PARSER
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// It reads the main and Quill contents in place. What it holds is the text of the text
    /// bits, each at most a megabyte, which `PublisherTextExtractor` gives whole.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Fails documents POI throws on; writes the macros after the text when asked to.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        if let Some(parser) = detected_otherwise(input, &[MEDIA_TYPE]) {
            return parser.prepare(input, metadata, configs);
        }
        let (summaries, text, macros) = read(input, configs).map_err(Unread::declined)?;
        Macros::followed_by(
            Ok(Box::new(move |xhtml| {
                summaries.write(xhtml.metadata_mut());
                xhtml.metadata_mut().set("Content-Type", MEDIA_TYPE);
                if !text.is_empty() {
                    xhtml.start_element("p")?;
                    xhtml.characters_utf16(&text)?;
                    xhtml.end_element("p")?;
                }
                Ok(())
            })),
            macros,
        )
    }
}

/// The summary information and the text `PublisherTextExtractor` gives of the document, and
/// its macros where they are asked for.
fn read(
    input: &[u8],
    configs: &ParserConfigs,
) -> Result<(Summaries, Vec<u16>, Option<Macros>), Unread> {
    let file = office::open(input, MEDIA_TYPE, "Publisher document")?;
    let root = file.root();
    let summaries = Summaries::read(&file, root)?;
    let text = text(&file, root)?;
    Ok((summaries, text, Macros::of(&file, configs)))
}

/// `new HPBFDocument(root)` and `PublisherTextExtractor.getText`: the text of the Quill
/// contents' text bits, once the main contents and the Quill contents are read.
fn text(file: &Ole2<'_>, root: EntryId) -> Read<Vec<u16>> {
    let _main = part(file, root, &MAIN_CONTENTS)?;
    let quill = part(file, root, &QUILL_CONTENTS)?;
    let bits = quill_bits(&quill)?;
    let mut text = Vec::new();
    for bit in bits {
        if let Bit::Text(data) = bit {
            text.extend(data.as_chunks::<2>().0.iter().map(|pair| {
                match u16::from_le_bytes(*pair) {
                    0x0D => 0x0A,
                    unit => unit,
                }
            }));
        }
    }
    Ok(text)
}

/// `HPBFPart`: the data of the stream at `path`, its directories named exactly and the stream in
/// any case, in place where it lies in one run.
fn part<'a>(file: &Ole2<'a>, root: EntryId, path: &[&str]) -> Read<Cow<'a, [u8]>> {
    let (name, directories) = path.split_last().expect("a part's path names its stream");
    let mut directory = root;
    for &name in directories {
        let Some(entry) = file.child(directory, name) else {
            return thrown(format!(
                "File invalid - failed to find directory entry '{name}'"
            ));
        };
        if file.kind(entry) == EntryKind::Document {
            return thrown(format!("Had unexpected type of entry for path: {name}"));
        }
        directory = entry;
    }
    let Some(entry) = file.child_ignoring_case(directory, name) else {
        return thrown(format!(
            "File invalid - failed to find document entry '{name}'"
        ));
    };
    if file.kind(entry) != EntryKind::Document {
        return thrown(format!("Entry '{name}' is not a DocumentEntry"));
    }
    file.content(entry)
        .map(|content| content.bytes())
        .map_err(|reason| Thrown::new(format!("cannot read the stream {name}: {}", reason.0)))
}

/// A bit of the Quill contents, as far as the text needs it, its data in place.
enum Bit<'a> {
    Text(&'a [u8]),
    Other,
}

/// `QuillContents`: the bits its index lists, each an entry that starts `18 00`.
fn quill_bits(data: &[u8]) -> Read<Vec<Bit<'_>>> {
    if data.get(..8) != Some(QUILL_SIGNATURE.as_slice()) {
        return thrown("Expecting 'CHNKINK '");
    }
    let mut bits = Vec::new();
    for index in 0..BITS {
        let offset = INDEX_START + index * INDEX_ENTRY;
        if u8_at(data, offset)? != 0x18 || u8_at(data, offset + 1)? != 0x00 {
            continue;
        }
        let entry = data.get(offset + 2..offset + INDEX_ENTRY).ok_or_else(|| {
            Thrown::new(format!(
                "Index {} out of bounds for length {}",
                data.len(),
                data.len()
            ))
        })?;
        let bit_type = &entry[10..14];
        let from = i32_at(entry, 14)?;
        let length = i32_at(entry, 18)?;
        let bit_data = safely_borrow(data, i64::from(from), i64::from(length), MAX_RECORD_LENGTH)?;
        bits.push(match bit_type {
            b"TEXT" => Bit::Text(bit_data),
            b"PLC " => {
                plc_bit(bit_data)?;
                Bit::Other
            }
            _ => Bit::Other,
        });
    }
    Ok(bits)
}

/// Whether a PLC bit throws what `QuillContents` does not catch: reading past its data is
/// caught, and the bit kept as unknown.
fn plc_bit(data: &[u8]) -> Read<()> {
    match plc_bit_read(data) {
        Err(Plc::OutOfBounds) | Ok(()) => Ok(()),
        Err(Plc::Thrown(thrown)) => Err(thrown),
    }
}

/// How reading a PLC bit fails: past its data, which `QuillContents` catches, or otherwise.
enum Plc {
    OutOfBounds,
    Thrown(Thrown),
}

/// `QCPLCBit.createQCPLCBit`, as far as it can throw.
fn plc_bit_read(data: &[u8]) -> Result<(), Plc> {
    let bounds = |read: Read<i64>| read.map_err(|_| Plc::OutOfBounds);
    let uint = |at: usize| bounds(i32_at(data, at).map(|value| i64::from(value as u32)));
    let ushort = |at: usize| bounds(u16_at(data, at).map(i64::from));
    let plc_type = uint(4)? as i32;
    if !matches!(plc_type, 0 | 4 | 8 | 12) {
        return Err(Plc::Thrown(Thrown::new(format!(
            "Sorry, I don't know how to deal with PLCs of type {plc_type}"
        ))));
    }
    let count = uint(0)? as i32;
    if count < 0 {
        return Err(Plc::Thrown(Thrown::new(format!(
            "Invalid number of PLCs: {count}"
        ))));
    }
    if i64::from(count) > MAX_PLCS {
        return Err(Plc::Thrown(Thrown::new(format!(
            "Tried to allocate an array of length {count}, but the maximum length for this \
             record type is {MAX_PLCS}."
        ))));
    }
    let count = count as usize;
    match plc_type {
        0 => {
            for at in [8, 10, 12, 14] {
                ushort(at)?;
            }
            for i in 0..count {
                ushort(16 + 4 * i)?;
                ushort(16 + 4 * i + 2)?;
            }
        }
        4 => {
            for at in [8, 10, 12, 14] {
                ushort(at)?;
            }
            for i in 0..count {
                uint(16 + 8 * i)?;
                uint(16 + 8 * i + 4)?;
            }
        }
        8 => {
            for at in [8, 10, 12, 14, 16, 18, 20] {
                ushort(at)?;
            }
            for i in 0..count {
                uint(22 + 8 * i)?;
                uint(22 + 8 * i + 4)?;
            }
        }
        _ => hyperlinks(data, count)?,
    }
    Ok(())
}

/// `QCPLCBit.Type12`'s reading of its hyperlinks.
fn hyperlinks(data: &[u8], count: usize) -> Result<(), Plc> {
    const ONE_STARTS_AT: usize = 0x4c;
    const TWO_STARTS_AT: usize = 0x68;
    const THREE_PLUS_INCREMENT: usize = 22;
    let bounds = |read: Read<u16>| read.map_err(|_| Plc::OutOfBounds);
    let links = if data.len() == 0x34 { 0 } else { count };
    for i in 0..count + 2 {
        bounds(i32_at(data, 8 + i * 4).map(|value| value as u16))?;
    }
    let mut at = 8 + 4 + count * 4 + 4;
    let until = if count == 1 && links == 1 {
        ONE_STARTS_AT
    } else if count >= 2 {
        TWO_STARTS_AT + (count - 2) * THREE_PLUS_INCREMENT
    } else {
        0x34
    };
    for i in 0..(until - at) / 2 {
        bounds(u16_at(data, at + i * 2))?;
    }
    at = until;
    for _ in 0..links {
        let length = usize::from(bounds(u16_at(data, at))?);
        let first = bounds(u16_at(data, at + 2))?;
        if first == 0 {
            at += length;
            continue;
        }
        let offset = at + 2;
        if offset >= data.len() {
            return Err(Plc::OutOfBounds);
        }
        if (data.len() - offset) / 2 < length {
            return Err(Plc::Thrown(Thrown::new(format!("Illegal length {length}"))));
        }
        at += 2 + 2 * length;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quill contents of the bits given as their type and data.
    fn quill(bits: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut quill = vec![0u8; INDEX_START + BITS * INDEX_ENTRY];
        quill[..8].copy_from_slice(QUILL_SIGNATURE);
        for (index, (kind, data)) in bits.iter().enumerate() {
            let from = quill.len() as u32;
            let at = INDEX_START + index * INDEX_ENTRY;
            let entry = &mut quill[at..at + INDEX_ENTRY];
            entry[..2].copy_from_slice(&[0x18, 0x00]);
            entry[2..6].copy_from_slice(b"STSH");
            entry[12..16].copy_from_slice(*kind);
            entry[16..20].copy_from_slice(&from.to_le_bytes());
            entry[20..24].copy_from_slice(&(data.len() as u32).to_le_bytes());
            quill.extend(data);
        }
        quill
    }

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// A document too large to read whole, its size in its main contents, is read in place,
    /// in memory that does not grow with it.
    #[test]
    fn extracts_documents_too_large_to_read_whole_in_bounded_memory() {
        use crate::input::MAX_READ_WHOLE;
        use crate::ole2::{write, Node, Storage};
        use extractous_core::large::LargeFile;

        let units = |name: &str| name.encode_utf16().collect::<Vec<u16>>();
        let storage = |entries| {
            Node::Storage(Storage {
                class_id: [0; 16],
                entries,
            })
        };
        let quill = quill(&[(b"TEXT", utf16("Havnen i \u{c5}lborg\rog fjorden"))]);
        let Node::Storage(root) = storage(vec![
            (
                units("Contents"),
                Node::Owned(vec![0; MAX_READ_WHOLE as usize + 1]),
            ),
            (
                units("Quill"),
                storage(vec![(
                    units("QuillSub"),
                    storage(vec![(units("CONTENTS"), Node::Owned(quill))]),
                )]),
            ),
        ]) else {
            unreachable!("a storage")
        };
        let document = write(&root).unwrap();
        drop(root);
        let file = LargeFile::new("large.pub", &document, 0, |_| Vec::new(), b"");
        drop(document);
        let (text, metadata) = file.extract("extractous::OfficeParser");
        assert!(text.contains("Havnen i \u{c5}lborg\nog fjorden"), "{text}");
        assert_eq!(metadata["Content-Type"], ["application/x-mspublisher"]);
    }

    #[test]
    fn reads_the_text_bits_and_throws_where_poi_throws() {
        let mut quill = vec![0u8; INDEX_START + BITS * INDEX_ENTRY];
        quill[..8].copy_from_slice(QUILL_SIGNATURE);
        let text: Vec<u8> = "Hej\rverden"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let from = quill.len() as u32;
        let entry = &mut quill[INDEX_START..INDEX_START + INDEX_ENTRY];
        entry[..2].copy_from_slice(&[0x18, 0x00]);
        entry[12..16].copy_from_slice(b"TEXT");
        entry[16..20].copy_from_slice(&from.to_le_bytes());
        entry[20..24].copy_from_slice(&(text.len() as u32).to_le_bytes());
        quill.extend(&text);
        let bits = quill_bits(&quill).unwrap();
        assert!(matches!(&bits[..], [Bit::Text(data)] if **data == text[..]));
        assert!(quill_bits(b"CHNKINX ").is_err());
        let mut plc = vec![0u8; 16];
        plc[4] = 3;
        assert!(plc_bit(&plc).is_err());
        assert!(plc_bit(&[0, 0]).is_ok());
    }
}
