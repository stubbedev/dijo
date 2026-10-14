//! The JDK's charsets as Tika's encoding detection names and decodes them: every charset of the
//! JDK Tika runs on, found by name or alias as `Charset.forName` finds it. Single-byte charsets
//! and the East Asian multi-byte ones decode with the JDK's own tables
//! (`models/java-charsets.bin`, written by `just encoding-reference`), character for character as
//! Java does; Unicode and ISO-2022-JP decode with `encoding_rs`, whose decoders are the WHATWG
//! equivalents of the JDK's; the rest, rarely seen charsets such as the IBM EBCDIC double-byte
//! ones, are known by name but not decoded.

mod multibyte;

use std::collections::HashMap;
use std::sync::LazyLock;

use encoding_rs::Encoding;

use multibyte::{Family, MultiByte};

const TABLES: &[u8] = include_bytes!("../../../../models/java-charsets.bin");
const MAGIC: &[u8] = b"JCS2";

/// A JDK charset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Charset(u16);

enum Decoding {
    SingleByte(Box<[char; 256]>),
    MultiByte(MultiByte),
    Whatwg(&'static Encoding),
    /// Java's `UTF-16`: big-endian unless a byte order mark says otherwise, which is dropped.
    Utf16,
    Utf32 {
        big_endian: bool,
        sniff_bom: bool,
    },
    None,
}

struct Entry {
    name: String,
    decoding: Decoding,
}

struct Registry {
    entries: Vec<Entry>,
    by_label: HashMap<String, Charset>,
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    Registry::parse(TABLES).unwrap_or_else(|e| panic!("The embedded JDK charsets are invalid: {e}"))
});

/// The `encoding_rs` decoder of a JDK charset without tables, where the JDK's and WHATWG's agree.
fn whatwg(name: &str) -> Option<&'static Encoding> {
    Some(match name {
        "UTF-8" => encoding_rs::UTF_8,
        "UTF-16BE" => encoding_rs::UTF_16BE,
        "UTF-16LE" => encoding_rs::UTF_16LE,
        "ISO-2022-JP"
        | "ISO-2022-JP-2"
        | "x-windows-iso2022jp"
        | "x-windows-50220"
        | "x-windows-50221" => encoding_rs::ISO_2022_JP,
        _ => return None,
    })
}

impl Registry {
    fn parse(bytes: &'static [u8]) -> Result<Self, String> {
        let mut at = 0usize;
        let mut take = |length: usize| -> Result<&'static [u8], String> {
            let slice = bytes
                .get(at..at + length)
                .ok_or_else(|| format!("truncated at byte {at}"))?;
            at += length;
            Ok(slice)
        };
        if take(MAGIC.len())? != MAGIC {
            return Err("bad magic".to_string());
        }
        let u16_at = |slice: &[u8]| u16::from_be_bytes([slice[0], slice[1]]);
        let count = usize::from(u16_at(take(2)?));
        let mut entries = Vec::with_capacity(count);
        let mut by_label = HashMap::new();
        for index in 0..count {
            let length = usize::from(u16_at(take(2)?));
            let name = String::from_utf8(take(length)?.to_vec()).map_err(|e| e.to_string())?;
            let charset = Charset(index as u16);
            by_label.insert(name.to_ascii_lowercase(), charset);
            let aliases = u16_at(take(2)?);
            for _ in 0..aliases {
                let length = usize::from(u16_at(take(2)?));
                let alias = String::from_utf8(take(length)?.to_vec()).map_err(|e| e.to_string())?;
                by_label
                    .entry(alias.to_ascii_lowercase())
                    .or_insert(charset);
            }
            let decoding = match take(1)?[0] {
                1 => {
                    let mut table = Box::new(['\0'; 256]);
                    for slot in table.iter_mut() {
                        let unit = u16_at(take(2)?);
                        *slot = char::from_u32(u32::from(unit))
                            .ok_or_else(|| format!("{name} maps a byte to a surrogate"))?;
                    }
                    Decoding::SingleByte(table)
                }
                0 => match name.as_str() {
                    "UTF-16" => Decoding::Utf16,
                    "UTF-32" => Decoding::Utf32 {
                        big_endian: true,
                        sniff_bom: true,
                    },
                    "UTF-32BE" => Decoding::Utf32 {
                        big_endian: true,
                        sniff_bom: false,
                    },
                    "UTF-32LE" => Decoding::Utf32 {
                        big_endian: false,
                        sniff_bom: false,
                    },
                    other => whatwg(other).map_or(Decoding::None, Decoding::Whatwg),
                },
                kind => {
                    let family = Family::from_kind(kind)
                        .ok_or_else(|| format!("{name} has table kind {kind}"))?;
                    let length = u32::from_be_bytes(take(4)?.try_into().expect("four bytes"));
                    let compressed = take(length as usize)?;
                    Decoding::MultiByte(MultiByte::new(family, name.clone(), compressed))
                }
            };
            entries.push(Entry { name, decoding });
        }
        if at != bytes.len() {
            return Err(format!("{} bytes left over", bytes.len() - at));
        }
        Ok(Registry { entries, by_label })
    }
}

impl Charset {
    /// The charset `Charset.forName` finds by a name or alias, ignoring case.
    pub fn for_name(label: &str) -> Option<Charset> {
        REGISTRY.by_label.get(&label.to_ascii_lowercase()).copied()
    }

    /// A charset the detection itself names, which the JDK always has.
    pub(crate) fn named(name: &str) -> Charset {
        Self::for_name(name).unwrap_or_else(|| panic!("The JDK has no charset {name}"))
    }

    /// The JDK's canonical name.
    pub fn name(self) -> &'static str {
        &REGISTRY.entries[usize::from(self.0)].name
    }

    fn decoding(self) -> &'static Decoding {
        &REGISTRY.entries[usize::from(self.0)].decoding
    }

    /// The text `new String(bytes, charset)` gives, malformed input replaced by U+FFFD, or
    /// `None` for a charset this port does not decode.
    pub fn decode(self, bytes: &[u8]) -> Option<String> {
        Some(match self.decoding() {
            Decoding::SingleByte(table) => bytes.iter().map(|&b| table[usize::from(b)]).collect(),
            Decoding::MultiByte(decoder) => decoder.decode(bytes),
            Decoding::Whatwg(encoding) => {
                encoding.decode_without_bom_handling(bytes).0.into_owned()
            }
            Decoding::Utf16 => match bytes {
                [0xFF, 0xFE, rest @ ..] => encoding_rs::UTF_16LE
                    .decode_without_bom_handling(rest)
                    .0
                    .into_owned(),
                [0xFE, 0xFF, rest @ ..] => encoding_rs::UTF_16BE
                    .decode_without_bom_handling(rest)
                    .0
                    .into_owned(),
                _ => encoding_rs::UTF_16BE
                    .decode_without_bom_handling(bytes)
                    .0
                    .into_owned(),
            },
            Decoding::Utf32 {
                big_endian,
                sniff_bom,
            } => decode_utf32(bytes, *big_endian, *sniff_bom),
            Decoding::None => return None,
        })
    }

    /// Whether the JDK's strict decoder, given `window` as the whole input and room for one
    /// UTF-16 unit, reports an error, and otherwise how many bytes it consumed: none for a
    /// character that needs two units. `None` for a charset other than the East Asian multi-byte
    /// ones.
    pub(crate) fn decode_first(self, window: &[u8]) -> Option<Result<usize, ()>> {
        match self.decoding() {
            Decoding::MultiByte(decoder) => Some(decoder.decode_first(window)),
            _ => None,
        }
    }
}

fn decode_utf32(bytes: &[u8], big_endian: bool, sniff_bom: bool) -> String {
    let mut big_endian = big_endian;
    let mut body = bytes;
    if sniff_bom {
        match bytes {
            [0x00, 0x00, 0xFE, 0xFF, rest @ ..] => {
                big_endian = true;
                body = rest;
            }
            [0xFF, 0xFE, 0x00, 0x00, rest @ ..] => {
                big_endian = false;
                body = rest;
            }
            _ => {}
        }
    }
    let mut text = String::with_capacity(body.len() / 4);
    let (groups, remainder) = body.as_chunks::<4>();
    for &unit in groups {
        let cp = if big_endian {
            u32::from_be_bytes(unit)
        } else {
            u32::from_le_bytes(unit)
        };
        text.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
    }
    if !remainder.is_empty() {
        text.push('\u{FFFD}');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: &str =
        include_str!("../../../../tests/fixtures/charset/decoder-reference.tsv");
    const INPUTS: usize = 4000;
    const MAX_LENGTH: u64 = 16;
    const ERROR: u16 = 0xFFFF;
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x100_0000_01b3;

    struct SplitMix64(u64);

    impl SplitMix64 {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    /// The hash `DecoderReference` in `CharsetTables.java` takes of the same inputs.
    fn hash(charset: Charset) -> u64 {
        let mut random = SplitMix64(0x5EED);
        let mut hash = FNV_OFFSET;
        let mut add = |unit: u16| hash = (hash ^ u64::from(unit)).wrapping_mul(FNV_PRIME);
        for _ in 0..INPUTS {
            let length = 1 + (random.next() % MAX_LENGTH) as usize;
            let input: Vec<u8> = (0..length)
                .map(|_| {
                    let r = random.next();
                    let low = ((r >> 8) & 0x7F) as u8;
                    if r.is_multiple_of(4) {
                        low
                    } else {
                        0x80 | low
                    }
                })
                .collect();
            let text = charset
                .decode(&input)
                .expect("a multi-byte charset decodes");
            text.encode_utf16().for_each(&mut add);
            add(ERROR);
            for at in 0..input.len() {
                let window = &input[at..input.len().min(at + 4)];
                add(
                    match charset
                        .decode_first(window)
                        .expect("a multi-byte charset decodes")
                    {
                        Ok(consumed) => consumed as u16,
                        Err(()) => ERROR,
                    },
                );
            }
        }
        hash
    }

    #[test]
    fn decodes_as_the_jdk_does() {
        let mut expected: Vec<&str> = REFERENCE
            .lines()
            .map(|line| line.split('\t').next().unwrap())
            .collect();
        let mut multi_byte: Vec<&str> = REGISTRY
            .entries
            .iter()
            .filter(|entry| matches!(entry.decoding, Decoding::MultiByte(_)))
            .map(|entry| entry.name.as_str())
            .collect();
        expected.sort_unstable();
        multi_byte.sort_unstable();
        assert_eq!(
            expected, multi_byte,
            "the reference covers every multi-byte charset"
        );
        let mismatches: Vec<String> = REFERENCE
            .lines()
            .filter_map(|line| {
                let (name, reference) = line.split_once('\t')?;
                let actual = format!("{:x}", hash(Charset::named(name)));
                (actual != reference).then(|| format!("{name}: JDK {reference}, port {actual}"))
            })
            .collect();
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }
}
