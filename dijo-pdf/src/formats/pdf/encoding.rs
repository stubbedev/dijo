//! PDFBox's simple font encodings and glyph lists: the built-in encodings as PDFBox holds them
//! (`models/pdf/encodings.tsv`, written by `just pdf-resources`), encodings with differences,
//! and the Adobe Glyph List with the names it maps to Unicode.

use std::sync::LazyLock;

use lopdf::{Dictionary, Object};

use super::cos::{name_string, Pdf};
use super::position::GlyphText;
use crate::hash::FastMap;

const ENCODINGS: &str = include_str!("../../../models/pdf/encodings.tsv");
const GLYPH_LIST: &str = include_str!("../../../models/pdf/glyphlist.txt");
const ADDITIONAL_GLYPHS: &str = include_str!("../../../models/pdf/additional.txt");
const ZAPF_DINGBATS_GLYPHS: &str = include_str!("../../../models/pdf/zapfdingbats.txt");

/// The encodings PDFBox has built in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Builtin {
    Standard,
    WinAnsi,
    MacRoman,
    MacOsRoman,
    MacExpert,
    Symbol,
    ZapfDingbats,
}

impl Builtin {
    const ALL: [Builtin; 7] = [
        Builtin::Standard,
        Builtin::WinAnsi,
        Builtin::MacRoman,
        Builtin::MacOsRoman,
        Builtin::MacExpert,
        Builtin::Symbol,
        Builtin::ZapfDingbats,
    ];

    fn tsv_name(self) -> &'static str {
        match self {
            Builtin::Standard => "Standard",
            Builtin::WinAnsi => "WinAnsi",
            Builtin::MacRoman => "MacRoman",
            Builtin::MacOsRoman => "MacOSRoman",
            Builtin::MacExpert => "MacExpert",
            Builtin::Symbol => "Symbol",
            Builtin::ZapfDingbats => "ZapfDingbats",
        }
    }

    /// `Encoding.getInstance`: the encodings a font's `/Encoding` name selects.
    pub(super) fn named(name: &[u8]) -> Option<Builtin> {
        match name {
            b"StandardEncoding" => Some(Builtin::Standard),
            b"WinAnsiEncoding" => Some(Builtin::WinAnsi),
            b"MacRomanEncoding" => Some(Builtin::MacRoman),
            b"MacExpertEncoding" => Some(Builtin::MacExpert),
            _ => None,
        }
    }

    /// The encoding itself.
    pub(super) fn encoding(self) -> &'static Encoding {
        &BUILTIN[&self]
    }
}

/// What an encoding is, as PDFBox's `instanceof` tests tell encodings apart.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Kind {
    Builtin(Builtin),
    /// `DictionaryEncoding`: a base encoding, if any, with differences.
    Dictionary {
        base: Option<Box<Encoding>>,
        differences: FastMap<i32, String>,
    },
    /// `BuiltInEncoding` or `Type1Encoding`: the encoding a font program has built in.
    FontProgram,
}

/// A simple font's encoding: its codes to glyph names, and names back to codes.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Encoding {
    pub(super) kind: Kind,
    code_to_name: FastMap<i32, String>,
    name_to_code: FastMap<String, i32>,
}

static BUILTIN: LazyLock<FastMap<Builtin, Encoding>> = LazyLock::new(|| {
    let mut encodings: FastMap<Builtin, Encoding> = Builtin::ALL
        .iter()
        .map(|&builtin| {
            (
                builtin,
                Encoding {
                    kind: Kind::Builtin(builtin),
                    code_to_name: FastMap::default(),
                    name_to_code: FastMap::default(),
                },
            )
        })
        .collect();
    for line in ENCODINGS.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('\t').collect();
        let [encoding, map, key, value] = fields[..] else {
            panic!("models/pdf/encodings.tsv has a malformed line: {line}");
        };
        let builtin = *Builtin::ALL
            .iter()
            .find(|builtin| builtin.tsv_name() == encoding)
            .unwrap_or_else(|| panic!("models/pdf/encodings.tsv names no encoding {encoding}"));
        let target = encodings
            .get_mut(&builtin)
            .expect("every builtin is present");
        let number = |text: &str| {
            text.parse::<i32>()
                .unwrap_or_else(|e| panic!("models/pdf/encodings.tsv has a bad code {text}: {e}"))
        };
        match map {
            "code" => {
                target.code_to_name.insert(number(key), value.to_owned());
            }
            "name" => {
                target.name_to_code.insert(key.to_owned(), number(value));
            }
            other => panic!("models/pdf/encodings.tsv has an unknown map {other}"),
        }
    }
    encodings
});

impl Encoding {
    /// `Encoding.getName`: the glyph name of a code, `.notdef` for one it does not encode.
    pub(super) fn name(&self, code: i32) -> &str {
        self.code_to_name
            .get(&code)
            .map_or(".notdef", String::as_str)
    }

    /// `Encoding.contains(String)`.
    pub(super) fn contains(&self, name: &str) -> bool {
        self.name_to_code.contains_key(name)
    }

    /// The code `getNameToCodeMap` gives a name.
    pub(super) fn code(&self, name: &str) -> Option<i32> {
        self.name_to_code.get(name).copied()
    }

    /// Whether this is one of the encodings `isFontSymbolic` takes for non-symbolic:
    /// `WinAnsiEncoding`, `MacRomanEncoding` (or its subclass) or `StandardEncoding`.
    pub(super) fn is_latin(&self) -> bool {
        matches!(
            self.kind,
            Kind::Builtin(
                Builtin::WinAnsi | Builtin::MacRoman | Builtin::MacOsRoman | Builtin::Standard
            )
        )
    }

    /// A font program's built-in encoding from its codes and names, as `BuiltInEncoding` and
    /// `Type1Encoding` add them: in the font's order, a name keeping the first code it is added
    /// with. PDFBox adds them in the iteration order of a Java `HashMap`, so that which code a
    /// name keeps depends on the hash table's layout (#186).
    pub(super) fn font_program(entries: &[(i32, String)]) -> Encoding {
        Self::in_order(entries)
    }

    /// `Type1Encoding.fromFontBox` of a FontBox encoding filled with `entries`: in their order,
    /// where PDFBox copies them in the iteration order of FontBox's `HashMap<Integer, String>`
    /// (#186).
    pub(super) fn from_font_box(entries: &[(i32, String)]) -> Encoding {
        Self::in_order(entries)
    }

    /// The code `PDType1Font.getInvertedEncoding` gives a name: the lowest code that names it,
    /// and -1, which an AFM file gives the glyphs it does not encode, only where no code does.
    /// PDFBox takes the first in the iteration order of a Java `HashMap` instead (#186).
    pub(super) fn first_code(&self, name: &str) -> Option<i32> {
        self.code_to_name
            .iter()
            .filter(|(_, named)| *named == name)
            .map(|(&code, _)| code)
            .min_by_key(|&code| (code < 0, code))
    }

    /// The codes and names of the encoding, by code.
    pub(super) fn entries(&self) -> Vec<(i32, String)> {
        let mut entries: Vec<(i32, String)> = self
            .code_to_name
            .iter()
            .map(|(&code, name)| (code, name.clone()))
            .collect();
        entries.sort_by_key(|(code, _)| *code);
        entries
    }

    /// A `Type1Encoding` from metrics, in their order.
    pub(super) fn in_order(entries: &[(i32, String)]) -> Encoding {
        let mut encoding = Encoding {
            kind: Kind::FontProgram,
            code_to_name: FastMap::default(),
            name_to_code: FastMap::default(),
        };
        for (code, name) in entries {
            encoding.add(*code, name.clone());
        }
        encoding
    }

    /// `Encoding.add`.
    fn add(&mut self, code: i32, name: String) {
        self.name_to_code.entry(name.clone()).or_insert(code);
        self.code_to_name.insert(code, name);
    }

    /// `Encoding.overwrite`.
    fn overwrite(&mut self, code: i32, name: String) {
        if let Some(old) = self.code_to_name.get(&code) {
            if self.name_to_code.get(old) == Some(&code) {
                let old = old.clone();
                self.name_to_code.remove(&old);
            }
        }
        self.name_to_code.insert(name.clone(), code);
        self.code_to_name.insert(code, name);
    }

    /// `new DictionaryEncoding(COSDictionary)`, the encoding of a Type 3 font: a valid base
    /// encoding, if named, with the differences.
    pub(super) fn type3_dictionary(pdf: &Pdf, dict: &Dictionary) -> Encoding {
        let base = pdf
            .name(dict, b"BaseEncoding")
            .and_then(Builtin::named)
            .map(Builtin::encoding);
        Self::with_differences(pdf, dict, base)
    }

    /// `new DictionaryEncoding(COSDictionary, isNonSymbolic, builtIn)`: a valid base encoding
    /// if named, else Standard for a non-symbolic font or the font's own; `None` where PDFBox
    /// throws because a symbolic font has no built-in encoding.
    pub(super) fn simple_dictionary(
        pdf: &Pdf,
        dict: &Dictionary,
        non_symbolic: bool,
        built_in: Option<&Encoding>,
    ) -> Option<Encoding> {
        let named = if dict.has(b"BaseEncoding") {
            pdf.name(dict, b"BaseEncoding")
                .and_then(Builtin::named)
                .map(Builtin::encoding)
        } else {
            None
        };
        let base = match named {
            Some(base) => base,
            None if non_symbolic => Builtin::Standard.encoding(),
            None => built_in?,
        };
        Some(Self::with_differences(pdf, dict, Some(base)))
    }

    fn with_differences(pdf: &Pdf, dict: &Dictionary, base: Option<&Encoding>) -> Encoding {
        let mut encoding = Encoding {
            kind: Kind::Dictionary {
                base: base.map(|base| Box::new(base.clone())),
                differences: FastMap::default(),
            },
            code_to_name: base.map(|b| b.code_to_name.clone()).unwrap_or_default(),
            name_to_code: base.map(|b| b.name_to_code.clone()).unwrap_or_default(),
        };
        let Some(array) = pdf.array(dict, b"Differences") else {
            return encoding;
        };
        let mut differences = FastMap::default();
        let mut index: i32 = -1;
        for item in array {
            match pdf.resolve(item) {
                Some(Object::Integer(value)) => index = *value as i32,
                Some(Object::Real(value)) => index = *value as i32,
                Some(Object::Name(name)) => {
                    let name = name_string(name);
                    encoding.overwrite(index, name.clone());
                    differences.insert(index, name);
                    index = index.wrapping_add(1);
                }
                _ => {}
            }
        }
        if let Kind::Dictionary {
            differences: target,
            ..
        } = &mut encoding.kind
        {
            *target = differences;
        }
        encoding
    }
}

/// PDFBox's `GlyphList`: glyph names to Unicode, and Unicode back to names.
#[derive(Debug)]
pub(super) struct GlyphList {
    name_to_unicode: FastMap<String, String>,
    unicode_to_name: FastMap<String, String>,
}

/// `GlyphList.getAdobeGlyphList`.
pub(super) static ADOBE: LazyLock<GlyphList> = LazyLock::new(|| {
    let mut list = GlyphList {
        name_to_unicode: FastMap::with_capacity_and_hasher(4281, Default::default()),
        unicode_to_name: FastMap::with_capacity_and_hasher(4281, Default::default()),
    };
    list.load(GLYPH_LIST);
    list
});

/// `GlyphList.getZapfDingbats`.
pub(super) static ZAPF_DINGBATS: LazyLock<GlyphList> = LazyLock::new(|| {
    let mut list = GlyphList {
        name_to_unicode: FastMap::default(),
        unicode_to_name: FastMap::default(),
    };
    list.load(ZAPF_DINGBATS_GLYPHS);
    list
});

/// The glyph list `LegacyPDFStreamEngine` maps text with: the Adobe Glyph List and PDFBox's
/// additional names.
pub(super) static TEXT_EXTRACTION: LazyLock<GlyphList> = LazyLock::new(|| {
    let mut list = GlyphList {
        name_to_unicode: ADOBE.name_to_unicode.clone(),
        unicode_to_name: ADOBE.unicode_to_name.clone(),
    };
    list.load(ADDITIONAL_GLYPHS);
    list
});

impl GlyphList {
    /// `GlyphList.loadList`.
    fn load(&mut self, text: &str) {
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let mut parts = line.split(';');
            let (Some(name), Some(codes)) = (parts.next(), parts.next()) else {
                panic!("a PDFBox glyph list has a malformed entry: {line}");
            };
            let unicode: String = codes
                .split(' ')
                .map(|hex| {
                    u32::from_str_radix(hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .unwrap_or_else(|| panic!("a PDFBox glyph list has a bad code {hex}"))
                })
                .collect();
            self.name_to_unicode
                .insert(name.to_owned(), unicode.clone());
            let canonical = [
                Builtin::WinAnsi,
                Builtin::MacRoman,
                Builtin::MacExpert,
                Builtin::Symbol,
                Builtin::ZapfDingbats,
            ]
            .iter()
            .any(|builtin| builtin.encoding().contains(name));
            if canonical {
                self.unicode_to_name.insert(unicode, name.to_owned());
            } else {
                self.unicode_to_name
                    .entry(unicode)
                    .or_insert_with(|| name.to_owned());
            }
        }
    }

    /// `GlyphList.codePointToName`.
    pub(super) fn code_point_to_name(&self, code_point: char) -> &str {
        self.unicode_to_name
            .get(code_point.encode_utf8(&mut [0; 4]) as &str)
            .map_or(".notdef", String::as_str)
    }

    /// `GlyphList.toUnicode`: the name's text, without a `.suffix`, or of a `uniXXXX` or
    /// `uXXXX` name.
    pub(super) fn to_unicode(&self, name: &str) -> Option<GlyphText> {
        if let Some(unicode) = self.name_to_unicode.get(name) {
            return Some(GlyphText::new(unicode));
        }
        if let Some(dot) = name.find('.').filter(|&dot| dot > 0) {
            return self.to_unicode(&name[..dot]);
        }
        let hex = if name.len() == 7 && name.starts_with("uni") {
            &name[3..]
        } else if name.len() == 5 && name.starts_with('u') {
            &name[1..]
        } else {
            return None;
        };
        let code = crate::java::parse_hex_int(hex)?;
        let code = code as u32 & 0xFFFF;
        if (0xD800..0xE000).contains(&code) {
            return None;
        }
        char::from_u32(code).map(GlyphText::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_builtin_encodings() {
        let win_ansi = Builtin::WinAnsi.encoding();
        assert_eq!(win_ansi.name(65), "A");
        assert_eq!(win_ansi.name(0x81), "bullet");
        assert_eq!(win_ansi.name(10), ".notdef");
        assert_eq!(win_ansi.code("space"), Some(32));
        assert!(win_ansi.is_latin());
        assert_eq!(Builtin::Standard.encoding().name(0xAE), "fi");
        assert!(!Builtin::Symbol.encoding().is_latin());
    }

    #[test]
    fn maps_glyph_names_as_pdfbox_does() {
        assert_eq!(ADOBE.to_unicode("A").as_deref(), Some("A"));
        assert_eq!(ADOBE.to_unicode("A.sc").as_deref(), Some("A"));
        assert_eq!(ADOBE.to_unicode("uni00E6").as_deref(), Some("æ"));
        assert_eq!(ADOBE.to_unicode("u00E6").as_deref(), Some("æ"));
        assert_eq!(ADOBE.to_unicode("uniD800"), None);
        assert_eq!(ADOBE.to_unicode("nosuchglyph"), None);
        assert_eq!(ADOBE.code_point_to_name(' '), "space");
        assert_eq!(ADOBE.code_point_to_name('~'), "asciitilde");
        assert!(TEXT_EXTRACTION.to_unicode("angbracketleft").is_some());
    }

    /// A font program's encoding gives a name its first code in the font's order, where
    /// PDFBox's hash order would give "a" code 3 (#186).
    #[test]
    fn gives_a_name_its_first_code_in_the_fonts_order() {
        let entries = [
            (40, "a".to_owned()),
            (3, "a".to_owned()),
            (17, "b".to_owned()),
        ];
        let encoding = Encoding::font_program(&entries);
        assert_eq!(encoding.code("a"), Some(40));
        assert_eq!(encoding.name(3), "a");
    }

    /// The inverse takes the lowest code of a name, past a byte too, and -1 only where no other
    /// code names it, where PDFBox's hash order gave up on codes past a byte (#186).
    #[test]
    fn inverts_a_name_to_its_lowest_code() {
        let entries = [
            (-1, "a".to_owned()),
            (300, "a".to_owned()),
            (7, "b".to_owned()),
        ];
        let encoding = Encoding::in_order(&entries);
        assert_eq!(encoding.first_code("a"), Some(300));
        assert_eq!(encoding.first_code("b"), Some(7));
        assert_eq!(encoding.first_code("c"), None);
        let unencoded = Encoding::in_order(&[(-1, "a".to_owned())]);
        assert_eq!(unencoded.first_code("a"), Some(-1));
    }
}
