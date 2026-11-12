//! What a PostScript program's fonts take from outside it: the encodings `systemdict` holds
//! (`StandardEncoding`, `ISOLatin1Encoding`), the fonts a printer has resident, with their
//! built-in encodings and, for the standard 14, their widths, and the text of a glyph name.

use unicode_normalization::UnicodeNormalization;

use crate::formats::pdf::afm::Standard14;
use crate::formats::pdf::encoding::{Builtin, TEXT_EXTRACTION, ZAPF_DINGBATS};

/// The codes of an encoding vector.
pub(super) const CODES: usize = 256;
const NOTDEF: &str = ".notdef";
/// `ISOLatin1Encoding` from 0x90: the accents.
const ISO_LATIN_1_ACCENTS: [&str; 16] = [
    "dotlessi",
    "grave",
    "acute",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "dieresis",
    NOTDEF,
    "ring",
    "cedilla",
    NOTDEF,
    "hungarumlaut",
    "ogonek",
    "caron",
];

/// An encoding vector a program finds without defining it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Vector {
    Standard,
    IsoLatin1,
    Symbol,
    Dingbats,
}

impl Vector {
    /// The glyph name of each code.
    pub(super) fn names(self) -> [&'static str; CODES] {
        let builtin = |builtin: Builtin| -> [&'static str; CODES] {
            std::array::from_fn(|code| builtin.encoding().name(code as i32))
        };
        match self {
            Vector::Standard => builtin(Builtin::Standard),
            Vector::Symbol => builtin(Builtin::Symbol),
            Vector::Dingbats => builtin(Builtin::ZapfDingbats),
            Vector::IsoLatin1 => {
                let standard = Builtin::Standard.encoding();
                let win_ansi = Builtin::WinAnsi.encoding();
                std::array::from_fn(|code| match code {
                    0x2D => "minus",
                    0x20..=0x2C | 0x2E..=0x7E => standard.name(code as i32),
                    0x90..=0x9F => ISO_LATIN_1_ACCENTS[code - 0x90],
                    0xA0 => "space",
                    0xAD => "hyphen",
                    0xA1..=0xAC | 0xAE..=0xFF => win_ansi.name(code as i32),
                    _ => NOTDEF,
                })
            }
        }
    }
}

/// A font the program finds without defining it, as a printer has it resident or substitutes
/// for it: its built-in encoding, and its metrics where it is one of the standard 14.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Resident {
    pub(super) vector: Vector,
    pub(super) metrics: Option<Standard14>,
}

impl Resident {
    pub(super) fn named(name: &str) -> Resident {
        let metrics = Standard14::named(name);
        let vector = match metrics {
            Some(Standard14::Symbol) => Vector::Symbol,
            Some(Standard14::ZapfDingbats) => Vector::Dingbats,
            _ => Vector::Standard,
        };
        Resident { vector, metrics }
    }
}

/// Appends to `text` what a glyph name stands for, if anything: by the Adobe Glyph List and its
/// `uniXXXX` and `uXXXX` forms, ZapfDingbats' names first in that font, and a name of one
/// printable ASCII character as that character, as fonts built from character codes name their
/// glyphs. A presentation form (a ligature) is decomposed. `.notdef` and an unknown name stand
/// for nothing.
pub(super) fn glyph_text(name: &[u8], dingbats: bool, text: &mut String) -> bool {
    let Ok(name) = std::str::from_utf8(name) else {
        return false;
    };
    if name == NOTDEF {
        return false;
    }
    let listed = dingbats
        .then(|| ZAPF_DINGBATS.to_unicode(name))
        .flatten()
        .or_else(|| TEXT_EXTRACTION.to_unicode(name));
    if let Some(listed) = listed {
        for c in listed.chars() {
            if is_presentation_form(c) {
                text.extend(c.nfkc());
            } else {
                text.push(c);
            }
        }
        return true;
    }
    match name.as_bytes() {
        [byte @ b'!'..=b'~'] => {
            text.push(char::from(*byte));
            true
        }
        _ => false,
    }
}

/// Whether `c` is one of the presentation forms text extraction decomposes.
fn is_presentation_form(c: char) -> bool {
    matches!(u32::from(c), 0xFB00..=0xFDFF | 0xFE70..=0xFEFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_iso_latin_1_vector() {
        let names = Vector::IsoLatin1.names();
        assert_eq!(names[0x41], "A");
        assert_eq!(names[0x27], "quoteright");
        assert_eq!(names[0x2D], "minus");
        assert_eq!(names[0x92], "acute");
        assert_eq!(names[0xE5], "aring");
        assert_eq!(names[0xAD], "hyphen");
        assert_eq!(names[0xA0], "space");
        assert_eq!(names[0x80], NOTDEF);
        assert_eq!(Vector::Standard.names()[0xAE], "fi");
    }

    #[test]
    fn reads_glyph_names() {
        let mut text = String::new();
        for name in [
            &b"T"[..],
            b"aring",
            b"uni00E6",
            b"fi",
            b"seven",
            b"1",
            b"g12",
            b".notdef",
        ] {
            glyph_text(name, false, &mut text);
        }
        assert_eq!(text, "Tåæfi71");
        let mut dingbat = String::new();
        assert!(glyph_text(b"a1", true, &mut dingbat));
        assert_eq!(dingbat, "\u{2701}");
    }

    #[test]
    fn knows_the_resident_fonts() {
        assert_eq!(
            Resident::named("Times-Roman"),
            Resident {
                vector: Vector::Standard,
                metrics: Some(Standard14::TimesRoman)
            }
        );
        assert_eq!(Resident::named("Symbol").vector, Vector::Symbol);
        assert_eq!(
            Resident::named("Palatino-Roman"),
            Resident {
                vector: Vector::Standard,
                metrics: None
            }
        );
    }
}
