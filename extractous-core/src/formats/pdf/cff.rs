//! FontBox's `CFFParser`: a CFF font program as PDFBox reads it from a document, failing where
//! FontBox fails, with what text extraction reads of it: its encoding, charset, font matrix and
//! bounding box, and the widths its Type 2 charstrings set, as `Type2CharStringParser` and
//! `Type2CharString` work them out.

use std::collections::HashMap;
use std::sync::LazyLock;

use super::Unported;

/// FontBox's predefined CFF data, which `just pdf-resources` writes from its jar.
const TABLES_TSV: &str = include_str!("../../../models/pdf/cff.tsv");
/// How deep subroutines may call each other before the port stops where Java would overflow
/// its stack.
const MAX_SUBROUTINE_DEPTH: usize = 64;

/// FontBox's standard strings, predefined charsets and predefined encodings.
struct Tables {
    strings: Vec<String>,
    charsets: HashMap<String, Vec<(u32, u32, String)>>,
    encodings: HashMap<String, Vec<(i32, String)>>,
}

static TABLES: LazyLock<Tables> = LazyLock::new(|| {
    let mut tables = Tables {
        strings: Vec::with_capacity(391),
        charsets: HashMap::new(),
        encodings: HashMap::new(),
    };
    let number = |text: &str| -> u32 {
        text.parse()
            .unwrap_or_else(|e| panic!("models/pdf/cff.tsv has a bad number {text}: {e}"))
    };
    for line in TABLES_TSV.lines().filter(|line| !line.starts_with('#')) {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields[..] {
            ["string", sid, name] => {
                assert_eq!(number(sid) as usize, tables.strings.len());
                tables.strings.push(name.to_owned());
            }
            ["charset", charset, gid, sid, name] => tables
                .charsets
                .entry(charset.to_owned())
                .or_default()
                .push((number(gid), number(sid), name.to_owned())),
            ["encoding", encoding, code, name] => tables
                .encodings
                .entry(encoding.to_owned())
                .or_default()
                .push((number(code) as i32, name.to_owned())),
            _ => panic!("models/pdf/cff.tsv has a malformed line: {line}"),
        }
    }
    tables
});

/// What reading a CFF program gives PDFBox.
#[derive(Debug)]
pub(super) enum Read {
    Font(Box<Cff>),
    /// The program fails to parse, which makes PDFBox call the font damaged.
    Damaged,
    Unported(String),
}

/// Why parsing stopped: an `IOException`, or an exception PDFBox does not catch.
enum Failure {
    Damaged,
    Uncaught(&'static str),
}

type Parsed<T> = Result<T, Failure>;

/// A DICT operand: an `Integer` or a `Double`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Number {
    Integer(i32),
    Real(f64),
}

impl Number {
    fn int_value(self) -> i32 {
        match self {
            Number::Integer(value) => value,
            Number::Real(value) => value as i32,
        }
    }

    pub(super) fn float_value(self) -> f32 {
        match self {
            Number::Integer(value) => value as f32,
            Number::Real(value) => value as f32,
        }
    }

    fn double_value(self) -> f64 {
        match self {
            Number::Integer(value) => f64::from(value),
            Number::Real(value) => value,
        }
    }
}

/// `DataInputByteArray`.
struct Input<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Input<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn has_remaining(&self) -> bool {
        self.position < self.bytes.len()
    }

    fn set_position(&mut self, position: i32) -> Parsed<()> {
        if position < 0 || position as usize >= self.bytes.len() {
            return Err(Failure::Damaged);
        }
        self.position = position as usize;
        Ok(())
    }

    fn u8(&mut self) -> Parsed<u8> {
        let byte = *self.bytes.get(self.position).ok_or(Failure::Damaged)?;
        self.position += 1;
        Ok(byte)
    }

    fn u16(&mut self) -> Parsed<u16> {
        Ok(u16::from(self.u8()?) << 8 | u16::from(self.u8()?))
    }

    fn i32(&mut self) -> Parsed<i32> {
        let mut value = 0u32;
        for _ in 0..4 {
            value = value << 8 | u32::from(self.u8()?);
        }
        Ok(value as i32)
    }

    fn offset(&mut self, size: u8) -> Parsed<i32> {
        let mut value = 0i32;
        for _ in 0..size {
            value = value.wrapping_shl(8) | i32::from(self.u8()?);
        }
        Ok(value)
    }

    fn bytes(&mut self, length: i32) -> Parsed<&'a [u8]> {
        if length < 0 || self.bytes.len() - self.position < length as usize {
            return Err(Failure::Damaged);
        }
        let start = self.position;
        self.position += length as usize;
        Ok(&self.bytes[start..self.position])
    }
}

fn read_off_size(input: &mut Input) -> Parsed<u8> {
    let size = input.u8()?;
    if !(1..=4).contains(&size) {
        return Err(Failure::Damaged);
    }
    Ok(size)
}

/// `readIndexData`.
fn read_index<'a>(input: &mut Input<'a>) -> Parsed<Vec<&'a [u8]>> {
    let count = usize::from(input.u16()?);
    if count == 0 {
        return Ok(Vec::new());
    }
    let size = read_off_size(input)?;
    let mut offsets = Vec::with_capacity(count + 1);
    for _ in 0..=count {
        let offset = input.offset(size)?;
        if offset > input.bytes.len() as i32 {
            return Err(Failure::Damaged);
        }
        offsets.push(offset);
    }
    offsets
        .windows(2)
        .map(|pair| input.bytes(pair[1].wrapping_sub(pair[0])))
        .collect()
}

/// `readStringIndexData`.
fn read_string_index(input: &mut Input) -> Parsed<Vec<String>> {
    Ok(read_index(input)?
        .into_iter()
        .map(|bytes| bytes.iter().map(|&byte| char::from(byte)).collect())
        .collect())
}

/// A DICT entry's operands.
type Entry = Vec<Number>;

/// `DictData`: the entries by operator name, the last of a name kept.
#[derive(Debug, Default)]
struct Dict {
    entries: HashMap<&'static str, Entry>,
}

impl Dict {
    fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.get(name)
    }

    /// An entry with operands.
    fn operands(&self, name: &str) -> Option<&Entry> {
        self.entry(name).filter(|entry| !entry.is_empty())
    }

    fn number(&self, name: &str) -> Option<Number> {
        self.operands(name).map(|entry| entry[0])
    }
}

/// `CFFOperator.getOperator`.
fn operator(b0: u8, b1: Option<u8>) -> Option<&'static str> {
    Some(match (b0, b1) {
        (0, None) => "version",
        (1, None) => "Notice",
        (12, Some(0)) => "Copyright",
        (2, None) => "FullName",
        (3, None) => "FamilyName",
        (4, None) => "Weight",
        (12, Some(1)) => "isFixedPitch",
        (12, Some(2)) => "ItalicAngle",
        (12, Some(3)) => "UnderlinePosition",
        (12, Some(4)) => "UnderlineThickness",
        (12, Some(5)) => "PaintType",
        (12, Some(6)) => "CharstringType",
        (12, Some(7)) => "FontMatrix",
        (13, None) => "UniqueID",
        (5, None) => "FontBBox",
        (12, Some(8)) => "StrokeWidth",
        (14, None) => "XUID",
        (15, None) => "charset",
        (16, None) => "Encoding",
        (17, None) => "CharStrings",
        (18, None) => "Private",
        (12, Some(20)) => "SyntheticBase",
        (12, Some(21)) => "PostScript",
        (12, Some(22)) => "BaseFontName",
        (12, Some(23)) => "BaseFontBlend",
        (12, Some(30)) => "ROS",
        (12, Some(31)) => "CIDFontVersion",
        (12, Some(32)) => "CIDFontRevision",
        (12, Some(33)) => "CIDFontType",
        (12, Some(34)) => "CIDCount",
        (12, Some(35)) => "UIDBase",
        (12, Some(36)) => "FDArray",
        (12, Some(37)) => "FDSelect",
        (12, Some(38)) => "FontName",
        (6, None) => "BlueValues",
        (7, None) => "OtherBlues",
        (8, None) => "FamilyBlues",
        (9, None) => "FamilyOtherBlues",
        (12, Some(9)) => "BlueScale",
        (12, Some(10)) => "BlueShift",
        (12, Some(11)) => "BlueFuzz",
        (10, None) => "StdHW",
        (11, None) => "StdVW",
        (12, Some(12)) => "StemSnapH",
        (12, Some(13)) => "StemSnapV",
        (12, Some(14)) => "ForceBold",
        (12, Some(15)) => "LanguageGroup",
        (12, Some(16)) => "ExpansionFactor",
        (12, Some(17)) => "initialRandomSeed",
        (19, None) => "Subrs",
        (20, None) => "defaultWidthX",
        (21, None) => "nominalWidthX",
        _ => return None,
    })
}

/// `readEntry`: operands up to their operator, whose name is `None` for one FontBox does not
/// know.
fn read_entry(input: &mut Input) -> Parsed<(Option<&'static str>, Entry)> {
    let mut operands = Vec::new();
    loop {
        let b0 = input.u8()?;
        match b0 {
            0..=21 => {
                let name = if b0 == 12 {
                    operator(12, Some(input.u8()?))
                } else {
                    operator(b0, None)
                };
                return Ok((name, operands));
            }
            28 => operands.push(Number::Integer(i32::from(input.u16()? as i16))),
            29 => operands.push(Number::Integer(input.i32()?)),
            30 => operands.push(Number::Real(read_real(input)?)),
            32..=246 => operands.push(Number::Integer(i32::from(b0) - 139)),
            247..=250 => {
                let b1 = i32::from(input.u8()?);
                operands.push(Number::Integer((i32::from(b0) - 247) * 256 + b1 + 108));
            }
            251..=254 => {
                let b1 = i32::from(input.u8()?);
                operands.push(Number::Integer(-(i32::from(b0) - 251) * 256 - b1 - 108));
            }
            _ => return Err(Failure::Damaged),
        }
    }
}

/// `readRealNumber`.
fn read_real(input: &mut Input) -> Parsed<f64> {
    let mut text = String::new();
    let mut exponent_missing = false;
    let mut has_exponent = false;
    let mut done = false;
    while !done {
        let byte = input.u8()?;
        for nibble in [byte / 16, byte % 16] {
            match nibble {
                0..=9 => {
                    text.push(char::from(b'0' + nibble));
                    exponent_missing = false;
                }
                0xa => text.push('.'),
                0xb if !has_exponent => {
                    text.push('E');
                    exponent_missing = true;
                    has_exponent = true;
                }
                0xc if !has_exponent => {
                    text.push_str("E-");
                    exponent_missing = true;
                    has_exponent = true;
                }
                0xe => text.push('-'),
                0xf => done = true,
                _ => {}
            }
        }
    }
    if exponent_missing {
        text.push('0');
    }
    if text.is_empty() {
        return Ok(0.0);
    }
    java_parse_double(&text).ok_or(Failure::Damaged)
}

/// `Double.valueOf` of the digits, points, signs and exponents a real number's nibbles spell.
fn java_parse_double(text: &str) -> Option<f64> {
    let (mantissa, exponent) = match text.split_once('E') {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (text, None),
    };
    let unsigned = mantissa.strip_prefix('-').unwrap_or(mantissa);
    let digits = unsigned.chars().filter(char::is_ascii_digit).count();
    if digits == 0 || unsigned.chars().filter(|&c| c == '.').count() > 1 || unsigned.contains('-') {
        return None;
    }
    if let Some(exponent) = exponent {
        let unsigned = exponent.strip_prefix('-').unwrap_or(exponent);
        if unsigned.is_empty() || !unsigned.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
    }
    text.parse().ok()
}

/// `readDictData` of a whole DICT.
fn read_dict(bytes: &[u8]) -> Parsed<Dict> {
    let mut input = Input::new(bytes);
    let mut dict = Dict::default();
    while input.has_remaining() {
        let (name, operands) = read_entry(&mut input)?;
        if let Some(name) = name {
            dict.entries.insert(name, operands);
        }
    }
    Ok(dict)
}

/// `readDictData(input, offset, dictSize)`.
fn read_dict_at(input: &mut Input, offset: i32, size: i32) -> Parsed<Dict> {
    let mut dict = Dict::default();
    if size > 0 {
        input.set_position(offset)?;
        let end = i64::from(offset) + i64::from(size);
        while (input.position as i64) < end {
            let (name, operands) = read_entry(input)?;
            if let Some(name) = name {
                dict.entries.insert(name, operands);
            }
        }
    }
    Ok(dict)
}

/// A Type 1-keyed font's charset: SIDs and names by glyph.
#[derive(Debug, Default, Clone)]
struct Charset {
    sid_to_gid: HashMap<u32, u32>,
    gid_to_sid: HashMap<u32, u32>,
    name_to_sid: HashMap<String, u32>,
}

impl Charset {
    fn add(&mut self, gid: u32, sid: u32, name: String) {
        self.sid_to_gid.insert(sid, gid);
        self.gid_to_sid.insert(gid, sid);
        self.name_to_sid.insert(name, sid);
    }

    fn predefined(name: &str) -> Charset {
        let mut charset = Charset::default();
        for (gid, sid, glyph) in &TABLES.charsets[name] {
            charset.add(*gid, *sid, glyph.clone());
        }
        charset
    }

    fn sid_for_gid(&self, gid: u32) -> u32 {
        self.gid_to_sid.get(&gid).copied().unwrap_or(0)
    }

    /// `nameToGID`: the glyph of a name, through its SID; 0 for a name the font lacks.
    fn name_to_gid(&self, name: &str) -> u32 {
        let sid = self.name_to_sid.get(name).copied().unwrap_or(0);
        self.sid_to_gid.get(&sid).copied().unwrap_or(0)
    }
}

/// A CID-keyed font's charset: CIDs by glyph, through ranges where the charset has them.
#[derive(Debug, Default)]
struct CidCharset {
    cid_to_gid: HashMap<u32, u32>,
    gid_to_cid: HashMap<u32, u32>,
    /// `(first glyph, first CID, glyphs after the first)`.
    ranges: Vec<(u32, u32, u32)>,
}

impl CidCharset {
    fn add(&mut self, gid: u32, cid: u32) {
        self.cid_to_gid.insert(cid, gid);
        self.gid_to_cid.insert(gid, cid);
    }

    /// `getGIDForCID`.
    fn gid_for_cid(&self, cid: u32) -> u32 {
        for &(first_gid, first_cid, left) in &self.ranges {
            if cid >= first_cid && cid <= first_cid + left {
                return first_gid + (cid - first_cid);
            }
        }
        self.cid_to_gid.get(&cid).copied().unwrap_or(0)
    }
}

/// The DICT values of a Private DICT a width reads, and its local subroutines.
#[derive(Debug, Default, Clone)]
struct Private {
    default_width_x: i32,
    nominal_width_x: i32,
    subrs: Option<Vec<Vec<u8>>>,
}

/// `FDSelect`: the font DICT of each glyph.
#[derive(Debug)]
enum FdSelect {
    /// The font DICT of each glyph in turn.
    Format0(Vec<u8>),
    /// `(first glyph, font DICT)` ranges, and the glyph after the last.
    Format3(Vec<(u16, u8)>, u16),
}

impl FdSelect {
    /// `getFDIndex`: the glyph's font DICT, -1 past the last range.
    fn index(&self, gid: u32) -> i32 {
        match self {
            FdSelect::Format0(fds) => fds.get(gid as usize).map_or(0, |&fd| i32::from(fd)),
            FdSelect::Format3(ranges, sentinel) => {
                for (index, &(first, fd)) in ranges.iter().enumerate() {
                    if u32::from(first) <= gid {
                        match ranges.get(index + 1) {
                            Some(&(next, _)) => {
                                if u32::from(next) > gid {
                                    return i32::from(fd);
                                }
                            }
                            None => {
                                return if u32::from(*sentinel) > gid {
                                    i32::from(fd)
                                } else {
                                    -1
                                };
                            }
                        }
                    }
                }
                0
            }
        }
    }
}

/// How a CFF font's glyphs are keyed.
#[derive(Debug)]
enum Keyed {
    Type1 {
        charset: Charset,
        /// The codes and names the encoding adds, in the order it adds them.
        encoding: Vec<(i32, String)>,
        private: Private,
    },
    Cid {
        charset: CidCharset,
        fd_select: FdSelect,
        /// The Private DICT of each font DICT, `None` for one without.
        privates: Vec<Option<Private>>,
    },
}

/// A parsed CFF font: the first of a program, as PDFBox reads it.
#[derive(Debug)]
pub(super) struct Cff {
    pub(super) font_matrix: Vec<Number>,
    pub(super) font_bbox: Vec<Number>,
    keyed: Keyed,
    charstrings: Vec<Vec<u8>>,
    global_subrs: Vec<Vec<u8>>,
}

/// `CFFParser.parse`: the font program's first font, FontBox having read them all.
pub(super) fn read(bytes: &[u8]) -> Read {
    match parse(bytes) {
        Ok(font) => Read::Font(Box::new(font)),
        Err(Failure::Damaged) => Read::Damaged,
        Err(Failure::Uncaught(reason)) => Read::Unported(reason.to_owned()),
    }
}

fn parse(bytes: &[u8]) -> Parsed<Cff> {
    let mut input = Input::new(bytes);
    let tag = input.bytes(4)?;
    let owned;
    match tag {
        b"OTTO" => {
            owned = open_type_cff(&mut input)?;
            input = Input::new(owned);
        }
        b"ttcf" | [0, 1, 0, 0] => return Err(Failure::Damaged),
        _ => input.set_position(0)?,
    }
    input.u8()?;
    input.u8()?;
    input.u8()?;
    read_off_size(&mut input)?;
    let names = read_string_index(&mut input)?;
    if names.is_empty() {
        return Err(Failure::Damaged);
    }
    let top_dicts = read_index(&mut input)?;
    if top_dicts.is_empty() {
        return Err(Failure::Damaged);
    }
    let strings = read_string_index(&mut input)?;
    let global_subrs: Vec<Vec<u8>> = read_index(&mut input)?
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect();
    let mut first = None;
    for index in 0..names.len() {
        let top_dict = top_dicts.get(index).ok_or(Failure::Uncaught(
            "a CFF program naming more fonts than it has top DICTs, which FontBox throws on",
        ))?;
        let font = parse_font(&mut input, &strings, top_dict, &global_subrs)?;
        first.get_or_insert(font);
    }
    first.ok_or(Failure::Damaged)
}

/// `createTaggedCFFDataInput`: the `CFF ` table of an OpenType font.
fn open_type_cff<'a>(input: &mut Input<'a>) -> Parsed<&'a [u8]> {
    let tables = input.u16()? as i16;
    input.bytes(6)?;
    for _ in 0..tables {
        let tag = input.bytes(4)?;
        input.bytes(4)?;
        let offset = (i64::from(input.u16()?) << 16 | i64::from(input.u16()?)) as i32;
        let length = (i64::from(input.u16()?) << 16 | i64::from(input.u16()?)) as i32;
        if tag == b"CFF " {
            input.set_position(offset)?;
            return input.bytes(length);
        }
    }
    Err(Failure::Damaged)
}

/// `readString`.
fn string(strings: &[String], index: i32) -> Parsed<String> {
    if index < 0 {
        return Err(Failure::Damaged);
    }
    let index = index as usize;
    if index <= 390 {
        return Ok(TABLES.strings[index].clone());
    }
    Ok(strings
        .get(index - 391)
        .cloned()
        .unwrap_or_else(|| format!("SID{index}")))
}

fn parse_font(
    input: &mut Input,
    strings: &[String],
    top_dict: &[u8],
    global_subrs: &[Vec<u8>],
) -> Parsed<Cff> {
    let top = read_dict(top_dict)?;
    if top.entry("SyntheticBase").is_some() {
        return Err(Failure::Damaged);
    }
    let cid = match top.entry("ROS") {
        Some(ros) => {
            if ros.len() < 3 {
                return Err(Failure::Damaged);
            }
            string(strings, ros[0].int_value())?;
            string(strings, ros[1].int_value())?;
            true
        }
        None => false,
    };
    for key in [
        "version",
        "Notice",
        "Copyright",
        "FullName",
        "FamilyName",
        "Weight",
    ] {
        if let Some(number) = top.number(key) {
            string(strings, number.int_value())?;
        }
    }
    let default_matrix = vec![
        Number::Real(0.001),
        Number::Real(0.0),
        Number::Real(0.0),
        Number::Real(0.001),
        Number::Real(0.0),
        Number::Real(0.0),
    ];
    let mut font_matrix = top
        .operands("FontMatrix")
        .cloned()
        .unwrap_or_else(|| default_matrix.clone());
    let font_bbox = top
        .operands("FontBBox")
        .cloned()
        .unwrap_or_else(|| vec![Number::Integer(0); 4]);
    let charstrings_offset = top.number("CharStrings").ok_or(Failure::Damaged)?;
    input.set_position(charstrings_offset.int_value())?;
    let charstrings: Vec<Vec<u8>> = read_index(input)?.into_iter().map(<[u8]>::to_vec).collect();
    let glyphs = charstrings.len() as u32;
    let keyed = if cid {
        let charset =
            match top.number("charset") {
                Some(id) if glyphs > 0 => {
                    input.set_position(id.int_value())?;
                    read_cid_charset(input, glyphs)?
                }
                Some(_) => return Err(Failure::Uncaught(
                    "a CID-keyed CFF font without glyphs, given a Type 1 charset FontBox fails \
                     on",
                )),
                None => {
                    let mut charset = CidCharset::default();
                    for gid in 0..=glyphs {
                        charset.add(gid, gid);
                    }
                    charset
                }
            };
        let dicts = parse_cid_dicts(input, strings, &top, glyphs)?;
        match (top.operands("FontMatrix").is_some(), dicts.first_matrix) {
            (false, Some(matrix)) => font_matrix = matrix,
            (false, None) => font_matrix = default_matrix,
            (true, Some(matrix)) => font_matrix = concatenate(&font_matrix, &matrix)?,
            (true, None) => {}
        }
        Keyed::Cid {
            charset,
            fd_select: dicts.fd_select,
            privates: dicts.privates,
        }
    } else {
        let charset = match top.number("charset").map(Number::int_value) {
            Some(0) | None => Charset::predefined("ISOAdobe"),
            Some(1) => Charset::predefined("Expert"),
            Some(2) => Charset::predefined("ExpertSubset"),
            Some(id) if glyphs > 0 => {
                input.set_position(id)?;
                read_charset(input, strings, glyphs)?
            }
            Some(_) => {
                let mut charset = Charset::default();
                charset.add(0, 0, ".notdef".to_owned());
                charset
            }
        };
        let encoding = match top.number("Encoding").map_or(0, Number::int_value) {
            0 => TABLES.encodings["Standard"].clone(),
            1 => TABLES.encodings["Expert"].clone(),
            id => {
                input.set_position(id)?;
                read_encoding(input, strings, &charset)?
            }
        };
        let private_entry = top.entry("Private").filter(|entry| entry.len() >= 2);
        let private_entry = private_entry.ok_or(Failure::Damaged)?;
        let private = read_private(input, private_entry)?;
        Keyed::Type1 {
            charset,
            encoding,
            private,
        }
    };
    Ok(Cff {
        font_matrix,
        font_bbox,
        keyed,
        charstrings,
        global_subrs: global_subrs.to_vec(),
    })
}

/// `concatenateMatrix`: the top DICT's matrix times the font DICT's. FontBox computes the
/// second element with `d1` where it wants `d2` (#109).
fn concatenate(destination: &[Number], other: &[Number]) -> Parsed<Vec<Number>> {
    let get = |list: &[Number], index: usize| {
        list.get(index)
            .copied()
            .map(Number::double_value)
            .ok_or(Failure::Uncaught(
                "a CFF font matrix of fewer than six numbers, which FontBox throws on",
            ))
    };
    let (a1, b1, c1, d1, x1, y1) = (
        get(destination, 0)?,
        get(destination, 1)?,
        get(destination, 2)?,
        get(destination, 3)?,
        get(destination, 4)?,
        get(destination, 5)?,
    );
    let (a2, b2, c2, d2, x2, y2) = (
        get(other, 0)?,
        get(other, 1)?,
        get(other, 2)?,
        get(other, 3)?,
        get(other, 4)?,
        get(other, 5)?,
    );
    let mut result = destination.to_vec();
    result[0] = Number::Real(a1 * a2 + b1 * c2);
    result[1] = Number::Real(a1 * b2 + b1 * d2);
    result[2] = Number::Real(c1 * a2 + d1 * c2);
    result[3] = Number::Real(c1 * b2 + d1 * d2);
    result[4] = Number::Real(x1 * a2 + y1 * c2 + x2);
    result[5] = Number::Real(x1 * b2 + y1 * d2 + y2);
    Ok(result)
}

/// What `parseCIDFontDicts` reads: the first font DICT's matrix, if it has one, the Private
/// DICTs and the FDSelect.
struct CidDicts {
    first_matrix: Option<Vec<Number>>,
    privates: Vec<Option<Private>>,
    fd_select: FdSelect,
}

/// `parseCIDFontDicts`.
fn parse_cid_dicts(
    input: &mut Input,
    strings: &[String],
    top: &Dict,
    glyphs: u32,
) -> Parsed<CidDicts> {
    let fd_array = top.number("FDArray").ok_or(Failure::Damaged)?;
    input.set_position(fd_array.int_value())?;
    let font_dicts = read_index(input)?;
    if font_dicts.is_empty() {
        return Err(Failure::Damaged);
    }
    let mut privates = Vec::with_capacity(font_dicts.len());
    let mut first_matrix = None;
    for (index, bytes) in font_dicts.iter().enumerate() {
        let font_dict = read_dict(bytes)?;
        if let Some(name) = font_dict.number("FontName") {
            string(strings, name.int_value())?;
        }
        if index == 0 {
            first_matrix = font_dict.operands("FontMatrix").cloned();
        }
        let private = match font_dict.entry("Private").filter(|entry| entry.len() >= 2) {
            Some(entry) => Some(read_private(input, entry)?),
            None => None,
        };
        privates.push(private);
    }
    if privates.iter().all(Option::is_none) {
        return Err(Failure::Damaged);
    }
    let fd_select = top.number("FDSelect").ok_or(Failure::Damaged)?;
    input.set_position(fd_select.int_value())?;
    let fd_select = match input.u8()? {
        0 => FdSelect::Format0(input.bytes(glyphs as i32)?.to_vec()),
        3 => {
            let count = input.u16()?;
            let mut ranges = Vec::with_capacity(usize::from(count));
            for _ in 0..count {
                ranges.push((input.u16()?, input.u8()?));
            }
            FdSelect::Format3(ranges, input.u16()?)
        }
        _ => {
            return Err(Failure::Uncaught(
                "an FDSelect of a format FontBox throws on",
            ))
        }
    };
    Ok(CidDicts {
        first_matrix,
        privates,
        fd_select,
    })
}

/// A Private DICT, at the offset and of the size its entry gives, and its local subroutines.
fn read_private(input: &mut Input, entry: &Entry) -> Parsed<Private> {
    let size = entry[0].int_value();
    let offset = entry[1].int_value();
    let dict = read_dict_at(input, offset, size)?;
    let mut private = Private {
        default_width_x: dict.number("defaultWidthX").map_or(0, Number::int_value),
        nominal_width_x: dict.number("nominalWidthX").map_or(0, Number::int_value),
        subrs: None,
    };
    if let Some(Number::Integer(subrs)) = dict.number("Subrs") {
        if subrs > 0 {
            input.set_position(offset.wrapping_add(subrs))?;
            private.subrs = Some(read_index(input)?.into_iter().map(<[u8]>::to_vec).collect());
        }
    }
    Ok(private)
}

/// `readCharset` of a Type 1-keyed font.
fn read_charset(input: &mut Input, strings: &[String], glyphs: u32) -> Parsed<Charset> {
    let mut charset = Charset::default();
    charset.add(0, 0, ".notdef".to_owned());
    match input.u8()? {
        0 => {
            for gid in 1..glyphs {
                let sid = u32::from(input.u16()?);
                charset.add(gid, sid, string(strings, sid as i32)?);
            }
        }
        format @ (1 | 2) => {
            let mut gid = 1;
            while gid < glyphs {
                let first = u32::from(input.u16()?);
                let left = if format == 1 {
                    u32::from(input.u8()?)
                } else {
                    u32::from(input.u16()?)
                } + 1;
                for j in 0..left {
                    let sid = first + j;
                    charset.add(gid + j, sid, string(strings, sid as i32)?);
                }
                gid += left;
            }
        }
        _ => return Err(Failure::Damaged),
    }
    Ok(charset)
}

/// `readCharset` of a CID-keyed font.
fn read_cid_charset(input: &mut Input, glyphs: u32) -> Parsed<CidCharset> {
    let mut charset = CidCharset::default();
    charset.add(0, 0);
    match input.u8()? {
        0 => {
            for gid in 1..glyphs {
                charset.add(gid, u32::from(input.u16()?));
            }
        }
        format @ (1 | 2) => {
            let mut gid = 1;
            while gid < glyphs {
                let first = u32::from(input.u16()?);
                let left = if format == 1 {
                    u32::from(input.u8()?)
                } else {
                    u32::from(input.u16()?)
                };
                charset.ranges.push((gid, first, left));
                gid += left + 1;
            }
        }
        _ => return Err(Failure::Damaged),
    }
    Ok(charset)
}

/// `readEncoding`: the codes and names a built-in encoding adds, in order.
fn read_encoding(
    input: &mut Input,
    strings: &[String],
    charset: &Charset,
) -> Parsed<Vec<(i32, String)>> {
    let format = input.u8()?;
    let mut entries = vec![(0, ".notdef".to_owned())];
    match format & 0x7f {
        0 => {
            let codes = u32::from(input.u8()?);
            for gid in 1..=codes {
                let code = i32::from(input.u8()?);
                let sid = charset.sid_for_gid(gid);
                entries.push((code, string(strings, sid as i32)?));
            }
        }
        1 => {
            let ranges = input.u8()?;
            let mut gid = 1;
            for _ in 0..ranges {
                let first = i32::from(input.u8()?);
                let left = i32::from(input.u8()?);
                for j in 0..=left {
                    let sid = charset.sid_for_gid(gid);
                    entries.push((first + j, string(strings, sid as i32)?));
                    gid += 1;
                }
            }
        }
        _ => return Err(Failure::Damaged),
    }
    if format & 0x80 != 0 {
        let supplements = input.u8()?;
        for _ in 0..supplements {
            let code = i32::from(input.u8()?);
            let sid = i32::from(input.u16()?);
            entries.push((code, string(strings, sid)?));
        }
    }
    Ok(entries)
}

/// A token of a Type 2 charstring: a number or an operator.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Token {
    Integer(i32),
    Real(f64),
    Float(f32),
    Operator(u8, Option<u8>),
}

impl Token {
    fn float_value(self) -> Option<f32> {
        match self {
            Token::Integer(value) => Some(value as f32),
            Token::Real(value) => Some(value as f32),
            Token::Float(value) => Some(value),
            Token::Operator(..) => None,
        }
    }
}

/// `Type2CharStringParser.parse`: the charstring's tokens, its subroutines' expanded in place.
struct CharStringParser<'a> {
    global: &'a [Vec<u8>],
    local: Option<&'a [Vec<u8>]>,
    sequence: Vec<Token>,
    hstems: usize,
    vstems: usize,
}

impl CharStringParser<'_> {
    fn trailing_numbers(&self) -> usize {
        self.sequence
            .iter()
            .rev()
            .take_while(|token| !matches!(token, Token::Operator(..)))
            .count()
    }

    fn parse(&mut self, bytes: &[u8], depth: usize) -> Result<(), Unported> {
        if depth > MAX_SUBROUTINE_DEPTH {
            return Err(Unported(
                "a Type 2 charstring whose subroutines recurse, which FontBox overflows on"
                    .to_owned(),
            ));
        }
        let damaged =
            || Unported("a Type 2 charstring that ends early, which FontBox throws on".to_owned());
        let mut index = 0;
        while index < bytes.len() {
            let b0 = bytes[index];
            index += 1;
            match b0 {
                10 | 29 => {
                    let subrs = if b0 == 10 {
                        self.local
                    } else {
                        Some(self.global)
                    };
                    if let Some(subrs) = subrs.filter(|subrs| !subrs.is_empty()) {
                        let Some(Token::Integer(operand)) = self.sequence.pop() else {
                            return Err(Unported(
                                "a Type 2 subroutine called with an operand FontBox cannot cast"
                                    .to_owned(),
                            ));
                        };
                        let bias = if subrs.len() < 1240 {
                            107
                        } else if subrs.len() < 33900 {
                            1131
                        } else {
                            32768
                        };
                        let number = bias + operand;
                        let subr = usize::try_from(number)
                            .ok()
                            .and_then(|number| subrs.get(number))
                            .ok_or_else(|| {
                                Unported(
                                    "a Type 2 subroutine past the index, which FontBox throws on"
                                        .to_owned(),
                                )
                            })?;
                        self.parse(subr, depth + 1)?;
                        match self.sequence.last() {
                            Some(Token::Operator(11, None)) => {
                                self.sequence.pop();
                            }
                            Some(_) => {}
                            None => {
                                return Err(Unported(
                                    "an empty Type 2 subroutine, which FontBox throws on"
                                        .to_owned(),
                                ))
                            }
                        }
                    }
                }
                19 | 20 => {
                    self.vstems += self.trailing_numbers() / 2;
                    let hints = self.hstems + self.vstems;
                    let length = hints.div_ceil(8);
                    if bytes.len() - index < length {
                        return Err(damaged());
                    }
                    index += length;
                    self.sequence.push(Token::Operator(b0, None));
                }
                0..=18 | 21..=27 | 30 | 31 => {
                    match b0 {
                        1 | 18 => self.hstems += self.trailing_numbers() / 2,
                        3 | 23 => self.vstems += self.trailing_numbers() / 2,
                        _ => {}
                    }
                    let token = if b0 == 12 {
                        let b1 = *bytes.get(index).ok_or_else(damaged)?;
                        index += 1;
                        Token::Operator(12, Some(b1))
                    } else {
                        Token::Operator(b0, None)
                    };
                    self.sequence.push(token);
                }
                28 => {
                    let pair = bytes.get(index..index + 2).ok_or_else(damaged)?;
                    index += 2;
                    self.sequence
                        .push(Token::Integer(i32::from(i16::from_be_bytes([
                            pair[0], pair[1],
                        ]))));
                }
                32..=246 => self.sequence.push(Token::Integer(i32::from(b0) - 139)),
                247..=250 => {
                    let b1 = i32::from(*bytes.get(index).ok_or_else(damaged)?);
                    index += 1;
                    self.sequence
                        .push(Token::Integer((i32::from(b0) - 247) * 256 + b1 + 108));
                }
                251..=254 => {
                    let b1 = i32::from(*bytes.get(index).ok_or_else(damaged)?);
                    index += 1;
                    self.sequence
                        .push(Token::Integer(-(i32::from(b0) - 251) * 256 - b1 - 108));
                }
                255 => {
                    let quad = bytes.get(index..index + 4).ok_or_else(damaged)?;
                    index += 4;
                    let value = i16::from_be_bytes([quad[0], quad[1]]);
                    let fraction = f64::from(u16::from_be_bytes([quad[2], quad[3]])) / 65535.0;
                    self.sequence.push(Token::Real(f64::from(value) + fraction));
                }
            }
        }
        Ok(())
    }
}

/// `Type2CharString`'s width: the `hsbw` its conversion to Type 1 puts first, from the first
/// operator that clears the stack, else none.
fn width(sequence: &[Token], default_width_x: i32, nominal_width_x: i32) -> Result<f32, Unported> {
    let mut divided: Vec<Token> = Vec::with_capacity(sequence.len());
    for (index, token) in sequence.iter().enumerate() {
        if *token == Token::Operator(12, Some(12)) && index >= 2 {
            if let (Some(numerator), Some(denominator)) = (
                sequence[index - 2].float_value(),
                sequence[index - 1].float_value(),
            ) {
                divided.pop();
                divided.pop();
                divided.push(Token::Float(numerator / denominator));
                continue;
            }
        }
        divided.push(*token);
    }
    let mut width = None;
    let mut numbers: Vec<f32> = Vec::new();
    for token in divided {
        let Token::Operator(b0, b1) = token else {
            numbers.push(token.float_value().unwrap_or_default());
            continue;
        };
        let count = numbers.len();
        match (b0, b1) {
            (12, Some(35)) if count < 12 => return Err(flex_failure()),
            (12, Some(37)) if count < 11 => return Err(flex_failure()),
            (14, None) if (count == 4 || count == 5) && width.is_none() => {
                return Err(Unported(
                    "a Type 2 glyph built from other glyphs with seac, which the port does not \
                     run"
                    .to_owned(),
                ))
            }
            _ => {}
        }
        if width.is_none() {
            let clears = match (b0, b1) {
                (1 | 3 | 18 | 23 | 19 | 20, None) => Some(!count.is_multiple_of(2)),
                (4 | 22, None) => Some(count > 1),
                (21, None) => Some(count > 2),
                (14, None) => Some(count == 5 || count == 1),
                _ => None,
            };
            let adds = match (b0, b1) {
                (5, None) => count >= 2,
                (6 | 7, None) => count >= 1,
                (8, None) => count >= 6,
                (12, Some(34)) => count >= 7,
                (12, Some(36)) => count >= 9,
                (24, None) => count >= 2,
                (25, None) => count >= 6,
                (26 | 27 | 30 | 31, None) => count >= 4,
                _ => true,
            };
            match clears {
                Some(true) => width = Some(numbers[0] + nominal_width_x as f32),
                Some(false) => width = Some(default_width_x as f32),
                None if adds => width = Some(0.0),
                None => {}
            }
        } else if (b0, b1) == (14, None) && count == 4 {
            return Err(Unported(
                "a Type 2 glyph built from other glyphs with seac, which the port does not run"
                    .to_owned(),
            ));
        }
        numbers.clear();
    }
    Ok(width.unwrap_or(0.0))
}

fn flex_failure() -> Unported {
    Unported("a Type 2 flex with too few operands, which FontBox throws on".to_owned())
}

impl Cff {
    /// `CFFType1Font.hasGlyph`.
    pub(super) fn has_glyph(&self, name: &str) -> bool {
        match &self.keyed {
            Keyed::Type1 { charset, .. } => charset.name_to_gid(name) != 0,
            Keyed::Cid { .. } => false,
        }
    }

    /// `CFFType1Font.getWidth(name)`: the width of the glyph a name has, `.notdef`'s for one
    /// the font lacks.
    pub(super) fn width(&self, name: &str) -> Result<f32, Unported> {
        match &self.keyed {
            Keyed::Type1 {
                charset, private, ..
            } => self.glyph_width(charset.name_to_gid(name), private),
            Keyed::Cid { .. } => Err(Unported(
                "a CID-keyed CFF font measured by glyph name, which FontBox throws on".to_owned(),
            )),
        }
    }

    fn glyph_width(&self, gid: u32, private: &Private) -> Result<f32, Unported> {
        let bytes = self
            .charstrings
            .get(gid as usize)
            .or_else(|| self.charstrings.first())
            .ok_or_else(|| {
                Unported(
                    "a CFF font without charstrings measured, which FontBox throws on".to_owned(),
                )
            })?;
        let mut parser = CharStringParser {
            global: &self.global_subrs,
            local: private.subrs.as_deref(),
            sequence: Vec::new(),
            hstems: 0,
            vstems: 0,
        };
        parser.parse(bytes, 0)?;
        width(
            &parser.sequence,
            private.default_width_x,
            private.nominal_width_x,
        )
    }

    /// The codes and names of a Type 1-keyed font's built-in encoding, in the order they are
    /// added.
    pub(super) fn encoding(&self) -> Option<&[(i32, String)]> {
        match &self.keyed {
            Keyed::Type1 { encoding, .. } => Some(encoding),
            Keyed::Cid { .. } => None,
        }
    }

    /// Whether the program is CID-keyed.
    pub(super) fn is_cid(&self) -> bool {
        matches!(self.keyed, Keyed::Cid { .. })
    }

    /// The width `PDCIDFontType0` takes for a CID: the CID-keyed font's glyph for it through
    /// its charset, measured with its font DICT's Private DICT, or the glyph the CID numbers
    /// in a Type 1-keyed font.
    pub(super) fn cid_width(&self, cid: u32) -> Result<f32, Unported> {
        match &self.keyed {
            Keyed::Type1 { private, .. } => self.glyph_width(cid, private),
            Keyed::Cid {
                charset,
                fd_select,
                privates,
            } => {
                let gid = charset.gid_for_cid(cid);
                if gid as usize >= self.charstrings.len() {
                    return Err(Unported(
                        "a CID mapped past the CFF font's glyphs, which FontBox throws on"
                            .to_owned(),
                    ));
                }
                let none = Private {
                    default_width_x: 1000,
                    nominal_width_x: 0,
                    subrs: None,
                };
                let fd = fd_select.index(gid);
                let private = usize::try_from(fd)
                    .ok()
                    .and_then(|fd| privates.get(fd))
                    .map_or(&none, |private| private.as_ref().unwrap_or(&none));
                self.glyph_width(gid, private)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concatenates_font_matrices() {
        let numbers = |values: [f64; 6]| values.map(Number::Real).to_vec();
        let Ok(product) = concatenate(
            &numbers([1.0, 2.0, 3.0, 4.0, 5.0, 6.0]),
            &numbers([1.0, 0.5, 0.0, 2.0, 1.0, 1.0]),
        ) else {
            panic!("the matrices do not concatenate");
        };
        assert_eq!(
            product
                .into_iter()
                .map(Number::double_value)
                .collect::<Vec<_>>(),
            [1.0, 4.5, 3.0, 9.5, 6.0, 15.5]
        );
    }

    fn index(items: &[&[u8]]) -> Vec<u8> {
        let mut out = (items.len() as u16).to_be_bytes().to_vec();
        if items.is_empty() {
            return out;
        }
        out.push(1);
        let mut offset = 1u8;
        out.push(offset);
        for item in items {
            offset += item.len() as u8;
            out.push(offset);
        }
        for item in items {
            out.extend_from_slice(item);
        }
        out
    }

    fn integer(value: i16) -> [u8; 3] {
        let [high, low] = value.to_be_bytes();
        [28, high, low]
    }

    /// A CFF font of `.notdef` and one more glyph, named `space` by the predefined ISOAdobe
    /// charset, with a default width of 100 and a nominal width of 10.
    fn font(glyph: &[u8]) -> Vec<u8> {
        let notdef: &[u8] = &[139 + 50, 14];
        let charstrings = index(&[notdef, glyph]);
        let private: &[u8] = &[139 + 100, 20, 139 + 10, 21];
        let header = [1u8, 0, 4, 1];
        let names = index(&[b"T"]);
        let top_length = 3 + 1 + 3 + 3 + 1;
        let charstrings_at = header.len() + names.len() + (2 + 1 + 2 + top_length) + 2 + 2;
        let private_at = charstrings_at + charstrings.len();
        let mut top = integer(charstrings_at as i16).to_vec();
        top.push(17);
        top.extend_from_slice(&integer(private.len() as i16));
        top.extend_from_slice(&integer(private_at as i16));
        top.push(18);
        let mut out = header.to_vec();
        out.extend_from_slice(&names);
        out.extend_from_slice(&index(&[&top]));
        out.extend_from_slice(&index(&[]));
        out.extend_from_slice(&index(&[]));
        out.extend_from_slice(&charstrings);
        out.extend_from_slice(private);
        out
    }

    #[test]
    fn reads_a_cff_program_as_fontbox_does() {
        let bytes = font(&[139 + 7, 139 + 20, 139 + 30, 1, 14]);
        let Read::Font(cff) = read(&bytes) else {
            panic!("the program does not read");
        };
        assert!(cff.has_glyph("space"));
        assert!(
            cff.has_glyph("A"),
            "the predefined charset names glyphs past the font's"
        );
        assert!(!cff.has_glyph("missing"));
        assert_eq!(cff.encoding().unwrap()[65], (65, "A".to_owned()));
        assert_eq!(cff.width("space").unwrap(), 17.0);
        assert_eq!(cff.width("A").unwrap(), 60.0);
        assert_eq!(cff.width("missing").unwrap(), 60.0);
        assert!(matches!(read(&bytes[..10]), Read::Damaged));
    }

    /// A CID-keyed font of `.notdef` and two glyphs for CIDs 100 and 101, each in a font DICT
    /// of its own, whose Private DICTs set different nominal widths.
    fn cid_font() -> Vec<u8> {
        let charstrings = index(&[
            &[139 + 50, 14],
            &[139 + 7, 139, 139 + 9, 1, 14],
            &[139 + 7, 139, 139 + 9, 1, 14],
        ]);
        let private_a: &[u8] = &[139 + 20, 21];
        let private_b: &[u8] = &[139 + 40, 21];
        let header = [1u8, 0, 4, 1];
        let names = index(&[b"C"]);
        let top_length = 9 + 4 + 4 + 5 + 5;
        let top_at = header.len() + names.len();
        let strings = index(&[b"Adobe", b"Identity"]);
        let charstrings_at = top_at + (2 + 1 + 2 + top_length) + strings.len() + 2;
        let charset_at = charstrings_at + charstrings.len();
        let charset = [2u8, 0, 100, 0, 1];
        let fd_select_at = charset_at + charset.len();
        let fd_select = [3u8, 0, 2, 0, 0, 0, 0, 2, 1, 0, 3];
        let private_a_at = fd_select_at + fd_select.len();
        let private_b_at = private_a_at + private_a.len();
        let font_dict = |at: usize, length: usize| {
            let mut dict = integer(length as i16).to_vec();
            dict.extend_from_slice(&integer(at as i16));
            dict.push(18);
            dict
        };
        let font_dicts = index(&[
            &font_dict(private_a_at, private_a.len()),
            &font_dict(private_b_at, private_b.len()),
        ]);
        let fd_array_at = private_b_at + private_b.len();
        let mut top = integer(391).to_vec();
        top.extend_from_slice(&integer(392));
        top.extend_from_slice(&[139, 12, 30]);
        top.extend_from_slice(&integer(charstrings_at as i16));
        top.push(17);
        top.extend_from_slice(&integer(charset_at as i16));
        top.push(15);
        top.extend_from_slice(&integer(fd_array_at as i16));
        top.extend_from_slice(&[12, 36]);
        top.extend_from_slice(&integer(fd_select_at as i16));
        top.extend_from_slice(&[12, 37]);
        assert_eq!(top.len(), top_length);
        let mut out = header.to_vec();
        out.extend_from_slice(&names);
        out.extend_from_slice(&index(&[&top]));
        out.extend_from_slice(&strings);
        out.extend_from_slice(&index(&[]));
        assert_eq!(out.len(), charstrings_at);
        out.extend_from_slice(&charstrings);
        out.extend_from_slice(&charset);
        out.extend_from_slice(&fd_select);
        out.extend_from_slice(private_a);
        out.extend_from_slice(private_b);
        out.extend_from_slice(&font_dicts);
        out
    }

    #[test]
    fn measures_cids_through_charset_and_font_dicts() {
        let Read::Font(cff) = read(&cid_font()) else {
            panic!("the program does not read");
        };
        assert!(cff.is_cid());
        assert_eq!(cff.cid_width(100).unwrap(), 27.0);
        assert_eq!(cff.cid_width(101).unwrap(), 47.0);
        assert_eq!(cff.cid_width(5).unwrap(), 70.0);
    }

    #[test]
    fn widths_follow_type2_conversion() {
        let width_of = |sequence: &[Token]| width(sequence, 500, 100).unwrap();
        let ints = |values: &[i32]| {
            values
                .iter()
                .map(|&v| Token::Integer(v))
                .collect::<Vec<_>>()
        };
        let mut odd_stems = ints(&[40, 0, 10]);
        odd_stems.push(Token::Operator(1, None));
        assert_eq!(width_of(&odd_stems), 140.0);
        let mut even_move = ints(&[0, 10]);
        even_move.push(Token::Operator(21, None));
        assert_eq!(width_of(&even_move), 500.0);
        let mut line_first = ints(&[1, 2]);
        line_first.push(Token::Operator(5, None));
        assert_eq!(width_of(&line_first), 0.0);
        let mut divided = ints(&[3, 2, 0, 0]);
        divided.insert(2, Token::Operator(12, Some(12)));
        divided.push(Token::Operator(21, None));
        assert_eq!(width_of(&divided), 101.5);
        let mut flex = ints(&[1, 2, 3]);
        flex.push(Token::Operator(12, Some(35)));
        assert!(width(&flex, 0, 0).is_err());
        let mut seac = ints(&[10, 0, 0, 65, 66]);
        seac.push(Token::Operator(14, None));
        assert!(width(&seac, 0, 0).is_err());
    }
}
