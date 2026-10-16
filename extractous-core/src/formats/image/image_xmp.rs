//! Tika's `ImageXmp`, `ExtendedXmp` and `XMPPacketScanner`: where an image keeps its XMP, read
//! before metadata-extractor so XMP's values are the canonical ones. A packet Tika fails on it
//! records, with its stack trace, in the metadata: such an image is unported.

use std::sync::LazyLock;

use regex::bytes::Regex;

use super::metadata_extractor::jpeg::{self, SegmentError};
use super::metadata_extractor::reader::Sequential;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xmp;
use crate::formats::Unported;

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

/// `ImageXmp.scanAndExtract`: the first `<?xpacket` packet anywhere in the file.
pub(crate) fn scan_and_extract(input: &[u8], metadata: &mut TikaMetadata) -> Result<(), Unported> {
    if let Some(packet) = scan(input)? {
        xmp::extract(&packet, metadata, xmp::as_is)?;
    }
    Ok(())
}

/// `XMPPacketScanner.parse`: the bytes between the packet header and trailer, matched as its
/// `skipAfter` matches, which does not look again at the byte that broke a partial match.
fn scan(input: &[u8]) -> Result<Option<Vec<u8>>, Unported> {
    let mut position = 0;
    if !skip_after(input, &mut position, b"<?xpacket begin=", None)? {
        return Ok(None);
    }
    if !skip_after(input, &mut position, b"?>", None)? {
        return Err(recorded("in an XMP packet header without its end"));
    }
    let mut packet = Vec::new();
    if !skip_after(input, &mut position, b"<?xpacket", Some(&mut packet))? {
        return Err(recorded("in an XMP packet without its trailer"));
    }
    Ok(Some(packet))
}

fn skip_after(
    input: &[u8],
    position: &mut usize,
    pattern: &[u8],
    mut out: Option<&mut Vec<u8>>,
) -> Result<bool, Unported> {
    let mut found = 0;
    while let Some(&byte) = input.get(*position) {
        *position += 1;
        if byte == pattern[found] {
            found += 1;
            if found == pattern.len() {
                return Ok(true);
            }
        } else {
            if let Some(out) = out.as_deref_mut() {
                out.extend_from_slice(&pattern[..found]);
                out.push(byte);
                if out.len() > MAX_PACKET {
                    return Err(recorded("in an XMP packet larger than Tika reads"));
                }
            }
            found = 0;
        }
    }
    Ok(false)
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
