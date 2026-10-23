//! The parts of a TrueType font program PDFBox's text extraction reads, read as FontBox's
//! `TTFParser` reads them: the table directory, `head`, `hhea`, `maxp`, `hmtx`, `loca`, `post`
//! and the `cmap` subtables, each mapped character to glyph as FontBox maps it. A font FontBox
//! fails to read is damaged to PDFBox, and fails here where it fails there.

use super::Unported;
use crate::hash::FastMap;
use crate::text::charset::Charset;

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
/// `NameRecord.PLATFORM_ISO`.
const PLATFORM_ISO: u16 = 2;
pub(super) const ENCODING_UNICODE_1_0: u16 = 0;
pub(super) const ENCODING_UNICODE_1_1: u16 = 1;
pub(super) const ENCODING_UNICODE_2_0_BMP: u16 = 3;
pub(super) const ENCODING_UNICODE_2_0_FULL: u16 = 4;
pub(super) const ENCODING_MAC_ROMAN: u16 = 0;
pub(super) const ENCODING_WIN_SYMBOL: u16 = 0;
pub(super) const ENCODING_WIN_UNICODE_BMP: u16 = 1;
pub(super) const ENCODING_WIN_UNICODE_FULL: u16 = 10;

/// Why FontBox fails to read a font program, which makes the font damaged to PDFBox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Damaged;

/// Why FontBox's `TTFParser` fails to read a font program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failure {
    /// An `IOException` reading or checking its tables.
    Damaged(Damaged),
    /// `parseTables`' `IOException` for a `CFF ` table, "True Type fonts using CFF outlines are
    /// not supported".
    PostScript,
    /// The `ArrayIndexOutOfBoundsException` of a format 8 or 13 cmap subtable that maps a code to
    /// the glyph one past the last, which `glyphIdToCharacterCode` has no room for.
    GlyphOutOfBounds { glyph: u32 },
}

impl From<Damaged> for Failure {
    fn from(damaged: Damaged) -> Self {
        Self::Damaged(damaged)
    }
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
    code_to_glyph: FastMap<u32, u32>,
    /// `glyphIdToCharacterCode`: a glyph's code, or none, or several (`Integer.MIN_VALUE`).
    glyph_to_code: Vec<GlyphCode>,
    glyph_to_codes: FastMap<u32, Vec<u32>>,
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

    /// `CmapSubtable.getCharCodes(glyph)[0]`: the least of the codes mapped to `glyph`.
    pub(super) fn first_code(&self, glyph: u32) -> Option<u32> {
        match self.glyph_to_code.get(glyph as usize)? {
            GlyphCode::None => None,
            GlyphCode::One(code) => Some(*code),
            GlyphCode::Several => self.glyph_to_codes.get(&glyph)?.iter().min().copied(),
        }
    }

    /// Maps `code` to `glyph` and `glyph` back to `code` alone, as the formats that write
    /// `glyphIdToCharacterCode` directly do.
    fn map(&mut self, code: u32, glyph: u32) -> Result<(), Failure> {
        let slot = self
            .glyph_to_code
            .get_mut(glyph as usize)
            .ok_or(Failure::GlyphOutOfBounds { glyph })?;
        *slot = GlyphCode::One(code);
        self.code_to_glyph.insert(code, glyph);
        Ok(())
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

/// The `head` table's version and dates, as FontBox's `HeaderTable` reads them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Header {
    /// `read32Fixed`, in `float` arithmetic.
    pub(crate) version: f32,
    /// Seconds since 1904-01-01, as `readInternationalDate` reads them.
    pub(crate) created: i64,
    pub(crate) modified: i64,
}

/// A record of the `name` table, as FontBox's `NamingTable` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameRecord {
    pub(crate) name_id: u16,
    /// The string, decoded as its platform and encoding say; `None` where its offset lies past
    /// the table.
    pub(crate) string: Option<String>,
}

/// A TrueType font program, as far as text extraction and Tika's `TrueTypeParser` read it.
#[derive(Debug)]
pub(crate) struct TrueType {
    pub(crate) header: Header,
    /// The name records in the table's order; empty without a `name` table.
    pub(crate) names: Vec<NameRecord>,
    pub(super) units_per_em: u16,
    bbox: [i16; 4],
    pub(super) num_glyphs: u16,
    advance_widths: Vec<u16>,
    post_names: Option<Vec<Option<String>>>,
    names_to_glyphs: FastMap<String, u32>,
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
    fn i64(&mut self) -> Result<i64, Damaged> {
        let bytes = self.take(8)?;
        Ok(i64::from_be_bytes(bytes.try_into().expect("eight bytes")))
    }

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
    pub(super) fn read(bytes: &[u8]) -> Result<Self, Failure> {
        Self::parse(bytes, Source::Embedded)
    }

    /// `new TTFParser().parse` of a font file, as Tika's `TrueTypeParser` reads one.
    pub(crate) fn read_file(bytes: &[u8]) -> Result<Self, Failure> {
        Self::parse(bytes, Source::Bundled)
    }

    /// `new TTFParser().parse` of a font PDFBox bundles, whose tables FontBox is known to read.
    pub(super) fn read_bundled(bytes: &[u8]) -> Result<Self, Unported> {
        Self::parse(bytes, Source::Bundled)
            .map_err(|_| Unported("a font PDFBox bundles that the port fails to read".to_owned()))
    }

    fn parse(bytes: &[u8], source: Source) -> Result<Self, Failure> {
        let embedded = source == Source::Embedded;
        let mut data = Data { bytes, pos: 0 };
        data.u32()?;
        let number_of_tables = data.u16()?;
        data.take(6)?;
        let mut tables: FastMap<[u8; 4], Table> = FastMap::default();
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
        let postscript = tables.contains_key(b"CFF ");
        let table = |tag: &[u8; 4]| tables.get(tag).copied();
        let mandatory = |tag: &[u8; 4]| match table(tag) {
            Some(table) => Ok(table),
            None if postscript => Err(Failure::PostScript),
            None => Err(Failure::Damaged(Damaged)),
        };
        let head = table(b"head").ok_or(Failure::Damaged(Damaged))?;
        data.seek(head.offset);
        let version = data.fixed()?;
        data.take(14)?;
        let units_per_em = data.u16()?;
        let header = Header {
            version,
            created: data.i64()?,
            modified: data.i64()?,
        };
        let bbox = [data.i16()?, data.i16()?, data.i16()?, data.i16()?];
        data.take(6)?;
        let index_to_loc_format = data.i16()?;
        let hhea = table(b"hhea").ok_or(Failure::Damaged(Damaged))?;
        data.seek(hhea.offset);
        data.take(34)?;
        let number_of_h_metrics = data.u16()?;
        let maxp = table(b"maxp").ok_or(Failure::Damaged(Damaged))?;
        data.seek(maxp.offset);
        let maxp_version = data.fixed()?;
        let num_glyphs = data.u16()?;
        if maxp_version >= 1.0 {
            data.take(26)?;
        }
        let mut font = TrueType {
            header,
            names: Vec::new(),
            units_per_em,
            bbox,
            num_glyphs,
            advance_widths: Vec::new(),
            post_names: None,
            names_to_glyphs: FastMap::default(),
            cmaps: None,
        };
        if let Some(post) = table(b"post") {
            font.post_names = read_post(&mut data, &post, num_glyphs)?;
        } else if !embedded {
            return Err(Failure::Damaged(Damaged));
        }
        if let Some(loca) = table(b"loca") {
            data.seek(loca.offset);
            let mut offsets = Vec::with_capacity(usize::from(num_glyphs) + 1);
            for _ in 0..=usize::from(num_glyphs) {
                offsets.push(match index_to_loc_format {
                    0 => u32::from(data.u16()?) * 2,
                    1 => data.u32()?,
                    _ => return Err(Failure::Damaged(Damaged)),
                });
            }
            if num_glyphs == 1 && offsets[0] == 0 && offsets[1] == 0 {
                return Err(Failure::Damaged(Damaged));
            }
        } else {
            mandatory(b"loca")?;
        }
        mandatory(b"glyf")?;
        if !embedded {
            mandatory(b"name")?;
        }
        let hmtx = mandatory(b"hmtx")?;
        data.seek(hmtx.offset);
        for _ in 0..number_of_h_metrics {
            font.advance_widths.push(data.u16()?);
            data.i16()?;
        }
        if let Some(cmap) = table(b"cmap") {
            font.cmaps = Some(read_cmap(&mut data, &cmap, num_glyphs)?);
        } else if !embedded {
            mandatory(b"cmap")?;
        }
        if let Some(names) = &font.post_names {
            for (index, name) in names.iter().enumerate() {
                if let Some(name) = name {
                    font.names_to_glyphs.insert(name.clone(), index as u32);
                }
            }
        }
        if let Some(name) = table(b"name") {
            font.names = read_name(&mut data, name)?;
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
            let count = vertical_metrics.ok_or(Failure::Damaged(Damaged))?;
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
        if postscript {
            return Err(Failure::PostScript);
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
            .and_then(|names| names.get(glyph as usize)?.as_deref())
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
                if let Some(glyph) = crate::java::parse_int(digits) {
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

/// `NamingTable.read`: the name records, and their strings within the table, each decoded with
/// the charset `getCharset` picks for its platform and encoding.
fn read_name(data: &mut Data, name: Table) -> Result<Vec<NameRecord>, Damaged> {
    data.seek(name.offset);
    data.take(2)?;
    let count = usize::from(data.u16()?);
    data.take(2)?;
    let mut headers = Vec::with_capacity(count);
    for _ in 0..count {
        let platform = data.u16()?;
        let encoding = data.u16()?;
        data.u16()?;
        let name_id = data.u16()?;
        let length = usize::from(data.u16()?);
        let offset = usize::from(data.u16()?);
        headers.push((platform, encoding, name_id, offset, length));
    }
    let mut records = Vec::with_capacity(count);
    for (platform, encoding, name_id, offset, length) in headers {
        if offset > name.length {
            records.push(NameRecord {
                name_id,
                string: None,
            });
            continue;
        }
        data.seek(name.offset + 6 + count * 12 + offset);
        let bytes = data.take(length)?;
        let charset = match (platform, encoding) {
            (PLATFORM_WINDOWS, ENCODING_WIN_SYMBOL | ENCODING_WIN_UNICODE_BMP)
            | (PLATFORM_UNICODE, _) => "UTF-16",
            (PLATFORM_ISO, 0) => "US-ASCII",
            (PLATFORM_ISO, 1) => "UTF-16BE",
            _ => "ISO-8859-1",
        };
        let string = Charset::named(charset)
            .decode(bytes)
            .expect("the JDK decoders decode the name table's charsets");
        records.push(NameRecord {
            name_id,
            string: Some(string),
        });
    }
    Ok(records)
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
    use crate::hash::FastSet;

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
        let mut seen = FastSet::default();
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
        let mut read = FastSet::default();
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
) -> Result<Option<Vec<Option<String>>>, Failure> {
    data.seek(post.offset);
    let format = data.fixed()?;
    data.take(28)?;
    if data.pos == data.bytes.len() {
        return Ok(None);
    }
    if format == 1.0 {
        return Ok(Some(
            MAC_GLYPH_NAMES
                .iter()
                .map(|&n| Some(n.to_owned()))
                .collect(),
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
                Some(if index < 258 {
                    MAC_GLYPH_NAMES[usize::from(index)].to_owned()
                } else if let (true, Some(extra)) = (index <= 32767, &extra) {
                    extra[usize::from(index) - 258].clone()
                } else {
                    ".undefined".to_owned()
                })
            })
            .collect();
        return Ok(Some(names));
    }
    if format == 2.5 {
        let mut names = Vec::with_capacity(usize::from(num_glyphs));
        for glyph in 0..i32::from(num_glyphs) {
            let offset = i32::from(data.u8()? as i8);
            let index = glyph + 1 + offset;
            names.push(
                usize::try_from(index)
                    .ok()
                    .and_then(|index| MAC_GLYPH_NAMES.get(index))
                    .map(|&name| name.to_owned()),
            );
        }
        return Ok(Some(names));
    }
    Ok(None)
}

/// `CmapTable.read`: every subtable, each read as FontBox reads its format.
fn read_cmap(data: &mut Data, cmap: &Table, num_glyphs: u16) -> Result<Vec<CmapSubtable>, Failure> {
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
                    subtable.map(code as u32, u32::from(glyph))?;
                }
            }
            2 => read_format2(data, &mut subtable, num_glyphs)?,
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
                            return Err(Failure::Damaged(Damaged));
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
            8 => read_format8(data, &mut subtable, num_glyphs)?,
            10 => {
                let start = u64::from(data.u32()?);
                let count = u64::from(data.u32()?);
                let end = start + count;
                if count > i32::MAX as u64 || end > 0x10FFFF || (0xD800..=0xDFFF).contains(&end) {
                    return Err(Failure::Damaged(Damaged));
                }
            }
            13 => read_format13(data, &mut subtable, num_glyphs)?,
            14 => {}
            _ => return Err(Failure::Damaged(Damaged)),
        }
        subtables.push(subtable);
    }
    Ok(subtables)
}

/// `CmapSubtable.processSubtype2`: a high byte's subheader, then the low bytes' glyphs.
fn read_format2(
    data: &mut Data,
    subtable: &mut CmapSubtable,
    num_glyphs: u32,
) -> Result<(), Failure> {
    let mut keys = Vec::with_capacity(256);
    for _ in 0..256 {
        keys.push(data.u16()?);
    }
    let last = keys
        .iter()
        .map(|&key| usize::from(key / 8))
        .max()
        .unwrap_or(0);
    let mut headers = Vec::with_capacity(last + 1);
    for index in 0..=last {
        let first = u32::from(data.u16()?);
        let count = data.u16()?;
        let delta = i64::from(data.i16()?);
        let range_offset = i64::from(data.u16()?) - (last - index) as i64 * 8 - 2;
        headers.push((first, count, delta, range_offset));
    }
    let glyphs = data.pos as i64;
    subtable.glyph_to_code = vec![GlyphCode::None; num_glyphs as usize];
    if num_glyphs == 0 {
        return Ok(());
    }
    for (high, (first, count, delta, range_offset)) in (0u32..).zip(headers) {
        let position = usize::try_from(glyphs + range_offset).map_err(|_| Damaged)?;
        data.seek(position);
        for low in 0..u32::from(count) {
            let code = (high << 8) + first + low;
            let glyph = match i64::from(data.u16()?) {
                0 => 0,
                glyph => (glyph + delta).rem_euclid(65536) as u32,
            };
            if glyph < num_glyphs {
                subtable.map(code, glyph)?;
            }
        }
    }
    Ok(())
}

/// `CmapSubtable.processSubtype8`: groups of mixed 16-bit and 32-bit codes.
fn read_format8(
    data: &mut Data,
    subtable: &mut CmapSubtable,
    num_glyphs: u32,
) -> Result<(), Failure> {
    let is32 = data.take(8192)?.to_vec();
    let groups = data.u32()?;
    if groups > 65536 {
        return Err(Failure::Damaged(Damaged));
    }
    subtable.glyph_to_code = vec![GlyphCode::None; num_glyphs as usize];
    if num_glyphs == 0 {
        return Ok(());
    }
    for _ in 0..groups {
        let first = u64::from(data.u32()?);
        let end = u64::from(data.u32()?);
        let start_glyph = u64::from(data.u32()?);
        if first > end {
            return Err(Failure::Damaged(Damaged));
        }
        for index in first..=end {
            let byte = match usize::try_from(index) {
                Ok(index) if index <= i32::MAX as usize => is32.get(index / 8),
                _ => None,
            }
            .ok_or(Damaged)?;
            let code = if byte & (1 << (index % 8)) == 0 {
                index
            } else {
                let lead = 0xD7C0 + (index >> 10);
                let trail = 0xDC00 + (index & 0x3FF);
                (lead << 10) + trail - 0x35FDC00
            };
            let glyph = start_glyph + (index - first);
            if code > i32::MAX as u64 || glyph > u64::from(num_glyphs) {
                return Err(Failure::Damaged(Damaged));
            }
            subtable.map(code as u32, glyph as u32)?;
        }
    }
    Ok(())
}

/// `CmapSubtable.processSubtype13`: ranges of codes that each map to one glyph.
fn read_format13(
    data: &mut Data,
    subtable: &mut CmapSubtable,
    num_glyphs: u32,
) -> Result<(), Failure> {
    let groups = data.u32()?;
    subtable.glyph_to_code = vec![GlyphCode::None; num_glyphs as usize];
    if num_glyphs == 0 {
        return Ok(());
    }
    let invalid = |code: u32| code > 0x10FFFF || (0xD800..=0xDFFF).contains(&code);
    for _ in 0..groups {
        let first = data.u32()?;
        let end = data.u32()?;
        let glyph = data.u32()?;
        if glyph > num_glyphs {
            break;
        }
        if invalid(first) || (end > 0 && end < first) || invalid(end) {
            return Err(Failure::Damaged(Damaged));
        }
        for code in first..=end {
            subtable.map(code, glyph)?;
        }
    }
    Ok(())
}

/// `CmapSubtable.processSubtype4`.
fn read_format4(data: &mut Data, subtable: &mut CmapSubtable) -> Result<(), Failure> {
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

/// Font programs of four glyphs, each table as small as FontBox reads it, for tests.
#[cfg(test)]
pub(crate) mod programs {
    /// The number of glyphs of each program.
    pub(crate) const GLYPHS: u16 = 4;

    /// A program of the mandatory tables, `replaced` replacing or adding tables and an empty
    /// table dropping one.
    pub(crate) fn program(replaced: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![
            (*b"head", head()),
            (*b"hhea", [vec![0; 34], be(GLYPHS)].concat()),
            (*b"maxp", [vec![0, 0, 0x50, 0], be(GLYPHS)].concat()),
            (*b"post", [vec![0, 3, 0, 0], vec![0; 28]].concat()),
            (*b"loca", vec![0; 2 * (usize::from(GLYPHS) + 1)]),
            (*b"glyf", vec![0; 4]),
            (*b"name", vec![0, 0, 0, 0, 0, 6]),
            (
                *b"hmtx",
                [500u16, 0]
                    .repeat(usize::from(GLYPHS))
                    .iter()
                    .flat_map(|v| v.to_be_bytes())
                    .collect(),
            ),
            (*b"cmap", cmap(&[(3, 1, format4())])),
        ];
        for (tag, bytes) in replaced {
            tables.retain(|(kept, _)| kept != *tag);
            if !bytes.is_empty() {
                tables.push((**tag, bytes.clone()));
            }
        }
        let count = tables.len() as u16;
        let mut out = [vec![0, 1, 0, 0], be(count), vec![0; 6]].concat();
        let mut offset = out.len() + 16 * tables.len();
        let mut data = Vec::new();
        for (tag, bytes) in &tables {
            out.extend_from_slice(tag);
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            data.extend_from_slice(bytes);
            offset += bytes.len();
        }
        out.extend(data);
        out
    }

    /// A `cmap` table of the subtables of the given platforms and encodings.
    fn cmap(subtables: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
        let mut out = [be(0), be(subtables.len() as u16)].concat();
        let mut offset = 4 + 8 * subtables.len();
        for (platform, encoding, subtable) in subtables {
            out.extend([be(*platform), be(*encoding)].concat());
            out.extend_from_slice(&(offset as u32).to_be_bytes());
            offset += subtable.len();
        }
        for (_, _, subtable) in subtables {
            out.extend_from_slice(subtable);
        }
        out
    }

    /// A format 4 subtable mapping A, B and C to glyphs 1, 2 and 3.
    fn format4() -> Vec<u8> {
        format4_range(0x41, 0x43)
    }

    /// A format 4 subtable mapping the codes `first` to `last` to the glyphs from 1 on.
    fn format4_range(first: u16, last: u16) -> Vec<u8> {
        let words: [u16; 16] = [
            4,
            32,
            0,
            4,
            4,
            1,
            0,
            last,
            0xFFFF,
            0,
            first,
            0xFFFF,
            1u16.wrapping_sub(first),
            1,
            0,
            0,
        ];
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }

    /// A program of `glyphs` glyphs, the codes from 32 on mapped to them in order, and a `glyf`
    /// table of `glyf` bytes, the last of its tables.
    pub(crate) fn of_glyphs(glyphs: u16, glyf: usize) -> Vec<u8> {
        let count = usize::from(glyphs);
        program(&[
            (b"hhea", [vec![0; 34], be(glyphs)].concat()),
            (b"maxp", [vec![0, 0, 0x50, 0], be(glyphs)].concat()),
            (b"loca", vec![0; 2 * (count + 1)]),
            (
                b"hmtx",
                [500u16, 0]
                    .repeat(count)
                    .iter()
                    .flat_map(|v| v.to_be_bytes())
                    .collect(),
            ),
            (
                b"cmap",
                cmap(&[(3, 1, format4_range(0x20, 0x20 + glyphs - 2))]),
            ),
            (b"glyf", vec![0; glyf]),
        ])
    }

    /// A program whose `cmap` table has the one (3, 1) subtable `subtable`.
    pub(crate) fn with_cmap(subtable: Vec<u8>) -> Vec<u8> {
        program(&[(b"cmap", cmap(&[(3, 1, subtable)]))])
    }

    /// A format 2 subtable of one subheader, whose `idRangeOffset` counts from itself, mapping
    /// A, B and C to glyphs 1, 2 and 3.
    pub(crate) fn format2() -> Vec<u8> {
        let words = [0x41u16, 3, 0, 2, 1, 2, 3];
        [
            be(2),
            be(0),
            be(0),
            vec![0; 512],
            words.iter().flat_map(|w| w.to_be_bytes()).collect(),
        ]
        .concat()
    }

    /// A format 8 subtable of 16-bit codes, mapping A, B and C to glyphs 1, 2 and 3.
    pub(crate) fn format8() -> Vec<u8> {
        let groups = [1u32, 0x41, 0x43, 1];
        [
            long_subtable(8, &[]),
            vec![0; 8192],
            groups.iter().flat_map(|v| v.to_be_bytes()).collect(),
        ]
        .concat()
    }

    /// A subtable of `format` 8 or more, whose header has a 32-bit length and language.
    pub(crate) fn long_subtable(format: u16, body: &[u32]) -> Vec<u8> {
        let mut out = [be(format), be(0), vec![0; 8]].concat();
        out.extend(body.iter().flat_map(|value| value.to_be_bytes()));
        out
    }

    fn be(value: u16) -> Vec<u8> {
        value.to_be_bytes().to_vec()
    }

    fn head() -> Vec<u8> {
        let mut out = vec![
            0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0x5F, 0x0F, 0x3C, 0xF5, 0, 0,
        ];
        out.extend(be(1000));
        out.extend([0; 16]);
        out.extend([0, 0, 0, 0, 3, 0xE8, 3, 0xE8]);
        out.extend([0; 10]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::programs::{format2, format8, long_subtable, program, with_cmap, GLYPHS};
    use super::*;

    fn subtable(format: Vec<u8>) -> Result<CmapSubtable, Failure> {
        let mut font = TrueType::read_file(&with_cmap(format))?;
        Ok(font.cmaps.take().unwrap().remove(0))
    }

    #[test]
    fn reads_a_format_2_subtable() {
        let format = format2();
        let cmap = subtable(format).unwrap();
        assert_eq!(
            (cmap.glyph(0x41), cmap.glyph(0x43), cmap.glyph(0x44)),
            (1, 3, 0)
        );
        assert_eq!(cmap.first_code(2), Some(0x42));
    }

    #[test]
    fn reads_a_format_8_subtable() {
        let format = format8();
        let cmap = subtable(format).unwrap();
        assert_eq!((cmap.glyph(0x41), cmap.glyph(0x43)), (1, 3));
        assert_eq!(cmap.first_code(3), Some(0x43));
    }

    /// Format 10 is checked and its glyphs left unread, as FontBox leaves them.
    #[test]
    fn checks_a_format_10_subtable() {
        let cmap = subtable(long_subtable(10, &[0x41, 3])).unwrap();
        assert_eq!((cmap.glyph(0x41), cmap.first_code(1)), (0, None));
        assert_eq!(
            subtable(long_subtable(10, &[0xD7FE, 3])).unwrap_err(),
            Failure::Damaged(Damaged)
        );
    }

    /// Format 13 maps a range to one glyph, which keeps the range's last code; a glyph one past
    /// the last is FontBox's `ArrayIndexOutOfBoundsException`, one further ends the groups.
    #[test]
    fn reads_a_format_13_subtable() {
        let cmap = subtable(long_subtable(13, &[1, 0x41, 0x43, 2])).unwrap();
        assert_eq!((cmap.glyph(0x41), cmap.glyph(0x43)), (2, 2));
        assert_eq!(cmap.first_code(2), Some(0x43));
        let past = u32::from(GLYPHS);
        assert_eq!(
            subtable(long_subtable(13, &[1, 0x41, 0x41, past])).unwrap_err(),
            Failure::GlyphOutOfBounds { glyph: past }
        );
        let ended = subtable(long_subtable(13, &[2, 0x41, 0x41, past + 1, 0x42, 0x42, 1])).unwrap();
        assert_eq!(ended.glyph(0x42), 0);
    }

    /// A glyph's first code is the first of its codes sorted, as `getCharCodes` sorts them.
    #[test]
    fn gives_a_glyph_the_least_of_its_codes() {
        let mut cmap = CmapSubtable::default();
        let mappings = [
            (0x41, 1),
            (0x10FFFF, 2),
            (0x42, 2),
            (0x20, 2),
            (0x300, 3),
            (0x30, 3),
            (0, 4),
            (0xFFFF, 4),
            (0x61, 6),
        ];
        for (code, glyph) in mappings {
            cmap.code_to_glyph.insert(code, glyph);
        }
        cmap.build_reverse(6);
        for glyph in 0..10 {
            let codes = match cmap.glyph_to_code.get(glyph as usize) {
                None | Some(GlyphCode::None) => None,
                Some(GlyphCode::One(code)) => Some(vec![*code]),
                Some(GlyphCode::Several) => cmap.glyph_to_codes.get(&glyph).map(|codes| {
                    let mut codes = codes.clone();
                    codes.sort_unstable();
                    codes
                }),
            };
            assert_eq!(
                cmap.first_code(glyph),
                codes.map(|codes| codes[0]),
                "{glyph}"
            );
        }
        assert_eq!(cmap.first_code(2), Some(0x20));
        assert_eq!(cmap.first_code(4), Some(0));
        assert_eq!(cmap.first_code(5), None);
    }

    #[test]
    fn an_unknown_cmap_format_is_damaged() {
        assert_eq!(
            subtable(vec![0, 7, 0, 0, 0, 0]).unwrap_err(),
            Failure::Damaged(Damaged)
        );
    }

    /// A `CFF ` table fails the program once `head`, `hhea`, `maxp` and `post` are there,
    /// whatever else is missing; `CFF2` alone does not.
    #[test]
    fn a_program_of_postscript_outlines_fails() {
        let cff = (b"CFF ", vec![1, 0, 4, 1]);
        for dropped in [b"loca", b"glyf", b"name", b"hmtx", b"cmap", b"OS/2"] {
            let bytes = program(&[cff.clone(), (dropped, Vec::new())]);
            assert_eq!(
                TrueType::read_file(&bytes).unwrap_err(),
                Failure::PostScript
            );
            assert_eq!(TrueType::read(&bytes).unwrap_err(), Failure::PostScript);
        }
        let headless = program(&[cff, (b"post", Vec::new())]);
        assert_eq!(
            TrueType::read_file(&headless).unwrap_err(),
            Failure::Damaged(Damaged)
        );
        assert!(TrueType::read_file(&program(&[(b"CFF2", vec![2, 0, 5, 0, 0])])).is_ok());
    }

    /// A `post` table of format 2.5, which `read32Fixed` reads from 0x00028000 rather than the
    /// specification's 0x00025000, names a glyph whose index falls outside the standard names
    /// with null.
    #[test]
    fn reads_post_format_2_5() {
        let post = [vec![0, 2, 0x80, 0], vec![0; 28], vec![0, 0xF6, 120, 127]].concat();
        let font = TrueType::read(&program(&[(b"post", post)])).unwrap();
        assert_eq!(font.post_name(0), Some(MAC_GLYPH_NAMES[1]));
        assert_eq!(font.post_name(1), None);
        assert_eq!(font.post_name(2), Some(MAC_GLYPH_NAMES[123]));
    }

    /// A TrueType collection is read as `TTFParser` reads any program, from its first bytes.
    #[test]
    fn a_collection_is_read_as_a_program() {
        let collection = [b"ttcf".to_vec(), vec![0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 16]].concat();
        assert_eq!(
            TrueType::read_file(&collection).unwrap_err(),
            Failure::Damaged(Damaged)
        );
    }

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
        assert_eq!(
            TrueType::read(&bytes).unwrap_err(),
            Failure::Damaged(Damaged)
        );
        assert_eq!(
            TrueType::read(&bytes[..4]).unwrap_err(),
            Failure::Damaged(Damaged)
        );
    }
}
