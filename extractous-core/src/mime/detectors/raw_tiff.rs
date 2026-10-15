//! Tika's `RawTiffDetector`: camera raw images in TIFF containers, by their DNG version tag,
//! a vendor's compression or a raw image of a known make, within the first megabyte.

use std::collections::{HashSet, VecDeque};

const MAX_PREFIX_LENGTH: u64 = 1024 * 1024;
const CHUNK_LENGTH: u64 = 64 * 1024;
const MAX_IFDS: usize = 32;
const MAX_ENTRIES_PER_IFD: u64 = 1024;
const MAX_MAKE_LENGTH: u64 = 256;

const TAG_NEW_SUBFILE_TYPE: u16 = 0x00FE;
const TAG_BITS_PER_SAMPLE: u16 = 0x0102;
const TAG_MAKE: u16 = 0x010F;
const TAG_COMPRESSION: u16 = 0x0103;
const TAG_PHOTOMETRIC_INTERPRETATION: u16 = 0x0106;
const TAG_SUB_IFDS: u16 = 0x014A;
const TAG_DNG_VERSION: u16 = 0xC612;

const NIKON: &str = "image/x-raw-nikon";
const PENTAX: &str = "image/x-raw-pentax";
const SONY: &str = "image/x-raw-sony";
const SAMSUNG: &str = "image/x-raw-samsung";
const ADOBE: &str = "image/x-raw-adobe";

/// The bytes the detector has read: it reads the stream in 64 KiB chunks as offsets demand,
/// up to a megabyte, and only what it has read is in bounds.
struct Prefix<'a> {
    data: &'a [u8],
    length: u64,
}

impl Prefix<'_> {
    fn ensure(&mut self, end: u64) -> bool {
        if end > MAX_PREFIX_LENGTH {
            return false;
        }
        if end <= self.length {
            return true;
        }
        let wanted = MAX_PREFIX_LENGTH.min(end.div_ceil(CHUNK_LENGTH) * CHUNK_LENGTH);
        self.length = self.length.max(wanted.min(self.data.len() as u64));
        end <= self.length
    }

    fn u16(&self, offset: u64, big_endian: bool) -> u64 {
        let at = offset as usize;
        let pair = [self.data[at], self.data[at + 1]];
        u64::from(if big_endian {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    }

    fn u32(&self, offset: u64, big_endian: bool) -> u64 {
        let (high, low) = (
            self.u16(offset, big_endian),
            self.u16(offset + 2, big_endian),
        );
        if big_endian {
            (high << 16) | low
        } else {
            (low << 16) | high
        }
    }

    fn u64(&self, offset: u64, big_endian: bool) -> u64 {
        let (high, low) = (
            self.u32(offset, big_endian),
            self.u32(offset + 4, big_endian),
        );
        if big_endian {
            (high << 32) | low
        } else {
            (low << 32) | high
        }
    }
}

fn is_tiff(header: &[u8]) -> bool {
    let big_endian = header.starts_with(b"MM");
    if !big_endian && !header.starts_with(b"II") {
        return false;
    }
    let pair = [header[2], header[3]];
    let magic = if big_endian {
        u16::from_be_bytes(pair)
    } else {
        u16::from_le_bytes(pair)
    };
    magic == 42 || magic == 43
}

/// The raw image type, or `None` for anything else.
pub(crate) fn detect(data: &[u8]) -> Option<&'static str> {
    if data.len() < 16 || !is_tiff(data) {
        return None;
    }
    let mut prefix = Prefix { data, length: 16 };
    let big_endian = data[0] == b'M';
    let big_tiff = prefix.u16(2, big_endian) == 43;
    let (count_size, entry_size, offset_size, inline_size) = if big_tiff {
        (8, 20, 8, 8)
    } else {
        (2, 12, 4, 4)
    };
    let first = if big_tiff {
        if prefix.u16(4, big_endian) != 8 {
            return None;
        }
        prefix.u64(8, big_endian)
    } else {
        prefix.u32(4, big_endian)
    };
    let mut dng = false;
    let mut by_compression = None;
    let mut raw_image = false;
    let mut make: Option<String> = None;
    let mut visited = HashSet::new();
    let mut to_visit = VecDeque::from([first]);
    while visited.len() < MAX_IFDS {
        let Some(ifd) = to_visit.pop_front() else {
            break;
        };
        if ifd == 0
            || ifd > i64::MAX as u64
            || !visited.insert(ifd)
            || !prefix.ensure(ifd + count_size)
        {
            continue;
        }
        let entries_count = if big_tiff {
            prefix.u64(ifd, big_endian)
        } else {
            prefix.u16(ifd, big_endian)
        };
        if entries_count > MAX_ENTRIES_PER_IFD
            || !prefix.ensure(ifd + count_size + entries_count * entry_size + offset_size)
        {
            continue;
        }
        let entries = ifd + count_size;
        let mut subfile_type: i64 = -1;
        let mut photometric = false;
        let mut bits_per_sample: Vec<u64> = Vec::new();
        for i in 0..entries_count {
            let e = entries + i * entry_size;
            let tag = prefix.u16(e, big_endian) as u16;
            let kind = prefix.u16(e + 2, big_endian);
            let count = if big_tiff {
                prefix.u64(e + 4, big_endian)
            } else {
                prefix.u32(e + 4, big_endian)
            };
            let value = e + 4 + inline_size;
            let values =
                |count: u64| long_values(&prefix, value, big_endian, big_tiff, kind, count);
            match tag {
                TAG_DNG_VERSION => dng = true,
                TAG_SUB_IFDS => {
                    for sub in values(count.min(MAX_IFDS as u64)) {
                        if to_visit.len() < MAX_IFDS {
                            to_visit.push_back(sub);
                        }
                    }
                }
                TAG_COMPRESSION if by_compression.is_none() && count == 1 => {
                    by_compression = match values(count).as_slice() {
                        [compression] => vendor_by_compression(*compression),
                        _ => None,
                    };
                }
                TAG_PHOTOMETRIC_INTERPRETATION => {
                    photometric = true;
                    if count == 1 && matches!(values(count).as_slice(), [32803 | 34892]) {
                        raw_image = true;
                    }
                }
                TAG_NEW_SUBFILE_TYPE if count == 1 => {
                    subfile_type = match values(count).as_slice() {
                        [v] => *v as i64,
                        _ => -1,
                    };
                }
                TAG_BITS_PER_SAMPLE if count <= 4 => bits_per_sample = values(count),
                TAG_MAKE if make.is_none() && kind == 2 => {
                    make = ascii_value(&prefix, value, big_endian, big_tiff, count);
                }
                _ => {}
            }
        }
        if !photometric
            && subfile_type == 0
            && matches!(bits_per_sample.as_slice(), [bits] if (9..=16).contains(bits))
        {
            raw_image = true;
        }
        let follower = entries + entries_count * entry_size;
        let next = if big_tiff {
            prefix.u64(follower, big_endian)
        } else {
            prefix.u32(follower, big_endian)
        };
        if to_visit.len() < MAX_IFDS {
            to_visit.push_back(next);
        }
    }
    if dng {
        return Some(ADOBE);
    }
    if by_compression.is_some() {
        return by_compression;
    }
    if raw_image {
        return make.as_deref().and_then(vendor_by_make);
    }
    None
}

fn vendor_by_compression(compression: u64) -> Option<&'static str> {
    match compression {
        34713 => Some(NIKON),
        32767 => Some(SONY),
        65535 => Some(PENTAX),
        32770 | 32772 => Some(SAMSUNG),
        _ => None,
    }
}

fn vendor_by_make(make: &str) -> Option<&'static str> {
    let make = make.trim_matches(|c: char| c <= ' ').to_ascii_uppercase();
    if make.starts_with("NIKON") {
        Some(NIKON)
    } else if make.starts_with("PENTAX") || make.starts_with("RICOH") {
        Some(PENTAX)
    } else if make.starts_with("SONY") {
        Some(SONY)
    } else if make.starts_with("SAMSUNG") {
        Some(SAMSUNG)
    } else {
        None
    }
}

fn long_values(
    prefix: &Prefix<'_>,
    value: u64,
    big_endian: bool,
    big_tiff: bool,
    kind: u64,
    count: u64,
) -> Vec<u64> {
    let size = match kind {
        3 => 2,
        4 | 13 => 4,
        16 | 18 if big_tiff => 8,
        _ => return Vec::new(),
    };
    if !(1..=MAX_ENTRIES_PER_IFD).contains(&count) {
        return Vec::new();
    }
    let total = size * count;
    let offset = if total <= if big_tiff { 8 } else { 4 } {
        value
    } else {
        let at = if big_tiff {
            prefix.u64(value, big_endian)
        } else {
            prefix.u32(value, big_endian)
        };
        if at > i64::MAX as u64 || (at as i64) > prefix.length as i64 - total as i64 {
            return Vec::new();
        }
        at
    };
    (0..count)
        .map(|i| {
            let p = offset + i * size;
            match size {
                2 => prefix.u16(p, big_endian),
                4 => prefix.u32(p, big_endian),
                _ => prefix.u64(p, big_endian),
            }
        })
        .collect()
}

fn ascii_value(
    prefix: &Prefix<'_>,
    value: u64,
    big_endian: bool,
    big_tiff: bool,
    count: u64,
) -> Option<String> {
    if !(1..=MAX_MAKE_LENGTH).contains(&count) {
        return None;
    }
    let offset = if count <= if big_tiff { 8 } else { 4 } {
        value
    } else {
        let at = if big_tiff {
            prefix.u64(value, big_endian)
        } else {
            prefix.u32(value, big_endian)
        };
        if at > i64::MAX as u64 || (at as i64) > prefix.length as i64 - count as i64 {
            return None;
        }
        at
    };
    let field = &prefix.data[offset as usize..(offset + count) as usize];
    let text = field.iter().take_while(|&&b| b != 0);
    Some(
        text.map(|&b| {
            if b.is_ascii() {
                char::from(b)
            } else {
                '\u{FFFD}'
            }
        })
        .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian TIFF with one IFD of the given (tag, type, count, value) entries.
    fn tiff(entries: &[(u16, u16, u32, u32)], extra: &[u8]) -> Vec<u8> {
        let mut data = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        data.extend((entries.len() as u16).to_le_bytes());
        for &(tag, kind, count, value) in entries {
            data.extend(tag.to_le_bytes());
            data.extend(kind.to_le_bytes());
            data.extend(count.to_le_bytes());
            data.extend(value.to_le_bytes());
        }
        data.extend(0u32.to_le_bytes());
        data.extend(extra);
        data
    }

    #[test]
    fn finds_dng_and_vendor_compression() {
        assert_eq!(
            detect(&tiff(&[(TAG_DNG_VERSION, 1, 4, 0x0104)], &[])),
            Some(ADOBE)
        );
        assert_eq!(
            detect(&tiff(&[(TAG_COMPRESSION, 3, 1, 34713)], &[])),
            Some(NIKON)
        );
        assert_eq!(detect(&tiff(&[(TAG_COMPRESSION, 3, 1, 5)], &[])), None);
    }

    #[test]
    fn needs_a_raw_image_for_the_make() {
        let make = |entries: u32| (TAG_MAKE, 2, 6, 8 + 2 + entries * 12 + 4);
        let cfa = (TAG_PHOTOMETRIC_INTERPRETATION, 3, 1, 32803);
        assert_eq!(detect(&tiff(&[make(2), cfa], b"SONY \0")), Some(SONY));
        assert_eq!(detect(&tiff(&[make(1)], b"SONY \0")), None);
    }

    #[test]
    fn ignores_what_is_not_tiff() {
        assert_eq!(detect(b"%PDF-1.4 and some more bytes"), None);
        assert_eq!(detect(b"II\x2a\x00"), None);
    }
}
