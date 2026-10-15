//! Tika's `BPListDetector`: binary property lists, named by the keys of their root dictionary,
//! the whole list parsed as dd-plist's `BinaryPropertyListParser` parses it. A list it rejects
//! makes Tika's detection fail.

use std::collections::{HashMap, HashSet};

use crate::mime::media_type::MediaType;

use super::Thrown;

const MAX_NESTING_DEPTH: usize = 512;

/// What the parse makes of an object, as far as detection looks.
#[derive(Debug, Clone)]
enum Object {
    Null,
    Text(String),
    Dictionary(HashSet<Option<String>>),
    Other,
}

struct Parser<'a> {
    bytes: &'a [u8],
    object_ref_size: usize,
    offset_size: usize,
    objects: usize,
    offset_table: usize,
    parsed: HashMap<usize, Object>,
}

fn unsigned(bytes: &[u8], start: usize, end: usize) -> u64 {
    bytes[start..end]
        .iter()
        .fold(0u64, |value, &b| (value << 8) | u64::from(b))
        & 0xFFFF_FFFF
}

impl Parser<'_> {
    fn safe_add(&self, offset: usize, length: usize) -> Result<usize, Thrown> {
        let end = offset.checked_add(length).ok_or(Thrown::Io)?;
        if end > self.offset_table {
            return Err(Thrown::Io);
        }
        Ok(end)
    }

    fn object_offset(&self, object: usize) -> Result<usize, Thrown> {
        if object >= self.objects {
            return Err(Thrown::Io);
        }
        let start = self.offset_table + object * self.offset_size;
        let offset = unsigned(self.bytes, start, start + self.offset_size);
        let offset = usize::try_from(i32::try_from(offset).map_err(|_| Thrown::Runtime)?)
            .expect("a non-negative offset");
        if offset > self.offset_table {
            return Err(Thrown::Io);
        }
        Ok(offset)
    }

    /// `readLengthAndOffset`: an object's count, and how many bytes its marker and count take.
    fn length_and_offset(&self, info: u8, offset: usize) -> Result<(usize, usize), Thrown> {
        if info != 0x0F {
            return Ok((usize::from(info), 1));
        }
        let marker = *self.bytes.get(offset + 1).ok_or(Thrown::Io)?;
        let int_length = 1usize << (marker & 0x0F);
        let length_length = 2 + int_length;
        let end = self.safe_add(offset, length_length)?;
        let digits = &self.bytes[offset + 2..end];
        let value: i64 = if int_length < 3 {
            i64::try_from(unsigned(digits, 0, digits.len())).expect("masked to 32 bits")
        } else {
            let low = digits[digits.len().saturating_sub(4)..]
                .iter()
                .fold(0u32, |value, &b| (value << 8) | u32::from(b));
            let sign_extended = if digits.len() < 4 && digits[0] & 0x80 != 0 {
                low | (u32::MAX << (8 * digits.len()))
            } else {
                low
            };
            i64::from(sign_extended as i32)
        };
        let value = usize::try_from(value).map_err(|_| Thrown::Io)?;
        Ok((value, length_length))
    }

    fn validate_references(&self, base: usize, count: usize) -> Result<(), Thrown> {
        let end = count
            .checked_mul(self.object_ref_size)
            .and_then(|size| base.checked_add(size))
            .ok_or(Thrown::Io)?;
        if end > self.offset_table {
            return Err(Thrown::Io);
        }
        Ok(())
    }

    fn reference(&self, base: usize, index: usize) -> Result<usize, Thrown> {
        let at = base + index * self.object_ref_size;
        let value = unsigned(self.bytes, at, at + self.object_ref_size);
        let value = i32::try_from(value).map_err(|_| Thrown::Runtime)?;
        usize::try_from(value).map_err(|_| Thrown::Runtime)
    }

    /// `calculateUtf8StringLength`, with its counting of two bytes for every multi-byte
    /// character.
    fn utf8_length(&self, offset: usize, characters: usize) -> usize {
        let mut length = 0usize;
        for _ in 0..characters {
            let at = offset + length;
            if at >= self.offset_table {
                return characters;
            }
            let byte = self.bytes[at];
            if byte & 0x80 != 0x80 {
                length += 1;
                continue;
            }
            let following = if byte & 0xC0 == 0x80 {
                return characters;
            } else if byte & 0xE0 == 0xC0 {
                1
            } else if byte & 0xF0 == 0xE0 {
                2
            } else if byte & 0xF8 == 0xF0 {
                3
            } else {
                0
            };
            let complete = (1..=following)
                .all(|i| at + i < self.offset_table && self.bytes[at + i] & 0xC0 == 0x80);
            if !complete {
                return characters;
            }
            length += 2;
        }
        length
    }

    fn parse(&mut self, stack: &[usize], object: usize) -> Result<Object, Thrown> {
        if stack.len() >= MAX_NESTING_DEPTH || stack.contains(&object) {
            return Err(Thrown::Io);
        }
        if let Some(parsed) = self.parsed.get(&object) {
            return Ok(parsed.clone());
        }
        let mut stack = stack.to_vec();
        stack.push(object);
        let offset = self.object_offset(object)?;
        let marker = self.bytes[offset];
        let (kind, info) = (marker >> 4, marker & 0x0F);
        let parsed = match kind {
            0x0 => match info {
                0x0 => Object::Null,
                0x8 | 0x9 => Object::Other,
                0xE => {
                    self.safe_add(offset, 17)?;
                    Object::Other
                }
                _ => return Err(Thrown::Io),
            },
            0x1 | 0x2 => {
                let length = 1usize << info;
                self.safe_add(offset, length + 1)?;
                if kind == 0x2 && length != 4 && length != 8 {
                    return Err(Thrown::Runtime);
                }
                Object::Other
            }
            0x3 => {
                if info != 0x3 {
                    return Err(Thrown::Io);
                }
                self.safe_add(offset, 9)?;
                Object::Other
            }
            0x4 => {
                let (length, header) = self.length_and_offset(info, offset)?;
                self.safe_add(offset + header, length)?;
                Object::Other
            }
            0x5..=0x7 => {
                let (count, header) = self.length_and_offset(info, offset)?;
                let start = offset + header;
                let length = match kind {
                    0x5 => count,
                    0x6 => count.checked_mul(2).ok_or(Thrown::Io)?,
                    _ => self.utf8_length(start, count),
                };
                let end = self.safe_add(start, length)?;
                let bytes = &self.bytes[start..end];
                Object::Text(match kind {
                    0x5 => bytes
                        .iter()
                        .map(|&b| {
                            if b.is_ascii() {
                                char::from(b)
                            } else {
                                '\u{FFFD}'
                            }
                        })
                        .collect(),
                    0x6 => {
                        let units: Vec<u16> = bytes
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|pair| u16::from_be_bytes(*pair))
                            .collect();
                        String::from_utf16_lossy(&units)
                    }
                    _ => String::from_utf8_lossy(bytes).into_owned(),
                })
            }
            0x8 => {
                self.safe_add(offset, usize::from(info) + 2)?;
                Object::Other
            }
            0xA..=0xC => {
                let (count, header) = self.length_and_offset(info, offset)?;
                let base = offset + header;
                self.validate_references(base, count)?;
                let mut seen = HashSet::new();
                for index in 0..count {
                    let reference = self.reference(base, index)?;
                    if kind == 0xA || seen.insert(reference) {
                        self.parse(&stack, reference)?;
                    }
                }
                Object::Other
            }
            0xD => {
                let (count, header) = self.length_and_offset(info, offset)?;
                let keys = offset + header;
                self.validate_references(keys, count)?;
                let values = keys + count * self.object_ref_size;
                self.validate_references(values, count)?;
                let mut names = HashSet::new();
                for index in 0..count {
                    let key_reference = self.reference(keys, index)?;
                    let value_reference = self.reference(values, index)?;
                    let key = self.parse(&stack, key_reference)?;
                    let name = match key {
                        Object::Null => return Err(Thrown::Io),
                        Object::Text(text) => Some(text),
                        Object::Dictionary(_) | Object::Other => None,
                    };
                    self.parse(&stack, value_reference)?;
                    names.insert(name);
                }
                Object::Dictionary(names)
            }
            _ => return Err(Thrown::Io),
        };
        self.parsed.insert(object, parsed.clone());
        Ok(parsed)
    }
}

/// `BinaryPropertyListParser.parse`: the root object of a binary property list.
fn parse_root(bytes: &[u8]) -> Result<Object, Thrown> {
    if bytes.len() < 8
        || !bytes.starts_with(b"bplist")
        || !bytes[6..8].iter().all(u8::is_ascii_digit)
    {
        return Err(Thrown::Io);
    }
    if bytes[6] != b'0' || bytes.len() < 40 {
        return Err(Thrown::Io);
    }
    let trailer = &bytes[bytes.len() - 32..];
    let offset_size = unsigned(trailer, 6, 7) as usize;
    let object_ref_size = unsigned(trailer, 7, 8) as usize;
    let objects = unsigned(trailer, 8, 16);
    let top = unsigned(trailer, 16, 24);
    let offset_table = unsigned(trailer, 24, 32);
    let length = bytes.len() as u64;
    let table_end = (objects + 1)
        .checked_mul(offset_size as u64)
        .and_then(|size| size.checked_add(offset_table));
    if offset_size < 1
        || object_ref_size < 1
        || objects > i32::MAX as u64
        || top >= length - 32
        || offset_table > length
        || table_end.is_none_or(|end| end > length)
    {
        return Err(Thrown::Io);
    }
    let mut parser = Parser {
        bytes,
        object_ref_size,
        offset_size,
        objects: objects as usize,
        offset_table: offset_table as usize,
        parsed: HashMap::new(),
    };
    parser.parse(&[], top as usize)
}

/// The binary property list type, `None` for a document that is not one, or what Tika throws
/// for one it cannot parse.
pub(crate) fn detect(data: &[u8]) -> Result<Option<MediaType>, Thrown> {
    if data.len() < 6 || !data.starts_with(b"bplist") {
        return Ok(None);
    }
    let kind = match parse_root(data)? {
        Object::Dictionary(keys) => {
            let has = |key: &str| keys.contains(&Some(key.to_owned()));
            if has("nodes") && has("edges") && has("graphEncodingVersion") {
                "application/x-bplist-memgraph"
            } else if has("WebMainResource") {
                "application/x-bplist-webarchive"
            } else if has("Playlists") && has("Tracks") && has("Music Folder") {
                "application/x-bplist-itunes"
            } else {
                "application/x-bplist"
            }
        }
        _ => "application/x-bplist",
    };
    Ok(Some(MediaType::named(kind)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A binary property list of one dictionary mapping each key to itself.
    fn plist(keys: &[&str]) -> Vec<u8> {
        let mut data = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        offsets.push(data.len());
        data.push(0xD0 | keys.len() as u8);
        for i in 0..keys.len() {
            data.push(i as u8 + 1);
        }
        for i in 0..keys.len() {
            data.push(i as u8 + 1);
        }
        for key in keys {
            offsets.push(data.len());
            if key.len() < 15 {
                data.push(0x50 | key.len() as u8);
            } else {
                data.extend([0x5F, 0x10, key.len() as u8]);
            }
            data.extend(key.as_bytes());
        }
        let table = data.len();
        data.extend(offsets.iter().map(|&o| o as u8));
        let mut trailer = vec![0u8; 6];
        trailer.push(1);
        trailer.push(1);
        trailer.extend((offsets.len() as u64).to_be_bytes());
        trailer.extend(0u64.to_be_bytes());
        trailer.extend((table as u64).to_be_bytes());
        data.extend(trailer);
        data
    }

    #[test]
    fn names_lists_by_their_keys() {
        assert_eq!(
            detect(&plist(&["WebMainResource"]))
                .ok()
                .flatten()
                .unwrap()
                .as_str(),
            "application/x-bplist-webarchive"
        );
        assert_eq!(
            detect(&plist(&["Tracks", "Playlists", "Music Folder"]))
                .ok()
                .flatten()
                .unwrap()
                .as_str(),
            "application/x-bplist-itunes"
        );
        assert_eq!(
            detect(&plist(&["x"])).ok().flatten().unwrap().as_str(),
            "application/x-bplist"
        );
    }

    #[test]
    fn fails_on_lists_it_cannot_parse() {
        let mut broken = plist(&["x"]);
        let at = broken.len() - 9;
        broken[at] = 0xFF;
        assert!(detect(&broken).is_err());
        assert!(detect(b"bplist01").is_err());
        assert!(detect(b"<plist>").unwrap().is_none());
    }
}
