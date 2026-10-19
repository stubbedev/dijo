//! PDFBox's fonts as its text extraction reads them (`PDFont` and its subclasses): how codes
//! are read from strings, mapped to Unicode, and measured, with the fallbacks PDFBox takes and
//! the font programs it reads, the fallback font `PdfFonts` maps non-embedded fonts to among
//! them. Where PDFBox would read a font program the port does not read, the font is
//! [`Unported`].

use std::borrow::Cow;
use std::sync::{LazyLock, OnceLock};

use lopdf::{Dictionary, Object, Stream};

use super::afm::{FontMetrics, Standard14};
use super::cff::{self, Cff};
use super::cmap::{self, CMap, CMapError};
use super::content::{Lexer, Token};
use super::cos::{as_dict, float_value, int_value, name_string, Identity, Pdf};
use super::encoding::{self, Builtin, Encoding, GlyphList, Kind as EncodingKind};
use super::jfloat;
use super::matrix::Matrix;
use super::position::GlyphText;
use super::truetype::{self, TrueType};
use super::type1::{self, BuiltIn, Type1};
use super::Unported;
use crate::formats::Difference;
use crate::hash::FastMap;

/// `PDFontDescriptor`, as far as text extraction reads it.
#[derive(Debug, Clone)]
pub(super) struct Descriptor {
    flags: i32,
    font_bbox: Option<[f32; 4]>,
    ascent: f32,
    descent: f32,
    cap_height: f32,
    missing_width: f32,
}

impl Descriptor {
    fn read(pdf: &Pdf, dict: &Dictionary) -> Self {
        Self {
            flags: pdf.int(dict, b"Flags", 0),
            font_bbox: pdf
                .array(dict, b"FontBBox")
                .map(|array| rectangle(pdf, array)),
            ascent: pdf.float(dict, b"Ascent", 0.0),
            descent: pdf.float(dict, b"Descent", 0.0),
            cap_height: pdf.float(dict, b"CapHeight", 0.0).abs(),
            missing_width: pdf.float(dict, b"MissingWidth", 0.0),
        }
    }

    /// `PDType1FontEmbedder.buildFontDescriptor`: a standard 14 font's descriptor.
    fn from_metrics(metrics: &FontMetrics) -> Self {
        let symbolic = metrics.is_symbolic();
        let flags = if symbolic { 4 } else { 32 };
        let [x1, y1, x2, y2] = metrics.font_bbox;
        Self {
            flags,
            font_bbox: Some([
                jfloat::min(x1, x2),
                jfloat::min(y1, y2),
                jfloat::max(x1, x2),
                jfloat::max(y1, y2),
            ]),
            ascent: metrics.ascender,
            descent: metrics.descender,
            cap_height: metrics.cap_height.abs(),
            missing_width: 0.0,
        }
    }

    pub(super) fn is_symbolic(&self) -> bool {
        self.flags & 4 != 0
    }
}

/// `new PDRectangle(COSArray)`: four numbers, absent ones 0, clamped to the `int` range and
/// normalized lower left to upper right.
pub(super) fn rectangle(pdf: &Pdf, array: &[Object]) -> [f32; 4] {
    let mut values = [0f32; 4];
    for (value, item) in values.iter_mut().zip(array) {
        let number = pdf.resolve(item).and_then(float_value).unwrap_or(0.0);
        *value = if number.abs() > i32::MAX as f32 {
            if number > 0.0 {
                i32::MAX as f32
            } else {
                -(i32::MAX as f32)
            }
        } else {
            number
        };
    }
    [
        jfloat::min(values[0], values[2]),
        jfloat::min(values[1], values[3]),
        jfloat::max(values[0], values[2]),
        jfloat::max(values[1], values[3]),
    ]
}

/// Where PDFBox reads a font: the program embedded in the document, or the fallback font
/// `PdfFonts` maps every other font to.
#[derive(Debug)]
enum Program<T> {
    Embedded(T),
    Fallback,
}

/// PDFBox's bundled Liberation Sans, which `PdfFonts` maps every font to that PDFBox finds no
/// program for.
static FALLBACK: LazyLock<Result<TrueType, Unported>> = LazyLock::new(|| {
    TrueType::read_bundled(include_bytes!(
        "../../../models/pdf/LiberationSans-Regular.ttf"
    ))
});

fn fallback_font() -> Result<&'static TrueType, Unported> {
    FALLBACK.as_ref().map_err(Clone::clone)
}

impl Program<Box<TrueType>> {
    /// The TrueType program PDFBox measures with: the embedded one, or the fallback font.
    fn font(&self) -> Result<&TrueType, Unported> {
        match self {
            Program::Embedded(ttf) => Ok(ttf),
            Program::Fallback => fallback_font(),
        }
    }
}

/// What PDFBox measures a Type 1 font's glyphs with (`genericFont`): its embedded program, a
/// Type 1 program or a compact one, or the fallback font.
#[derive(Debug)]
enum Generic {
    Program(Box<Type1>),
    Compact(Box<Cff>),
    Fallback,
}

/// `PDType1Font.ALT_NAMES`: names a substitute font may give ligatures.
fn alternative_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "ff" => "f_f",
        "ffi" => "f_f_i",
        "ffl" => "f_f_l",
        "fi" => "f_i",
        "fl" => "f_l",
        "st" => "s_t",
        "IJ" => "I_J",
        "ij" => "i_j",
        "ellipsis" => "elipsis",
        _ => return None,
    })
}

/// `UniUtil.getUniNameOfCodePoint`.
fn uni_name(code_point: char) -> String {
    format!("uni{:04X}", u32::from(code_point))
}

impl Generic {
    /// `hasGlyph`.
    fn has_glyph(&self, name: &str) -> Result<bool, Unported> {
        Ok(match self {
            Generic::Program(program) => program.has_glyph(name),
            Generic::Compact(program) => program.has_glyph(name),
            Generic::Fallback => fallback_font()?.name_to_glyph(name) != 0,
        })
    }

    /// `getWidth(name)`, in the program's glyph space.
    fn width(&self, name: &str) -> Result<f32, Unported> {
        match self {
            Generic::Program(program) => program.width(name),
            Generic::Compact(program) => program.width(name),
            Generic::Fallback => {
                let font = fallback_font()?;
                Ok(f32::from(font.advance_width(font.name_to_glyph(name))))
            }
        }
    }

    /// `PDType1Font.getFontMatrix`: the program's matrix of six numbers, else PDFBox's default.
    fn font_matrix(&self) -> Result<[f32; 6], Unported> {
        Ok(match self {
            Generic::Program(program) => match program.font_matrix[..] {
                [a, b, c, d, e, f] => [a, b, c, d, e, f],
                _ => [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            },
            Generic::Compact(program) => match program.font_matrix[..] {
                [a, b, c, d, e, f] => [a, b, c, d, e, f].map(cff::Number::float_value),
                _ => [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            },
            Generic::Fallback => {
                let scale = 0.001f32 * (1000.0f32 / f32::from(fallback_font()?.units_per_em));
                [scale, 0.0, 0.0, scale, 0.0, 0.0]
            }
        })
    }

    /// `getFontBBox`.
    fn font_bbox(&self) -> Result<[f32; 4], Unported> {
        match self {
            Generic::Program(program) => match program.font_bbox[..] {
                [a, b, c, d, ..] => Ok([a, b, c, d]),
                _ => Err(Unported(
                    "a Type 1 font program without four numbers in its FontBBox measured, which \
                     FontBox throws on"
                        .to_owned(),
                )),
            },
            Generic::Compact(program) => match program.font_bbox[..] {
                [a, b, c, d, ..] => Ok([a, b, c, d].map(cff::Number::float_value)),
                _ => Err(Unported(
                    "a CFF font program without four numbers in its FontBBox measured, which \
                     FontBox throws on"
                        .to_owned(),
                )),
            },
            Generic::Fallback => Ok(fallback_font()?.font_bbox()),
        }
    }
}

fn fallback() -> Unported {
    Unported(
        "a font PDFBox measures with its fallback font, as it maps fonts the document does not \
         embed"
            .to_owned(),
    )
}

/// The `/Widths` of a simple font.
#[derive(Debug, Default)]
struct Widths {
    values: Vec<Option<f32>>,
    first_char: i32,
    last_char: i32,
    /// `/Widths` resolves to something, or `/MissingWidth` is a key of the font.
    declared: bool,
    /// `PDFont.getAverageFontWidth`: the average of the positive numbers of `/Widths`.
    average: f32,
}

impl Widths {
    fn read(pdf: &Pdf, dict: &Dictionary) -> Self {
        let array = pdf.array(dict, b"Widths").unwrap_or(&[]);
        let values: Vec<Option<f32>> = array
            .iter()
            .map(|item| pdf.resolve(item).and_then(float_value))
            .collect();
        let mut total = 0.0f32;
        let mut count = 0.0f32;
        for value in values.iter().flatten() {
            if *value > 0.0 {
                total += value;
                count += 1.0;
            }
        }
        Self {
            values,
            first_char: pdf.int(dict, b"FirstChar", -1),
            last_char: pdf.int(dict, b"LastChar", -1),
            declared: pdf.get(dict, b"Widths").is_some() || dict.has(b"MissingWidth"),
            average: if total > 0.0 { total / count } else { 0.0 },
        }
    }

    /// The width `/Widths` gives a code, if it covers it.
    fn width(&self, code: i32) -> Option<f32> {
        let index = code.checked_sub(self.first_char)?;
        let in_range = !self.values.is_empty()
            && code >= self.first_char
            && code <= self.last_char
            && (index as usize) < self.values.len();
        in_range.then(|| self.values[index as usize].unwrap_or(0.0))
    }
}

/// The kind of a simple font, with what PDFBox reads of its program.
#[derive(Debug)]
enum SimpleKind {
    TrueType(Program<Box<TrueType>>),
    /// A Type 1 font, measured with its program or, unless it is one of the standard 14 PDFBox
    /// measures with their metrics, with the fallback font.
    Type1(Generic),
    /// A Type 1 font with a compact font program (`PDType1CFont`), measured likewise.
    Type1C(Generic),
    Type3 {
        matrix: Matrix,
        bbox: Option<[f32; 4]>,
        /// The glyph procedures of `CharProcs`, if the font has that dictionary.
        procs: Option<Vec<CharProc>>,
    },
}

/// A Type 3 glyph procedure, as far as PDFBox reads one without running it: its stream's
/// `Length`, and its first operator with the operands before it, if it has one.
#[derive(Debug)]
struct CharProc {
    name: String,
    length: i32,
    first: Result<Option<(String, Vec<Object>)>, Unported>,
}

impl CharProc {
    fn read(pdf: &Pdf, name: &[u8], stream: &Stream) -> Self {
        Self {
            name: name_string(name),
            length: pdf.int(&stream.dict, b"Length", 0),
            first: first_operation(pdf, stream),
        }
    }

    /// `PDType3CharProc.getWidth`: the advance `d0` or `d1` gives first.
    fn width(&self) -> Result<f32, Unported> {
        let unported = |why: &str| {
            Unported(format!(
                "a Type 3 glyph procedure {why}, which PDFBox fails to measure"
            ))
        };
        match self.first.as_ref().map_err(Clone::clone)? {
            None => Err(unported("without an operator")),
            Some((operator, operands)) if operator == "d0" || operator == "d1" => operands
                .first()
                .and_then(float_value)
                .ok_or_else(|| unported("whose d0 or d1 has no number first")),
            Some(_) => Err(unported("not starting with d0 or d1")),
        }
    }

    /// `PDType3CharProc.getGlyphBBox`: the box a `d1` of six numbers gives first, its corners
    /// as `d1` gives them. Tika reaches the upper corner by adding the box's size in `float`,
    /// as `PDRectangle` does, which can round it off the corner (#109).
    fn glyph_bbox(&self) -> Result<Option<[f32; 4]>, Unported> {
        let Some((operator, operands)) = self.first.as_ref().map_err(Clone::clone)? else {
            return Ok(None);
        };
        if operator != "d1" || operands.len() != 6 {
            return Ok(None);
        }
        let Some(values) = operands
            .iter()
            .map(float_value)
            .collect::<Option<Vec<f32>>>()
        else {
            return Ok(None);
        };
        Ok(Some([values[2], values[3], values[4], values[5]]))
    }
}

/// The first operator of a stream and the operands before it, as `PDFStreamParser` reads them;
/// nothing if the stream has no operator.
fn first_operation(pdf: &Pdf, stream: &Stream) -> Result<Option<(String, Vec<Object>)>, Unported> {
    let data = pdf.decode(stream)?;
    let mut lexer = Lexer::new(&data);
    let mut operands = Vec::new();
    while let Some(token) = lexer.next_token()? {
        match token {
            Token::Operator(name) => return Ok(Some((name.into_owned(), operands))),
            Token::InlineImage(_) => return Ok(Some(("BI".to_owned(), operands))),
            Token::Operand(object) => operands.push(object),
        }
    }
    Ok(None)
}

#[derive(Debug)]
struct SimpleFont {
    kind: SimpleKind,
    encoding: Option<Box<Encoding>>,
    zapf_dingbats: bool,
    symbolic: bool,
    embedded: bool,
    damaged: bool,
}

/// The descendant of a composite font.
#[derive(Debug)]
struct CidFont {
    descriptor: Option<Descriptor>,
    widths: FastMap<i32, f32>,
    width_order: Vec<(i32, f32)>,
    default_width: f32,
    dw2: [f32; 2],
    vertical: FastMap<i32, (f32, f32, f32)>,
    vertical_ranges: Vec<(i32, i32, f32, f32, f32)>,
    kind: CidKind,
    cid_to_gid: Option<Vec<u32>>,
    embedded: bool,
    damaged: bool,
    registry: Option<String>,
    ordering: Option<String>,
    supplement: Option<i32>,
}

/// The kind of a composite font's descendant, with what PDFBox reads of its program.
#[derive(Debug)]
enum CidKind {
    /// `PDCIDFontType2`: its TrueType program, or the fallback font.
    TrueType(Program<Box<TrueType>>),
    /// `PDCIDFontType0`: its CFF program, CID-keyed or not, or the fallback font.
    Compact(Program<Box<Cff>>),
}

#[derive(Debug)]
struct Type0Font {
    cmap: Option<Cow<'static, CMap>>,
    predefined: bool,
    descendant_cjk: bool,
    /// `cMapUCS2`: the predefined map from CIDs to Unicode, of a font whose CMap is predefined
    /// or whose descendant is a CJK font, where FontBox bundles it.
    ucs2: Option<&'static CMap>,
    descendant: CidFont,
}

#[derive(Debug)]
enum FontKind {
    Simple(SimpleFont),
    Type0(Box<Type0Font>),
}

/// A font, as PDFBox reads it for text extraction.
#[derive(Debug)]
pub(super) struct Font {
    pub(super) identity: Identity,
    name: Option<String>,
    to_unicode: Option<CMap>,
    to_unicode_is_name: bool,
    has_to_unicode: bool,
    encoding_entry: Option<String>,
    widths: Widths,
    afm: Option<&'static FontMetrics>,
    descriptor: Option<Descriptor>,
    kind: FontKind,
    space_width: OnceLock<f32>,
    height: OnceLock<f32>,
}

/// A glyph's code in a string, and how many bytes it takes.
#[derive(Debug, Clone, Copy)]
pub(super) struct Code {
    pub(super) code: u32,
    pub(super) length: usize,
}

impl Font {
    /// `PDFontFactory.createFont`.
    pub(super) fn load(pdf: &Pdf, identity: Identity, dict: &Dictionary) -> Result<Font, Unported> {
        let subtype = pdf.name(dict, b"Subtype").unwrap_or_default();
        match subtype {
            b"Type1" | b"MMType1" => Self::type1(pdf, identity, dict),
            b"TrueType" => Self::true_type(pdf, identity, dict),
            b"Type3" => Self::type3(pdf, identity, dict),
            b"Type0" => Self::type0(pdf, identity, dict),
            b"CIDFontType0" | b"CIDFontType2" => Err(Unported(
                "a descendant font used as a font, which PDFBox throws on".to_owned(),
            )),
            _ => Self::type1(pdf, identity, dict),
        }
    }

    /// The parts every font shares (`PDFont(COSDictionary)`), named `name`.
    fn base(
        pdf: &Pdf,
        identity: Identity,
        dict: &Dictionary,
        name: Option<String>,
        kind: FontKind,
    ) -> Result<Font, Unported> {
        let afm = name
            .as_deref()
            .and_then(Standard14::named)
            .map(Standard14::metrics);
        let descriptor = match pdf.dict(dict, b"FontDescriptor") {
            Some(fd) => Some(Descriptor::read(pdf, fd)),
            None => afm.map(Descriptor::from_metrics),
        };
        let encoding_entry = pdf.name(dict, b"Encoding").map(name_string);
        let to_unicode = load_to_unicode(pdf, dict, encoding_entry.as_deref())?;
        Ok(Font {
            identity,
            name,
            to_unicode,
            to_unicode_is_name: pdf.name(dict, b"ToUnicode").is_some(),
            has_to_unicode: dict.has(b"ToUnicode"),
            encoding_entry,
            widths: Widths::read(pdf, dict),
            afm,
            descriptor,
            kind,
            space_width: OnceLock::new(),
            height: OnceLock::new(),
        })
    }

    fn placeholder() -> FontKind {
        FontKind::Simple(SimpleFont {
            kind: SimpleKind::Type1(Generic::Fallback),
            encoding: None,
            zapf_dingbats: false,
            symbolic: false,
            embedded: false,
            damaged: false,
        })
    }

    fn true_type(pdf: &Pdf, identity: Identity, dict: &Dictionary) -> Result<Font, Unported> {
        let name = pdf.name_as_string(dict, b"BaseFont");
        let mut font = Self::base(pdf, identity, dict, name, Self::placeholder())?;
        let mut damaged = false;
        let mut program = Program::Fallback;
        if let Some(stream) = font
            .descriptor
            .as_ref()
            .and_then(|_| pdf.dict(dict, b"FontDescriptor"))
            .and_then(|fd| pdf.stream(fd, b"FontFile2"))
        {
            match read_true_type(&pdf.decode(stream)?)? {
                Some(ttf) => program = Program::Embedded(ttf),
                None => damaged = true,
            }
        }
        let embedded = matches!(program, Program::Embedded(_));
        let mut simple = SimpleFont {
            kind: SimpleKind::TrueType(program),
            encoding: None,
            zapf_dingbats: false,
            symbolic: false,
            embedded,
            damaged,
        };
        simple.encoding = Some(Box::new(font.read_encoding(pdf, dict, &simple)?));
        simple.zapf_dingbats = font.standard14_name() == Some(Standard14::ZapfDingbats);
        font.kind = FontKind::Simple(simple);
        font.settle_symbolic();
        Ok(font)
    }

    fn type1(pdf: &Pdf, identity: Identity, dict: &Dictionary) -> Result<Font, Unported> {
        if pdf
            .dict(dict, b"FontDescriptor")
            .is_some_and(|fd| fd.has(b"FontFile3"))
        {
            return Self::type1c(pdf, identity, dict);
        }
        let name = pdf.name_as_string(dict, b"BaseFont");
        let font = Self::base(pdf, identity, dict, name, Self::placeholder())?;
        let mut generic = Generic::Fallback;
        let mut damaged = false;
        if let Some(fd) = pdf.dict(dict, b"FontDescriptor") {
            if let Some(stream) = pdf.stream(fd, b"FontFile") {
                let bytes = pdf.decode(stream)?;
                let length1 = pdf.int(&stream.dict, b"Length1", -1);
                let length2 = pdf.int(&stream.dict, b"Length2", -1);
                match type1::read(&bytes, length1, length2) {
                    type1::Read::Font(program) => generic = Generic::Program(program),
                    type1::Read::Absent => {}
                    type1::Read::Damaged => damaged = true,
                    type1::Read::Unported(reason) => return Err(Unported(reason)),
                }
            }
        }
        let embedded = matches!(generic, Generic::Program(_));
        font.settle_simple(pdf, dict, SimpleKind::Type1(generic), embedded, damaged)
    }

    /// `new PDType1CFont`: a Type 1 font with a compact font program, which PDFBox calls damaged
    /// when it fails to parse or is CID-keyed, and ignores when it is empty.
    fn type1c(pdf: &Pdf, identity: Identity, dict: &Dictionary) -> Result<Font, Unported> {
        let name = pdf.name_as_string(dict, b"BaseFont");
        let font = Self::base(pdf, identity, dict, name, Self::placeholder())?;
        let mut generic = Generic::Fallback;
        let mut damaged = false;
        if let Some(stream) = pdf
            .dict(dict, b"FontDescriptor")
            .and_then(|fd| pdf.stream(fd, b"FontFile3"))
        {
            let bytes = pdf.decode(stream)?;
            if !bytes.is_empty() {
                match cff::read(&bytes) {
                    cff::Read::Font(program) if program.is_cid() => damaged = true,
                    cff::Read::Font(program) => generic = Generic::Compact(program),
                    cff::Read::Damaged => damaged = true,
                    cff::Read::Unported(reason) => return Err(Unported(reason)),
                }
            }
        }
        let embedded = matches!(generic, Generic::Compact(_));
        font.settle_simple(pdf, dict, SimpleKind::Type1C(generic), embedded, damaged)
    }

    /// A simple font of `kind`, its encoding read and its symbolic flag settled.
    fn settle_simple(
        mut self,
        pdf: &Pdf,
        dict: &Dictionary,
        kind: SimpleKind,
        embedded: bool,
        damaged: bool,
    ) -> Result<Font, Unported> {
        let mut simple = SimpleFont {
            kind,
            encoding: None,
            zapf_dingbats: false,
            symbolic: false,
            embedded,
            damaged,
        };
        simple.encoding = Some(Box::new(self.read_encoding(pdf, dict, &simple)?));
        simple.zapf_dingbats = self.standard14_name() == Some(Standard14::ZapfDingbats);
        self.kind = FontKind::Simple(simple);
        self.settle_symbolic();
        Ok(self)
    }

    fn type3(pdf: &Pdf, identity: Identity, dict: &Dictionary) -> Result<Font, Unported> {
        let name = pdf.name_as_string(dict, b"Name");
        let mut font = Self::base(pdf, identity, dict, name, Self::placeholder())?;
        let encoding = match pdf.get(dict, b"Encoding") {
            Some(Object::Name(name)) => Builtin::named(name).map(|b| b.encoding().clone()),
            Some(object) => {
                as_dict(object).map(|encoding| Encoding::type3_dictionary(pdf, encoding))
            }
            None => None,
        };
        let matrix = match pdf.array(dict, b"FontMatrix") {
            Some(array)
                if array.len() == 6
                    && array
                        .iter()
                        .all(|item| pdf.resolve(item).and_then(float_value).is_some()) =>
            {
                let values: Vec<f32> = array
                    .iter()
                    .map(|item| pdf.resolve(item).and_then(float_value).unwrap_or(0.0))
                    .collect();
                Matrix::new(
                    values[0], values[1], values[2], values[3], values[4], values[5],
                )
            }
            _ => Matrix::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0),
        };
        let bbox = pdf
            .array(dict, b"FontBBox")
            .map(|array| rectangle(pdf, array));
        let procs = pdf.dict(dict, b"CharProcs").map(|procs| {
            procs
                .iter()
                .filter_map(|(name, value)| match pdf.resolve(value) {
                    Some(Object::Stream(stream)) => Some(CharProc::read(pdf, name, stream)),
                    _ => None,
                })
                .collect()
        });
        font.kind = FontKind::Simple(SimpleFont {
            kind: SimpleKind::Type3 {
                matrix,
                bbox,
                procs,
            },
            encoding: encoding.map(Box::new),
            zapf_dingbats: false,
            symbolic: false,
            embedded: true,
            damaged: false,
        });
        Ok(font)
    }

    fn type0(pdf: &Pdf, identity: Identity, dict: &Dictionary) -> Result<Font, Unported> {
        let name = pdf.name_as_string(dict, b"BaseFont");
        let mut font = Self::base(pdf, identity, dict, name, Self::placeholder())?;
        let descendants = pdf.array(dict, b"DescendantFonts").ok_or_else(|| {
            Unported("a composite font without descendant fonts, which PDFBox throws on".to_owned())
        })?;
        let descendant_dict = descendants
            .first()
            .and_then(|first| pdf.resolve(first))
            .and_then(as_dict)
            .ok_or_else(|| {
                Unported(
                    "a composite font without a descendant font dictionary, which PDFBox throws on"
                        .to_owned(),
                )
            })?;
        if pdf.name(descendant_dict, b"Type").unwrap_or(b"Font") != b"Font" {
            return Err(Unported(
                "a composite font whose descendant is not a font, which PDFBox throws on"
                    .to_owned(),
            ));
        }
        let declared = pdf.name(descendant_dict, b"Subtype");
        let subtype = match (declared, sniffed_cid_subtype(pdf, dict, descendant_dict)?) {
            (Some(declared), Some(sniffed)) if declared != sniffed => Some(sniffed),
            (declared, _) => declared,
        };
        let descendant =
            match subtype {
                Some(b"CIDFontType2") => CidFont::true_type(pdf, descendant_dict)?,
                Some(b"CIDFontType0") => CidFont::compact(pdf, descendant_dict)?,
                _ => return Err(Unported(
                    "a composite font with a descendant of no known type, which PDFBox throws on"
                        .to_owned(),
                )),
            };
        let (cmap, predefined) =
            match pdf.get(dict, b"Encoding") {
                Some(Object::Name(name)) => match cmap::predefined(&name_string(name)) {
                    Ok(cmap) => (Some(Cow::Borrowed(cmap)), true),
                    Err(CMapError::Unported(unported)) => return Err(unported),
                    Err(CMapError::Malformed(reason)) => {
                        return Err(Unported(format!(
                        "a composite font naming a CMap PDFBox does not find, which it throws on: \
                         {reason}"
                    )))
                    }
                },
                Some(Object::Stream(stream)) => match cmap::parse(&pdf.decode(stream)?) {
                    Ok(cmap) => (Some(Cow::Owned(cmap)), false),
                    Err(CMapError::Unported(unported)) => return Err(unported),
                    Err(CMapError::Malformed(reason)) => {
                        return Err(Unported(format!(
                            "a composite font whose CMap PDFBox fails to read: {reason}"
                        )))
                    }
                },
                Some(_) => return Err(Unported(
                    "a composite font whose encoding is neither a name nor a stream, which PDFBox \
                     throws on"
                        .to_owned(),
                )),
                None => (None, false),
            };
        let descendant_cjk = descendant.registry.as_deref() == Some("Adobe")
            && matches!(
                descendant.ordering.as_deref(),
                Some("GB1" | "CNS1" | "Japan1" | "Korea1")
            );
        let encoding_name = font.encoding_entry.clone();
        let identity_encoding =
            matches!(encoding_name.as_deref(), Some("Identity-H" | "Identity-V"));
        let ucs2 = if (predefined && !identity_encoding) || descendant_cjk {
            let source = if descendant_cjk {
                match (&descendant.registry, &descendant.ordering) {
                    (Some(registry), Some(ordering)) => Some(format!(
                        "{registry}-{ordering}-{}",
                        descendant.supplement.unwrap_or(0)
                    )),
                    _ => None,
                }
            } else {
                encoding_name
            };
            match source {
                Some(source) => ucs2_map(&source)?,
                None => None,
            }
        } else {
            None
        };
        font.kind = FontKind::Type0(Box::new(Type0Font {
            cmap,
            predefined,
            descendant_cjk,
            ucs2,
            descendant,
        }));
        Ok(font)
    }

    /// `PDSimpleFont.readEncoding`, with the font's `readEncodingFromFont`.
    fn read_encoding(
        &self,
        pdf: &Pdf,
        dict: &Dictionary,
        simple: &SimpleFont,
    ) -> Result<Encoding, Unported> {
        match pdf.get(dict, b"Encoding") {
            Some(Object::Name(_))
                if self.name.as_deref() == Some("ZapfDingbats") && !simple.embedded =>
            {
                Ok(Builtin::ZapfDingbats.encoding().clone())
            }
            Some(Object::Name(name)) => match Builtin::named(name) {
                Some(builtin) => Ok(builtin.encoding().clone()),
                None => self.encoding_from_font(simple),
            },
            Some(object) if as_dict(object).is_some() => {
                let encoding_dict = as_dict(object).unwrap_or_else(|| unreachable!());
                let symbolic = self.descriptor.as_ref().map(Descriptor::is_symbolic);
                let base_valid = pdf
                    .name(encoding_dict, b"BaseEncoding")
                    .and_then(Builtin::named)
                    .is_some();
                let built_in = if !base_valid && symbolic == Some(true) {
                    Some(self.encoding_from_font(simple)?)
                } else {
                    None
                };
                Encoding::simple_dictionary(
                    pdf,
                    encoding_dict,
                    !symbolic.unwrap_or(false),
                    built_in.as_ref(),
                )
                .ok_or_else(|| {
                    Unported(
                        "a symbolic font without a built-in encoding, which PDFBox throws on"
                            .to_owned(),
                    )
                })
            }
            _ => self.encoding_from_font(simple),
        }
    }

    /// `readEncodingFromFont` of a TrueType or Type 1 font.
    fn encoding_from_font(&self, simple: &SimpleFont) -> Result<Encoding, Unported> {
        if !simple.embedded {
            if let Some(afm) = self.afm {
                return Ok(afm.encoding());
            }
        }
        match &simple.kind {
            SimpleKind::Type1(Generic::Fallback) | SimpleKind::Type1C(Generic::Fallback) => {
                Ok(Builtin::Standard.encoding().clone())
            }
            SimpleKind::Type1(Generic::Compact(_)) | SimpleKind::Type1C(Generic::Program(_)) => {
                Err(Unported(
                    "a program of the other Type 1 kind in a font, which PDFBox cannot hold"
                        .to_owned(),
                ))
            }
            SimpleKind::Type1C(Generic::Compact(program)) => {
                let entries = program.encoding().ok_or_else(|| {
                    Unported("a CID-keyed program read for its encoding".to_owned())
                })?;
                Ok(Encoding::from_font_box(entries))
            }
            SimpleKind::Type1(Generic::Program(program)) => match &program.encoding {
                None => Err(Unported(
                    "a Type 1 font program without an encoding, which PDFBox fails on".to_owned(),
                )),
                Some(BuiltIn::Standard) => Ok(Encoding::from_font_box(
                    &Builtin::Standard.encoding().entries(),
                )),
                Some(BuiltIn::Custom(entries)) => {
                    let parsed: Vec<(i32, String)> = encoding::java_integer_map_order(entries)
                        .into_iter()
                        .map(|(code, name)| (code, name.clone()))
                        .collect();
                    Ok(Encoding::from_font_box(&parsed))
                }
            },
            SimpleKind::TrueType(program) => {
                if self.descriptor.as_ref().map(Descriptor::is_symbolic) == Some(false) {
                    return Ok(Builtin::Standard.encoding().clone());
                }
                let standard14 = self.standard14_name();
                if self.is_standard14_name(simple.embedded)
                    && !matches!(
                        standard14,
                        Some(Standard14::Symbol | Standard14::ZapfDingbats)
                    )
                {
                    return Ok(Builtin::Standard.encoding().clone());
                }
                let ttf = program.font()?;
                let mut entries = Vec::new();
                for code in 0..=256 {
                    let glyph = true_type_code_to_gid_symbolic_unsettled(ttf, code);
                    if glyph > 0 {
                        let name = ttf
                            .post_name(glyph)
                            .map_or_else(|| glyph.to_string(), str::to_owned);
                        entries.push((code, name));
                    }
                }
                Ok(Encoding::font_program(&entries))
            }
            SimpleKind::Type3 { .. } => Err(Unported(
                "a Type 3 font asked for its built-in encoding, which PDFBox throws on".to_owned(),
            )),
        }
    }

    /// The standard 14 font the font's name maps to.
    fn standard14_name(&self) -> Option<Standard14> {
        self.name.as_deref().and_then(Standard14::named)
    }

    /// `PDFont.isStandard14`: not embedded, and named as one of the standard 14.
    fn is_standard14_name(&self, embedded: bool) -> bool {
        !embedded && self.standard14_name().is_some()
    }

    /// `PDSimpleFont.isStandard14`: also without real differences from its base encoding.
    fn is_standard14(&self) -> bool {
        match &self.kind {
            FontKind::Type0(_) => false,
            FontKind::Simple(simple) => {
                if matches!(simple.kind, SimpleKind::Type3 { .. }) {
                    return false;
                }
                if let Some(Encoding {
                    kind:
                        EncodingKind::Dictionary {
                            base, differences, ..
                        },
                    ..
                }) = simple.encoding.as_deref()
                {
                    if !differences.is_empty() {
                        let base = base
                            .as_deref()
                            .unwrap_or_else(|| Builtin::Standard.encoding());
                        if differences
                            .iter()
                            .any(|(&code, name)| name != base.name(code))
                        {
                            return false;
                        }
                    }
                }
                self.is_standard14_name(simple.embedded)
            }
        }
    }

    /// `PDSimpleFont.isSymbolic`, settled once the encoding is read.
    fn settle_symbolic(&mut self) {
        let symbolic = self.font_symbolic().unwrap_or(true);
        if let FontKind::Simple(simple) = &mut self.kind {
            simple.symbolic = symbolic;
        }
    }

    /// `PDSimpleFont.isFontSymbolic`.
    fn font_symbolic(&self) -> Option<bool> {
        let FontKind::Simple(simple) = &self.kind else {
            return None;
        };
        if matches!(simple.kind, SimpleKind::Type3 { .. }) {
            return Some(false);
        }
        if let Some(descriptor) = &self.descriptor {
            return Some(descriptor.is_symbolic());
        }
        if self.is_standard14() {
            return Some(matches!(
                self.standard14_name(),
                Some(Standard14::Symbol | Standard14::ZapfDingbats)
            ));
        }
        let encoding = simple.encoding.as_deref()?;
        if encoding.is_latin() {
            return Some(false);
        }
        if let EncodingKind::Dictionary { differences, .. } = &encoding.kind {
            let latin = [Builtin::WinAnsi, Builtin::MacRoman, Builtin::Standard];
            let symbolic = differences.values().any(|name| {
                name != ".notdef" && !latin.iter().all(|b| b.encoding().contains(name))
            });
            return Some(symbolic);
        }
        None
    }

    /// `PDFont.getName`.
    pub(super) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// `isEmbedded`.
    pub(super) fn is_embedded(&self) -> bool {
        match &self.kind {
            FontKind::Simple(simple) => simple.embedded,
            FontKind::Type0(type0) => type0.descendant.embedded,
        }
    }

    /// `isDamaged`.
    pub(super) fn is_damaged(&self) -> bool {
        match &self.kind {
            FontKind::Simple(simple) => simple.damaged,
            FontKind::Type0(type0) => type0.descendant.damaged,
        }
    }

    /// Whether the font is a simple font, whose codes Acrobat coerces to Unicode.
    pub(super) fn is_simple(&self) -> bool {
        matches!(self.kind, FontKind::Simple(_))
    }

    /// Whether the font is a Type 3 font.
    pub(super) fn type3_matrix(&self) -> Option<&Matrix> {
        match &self.kind {
            FontKind::Simple(SimpleFont {
                kind: SimpleKind::Type3 { matrix, .. },
                ..
            }) => Some(matrix),
            _ => None,
        }
    }

    /// `isVertical`.
    pub(super) fn is_vertical(&self) -> bool {
        match &self.kind {
            FontKind::Simple(_) => false,
            FontKind::Type0(type0) => type0.cmap.as_ref().is_some_and(|cmap| cmap.wmode == 1),
        }
    }

    /// `readCode`: the next code in `bytes`.
    pub(super) fn read_code(&self, bytes: &[u8]) -> Result<Code, Unported> {
        match &self.kind {
            FontKind::Simple(_) => Ok(Code {
                code: u32::from(bytes[0]),
                length: 1,
            }),
            FontKind::Type0(type0) => {
                let cmap = type0.cmap.as_ref().ok_or_else(|| {
                    Unported("a composite font without a CMap, which PDFBox throws on".to_owned())
                })?;
                let (code, length) = cmap.read_code(bytes).ok_or_else(|| {
                    Unported(
                        "a CMap without codespace ranges reads a code, which PDFBox fails on"
                            .to_owned(),
                    )
                })?;
                Ok(Code { code, length })
            }
        }
    }

    /// `PDFont.toUnicode(int)` as the base class maps a code with the `ToUnicode` map.
    fn base_to_unicode(&self, code: u32) -> Result<Option<GlyphText>, Unported> {
        let Some(cmap) = &self.to_unicode else {
            return Ok(None);
        };
        let identity_named = cmap
            .name
            .as_deref()
            .is_some_and(|name| name.starts_with("Identity-"));
        if identity_named && (self.to_unicode_is_name || !cmap.has_unicode_mappings()) {
            return java_char(code).map(Some);
        }
        if code < 256 && self.is_simple() {
            if let Some(encoding) = &self.encoding_entry {
                if !encoding.starts_with("Identity") {
                    return Ok(cmap.to_unicode_length(code, 1).map(GlyphText::new));
                }
            }
        }
        Ok(cmap.to_unicode(code).map(GlyphText::new))
    }

    /// `toUnicode`: the text of a code, a simple font's glyph names read with PDFBox's
    /// extended glyph list as `LegacyPDFStreamEngine` reads them when `extended`, or with the
    /// Adobe Glyph List as `toUnicode(int)` reads them.
    pub(super) fn to_unicode(
        &self,
        code: u32,
        extended: bool,
    ) -> Result<Option<GlyphText>, Unported> {
        match &self.kind {
            FontKind::Simple(simple) => {
                if let Some(unicode) = self.base_to_unicode(code)? {
                    return Ok(Some(unicode));
                }
                let Some(encoding) = simple.encoding.as_deref() else {
                    return Ok(None);
                };
                let list: &GlyphList = if simple.zapf_dingbats {
                    &encoding::ZAPF_DINGBATS
                } else if extended {
                    &encoding::TEXT_EXTRACTION
                } else {
                    &encoding::ADOBE
                };
                Ok(list.to_unicode(encoding.name(code as i32)))
            }
            FontKind::Type0(type0) => {
                if let Some(unicode) = self.base_to_unicode(code)? {
                    return Ok(Some(unicode));
                }
                if self.to_unicode.is_some() && !type0.predefined {
                    return java_char(code).map(Some);
                }
                if type0.predefined || type0.descendant_cjk {
                    if let Some(ucs2) = type0.ucs2 {
                        return Ok(ucs2.to_unicode(self.code_to_cid(code)).map(GlyphText::new));
                    }
                }
                let CidKind::TrueType(program) = &type0.descendant.kind else {
                    return Ok(None);
                };
                let Some(cmap) = program.font()?.unicode_cmap() else {
                    return Ok(None);
                };
                let glyph = if type0.descendant.embedded {
                    self.code_to_gid(code)?
                } else {
                    self.code_to_cid(code)
                };
                match cmap.codes(glyph) {
                    Some(codes) if !codes.is_empty() => java_char(codes[0]).map(Some),
                    _ => Ok(None),
                }
            }
        }
    }

    /// `PDType0Font.codeToCID`.
    fn code_to_cid(&self, code: u32) -> u32 {
        let FontKind::Type0(type0) = &self.kind else {
            return code;
        };
        let Some(cmap) = &type0.cmap else {
            return 0;
        };
        if !cmap.has_cid_mappings() && cmap.has_unicode_mappings() {
            if let Some(unicode) = cmap.to_unicode(code) {
                return unicode.chars().next().map_or(0, u32::from);
            }
        }
        cmap.to_cid(code)
    }

    /// `codeToGID` of a TrueType font or a CIDFontType2 descendant.
    fn code_to_gid(&self, code: u32) -> Result<u32, Unported> {
        match &self.kind {
            FontKind::Simple(simple) => {
                let SimpleKind::TrueType(program) = &simple.kind else {
                    return Err(Unported(
                        "a glyph id asked of a font that is not TrueType, which PDFBox has none \
                         for"
                        .to_owned(),
                    ));
                };
                let encoding = simple.encoding.as_deref().ok_or_else(fallback)?;
                Ok(true_type_code_to_gid(
                    program.font()?,
                    encoding,
                    simple.symbolic,
                    code,
                ))
            }
            FontKind::Type0(type0) => {
                let descendant = &type0.descendant;
                let CidKind::TrueType(program) = &descendant.kind else {
                    return Err(Unported(
                        "a glyph id asked of a compact composite font, which the port does not \
                         map"
                        .to_owned(),
                    ));
                };
                let cid = self.code_to_cid(code);
                let Program::Embedded(ttf) = program else {
                    let Some(unicode) = self.to_unicode(code, false)? else {
                        return Ok(cid);
                    };
                    let cmap = fallback_font()?.unicode_cmap().ok_or_else(|| {
                        Unported("the fallback font without a Unicode cmap".to_owned())
                    })?;
                    let first = unicode.chars().next().map_or(0, u32::from);
                    return Ok(cmap.glyph(first));
                };
                if let Some(map) = &descendant.cid_to_gid {
                    return Ok(map.get(cid as usize).copied().unwrap_or(0));
                }
                Ok(if cid < u32::from(ttf.num_glyphs) {
                    cid
                } else {
                    0
                })
            }
        }
    }

    /// `getWidth`: a code's advance in glyph space.
    pub(super) fn width(&self, code: u32) -> Result<f32, Unported> {
        match &self.kind {
            FontKind::Type0(type0) => {
                let cid = self.code_to_cid(code) as i32;
                Ok(type0.descendant.width_for_cid(cid))
            }
            FontKind::Simple(SimpleFont {
                kind: SimpleKind::Type3 { .. },
                ..
            }) => {
                if !self.widths.values.is_empty()
                    && code as i32 >= self.widths.first_char
                    && code as i32 <= self.widths.last_char
                {
                    let index = (code as i32 - self.widths.first_char) as usize;
                    return Ok(self
                        .widths
                        .values
                        .get(index)
                        .map_or(0.0, |width| width.unwrap_or(0.0)));
                }
                match &self.descriptor {
                    Some(descriptor) => Ok(descriptor.missing_width),
                    None => self.width_from_font(code),
                }
            }
            FontKind::Simple(_) => {
                if self.widths.declared {
                    if let Some(width) = self.widths.width(code as i32) {
                        return Ok(width);
                    }
                    if let Some(descriptor) = &self.descriptor {
                        return Ok(descriptor.missing_width);
                    }
                }
                if self.is_standard14() {
                    return self.standard14_width(code);
                }
                self.width_from_font(code)
            }
        }
    }

    /// `PDSimpleFont.getStandard14Width`.
    fn standard14_width(&self, code: u32) -> Result<f32, Unported> {
        let (FontKind::Simple(simple), Some(afm)) = (&self.kind, self.afm) else {
            return Err(Unported("a standard 14 font without metrics".to_owned()));
        };
        let encoding = simple.encoding.as_deref().ok_or_else(fallback)?;
        let name = match encoding.name(code as i32) {
            ".notdef" => return Ok(250.0),
            "nbspace" => "space",
            "sfthyphen" => "hyphen",
            other => other,
        };
        Ok(afm.character_width(name))
    }

    /// `getWidthFromFont`: a code's advance from the font program.
    fn width_from_font(&self, code: u32) -> Result<f32, Unported> {
        match &self.kind {
            FontKind::Simple(simple) => match &simple.kind {
                SimpleKind::TrueType(program) => {
                    let glyph = self.code_to_gid(code)?;
                    Ok(scaled_advance(program.font()?, glyph))
                }
                SimpleKind::Type1(generic) | SimpleKind::Type1C(generic) => {
                    let compact = matches!(simple.kind, SimpleKind::Type1C(_));
                    let encoding = simple.encoding.as_deref().ok_or_else(fallback)?;
                    let name = self.name_in_font(simple, generic, encoding.name(code as i32))?;
                    if !compact && !simple.embedded && name == ".notdef" {
                        return Ok(250.0);
                    }
                    let width = generic.width(&name)?;
                    let [a, _, _, _, e, _] = generic.font_matrix()?;
                    Ok((f64::from(width) * (f64::from(a) * 1000.0) + f64::from(e)) as f32)
                }
                SimpleKind::Type3 { procs, .. } => {
                    let (Some(encoding), Some(procs)) = (simple.encoding.as_deref(), procs) else {
                        return Ok(0.0);
                    };
                    let name = encoding.name(code as i32);
                    match procs.iter().find(|proc| proc.name == name) {
                        Some(proc) if proc.length != 0 => proc.width(),
                        _ => Ok(0.0),
                    }
                }
            },
            FontKind::Type0(type0) => match &type0.descendant.kind {
                CidKind::TrueType(program) => {
                    Ok(scaled_advance(program.font()?, self.code_to_gid(code)?))
                }
                CidKind::Compact(program) => {
                    let (width, matrix) = match program {
                        Program::Embedded(cff) => {
                            let width = cff.cid_width(self.code_to_cid(code))?;
                            let matrix = match cff.font_matrix[..] {
                                [a, _, _, _, e, _] if cff.font_matrix.len() == 6 => {
                                    [a.float_value(), e.float_value()]
                                }
                                _ => [0.001, 0.0],
                            };
                            (width, matrix)
                        }
                        Program::Fallback => {
                            let font = fallback_font()?;
                            let name = match self.to_unicode(code, false)? {
                                Some(unicode) => unicode
                                    .chars()
                                    .next()
                                    .map_or(".notdef".to_owned(), uni_name),
                                None => ".notdef".to_owned(),
                            };
                            let width = f32::from(font.advance_width(font.name_to_glyph(&name)));
                            let scale = 0.001f32 * (1000.0f32 / f32::from(font.units_per_em));
                            (width, [scale, 0.0])
                        }
                    };
                    let [a, e] = matrix;
                    Ok((f64::from(width) * (f64::from(a) * 1000.0) + f64::from(e)) as f32)
                }
            },
        }
    }

    /// `getDisplacement`: how far a glyph moves the text position, in text space.
    pub(super) fn displacement(&self, code: u32) -> Result<(f32, f32), Unported> {
        match &self.kind {
            FontKind::Type0(type0) if self.is_vertical() => {
                let cid = self.code_to_cid(code) as i32;
                Ok((0.0, type0.descendant.vertical_displacement(cid) / 1000.0))
            }
            FontKind::Simple(SimpleFont {
                kind: SimpleKind::Type3 { matrix, .. },
                ..
            }) => Ok(matrix.transform_point(self.width(code)?, 0.0)),
            _ => Ok((self.width(code)? / 1000.0, 0.0)),
        }
    }

    /// `getPositionVector` of a vertical font, in text space.
    pub(super) fn position_vector(&self, code: u32) -> Result<(f32, f32), Unported> {
        let FontKind::Type0(type0) = &self.kind else {
            return Err(Unported(
                "a horizontal font asked for a position vector, which PDFBox throws on".to_owned(),
            ));
        };
        let cid = self.code_to_cid(code) as i32;
        let (x, y) = type0.descendant.position_vector(cid);
        let scale = -1.0 / 1000.0f32;
        Ok((x * scale, y * scale))
    }

    /// The units per em of a font's TrueType program, which `LegacyPDFStreamEngine` scales
    /// vertical glyph widths by.
    pub(super) fn true_type_units_per_em(&self) -> Result<Option<u16>, Unported> {
        let program = match &self.kind {
            FontKind::Simple(SimpleFont {
                kind: SimpleKind::TrueType(program),
                ..
            }) => program,
            FontKind::Type0(type0) => match &type0.descendant.kind {
                CidKind::TrueType(program) => program,
                CidKind::Compact(_) => return Ok(None),
            },
            FontKind::Simple(_) => return Ok(None),
        };
        Ok(Some(program.font()?.units_per_em))
    }

    /// `getFontDescriptor`.
    pub(super) fn descriptor(&self) -> Option<&Descriptor> {
        match &self.kind {
            FontKind::Type0(type0) => type0.descendant.descriptor.as_ref(),
            FontKind::Simple(_) => self.descriptor.as_ref(),
        }
    }

    /// `getBoundingBox`.
    fn bounding_box(&self) -> Result<[f32; 4], Unported> {
        let nonzero = |bbox: &[f32; 4]| bbox.iter().any(|&value| !jfloat::same(value, 0.0));
        match &self.kind {
            FontKind::Simple(simple) => match &simple.kind {
                SimpleKind::TrueType(program) => {
                    if let Some(bbox) = self.descriptor.as_ref().and_then(|d| d.font_bbox) {
                        return Ok(bbox);
                    }
                    Ok(program.font()?.font_bbox())
                }
                SimpleKind::Type1(generic) | SimpleKind::Type1C(generic) => {
                    match self.descriptor.as_ref().and_then(|d| d.font_bbox) {
                        Some(bbox) if nonzero(&bbox) => Ok(bbox),
                        _ => generic.font_bbox(),
                    }
                }
                SimpleKind::Type3 { bbox, procs, .. } => match bbox {
                    None => Ok([0.0; 4]),
                    Some(bbox) if nonzero(bbox) => Ok(*bbox),
                    Some(bbox) => {
                        let mut rect = *bbox;
                        for proc in procs.iter().flatten() {
                            if let Some(glyph) = proc.glyph_bbox()? {
                                rect = [
                                    jfloat::min(rect[0], glyph[0]),
                                    jfloat::min(rect[1], glyph[1]),
                                    jfloat::max(rect[2], glyph[2]),
                                    jfloat::max(rect[3], glyph[3]),
                                ];
                            }
                        }
                        Ok(rect)
                    }
                },
            },
            FontKind::Type0(type0) => {
                if let Some(bbox) = type0
                    .descendant
                    .descriptor
                    .as_ref()
                    .and_then(|d| d.font_bbox)
                    .filter(nonzero)
                {
                    return Ok(bbox);
                }
                match &type0.descendant.kind {
                    CidKind::TrueType(program) => Ok(program.font()?.font_bbox()),
                    CidKind::Compact(Program::Embedded(cff)) => Ok(match cff.font_bbox[..] {
                        [a, b, c, d, ..] => [a, b, c, d].map(cff::Number::float_value),
                        _ => [0.0; 4],
                    }),
                    CidKind::Compact(Program::Fallback) => Ok(fallback_font()?.font_bbox()),
                }
            }
        }
    }

    /// `LegacyPDFStreamEngine.computeFontHeight`, computed once as its `fontHeightMap` keeps it:
    /// the height of the font's glyphs in text space.
    pub(super) fn height(&self) -> Result<f32, Unported> {
        if let Some(height) = self.height.get() {
            return Ok(*height);
        }
        let height = self.compute_height()?;
        Ok(*self.height.get_or_init(|| height))
    }

    fn compute_height(&self) -> Result<f32, Unported> {
        let mut bbox = self.bounding_box()?;
        if bbox[1] < f32::from(i16::MIN) {
            bbox[1] = -(bbox[1] + 65536.0);
        }
        let mut glyph_height = (bbox[3] - bbox[1]) / 2.0;
        if let Some(descriptor) = self.descriptor() {
            let cap_height = descriptor.cap_height;
            if !jfloat::same(cap_height, 0.0)
                && (cap_height < glyph_height || jfloat::same(glyph_height, 0.0))
            {
                glyph_height = cap_height;
            }
            let ascent = descriptor.ascent;
            let descent = descriptor.descent;
            if cap_height > ascent
                && ascent > 0.0
                && descent < 0.0
                && ((ascent - descent) / 2.0 < glyph_height || jfloat::same(glyph_height, 0.0))
            {
                glyph_height = (ascent - descent) / 2.0;
            }
        }
        Ok(match self.type3_matrix() {
            Some(matrix) => matrix.transform_point(0.0, glyph_height).1,
            None => glyph_height / 1000.0,
        })
    }

    /// `getAverageFontWidth`. PDFBox's `PDType1CFont` takes 500 whatever the font's widths
    /// (#109); the port averages its `/Widths` as for other simple fonts, taking 500 only
    /// where they have no positive width.
    pub(super) fn average_width(&self) -> f32 {
        match &self.kind {
            FontKind::Type0(type0) => type0.descendant.average_width(),
            FontKind::Simple(SimpleFont {
                kind: SimpleKind::Type1C(_),
                ..
            }) if self.widths.average <= 0.0 => 500.0,
            FontKind::Simple(SimpleFont {
                kind: SimpleKind::Type1(_),
                ..
            }) if self.afm.is_some() => self.afm.map_or(0.0, FontMetrics::average_character_width),
            FontKind::Simple(_) => self.widths.average,
        }
    }

    /// `PDFont.getSpaceWidth`, computed once.
    pub(super) fn space_width(&self) -> Result<f32, Unported> {
        if let Some(width) = self.space_width.get() {
            return Ok(*width);
        }
        let width = self.compute_space_width()?;
        Ok(*self.space_width.get_or_init(|| width))
    }

    fn compute_space_width(&self) -> Result<f32, Unported> {
        let mut width = -1.0f32;
        if let (Some(cmap), true) = (&self.to_unicode, self.has_to_unicode) {
            let mapping = cmap.space_mapping();
            if mapping > -1 {
                width = self.width(mapping as u32)?;
            }
        } else {
            if let Some(string_width) = self.string_width_of_space()? {
                width = string_width;
            }
            if width <= 0.0 {
                width = self.width(32)?;
            }
        }
        if width <= 0.0 {
            width = self.width_from_font(32)?;
            if width <= 0.0 {
                width = self.average_width();
            }
        }
        Ok(width)
    }

    /// `PDFont.getStringWidth(" ")`: `None` where encoding the space throws an exception
    /// `getSpaceWidth` catches.
    fn string_width_of_space(&self) -> Result<Option<f32>, Unported> {
        if let FontKind::Simple(SimpleFont {
            kind: SimpleKind::Type1C(generic),
            zapf_dingbats,
            ..
        }) = &self.kind
        {
            let Generic::Compact(program) = generic else {
                return Ok(Some(0.0));
            };
            let list: &GlyphList = if *zapf_dingbats {
                &encoding::ZAPF_DINGBATS
            } else {
                &encoding::ADOBE
            };
            let name = list.code_point_to_name(' ');
            if !program.has_glyph(name) {
                return Ok(None);
            }
            return program.width(name).map(Some);
        }
        let Some(bytes) = self.encode_space()? else {
            return Ok(None);
        };
        let mut width = 0.0f32;
        let mut rest = bytes.as_slice();
        while !rest.is_empty() {
            let code = self.read_code(rest)?;
            width += self.width(code.code)?;
            rest = &rest[code.length.min(rest.len()).max(1)..];
        }
        Ok(Some(width))
    }

    /// `getNameInFont` of a Type 1 font: the name a glyph has in the program PDFBox measures
    /// with, `.notdef` where a substitute has no glyph for it; `PDType1CFont` does not try the
    /// alternative ligature names `PDType1Font` does.
    fn name_in_font(
        &self,
        simple: &SimpleFont,
        generic: &Generic,
        name: &str,
    ) -> Result<String, Unported> {
        if simple.embedded || generic.has_glyph(name)? {
            return Ok(name.to_owned());
        }
        if matches!(simple.kind, SimpleKind::Type1(_)) {
            if let Some(alternative) = alternative_name(name) {
                if generic.has_glyph(alternative)? {
                    return Ok(alternative.to_owned());
                }
            }
        }
        let list: &GlyphList = if simple.zapf_dingbats {
            &encoding::ZAPF_DINGBATS
        } else {
            &encoding::ADOBE
        };
        if let Some(unicode) = list.to_unicode(name) {
            let mut chars = unicode.chars();
            if let (Some(only), None) = (chars.next(), chars.next()) {
                if only.len_utf16() == 1 {
                    let uni = uni_name(only);
                    if generic.has_glyph(&uni)? {
                        return Ok(uni);
                    }
                }
            }
        }
        Ok(".notdef".to_owned())
    }

    /// `encode(32)`: the bytes of a space in the font, or `None` where PDFBox throws an
    /// `IllegalArgumentException` or an `UnsupportedOperationException`.
    fn encode_space(&self) -> Result<Option<Vec<u8>>, Unported> {
        match &self.kind {
            FontKind::Simple(simple) => {
                let list: &GlyphList = if simple.zapf_dingbats {
                    &encoding::ZAPF_DINGBATS
                } else {
                    &encoding::ADOBE
                };
                let name = list.code_point_to_name(' ');
                match &simple.kind {
                    SimpleKind::Type3 { .. } => Ok(None),
                    SimpleKind::Type1C(_) => Err(Unported(
                        "a compact Type 1 font encoding text, which the port does not do"
                            .to_owned(),
                    )),
                    SimpleKind::Type1(generic) => {
                        let encoding = simple.encoding.as_deref().ok_or_else(fallback)?;
                        if !encoding.contains(name) {
                            return Ok(None);
                        }
                        if self.is_standard14() {
                            if name == ".notdef" {
                                return Ok(None);
                            }
                        } else {
                            let in_font = self.name_in_font(simple, generic, name)?;
                            if in_font == ".notdef" || !generic.has_glyph(&in_font)? {
                                return Ok(None);
                            }
                        }
                        Ok(encoding.first_code(name)?.map(|code| vec![code as u8]))
                    }
                    SimpleKind::TrueType(program) => {
                        let ttf = program.font()?;
                        let encoding = simple.encoding.as_deref().ok_or_else(|| {
                            Unported(
                                "a TrueType font without an encoding, which PDFBox encodes \
                                 through the font's own"
                                    .to_owned(),
                            )
                        })?;
                        if !encoding.contains(name)
                            || (ttf.name_to_glyph(name) == 0 && ttf.name_to_glyph("uni0020") == 0)
                        {
                            return Ok(None);
                        }
                        Ok(encoding.code(name).map(|code| vec![code as u8]))
                    }
                }
            }
            FontKind::Type0(type0) => self.encode_composite_space(type0),
        }
    }

    /// `PDCIDFontType0.encode(32)`, which throws an `UnsupportedOperationException`, or
    /// `PDCIDFontType2.encode(32)`: the CID of a space, two bytes, as the program's Unicode cmap
    /// or the UCS-2 map gives it, or the code the `ToUnicode` map gives a space; `None` where
    /// PDFBox throws an `IllegalArgumentException` for a CID of 0.
    fn encode_composite_space(&self, type0: &Type0Font) -> Result<Option<Vec<u8>>, Unported> {
        let CidKind::TrueType(program) = &type0.descendant.kind else {
            return Ok(None);
        };
        let unicode_cmap = program.font()?.unicode_cmap();
        let cid = if type0.descendant.embedded {
            let name = type0
                .cmap
                .as_ref()
                .and_then(|cmap| cmap.name.as_deref())
                .ok_or_else(|| {
                    Unported(
                        "an embedded composite TrueType font whose CMap has no name, whose space \
                         PDFBox fails to encode with a NullPointerException"
                            .to_owned(),
                    )
                })?;
            let cid = if name.starts_with("Identity-") {
                unicode_cmap.map(|cmap| cmap.glyph(u32::from(b' ')))
            } else {
                type0.ucs2.map(|ucs2| ucs2.to_cid(u32::from(b' ')))
            };
            match cid {
                Some(cid) => cid,
                None => {
                    if let Some(to_unicode) = &self.to_unicode {
                        if let Some(codes) = to_unicode.codes_from_unicode(" ")? {
                            return Ok(Some(codes.to_vec()));
                        }
                    }
                    0
                }
            }
        } else {
            unicode_cmap
                .ok_or_else(|| {
                    Unported(
                        "a composite TrueType font whose fallback has no Unicode cmap, whose \
                         space PDFBox fails to encode with a NullPointerException"
                            .to_owned(),
                    )
                })?
                .glyph(u32::from(b' '))
        };
        if cid == 0 {
            return Ok(None);
        }
        Ok(Some(vec![(cid >> 8) as u8, cid as u8]))
    }
}

/// `PDFontFactory.getFontTypeFromFont` for a composite font: the descendant subtype its font
/// program's first bytes call for, which PDFBox puts in place of the declared one.
fn sniffed_cid_subtype(
    pdf: &Pdf,
    font: &Dictionary,
    descendant: &Dictionary,
) -> Result<Option<&'static [u8]>, Unported> {
    let Some(fd) = pdf
        .dict(font, b"FontDescriptor")
        .or_else(|| pdf.dict(descendant, b"FontDescriptor"))
    else {
        return Ok(None);
    };
    let Some(stream) = pdf
        .stream(fd, b"FontFile")
        .or_else(|| pdf.stream(fd, b"FontFile2"))
        .or_else(|| pdf.stream(fd, b"FontFile3"))
    else {
        return Ok(None);
    };
    let Ok(data) = pdf.decode(stream) else {
        return Ok(None);
    };
    let mut header = [0u8; 4];
    for (target, &byte) in header.iter_mut().zip(&data) {
        *target = byte;
    }
    let true_type = header == [0, 1, 0, 0] || &header == b"true" || &header == b"ttcf";
    let open_type = &header == b"OTTO";
    let type1 = header.starts_with(b"%!") || (header[0] == 0x80 && matches!(header[1], 1 | 2));
    let cff = (1..=127).contains(&header[0]) && (1..=4).contains(&header[3]);
    Ok(if true_type || open_type {
        Some(b"CIDFontType2")
    } else if type1 || cff {
        Some(b"CIDFontType0")
    } else {
        None
    })
}

/// `(char) code` as a string: the low 16 bits of the code as one UTF-16 unit, unported when
/// that unit is half of a surrogate pair.
fn java_char(code: u32) -> Result<GlyphText, Unported> {
    char::from_u32(code & 0xFFFF)
        .map(GlyphText::from)
        .ok_or_else(|| Unported("a glyph mapped to half of a UTF-16 surrogate pair".to_owned()))
}

/// `getWidthFromFont` of a TrueType program: the glyph's advance at 1000 units per em.
fn scaled_advance(ttf: &TrueType, glyph: u32) -> f32 {
    let mut width = f32::from(ttf.advance_width(glyph));
    let units = f32::from(ttf.units_per_em);
    if !jfloat::same(units, 1000.0) {
        width *= 1000.0 / units;
    }
    width
}

/// `PDTrueTypeFont.codeToGID` while the encoding is being read: PDFBox reads the encoding of a
/// font it has not yet settled as symbolic or not, with no encoding, which it treats as
/// symbolic.
fn true_type_code_to_gid_symbolic_unsettled(ttf: &TrueType, code: i32) -> u32 {
    symbolic_code_to_gid(ttf, None, code)
}

/// `PDTrueTypeFont.codeToGID`.
fn true_type_code_to_gid(ttf: &TrueType, encoding: &Encoding, symbolic: bool, code: u32) -> u32 {
    let code = code as i32;
    if !symbolic {
        let name = encoding.name(code);
        if name == ".notdef" {
            return 0;
        }
        let mut glyph = 0;
        if let Some(cmap) = ttf
            .subtable(
                truetype::PLATFORM_WINDOWS,
                truetype::ENCODING_WIN_UNICODE_BMP,
            )
            .or_else(|| windows_unicode(ttf))
        {
            if let Some(unicode) = encoding::ADOBE.to_unicode(name) {
                if let Some(first) = unicode.chars().next() {
                    glyph = cmap.glyph(u32::from(first));
                }
            }
        }
        if glyph == 0 {
            if let Some(cmap) = mac_roman(ttf) {
                if let Some(mac_code) = Builtin::MacOsRoman.encoding().code(name) {
                    glyph = cmap.glyph(mac_code as u32);
                }
            }
        }
        if glyph == 0 {
            glyph = ttf.name_to_glyph(name);
        }
        return glyph;
    }
    symbolic_code_to_gid(ttf, Some(encoding), code)
}

fn symbolic_code_to_gid(ttf: &TrueType, encoding: Option<&Encoding>, code: i32) -> u32 {
    let mut glyph = 0;
    if let Some(cmap) = windows_unicode(ttf) {
        match encoding {
            Some(encoding)
                if matches!(
                    encoding.kind,
                    EncodingKind::Builtin(
                        Builtin::WinAnsi | Builtin::MacRoman | Builtin::MacOsRoman
                    )
                ) =>
            {
                let name = encoding.name(code);
                if name == ".notdef" {
                    return 0;
                }
                if let Some(first) = encoding::ADOBE
                    .to_unicode(name)
                    .and_then(|u| u.chars().next())
                {
                    glyph = cmap.glyph(u32::from(first));
                }
            }
            _ => glyph = cmap.glyph(code as u32),
        }
    }
    if glyph == 0 {
        if let Some(cmap) = windows_symbol(ttf) {
            glyph = cmap.glyph(code as u32);
            if (0..=0xFF).contains(&code) {
                for base in [0xF000, 0xF100, 0xF200] {
                    if glyph == 0 {
                        glyph = cmap.glyph(code as u32 + base);
                    }
                }
            }
        }
    }
    if glyph == 0 {
        if let Some(cmap) = mac_roman(ttf) {
            glyph = cmap.glyph(code as u32);
        }
    }
    glyph
}

/// `extractCmapTable`: the last subtable of each kind PDFBox picks.
fn picked(ttf: &TrueType, kind: fn(u16, u16) -> bool) -> Option<&truetype::CmapSubtable> {
    ttf.subtables()?
        .iter()
        .rfind(|cmap| kind(cmap.platform_id, cmap.encoding_id))
}

fn windows_unicode(ttf: &TrueType) -> Option<&truetype::CmapSubtable> {
    picked(ttf, |platform, encoding| {
        (platform == truetype::PLATFORM_WINDOWS && encoding == truetype::ENCODING_WIN_UNICODE_BMP)
            || (platform == truetype::PLATFORM_UNICODE
                && (encoding == truetype::ENCODING_UNICODE_1_0
                    || encoding == truetype::ENCODING_UNICODE_2_0_BMP))
    })
}

fn windows_symbol(ttf: &TrueType) -> Option<&truetype::CmapSubtable> {
    picked(ttf, |platform, encoding| {
        platform == truetype::PLATFORM_WINDOWS && encoding == truetype::ENCODING_WIN_SYMBOL
    })
}

fn mac_roman(ttf: &TrueType) -> Option<&truetype::CmapSubtable> {
    picked(ttf, |platform, encoding| {
        platform == truetype::PLATFORM_MACINTOSH && encoding == truetype::ENCODING_MAC_ROMAN
    })
}

/// Reads an embedded TrueType program: the font, or `None` when it is damaged to PDFBox.
fn read_true_type(data: &[u8]) -> Result<Option<Box<TrueType>>, Unported> {
    match TrueType::read(data) {
        Ok(ttf) => Ok(Some(Box::new(ttf))),
        Err(truetype::Failure::Damaged(_) | truetype::Failure::PostScript) => Ok(None),
        Err(truetype::Failure::GlyphOutOfBounds { glyph }) => Err(Unported(format!(
            "a font program whose cmap maps a code to glyph {glyph}, one past the last, where \
             FontBox throws an ArrayIndexOutOfBoundsException"
        ))),
    }
}

/// `PDType0Font.fetchCMapUCS2`'s map: the predefined CMap `source` names, whose registry and
/// ordering name the map from CIDs to Unicode; `None` where FontBox bundles either not.
fn ucs2_map(source: &str) -> Result<Option<&'static CMap>, Unported> {
    let found = |name: &str| match cmap::predefined(name) {
        Ok(map) => Ok(Some(map)),
        Err(CMapError::Malformed(_)) => Ok(None),
        Err(CMapError::Unported(unported)) => Err(unported),
    };
    let Some(predefined) = found(source)? else {
        return Ok(None);
    };
    let java = |part: &Option<String>| part.clone().unwrap_or_else(|| "null".to_owned());
    found(&format!(
        "{}-{}-UCS2",
        java(&predefined.registry),
        java(&predefined.ordering)
    ))
}

/// `PDFont.loadUnicodeCmap`: the `ToUnicode` map, `None` where PDFBox fails to read it, or the
/// predefined Identity map PDFBox puts in place of one without Unicode mappings.
fn load_to_unicode(
    pdf: &Pdf,
    dict: &Dictionary,
    encoding_name: Option<&str>,
) -> Result<Option<CMap>, Unported> {
    let parsed = match pdf.get(dict, b"ToUnicode") {
        None => return Ok(None),
        Some(Object::Name(name)) => match cmap::predefined(&name_string(name)) {
            Ok(cmap) => Ok(cmap.clone()),
            Err(error) => Err(error),
        },
        Some(Object::Stream(stream)) => match pdf.decode(stream) {
            Ok(data) => cmap::parse(&data),
            Err(_) => return Ok(None),
        },
        Some(_) => return Ok(None),
    };
    let cmap = match parsed {
        Ok(cmap) => cmap,
        Err(CMapError::Malformed(_)) => return Ok(None),
        Err(CMapError::Unported(unported)) => return Err(unported),
    };
    if cmap.has_unicode_mappings() {
        return Ok(Some(cmap));
    }
    let name = cmap.name.as_deref().unwrap_or_default();
    let ordering = cmap.ordering.as_deref().unwrap_or_default();
    let identity = name.contains("Identity")
        || ordering.contains("Identity")
        || matches!(encoding_name, Some("Identity-H" | "Identity-V"));
    if identity {
        let differences = pdf
            .dict(dict, b"Encoding")
            .is_some_and(|encoding| encoding.has(b"Differences"));
        if !differences {
            return match cmap::predefined("Identity-H") {
                Ok(identity) => Ok(Some(identity.clone())),
                Err(CMapError::Unported(unported)) => Err(unported),
                Err(CMapError::Malformed(reason)) => Err(Unported(reason)),
            };
        }
    }
    Ok(Some(cmap))
}

impl CidFont {
    /// `new PDCIDFontType2`.
    fn true_type(pdf: &Pdf, dict: &Dictionary) -> Result<CidFont, Unported> {
        let mut font = Self::common(pdf, dict);
        let fd = pdf.dict(dict, b"FontDescriptor");
        let stream = fd.and_then(|fd| {
            pdf.stream(fd, b"FontFile2")
                .or_else(|| pdf.stream(fd, b"FontFile3"))
                .or_else(|| pdf.stream(fd, b"FontFile"))
        });
        if let Some(stream) = stream {
            let data = pdf.decode(stream)?;
            if data.starts_with(b"OTTO") {
                pdf.differ(Difference(
                    "an OpenType font program, which PDFBox reads with OTFParser and the port \
                     takes for damaged"
                        .to_owned(),
                ));
                font.damaged = true;
            } else {
                match read_true_type(&data)? {
                    Some(ttf) => {
                        font.kind = CidKind::TrueType(Program::Embedded(ttf));
                        font.embedded = true;
                    }
                    None => font.damaged = true,
                }
            }
        }
        font.cid_to_gid = match pdf.stream(dict, b"CIDToGIDMap") {
            Some(stream) => {
                let bytes = pdf.decode(stream)?;
                Some(
                    bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| u32::from(u16::from_be_bytes(*pair)))
                        .collect(),
                )
            }
            None => None,
        };
        Ok(font)
    }

    /// `new PDCIDFontType0`: a composite font's CFF program, which PDFBox calls damaged when it
    /// fails to parse or starts as a Type 1 program does.
    fn compact(pdf: &Pdf, dict: &Dictionary) -> Result<CidFont, Unported> {
        let mut font = Self::common(pdf, dict);
        font.kind = CidKind::Compact(Program::Fallback);
        if let Some(stream) = pdf
            .dict(dict, b"FontDescriptor")
            .and_then(|fd| pdf.stream(fd, b"FontFile3"))
        {
            let bytes = pdf.decode(stream)?;
            if bytes.first() == Some(&b'%') {
                font.damaged = true;
                return Ok(font);
            }
            match cff::read(&bytes) {
                cff::Read::Font(program) => {
                    font.kind = CidKind::Compact(Program::Embedded(program));
                    font.embedded = true;
                }
                cff::Read::Damaged => font.damaged = true,
                cff::Read::Unported(reason) => return Err(Unported(reason)),
            }
        }
        Ok(font)
    }

    /// `PDCIDFont(COSDictionary, PDType0Font)`: the widths and vertical metrics.
    fn common(pdf: &Pdf, dict: &Dictionary) -> CidFont {
        let mut font = CidFont {
            descriptor: pdf
                .dict(dict, b"FontDescriptor")
                .map(|fd| Descriptor::read(pdf, fd)),
            widths: FastMap::default(),
            width_order: Vec::new(),
            default_width: pdf.get(dict, b"DW").and_then(float_value).unwrap_or(1000.0),
            dw2: [880.0, -1000.0],
            vertical: FastMap::default(),
            vertical_ranges: Vec::new(),
            kind: CidKind::TrueType(Program::Fallback),
            cid_to_gid: None,
            embedded: false,
            damaged: false,
            registry: None,
            ordering: None,
            supplement: None,
        };
        if let Some(info) = pdf.dict(dict, b"CIDSystemInfo") {
            font.registry = pdf.text(info, b"Registry");
            font.ordering = pdf.text(info, b"Ordering");
            font.supplement = Some(pdf.int(info, b"Supplement", 0));
        }
        if let Some(array) = pdf.array(dict, b"W") {
            font.read_widths(pdf, array);
        }
        if let Some(dw2) = pdf.array(dict, b"DW2") {
            let first = dw2
                .first()
                .and_then(|o| pdf.resolve(o))
                .and_then(float_value);
            let second = dw2
                .get(1)
                .and_then(|o| pdf.resolve(o))
                .and_then(float_value);
            if let (Some(first), Some(second)) = (first, second) {
                font.dw2 = [first, second];
            }
        }
        if let Some(w2) = pdf.array(dict, b"W2") {
            font.read_vertical(pdf, w2);
        }
        font
    }

    fn put_width(&mut self, cid: i32, width: f32) {
        self.widths.insert(cid, width);
        self.width_order.push((cid, width));
    }

    /// `PDCIDFont.readWidths`.
    fn read_widths(&mut self, pdf: &Pdf, array: &[Object]) {
        let resolved: Vec<Option<&Object>> = array.iter().map(|o| pdf.resolve(o)).collect();
        let size = resolved.len();
        let mut counter = 0;
        while counter + 1 < size {
            let first = resolved[counter];
            counter += 1;
            let Some(first) = first.and_then(int_value) else {
                continue;
            };
            let next = resolved[counter];
            counter += 1;
            if let Some(Object::Array(widths)) = next {
                for (offset, width) in widths.iter().enumerate() {
                    if let Some(width) = pdf.resolve(width).and_then(float_value) {
                        self.put_width(first.wrapping_add(offset as i32), width);
                    }
                }
            } else {
                if counter >= size {
                    break;
                }
                let width = resolved[counter];
                counter += 1;
                let (Some(last), Some(width)) =
                    (next.and_then(int_value), width.and_then(float_value))
                else {
                    continue;
                };
                let mut cid = first;
                while cid <= last {
                    self.put_width(cid, width);
                    if cid == i32::MAX {
                        break;
                    }
                    cid += 1;
                }
            }
        }
    }

    /// `PDCIDFont.readVerticalDisplacements`, for `/W2`.
    fn read_vertical(&mut self, pdf: &Pdf, array: &[Object]) {
        let number = |index: usize| {
            array
                .get(index)
                .and_then(|o| pdf.resolve(o))
                .and_then(float_value)
        };
        let mut index = 0;
        while index < array.len() {
            let Some(first) = array
                .get(index)
                .and_then(|o| pdf.resolve(o))
                .and_then(int_value)
            else {
                break;
            };
            index += 1;
            match array.get(index).and_then(|o| pdf.resolve(o)) {
                Some(Object::Array(values)) => {
                    let value = |i: usize| {
                        values
                            .get(i)
                            .and_then(|o| pdf.resolve(o))
                            .and_then(float_value)
                    };
                    let mut j = 0;
                    while j < values.len() {
                        let cid = first + (j / 3) as i32;
                        let (Some(w1y), Some(v1x), Some(v1y)) =
                            (value(j), value(j + 1), value(j + 2))
                        else {
                            break;
                        };
                        self.vertical.insert(cid, (w1y, v1x, v1y));
                        j += 3;
                    }
                }
                Some(other) => {
                    let Some(last) = int_value(other) else {
                        break;
                    };
                    let (Some(w1y), Some(v1x), Some(v1y)) =
                        (number(index + 1), number(index + 2), number(index + 3))
                    else {
                        break;
                    };
                    self.vertical_ranges.push((first, last, w1y, v1x, v1y));
                    index += 3;
                }
                None => break,
            }
            index += 1;
        }
    }

    /// `getWidthForCID`.
    fn width_for_cid(&self, cid: i32) -> f32 {
        self.widths.get(&cid).copied().unwrap_or(self.default_width)
    }

    /// `PDCIDFont.getAverageFontWidth`: the positive widths averaged in the order a Java
    /// `HashMap<Integer, Float>` iterates them. PDFBox's `PDCIDFontType0` takes 500 whatever
    /// its widths (#109); the port averages them as for other CID fonts.
    fn average_width(&self) -> f32 {
        let mut total = 0.0f32;
        let mut count = 0;
        for (_, width) in encoding::java_integer_map_order(&self.width_order) {
            if *width > 0.0 {
                total += width;
                count += 1;
            }
        }
        let mut average = 0.0f32;
        if count != 0 {
            average = total / count as f32;
        }
        if average <= 0.0 || average.is_nan() {
            average = self.default_width;
        }
        average
    }

    fn vertical_metrics(&self, cid: i32) -> Option<(f32, f32, f32)> {
        self.vertical.get(&cid).copied().or_else(|| {
            self.vertical_ranges
                .iter()
                .find(|(first, last, ..)| cid >= *first && cid <= *last)
                .map(|&(_, _, w1y, v1x, v1y)| (w1y, v1x, v1y))
        })
    }

    /// `getVerticalDisplacementVectorY`.
    fn vertical_displacement(&self, cid: i32) -> f32 {
        self.vertical_metrics(cid)
            .map_or(self.dw2[1], |(w1y, _, _)| w1y)
    }

    /// `PDCIDFont.getPositionVector`.
    fn position_vector(&self, cid: i32) -> (f32, f32) {
        self.vertical_metrics(cid)
            .map_or((self.width_for_cid(cid) / 2.0, self.dw2[0]), |(_, x, y)| {
                (x, y)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_type3_glyph_boxes_as_d1_gives_them() {
        let proc = CharProc {
            name: "a".to_owned(),
            length: 0,
            first: Ok(Some((
                "d1".to_owned(),
                [520.0, 0.0, -0.1, 0.3, 480.7, 700.1]
                    .map(Object::Real)
                    .to_vec(),
            ))),
        };
        assert_eq!(proc.glyph_bbox().unwrap(), Some([-0.1, 0.3, 480.7, 700.1]));
    }

    #[test]
    fn coerces_codes_to_java_chars() {
        assert_eq!(java_char(0x41).unwrap().as_str(), "A");
        assert_eq!(java_char(0x10041).unwrap().as_str(), "A");
        assert!(java_char(0xD800).is_err());
    }
}
