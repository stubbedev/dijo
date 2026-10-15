//! The parts of a TrueType font program PDFBox's text extraction reads, read as FontBox's
//! `TTFParser` reads them: the table directory, `head`, `hhea`, `maxp`, `hmtx`, `loca`, `post`
//! and the `cmap` subtables, each mapped character to glyph as FontBox maps it. A font FontBox
//! fails to read is damaged to PDFBox, and fails here where it fails there.

use std::collections::HashMap;

use super::Unported;

/// `WGL4Names.MAC_GLYPH_NAMES`: the glyph names of the standard Macintosh ordering.
const MAC_GLYPH_NAMES: [&str; 258] = [
    ".notdef",
    ".null",
    "nonmarkingreturn",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quotesingle",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "grave",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    "Adieresis",
    "Aring",
    "Ccedilla",
    "Eacute",
    "Ntilde",
    "Odieresis",
    "Udieresis",
    "aacute",
    "agrave",
    "acircumflex",
    "adieresis",
    "atilde",
    "aring",
    "ccedilla",
    "eacute",
    "egrave",
    "ecircumflex",
    "edieresis",
    "iacute",
    "igrave",
    "icircumflex",
    "idieresis",
    "ntilde",
    "oacute",
    "ograve",
    "ocircumflex",
    "odieresis",
    "otilde",
    "uacute",
    "ugrave",
    "ucircumflex",
    "udieresis",
    "dagger",
    "degree",
    "cent",
    "sterling",
    "section",
    "bullet",
    "paragraph",
    "germandbls",
    "registered",
    "copyright",
    "trademark",
    "acute",
    "dieresis",
    "notequal",
    "AE",
    "Oslash",
    "infinity",
    "plusminus",
    "lessequal",
    "greaterequal",
    "yen",
    "mu",
    "partialdiff",
    "summation",
    "product",
    "pi",
    "integral",
    "ordfeminine",
    "ordmasculine",
    "Omega",
    "ae",
    "oslash",
    "questiondown",
    "exclamdown",
    "logicalnot",
    "radical",
    "florin",
    "approxequal",
    "Delta",
    "guillemotleft",
    "guillemotright",
    "ellipsis",
    "nonbreakingspace",
    "Agrave",
    "Atilde",
    "Otilde",
    "OE",
    "oe",
    "endash",
    "emdash",
    "quotedblleft",
    "quotedblright",
    "quoteleft",
    "quoteright",
    "divide",
    "lozenge",
    "ydieresis",
    "Ydieresis",
    "fraction",
    "currency",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "daggerdbl",
    "periodcentered",
    "quotesinglbase",
    "quotedblbase",
    "perthousand",
    "Acircumflex",
    "Ecircumflex",
    "Aacute",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Oacute",
    "Ocircumflex",
    "apple",
    "Ograve",
    "Uacute",
    "Ucircumflex",
    "Ugrave",
    "dotlessi",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "Lslash",
    "lslash",
    "Scaron",
    "scaron",
    "Zcaron",
    "zcaron",
    "brokenbar",
    "Eth",
    "eth",
    "Yacute",
    "yacute",
    "Thorn",
    "thorn",
    "minus",
    "multiply",
    "onesuperior",
    "twosuperior",
    "threesuperior",
    "onehalf",
    "onequarter",
    "threequarters",
    "franc",
    "Gbreve",
    "gbreve",
    "Idotaccent",
    "Scedilla",
    "scedilla",
    "Cacute",
    "cacute",
    "Ccaron",
    "ccaron",
    "dcroat",
];

/// `CmapTable` platform and encoding identifiers.
pub(super) const PLATFORM_UNICODE: u16 = 0;
pub(super) const PLATFORM_MACINTOSH: u16 = 1;
pub(super) const PLATFORM_WINDOWS: u16 = 3;
pub(super) const ENCODING_UNICODE_1_0: u16 = 0;
pub(super) const ENCODING_UNICODE_1_1: u16 = 1;
pub(super) const ENCODING_UNICODE_2_0_BMP: u16 = 3;
pub(super) const ENCODING_UNICODE_2_0_FULL: u16 = 4;
pub(super) const ENCODING_MAC_ROMAN: u16 = 0;
pub(super) const ENCODING_WIN_SYMBOL: u16 = 0;
pub(super) const ENCODING_WIN_UNICODE_BMP: u16 = 1;
pub(super) const ENCODING_WIN_UNICODE_FULL: u16 = 10;

/// Why FontBox fails to read a font program, which makes the font damaged to PDFBox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Damaged;

/// What reading a font program gives: the font, the font damaged as FontBox finds it, or a font
/// FontBox reads in a way the port does not follow.
#[derive(Debug)]
pub(super) enum Read {
    Font(TrueType),
    Damaged(Damaged),
    Unported(String),
}

/// Where a font program comes from: a document, which FontBox reads leniently, or PDFBox's own
/// resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Embedded,
    Bundled,
}

/// A character to glyph mapping, as FontBox's `CmapSubtable` holds it.
#[derive(Debug, Default)]
pub(super) struct CmapSubtable {
    pub(super) platform_id: u16,
    pub(super) encoding_id: u16,
    code_to_glyph: HashMap<u32, u32>,
    /// `glyphIdToCharacterCode`: a glyph's code, or none, or several (`Integer.MIN_VALUE`).
    glyph_to_code: Vec<GlyphCode>,
    glyph_to_codes: HashMap<u32, Vec<u32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GlyphCode {
    None,
    One(u32),
    Several,
}

impl CmapSubtable {
    /// `CmapSubtable.getGlyphId`: 0 for a code it does not map.
    pub(super) fn glyph(&self, code: u32) -> u32 {
        self.code_to_glyph.get(&code).copied().unwrap_or(0)
    }

    /// `CmapSubtable.getCharCodes`: the codes mapped to `glyph`, in ascending order when there
    /// are several.
    pub(super) fn codes(&self, glyph: u32) -> Option<Vec<u32>> {
        match self.glyph_to_code.get(glyph as usize)? {
            GlyphCode::None => None,
            GlyphCode::One(code) => Some(vec![*code]),
            GlyphCode::Several => {
                let mut codes = self.glyph_to_codes.get(&glyph)?.clone();
                codes.sort_unstable();
                Some(codes)
            }
        }
    }

    /// `buildGlyphIdToCharacterCodeLookup`: each glyph's codes from the code to glyph map.
    fn build_reverse(&mut self, max_glyph: u32) {
        self.glyph_to_code = vec![GlyphCode::None; max_glyph as usize + 1];
        for (&code, &glyph) in &self.code_to_glyph {
            let slot = &mut self.glyph_to_code[glyph as usize];
            match *slot {
                GlyphCode::None => *slot = GlyphCode::One(code),
                GlyphCode::One(first) => {
                    *slot = GlyphCode::Several;
                    self.glyph_to_codes.insert(glyph, vec![first, code]);
                }
                GlyphCode::Several => self.glyph_to_codes.entry(glyph).or_default().push(code),
            }
        }
    }
}

/// A TrueType font program, as far as text extraction reads it.
#[derive(Debug)]
pub(super) struct TrueType {
    pub(super) units_per_em: u16,
    bbox: [i16; 4],
    pub(super) num_glyphs: u16,
    advance_widths: Vec<u16>,
    post_names: Option<Vec<String>>,
    names_to_glyphs: HashMap<String, u32>,
    cmaps: Option<Vec<CmapSubtable>>,
}

/// A big-endian reader over the whole font, as FontBox's `TTFDataStream` reads it: past the
/// end it fails.
struct Data<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Data<'_> {
    /// `seek`, which stops at the end of the font.
    fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.bytes.len());
    }

    fn take(&mut self, count: usize) -> Result<&[u8], Damaged> {
        let end = self
            .pos
            .checked_add(count)
            .filter(|&end| end <= self.bytes.len());
        let Some(end) = end else {
            return Err(Damaged);
        };
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, Damaged> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Damaged> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn i16(&mut self) -> Result<i16, Damaged> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> Result<u32, Damaged> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// `read32Fixed`: a 16.16 number as FontBox computes it, in `float`.
    fn fixed(&mut self) -> Result<f32, Damaged> {
        let integer = f32::from(self.i16()?);
        let fraction = f32::from(self.u16()?) / 65536.0;
        Ok(integer + fraction)
    }
}

#[derive(Debug, Clone, Copy)]
struct Table {
    offset: usize,
    length: usize,
}

impl TrueType {
    /// `new TTFParser(true).parse`: a program embedded in a document.
    pub(super) fn read(bytes: &[u8]) -> Read {
        if bytes.starts_with(b"ttcf") {
            return Read::Unported(
                "a TrueType collection as a font program, which the port does not read".to_owned(),
            );
        }
        if bytes.starts_with(b"OTTO") {
            return Read::Unported(
                "an OpenType font program with PostScript outlines, which the port does not read"
                    .to_owned(),
            );
        }
        match Self::parse(bytes, Source::Embedded) {
            Ok(font) => Read::Font(font),
            Err(Parsing::Damaged(damaged)) => Read::Damaged(damaged),
            Err(Parsing::Unported(reason)) => Read::Unported(reason),
        }
    }

    /// `new TTFParser().parse` of a font PDFBox bundles, whose tables FontBox is known to read.
    pub(super) fn read_bundled(bytes: &[u8]) -> Result<Self, Unported> {
        Self::parse(bytes, Source::Bundled)
            .map_err(|_| Unported("a font PDFBox bundles that the port fails to read".to_owned()))
    }

    fn parse(bytes: &[u8], source: Source) -> Result<Self, Parsing> {
        let embedded = source == Source::Embedded;
        let mut data = Data { bytes, pos: 0 };
        data.u32()?;
        let number_of_tables = data.u16()?;
        data.take(6)?;
        let mut tables: HashMap<[u8; 4], Table> = HashMap::new();
        for _ in 0..number_of_tables {
            let tag: [u8; 4] = data.take(4)?.try_into().unwrap_or_default();
            data.u32()?;
            let offset = data.u32()? as usize;
            let length = data.u32()? as usize;
            if length == 0 && &tag != b"glyf" {
                continue;
            }
            if offset.saturating_add(length) <= bytes.len() {
                tables.insert(tag, Table { offset, length });
            }
        }
        if tables.contains_key(b"CFF ") || tables.contains_key(b"CFF2") {
            return Err(Parsing::Unported(
                "a TrueType font program with PostScript outlines, which the port does not read"
                    .to_owned(),
            ));
        }
        let table = |tag: &[u8; 4]| tables.get(tag).copied();
        let head = table(b"head").ok_or(Parsing::Damaged(Damaged))?;
        data.seek(head.offset);
        data.take(18)?;
        let units_per_em = data.u16()?;
        data.take(16)?;
        let bbox = [data.i16()?, data.i16()?, data.i16()?, data.i16()?];
        data.take(6)?;
        let index_to_loc_format = data.i16()?;
        let hhea = table(b"hhea").ok_or(Parsing::Damaged(Damaged))?;
        data.seek(hhea.offset);
        data.take(34)?;
        let number_of_h_metrics = data.u16()?;
        let maxp = table(b"maxp").ok_or(Parsing::Damaged(Damaged))?;
        data.seek(maxp.offset);
        let maxp_version = data.fixed()?;
        let num_glyphs = data.u16()?;
        if maxp_version >= 1.0 {
            data.take(26)?;
        }
        let mut font = TrueType {
            units_per_em,
            bbox,
            num_glyphs,
            advance_widths: Vec::new(),
            post_names: None,
            names_to_glyphs: HashMap::new(),
            cmaps: None,
        };
        if let Some(post) = table(b"post") {
            font.post_names = read_post(&mut data, &post, num_glyphs)?;
        } else if !embedded {
            return Err(Parsing::Damaged(Damaged));
        }
        if let Some(loca) = table(b"loca") {
            data.seek(loca.offset);
            let mut offsets = Vec::with_capacity(usize::from(num_glyphs) + 1);
            for _ in 0..=usize::from(num_glyphs) {
                offsets.push(match index_to_loc_format {
                    0 => u32::from(data.u16()?) * 2,
                    1 => data.u32()?,
                    _ => return Err(Parsing::Damaged(Damaged)),
                });
            }
            if num_glyphs == 1 && offsets[0] == 0 && offsets[1] == 0 {
                return Err(Parsing::Damaged(Damaged));
            }
        } else {
            return Err(Parsing::Damaged(Damaged));
        }
        if table(b"glyf").is_none() {
            return Err(Parsing::Damaged(Damaged));
        }
        if table(b"name").is_none() && !embedded {
            return Err(Parsing::Damaged(Damaged));
        }
        let hmtx = table(b"hmtx").ok_or(Parsing::Damaged(Damaged))?;
        data.seek(hmtx.offset);
        for _ in 0..number_of_h_metrics {
            font.advance_widths.push(data.u16()?);
            data.i16()?;
        }
        if let Some(cmap) = table(b"cmap") {
            font.cmaps = Some(read_cmap(&mut data, &cmap, num_glyphs)?);
        } else if !embedded {
            return Err(Parsing::Damaged(Damaged));
        }
        if let Some(names) = &font.post_names {
            for (index, name) in names.iter().enumerate() {
                font.names_to_glyphs.insert(name.clone(), index as u32);
            }
        }
        if let Some(name) = table(b"name") {
            read_name(&mut data, name)?;
        }
        if let Some(os2) = table(b"OS/2") {
            read_os2(&mut data, os2)?;
        }
        if let Some(kern) = table(b"kern") {
            read_kern(&mut data, kern)?;
        }
        let vertical_metrics = match table(b"vhea") {
            Some(vhea) => Some(read_vhea(&mut data, vhea)?),
            None => None,
        };
        if let Some(vmtx) = table(b"vmtx") {
            let count = vertical_metrics.ok_or(Parsing::Damaged(Damaged))?;
            read_vmtx(&mut data, vmtx, count, num_glyphs)?;
        }
        if let Some(vorg) = table(b"VORG") {
            data.seek(vorg.offset);
            data.take(6)?;
            let count = data.u16()?;
            data.take(4 * usize::from(count))?;
        }
        if let Some(gsub) = table(b"GSUB") {
            gsub::read(&mut data, gsub.offset)?;
        }
        Ok(font)
    }

    /// `TrueTypeFont.getAdvanceWidth`.
    pub(super) fn advance_width(&self, glyph: u32) -> u16 {
        match self.advance_widths.last() {
            None => 250,
            Some(&last) => self
                .advance_widths
                .get(glyph as usize)
                .copied()
                .unwrap_or(last),
        }
    }

    /// `TrueTypeFont.getFontBBox`: the `head` bounding box, scaled to 1000 units per em.
    pub(super) fn font_bbox(&self) -> [f32; 4] {
        let scale = 1000.0 / f32::from(self.units_per_em);
        self.bbox.map(|value| f32::from(value) * scale)
    }

    /// The glyph names of the `post` table.
    pub(super) fn post_name(&self, glyph: u32) -> Option<&str> {
        self.post_names
            .as_ref()
            .and_then(|names| names.get(glyph as usize))
            .map(String::as_str)
    }

    /// `CmapTable.getSubtable`: the first subtable of the platform and encoding.
    pub(super) fn subtable(&self, platform: u16, encoding: u16) -> Option<&CmapSubtable> {
        self.cmaps
            .as_ref()?
            .iter()
            .find(|cmap| cmap.platform_id == platform && cmap.encoding_id == encoding)
    }

    /// The subtables, when the font has a `cmap` table.
    pub(super) fn subtables(&self) -> Option<&[CmapSubtable]> {
        self.cmaps.as_deref()
    }

    /// `TrueTypeFont.getUnicodeCmapLookup(false)`.
    pub(super) fn unicode_cmap(&self) -> Option<&CmapSubtable> {
        let cmaps = self.cmaps.as_ref()?;
        [
            (PLATFORM_UNICODE, ENCODING_UNICODE_2_0_FULL),
            (PLATFORM_WINDOWS, ENCODING_WIN_UNICODE_FULL),
            (PLATFORM_UNICODE, ENCODING_UNICODE_2_0_BMP),
            (PLATFORM_WINDOWS, ENCODING_WIN_UNICODE_BMP),
            (PLATFORM_WINDOWS, ENCODING_WIN_SYMBOL),
            (PLATFORM_UNICODE, ENCODING_UNICODE_1_1),
        ]
        .iter()
        .find_map(|&(platform, encoding)| self.subtable(platform, encoding))
        .or_else(|| cmaps.first())
    }

    /// `TrueTypeFont.nameToGID`: by `post` name, by `uniXXXX` name through the Unicode cmap,
    /// or by `gNN` name.
    pub(super) fn name_to_glyph(&self, name: &str) -> u32 {
        if let Some(&glyph) = self.names_to_glyphs.get(name) {
            if glyph > 0 && glyph < u32::from(self.num_glyphs) {
                return glyph;
            }
        }
        if let Some(unicode) = parse_uni_name(name) {
            return self.unicode_cmap().map_or(0, |cmap| cmap.glyph(unicode));
        }
        if let Some(digits) = name.strip_prefix('g') {
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                if let Some(glyph) = crate::formats::java::parse_int(digits) {
                    return glyph as u32;
                }
            }
        }
        0
    }
}

/// `TrueTypeFont.parseUniName`: the code point of a `uniXXXX` name.
fn parse_uni_name(name: &str) -> Option<u32> {
    let hex = name.strip_prefix("uni").filter(|_| name.len() == 7)?;
    let code = u32::from_str_radix(hex, 16).ok()?;
    if hex.starts_with(['+', '-']) || (0xD800..=0xDFFF).contains(&code) {
        return None;
    }
    Some(code)
}

enum Parsing {
    Damaged(Damaged),
    Unported(String),
}

impl From<Damaged> for Parsing {
    fn from(damaged: Damaged) -> Self {
        Self::Damaged(damaged)
    }
}

/// `NamingTable.read`: the name records, and their strings within the table.
fn read_name(data: &mut Data, name: Table) -> Result<(), Damaged> {
    data.seek(name.offset);
    data.take(2)?;
    let count = usize::from(data.u16()?);
    data.take(2)?;
    let mut strings = Vec::with_capacity(count);
    for _ in 0..count {
        data.take(8)?;
        let length = usize::from(data.u16()?);
        let offset = usize::from(data.u16()?);
        strings.push((offset, length));
    }
    for (offset, length) in strings {
        if offset > name.length {
            continue;
        }
        data.seek(name.offset + 6 + count * 12 + offset);
        data.take(length)?;
    }
    Ok(())
}

/// `OS2WindowsMetricsTable.read`: the fields every version has, the later ones read only as far
/// as the font goes.
fn read_os2(data: &mut Data, os2: Table) -> Result<(), Damaged> {
    data.seek(os2.offset);
    data.take(68)?;
    Ok(())
}

/// `KerningTable.read`: the subtables of a version 0 table, read one after another.
fn read_kern(data: &mut Data, kern: Table) -> Result<(), Damaged> {
    data.seek(kern.offset);
    let version = data.u16()?;
    let subtables = if version == 0 {
        data.u16()?
    } else {
        data.u16()?;
        0
    };
    for _ in 0..subtables {
        if data.u16()? != 0 {
            continue;
        }
        let length = data.u16()?;
        if length < 6 {
            continue;
        }
        let coverage = data.u16()?;
        if (coverage & 0xFF00) >> 8 == 0 {
            let pairs = usize::from(data.u16()?);
            data.take(6)?;
            data.take(6 * pairs)?;
        }
    }
    Ok(())
}

/// `VerticalHeaderTable.read`: its number of vertical metrics.
fn read_vhea(data: &mut Data, vhea: Table) -> Result<u16, Damaged> {
    data.seek(vhea.offset);
    data.take(34)?;
    data.u16()
}

/// `VerticalMetricsTable.read`.
fn read_vmtx(data: &mut Data, vmtx: Table, metrics: u16, num_glyphs: u16) -> Result<(), Damaged> {
    data.seek(vmtx.offset);
    let metrics = usize::from(metrics);
    data.take(4 * metrics)?;
    let mut read = 4 * metrics;
    if read < vmtx.length {
        let mut remaining = i64::from(num_glyphs) - metrics as i64;
        if remaining < 0 {
            remaining = i64::from(num_glyphs);
        }
        for _ in 0..remaining {
            if read < vmtx.length {
                data.take(2)?;
                read += 2;
            }
        }
    }
    Ok(())
}

/// FontBox's `GlyphSubstitutionTable.read`, as far as it can fail: the script, feature and
/// lookup lists, and the substitution subtables of the lookup types it reads.
mod gsub {
    use super::{Damaged, Data};
    use std::collections::HashSet;

    pub(super) fn read(data: &mut Data, start: usize) -> Result<(), Damaged> {
        data.seek(start);
        data.u16()?;
        let minor = data.u16()?;
        let scripts = usize::from(data.u16()?);
        let features = usize::from(data.u16()?);
        let lookups = usize::from(data.u16()?);
        if minor == 1 {
            data.u32()?;
        }
        read_script_list(data, start + scripts)?;
        read_feature_list(data, start + features)?;
        if lookups > 0 {
            read_lookup_list(data, start + lookups)?;
        }
        Ok(())
    }

    fn tag(data: &mut Data) -> Result<[u8; 4], Damaged> {
        let bytes = data.take(4)?;
        Ok([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    fn read_script_list(data: &mut Data, offset: usize) -> Result<(), Damaged> {
        data.seek(offset);
        let count = usize::from(data.u16()?);
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let tag = tag(data)?;
            let script = usize::from(data.u16()?);
            if script < data.pos - offset {
                return Ok(());
            }
            records.push((tag, script));
        }
        let mut seen = HashSet::new();
        for (tag, script) in records {
            if seen.insert(tag) {
                read_script_table(data, offset + script)?;
            }
        }
        Ok(())
    }

    fn read_script_table(data: &mut Data, offset: usize) -> Result<(), Damaged> {
        data.seek(offset);
        let default = usize::from(data.u16()?);
        let count = usize::from(data.u16()?);
        let mut records: Vec<usize> = Vec::with_capacity(count);
        let mut previous: Option<[u8; 4]> = None;
        for _ in 0..count {
            let tag = tag(data)?;
            let lang_sys = usize::from(data.u16()?);
            if lang_sys < data.pos - offset || previous.is_some_and(|previous| tag < previous) {
                return Ok(());
            }
            previous = Some(tag);
            records.push(lang_sys);
        }
        if default != 0 {
            read_lang_sys(data, offset + default)?;
        }
        for lang_sys in records {
            read_lang_sys(data, offset + lang_sys)?;
        }
        Ok(())
    }

    fn read_lang_sys(data: &mut Data, offset: usize) -> Result<(), Damaged> {
        data.seek(offset);
        data.take(4)?;
        let count = usize::from(data.u16()?);
        data.take(2 * count)?;
        Ok(())
    }

    /// `\w{4}`: four word characters.
    fn is_word(tag: &[u8; 4]) -> bool {
        tag.iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }

    fn read_feature_list(data: &mut Data, offset: usize) -> Result<(), Damaged> {
        data.seek(offset);
        let count = usize::from(data.u16()?);
        let mut records = Vec::with_capacity(count);
        let mut previous: Option<[u8; 4]> = None;
        for _ in 0..count {
            let tag = tag(data)?;
            if let Some(previous) = previous {
                if tag < previous && !(is_word(&tag) && is_word(&previous)) {
                    return Ok(());
                }
            }
            previous = Some(tag);
            records.push(usize::from(data.u16()?));
        }
        for feature in records {
            data.seek(offset + feature);
            data.take(2)?;
            let lookups = usize::from(data.u16()?);
            data.take(2 * lookups)?;
        }
        Ok(())
    }

    fn read_lookup_list(data: &mut Data, offset: usize) -> Result<(), Damaged> {
        data.seek(offset);
        let count = usize::from(data.u16()?);
        let mut lookups = Vec::with_capacity(count);
        for _ in 0..count {
            lookups.push(usize::from(data.u16()?));
        }
        let mut read = HashSet::new();
        for lookup in lookups {
            if read.insert(lookup) {
                read_lookup_table(data, offset + lookup)?;
            }
        }
        Ok(())
    }

    fn read_lookup_table(data: &mut Data, offset: usize) -> Result<(), Damaged> {
        data.seek(offset);
        let mut lookup_type = data.u16()?;
        let flag = data.u16()?;
        let count = usize::from(data.u16()?);
        let mut subtables = Vec::with_capacity(count);
        for _ in 0..count {
            let subtable = usize::from(data.u16()?);
            if subtable == 0 || offset + subtable > data.bytes.len() {
                return Ok(());
            }
            subtables.push(subtable);
        }
        if flag & 0x0010 != 0 {
            data.u16()?;
        }
        match lookup_type {
            1..=4 => {
                for subtable in subtables {
                    read_subtable(data, offset + subtable, lookup_type)?;
                }
            }
            7 => {
                for subtable in subtables {
                    data.seek(offset + subtable);
                    if data.u16()? != 1 {
                        continue;
                    }
                    let extension_type = data.u16()?;
                    if lookup_type != 7 && lookup_type != extension_type {
                        continue;
                    }
                    lookup_type = extension_type;
                    let extension = data.u32()? as usize;
                    read_subtable(data, offset + subtable + extension, extension_type)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn read_subtable(data: &mut Data, offset: usize, lookup_type: u16) -> Result<(), Damaged> {
        data.seek(offset);
        let format = data.u16()?;
        match lookup_type {
            1 => match format {
                1 => {
                    let coverage = usize::from(data.u16()?);
                    data.u16()?;
                    read_coverage(data, offset + coverage)?;
                }
                2 => {
                    let coverage = usize::from(data.u16()?);
                    let count = usize::from(data.u16()?);
                    data.take(2 * count)?;
                    read_coverage(data, offset + coverage)?;
                }
                _ => {}
            },
            2 | 3 => {
                if format != 1 {
                    return Err(Damaged);
                }
                let coverage = usize::from(data.u16()?);
                let count = usize::from(data.u16()?);
                let mut sets = Vec::with_capacity(count);
                for _ in 0..count {
                    sets.push(usize::from(data.u16()?));
                }
                if read_coverage(data, offset + coverage)? != count {
                    return Err(Damaged);
                }
                for set in sets {
                    data.seek(offset + set);
                    let glyphs = usize::from(data.u16()?);
                    data.take(2 * glyphs)?;
                }
            }
            4 => {
                if format != 1 {
                    return Err(Damaged);
                }
                let coverage = usize::from(data.u16()?);
                let count = usize::from(data.u16()?);
                let mut sets = Vec::with_capacity(count);
                for _ in 0..count {
                    sets.push(usize::from(data.u16()?));
                }
                if read_coverage(data, offset + coverage)? != count {
                    return Err(Damaged);
                }
                for set in sets {
                    read_ligature_set(data, offset + set)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn read_ligature_set(data: &mut Data, offset: usize) -> Result<(), Damaged> {
        data.seek(offset);
        let count = usize::from(data.u16()?);
        let mut ligatures = Vec::with_capacity(count);
        for _ in 0..count {
            ligatures.push(usize::from(data.u16()?));
        }
        for ligature in ligatures {
            data.seek(offset + ligature);
            data.u16()?;
            let components = usize::from(data.u16()?);
            if components > 100 {
                return Err(Damaged);
            }
            data.take(2 * components.saturating_sub(1))?;
        }
        Ok(())
    }

    /// `readCoverageTable`: how many glyphs it covers.
    fn read_coverage(data: &mut Data, offset: usize) -> Result<usize, Damaged> {
        data.seek(offset);
        match data.u16()? {
            1 => {
                let count = usize::from(data.u16()?);
                data.take(2 * count)?;
                Ok(count)
            }
            2 => {
                let count = usize::from(data.u16()?);
                let mut glyphs = 0;
                for _ in 0..count {
                    let start = data.u16()?;
                    let end = data.u16()?;
                    data.u16()?;
                    if start <= end {
                        glyphs += usize::from(end - start) + 1;
                    }
                }
                Ok(glyphs)
            }
            _ => Err(Damaged),
        }
    }
}

/// `PostScriptTable.read`: the glyph names, if the table has any.
fn read_post(
    data: &mut Data,
    post: &Table,
    num_glyphs: u16,
) -> Result<Option<Vec<String>>, Parsing> {
    data.seek(post.offset);
    let format = data.fixed()?;
    data.take(28)?;
    if data.pos == data.bytes.len() {
        return Ok(None);
    }
    if format == 1.0 {
        return Ok(Some(
            MAC_GLYPH_NAMES.iter().map(|&n| n.to_owned()).collect(),
        ));
    }
    if format == 2.0 {
        let count = data.u16()?;
        let mut indices = Vec::with_capacity(usize::from(count));
        let mut max_index: i32 = i32::MIN;
        for _ in 0..count {
            let index = data.u16()?;
            indices.push(index);
            if index <= 32767 {
                max_index = max_index.max(i32::from(index));
            }
        }
        let mut extra: Option<Vec<String>> = None;
        if max_index >= 258 {
            let length = (max_index - 258 + 1) as usize;
            let mut names = Vec::with_capacity(length);
            for _ in 0..length {
                let count = data.u8()?;
                match data.take(usize::from(count)) {
                    Ok(bytes) => names.push(bytes.iter().map(|&b| char::from(b)).collect()),
                    Err(_) => {
                        names.resize(length, ".notdef".to_owned());
                        break;
                    }
                }
            }
            extra = Some(names);
        }
        let names = indices
            .iter()
            .map(|&index| {
                if index < 258 {
                    MAC_GLYPH_NAMES[usize::from(index)].to_owned()
                } else if let (true, Some(extra)) = (index <= 32767, &extra) {
                    extra[usize::from(index) - 258].clone()
                } else {
                    ".undefined".to_owned()
                }
            })
            .collect();
        return Ok(Some(names));
    }
    if format == 2.5 {
        let mut names = Vec::with_capacity(usize::from(num_glyphs));
        for glyph in 0..i32::from(num_glyphs) {
            let offset = i32::from(data.u8()? as i8);
            let index = glyph + 1 + offset;
            if !(0..258).contains(&index) {
                return Err(Parsing::Unported(
                    "a 'post' table of format 2.5 with a name index out of range".to_owned(),
                ));
            }
            names.push(MAC_GLYPH_NAMES[index as usize].to_owned());
        }
        return Ok(Some(names));
    }
    Ok(None)
}

/// `CmapTable.read`: every subtable, each read as FontBox reads its format.
fn read_cmap(data: &mut Data, cmap: &Table, num_glyphs: u16) -> Result<Vec<CmapSubtable>, Parsing> {
    data.seek(cmap.offset);
    data.u16()?;
    let count = data.u16()?;
    let mut headers = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        headers.push((data.u16()?, data.u16()?, data.u32()? as usize));
    }
    let num_glyphs = u32::from(num_glyphs);
    let mut subtables = Vec::with_capacity(headers.len());
    for (platform_id, encoding_id, offset) in headers {
        data.seek(cmap.offset + offset);
        let format = data.u16()?;
        if format < 8 {
            data.u16()?;
            data.u16()?;
        } else {
            data.u16()?;
            data.u32()?;
            data.u32()?;
        }
        let mut subtable = CmapSubtable {
            platform_id,
            encoding_id,
            ..CmapSubtable::default()
        };
        match format {
            0 => {
                let mapping = data.take(256)?.to_vec();
                subtable.glyph_to_code = vec![GlyphCode::None; 256];
                for (code, &glyph) in mapping.iter().enumerate() {
                    subtable.glyph_to_code[usize::from(glyph)] = GlyphCode::One(code as u32);
                    subtable.code_to_glyph.insert(code as u32, u32::from(glyph));
                }
            }
            4 => read_format4(data, &mut subtable)?,
            6 => {
                let first = u32::from(data.u16()?);
                let entries = data.u16()?;
                if entries > 0 {
                    let mut max_glyph = 0;
                    for index in 0..u32::from(entries) {
                        let glyph = u32::from(data.u16()?);
                        max_glyph = max_glyph.max(glyph);
                        subtable.code_to_glyph.insert(first + index, glyph);
                    }
                    subtable.build_reverse(max_glyph);
                }
            }
            12 => {
                let groups = data.u32()?;
                subtable.glyph_to_code = vec![GlyphCode::None; num_glyphs as usize];
                if num_glyphs > 0 {
                    let mut max_glyph = 0;
                    for _ in 0..groups {
                        let first = data.u32()?;
                        let end = data.u32()?;
                        let start_glyph = data.u32()?;
                        let invalid =
                            |code: u32| code > 0x10FFFF || (0xD800..=0xDFFF).contains(&code);
                        if invalid(first) || (end > 0 && end < first) || invalid(end) {
                            return Err(Parsing::Damaged(Damaged));
                        }
                        let last = if end >= first {
                            Some(end - first)
                        } else {
                            None
                        };
                        for offset in last.into_iter().flat_map(|last| 0..=last) {
                            let glyph = u64::from(start_glyph) + u64::from(offset);
                            if glyph >= u64::from(num_glyphs) {
                                break;
                            }
                            max_glyph = max_glyph.max(glyph as u32);
                            subtable.code_to_glyph.insert(first + offset, glyph as u32);
                        }
                    }
                    subtable.build_reverse(max_glyph);
                }
            }
            14 => {}
            other => {
                return Err(Parsing::Unported(format!(
                    "a cmap subtable of format {other}, which the port does not read"
                )))
            }
        }
        subtables.push(subtable);
    }
    Ok(subtables)
}

/// `CmapSubtable.processSubtype4`.
fn read_format4(data: &mut Data, subtable: &mut CmapSubtable) -> Result<(), Parsing> {
    let seg_count = usize::from(data.u16()? / 2);
    data.take(6)?;
    let mut ends = Vec::with_capacity(seg_count);
    for _ in 0..seg_count {
        ends.push(u32::from(data.u16()?));
    }
    data.u16()?;
    let mut starts = Vec::with_capacity(seg_count);
    for _ in 0..seg_count {
        starts.push(u32::from(data.u16()?));
    }
    let mut deltas = Vec::with_capacity(seg_count);
    for _ in 0..seg_count {
        deltas.push(u32::from(data.u16()?));
    }
    let range_offset_position = data.pos;
    let mut range_offsets = Vec::with_capacity(seg_count);
    for _ in 0..seg_count {
        range_offsets.push(data.u16()? as usize);
    }
    let mut max_glyph = 0;
    for index in 0..seg_count {
        let (start, end) = (starts[index], ends[index]);
        if start == 65535 || end == 65535 {
            continue;
        }
        let delta = deltas[index];
        let range_offset = range_offsets[index];
        let segment_offset = range_offset_position + index * 2 + range_offset;
        for code in start..=end {
            if range_offset == 0 {
                let glyph = (code + delta) & 0xFFFF;
                max_glyph = max_glyph.max(glyph);
                subtable.code_to_glyph.insert(code, glyph);
            } else {
                data.seek(segment_offset + (code - start) as usize * 2);
                let glyph = u32::from(data.u16()?);
                if glyph != 0 {
                    let glyph = (glyph + delta) & 0xFFFF;
                    max_glyph = max_glyph.max(glyph);
                    subtable.code_to_glyph.insert(code, glyph);
                }
            }
        }
    }
    if !subtable.code_to_glyph.is_empty() {
        subtable.build_reverse(max_glyph);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fallback font has every table FontBox reads, the substitutions of `GSUB` and the
    /// pairs of `kern` among them.
    #[test]
    fn reads_the_bundled_fallback_font() {
        let font = TrueType::read_bundled(include_bytes!(
            "../../../models/pdf/LiberationSans-Regular.ttf"
        ))
        .unwrap();
        assert_eq!(font.units_per_em, 2048);
        assert_eq!(font.advance_width(font.name_to_glyph("space")), 569);
        assert_ne!(font.name_to_glyph("A"), 0);
        assert_eq!(font.name_to_glyph("uni0041"), font.name_to_glyph("A"));
    }

    #[test]
    fn reads_uni_names() {
        assert_eq!(parse_uni_name("uni0041"), Some(0x41));
        assert_eq!(parse_uni_name("uniD800"), None);
        assert_eq!(parse_uni_name("uni004"), None);
        assert_eq!(parse_uni_name("u0041"), None);
    }

    #[test]
    fn a_program_without_tables_is_damaged() {
        let bytes = [0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(matches!(TrueType::read(&bytes), Read::Damaged(_)));
        assert!(matches!(TrueType::read(&bytes[..4]), Read::Damaged(_)));
    }
}
