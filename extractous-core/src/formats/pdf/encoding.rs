//! PDFBox's simple font encodings and glyph lists: the built-in encodings as PDFBox holds them
//! (`models/pdf/encodings.tsv`, written by `just pdf-resources`), encodings with differences,
//! and the Adobe Glyph List with the names it maps to Unicode.

use std::collections::HashMap;
use std::sync::LazyLock;

use lopdf::{Dictionary, Object};

use super::cos::{name_string, Pdf};
use super::Unported;

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
        differences: HashMap<i32, String>,
    },
    /// `BuiltInEncoding` or `Type1Encoding`: the encoding a font program has built in.
    FontProgram,
}

/// A simple font's encoding: its codes to glyph names, and names back to codes.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Encoding {
    pub(super) kind: Kind,
    code_to_name: HashMap<i32, String>,
    name_to_code: HashMap<String, i32>,
}

static BUILTIN: LazyLock<HashMap<Builtin, Encoding>> = LazyLock::new(|| {
    let mut encodings: HashMap<Builtin, Encoding> = Builtin::ALL
        .iter()
        .map(|&builtin| {
            (
                builtin,
                Encoding {
                    kind: Kind::Builtin(builtin),
                    code_to_name: HashMap::new(),
                    name_to_code: HashMap::new(),
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
    /// `Type1Encoding` add them: in the iteration order of the Java `HashMap<Integer, String>`
    /// they come in, a name keeping the first code it is added with.
    pub(super) fn font_program(entries: &[(i32, String)]) -> Encoding {
        let mut encoding = Encoding {
            kind: Kind::FontProgram,
            code_to_name: HashMap::new(),
            name_to_code: HashMap::new(),
        };
        for (code, name) in java_integer_map_order(entries) {
            encoding.add(code, name.clone());
        }
        encoding
    }

    /// `Type1Encoding.fromFontBox` of a FontBox encoding filled with `entries` in their order:
    /// in the iteration order of FontBox's `HashMap<Integer, String>(250)`.
    pub(super) fn from_font_box(entries: &[(i32, String)]) -> Encoding {
        let ordered: Vec<(i32, String)> = java_integer_map_order_from(entries, 256)
            .into_iter()
            .map(|(code, name)| (code, name.clone()))
            .collect();
        Self::in_order(&ordered)
    }

    /// The code `PDType1Font.getInvertedEncoding` gives a name: the first that has it in the
    /// iteration order of the codes, which is ascending for the codes of a byte.
    pub(super) fn first_code(&self, name: &str) -> Result<Option<i32>, Unported> {
        if self
            .code_to_name
            .keys()
            .any(|code| !(0..=255).contains(code))
        {
            return Err(Unported(
                "an encoding with codes past a byte, whose inverse PDFBox takes in hash order"
                    .to_owned(),
            ));
        }
        Ok(self
            .code_to_name
            .iter()
            .filter(|(_, candidate)| *candidate == name)
            .map(|(&code, _)| code)
            .min())
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
            code_to_name: HashMap::new(),
            name_to_code: HashMap::new(),
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
                differences: HashMap::new(),
            },
            code_to_name: base.map(|b| b.code_to_name.clone()).unwrap_or_default(),
            name_to_code: base.map(|b| b.name_to_code.clone()).unwrap_or_default(),
        };
        let Some(array) = pdf.array(dict, b"Differences") else {
            return encoding;
        };
        let mut differences = HashMap::new();
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

/// The entries of a Java `HashMap<Integer, V>` filled in the given order, in the order it
/// iterates them: by bucket, the table grown as `HashMap` grows it, and within a bucket in
/// insertion order.
pub(super) fn java_integer_map_order<V>(entries: &[(i32, V)]) -> Vec<(i32, &V)> {
    java_integer_map_order_from(entries, 16)
}

/// The bucket a key of hash code `hash` falls in, in a Java `HashMap` of `capacity` buckets, a
/// power of two.
pub(super) fn java_bucket(hash: i32, capacity: usize) -> usize {
    let hash = hash as u32;
    ((hash ^ (hash >> 16)) as usize) & (capacity - 1)
}

/// [`java_integer_map_order`] of a map whose table starts at `table` buckets, a power of two.
pub(super) fn java_integer_map_order_from<V>(entries: &[(i32, V)], table: usize) -> Vec<(i32, &V)> {
    let mut unique: Vec<(i32, &V)> = Vec::with_capacity(entries.len());
    let mut positions: HashMap<i32, usize> = HashMap::new();
    for (key, value) in entries {
        match positions.get(key) {
            Some(&position) => unique[position].1 = value,
            None => {
                positions.insert(*key, unique.len());
                unique.push((*key, value));
            }
        }
    }
    let mut capacity: usize = table;
    while unique.len() * 4 > capacity * 3 {
        capacity *= 2;
    }
    let bucket = |key: i32| java_bucket(key, capacity);
    let mut ordered: Vec<(usize, usize, (i32, &V))> = unique
        .into_iter()
        .enumerate()
        .map(|(position, entry)| (bucket(entry.0), position, entry))
        .collect();
    ordered.sort_by_key(|&(bucket, position, _)| (bucket, position));
    ordered.into_iter().map(|(_, _, entry)| entry).collect()
}

/// PDFBox's `GlyphList`: glyph names to Unicode, and Unicode back to names.
#[derive(Debug)]
pub(super) struct GlyphList {
    name_to_unicode: HashMap<String, String>,
    unicode_to_name: HashMap<String, String>,
}

/// `GlyphList.getAdobeGlyphList`.
pub(super) static ADOBE: LazyLock<GlyphList> = LazyLock::new(|| {
    let mut list = GlyphList {
        name_to_unicode: HashMap::with_capacity(4281),
        unicode_to_name: HashMap::with_capacity(4281),
    };
    list.load(GLYPH_LIST);
    list
});

/// `GlyphList.getZapfDingbats`.
pub(super) static ZAPF_DINGBATS: LazyLock<GlyphList> = LazyLock::new(|| {
    let mut list = GlyphList {
        name_to_unicode: HashMap::new(),
        unicode_to_name: HashMap::new(),
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
    pub(super) fn to_unicode(&self, name: &str) -> Option<String> {
        if let Some(unicode) = self.name_to_unicode.get(name) {
            return Some(unicode.clone());
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
        let code = crate::formats::java::parse_hex_int(hex)?;
        let code = code as u32 & 0xFFFF;
        if (0xD800..0xE000).contains(&code) {
            return None;
        }
        char::from_u32(code).map(String::from)
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

    #[test]
    fn orders_integer_keys_as_java_hash_map_does() {
        let entries: Vec<(i32, &str)> = vec![(40, "a"), (3, "b"), (17, "c"), (3, "d")];
        let order: Vec<i32> = java_integer_map_order(&entries)
            .iter()
            .map(|(key, _)| *key)
            .collect();
        assert_eq!(order, vec![17, 3, 40]);
        let replaced = java_integer_map_order(&entries);
        assert_eq!(*replaced[1].1, "d");
    }
}
