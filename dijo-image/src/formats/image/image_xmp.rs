//! Tika's `ImageXmp`, `ExtendedXmp` and `XMPPacketScanner`: where an image keeps its XMP, read
//! before metadata-extractor so XMP's values are the canonical ones. A packet Tika fails on it
//! records, with its stack trace, in the metadata: such an image is unported.

use std::sync::LazyLock;

use regex::bytes::Regex;

use super::metadata_extractor::heif;
use super::metadata_extractor::jpeg::{self, SegmentError};
use super::metadata_extractor::reader::Sequential;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xmp;
use crate::formats::Unported;
use crate::input::Paging;

const XMP_PREAMBLE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const EXTENSION_PREAMBLE: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const GUID_LENGTH: usize = 32;
/// Tika's cap on the declared length of an Extended XMP packet.
const MAX_EXTENDED: i64 = 64 * 1024 * 1024;
/// `XMPPacketScanner.MAX_PACKET`.
const MAX_PACKET: usize = 64 * 1024 * 1024;
/// `ImageXmp.MAX_CHUNK`.
const MAX_CHUNK: u64 = 64 * 1024 * 1024;
const APP1: u8 = 0xE1;

/// `HAS_EXTENDED`, Java's `\s` spelled out.
static HAS_EXTENDED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"HasExtendedXMP["'>= \t\n\x0B\x0C\r]*([A-Fa-f0-9]{32})"#)
        .expect("the pattern compiles")
});

fn recorded(what: &str) -> Unported {
    Unported::new(format!(
        "XMP Tika fails on {what}, recording its exception with a stack trace"
    ))
}

/// `ImageXmp.extractJpeg`: the XMP of the APP1 segments, the standard packet then the extended
/// one put back together.
pub(crate) fn extract_jpeg(input: &[u8], metadata: &mut TikaMetadata) -> Result<(), Unported> {
    let segments = match jpeg::read_segments(input, Some(&|kind| kind == APP1)) {
        Ok(segments) => segments,
        Err(SegmentError::Processing(_)) => return Ok(()),
        Err(SegmentError::End(_)) => return Err(recorded("in a JPEG that ends inside a segment")),
    };
    for packet in assemble(&segments.of(APP1)) {
        xmp::extract(&packet, metadata, xmp::as_is)?;
    }
    Ok(())
}

/// `ExtendedXmp.assemble`.
fn assemble(segments: &[&[u8]]) -> Vec<Vec<u8>> {
    let mut guid: Option<String> = None;
    let mut standard: Option<Vec<u8>> = None;
    let mut extended: Option<Vec<u8>> = None;
    for segment in segments {
        if starts_ignoring_case(segment, XMP_PREAMBLE) {
            let packet = segment[XMP_PREAMBLE.len()..].to_vec();
            guid = find_guid(&packet);
            standard = Some(packet);
            extended = None;
        } else if let Some(guid) = &guid {
            if starts_ignoring_case(segment, EXTENSION_PREAMBLE) {
                extended = extended_chunk(segment, guid, extended);
            }
        }
    }
    standard.into_iter().chain(extended).collect()
}

/// `String.equalsIgnoreCase` of the segment's start, read as US-ASCII, with the preamble.
fn starts_ignoring_case(segment: &[u8], preamble: &[u8]) -> bool {
    segment.len() >= preamble.len()
        && segment[..preamble.len()]
            .iter()
            .zip(preamble)
            .all(|(byte, expected)| byte.is_ascii() && byte.eq_ignore_ascii_case(expected))
}

/// The GUID the standard packet names for its extended part, as US-ASCII sees the packet.
fn find_guid(packet: &[u8]) -> Option<String> {
    let ascii: Vec<u8> = packet
        .iter()
        .map(|byte| if byte.is_ascii() { *byte } else { b'?' })
        .collect();
    HAS_EXTENDED
        .captures(&ascii)
        .map(|captures| String::from_utf8_lossy(&captures[1]).into_owned())
}

/// `processExtendedXMPChunk`: a chunk copied into the buffer the first chunk sizes.
fn extended_chunk(segment: &[u8], guid: &str, buffer: Option<Vec<u8>>) -> Option<Vec<u8>> {
    let header = EXTENSION_PREAMBLE.len() + GUID_LENGTH + 4 + 4;
    if segment.len() < header {
        return buffer;
    }
    let mut reader = Sequential::new(segment);
    let read = (|| {
        reader.skip(EXTENSION_PREAMBLE.len())?;
        let chunk_guid = super::metadata_extractor::java_string(reader.bytes(GUID_LENGTH)?, None);
        let full = reader.u32()? as i32;
        let offset = reader.u32()? as i32;
        Ok::<_, super::metadata_extractor::reader::Bounds>((chunk_guid, full, offset))
    })();
    let Ok((chunk_guid, full, offset)) = read else {
        return buffer;
    };
    if chunk_guid != guid {
        return buffer;
    }
    let mut buffer = match buffer {
        Some(buffer) => buffer,
        None => {
            if full <= 0 || i64::from(full) > MAX_EXTENDED {
                return None;
            }
            vec![0; full as usize]
        }
    };
    if buffer.len() as i64 == i64::from(full) {
        let length = (segment.len() - header) as i64;
        if offset >= 0 && i64::from(offset) <= buffer.len() as i64 - length {
            let offset = offset as usize;
            buffer[offset..offset + length as usize].copy_from_slice(&segment[header..]);
        }
    }
    Some(buffer)
}

/// `ImageXmp.scanAndExtract`: the first `<?xpacket` packet anywhere in the file, searched with
/// the file's `paging`, so that a mapped file's pages are released as the search goes on.
pub(crate) fn scan_and_extract(
    input: &[u8],
    paging: &Paging<'_>,
    metadata: &mut TikaMetadata,
) -> Result<(), Unported> {
    if let Some(packet) = scan(input, paging)? {
        xmp::extract(packet, metadata, xmp::as_is)?;
    }
    Ok(())
}

/// `XMPPacketScanner.parse`: the bytes between the packet header and trailer, in place. Its
/// `skipAfter` does not look again at the byte that broke a partial match, so that it misses a
/// match that byte starts (#114).
fn scan<'a>(input: &'a [u8], paging: &Paging<'_>) -> Result<Option<&'a [u8]>, Unported> {
    let after = |from: usize, pattern: &[u8]| {
        paging
            .find(&input[from..], pattern)
            .map(|at| from + at + pattern.len())
    };
    let Some(header) = after(0, b"<?xpacket begin=") else {
        return Ok(None);
    };
    let Some(start) = after(header, b"?>") else {
        return Err(recorded("in an XMP packet header without its end"));
    };
    let trailer = b"<?xpacket";
    let rest = &input[start..];
    let searched = &rest[..rest.len().min(MAX_PACKET + trailer.len())];
    match paging.find(searched, trailer) {
        Some(length) if length <= MAX_PACKET => Ok(Some(&rest[..length])),
        _ if rest.len() > MAX_PACKET => Err(recorded("in an XMP packet larger than Tika reads")),
        _ => Err(recorded("in an XMP packet without its trailer")),
    }
}

/// `ImageXmp.extractWebp`: the packet of the first top-level `XMP ` chunk.
pub(crate) fn extract_webp(input: &[u8], metadata: &mut TikaMetadata) -> Result<(), Unported> {
    if let Some(packet) = riff_chunk(input, b"XMP ")? {
        xmp::extract(packet, metadata, xmp::as_is)?;
    }
    Ok(())
}

/// `readRiffChunk`: the payload of the first top-level chunk named `four_cc`, if the file is a
/// WebP RIFF and the chunk is whole; a chunk cut short by the end of the file is none, and a
/// chunk before it cut short fails the skip over it.
fn riff_chunk<'a>(input: &'a [u8], four_cc: &[u8]) -> Result<Option<&'a [u8]>, Unported> {
    let Some(head) = input.get(..12) else {
        return Ok(None);
    };
    if &head[..4] != b"RIFF" || &head[8..12] != b"WEBP" {
        return Ok(None);
    }
    let mut position = 12usize;
    loop {
        let Some(chunk) = input.get(position..position + 8) else {
            return Ok(None);
        };
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        position += 8;
        if &chunk[..4] == four_cc {
            if size > MAX_CHUNK {
                return Ok(None);
            }
            return Ok(input.get(position..position + size as usize));
        }
        let skip = size + (size & 1);
        if position as u64 + skip > input.len() as u64 {
            return Err(recorded(
                "in a WebP whose chunk before the XMP is cut short",
            ));
        }
        position += skip as usize;
    }
}

/// `HeifXmp.MAX_ITEM`.
const MAX_ITEM: i64 = 64 * 1024 * 1024;

/// `HeifXmp.extract`: the XMP item of a HEIF file, read when there is one, which tells whether
/// it was.
pub(crate) fn extract_heif(input: &[u8], metadata: &mut TikaMetadata) -> Result<bool, Unported> {
    let Some(packet) = heif_item(input) else {
        return Ok(false);
    };
    xmp::extract(&packet, metadata, xmp::as_is)?;
    Ok(true)
}

/// `HeifXmp.locate`: the first item of type `mime` that looks like XMP, among those the item
/// location box of the top-level `meta` box places in the file; none for a box structure that
/// does not read, whatever fails reading it.
fn heif_item(input: &[u8]) -> Option<Vec<u8>> {
    let length = input.len() as i64;
    let (_, meta_children, meta_end) = heif_box(input, 0, length, b"meta")?;
    let children = meta_children + 4;
    let (info_start, _, info_end) = heif_box(input, children, meta_end, b"iinf")?;
    let (location_start, _, location_end) = heif_box(input, children, meta_end, b"iloc")?;
    let read_box = |start: i64, end: i64| {
        let length = usize::try_from((end - start) as i32).ok()?;
        input.get(usize::try_from(start).ok()?..)?.get(..length)
    };
    let mut reader = heif::Sequential::new(read_box(info_start, info_end)?);
    let found = heif::Box::read(&mut reader).ok()?;
    let info = heif::item_info_box(&mut reader, &found).ok()?;
    let mut reader = heif::Sequential::new(read_box(location_start, location_end)?);
    let found = heif::Box::read(&mut reader).ok()?;
    let extents = heif::item_location_box(&mut reader, &found).ok()?;
    let mut items: Vec<(i64, Vec<heif::Extent>)> = Vec::new();
    for extent in extents {
        match items.iter_mut().find(|(item, _)| *item == extent.item) {
            Some((_, of_item)) => of_item.push(extent),
            None => items.push((extent.item, vec![extent])),
        }
    }
    items.into_iter().find_map(|(item, extents)| {
        let mime = info.get(&item)?.item_type.as_deref() == Some("mime");
        let data = mime.then(|| item_data(input, &extents)).flatten()?;
        looks_like_xmp(&data).then_some(data)
    })
}

/// `HeifXmp.box`: the start, payload start and end of the first box of `kind` between `start`
/// and `end`, none past a box that does not fit.
fn heif_box(input: &[u8], start: i64, end: i64, kind: &[u8; 4]) -> Option<(i64, i64, i64)> {
    let mut position = start;
    while position + 8 <= end {
        let at = usize::try_from(position).ok()?;
        let header = input.get(at..at + 8)?;
        let mut size = i64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let mut header_length = 8;
        if size == 1 {
            let long = input.get(at + 8..at + 16)?;
            size = i64::from_be_bytes(long.try_into().expect("eight bytes"));
            header_length = 16;
        } else if size == 0 {
            size = end - position;
        }
        if size < header_length || position + size > end {
            return None;
        }
        if &header[4..8] == kind {
            return Some((position, position + header_length, position + size));
        }
        position += size;
    }
    None
}

/// `HeifXmp.readExtents`: the item's extents in turn, none when one is outside the file or they
/// come to more than `MAX_ITEM`.
fn item_data(input: &[u8], extents: &[heif::Extent]) -> Option<Vec<u8>> {
    let length = input.len() as i64;
    let mut total = 0i64;
    for extent in extents {
        if extent.offset < 0 || extent.length < 0 || extent.offset + extent.length > length {
            return None;
        }
        total += extent.length;
        if total > MAX_ITEM {
            return None;
        }
    }
    let mut data = Vec::with_capacity(total as usize);
    for extent in extents {
        let start = extent.offset as usize;
        data.extend_from_slice(&input[start..start + extent.length as usize]);
    }
    Some(data)
}

/// `looksLikeXmp`: whether the first 8192 bytes name the XMP meta element, the RDF root or a
/// packet.
fn looks_like_xmp(data: &[u8]) -> bool {
    let head = &data[..data.len().min(8192)];
    [&b"xmpmeta"[..], b"rdf:RDF", b"xpacket"]
        .iter()
        .any(|needle| head.windows(needle.len()).any(|window| window == *needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Pages;

    #[test]
    fn scans_for_packets_past_partial_matches() {
        let paging = Paging::new(Pages::IN_MEMORY);
        assert_eq!(
            scan(
                b"x<<?xpacket begin=\"\"??>XMP<<?xpacket end=\"w\"?>",
                &paging
            )
            .unwrap(),
            Some(&b"XMP<"[..])
        );
        assert_eq!(scan(b"no packet", &paging).unwrap(), None);
    }

    #[test]
    fn records_packets_without_their_trailer_or_larger_than_tika_reads() {
        let paging = Paging::new(Pages::IN_MEMORY);
        assert!(scan(b"<?xpacket begin=\"\"?>XMP", &paging).is_err());
        let mut large = b"<?xpacket begin=\"\"?>".to_vec();
        let start = large.len();
        large.resize(start + MAX_PACKET + 1, b' ');
        large.extend(b"<?xpacket end=\"w\"?>");
        assert!(scan(&large, &paging).is_err());
        large.remove(start);
        assert_eq!(
            scan(&large, &paging).unwrap().map(<[u8]>::len),
            Some(MAX_PACKET)
        );
    }
}
