//! Camera raw images, as Tika's `RawTiffParser` reads them: the TIFF's XMP and
//! metadata-extractor's directories, as Tika's `TiffParser` reads them, without OCR; then the
//! JPEG previews the IFDs hold, largest first, the first as a thumbnail, which is not parsed,
//! and the others as images parsed as embedded documents.

use std::cmp::Reverse;
use std::collections::{HashSet, VecDeque};

use super::{dates_zone, extract_metadata, image_xmp, Format, Read};
use crate::config::ParserConfigs;
use crate::formats::embedded::{self, ResourceType, Wrapping};
use crate::formats::metadata::office::RESOURCE_NAME_EXTENSION_INFERRED;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Footprint, Opened, Parser, Prepared};
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

/// Tika's `RawTiffParser`.
pub struct RawTiffParser;

impl Parser for RawTiffParser {
    fn name(&self) -> &'static str {
        "extractous::RawTiffParser"
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
            "image/x-raw-adobe",
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
    /// are released, and its previews found in place.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let zone = dates_zone();
        let paging = Paging::new(pages);
        let read = if is_big_tiff(input) {
            let mut metadata = metadata.clone();
            image_xmp::scan_and_extract(input, &paging, &mut metadata)?;
            Read {
                metadata,
                prefix_route_names: 0,
                reads_file: false,
            }
        } else {
            extract_metadata(Format::Tiff, input, &paging, metadata.clone(), zone)?
        };
        let previews = previews(input, MAX_TOTAL_PREVIEW_BYTES);
        Ok(Box::new(move |xhtml| {
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
                let data =
                    &input[preview.offset as usize..(preview.offset + preview.length) as usize];
                xhtml.parse_embedded_opened(
                    data,
                    &metadata,
                    Wrapping::PackageEntry,
                    Opened::Alone,
                )?;
            }
            Ok(())
        }))
    }
}

/// `generateResourceName`: the prefix, the count and the registered extension of JPEG.
fn generated_name(prefix: &str, count: usize) -> String {
    format!(
        "{prefix}-{count}{}",
        mime::registered_extension(&MediaType::named(JPEG))
    )
}

/// `isBigTiff`.
fn is_big_tiff(input: &[u8]) -> bool {
    matches!(
        input.get(..4),
        Some([b'I', b'I', 0x2B, 0] | [b'M', b'M', 0, 0x2B])
    )
}

/// A JPEG preview: where it starts and how long it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Preview {
    offset: i64,
    length: i64,
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

/// `locateJpegPreviews`: the JPEG previews the IFDs and their SubIFDs point to, together at
/// most `max_total` bytes, or the exception Tika records where the TIFF's structure is broken.
/// A preview past `max_total` is skipped and the walk goes on, where Tika stops at it and loses
/// the smaller previews after it (#127).
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
    let big_tiff = match file.u16_at(2)? {
        42 => false,
        43 => {
            if file.u16_at(4)? != 8 || file.u16_at(6)? != 0 {
                return Err(structure("unsupported BigTIFF header"));
            }
            true
        }
        _ => return Err(structure("bad TIFF magic number")),
    };
    let (count_size, entry_size, offset_size) = if big_tiff { (8, 20, 8) } else { (2, 12, 4) };
    let first = if big_tiff {
        file.u64_at(8)?
    } else {
        file.u32_at(4)?
    };
    let mut previews = Vec::new();
    let mut preview_offsets = HashSet::new();
    let mut total = 0i64;
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
        let ifd_values = read_ifd(&file, table, entries as usize, big_tiff)?;
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
        let (mut offset, mut length) = (ifd_values.jpeg_offset, ifd_values.jpeg_length);
        if offset < 0 && ifd_values.displayable_strip() {
            offset = ifd_values.strip_offsets[0];
            length = ifd_values.strip_byte_counts[0];
        }
        if offset > 0
            && length > 4
            && length <= MAX_PREVIEW_LENGTH
            && offset <= file.length() - length
            && total <= max_total - length
            && data[offset as usize..].starts_with(&[0xFF, 0xD8])
            && preview_offsets.insert(offset)
        {
            total += length;
            previews.push(Preview { offset, length });
        }
    }
    Ok(previews)
}

/// `enqueue`: an IFD to visit, unless too many wait already.
fn enqueue(to_visit: &mut VecDeque<i64>, offset: i64) {
    if to_visit.len() < MAX_PENDING_IFDS {
        to_visit.push_back(offset);
    }
}

/// What the walk takes from an IFD's entries.
#[derive(Debug)]
struct IfdValues {
    sub_ifds: Vec<i64>,
    jpeg_offset: i64,
    jpeg_length: i64,
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

/// The entries of an IFD's `table`.
fn read_ifd(
    file: &File,
    table: &[u8],
    entries: usize,
    big_tiff: bool,
) -> Result<IfdValues, String> {
    let big_endian = file.big_endian;
    let (entry_size, inline_size) = if big_tiff { (20, 8) } else { (12, 4) };
    let mut values = IfdValues {
        sub_ifds: Vec::new(),
        jpeg_offset: -1,
        jpeg_length: -1,
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
        let field = at + 4 + inline_size;
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
            _ => {}
        }
    }
    Ok(values)
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
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    #[cfg(feature = "tika")]
    use super::super::tests::IMAGE_DEPARTURES;
    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{assert_same_as_tika_departing, conformance_corpus, documents};

    #[cfg(feature = "tika")]
    fn paths(files: &[PathBuf]) -> Vec<&Path> {
        files.iter().map(PathBuf::as_path).collect()
    }

    #[test]
    fn finds_previews_in_ifds_and_sub_ifds() {
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        let entry = |tag: u16, value: u32| {
            let mut entry = tag.to_le_bytes().to_vec();
            entry.extend_from_slice(&4u16.to_le_bytes());
            entry.extend_from_slice(&1u32.to_le_bytes());
            entry.extend_from_slice(&value.to_le_bytes());
            entry
        };
        tiff.extend(entry(TAG_JPEG_INTERCHANGE_FORMAT, 40));
        tiff.extend(entry(TAG_JPEG_INTERCHANGE_FORMAT_LENGTH, 6));
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.resize(40, 0);
        tiff.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xD9, 0, 0]);
        assert_eq!(
            previews(&tiff, MAX_TOTAL_PREVIEW_BYTES).unwrap(),
            [Preview {
                offset: 40,
                length: 6
            }]
        );
        assert!(previews(b"XX*\0\0\0\0\0", MAX_TOTAL_PREVIEW_BYTES)
            .unwrap_err()
            .contains("byte order"));
        assert!(previews(b"II", MAX_TOTAL_PREVIEW_BYTES)
            .unwrap_err()
            .contains("too short"));
        assert_eq!(
            previews(b"II+\0\x08\0\0\0", MAX_TOTAL_PREVIEW_BYTES).unwrap_err(),
            "java.io.IOException: unexpected end of file"
        );
        assert!(previews(b"II+\0\x04\0\0\0", MAX_TOTAL_PREVIEW_BYTES)
            .unwrap_err()
            .contains("BigTIFF header"));
        assert!(is_big_tiff(b"MM\0+") && !is_big_tiff(b"MM\0*"));
    }

    #[test]
    fn skips_a_preview_past_the_total_and_walks_on() {
        let entry = |tag: u16, value: u32| {
            let mut entry = tag.to_le_bytes().to_vec();
            entry.extend_from_slice(&4u16.to_le_bytes());
            entry.extend_from_slice(&1u32.to_le_bytes());
            entry.extend_from_slice(&value.to_le_bytes());
            entry
        };
        let ifd = |offset: u32, length: u32, next: u32| {
            let mut ifd = 2u16.to_le_bytes().to_vec();
            ifd.extend(entry(TAG_JPEG_INTERCHANGE_FORMAT, offset));
            ifd.extend(entry(TAG_JPEG_INTERCHANGE_FORMAT_LENGTH, length));
            ifd.extend_from_slice(&next.to_le_bytes());
            ifd
        };
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend(ifd(100, 60, 38));
        tiff.extend(ifd(200, 6, 0));
        tiff.resize(300, 0);
        for at in [100, 200] {
            tiff[at..at + 2].copy_from_slice(&[0xFF, 0xD8]);
        }
        assert_eq!(
            previews(&tiff, 50).unwrap(),
            [Preview {
                offset: 200,
                length: 6
            }]
        );
        assert_eq!(previews(&tiff, MAX_TOTAL_PREVIEW_BYTES).unwrap().len(), 2);
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_raw_images_as_tikas_path_does() {
        let (parsed, deferred) = documents(&RawTiffParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 13, "{parsed:?}");
        assert_same_as_tika_departing(&paths(&parsed), RawTiffParser.tika_name(), IMAGE_DEPARTURES);
    }
}
