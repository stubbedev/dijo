//! The font encodings of TeX's text and math fonts: which character each code of a font sets,
//! from the encodings' Unicode mappings. A font's encoding is known by its name: the Computer
//! Modern fonts by their families (`cmr10`, `cmtt12`, `cmmi7`), the European Computer Modern
//! (`ecrm1000`) and Latin Modern fonts (`ec-lmr10`, `rm-lmr10`) by their prefixes, and fonts of
//! Karl Berry's names (`ptmr8t`) by their encoding suffix.

/// The codes of an encoding, and the text each assigned sets.
type Table = [Option<&'static str>; 256];

/// A font encoding this parser maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// TeX text (OT1), of Computer Modern Roman, bold, slanted and sans serif.
    Ot1,
    /// TeX text of the italic fonts, which have `£` where the others have `$`.
    Ot1Italic,
    /// TeX typewriter text, of the monospaced fonts, ASCII where OT1 has ligatures and quotes.
    Ot1Typewriter,
    /// Cork (T1), the European Computer Modern encoding.
    T1,
    /// TeX math italic (OML).
    Oml,
    /// TeX math symbols (OMS).
    Oms,
}

/// The Computer Modern families of each encoding.
const FAMILIES: [(&str, Encoding); 28] = [
    ("cmr", Encoding::Ot1),
    ("cmbx", Encoding::Ot1),
    ("cmsl", Encoding::Ot1),
    ("cmb", Encoding::Ot1),
    ("cmbxsl", Encoding::Ot1),
    ("cmss", Encoding::Ot1),
    ("cmssbx", Encoding::Ot1),
    ("cmssdc", Encoding::Ot1),
    ("cmssi", Encoding::Ot1),
    ("cmssq", Encoding::Ot1),
    ("cmssqi", Encoding::Ot1),
    ("cmcsc", Encoding::Ot1),
    ("cmdunh", Encoding::Ot1),
    ("cmfib", Encoding::Ot1),
    ("cmvtt", Encoding::Ot1),
    ("cmti", Encoding::Ot1Italic),
    ("cmbxti", Encoding::Ot1Italic),
    ("cmu", Encoding::Ot1Italic),
    ("cmtt", Encoding::Ot1Typewriter),
    ("cmsltt", Encoding::Ot1Typewriter),
    ("cmitt", Encoding::Ot1Typewriter),
    ("cmtcsc", Encoding::Ot1Typewriter),
    ("cmmi", Encoding::Oml),
    ("cmmib", Encoding::Oml),
    ("lmmi", Encoding::Oml),
    ("cmsy", Encoding::Oms),
    ("cmbsy", Encoding::Oms),
    ("lmsy", Encoding::Oms),
];

impl Encoding {
    /// The encoding of the font named `name`, if this parser knows it.
    pub fn of_font(name: &str) -> Option<Encoding> {
        let family = name.trim_end_matches(|c: char| c.is_ascii_digit());
        if let Some(&(_, encoding)) = FAMILIES.iter().find(|(known, _)| *known == family) {
            return Some(encoding);
        }
        if family.starts_with("ec-lm") {
            return Some(Encoding::T1);
        }
        if let Some(shape) = family.strip_prefix("rm-lm") {
            return Some(latin_modern_ot1(shape));
        }
        if let Some(shape) = family.strip_prefix("ec") {
            if shape.len() == 2 && shape.bytes().all(|byte| byte.is_ascii_lowercase()) {
                return Some(Encoding::T1);
            }
        }
        berry(name)
    }

    /// The text code `code` sets, `None` for a code the encoding does not assign.
    pub fn glyph(self, code: i64) -> Option<Glyph> {
        let table = match self {
            Encoding::Ot1 => &OT1,
            Encoding::Ot1Italic => &OT1_ITALIC,
            Encoding::Ot1Typewriter => &OT1_TYPEWRITER,
            Encoding::T1 => &T1,
            Encoding::Oml => &OML,
            Encoding::Oms => &OMS,
        };
        let text = (*table.get(usize::try_from(code).ok()?)?)?;
        Some(Glyph::of(text))
    }
}

/// The OT1 encoding of a Latin Modern font of the `shape` after `rm-lm`: `lmtt`, `lmtl` and the
/// other typewriter fonts begin with `t`, the italics end with `i` but for `lmu`, the unslanted
/// italic.
fn latin_modern_ot1(shape: &str) -> Encoding {
    if shape.starts_with('t') {
        Encoding::Ot1Typewriter
    } else if shape.ends_with('i') || shape == "u" {
        Encoding::Ot1Italic
    } else {
        Encoding::Ot1
    }
}

/// The encoding of a font of Karl Berry's naming scheme, `ptmri7t`, by its suffix: `8t` is T1,
/// `7t` OT1, of typewriter text for Courier (`pcr`) and of italic text for an italic variant
/// (`i`) before the suffix.
fn berry(name: &str) -> Option<Encoding> {
    if name.len() < 6 || !name.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return None;
    }
    if name.ends_with("8t") {
        return Some(Encoding::T1);
    }
    let stem = name.strip_suffix("7t")?;
    Some(if stem.starts_with("pcr") {
        Encoding::Ot1Typewriter
    } else if stem.ends_with('i') {
        Encoding::Ot1Italic
    } else {
        Encoding::Ot1
    })
}

/// What a code of an encoding sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// Text, empty for a glyph that is part of another, such as the hooks of `↪`.
    Text(&'static str),
    /// An accent, set apart from the letter it is placed over or under.
    Accent(Accent),
}

/// An accent: the combining mark it is on a letter, and the text it is alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accent {
    pub mark: char,
    pub alone: &'static str,
}

/// The stroke through `l` and `L` OT1 sets as a glyph of its own, which no letter but those
/// composes with: `ł` has no decomposition.
pub const STROKE: char = '\u{337}';

/// The accents of the encodings.
const ACCENTS: [Accent; 17] = [
    Accent::new('\u{300}', "`"),
    Accent::new('\u{301}', "\u{b4}"),
    Accent::new('\u{302}', "^"),
    Accent::new('\u{303}', "~"),
    Accent::new('\u{304}', "\u{af}"),
    Accent::new('\u{306}', "\u{2d8}"),
    Accent::new('\u{307}', "\u{2d9}"),
    Accent::new('\u{308}', "\u{a8}"),
    Accent::new('\u{30a}', "\u{2da}"),
    Accent::new('\u{30b}', "\u{2dd}"),
    Accent::new('\u{30c}', "\u{2c7}"),
    Accent::new('\u{327}', "\u{b8}"),
    Accent::new('\u{328}', "\u{2db}"),
    Accent::new(STROKE, ""),
    Accent::new('\u{338}', "/"),
    Accent::new('\u{20d7}', "\u{2192}"),
    Accent::new('\u{361}', "\u{2040}"),
];

impl Accent {
    const fn new(mark: char, alone: &'static str) -> Self {
        Self { mark, alone }
    }
}

impl Glyph {
    /// The glyph of a table's `text`: an accent where it is one of [`ACCENTS`]' marks.
    fn of(text: &'static str) -> Glyph {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(only), None) => ACCENTS
                .iter()
                .find(|accent| accent.mark == only)
                .map_or(Glyph::Text(text), |&accent| Glyph::Accent(accent)),
            _ => Glyph::Text(text),
        }
    }
}

/// A table of the printable ASCII characters, `space` to `~`, at their codes, and of nothing else.
const fn ascii() -> Table {
    with([None; 256], 0x20, &ASCII)
}

/// The printable ASCII characters, `space` to `~`.
const ASCII: [&str; 95] = [
    " ", "!", "\"", "#", "$", "%", "&", "'", "(", ")", "*", "+", ",", "-", ".", "/", "0", "1", "2",
    "3", "4", "5", "6", "7", "8", "9", ":", ";", "<", "=", ">", "?", "@", "A", "B", "C", "D", "E",
    "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X",
    "Y", "Z", "[", "\\", "]", "^", "_", "`", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k",
    "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "{", "|", "}", "~",
];

/// `table` with the codes from `start` set to `texts`.
const fn with(mut table: Table, start: usize, texts: &[&'static str]) -> Table {
    let mut index = 0;
    while index < texts.len() {
        table[start + index] = Some(texts[index]);
        index += 1;
    }
    table
}

/// The upright Greek capitals TeX's text and math italic fonts begin with, codes 0x00 to 0x0a.
const GREEK_CAPITALS: [&str; 11] = [
    "\u{393}", "\u{394}", "\u{398}", "\u{39b}", "\u{39e}", "\u{3a0}", "\u{3a3}", "\u{3a5}",
    "\u{3a6}", "\u{3a8}", "\u{3a9}",
];

/// The letters, accents and ligatures TeX text has at codes 0x10 to 0x1f, after its Greek
/// capitals and ligatures or arrows.
const OT1_LETTERS: [&str; 16] = [
    "\u{131}", "\u{237}", "\u{300}", "\u{301}", "\u{30c}", "\u{306}", "\u{304}", "\u{30a}",
    "\u{327}", "\u{df}", "\u{e6}", "\u{153}", "\u{f8}", "\u{c6}", "\u{152}", "\u{d8}",
];

/// The OT1 encoding of `cmr10`.
const OT1: Table = {
    let table = with(ascii(), 0x00, &GREEK_CAPITALS);
    let table = with(table, 0x0b, &["ff", "fi", "fl", "ffi", "ffl"]);
    let table = with(table, 0x10, &OT1_LETTERS);
    let table = with(table, 0x20, &["\u{337}", "!", "\u{201d}"]);
    let table = with(table, 0x27, &["\u{2019}"]);
    let table = with(table, 0x3c, &["\u{a1}", "=", "\u{bf}"]);
    let table = with(
        table,
        0x5c,
        &["\u{201c}", "]", "\u{302}", "\u{307}", "\u{2018}"],
    );
    with(
        table,
        0x7b,
        &["\u{2013}", "\u{2014}", "\u{30b}", "\u{303}", "\u{308}"],
    )
};

/// The OT1 encoding of `cmti10`, which sets `£` where `cmr10` sets `$`.
const OT1_ITALIC: Table = with(OT1, 0x24, &["\u{a3}"]);

/// The OT1 encoding of `cmtt10`.
const OT1_TYPEWRITER: Table = {
    let table = with(ascii(), 0x00, &GREEK_CAPITALS);
    let table = with(
        table,
        0x0b,
        &["\u{2191}", "\u{2193}", "'", "\u{a1}", "\u{bf}"],
    );
    let table = with(table, 0x10, &OT1_LETTERS);
    let table = with(table, 0x20, &["\u{2423}"]);
    let table = with(table, 0x27, &["\u{2019}"]);
    let table = with(table, 0x60, &["\u{2018}"]);
    with(table, 0x7f, &["\u{308}"])
};

/// The T1 encoding of `ecrm1000`.
const T1: Table = {
    let table = with(
        ascii(),
        0x00,
        &[
            "\u{300}", "\u{301}", "\u{302}", "\u{303}", "\u{308}", "\u{30b}", "\u{30a}", "\u{30c}",
            "\u{306}", "\u{304}", "\u{307}", "\u{327}", "\u{328}", "\u{201a}", "\u{2039}",
            "\u{203a}", "\u{201c}", "\u{201d}", "\u{201e}", "\u{ab}", "\u{bb}", "\u{2013}",
            "\u{2014}", "", "", "\u{131}", "\u{237}", "ff", "fi", "fl", "ffi", "ffl", "\u{2423}",
        ],
    );
    let table = with(table, 0x27, &["\u{2019}"]);
    let table = with(table, 0x60, &["\u{2018}"]);
    let table = with(
        table,
        0x7f,
        &[
            "-", "\u{102}", "\u{104}", "\u{106}", "\u{10c}", "\u{10e}", "\u{11a}", "\u{118}",
            "\u{11e}", "\u{139}", "\u{13d}", "\u{141}", "\u{143}", "\u{147}", "\u{14a}", "\u{150}",
            "\u{154}", "\u{158}", "\u{15a}", "\u{160}", "\u{15e}", "\u{164}", "\u{162}", "\u{170}",
            "\u{16e}", "\u{178}", "\u{179}", "\u{17d}", "\u{17b}", "\u{132}", "\u{130}", "\u{111}",
            "\u{a7}", "\u{103}", "\u{105}", "\u{107}", "\u{10d}", "\u{10f}", "\u{11b}", "\u{119}",
            "\u{11f}", "\u{13a}", "\u{13e}", "\u{142}", "\u{144}", "\u{148}", "\u{14b}", "\u{151}",
            "\u{155}", "\u{159}", "\u{15b}", "\u{161}", "\u{15f}", "\u{165}", "\u{163}", "\u{171}",
            "\u{16f}", "\u{ff}", "\u{17a}", "\u{17e}", "\u{17c}", "\u{133}", "\u{a1}", "\u{bf}",
            "\u{a3}",
        ],
    );
    with(table, 0xc0, &T1_LETTERS)
};

/// T1's letters at 0xc0 to 0xff: Latin-1's, but for `Œ`, `SS`, `œ` and `ß` at 0xd7, 0xdf, 0xf7
/// and 0xff.
const T1_LETTERS: [&str; 64] = [
    "\u{c0}", "\u{c1}", "\u{c2}", "\u{c3}", "\u{c4}", "\u{c5}", "\u{c6}", "\u{c7}", "\u{c8}",
    "\u{c9}", "\u{ca}", "\u{cb}", "\u{cc}", "\u{cd}", "\u{ce}", "\u{cf}", "\u{d0}", "\u{d1}",
    "\u{d2}", "\u{d3}", "\u{d4}", "\u{d5}", "\u{d6}", "\u{152}", "\u{d8}", "\u{d9}", "\u{da}",
    "\u{db}", "\u{dc}", "\u{dd}", "\u{de}", "SS", "\u{e0}", "\u{e1}", "\u{e2}", "\u{e3}", "\u{e4}",
    "\u{e5}", "\u{e6}", "\u{e7}", "\u{e8}", "\u{e9}", "\u{ea}", "\u{eb}", "\u{ec}", "\u{ed}",
    "\u{ee}", "\u{ef}", "\u{f0}", "\u{f1}", "\u{f2}", "\u{f3}", "\u{f4}", "\u{f5}", "\u{f6}",
    "\u{153}", "\u{f8}", "\u{f9}", "\u{fa}", "\u{fb}", "\u{fc}", "\u{fd}", "\u{fe}", "\u{df}",
];

/// The OML encoding of `cmmi10`: its italic letters as the letters they are.
const OML: Table = {
    let table = with(ascii(), 0x00, &GREEK_CAPITALS);
    let table = with(
        table,
        0x0b,
        &[
            "\u{3b1}", "\u{3b2}", "\u{3b3}", "\u{3b4}", "\u{3f5}", "\u{3b6}", "\u{3b7}", "\u{3b8}",
            "\u{3b9}", "\u{3ba}", "\u{3bb}", "\u{3bc}", "\u{3bd}", "\u{3be}", "\u{3c0}", "\u{3c1}",
            "\u{3c3}", "\u{3c4}", "\u{3c5}", "\u{3d5}", "\u{3c7}", "\u{3c8}", "\u{3c9}", "\u{3b5}",
            "\u{3d1}", "\u{3d6}", "\u{3f1}", "\u{3c2}", "\u{3c6}", "\u{21bc}", "\u{21bd}",
            "\u{21c0}", "\u{21c1}", "", "", "\u{25b9}", "\u{25c3}",
        ],
    );
    let table = with(
        table,
        0x3a,
        &[".", ",", "<", "/", ">", "\u{22c6}", "\u{2202}"],
    );
    let table = with(
        table,
        0x5b,
        &[
            "\u{266d}", "\u{266e}", "\u{266f}", "\u{2323}", "\u{2322}", "\u{2113}",
        ],
    );
    with(
        table,
        0x7b,
        &["\u{131}", "\u{237}", "\u{2118}", "\u{20d7}", "\u{361}"],
    )
};

/// The OMS encoding of `cmsy10`: its calligraphic capitals as the letters they are.
const OMS: Table = {
    let table = with(
        ascii(),
        0x00,
        &[
            "\u{2212}", "\u{22c5}", "\u{d7}", "\u{2217}", "\u{f7}", "\u{22c4}", "\u{b1}",
            "\u{2213}", "\u{2295}", "\u{2296}", "\u{2297}", "\u{2298}", "\u{2299}", "\u{25ef}",
            "\u{2218}", "\u{2022}", "\u{224d}", "\u{2261}", "\u{2286}", "\u{2287}", "\u{2264}",
            "\u{2265}", "\u{2aaf}", "\u{2ab0}", "\u{223c}", "\u{2248}", "\u{2282}", "\u{2283}",
            "\u{226a}", "\u{226b}", "\u{227a}", "\u{227b}", "\u{2190}", "\u{2192}", "\u{2191}",
            "\u{2193}", "\u{2194}", "\u{2197}", "\u{2198}", "\u{2243}", "\u{21d0}", "\u{21d2}",
            "\u{21d1}", "\u{21d3}", "\u{21d4}", "\u{2196}", "\u{2199}", "\u{221d}", "\u{2032}",
            "\u{221e}", "\u{2208}", "\u{220b}", "\u{25b3}", "\u{25bd}", "\u{338}", "", "\u{2200}",
            "\u{2203}", "\u{ac}", "\u{2205}", "\u{211c}", "\u{2111}", "\u{22a4}", "\u{22a5}",
            "\u{2135}",
        ],
    );
    with(
        table,
        0x5b,
        &[
            "\u{222a}", "\u{2229}", "\u{228e}", "\u{2227}", "\u{2228}", "\u{22a2}", "\u{22a3}",
            "\u{230a}", "\u{230b}", "\u{2308}", "\u{2309}", "{", "}", "\u{27e8}", "\u{27e9}", "|",
            "\u{2016}", "\u{2195}", "\u{21d5}", "\\", "\u{2240}", "\u{221a}", "\u{2a3f}",
            "\u{2207}", "\u{222b}", "\u{2294}", "\u{2293}", "\u{2291}", "\u{2292}", "\u{a7}",
            "\u{2020}", "\u{2021}", "\u{b6}", "\u{2663}", "\u{2662}", "\u{2661}", "\u{2660}",
        ],
    )
};
