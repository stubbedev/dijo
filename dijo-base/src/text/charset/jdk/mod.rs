//! The JDK's charsets as Tika's encoding detection names and decodes them: every charset of the
//! JDK Tika runs on, found by name or alias as `Charset.forName` finds it. Single-byte charsets
//! and the East Asian multi-byte ones decode with the JDK's own tables
//! (`models/java-charsets.bin`, written by `just encoding-reference`); UTF-8, UTF-16 and
//! ISO-2022-JP decode with `encoding_rs`, as WHATWG specifies, each malformed sequence one U+FFFD
//! (#186); UTF-32 decodes each unit outside Unicode, and a partial unit at the end, as one U+FFFD;
//! ISCII-91, whose decoder holds characters back, and ISO-2022-KR and ISO-2022-CN, whose decoders
//! read the JDK's KS X 1001, GB 2312 and CNS 11643 tables, decode with ports of the JDK's
//! decoders; the rest, rarely seen charsets such as the IBM EBCDIC double-byte ones, are known by
//! name but not decoded.

mod iscii91;
mod iso2022;
mod multibyte;

use std::collections::HashMap;
use std::sync::LazyLock;

use encoding_rs::Encoding;

use iso2022::Variant;
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
    /// Java's `UTF-16`: a byte order mark at the start sets the byte order and is dropped, and
    /// without one the text is big-endian.
    Utf16,
    Utf32 {
        big_endian: bool,
        sniff_bom: bool,
    },
    Iscii91,
    Iso2022(Variant),
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
                    "UTF-8" => Decoding::Whatwg(encoding_rs::UTF_8),
                    "UTF-16" => Decoding::Utf16,
                    "UTF-16BE" => Decoding::Whatwg(encoding_rs::UTF_16BE),
                    "UTF-16LE" => Decoding::Whatwg(encoding_rs::UTF_16LE),
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
                    "x-ISCII91" => Decoding::Iscii91,
                    other => match Variant::from_name(other) {
                        Some(variant) => Decoding::Iso2022(variant),
                        None => whatwg(other).map_or(Decoding::None, Decoding::Whatwg),
                    },
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
    pub fn named(name: &str) -> Charset {
        Self::for_name(name).unwrap_or_else(|| panic!("The JDK has no charset {name}"))
    }

    /// The JDK's canonical name.
    pub fn name(self) -> &'static str {
        &REGISTRY.entries[usize::from(self.0)].name
    }

    fn decoding(self) -> &'static Decoding {
        &REGISTRY.entries[usize::from(self.0)].decoding
    }

    /// The tables of an East Asian multi-byte charset the port itself names.
    fn multi_byte(name: &str) -> &'static MultiByte {
        match Self::named(name).decoding() {
            Decoding::MultiByte(decoder) => decoder,
            _ => panic!("The JDK's {name} is no multi-byte charset"),
        }
    }

    fn iso2022(variant: Variant) -> iso2022::Decoder {
        iso2022::Decoder::new(variant, Self::multi_byte)
    }

    /// Whether this port decodes the charset.
    pub fn decodes(self) -> bool {
        !matches!(self.decoding(), Decoding::None)
    }

    /// The text of `bytes`, malformed input replaced by U+FFFD, or `None` for a charset this
    /// port does not decode.
    pub fn decode(self, bytes: &[u8]) -> Option<String> {
        Some(match self.decoding() {
            Decoding::SingleByte(table) => bytes.iter().map(|&b| table[usize::from(b)]).collect(),
            Decoding::MultiByte(decoder) => decoder.decode(bytes),
            Decoding::Whatwg(encoding) => {
                encoding.decode_without_bom_handling(bytes).0.into_owned()
            }
            Decoding::Utf16 => {
                let (rest, encoding) = utf16_bom(bytes);
                encoding.decode_without_bom_handling(rest).0.into_owned()
            }
            Decoding::Utf32 {
                big_endian,
                sniff_bom,
            } => decode_utf32(bytes, *big_endian, *sniff_bom),
            Decoding::Iscii91 => iscii91::decode(bytes),
            Decoding::Iso2022(variant) => {
                Self::iso2022(*variant).decode_prefix(bytes, bytes.len()).0
            }
            Decoding::None => return None,
        })
    }

    /// The text of `bytes` as [`Charset::decode`] gives it, in pieces decoded one at a time
    /// from at least `size` bytes each, the last from what is left, so that decoding a large
    /// input holds no more than a piece of it. `None` for a charset this port does not decode.
    pub fn pieces(self, bytes: &[u8], size: usize) -> Option<Pieces<'_>> {
        let decoding = self.decoding();
        let mut whatwg = None;
        let (rest, big_endian) = match decoding {
            Decoding::None => return None,
            Decoding::Whatwg(encoding) => {
                whatwg = Some(encoding.new_decoder_without_bom_handling());
                (bytes, true)
            }
            Decoding::Utf16 => {
                let (rest, encoding) = utf16_bom(bytes);
                whatwg = Some(encoding.new_decoder_without_bom_handling());
                (rest, true)
            }
            Decoding::Utf32 {
                big_endian,
                sniff_bom,
            } => match bytes {
                [0x00, 0x00, 0xFE, 0xFF, rest @ ..] if *sniff_bom => (rest, true),
                [0xFF, 0xFE, 0x00, 0x00, rest @ ..] if *sniff_bom => (rest, false),
                _ => (bytes, *big_endian),
            },
            _ => (bytes, true),
        };
        let dropped_bom = rest.len() < bytes.len();
        let iso2022 = match decoding {
            Decoding::Iso2022(variant) => Some(Self::iso2022(*variant)),
            _ => None,
        };
        Some(Pieces {
            decoding,
            rest,
            size: size.max(1),
            big_endian,
            whatwg,
            iso2022,
            dropped_bom,
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

/// The pieces of [`Charset::pieces`]. Each piece ends where decoding it apart from what
/// follows gives what decoding the whole gives: anywhere for a single-byte charset and after a
/// whole unit for UTF-32, once a byte order mark has set the byte order; ISCII-91, whose decoder
/// can hold a character back to the end, in one piece. The East Asian multi-byte charsets decode
/// the characters that end past the piece's size, looking ahead into what follows as the whole's
/// decoding does, and so do ISO-2022-KR and ISO-2022-CN, whose decoders keep the state their
/// escapes and shifts set from one piece to the next; UTF-8, UTF-16 and ISO-2022-JP decode with
/// `encoding_rs`'s streaming decoders.
pub struct Pieces<'a> {
    decoding: &'static Decoding,
    rest: &'a [u8],
    size: usize,
    big_endian: bool,
    whatwg: Option<encoding_rs::Decoder>,
    iso2022: Option<iso2022::Decoder>,
    /// Whether a byte order mark that set the byte order was dropped from the start.
    dropped_bom: bool,
}

impl Pieces<'_> {
    /// Whether the decoder dropped a byte order mark from the start, which Java's `UTF-16` and
    /// `UTF-32` read to set the byte order.
    pub fn dropped_bom(&self) -> bool {
        self.dropped_bom
    }

    /// How many of the bytes are still to be decoded.
    pub fn unread(&self) -> usize {
        self.rest.len()
    }

    /// Where the next piece ends.
    fn cut(&self) -> usize {
        let rest = self.rest;
        let size = self.size;
        if rest.len() <= size {
            return rest.len();
        }
        match self.decoding {
            Decoding::SingleByte(_)
            | Decoding::MultiByte(_)
            | Decoding::Iso2022(_)
            | Decoding::Whatwg(_)
            | Decoding::Utf16
            | Decoding::None => size,
            Decoding::Iscii91 => rest.len(),
            Decoding::Utf32 { .. } => size.next_multiple_of(4).min(rest.len()),
        }
    }
}

impl Iterator for Pieces<'_> {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        if self.rest.is_empty() {
            return None;
        }
        if let Decoding::MultiByte(decoder) = self.decoding {
            let (text, read) = decoder.decode_prefix(self.rest, self.size);
            self.rest = &self.rest[read..];
            return Some(text);
        }
        if let Some(decoder) = &mut self.iso2022 {
            let (text, read) = decoder.decode_prefix(self.rest, self.size);
            self.rest = &self.rest[read..];
            return Some(text);
        }
        let (piece, rest) = self.rest.split_at(self.cut());
        self.rest = rest;
        Some(match self.decoding {
            Decoding::SingleByte(table) => piece.iter().map(|&b| table[usize::from(b)]).collect(),
            Decoding::MultiByte(_) | Decoding::Iso2022(_) => {
                unreachable!("a multi-byte or ISO-2022 charset decodes its own pieces")
            }
            Decoding::Utf32 { .. } => decode_utf32(piece, self.big_endian, false),
            Decoding::Iscii91 => iscii91::decode(piece),
            Decoding::Whatwg(_) | Decoding::Utf16 => {
                let decoder = self
                    .whatwg
                    .as_mut()
                    .expect("an encoding_rs charset's pieces hold its decoder");
                let last = self.rest.is_empty();
                let mut text = String::with_capacity(
                    decoder
                        .max_utf8_buffer_length(piece.len())
                        .expect("a piece's text fits in memory"),
                );
                let (result, read, _) = decoder.decode_to_string(piece, &mut text, last);
                assert!(
                    result == encoding_rs::CoderResult::InputEmpty && read == piece.len(),
                    "a buffer of the decoder's maximum length holds the piece's text"
                );
                text
            }
            Decoding::None => unreachable!("a charset without a decoder has no pieces"),
        })
    }
}

/// The bytes of Java's `UTF-16` after a byte order mark, and the `encoding_rs` decoder of the
/// byte order it sets, big-endian without one.
fn utf16_bom(bytes: &[u8]) -> (&[u8], &'static Encoding) {
    match bytes {
        [0xFE, 0xFF, rest @ ..] => (rest, encoding_rs::UTF_16BE),
        [0xFF, 0xFE, rest @ ..] => (rest, encoding_rs::UTF_16LE),
        _ => (bytes, encoding_rs::UTF_16BE),
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
            let text = charset.decode(&input).expect("the charset decodes");
            text.encode_utf16().for_each(&mut add);
            add(ERROR);
            if !matches!(charset.decoding(), Decoding::MultiByte(_)) {
                continue;
            }
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

    /// The escapes and shifts `ISO_2022_TOKENS` in `CharsetTables.java` draws from.
    const ISO_2022_TOKENS: [&[u8]; 16] = [
        b"\x1b$)C", b"\x1b$)A", b"\x1b$)G", b"\x1b$)X", b"\x1b$A", b"\x1b$*H", b"\x1b$+I",
        b"\x1b$*X", b"\x1bN", b"\x1bO", b"\x1b", b"\x1b$", b"\x1b$)", b"\x0e", b"\x0f", b"\n",
    ];
    const ISO_2022_INPUTS: usize = 20000;
    const ISO_2022_MAX_TOKENS: u64 = 24;

    /// The inputs `DecoderReference.iso2022Hash` draws.
    fn iso2022_inputs() -> impl Iterator<Item = Vec<u8>> {
        let mut random = SplitMix64(0x2022);
        (0..ISO_2022_INPUTS).map(move |_| {
            let tokens = 1 + random.next() % ISO_2022_MAX_TOKENS;
            let mut input = Vec::new();
            for _ in 0..tokens {
                let r = random.next();
                let rest = r >> 8;
                match (r % 32) as usize {
                    kind if kind < ISO_2022_TOKENS.len() => {
                        input.extend_from_slice(ISO_2022_TOKENS[kind])
                    }
                    kind if kind < 30 => input.push(0x21 + (rest % 94) as u8),
                    30 => input.push(0x80 | (rest & 0x7F) as u8),
                    _ => input.push((rest & 0x7F) as u8),
                }
            }
            input
        })
    }

    /// The hash `DecoderReference.iso2022Hash` takes of the same inputs.
    fn iso2022_hash(charset: Charset) -> u64 {
        let mut hash = FNV_OFFSET;
        let mut add = |unit: u16| hash = (hash ^ u64::from(unit)).wrapping_mul(FNV_PRIME);
        for input in iso2022_inputs() {
            let text = charset.decode(&input).expect("the charset decodes");
            text.encode_utf16().for_each(&mut add);
            add(ERROR);
        }
        hash
    }

    #[test]
    fn decodes_iso2022_as_the_jdk_does_on_known_text() {
        let kr = Charset::named("ISO-2022-KR");
        assert_eq!(
            kr.decode(b"\x1b$)C\x0e\x30\x21\x0f hello\n").as_deref(),
            Some("\u{AC00} hello\n")
        );
        let cn = Charset::named("ISO-2022-CN");
        assert_eq!(
            cn.decode(b"\x1b$)A\x0e\x30\x21\x0f hi \x1b$)G\x0e\x44\x21\x0f")
                .as_deref(),
            Some("\u{554A} hi \u{4E00}")
        );
        assert_eq!(kr.decode(b"ok\x1b$)").as_deref(), Some("ok\u{FFFD}"));
    }

    /// UTF-8 replaces each malformed sequence as WHATWG delimits it: an encoded surrogate is
    /// three, where the JDK's decoder makes it one U+FFFD (#186).
    #[test]
    fn replaces_malformed_utf8_as_whatwg_does() {
        let utf8 = Charset::named("UTF-8");
        assert_eq!(utf8.decode("añ€😀".as_bytes()).as_deref(), Some("añ€😀"));
        assert_eq!(
            utf8.decode(b"a\xED\xA0\x80b").as_deref(),
            Some("a\u{FFFD}\u{FFFD}\u{FFFD}b")
        );
        assert_eq!(utf8.decode(b"a\xE2\x82").as_deref(), Some("a\u{FFFD}"));
    }

    /// UTF-16 replaces an unpaired high surrogate alone and keeps what follows it, where the
    /// JDK's decoder replaces it together with the next unit (#186); `UTF-16` reads a byte order
    /// mark to set the byte order and drops it.
    #[test]
    fn replaces_malformed_utf16_as_whatwg_does() {
        let be = Charset::named("UTF-16BE");
        assert_eq!(be.decode(b"\x00a\xD8\x3D\xDE\x00").as_deref(), Some("a😀"));
        assert_eq!(be.decode(b"\xD8\x00\x00A").as_deref(), Some("\u{FFFD}A"));
        let le = Charset::named("UTF-16LE");
        assert_eq!(
            le.decode(b"\x00\xDCa\x00b").as_deref(),
            Some("\u{FFFD}a\u{FFFD}")
        );
        let sniffing = Charset::named("UTF-16");
        assert_eq!(sniffing.decode(b"\xFF\xFEa\x00").as_deref(), Some("a"));
        assert_eq!(sniffing.decode(b"\x00a").as_deref(), Some("a"));
        assert!(sniffing.pieces(b"\xFE\xFF\x00a", 4).unwrap().dropped_bom());
        assert!(!be.pieces(b"\xFE\xFF\x00a", 4).unwrap().dropped_bom());
    }

    #[test]
    fn decodes_in_pieces_as_whole() {
        let mut random = SplitMix64(0x9133);
        let inputs: Vec<Vec<u8>> = (0..300)
            .map(|_| {
                let length = (random.next() % 64) as usize;
                (0..length)
                    .map(|_| {
                        let r = random.next();
                        match r % 8 {
                            0 => b'\n',
                            1 => 0x1B,
                            2..=4 => (r >> 8) as u8 & 0x7F,
                            _ => (r >> 8) as u8,
                        }
                    })
                    .collect()
            })
            .chain([
                b"\xFE\xFF\x00a\xD8\x3D\xDE\x00\x00b".to_vec(),
                b"\xFF\xFE\x00\x00a\x00\x00\x00".to_vec(),
                b"\x1b$B$3$s$K$A$O\x1b(B world, \x1b$B@$3&\x1b(B".to_vec(),
            ])
            .collect();
        let iso2022: Vec<Vec<u8>> = iso2022_inputs().take(500).collect();
        for entry in &REGISTRY.entries {
            let charset = Charset::named(&entry.name);
            for input in inputs.iter().chain(&iso2022) {
                let Some(whole) = charset.decode(input) else {
                    continue;
                };
                for size in [1, 2, 3, 5, 8] {
                    let pieces: String = charset.pieces(input, size).unwrap().collect();
                    assert_eq!(
                        pieces, whole,
                        "{} in pieces of {size}: {input:?}",
                        entry.name
                    );
                }
            }
        }
    }

    #[test]
    fn decodes_as_the_jdk_does() {
        let mut expected: Vec<&str> = REFERENCE
            .lines()
            .map(|line| line.split('\t').next().unwrap())
            .collect();
        let mut ported: Vec<&str> = REGISTRY
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.decoding,
                    Decoding::MultiByte(_) | Decoding::Iscii91 | Decoding::Iso2022(_)
                )
            })
            .map(|entry| entry.name.as_str())
            .collect();
        expected.sort_unstable();
        ported.sort_unstable();
        assert_eq!(
            expected, ported,
            "the reference covers every multi-byte, ISCII-91 and ISO-2022 charset"
        );
        let mismatches: Vec<String> = REFERENCE
            .lines()
            .filter_map(|line| {
                let (name, reference) = line.split_once('\t')?;
                let charset = Charset::named(name);
                let actual = match charset.decoding() {
                    Decoding::Iso2022(_) => format!("{:x}", iso2022_hash(charset)),
                    _ => format!("{:x}", hash(charset)),
                };
                (actual != reference).then(|| format!("{name}: JDK {reference}, port {actual}"))
            })
            .collect();
        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    }
}
