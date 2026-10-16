//! POI's `CompressedRTF` and `MAPIRtfAttribute`: the RTF of a message's compressed RTF body,
//! decompressed with the LZFu dictionary preloaded as the format defines, or stored as it is.

use crate::formats::poi::{safely_clone, thrown, Read};

const COMPRESSED: u32 = u32::from_le_bytes(*b"LZFu");
const UNCOMPRESSED: u32 = u32::from_le_bytes(*b"MELA");
const DICTIONARY_SIZE: usize = 0x1000;
const DICTIONARY_MASK: usize = 0xFFF;
const PRELOAD: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\n\r\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";
/// `MAPIRtfAttribute`'s largest record.
const MAX_RECORD_LENGTH: i64 = 50_000_000;
/// How much longer than its pointer's length a dictionary reference is.
const CODE_LENGTH_INCREASE: usize = 2;

/// `new MAPIRtfAttribute(RTF_COMPRESSED, BINARY, data).getData()`: the RTF, cut to the size
/// its header gives where the data decompresses to more.
pub(crate) fn decompress(data: &[u8]) -> Read<Vec<u8>> {
    let Some(header) = data.get(..16) else {
        return thrown("a compressed RTF body ends before its header");
    };
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("four bytes"));
    let size = word(4) as i32;
    let body = &data[16..];
    let mut out = match word(8) {
        UNCOMPRESSED => body.to_vec(),
        COMPRESSED => lzfu(body),
        signature => {
            return thrown(format!(
                "Invalid compression signature {}",
                signature as i32
            ))
        }
    };
    if out.len() as i64 > i64::from(size) {
        out = safely_clone(&out, 0, i64::from(size), MAX_RECORD_LENGTH)?;
    }
    Ok(out)
}

/// `LZWDecompresser.decompress`: flags of eight items, each a byte or a reference into the
/// dictionary of what came before, until the data ends.
fn lzfu(data: &[u8]) -> Vec<u8> {
    let mut dictionary = [0u8; DICTIONARY_SIZE];
    dictionary[..PRELOAD.len()].copy_from_slice(PRELOAD);
    let mut position = PRELOAD.len();
    let mut out = Vec::with_capacity(data.len() * 2);
    let mut input = data.iter().copied();
    while let Some(flag) = input.next() {
        for bit in 0..8 {
            if flag & (1 << bit) == 0 {
                if let Some(byte) = input.next() {
                    dictionary[position & DICTIONARY_MASK] = byte;
                    position += 1;
                    out.push(byte);
                }
                continue;
            }
            let (Some(high), Some(low)) = (input.next(), input.next()) else {
                break;
            };
            let length = usize::from(low & 0x0F) + CODE_LENGTH_INCREASE;
            let pointer = (usize::from(high) << 4) + (usize::from(low) >> 4);
            for i in 0..length {
                let byte = dictionary[(pointer + i) & DICTIONARY_MASK];
                dictionary[(position + i) & DICTIONARY_MASK] = byte;
                out.push(byte);
            }
            position += length;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(size: u32, signature: &[u8; 4]) -> Vec<u8> {
        let mut data = 0u32.to_le_bytes().to_vec();
        data.extend(size.to_le_bytes());
        data.extend(signature);
        data.extend([0; 4]);
        data
    }

    #[test]
    fn decompresses_as_poi_does() {
        let mut data = header(4, b"LZFu");
        data.extend([0, b'a', b'b', b'c', b'd', b'e', b'f', b'g', b'h']);
        assert_eq!(decompress(&data).unwrap(), b"abcd");
        let mut data = header(100, b"MELA");
        data.extend(b"{\\rtf1}");
        assert_eq!(decompress(&data).unwrap(), b"{\\rtf1}");
        assert!(decompress(&header(1, b"NONE")).is_err());
    }

    #[test]
    fn copies_from_the_preloaded_dictionary() {
        let mut data = header(100, b"LZFu");
        data.extend([1, 0x00, 0x03]);
        assert_eq!(decompress(&data).unwrap(), b"{\\rtf");
    }
}
