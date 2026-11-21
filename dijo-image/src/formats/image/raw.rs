//! Camera raw images, as Tika's `RawTiffParser` reads them: the TIFF's XMP and
//! metadata-extractor's directories, as Tika's `TiffParser` reads them, without OCR; then the
//! JPEG previews the IFDs, a Panasonic RW2's own tag, a Leaf MOS's data block and an Olympus ORF's
//! or Epson ERF's maker note hold, largest first, the first as a thumbnail and the others as
//! inline images, parsed as embedded documents.
//!
//! What metadata-extractor reads that the port does not is left out of the metadata, which the
//! reading records as its difference from Tika's: a maker note read as directories of its own,
//! and the raw IFD0 of a Panasonic RW2.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{HashSet, VecDeque};

use super::metadata_extractor::exif::MakerNotes;
use super::{dates_zone, extract_tiff_metadata, image_xmp, Read};
use crate::config::ParserConfigs;
use crate::formats::embedded::{self, ResourceType, Wrapping};
use crate::formats::metadata::office::RESOURCE_NAME_EXTENSION_INFERRED;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Difference, Footprint, Opened, Parser, Prepared};
use crate::input::{Pages, Paging};
use crate::mime::{self, MediaType};

/// The type of the previews.
const JPEG: &str = "image/jpeg";
const TAG_BITS_PER_SAMPLE: u16 = 0x0102;
const TAG_COMPRESSION: u16 = 0x0103;
const TAG_PHOTOMETRIC_INTERPRETATION: u16 = 0x0106;
const TAG_STRIP_OFFSETS: u16 = 0x0111;
const TAG_STRIP_BYTE_COUNTS: u16 = 0x0117;
const TAG_SUB_IFDS: u16 = 0x014A;
const TAG_JPEG_INTERCHANGE_FORMAT: u16 = 0x0201;
const TAG_JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 0x0202;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_MAKER_NOTE: u16 = 0x927C;
/// Panasonic's `JpgFromRaw`, the whole preview in IFD0.
const TAG_PANASONIC_JPG_FROM_RAW: u16 = 0x002E;
/// In an Olympus maker note: the camera's thumbnail, and the sub-IFD that points at the preview.
const TAG_OLYMPUS_THUMBNAIL: u16 = 0x0100;
const TAG_OLYMPUS_CAMERA_SETTINGS: u16 = 0x2020;
const TAG_OLYMPUS_PREVIEW_START: u16 = 0x0101;
const TAG_OLYMPUS_PREVIEW_LENGTH: u16 = 0x0102;
/// In an Epson maker note: the preview.
const TAG_EPSON_PREVIEW: u16 = 0x0280;
/// Leaf's data, a chain of named blocks.
const TAG_LEAF_DATA: u16 = 0x8606;
const LEAF_BLOCK_MAGIC: &[u8] = b"PKTS";
const LEAF_PREVIEW_NAME: &[u8] = b"JPEG_preview_data";
/// A Leaf block's magic and version, then its padded name, then its payload's length.
const LEAF_BLOCK_NAME_OFFSET: usize = 8;
const LEAF_BLOCK_NAME_LENGTH: usize = 40;
const LEAF_BLOCK_HEADER_LENGTH: usize = LEAF_BLOCK_NAME_OFFSET + LEAF_BLOCK_NAME_LENGTH + 4;
/// The most Leaf blocks the walk reads.
const MAX_LEAF_BLOCKS: usize = 64;
const OLYMPUS_MAKER_NOTE_NAME: &[u8] = b"OLYMPUS\0";
/// What OM Digital Solutions bodies write, with the same layout behind it.
const OM_SYSTEM_MAKER_NOTE_NAME: &[u8] = b"OM SYSTEM\0\0\0";
const EPSON_MAKER_NOTE_NAME: &[u8] = b"EPSON\0";
/// An Epson maker note's name and version, then its IFD, whose offsets count from the start of
/// the file.
const EPSON_MAKER_NOTE_HEADER_LENGTH: i64 = 8;
/// An Olympus maker note's name, its own byte order marker and its version, then its IFD, whose
/// offsets count from the start of the maker note.
const OLYMPUS_MAKER_NOTE_HEADER_LENGTH: i64 = 12;
const OM_SYSTEM_MAKER_NOTE_HEADER_LENGTH: i64 = 16;
/// The most Exif IFDs whose maker note the walk looks up.
const MAX_MAKER_NOTE_LOOKUPS: usize = 4;
const CLASSIC_ENTRY_SIZE: usize = 12;
const CLASSIC_INLINE_SIZE: i64 = 4;
const TYPE_BYTE: i64 = 1;
const TYPE_ASCII: i64 = 2;
const TYPE_UNDEFINED: i64 = 7;
const COMPRESSION_OLD_JPEG: i64 = 6;
const COMPRESSION_JPEG: i64 = 7;
const PHOTOMETRIC_RGB: i64 = 2;
const PHOTOMETRIC_YCBCR: i64 = 6;
/// The most IFDs the walk visits.
const MAX_IFDS: usize = 32;
/// The most entries an IFD the walk reads may have, and the most values of a tag it reads.
const MAX_ENTRIES_PER_IFD: i64 = 1024;
/// The most IFDs the walk keeps waiting to be visited.
const MAX_PENDING_IFDS: usize = 1024;
/// `RawTiffParserConfig`'s default `maxPreviewLengthBytes`.
const MAX_PREVIEW_LENGTH: i64 = 100 * 1024 * 1024;
/// `RawTiffParserConfig`'s default `maxTotalPreviewBytes`.
const MAX_TOTAL_PREVIEW_BYTES: i64 = 256 * 1024 * 1024;

/// The layout the magic number after the byte order marker names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Magic {
    Tiff,
    BigTiff,
    /// An Olympus ORF: `IIRO`, `IIRS` or `MMOR`.
    Olympus,
    /// A Panasonic RW2: `IIU\0`.
    Panasonic,
}

impl Magic {
    /// `readMagic` and `isKnownMagic`: the layout of a file that starts with a byte order marker
    /// and a magic number Tika knows.
    fn of(input: &[u8]) -> Option<Self> {
        let big_endian = match input.get(..2)? {
            b"MM" => true,
            b"II" => false,
            _ => return None,
        };
        Self::named(get_u16(input.get(..4)?, 2, big_endian))
    }

    fn named(magic: i64) -> Option<Self> {
        match magic {
            42 => Some(Self::Tiff),
            43 => Some(Self::BigTiff),
            0x4F52 | 0x5352 => Some(Self::Olympus),
            0x0055 => Some(Self::Panasonic),
            _ => None,
        }
    }
}

/// Tika's `RawTiffParser`.
pub struct RawTiffParser;

impl Parser for RawTiffParser {
    fn name(&self) -> &'static str {
        "dijo::RawTiffParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.image.RawTiffParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "image/x-raw-nikon",
            "image/x-raw-sony",
            "image/x-raw-pentax",
            "image/x-raw-samsung",
            "image/x-raw-epson",
            "image/x-raw-kodak",
            "image/x-raw-leaf",
            "image/x-raw-adobe",
            "image/x-raw-olympus",
            "image/x-raw-panasonic",
            "image/x-canon-cr2",
        ]
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

    /// The image's metadata, its XMP searched for in all of it as the `pages` it is mapped to
    /// are released, and its previews found in place. A file of a magic number Tika does not
    /// know, which only its name sent here, is an empty document.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let Some(magic) = Magic::of(input) else {
            return Ok(Box::new(|_| Ok(())));
        };
        let zone = dates_zone();
        let paging = Paging::new(pages);
        let (read, difference) = match magic {
            Magic::Panasonic => (
                xmp_only(input, &paging, metadata)?,
                Some("the raw IFD0 of a Panasonic RW2".to_owned()),
            ),
            Magic::Tiff | Magic::BigTiff | Magic::Olympus => {
                let (read, skipped) = extract_tiff_metadata(
                    input,
                    &paging,
                    metadata.clone(),
                    zone,
                    MakerNotes::Skip,
                )?;
                (
                    read,
                    skipped.map(|maker| format!("an Exif maker note of {maker}")),
                )
            }
        };
        let previews = previews(input, MAX_TOTAL_PREVIEW_BYTES);
        let reading: Prepared<'a> = Box::new(move |xhtml| {
            *xhtml.metadata_mut() = read.metadata;
            if read.reads_file {
                super::add_file_directory(
                    xhtml,
                    input.len(),
                    false,
                    zone,
                    read.prefix_route_names,
                )?;
            }
            let mut previews = match previews {
                Ok(previews) => previews,
                Err(exception) => {
                    embedded::record_exception(xhtml.metadata_mut(), &exception);
                    return Ok(());
                }
            };
            previews.sort_by_key(|preview| Reverse(preview.length));
            for (count, preview) in previews.iter().enumerate() {
                let mut metadata = TikaMetadata::new();
                metadata.set("Content-Type", JPEG);
                metadata.set("Content-Length", preview.length.to_string());
                let (resource_type, name) = if count == 0 {
                    (ResourceType::Thumbnail, generated_name("thumbnail", 0))
                } else {
                    (ResourceType::Inline, generated_name("image", count - 1))
                };
                resource_type.set(&mut metadata);
                metadata.set(embedded::RESOURCE_NAME, name);
                metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
                xhtml.parse_embedded_opened(
                    &preview.bytes(input),
                    &metadata,
                    Wrapping::PackageEntry,
                    Opened::Alone,
                )?;
            }
            Ok(())
        });
        match difference {
            None => Ok(reading),
            Some(left_out) => Err(Decline::Differs {
                reading,
                difference: Difference::new(format!(
                    "the metadata leaves out what metadata-extractor reads from {left_out}"
                )),
            }),
        }
    }
}

/// The metadata of a Panasonic RW2, whose raw IFD0 the port does not read: its XMP, and the
/// file's directory, which metadata-extractor reads it for.
fn xmp_only(
    input: &[u8],
    paging: &Paging<'_>,
    metadata: &TikaMetadata,
) -> Result<Read, Decline<'static>> {
    let mut metadata = metadata.clone();
    image_xmp::scan_and_extract(input, paging, &mut metadata)?;
    Ok(Read {
        metadata,
        prefix_route_names: 0,
        reads_file: true,
    })
}

/// `generateResourceName`: the prefix, the count and the registered extension of JPEG.
fn generated_name(prefix: &str, count: usize) -> String {
    format!(
        "{prefix}-{count}{}",
        mime::registered_extension(&MediaType::named(JPEG))
    )
}

/// A JPEG preview: where it starts and how long it is, and whether the camera wrote a byte of
/// its own over its first one, which is put back when it is handed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Preview {
    offset: i64,
    length: i64,
    soi_patched: bool,
}

impl Preview {
    fn new(offset: i64, length: i64) -> Self {
        Self {
            offset,
            length,
            soi_patched: false,
        }
    }

    /// The preview's bytes in `input`, which [`previews`] found it within, its first one put
    /// back if the camera wrote over it.
    fn bytes(self, input: &[u8]) -> Cow<'_, [u8]> {
        let data = &input[self.offset as usize..(self.offset + self.length) as usize];
        if self.soi_patched {
            Cow::Owned([&[0xFF], &data[1..]].concat())
        } else {
            Cow::Borrowed(data)
        }
    }
}

/// The TIFF as `RandomAccessFile` reads it.
struct File<'a> {
    data: &'a [u8],
    big_endian: bool,
}

impl File<'_> {
    fn length(&self) -> i64 {
        self.data.len() as i64
    }

    fn bytes(&self, at: i64, length: i64) -> Result<&[u8], String> {
        usize::try_from(at)
            .ok()
            .zip(usize::try_from(length).ok())
            .and_then(|(at, length)| self.data.get(at..at.checked_add(length)?))
            .ok_or_else(|| "java.io.EOFException".to_owned())
    }

    /// `readUInt16` at `at`.
    fn u16_at(&self, at: i64) -> Result<i64, String> {
        let bytes = self
            .bytes(at, 2)
            .map_err(|_| "java.io.IOException: unexpected end of file".to_owned())?;
        Ok(get_u16(bytes, 0, self.big_endian))
    }

    fn u32_at(&self, at: i64) -> Result<i64, String> {
        let (high, low) = (self.u16_at(at)?, self.u16_at(at + 2)?);
        Ok(combine(high, low, 16, self.big_endian))
    }

    fn u64_at(&self, at: i64) -> Result<i64, String> {
        let (high, low) = (self.u32_at(at)?, self.u32_at(at + 4)?);
        Ok(combine(high, low, 32, self.big_endian))
    }
}

/// Two halves read in order into one value, as the byte order puts them.
fn combine(first: i64, second: i64, bits: u32, big_endian: bool) -> i64 {
    if big_endian {
        (first << bits) | second
    } else {
        (second << bits) | first
    }
}

/// `getUInt16`.
fn get_u16(bytes: &[u8], at: usize, big_endian: bool) -> i64 {
    let (a, b) = (i64::from(bytes[at]), i64::from(bytes[at + 1]));
    if big_endian {
        (a << 8) | b
    } else {
        (b << 8) | a
    }
}

fn get_u32(bytes: &[u8], at: usize, big_endian: bool) -> i64 {
    combine(
        get_u16(bytes, at, big_endian),
        get_u16(bytes, at + 2, big_endian),
        16,
        big_endian,
    )
}

fn get_u64(bytes: &[u8], at: usize, big_endian: bool) -> i64 {
    combine(
        get_u32(bytes, at, big_endian),
        get_u32(bytes, at + 4, big_endian),
        32,
        big_endian,
    )
}

/// What a TIFF structure exception says, as Tika records it.
fn structure(message: &str) -> String {
    format!("org.apache.tika.parser.image.RawTiffParser$TiffStructureException: {message}")
}

/// `locateJpegPreviews`: the JPEG previews the IFDs and their SubIFDs point to, then those of
/// Leaf's data block and of the maker notes, together at most `max_total` bytes, or the exception
/// Tika records where the TIFF's structure is broken. A preview past `max_total` is skipped and
/// the walk goes on, where Tika stops at it and loses the smaller previews after it (#127).
fn previews(data: &[u8], max_total: i64) -> Result<Vec<Preview>, String> {
    if data.len() < 8 {
        return Err(structure("file too short for a TIFF header"));
    }
    let big_endian = match &data[..2] {
        b"MM" => true,
        b"II" => false,
        _ => return Err(structure("not a TIFF byte order marker")),
    };
    let file = File { data, big_endian };
    let magic = Magic::named(file.u16_at(2)?).ok_or_else(|| structure("bad TIFF magic number"))?;
    let big_tiff = magic == Magic::BigTiff;
    if big_tiff && (file.u16_at(4)? != 8 || file.u16_at(6)? != 0) {
        return Err(structure("unsupported BigTIFF header"));
    }
    let (count_size, entry_size, offset_size) = if big_tiff { (8, 20, 8) } else { (2, 12, 4) };
    let first = if big_tiff {
        file.u64_at(8)?
    } else {
        file.u32_at(4)?
    };
    let mut collector = Collector::new(max_total);
    let mut found = Found::default();
    let mut visited = HashSet::new();
    let mut to_visit = VecDeque::from([first]);
    while let Some(ifd) = to_visit.pop_front() {
        if visited.len() >= MAX_IFDS {
            break;
        }
        if ifd <= 0 || !visited.insert(ifd) {
            continue;
        }
        if ifd > file.length() - count_size {
            continue;
        }
        let entries = if big_tiff {
            file.u64_at(ifd)?
        } else {
            file.u16_at(ifd)?
        };
        if !(0..=MAX_ENTRIES_PER_IFD).contains(&entries)
            || ifd + count_size + entries * entry_size + offset_size > file.length()
        {
            continue;
        }
        let table = file.bytes(ifd + count_size, entries * entry_size + offset_size)?;
        let layout = Layout {
            big_tiff,
            panasonic: magic == Magic::Panasonic,
        };
        let ifd_values = read_ifd(&file, table, entries as usize, layout, &mut found)?;
        for offset in &ifd_values.sub_ifds {
            enqueue(&mut to_visit, *offset);
        }
        let follower = entries as usize * entry_size as usize;
        enqueue(
            &mut to_visit,
            if big_tiff {
                get_u64(table, follower, big_endian)
            } else {
                get_u32(table, follower, big_endian)
            },
        );
        let (mut offset, mut length) = ifd_values
            .panasonic
            .filter(|&(offset, _)| offset > 0)
            .unwrap_or((ifd_values.jpeg_offset, ifd_values.jpeg_length));
        if offset < 0 && ifd_values.displayable_strip() {
            offset = ifd_values.strip_offsets[0];
            length = ifd_values.strip_byte_counts[0];
        }
        collector.offer(&file, Preview::new(offset, length));
    }
    if let Some((block, length)) = found.leaf {
        for candidate in leaf_previews(&file, block, length)? {
            collector.offer(&file, candidate);
        }
    }
    for maker_note in found.maker_notes {
        for candidate in maker_note_previews(&file, maker_note)? {
            collector.offer(&file, candidate);
        }
    }
    Ok(collector.previews)
}

/// `PreviewCollector`: the previews found so far, each region once, and the budget they share.
struct Collector {
    previews: Vec<Preview>,
    offsets: HashSet<i64>,
    max_total: i64,
    total: i64,
}

impl Collector {
    fn new(max_total: i64) -> Self {
        Self {
            previews: Vec::new(),
            offsets: HashSet::new(),
            max_total,
            total: 0,
        }
    }

    /// `isPreviewAt` and `add`: the candidate, if a preview lies there, it is not one already
    /// found and the budget has room for it.
    fn offer(&mut self, file: &File, candidate: Preview) {
        if !is_preview_at(file, candidate) || !self.offsets.insert(candidate.offset) {
            return;
        }
        if candidate.length > self.max_total - self.total {
            return;
        }
        self.total += candidate.length;
        self.previews.push(candidate);
    }
}

/// `isPreviewAt`: whether the preview lies inside the file, within the length a preview may
/// have, and starts with the JPEG SOI marker, of which a patched one keeps only the second byte.
fn is_preview_at(file: &File, preview: Preview) -> bool {
    if preview.offset <= 0
        || preview.length <= 4
        || preview.length > MAX_PREVIEW_LENGTH
        || preview.offset > file.length() - preview.length
    {
        return false;
    }
    let at = preview.offset as usize;
    (preview.soi_patched || file.data[at] == 0xFF) && file.data[at + 1] == 0xD8
}

/// `enqueue`: an IFD to visit, unless too many wait already.
fn enqueue(to_visit: &mut VecDeque<i64>, offset: i64) {
    if to_visit.len() < MAX_PENDING_IFDS {
        to_visit.push_back(offset);
    }
}

/// How the walk reads an IFD's entries: as a BigTIFF's, and with Panasonic's preview tag.
#[derive(Debug, Clone, Copy)]
struct Layout {
    big_tiff: bool,
    panasonic: bool,
}

/// What the walk gathers across the IFDs: the first Leaf data block, where and how long, and the
/// maker notes of the Exif IFDs it looked up, of which it looks up a few at most.
#[derive(Debug, Default)]
struct Found {
    leaf: Option<(i64, i64)>,
    maker_notes: Vec<i64>,
    maker_note_lookups: usize,
}

/// What the walk takes from an IFD's entries.
#[derive(Debug)]
struct IfdValues {
    sub_ifds: Vec<i64>,
    jpeg_offset: i64,
    jpeg_length: i64,
    /// A Panasonic RW2's preview, where and how long, which comes before the other tags: the
    /// strip tags of an RW2 hold placeholders.
    panasonic: Option<(i64, i64)>,
    compression: i64,
    photometric: i64,
    bits_per_sample: Vec<i64>,
    strip_offsets: Vec<i64>,
    strip_byte_counts: Vec<i64>,
}

impl IfdValues {
    /// `isDisplayableJpegStrip`: a single JPEG strip in RGB or YCbCr, or of eight-bit samples
    /// where the photometric interpretation is not given.
    fn displayable_strip(&self) -> bool {
        if self.compression != COMPRESSION_OLD_JPEG && self.compression != COMPRESSION_JPEG {
            return false;
        }
        if self.strip_offsets.len() != 1 || self.strip_byte_counts.len() != 1 {
            return false;
        }
        if self.photometric == PHOTOMETRIC_RGB || self.photometric == PHOTOMETRIC_YCBCR {
            return true;
        }
        if self.photometric != -1 {
            return false;
        }
        !self.bits_per_sample.is_empty() && self.bits_per_sample.iter().all(|&bits| bits == 8)
    }
}

/// `isBlob`: whether an entry of `kind` holds its `count` bytes elsewhere, as a tag naming an
/// image does: bytes or undefined, too many for the value field.
fn is_blob(kind: i64, count: i64, inline_size: i64) -> bool {
    (kind == TYPE_BYTE || kind == TYPE_UNDEFINED) && count > inline_size
}

/// The entries of an IFD's `table`, the Leaf data block and maker notes among them gathered in
/// `found`.
fn read_ifd(
    file: &File,
    table: &[u8],
    entries: usize,
    layout: Layout,
    found: &mut Found,
) -> Result<IfdValues, String> {
    let big_endian = file.big_endian;
    let big_tiff = layout.big_tiff;
    let (entry_size, inline_size) = if big_tiff { (20, 8) } else { (12, 4) };
    let mut values = IfdValues {
        sub_ifds: Vec::new(),
        jpeg_offset: -1,
        jpeg_length: -1,
        panasonic: None,
        compression: -1,
        photometric: -1,
        bits_per_sample: Vec::new(),
        strip_offsets: Vec::new(),
        strip_byte_counts: Vec::new(),
    };
    for entry in 0..entries {
        let at = entry * entry_size;
        let tag = get_u16(table, at, big_endian) as u16;
        let kind = get_u16(table, at + 2, big_endian);
        let count = if big_tiff {
            get_u64(table, at + 4, big_endian)
        } else {
            get_u32(table, at + 4, big_endian)
        };
        let field = at + 4 + inline_size as usize;
        let read = || long_values(file, table, field, big_tiff, kind, count);
        let single = || -> Result<i64, String> {
            let found = read()?;
            Ok(if found.len() == 1 { found[0] } else { -1 })
        };
        match tag {
            TAG_SUB_IFDS => values.sub_ifds.extend(read()?),
            TAG_JPEG_INTERCHANGE_FORMAT if count == 1 => values.jpeg_offset = single()?,
            TAG_JPEG_INTERCHANGE_FORMAT_LENGTH if count == 1 => values.jpeg_length = single()?,
            TAG_COMPRESSION if count == 1 => values.compression = single()?,
            TAG_PHOTOMETRIC_INTERPRETATION if count == 1 => values.photometric = single()?,
            TAG_BITS_PER_SAMPLE => values.bits_per_sample = read()?,
            TAG_STRIP_OFFSETS => values.strip_offsets = read()?,
            TAG_STRIP_BYTE_COUNTS => values.strip_byte_counts = read()?,
            TAG_PANASONIC_JPG_FROM_RAW
                if layout.panasonic && is_blob(kind, count, inline_size) =>
            {
                values.panasonic = Some((get_u32(table, field, big_endian), count));
            }
            TAG_LEAF_DATA
                if found.leaf.is_none()
                    && count > inline_size
                    && (kind == TYPE_ASCII || is_blob(kind, count, inline_size)) =>
            {
                found.leaf = Some((get_u32(table, field, big_endian), count));
            }
            TAG_EXIF_IFD
                if count == 1 && !big_tiff && found.maker_note_lookups < MAX_MAKER_NOTE_LOOKUPS =>
            {
                for exif_ifd in read()? {
                    found.maker_note_lookups += 1;
                    if let Some(maker_note) = maker_note_offset(file, exif_ifd) {
                        found.maker_notes.push(maker_note);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(values)
}

/// `readIfdEntries`: the entries of the classic IFD at `offset`, without the pointer that follows
/// them, read in the byte order `big_endian`; none where the file holds no such table.
fn ifd_entries<'a>(file: &File<'a>, offset: i64, big_endian: bool) -> Option<&'a [u8]> {
    if offset <= 0 || offset > file.length() - 2 {
        return None;
    }
    let at = offset as usize;
    let entries = get_u16(file.data, at, big_endian) as usize;
    if entries < 1
        || entries as i64 > MAX_ENTRIES_PER_IFD
        || offset + 2 + (entries * CLASSIC_ENTRY_SIZE) as i64 > file.length()
    {
        return None;
    }
    Some(&file.data[at + 2..at + 2 + entries * CLASSIC_ENTRY_SIZE])
}

/// An entry of a classic IFD's `table`: its tag, type, count and value field read as a long.
#[derive(Debug, Clone, Copy)]
struct Entry {
    tag: u16,
    kind: i64,
    count: i64,
    value: i64,
}

/// The entries of a classic IFD's `table`.
fn classic_entries(table: &[u8], big_endian: bool) -> impl Iterator<Item = Entry> + '_ {
    table
        .as_chunks::<CLASSIC_ENTRY_SIZE>()
        .0
        .iter()
        .map(move |entry| Entry {
            tag: get_u16(entry, 0, big_endian) as u16,
            kind: get_u16(entry, 2, big_endian),
            count: get_u32(entry, 4, big_endian),
            value: get_u32(entry, 8, big_endian),
        })
}

/// `readMakerNoteOffset`: where the maker note of the Exif IFD at `exif_ifd` begins.
fn maker_note_offset(file: &File, exif_ifd: i64) -> Option<i64> {
    let table = ifd_entries(file, exif_ifd, file.big_endian)?;
    classic_entries(table, file.big_endian)
        .find(|entry| {
            entry.tag == TAG_MAKER_NOTE && is_blob(entry.kind, entry.count, CLASSIC_INLINE_SIZE)
        })
        .map(|entry| entry.value)
        .filter(|&offset| offset > 0)
}

/// `readLeafPreviews`: the JPEG a Leaf MOS keeps in its data block, a chain of blocks, each a
/// magic and a version, a padded name and the length of what follows; the one named
/// `JPEG_preview_data` holds the preview as it is.
fn leaf_previews(file: &File, block: i64, block_length: i64) -> Result<Vec<Preview>, String> {
    let mut candidates = Vec::new();
    if block <= 0 || block_length <= 0 || block > file.length() - block_length {
        return Ok(candidates);
    }
    let header_length = LEAF_BLOCK_HEADER_LENGTH as i64;
    let end = block + block_length;
    let mut at = block;
    for _ in 0..MAX_LEAF_BLOCKS {
        if at > end - header_length {
            break;
        }
        let header = file.bytes(at, header_length)?;
        if !header.starts_with(LEAF_BLOCK_MAGIC) {
            break;
        }
        let length = get_u32(header, LEAF_BLOCK_HEADER_LENGTH - 4, file.big_endian);
        let payload = at + header_length;
        if length > end - payload {
            break;
        }
        if header[LEAF_BLOCK_NAME_OFFSET..].starts_with(LEAF_PREVIEW_NAME) {
            candidates.push(Preview::new(payload, length));
        }
        at = payload + length;
    }
    Ok(candidates)
}

/// `readMakerNotePreviews`: the previews the maker note at `maker_note` points at, by the name it
/// begins with.
fn maker_note_previews(file: &File, maker_note: i64) -> Result<Vec<Preview>, String> {
    if maker_note <= 0 || maker_note > file.length() - EPSON_MAKER_NOTE_HEADER_LENGTH {
        return Ok(Vec::new());
    }
    let mut header = [0u8; OM_SYSTEM_MAKER_NOTE_HEADER_LENGTH as usize];
    let available = &file.data[maker_note as usize..];
    let read = header.len().min(available.len());
    header[..read].copy_from_slice(&available[..read]);
    Ok(if header.starts_with(OLYMPUS_MAKER_NOTE_NAME) {
        olympus_previews(
            file,
            maker_note,
            &header,
            OLYMPUS_MAKER_NOTE_NAME,
            OLYMPUS_MAKER_NOTE_HEADER_LENGTH,
        )
    } else if header.starts_with(OM_SYSTEM_MAKER_NOTE_NAME) {
        olympus_previews(
            file,
            maker_note,
            &header,
            OM_SYSTEM_MAKER_NOTE_NAME,
            OM_SYSTEM_MAKER_NOTE_HEADER_LENGTH,
        )
    } else if header.starts_with(EPSON_MAKER_NOTE_NAME) {
        epson_previews(file, maker_note)
    } else {
        Vec::new()
    })
}

/// `readEpsonMakerNotePreviews`: the preview an Epson ERF keeps in its maker note, at an offset
/// counted from the start of the file and behind a byte the camera writes over the SOI's first.
fn epson_previews(file: &File, maker_note: i64) -> Vec<Preview> {
    let Some(table) = ifd_entries(
        file,
        maker_note + EPSON_MAKER_NOTE_HEADER_LENGTH,
        file.big_endian,
    ) else {
        return Vec::new();
    };
    classic_entries(table, file.big_endian)
        .filter(|entry| {
            entry.tag == TAG_EPSON_PREVIEW && is_blob(entry.kind, entry.count, CLASSIC_INLINE_SIZE)
        })
        .map(|entry| Preview {
            offset: entry.value,
            length: entry.count,
            soi_patched: true,
        })
        .collect()
}

/// `readOlympusMakerNotePreviews`: the camera's thumbnail an Olympus maker note holds, and the
/// preview its CameraSettings sub-IFD names, every offset counted from the maker note's start
/// and read in the maker note's own byte order, which follows its `name` in its `header`.
fn olympus_previews(
    file: &File,
    maker_note: i64,
    header: &[u8],
    name: &[u8],
    header_length: i64,
) -> Vec<Preview> {
    let mut candidates = Vec::new();
    let big_endian = match &header[name.len()..name.len() + 2] {
        b"II" => false,
        b"MM" => true,
        _ => return candidates,
    };
    let Some(table) = ifd_entries(file, maker_note + header_length, big_endian) else {
        return candidates;
    };
    let mut settings = -1;
    for entry in classic_entries(table, big_endian) {
        if entry.tag == TAG_OLYMPUS_THUMBNAIL
            && is_blob(entry.kind, entry.count, CLASSIC_INLINE_SIZE)
        {
            candidates.push(Preview::new(maker_note + entry.value, entry.count));
        } else if entry.tag == TAG_OLYMPUS_CAMERA_SETTINGS && entry.count == 1 {
            settings = maker_note + entry.value;
        }
    }
    let Some(settings_table) = ifd_entries(file, settings, big_endian) else {
        return candidates;
    };
    let (mut start, mut length) = (-1, -1);
    for entry in classic_entries(settings_table, big_endian).filter(|entry| entry.count == 1) {
        match entry.tag {
            TAG_OLYMPUS_PREVIEW_START => start = entry.value,
            TAG_OLYMPUS_PREVIEW_LENGTH => length = entry.value,
            _ => {}
        }
    }
    if start >= 0 && length > 0 {
        candidates.push(Preview::new(maker_note + start, length));
    }
    candidates
}

/// `readLongValues`: the values of a field of shorts, longs or IFD offsets (BigTIFF's eight-byte
/// ones too), inline or where the field points; none for any other type, too many values, or
/// values outside the file.
fn long_values(
    file: &File,
    table: &[u8],
    field: usize,
    big_tiff: bool,
    kind: i64,
    count: i64,
) -> Result<Vec<i64>, String> {
    let big_endian = file.big_endian;
    let size: usize = match kind {
        3 => 2,
        4 | 13 => 4,
        16 | 18 if big_tiff => 8,
        _ => return Ok(Vec::new()),
    };
    if !(1..=MAX_ENTRIES_PER_IFD).contains(&count) {
        return Ok(Vec::new());
    }
    let count = count as usize;
    let total = size * count;
    let source: &[u8] = if total <= if big_tiff { 8 } else { 4 } {
        &table[field..]
    } else {
        let offset = if big_tiff {
            get_u64(table, field, big_endian)
        } else {
            get_u32(table, field, big_endian)
        };
        if offset < 0 || offset > file.length() - total as i64 {
            return Ok(Vec::new());
        }
        file.bytes(offset, total as i64)?
    };
    Ok((0..count)
        .map(|index| {
            let at = index * size;
            match size {
                2 => get_u16(source, at, big_endian),
                4 => get_u32(source, at, big_endian),
                _ => get_u64(source, at, big_endian),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {

    use super::*;

    /// A classic IFD entry of a tag of `kind` with `count` values and the value field `value`,
    /// little endian.
    fn entry(tag: u16, kind: u16, count: u32, value: u32) -> Vec<u8> {
        let mut entry = tag.to_le_bytes().to_vec();
        entry.extend_from_slice(&kind.to_le_bytes());
        entry.extend_from_slice(&count.to_le_bytes());
        entry.extend_from_slice(&value.to_le_bytes());
        entry
    }

    /// A little endian IFD of `entries`, followed by the IFD at `next`.
    fn ifd(entries: &[Vec<u8>], next: u32) -> Vec<u8> {
        let mut ifd = (entries.len() as u16).to_le_bytes().to_vec();
        for entry in entries {
            ifd.extend_from_slice(entry);
        }
        ifd.extend_from_slice(&next.to_le_bytes());
        ifd
    }

    /// A little endian file of the `magic` number whose first IFD at 8 has `entries`, padded to
    /// `length` bytes, with the SOI marker at each of `jpegs`.
    fn file(magic: u16, entries: &[Vec<u8>], length: usize, jpegs: &[usize]) -> Vec<u8> {
        let mut data = b"II".to_vec();
        data.extend_from_slice(&magic.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend(ifd(entries, 0));
        data.resize(length, 0);
        for &at in jpegs {
            data[at..at + 2].copy_from_slice(&[0xFF, 0xD8]);
        }
        data
    }

    fn at(offset: i64, length: i64) -> Preview {
        Preview::new(offset, length)
    }

    #[test]
    fn finds_previews_in_ifds_and_sub_ifds() {
        let tiff = file(
            42,
            &[
                entry(TAG_JPEG_INTERCHANGE_FORMAT, 4, 1, 40),
                entry(TAG_JPEG_INTERCHANGE_FORMAT_LENGTH, 4, 1, 6),
            ],
            46,
            &[40],
        );
        assert_eq!(
            previews(&tiff, MAX_TOTAL_PREVIEW_BYTES).unwrap(),
            [at(40, 6)]
        );
        assert!(previews(b"XX*\0\0\0\0\0", MAX_TOTAL_PREVIEW_BYTES)
            .unwrap_err()
            .contains("byte order"));
        assert!(previews(b"II", MAX_TOTAL_PREVIEW_BYTES)
            .unwrap_err()
            .contains("too short"));
        assert!(previews(b"II\x2c\0\x08\0\0\0", MAX_TOTAL_PREVIEW_BYTES)
            .unwrap_err()
            .contains("magic number"));
        assert_eq!(
            previews(b"II+\0\x08\0\0\0", MAX_TOTAL_PREVIEW_BYTES).unwrap_err(),
            "java.io.IOException: unexpected end of file"
        );
        assert!(previews(b"II+\0\x04\0\0\0", MAX_TOTAL_PREVIEW_BYTES)
            .unwrap_err()
            .contains("BigTIFF header"));
    }

    #[test]
    fn knows_the_magic_numbers_tika_reads() {
        assert_eq!(Magic::of(b"MM\0*"), Some(Magic::Tiff));
        assert_eq!(Magic::of(b"MM\0+"), Some(Magic::BigTiff));
        assert_eq!(Magic::of(b"IIRO"), Some(Magic::Olympus));
        assert_eq!(Magic::of(b"IIRS"), Some(Magic::Olympus));
        assert_eq!(Magic::of(b"MMOR"), Some(Magic::Olympus));
        assert_eq!(Magic::of(b"IIU\0"), Some(Magic::Panasonic));
        assert_eq!(Magic::of(b"II\0\0"), None);
        assert_eq!(Magic::of(b"II*"), None);
        assert_eq!(Magic::of(b"RIFF"), None);
    }

    #[test]
    fn reads_nothing_of_a_file_only_its_name_sent() {
        let configs = ParserConfigs::default();
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", "image/x-raw-panasonic");
        assert!(RawTiffParser
            .prepare(b"not a raw image", &metadata, &configs)
            .is_ok());
    }

    #[test]
    fn skips_a_preview_past_the_total_and_walks_on() {
        let jpeg = |offset: u32, length: u32| {
            [
                entry(TAG_JPEG_INTERCHANGE_FORMAT, 4, 1, offset),
                entry(TAG_JPEG_INTERCHANGE_FORMAT_LENGTH, 4, 1, length),
            ]
        };
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend(ifd(&jpeg(100, 60), 38));
        tiff.extend(ifd(&jpeg(200, 6), 0));
        tiff.resize(300, 0);
        for at in [100, 200] {
            tiff[at..at + 2].copy_from_slice(&[0xFF, 0xD8]);
        }
        assert_eq!(previews(&tiff, 50).unwrap(), [at(200, 6)]);
        assert_eq!(previews(&tiff, MAX_TOTAL_PREVIEW_BYTES).unwrap().len(), 2);
    }

    #[test]
    fn takes_a_panasonic_preview_from_its_own_tag_before_the_strips() {
        let entries = [
            entry(TAG_COMPRESSION, 3, 1, COMPRESSION_JPEG as u32),
            entry(TAG_PHOTOMETRIC_INTERPRETATION, 3, 1, PHOTOMETRIC_RGB as u32),
            entry(TAG_STRIP_OFFSETS, 4, 1, 120),
            entry(TAG_STRIP_BYTE_COUNTS, 4, 1, 10),
            entry(TAG_PANASONIC_JPG_FROM_RAW, 7, 30, 200),
        ];
        let rw2 = file(0x55, &entries, 240, &[120, 200]);
        assert_eq!(
            previews(&rw2, MAX_TOTAL_PREVIEW_BYTES).unwrap(),
            [at(200, 30)]
        );
        let tiff = file(42, &entries, 240, &[120, 200]);
        assert_eq!(
            previews(&tiff, MAX_TOTAL_PREVIEW_BYTES).unwrap(),
            [at(120, 10)]
        );
    }

    #[test]
    fn finds_the_preview_block_of_leaf_data() {
        let block = |name: &[u8], payload: &[u8]| {
            let mut block = LEAF_BLOCK_MAGIC.to_vec();
            block.extend_from_slice(&[0; 4]);
            let mut padded = name.to_vec();
            padded.resize(LEAF_BLOCK_NAME_LENGTH, 0);
            block.extend(padded);
            block.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            block.extend_from_slice(payload);
            block
        };
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.resize(20, 0x11);
        let mut data_block = block(b"CaptProf_number", &[1, 2, 3]);
        data_block.extend(block(LEAF_PREVIEW_NAME, &jpeg));
        let start = 100usize;
        let mut mos = file(
            42,
            &[entry(TAG_LEAF_DATA, 2, data_block.len() as u32, start as u32)],
            start,
            &[],
        );
        mos.extend(&data_block);
        let payload = (start + 2 * LEAF_BLOCK_HEADER_LENGTH + 3) as i64;
        assert_eq!(
            previews(&mos, MAX_TOTAL_PREVIEW_BYTES).unwrap(),
            [at(payload, 20)]
        );
    }

    /// A little endian file whose IFD0 points at an Exif IFD at 40 whose maker note at 100 is
    /// `maker_note`, padded to `length` bytes, with the SOI marker at each of `jpegs`.
    fn with_maker_note(maker_note: &[u8], length: usize, jpegs: &[usize]) -> Vec<u8> {
        let mut data = file(42, &[entry(TAG_EXIF_IFD, 4, 1, 40)], 40, &[]);
        data.extend(ifd(
            &[entry(TAG_MAKER_NOTE, 7, maker_note.len() as u32, 100)],
            0,
        ));
        data.resize(100, 0);
        data.extend_from_slice(maker_note);
        data.resize(length, 0);
        for &at in jpegs {
            data[at..at + 2].copy_from_slice(&[0xFF, 0xD8]);
        }
        data
    }

    #[test]
    fn finds_the_thumbnail_and_preview_of_an_olympus_maker_note() {
        for (name, header_length) in [
            (OLYMPUS_MAKER_NOTE_NAME, OLYMPUS_MAKER_NOTE_HEADER_LENGTH),
            (OM_SYSTEM_MAKER_NOTE_NAME, OM_SYSTEM_MAKER_NOTE_HEADER_LENGTH),
        ] {
            let mut note = name.to_vec();
            note.extend_from_slice(b"II\x03\0");
            note.resize(header_length as usize, 0);
            let settings = header_length as u32 + 2 + 2 * 12 + 4;
            note.extend(ifd(
                &[
                    entry(TAG_OLYMPUS_THUMBNAIL, 7, 10, 200),
                    entry(TAG_OLYMPUS_CAMERA_SETTINGS, 13, 1, settings),
                ],
                0,
            ));
            note.extend(ifd(
                &[
                    entry(TAG_OLYMPUS_PREVIEW_START, 4, 1, 300),
                    entry(TAG_OLYMPUS_PREVIEW_LENGTH, 4, 1, 40),
                ],
                0,
            ));
            let orf = with_maker_note(&note, 500, &[300, 400]);
            assert_eq!(
                previews(&orf, MAX_TOTAL_PREVIEW_BYTES).unwrap(),
                [at(300, 10), at(400, 40)]
            );
        }
    }

    #[test]
    fn restores_the_soi_of_an_epson_preview() {
        let mut note = EPSON_MAKER_NOTE_NAME.to_vec();
        note.resize(EPSON_MAKER_NOTE_HEADER_LENGTH as usize, 0);
        note.extend(ifd(&[entry(TAG_EPSON_PREVIEW, 7, 12, 200)], 0));
        let mut erf = with_maker_note(&note, 220, &[]);
        erf[200..202].copy_from_slice(&[0xEE, 0xD8]);
        let found = previews(&erf, MAX_TOTAL_PREVIEW_BYTES).unwrap();
        assert_eq!(
            found,
            [Preview {
                offset: 200,
                length: 12,
                soi_patched: true
            }]
        );
        let bytes = found[0].bytes(&erf);
        assert_eq!(bytes.len(), 12);
        assert_eq!(bytes[..2], [0xFF, 0xD8]);
    }
}
