//! java-libpst's `PSTNodeInputStream`: the data of a node, one block or the blocks an XBLOCK
//! lists, decoded where the file is encoded, and inflated where it starts as a zlib stream.

use std::borrow::Cow;

use flate2::{Decompress, FlushDecompress, Status};

use super::file::{le, le_checked, slice, BlockEntry, Format, PstFile, Read, SubNode, Thrown};

/// `PSTObject.compEnc`: the permutation that decodes a file of compressible encryption.
const DECODE: [u8; 256] = [
    0x47, 0xF1, 0xB4, 0xE6, 0x0B, 0x6A, 0x72, 0x48, 0x85, 0x4E, 0x9E, 0xEB, 0xE2, 0xF8, 0x94, 0x53,
    0xE0, 0xBB, 0xA0, 0x02, 0xE8, 0x5A, 0x09, 0xAB, 0xDB, 0xE3, 0xBA, 0xC6, 0x7C, 0xC3, 0x10, 0xDD,
    0x39, 0x05, 0x96, 0x30, 0xF5, 0x37, 0x60, 0x82, 0x8C, 0xC9, 0x13, 0x4A, 0x6B, 0x1D, 0xF3, 0xFB,
    0x8F, 0x26, 0x97, 0xCA, 0x91, 0x17, 0x01, 0xC4, 0x32, 0x2D, 0x6E, 0x31, 0x95, 0xFF, 0xD9, 0x23,
    0xD1, 0x00, 0x5E, 0x79, 0xDC, 0x44, 0x3B, 0x1A, 0x28, 0xC5, 0x61, 0x57, 0x20, 0x90, 0x3D, 0x83,
    0xB9, 0x43, 0xBE, 0x67, 0xD2, 0x46, 0x42, 0x76, 0xC0, 0x6D, 0x5B, 0x7E, 0xB2, 0x0F, 0x16, 0x29,
    0x3C, 0xA9, 0x03, 0x54, 0x0D, 0xDA, 0x5D, 0xDF, 0xF6, 0xB7, 0xC7, 0x62, 0xCD, 0x8D, 0x06, 0xD3,
    0x69, 0x5C, 0x86, 0xD6, 0x14, 0xF7, 0xA5, 0x66, 0x75, 0xAC, 0xB1, 0xE9, 0x45, 0x21, 0x70, 0x0C,
    0x87, 0x9F, 0x74, 0xA4, 0x22, 0x4C, 0x6F, 0xBF, 0x1F, 0x56, 0xAA, 0x2E, 0xB3, 0x78, 0x33, 0x50,
    0xB0, 0xA3, 0x92, 0xBC, 0xCF, 0x19, 0x1C, 0xA7, 0x63, 0xCB, 0x1E, 0x4D, 0x3E, 0x4B, 0x1B, 0x9B,
    0x4F, 0xE7, 0xF0, 0xEE, 0xAD, 0x3A, 0xB5, 0x59, 0x04, 0xEA, 0x40, 0x55, 0x25, 0x51, 0xE5, 0x7A,
    0x89, 0x38, 0x68, 0x52, 0x7B, 0xFC, 0x27, 0xAE, 0xD7, 0xBD, 0xFA, 0x07, 0xF4, 0xCC, 0x8E, 0x5F,
    0xEF, 0x35, 0x9C, 0x84, 0x2B, 0x15, 0xD5, 0x77, 0x34, 0x49, 0xB6, 0x12, 0x0A, 0x7F, 0x71, 0x88,
    0xFD, 0x9D, 0x18, 0x41, 0x7D, 0x93, 0xD8, 0x58, 0x2C, 0xCE, 0xFE, 0x24, 0xAF, 0xDE, 0xB8, 0x36,
    0xC8, 0xA1, 0x80, 0xA6, 0x99, 0x98, 0xA8, 0x2F, 0x0E, 0x81, 0x65, 0x73, 0xE4, 0xC2, 0xA2, 0x8A,
    0xD4, 0xE1, 0x11, 0xD0, 0x08, 0x8B, 0x2A, 0xF2, 0xED, 0x9A, 0x64, 0x3F, 0xC1, 0x6C, 0xF9, 0xEC,
];
const ZLIB: [i32; 2] = [0x78, 0x9C];
/// How deep XBLOCKs nest before the stream gives up on blocks that list each other, where
/// java-libpst would overflow its stack.
const MAX_XBLOCK_LEVELS: usize = 16;

/// A data block of the file, at `offset`, of `size` bytes, from `start` in the stream.
#[derive(Debug, Clone, Copy)]
struct Block {
    offset: i64,
    size: usize,
    start: usize,
}

enum Source<'f> {
    Whole(Cow<'f, [u8]>),
    Blocks { file: &'f [u8], blocks: Vec<Block> },
}

/// The data of a node.
pub(super) struct NodeStream<'f> {
    source: Source<'f>,
    length: usize,
    encrypted: bool,
    ends: Vec<i64>,
}

impl<'f> NodeStream<'f> {
    /// `new PSTNodeInputStream(pstFile, attachmentData, encrypted)`.
    pub(super) fn from_bytes(data: Vec<u8>, encrypted: bool) -> Read<Self> {
        let mut stream = Self {
            length: data.len(),
            source: Source::Whole(Cow::Owned(data)),
            encrypted,
            ends: Vec::new(),
        };
        stream.inflate_zlib(None)?;
        Ok(stream)
    }

    /// `new PSTNodeInputStream(pstFile, descriptorItem)`.
    pub(super) fn from_sub_node(file: &PstFile<'f>, sub_node: &SubNode) -> Read<Self> {
        Self::from_block(file, file.block(sub_node.data)?)
    }

    /// `new PSTNodeInputStream(pstFile, offsetItem)`: `loadFromOffsetItem`, then the zlib check.
    pub(super) fn from_block(file: &PstFile<'f>, entry: BlockEntry) -> Read<Self> {
        let internal = entry.id & 0x02 != 0;
        let data = file.bytes(entry.offset, entry.size)?;
        let mut encrypted = file.is_encrypted();
        if internal {
            if entry.size < 8 {
                return Err(Thrown::pst("Invalid internal block size"));
            }
            if data[0] == 0x1 {
                let length = le(data, 4, 8) as usize;
                let mut blocks = Vec::new();
                let mut location = 0;
                skip_points(file, data, &mut blocks, &mut location, 0)?;
                let ends = blocks
                    .iter()
                    .map(|block| (block.start + block.size) as i64)
                    .collect();
                let mut stream = Self {
                    source: Source::Blocks {
                        file: file.data(),
                        blocks,
                    },
                    length,
                    encrypted,
                    ends,
                };
                stream.inflate_zlib(Some(file.data()))?;
                return Ok(stream);
            }
            encrypted = false;
        }
        let mut stream = Self {
            source: Source::Whole(Cow::Borrowed(data)),
            length: data.len(),
            encrypted,
            ends: Vec::new(),
        };
        stream.inflate_zlib(Some(file.data()))?;
        Ok(stream)
    }

    /// `length`.
    pub(super) fn len(&self) -> usize {
        self.length
    }

    /// `getBlockOffsets`: where each block of the stream ends.
    pub(super) fn block_offsets(&self) -> Vec<i64> {
        if self.ends.is_empty() {
            vec![self.length as i64]
        } else {
            self.ends.clone()
        }
    }

    /// `seek(position)` and `readCompletely` of `length` bytes.
    pub(super) fn read(&self, position: usize, length: usize) -> Read<Vec<u8>> {
        if position > self.length {
            return Err(Thrown::pst(format!(
                "Unable to seek past end of item! size = {}, seeking to:{position}",
                self.length
            )));
        }
        if length == 0 {
            return Ok(Vec::new());
        }
        let end = position + length;
        if end > self.length {
            return Err(Thrown::io(
                "unexpected EOF encountered attempting to read from PSTInputStream",
            ));
        }
        let mut bytes = match &self.source {
            Source::Whole(data) => data
                .get(position..end)
                .ok_or_else(|| Thrown::out_of_bounds("a node's data"))?
                .to_vec(),
            Source::Blocks { file, blocks } => {
                let mut bytes = Vec::with_capacity(length);
                let mut at = position;
                while at < end {
                    let block = blocks
                        .iter()
                        .find(|block| at < block.start + block.size)
                        .ok_or_else(|| {
                            Thrown::runtime(
                                "java.lang.IndexOutOfBoundsException",
                                "a block past the XBLOCK's blocks",
                            )
                        })?;
                    let within = at - block.start;
                    let take = (block.size - within).min(end - at);
                    bytes.extend_from_slice(slice(file, block.offset + within as i64, take)?);
                    at += take;
                }
                bytes
            }
        };
        if self.encrypted {
            decode(&mut bytes);
        }
        Ok(bytes)
    }

    /// `seekAndReadLong(position, length)`.
    pub(super) fn read_long(&self, position: usize, length: usize) -> Read<i64> {
        let bytes = self.read(position, length)?;
        le_checked(&bytes, 0, length, "a node's data")
    }

    /// `seek(position)` and `read()`: the byte there, or -1 at the end.
    pub(super) fn byte(&self, position: usize) -> Read<i32> {
        if position > self.length {
            return Err(Thrown::pst(format!(
                "Unable to seek past end of item! size = {}, seeking to:{position}",
                self.length
            )));
        }
        if position == self.length {
            return Ok(-1);
        }
        let byte = match &self.source {
            Source::Whole(data) => data.get(position).copied(),
            Source::Blocks { file, blocks } => match blocks
                .iter()
                .find(|block| position < block.start + block.size)
            {
                Some(block) => slice(file, block.offset + (position - block.start) as i64, 1)
                    .ok()
                    .map(|bytes| bytes[0]),
                None => None,
            },
        };
        Ok(byte.map_or(-1, |byte| {
            i32::from(if self.encrypted {
                DECODE[usize::from(byte)]
            } else {
                byte
            })
        }))
    }

    /// `detectZlib`: a stream that starts as a zlib stream, inflated, block by block where its
    /// second block starts as one too.
    fn inflate_zlib(&mut self, file: Option<&[u8]>) -> Read<()> {
        if self.length < 4 || [self.byte(0)?, self.byte(1)?] != ZLIB {
            return Ok(());
        }
        let blocks = match &self.source {
            Source::Blocks { blocks, .. } => blocks.clone(),
            Source::Whole(_) => Vec::new(),
        };
        let separate = match (file, blocks.get(1)) {
            (Some(file), Some(second)) => {
                let byte = |at: i64| slice(file, at, 1).map_or(-1, |bytes| i32::from(bytes[0]));
                [byte(second.offset), byte(second.offset + 1)] == ZLIB
            }
            _ => false,
        };
        let inflated = if let (true, Some(file)) = (separate, file) {
            let mut inflated = Vec::with_capacity(self.length);
            for block in &blocks {
                inflate(slice(file, block.offset, block.size)?, &mut inflated)?;
            }
            self.ends.clear();
            inflated
        } else {
            let compressed = if blocks.is_empty() {
                self.length
            } else {
                blocks.iter().map(|block| block.size).sum()
            };
            let mut inflated = Vec::with_capacity(self.length);
            inflate(&self.read(0, compressed)?, &mut inflated)?;
            inflated
        };
        self.length = inflated.len();
        self.source = Source::Whole(Cow::Owned(inflated));
        Ok(())
    }
}

/// `getBlockSkipPoints`: the data blocks an XBLOCK, or the XBLOCKs an XXBLOCK, lists.
fn skip_points(
    file: &PstFile<'_>,
    data: &[u8],
    blocks: &mut Vec<Block>,
    location: &mut usize,
    depth: usize,
) -> Read<()> {
    if data.first() != Some(&0x1) || depth > MAX_XBLOCK_LEVELS {
        return Err(Thrown::pst(
            "Unable to process XBlock, incorrect identifier",
        ));
    }
    let count = le_checked(data, 2, 4, "an XBLOCK")?;
    let width = if file.format() == Format::Ansi { 4 } else { 8 };
    let mut offset = 8;
    match data.get(1) {
        Some(0x2) => {
            for _ in 0..count {
                let id = le_checked(data, offset, offset + width, "an XXBLOCK")? & !1;
                let entry = file.block(id)?;
                let block = file.bytes(entry.offset, entry.size)?;
                skip_points(file, block, blocks, location, depth + 1)?;
                offset += width;
            }
        }
        Some(0x1) => {
            for _ in 0..count {
                let id = le_checked(data, offset, offset + width, "an XBLOCK")? & !1;
                let entry = file.block(id)?;
                blocks.push(Block {
                    offset: entry.offset,
                    size: entry.size,
                    start: *location,
                });
                *location += entry.size;
                offset += width;
            }
        }
        _ => {}
    }
    Ok(())
}

/// `PSTObject.decode`.
fn decode(bytes: &mut [u8]) {
    for byte in bytes {
        *byte = DECODE[usize::from(*byte)];
    }
}

/// What an `InflaterOutputStream` writes of `input`: the zlib stream it starts with, as far as
/// it goes.
fn inflate(input: &[u8], output: &mut Vec<u8>) -> Read<()> {
    let mut inflater = Decompress::new(true);
    loop {
        output.reserve(input.len().max(4096));
        let consumed = inflater.total_in() as usize;
        let produced = inflater.total_out();
        let status = inflater
            .decompress_vec(&input[consumed..], output, FlushDecompress::None)
            .map_err(|error| {
                Thrown::pst(format!(
                    "Unable to decompress reportedly compressed block: {error}"
                ))
            })?;
        let progressed =
            inflater.total_in() as usize != consumed || inflater.total_out() != produced;
        if status == Status::StreamEnd || !progressed {
            return Ok(());
        }
    }
}
