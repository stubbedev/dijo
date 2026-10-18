//! What the ports of POI's readers share: the Java exception POI throws reading a document,
//! which fails Tika's parse, and little-endian reads that fail where Java's array bounds do.

use crate::formats::{Decline, Difference, Failure, Unported};

/// A Java exception POI throws reading a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thrown(pub String);

impl Thrown {
    pub(crate) fn new(reason: impl Into<String>) -> Self {
        Thrown(reason.into())
    }
}

pub(crate) type Read<T> = Result<T, Thrown>;

pub(crate) fn thrown<T>(reason: impl Into<String>) -> Read<T> {
    Err(Thrown(reason.into()))
}

/// Why a native parser of POI's formats does not read a document as Tika's parser does: POI
/// throws on it, which fails Tika's parse, or it has what the port does not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// POI throws reading the document, for this reason as the port tells it.
    Thrown(Thrown),
    Unported(Unported),
}

impl From<Thrown> for Unread {
    fn from(thrown: Thrown) -> Self {
        Unread::Thrown(thrown)
    }
}

impl From<Unported> for Unread {
    fn from(unported: Unported) -> Self {
        Unread::Unported(unported)
    }
}

impl From<Thrown> for Decline<'_> {
    fn from(thrown: Thrown) -> Self {
        Unread::from(thrown).declined()
    }
}

impl From<Unread> for Decline<'_> {
    fn from(unread: Unread) -> Self {
        unread.declined()
    }
}

impl Unread {
    /// What a parser declines the document with: what the port does not read is left to Tika's
    /// path, and a document POI throws on fails, as it fails Tika's parse, though with the
    /// port's account of the exception where Tika's path names POI's: a reading that differs.
    pub fn declined<'a>(self) -> Decline<'a> {
        match self {
            Unread::Unported(unported) => unported.into(),
            Unread::Thrown(Thrown(reason)) => Decline::Differs {
                difference: Difference(format!(
                    "POI throws on the document, which fails Tika's parse with POI's exception: \
                     {reason}"
                )),
                reading: Box::new(move |_| {
                    Err(Failure::Malformed(format!(
                        "POI throws reading the document: {reason}"
                    )))
                }),
            },
        }
    }
}

fn slice(data: &[u8], at: usize, length: usize) -> Read<&[u8]> {
    at.checked_add(length)
        .and_then(|end| data.get(at..end))
        .ok_or_else(|| {
            Thrown(format!(
                "Index {at} out of bounds for length {}",
                data.len()
            ))
        })
}

pub(crate) fn u8_at(data: &[u8], at: usize) -> Read<u8> {
    Ok(slice(data, at, 1)?[0])
}

/// `LittleEndian.getShort`.
pub(crate) fn i16_at(data: &[u8], at: usize) -> Read<i16> {
    let bytes = slice(data, at, 2)?;
    Ok(i16::from_le_bytes([bytes[0], bytes[1]]))
}

/// `LittleEndian.getUShort`.
pub(crate) fn u16_at(data: &[u8], at: usize) -> Read<u16> {
    Ok(i16_at(data, at)? as u16)
}

/// `LittleEndian.getInt`.
pub(crate) fn i32_at(data: &[u8], at: usize) -> Read<i32> {
    let bytes = slice(data, at, 4)?;
    Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// `IOUtils.safelyClone(data, offset, length, max)`: the bytes from `offset`, as many as there
/// are up to `length`.
pub(crate) fn safely_clone(data: &[u8], offset: i64, length: i64, max: i64) -> Read<Vec<u8>> {
    if offset < 0 || length < 0 {
        return thrown(format!(
            "Invalid offset/length specified: offset: {offset}, length: {length}"
        ));
    }
    let real = (data.len() as i64 - offset).min(length);
    if real < 0 || real > max {
        return thrown(format!("Tried to allocate an array of length {real}"));
    }
    let start = offset as usize;
    Ok(data[start..start + real as usize].to_vec())
}

/// `InflaterInputStream` read to its end, or to `limit` bytes where a `BoundedInputStream` stops
/// it there: the inflated data, or the exception Java's stream throws first, the zlib stream
/// broken or ending early, as the first line of its stack trace.
pub(crate) fn inflate(data: &[u8], limit: Option<usize>) -> Read<Vec<u8>> {
    let mut inflater = flate2::Decompress::new(true);
    let mut out = Vec::new();
    loop {
        let written = out.len();
        if limit.is_some_and(|limit| written >= limit) {
            return Ok(out);
        }
        let room = written.max(data.len().saturating_mul(4)).max(64);
        let end = limit.map_or(written + room, |limit| limit.min(written + room));
        out.resize(end, 0);
        let consumed = inflater.total_in() as usize;
        let before = inflater.total_out();
        let status = inflater.decompress(
            &data[consumed..],
            &mut out[written..],
            flate2::FlushDecompress::None,
        );
        let produced = (inflater.total_out() - before) as usize;
        out.truncate(written + produced);
        match status {
            Ok(flate2::Status::StreamEnd) => return Ok(out),
            Ok(_) if inflater.total_in() as usize == data.len() && produced == 0 => {
                return thrown("java.io.EOFException: Unexpected end of ZLIB input stream");
            }
            Ok(_) => {}
            Err(error) => {
                return thrown(format!(
                    "java.util.zip.ZipException: {}",
                    error.message().unwrap_or("Invalid ZLIB data format")
                ))
            }
        }
    }
}

/// How a subclass of POI's `LZWDecompresser` reads its data.
pub(crate) struct Lzw {
    /// Whether a set flag bit marks a dictionary reference, not a byte.
    pub(crate) mask_means_compressed: bool,
    /// How much longer than its length field a reference copies.
    pub(crate) code_length_increase: usize,
    /// Whether a reference's position is its first byte and the high half of its second.
    pub(crate) position_is_big_endian: bool,
    /// `populateDictionary`: what the dictionary starts with.
    pub(crate) preload: &'static [u8],
    /// `adjustDictionaryOffset`.
    pub(crate) adjust: fn(usize) -> usize,
}

/// `LZWDecompresser`'s dictionary of the last 4096 bytes.
const LZW_DICTIONARY_SIZE: usize = 0x1000;
const LZW_DICTIONARY_MASK: usize = 0xFFF;

impl Lzw {
    /// `LZWDecompresser.decompress`: flags of eight items, each a byte or a reference into the
    /// dictionary of what came before, until the data ends.
    pub(crate) fn decompress(&self, data: &[u8]) -> Vec<u8> {
        let mut dictionary = [0u8; LZW_DICTIONARY_SIZE];
        dictionary[..self.preload.len()].copy_from_slice(self.preload);
        let mut position = self.preload.len();
        let mut out = Vec::with_capacity(data.len() * 2);
        let mut input = data.iter().copied();
        while let Some(flag) = input.next() {
            for bit in 0..8 {
                if (flag & (1 << bit) != 0) != self.mask_means_compressed {
                    if let Some(byte) = input.next() {
                        dictionary[position & LZW_DICTIONARY_MASK] = byte;
                        position += 1;
                        out.push(byte);
                    }
                    continue;
                }
                let (Some(first), Some(second)) = (input.next(), input.next()) else {
                    break;
                };
                let length = usize::from(second & 0x0F) + self.code_length_increase;
                let pointer = if self.position_is_big_endian {
                    (usize::from(first) << 4) + (usize::from(second) >> 4)
                } else {
                    usize::from(first) + (usize::from(second & 0xF0) << 4)
                };
                let pointer = (self.adjust)(pointer);
                for i in 0..length {
                    let byte = dictionary[(pointer + i) & LZW_DICTIONARY_MASK];
                    dictionary[(position + i) & LZW_DICTIONARY_MASK] = byte;
                    out.push(byte);
                }
                position += length;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflates_only_a_complete_stream_or_as_far_as_a_limit() {
        use std::io::Write;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"metafile").unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(inflate(&compressed, None), Ok(b"metafile".to_vec()));
        assert_eq!(
            inflate(&compressed[..compressed.len() - 5], None),
            thrown("java.io.EOFException: Unexpected end of ZLIB input stream")
        );
        assert_eq!(
            inflate(b"not zlib", None),
            thrown("java.util.zip.ZipException: incorrect header check")
        );
        let mut bad_check = compressed.clone();
        *bad_check.last_mut().unwrap() ^= 1;
        assert_eq!(
            inflate(&bad_check, None),
            thrown("java.util.zip.ZipException: incorrect data check")
        );
        assert_eq!(inflate(&bad_check, Some(4)), Ok(b"meta".to_vec()));
        assert_eq!(inflate(&compressed, Some(20)), Ok(b"metafile".to_vec()));
    }
}
