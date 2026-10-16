//! metadata-extractor's `RiffReader` with its `WebpRiffHandler`: the chunks of a WebP file, its
//! size and flags from the VP8X, VP8L or VP8 chunk, and its Exif and ICC chunks read into
//! directories of their own. An XMP chunk, which metadata-extractor reads with XMPCore, is left
//! to Tika's path.

use super::descriptions::{base, Described};
use super::reader::{Bounds, Reader, Sequential};
use super::{exif, icc, java_string, Directory, Kind, Metadata, Unported, Value};

const TAG_IMAGE_HEIGHT: i32 = 1;
const TAG_IMAGE_WIDTH: i32 = 2;
const TAG_HAS_ALPHA: i32 = 3;
const TAG_IS_ANIMATION: i32 = 4;

/// Why the RIFF structure could not be read.
#[derive(Debug)]
pub(crate) enum RiffError {
    /// `RiffProcessingException`.
    Processing(String),
    /// An `IOException` reading its header.
    End(Bounds),
    Unported(Unported),
}

impl From<Bounds> for RiffError {
    fn from(bounds: Bounds) -> Self {
        Self::End(bounds)
    }
}

/// `RiffReader.processRiff` with a `WebpRiffHandler`.
pub(crate) fn read(data: &[u8], metadata: &mut Metadata) -> Result<(), RiffError> {
    let mut reader = Sequential::new(data);
    reader.motorola = false;
    let file = java_string(reader.bytes(4)?, None);
    if file != "RIFF" {
        return Err(RiffError::Processing(format!(
            "Invalid RIFF header: {file}"
        )));
    }
    let size = reader.i32()?;
    let left = i64::from(size) - 4;
    let identifier = java_string(reader.bytes(4)?, None);
    if identifier != "WEBP" {
        return Ok(());
    }
    let end = reader.position() as i64 + left;
    match chunks(&mut reader, end, metadata) {
        Ok(()) => Ok(()),
        Err(ChunkError::Bounds(bounds)) => {
            add_error(metadata, bounds.0);
            Ok(())
        }
        Err(ChunkError::Unported(unported)) => Err(RiffError::Unported(unported)),
    }
}

enum ChunkError {
    Bounds(Bounds),
    Unported(Unported),
}

impl From<Bounds> for ChunkError {
    fn from(bounds: Bounds) -> Self {
        Self::Bounds(bounds)
    }
}

impl From<Unported> for ChunkError {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

fn add_error(metadata: &mut Metadata, message: impl Into<String>) {
    let mut directory = Directory::new(Kind::Webp);
    directory.add_error(message);
    metadata.add(directory);
}

/// `processChunks`: the chunks up to `end`.
fn chunks(reader: &mut Sequential, end: i64, metadata: &mut Metadata) -> Result<(), ChunkError> {
    while (reader.position() as i64) < end - 8 {
        let four_cc = java_string(reader.bytes(4)?, None);
        let size = reader.i32()?;
        if size < 0 || i64::from(size) + reader.position() as i64 > end {
            add_error(metadata, "Invalid RIFF chunk size");
            return Ok(());
        }
        let size = size as usize;
        match four_cc.as_str() {
            "LIST" | "RIFF" => {
                if size < 4 {
                    add_error(metadata, "Chunk size too small.");
                    return Ok(());
                }
                reader.bytes(4)?;
                reader.skip(size - 4)?;
            }
            "IDIT" => {
                if size < 2 {
                    add_error(metadata, "Chunk size too small.");
                } else {
                    reader.skip(size - 2)?;
                    reader.skip(2)?;
                }
            }
            _ => {
                if ["VP8X", "VP8L", "VP8 ", "EXIF", "ICCP", "XMP "].contains(&four_cc.as_str()) {
                    let payload = reader.bytes(size)?;
                    chunk(&four_cc, payload, metadata)?;
                } else {
                    reader.skip(size)?;
                }
                if size & 1 == 1 {
                    reader.skip(1)?;
                }
            }
        }
    }
    Ok(())
}

/// `WebpRiffHandler.processChunk`.
fn chunk(four_cc: &str, payload: &[u8], metadata: &mut Metadata) -> Result<(), Unported> {
    let mut directory = Directory::new(Kind::Webp);
    match four_cc {
        "EXIF" => {
            let tiff = if payload.len() >= 6 && java_string(&payload[..6], None) == "Exif\0\0" {
                &payload[6..]
            } else {
                payload
            };
            match exif::read_tiff(tiff, 0, metadata) {
                Ok(()) | Err(exif::TiffError::Bounds(_) | exif::TiffError::Processing(_)) => {}
                Err(exif::TiffError::Unported(unported)) => return Err(unported),
            }
        }
        "ICCP" => icc::extract(payload, metadata),
        "XMP " => return Err(Unported::new("a WebP XMP chunk, which XMPCore reads")),
        "VP8X" if payload.len() == 10 => {
            let mut reader = Reader::new(payload);
            reader.motorola = false;
            let result = (|| -> Result<(), Bounds> {
                let flags = reader.u8(0)?;
                let animation = (flags >> 1) & 1 == 1;
                let alpha = (flags >> 4) & 1 == 1;
                let width = int24(&reader, 4)?;
                let height = int24(&reader, 7)?;
                directory.set(TAG_IMAGE_WIDTH, Value::Int(width + 1));
                directory.set(TAG_IMAGE_HEIGHT, Value::Int(height + 1));
                directory.set(TAG_HAS_ALPHA, Value::Boolean(alpha));
                directory.set(TAG_IS_ANIMATION, Value::Boolean(animation));
                Ok(())
            })();
            match result {
                Ok(()) => metadata.add(directory),
                Err(bounds) => directory.add_error(bounds.0),
            }
        }
        "VP8L" if payload.len() > 4 => {
            if payload[0] != 0x2F {
                return Ok(());
            }
            let (b1, b2, b3, b4) = (
                i32::from(payload[1]),
                i32::from(payload[2]),
                i32::from(payload[3]),
                i32::from(payload[4]),
            );
            let width = (b2 & 0x3F) << 8 | b1;
            let height = (b4 & 0x0F) << 10 | b3 << 2 | (b2 & 0xC0) >> 6;
            directory.set(TAG_IMAGE_WIDTH, Value::Int(width + 1));
            directory.set(TAG_IMAGE_HEIGHT, Value::Int(height + 1));
            metadata.add(directory);
        }
        "VP8 " if payload.len() > 9 => {
            if payload[3] != 0x9D || payload[4] != 0x01 || payload[5] != 0x2A {
                return Ok(());
            }
            let mut reader = Reader::new(payload);
            reader.motorola = false;
            let (Ok(width), Ok(height)) = (reader.u16(6), reader.u16(8)) else {
                return Ok(());
            };
            directory.set(TAG_IMAGE_WIDTH, Value::Int(i32::from(width)));
            directory.set(TAG_IMAGE_HEIGHT, Value::Int(i32::from(height)));
            metadata.add(directory);
        }
        _ => {}
    }
    Ok(())
}

/// `getInt24` in Intel byte order.
fn int24(reader: &Reader, index: i64) -> Result<i32, Bounds> {
    let bytes = reader.bytes(index, 3)?;
    Ok(i32::from(bytes[2]) << 16 | i32::from(bytes[1]) << 8 | i32::from(bytes[0]))
}

/// `WebpDescriptor`, which describes every tag as `TagDescriptor` does.
pub(crate) fn describe(directory: &Directory, tag: i32) -> Described {
    Ok(base(directory, tag))
}
