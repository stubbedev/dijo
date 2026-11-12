//! The Lotus Multi-Byte Character Set, in which 1-2-3 keeps its text, as libwps and ICU read it.
//!
//! A byte from 0x20 to 0x7F is ASCII, and a tab, line feed or carriage return is itself. A byte
//! from 0x01 to 0x0F names the single-byte group of the one byte after it, and one from 0x10 to
//! 0x1F the double-byte group of the two bytes after it; any other byte of 0x80 or above is a
//! character of the file's default group, two bytes long where that group is a double-byte one.
//! Group 1 is code page 850, and its bytes below 0x80 the typographic and box-drawing
//! characters LMBCS adds; group 2 is Greek (code page 851, which the JDK lacks, as a table of
//! its own); group 6 is code page 852, its bytes below 0x80 the Baltic and Esperanto letters LMBCS
//! adds; groups 3, 4, 5, 8 and 11 are the Hebrew, Arabic, Cyrillic, Turkish and Thai code pages,
//! of which only the bytes from 0x80 are characters; groups 0x10 to 0x13 are the Japanese,
//! Korean, traditional and simplified Chinese double-byte code pages, and group 0x14 is UTF-16
//! big-endian. Group 14 holds a style change and group 15 a control character, neither of
//! them text. A byte LMBCS gives no character becomes U+FFFD.

use std::sync::LazyLock;

use crate::text::charset::Charset;

/// The group 1-2-3 Release 2 keeps its text in, and the default of a workbook's.
pub(super) const LATIN: u8 = 0x01;
const GREEK: u8 = 0x02;
const HEBREW: u8 = 0x03;
const ARABIC: u8 = 0x04;
const CYRILLIC: u8 = 0x05;
const CENTRAL_EUROPEAN: u8 = 0x06;
const TURKISH: u8 = 0x08;
const THAI: u8 = 0x0B;
const STYLE: u8 = 0x0E;
const CONTROL: u8 = 0x0F;
const JAPANESE: u8 = 0x10;
const KOREAN: u8 = 0x11;
const TRADITIONAL_CHINESE: u8 = 0x12;
const SIMPLIFIED_CHINESE: u8 = 0x13;
const UNICODE: u8 = 0x14;

/// The characters of group 1's bytes below 0x80, 0 where there is none. 0x0A and 0x0D are the
/// line breaks they are in the files libwps reads, not the glyphs of code page 850.
const LATIN_LOW: [u16; 128] = [
    0x0000, 0x263A, 0x263B, 0x2665, 0x2666, 0x2663, 0x2660, 0x2022, 0x25D8, 0x25CB, 0x000A, 0x2642,
    0x2640, 0x000D, 0x266B, 0x263C, 0x25BA, 0x25C4, 0x2195, 0x203C, 0x00B6, 0x00A7, 0x25AC, 0x21A8,
    0x2191, 0x2193, 0x2192, 0x2190, 0x221F, 0x2194, 0x25B2, 0x25BC, 0x00A8, 0x007E, 0x02DA, 0x005E,
    0x0060, 0x00B4, 0x201C, 0x0027, 0x2026, 0x2013, 0x2014, 0x2018, 0x2019, 0x0000, 0x2039, 0x203A,
    0x00A8, 0x007E, 0x02DA, 0x005E, 0x0060, 0x00B4, 0x201E, 0x201A, 0x201D, 0x2017, 0x0000, 0x00A0,
    0x0000, 0x0000, 0x0000, 0x0000, 0x0152, 0x0153, 0x0178, 0x02D9, 0x02DA, 0x0000, 0x255E, 0x255F,
    0x258C, 0x2590, 0x25CA, 0x2318, 0xF8FF, 0xF8FE, 0x2126, 0x0000, 0x2568, 0x2564, 0x2565, 0x2559,
    0x2558, 0x2552, 0x2553, 0x256B, 0x256A, 0x2561, 0x2562, 0x2556, 0x2555, 0x255C, 0x255B, 0x2567,
    0x0133, 0x0132, 0xFB01, 0xFB02, 0x0149, 0x0140, 0x013F, 0x00AF, 0x02D8, 0x02DD, 0x02DB, 0x02C7,
    0x007E, 0x005E, 0x0000, 0x0000, 0x2020, 0x2021, 0x0126, 0x0127, 0x0166, 0x0167, 0x2122, 0x2113,
    0x014A, 0x014B, 0x0138, 0x0000, 0xF8FB, 0x2310, 0x20A4, 0x20A7,
];

/// The characters of group 2, 0 where there is none.
const GREEK_ALL: [u16; 256] = [
    0x0000, 0x037A, 0x0385, 0x03AA, 0x03AB, 0x2015, 0x0384, 0x02BC, 0x02BD, 0x203E, 0x0000, 0x0000,
    0x0000, 0x0000, 0xF862, 0xF863, 0xF864, 0xF865, 0x21D5, 0x215E, 0x215D, 0x215C, 0x215B, 0xF867,
    0x21D1, 0x21D3, 0x21D2, 0x21D0, 0xF868, 0x21D4, 0xF869, 0xF89F, 0xF89E, 0xF89D, 0xF89C, 0xF89B,
    0xF89A, 0xF899, 0xF898, 0xF897, 0xF896, 0xF895, 0xF894, 0xF893, 0xF892, 0xF891, 0xF890, 0xF88F,
    0xF88E, 0xF88D, 0xF88C, 0xF88B, 0xF88A, 0xF889, 0xF888, 0xF887, 0xF886, 0xF885, 0xF884, 0xF883,
    0xF882, 0xF881, 0xF880, 0xF866, 0x2220, 0x2207, 0xF87F, 0xF87E, 0xF87D, 0xF87C, 0xF87B, 0xF87A,
    0xF879, 0xF878, 0xF877, 0xF876, 0xF875, 0xF874, 0xF873, 0xF872, 0x2202, 0x2135, 0x2111, 0x211C,
    0xF871, 0xF870, 0xF86F, 0xF86E, 0xF86D, 0xF86C, 0xF86B, 0x220B, 0x2208, 0x2209, 0x2286, 0x2287,
    0x2297, 0x2295, 0x2713, 0x22C0, 0x2201, 0x222B, 0x2200, 0x2203, 0xF86A, 0x2032, 0x2033, 0x221E,
    0x221D, 0x03C6, 0x222A, 0x2229, 0x2261, 0x2245, 0x2265, 0x2264, 0x2320, 0x2321, 0x2260, 0x2248,
    0x2044, 0x2219, 0x2030, 0x221A, 0x207F, 0x2205, 0x2282, 0x2283, 0x00C7, 0x00FC, 0x00E9, 0x00E2,
    0x00E4, 0x00E0, 0x0386, 0x00E7, 0x00EA, 0x00EB, 0x00E8, 0x00EF, 0x00EE, 0x0388, 0x00C4, 0x0389,
    0x038A, 0x0000, 0x038C, 0x00F4, 0x00F6, 0x038E, 0x00FB, 0x00F9, 0x038F, 0x00D6, 0x00DC, 0x03AC,
    0x00A3, 0x03AD, 0x03AE, 0x03AF, 0x03CA, 0x0390, 0x03CC, 0x03CD, 0x0391, 0x0392, 0x0393, 0x0394,
    0x0395, 0x0396, 0x0397, 0x00BD, 0x0398, 0x0399, 0x00AB, 0x00BB, 0x2591, 0x2592, 0x2593, 0x2502,
    0x2524, 0x039A, 0x039B, 0x039C, 0x039D, 0x2563, 0x2551, 0x2557, 0x255D, 0x039E, 0x039F, 0x2510,
    0x2514, 0x2534, 0x252C, 0x251C, 0x2500, 0x253C, 0x03A0, 0x03A1, 0x255A, 0x2554, 0x2569, 0x2566,
    0x2560, 0x2550, 0x256C, 0x03A3, 0x03A4, 0x03A5, 0x03A6, 0x03A7, 0x03A8, 0x03A9, 0x03B1, 0x03B2,
    0x03B3, 0x2518, 0x250C, 0x2588, 0x2584, 0x03B4, 0x03B5, 0x2580, 0x03B6, 0x03B7, 0x03B8, 0x03B9,
    0x03BA, 0x03BB, 0x03BC, 0x03BD, 0x03BE, 0x03BF, 0x03C0, 0x03C1, 0x03C3, 0x03C2, 0x03C4, 0x00B4,
    0x00AD, 0x00B1, 0x03C5, 0x03C6, 0x03C7, 0x00A7, 0x03C8, 0x00B8, 0x00B0, 0x00A8, 0x03C9, 0x03CB,
    0x03B0, 0x03CE, 0x25A0, 0x00A0,
];

/// The characters of group 6's bytes below 0x80, 0 where there is none.
const CENTRAL_EUROPEAN_LOW: [u16; 47] = [
    0x0000, 0x0101, 0x0108, 0x0109, 0x010A, 0x010B, 0x0112, 0x0113, 0x0116, 0x0117, 0x011C, 0x011D,
    0x0120, 0x0121, 0x0122, 0x0123, 0x0124, 0x0125, 0x0128, 0x0129, 0x012A, 0x012B, 0x012E, 0x012F,
    0x0134, 0x0135, 0x0136, 0x0137, 0x013B, 0x013C, 0x0145, 0x0146, 0x014C, 0x014D, 0x0156, 0x0157,
    0x015C, 0x015D, 0x0168, 0x0169, 0x016A, 0x016B, 0x016C, 0x016D, 0x0172, 0x0173, 0x0100,
];

/// The characters of the bytes from 0x80 of the single-byte groups that are code pages.
struct HighHalves {
    latin: [char; 128],
    hebrew: [char; 128],
    arabic: [char; 128],
    cyrillic: [char; 128],
    central_european: [char; 128],
    turkish: [char; 128],
    thai: [char; 128],
}

static HIGH_HALVES: LazyLock<HighHalves> = LazyLock::new(|| HighHalves {
    latin: high_half("IBM850"),
    hebrew: high_half("windows-1255"),
    arabic: high_half("windows-1256"),
    cyrillic: high_half("windows-1251"),
    central_european: high_half("IBM852"),
    turkish: high_half("windows-1254"),
    thai: high_half("x-IBM874"),
});

/// The characters of the bytes from 0x80 of the JDK's single-byte charset `name`.
fn high_half(name: &str) -> [char; 128] {
    let bytes: Vec<u8> = (0x80..=0xFF).collect();
    let text = Charset::named(name)
        .decode(&bytes)
        .unwrap_or_else(|| panic!("the port decodes {name}"));
    let mut half = ['\u{FFFD}'; 128];
    let mut chars = text.chars();
    for slot in &mut half {
        *slot = chars
            .next()
            .unwrap_or_else(|| panic!("{name} is a single-byte charset"));
    }
    half
}

/// What a character of a group is.
enum Decoded {
    Char(char),
    /// A style change, which is no text.
    Nothing,
    /// No character.
    Invalid,
}

impl Decoded {
    fn of(code: u16) -> Self {
        match char::from_u32(u32::from(code)) {
            Some(c) if code != 0 => Decoded::Char(c),
            _ => Decoded::Invalid,
        }
    }
}

/// The byte `c` of the single-byte group `group`.
fn single(group: u8, c: u8) -> Decoded {
    let high = |half: &[char; 128]| {
        c.checked_sub(0x80)
            .map_or(Decoded::Invalid, |at| Decoded::Char(half[usize::from(at)]))
    };
    let halves = &*HIGH_HALVES;
    match group {
        LATIN if c < 0x80 => Decoded::of(LATIN_LOW[usize::from(c)]),
        LATIN => high(&halves.latin),
        GREEK => Decoded::of(GREEK_ALL[usize::from(c)]),
        HEBREW => high(&halves.hebrew),
        ARABIC => high(&halves.arabic),
        CYRILLIC => high(&halves.cyrillic),
        CENTRAL_EUROPEAN if c < 0x80 => CENTRAL_EUROPEAN_LOW
            .get(usize::from(c))
            .map_or(Decoded::Invalid, |&code| Decoded::of(code)),
        CENTRAL_EUROPEAN => high(&halves.central_european),
        TURKISH => high(&halves.turkish),
        THAI => high(&halves.thai),
        STYLE => Decoded::Nothing,
        CONTROL => match c {
            0x21..=0x3F => Decoded::Char(char::from(c - 0x20)),
            0x80.. => Decoded::Char(char::from(c)),
            _ => Decoded::Invalid,
        },
        _ => Decoded::Invalid,
    }
}

/// The two bytes `pair` of the double-byte group `group`.
fn double(group: u8, pair: [u8; 2]) -> Decoded {
    let charset = match group {
        JAPANESE => "windows-31j",
        KOREAN => "x-windows-949",
        TRADITIONAL_CHINESE => "x-windows-950",
        SIMPLIFIED_CHINESE => "GBK",
        UNICODE => return Decoded::of(u16::from_be_bytes(pair)),
        _ => return Decoded::Invalid,
    };
    let text = Charset::named(charset)
        .decode(&pair)
        .unwrap_or_else(|| panic!("the port decodes {charset}"));
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c != char::REPLACEMENT_CHARACTER => Decoded::Char(c),
        _ => Decoded::Invalid,
    }
}

/// Whether `group` is a double-byte group.
fn is_double(group: u8) -> bool {
    (0x10..=0x1F).contains(&group)
}

/// The text of `bytes`, up to the first NUL, in a file whose default group is `default`.
pub(super) fn decode(bytes: &[u8], default: u8) -> String {
    let mut text = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&byte, after)) = rest.split_first() {
        rest = after;
        let decoded = match byte {
            0 => break,
            b'\t' | b'\n' | b'\r' | 0x20..=0x7F => Decoded::Char(char::from(byte)),
            0x01..=0x0F => match rest.split_first() {
                Some((&c, after)) => {
                    rest = after;
                    single(byte, c)
                }
                None => Decoded::Invalid,
            },
            0x10..=0x1F => match rest {
                [first, second, after @ ..] => {
                    let pair = [*first, *second];
                    rest = after;
                    double(byte, pair)
                }
                _ => {
                    rest = &[];
                    Decoded::Invalid
                }
            },
            _ if is_double(default) => match rest.split_first() {
                Some((&trail, after)) => {
                    rest = after;
                    double(default, [byte, trail])
                }
                None => Decoded::Invalid,
            },
            _ => single(default, byte),
        };
        match decoded {
            Decoded::Char(c) if is_kept(c) => text.push(c),
            Decoded::Char(_) | Decoded::Nothing => {}
            Decoded::Invalid => text.push(char::REPLACEMENT_CHARACTER),
        }
    }
    text
}

/// Whether `c` is text: no control character but a tab or line break.
fn is_kept(c: char) -> bool {
    !c.is_control() || matches!(c, '\t' | '\n' | '\r')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ascii_and_the_default_groups_code_page() {
        assert_eq!(decode(b"Total\0ignored", LATIN), "Total");
        assert_eq!(
            decode(&[b'K', 0x9B, b'b', 0x86, b'n'], LATIN),
            "K\u{f8}b\u{e5}n"
        );
        assert_eq!(decode(&[0x80, 0xE0], CYRILLIC), "\u{402}\u{430}");
    }

    #[test]
    fn reads_characters_of_named_groups() {
        assert_eq!(
            decode(&[0x01, 0x26, b'q', 0x01, 0x36], LATIN),
            "\u{201c}q\u{201e}"
        );
        assert_eq!(decode(&[0x02, 0xD6, 0x06, 0x01], LATIN), "\u{3b1}\u{101}");
        assert_eq!(decode(&[0x14, 0x20, 0xAC], LATIN), "\u{20ac}");
        assert_eq!(decode(&[0x10, 0x82, 0xA0], LATIN), "\u{3042}");
    }

    #[test]
    fn reads_two_bytes_of_a_double_byte_default_group() {
        assert_eq!(decode(&[0x82, 0xA0, b'!'], JAPANESE), "\u{3042}!");
    }

    #[test]
    fn leaves_out_style_changes_and_controls_and_marks_what_is_no_character() {
        assert_eq!(decode(&[b'a', 0x0E, 0x31, b'b'], LATIN), "ab");
        assert_eq!(decode(&[b'a', 0x0F, 0x29, b'b'], LATIN), "a\tb");
        assert_eq!(decode(&[b'a', 0x0F, 0x22, b'b'], LATIN), "ab");
        assert_eq!(decode(&[0x03, 0x41], LATIN), "\u{fffd}");
        assert_eq!(decode(&[0x07, 0x41, b'x'], LATIN), "\u{fffd}x");
        assert_eq!(decode(&[0x12, 0x41], LATIN), "\u{fffd}");
    }
}
