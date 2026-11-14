//! Static text, mapped through the code tables of its fonts, and text fields.

use std::collections::HashMap;

use super::reader::Bytes;
use super::{
    html, write_lines, Movie, Tag, Warning, DEFINE_FONT_INFO_2, DEFINE_TEXT_2, NAME,
    UNICODE_VERSION,
};
use crate::formats::Failure;
use crate::text::charset::Charset;

/// The `DefineFontInfo` flag of codes Shift_JIS encodes, before SWF 6.
const FONT_INFO_SHIFT_JIS: u8 = 0x10;
/// The `DefineFontInfo` flag of codes of two bytes.
const FONT_INFO_WIDE_CODES: u8 = 0x01;
/// The `DefineFont2` flag of codes Shift_JIS encodes, before SWF 6.
const FONT_2_SHIFT_JIS: u8 = 0x40;
/// The `DefineFont2` flag of glyph offsets of four bytes.
const FONT_2_WIDE_OFFSETS: u8 = 0x08;
/// The `DefineFont2` flag of codes of two bytes.
const FONT_2_WIDE_CODES: u8 = 0x04;

/// The flag of a text record that is not the end of its text's records.
const RECORD_TYPE: u8 = 0x80;
const RECORD_HAS_FONT: u8 = 0x08;
const RECORD_HAS_COLOR: u8 = 0x04;
const RECORD_HAS_Y: u8 = 0x02;
const RECORD_HAS_X: u8 = 0x01;

const EDIT_HAS_TEXT: u8 = 0x80;
const EDIT_PASSWORD: u8 = 0x10;
const EDIT_HAS_TEXT_COLOR: u8 = 0x04;
const EDIT_HAS_MAX_LENGTH: u8 = 0x02;
const EDIT_HAS_FONT: u8 = 0x01;
/// The flags of `DefineEditText`'s second byte.
const EDIT_HAS_FONT_CLASS: u8 = 0x80;
const EDIT_HAS_LAYOUT: u8 = 0x20;
const EDIT_HTML: u8 = 0x02;
/// The bytes of a text field's layout: its alignment, margins, indent and leading.
const EDIT_LAYOUT_BYTES: usize = 9;

/// The widest bit field of a glyph entry.
const MAX_ENTRY_BITS: u32 = 32;

/// How the codes of a font are characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// Each code is its Unicode code point.
    Unicode,
    /// Each code is one byte, or two, big-endian, of a charset.
    Charset(Charset),
}

impl Encoding {
    /// The encoding of the codes, `wide` or not, of a font Shift_JIS encodes or not in a movie
    /// of `version`.
    fn of(version: u8, shift_jis: bool, wide: bool) -> Encoding {
        match (version >= UNICODE_VERSION, shift_jis, wide) {
            (true, _, _) | (false, false, true) => Encoding::Unicode,
            (false, true, _) => Encoding::Charset(Charset::named("Shift_JIS")),
            (false, false, false) => Encoding::Charset(Charset::named("windows-1252")),
        }
    }
}

/// A font's code table: the code of each of its glyphs, in its tag.
#[derive(Debug, Clone, Copy)]
struct Codes<'d> {
    table: &'d [u8],
    wide: bool,
    encoding: Encoding,
}

impl Codes<'_> {
    fn len(&self) -> usize {
        match self.wide {
            true => self.table.len() / 2,
            false => self.table.len(),
        }
    }

    /// The code of glyph `glyph`, if the font has the glyph.
    fn code(&self, glyph: usize) -> Option<u16> {
        match self.wide {
            true => {
                let at = glyph.checked_mul(2)?;
                let code = self.table.get(at..at.checked_add(2)?)?;
                Some(u16::from_le_bytes([code[0], code[1]]))
            }
            false => self.table.get(glyph).map(|&code| u16::from(code)),
        }
    }

    /// Writes the characters of `codes` to `text`.
    fn write(&self, codes: &[u16], text: &mut String) {
        match self.encoding {
            Encoding::Unicode => text.extend(codes.iter().map(|&code| {
                char::from_u32(u32::from(code)).unwrap_or(char::REPLACEMENT_CHARACTER)
            })),
            Encoding::Charset(charset) => {
                let mut bytes = Vec::with_capacity(codes.len() * 2);
                for &code in codes {
                    match u8::try_from(code) {
                        Ok(byte) => bytes.push(byte),
                        Err(_) => bytes.extend_from_slice(&code.to_be_bytes()),
                    }
                }
                text.push_str(
                    &charset
                        .decode(&bytes)
                        .expect("the JDK decoders decode windows-1252 and Shift_JIS"),
                );
            }
        }
    }
}

/// The fonts the movie has defined, by id, with their code tables: none for a `DefineFont`
/// until a `DefineFontInfo` gives it one.
#[derive(Debug, Default)]
pub(super) struct Fonts<'d>(HashMap<u16, Option<Codes<'d>>>);

impl<'d> Fonts<'d> {
    /// `DefineFont`: a font whose codes its `DefineFontInfo` gives.
    pub(super) fn define_font(&mut self, tag: Tag<'d>) -> Result<(), Failure> {
        let id = tag
            .bytes()
            .u16("font id")
            .map_err(|short| tag.short(short))?;
        self.0.insert(id, None);
        Ok(())
    }

    /// `DefineFontInfo` and `DefineFontInfo2`: the code table of a `DefineFont`, in a movie of
    /// `version`.
    pub(super) fn define_font_info(&mut self, tag: Tag<'d>, version: u8) -> Result<(), Failure> {
        let short = |short| tag.short(short);
        let mut bytes = tag.bytes();
        let id = bytes.u16("font id").map_err(short)?;
        let name = bytes.u8("font name length").map_err(short)?;
        bytes.skip(usize::from(name), "font name").map_err(short)?;
        let flags = bytes.u8("flags").map_err(short)?;
        if tag.code == DEFINE_FONT_INFO_2 {
            bytes.skip(1, "language code").map_err(short)?;
        }
        let table = bytes.rest();
        let wide = flags & FONT_INFO_WIDE_CODES != 0;
        if wide && !table.len().is_multiple_of(2) {
            return Err(tag.malformed(format_args!(
                "has a code table of two-byte codes of {} bytes",
                table.len()
            )));
        }
        let font = self.0.get_mut(&id).ok_or_else(|| {
            tag.malformed(format_args!(
                "gives the codes of the font {id}, which no DefineFont defines before it"
            ))
        })?;
        *font = Some(Codes {
            table,
            wide,
            encoding: Encoding::of(version, flags & FONT_INFO_SHIFT_JIS != 0, wide),
        });
        Ok(())
    }

    /// `DefineFont2` and `DefineFont3`, in a movie of `version`.
    pub(super) fn define_font_2(&mut self, tag: Tag<'d>, version: u8) -> Result<(), Failure> {
        let short = |short| tag.short(short);
        let mut bytes = tag.bytes();
        let id = bytes.u16("font id").map_err(short)?;
        let flags = bytes.u8("flags").map_err(short)?;
        bytes.skip(1, "language code").map_err(short)?;
        let name = bytes.u8("font name length").map_err(short)?;
        bytes.skip(usize::from(name), "font name").map_err(short)?;
        let glyphs = usize::from(bytes.u16("glyph count").map_err(short)?);
        let wide = flags & FONT_2_WIDE_CODES != 0;
        let table = match glyphs {
            0 => &[][..],
            _ => code_table(&tag, &mut bytes, glyphs, flags, wide)?,
        };
        self.0.insert(
            id,
            Some(Codes {
                table,
                wide,
                encoding: Encoding::of(version, flags & FONT_2_SHIFT_JIS != 0, wide),
            }),
        );
        Ok(())
    }
}

/// The code table of the `glyphs` glyphs of the `DefineFont2` or `DefineFont3` `tag` with
/// `flags`, whose offset table `bytes` is at, its codes `wide` or not.
fn code_table<'d>(
    tag: &Tag<'d>,
    bytes: &mut Bytes<'d>,
    glyphs: usize,
    flags: u8,
    wide: bool,
) -> Result<&'d [u8], Failure> {
    let short = |short| tag.short(short);
    let offsets = bytes.at();
    let code_offset = match flags & FONT_2_WIDE_OFFSETS != 0 {
        true => {
            bytes.skip(glyphs * 4, "offset table").map_err(short)?;
            bytes.u32("code table offset").map_err(short)?
        }
        false => {
            bytes.skip(glyphs * 2, "offset table").map_err(short)?;
            u32::from(bytes.u16("code table offset").map_err(short)?)
        }
    };
    let length = glyphs * if wide { 2 } else { 1 };
    let start = usize::try_from(code_offset)
        .ok()
        .and_then(|offset| offsets.checked_add(offset));
    start
        .and_then(|start| tag.body.get(start..start.checked_add(length)?))
        .ok_or_else(|| {
            tag.malformed(format_args!(
                "puts the code table of its {glyphs} glyphs at offset {code_offset}, which it \
                 does not hold"
            ))
        })
}

/// The lines of a static text, as its records place them.
#[derive(Debug, Default)]
struct Lines {
    done: Vec<String>,
    line: String,
    /// The Y offset of the line.
    y: Option<i16>,
    /// Where the glyphs set on the line so far end along it.
    end: Option<i64>,
}

impl Lines {
    /// Moves to where a text record of `height` sets its glyphs: a new line where it moves to
    /// another Y offset, a space where it moves along the line by more than a fifth of its
    /// height past the glyphs before it.
    fn move_to(&mut self, x: Option<i16>, y: Option<i16>, height: u16) {
        if let Some(y) = y.filter(|&y| self.y != Some(y)) {
            if !self.line.is_empty() {
                self.done.push(std::mem::take(&mut self.line));
            }
            self.y = Some(y);
            self.end = None;
        }
        let Some(x) = x.map(i64::from) else {
            return;
        };
        if let Some(end) = self.end {
            if (x - end) * 5 > i64::from(height)
                && self
                    .line
                    .chars()
                    .next_back()
                    .is_some_and(|c| !c.is_whitespace())
            {
                self.line.push(' ');
            }
        }
        self.end = Some(x);
    }

    fn advance(&mut self, by: i32) {
        self.end = Some(self.end.unwrap_or(0) + i64::from(by));
    }

    fn finish(mut self) -> Vec<String> {
        if !self.line.is_empty() {
            self.done.push(self.line);
        }
        self.done
    }
}

/// The lines of a text field's plain text, broken where it breaks them, with a carriage
/// return, a line feed or both.
fn plain_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
}

impl<'d> Movie<'d, '_, '_> {
    /// `DefineText` and `DefineText2`: a paragraph of the lines its records set.
    pub(super) fn define_text(&mut self, tag: Tag<'d>) -> Result<(), Failure> {
        let short = |short| tag.short(short);
        let mut bytes = tag.bytes();
        bytes.skip(2, "character id").map_err(short)?;
        bytes.skip_rect("bounds").map_err(short)?;
        bytes.skip_matrix("matrix").map_err(short)?;
        let glyph_bits = u32::from(bytes.u8("glyph bits").map_err(short)?);
        let advance_bits = u32::from(bytes.u8("advance bits").map_err(short)?);
        if glyph_bits > MAX_ENTRY_BITS || advance_bits > MAX_ENTRY_BITS {
            return Err(tag.malformed(format_args!(
                "packs glyph entries of {glyph_bits}-bit glyphs and {advance_bits}-bit \
                 advances, wider than {MAX_ENTRY_BITS} bits"
            )));
        }
        let color = match tag.code == DEFINE_TEXT_2 {
            true => 4,
            false => 3,
        };
        let mut lines = Lines::default();
        let (mut font, mut height) = (None, 0);
        let mut codes = Vec::new();
        while !bytes.is_empty() {
            let flags = bytes.u8("text record").map_err(short)?;
            if flags == 0 {
                break;
            }
            if flags & RECORD_TYPE == 0 {
                return Err(tag.malformed(format_args!(
                    "has a text record of type 0 with the style flags {flags:#04x}"
                )));
            }
            if flags & RECORD_HAS_FONT != 0 {
                font = Some(bytes.u16("font id").map_err(short)?);
            }
            if flags & RECORD_HAS_COLOR != 0 {
                bytes.skip(color, "text color").map_err(short)?;
            }
            let x = match flags & RECORD_HAS_X != 0 {
                true => Some(bytes.i16("X offset").map_err(short)?),
                false => None,
            };
            let y = match flags & RECORD_HAS_Y != 0 {
                true => Some(bytes.i16("Y offset").map_err(short)?),
                false => None,
            };
            if flags & RECORD_HAS_FONT != 0 {
                height = bytes.u16("text height").map_err(short)?;
            }
            let count = bytes.u8("glyph count").map_err(short)?;
            lines.move_to(x, y, height);
            if count == 0 {
                continue;
            }
            let font = font.ok_or_else(|| {
                tag.malformed("sets glyphs in a text record before any selects a font")
            })?;
            let font_codes = self.font_codes(font);
            codes.clear();
            for _ in 0..count {
                let glyph = bytes.ub(glyph_bits, "glyph entries").map_err(short)?;
                lines.advance(bytes.sb(advance_bits, "glyph entries").map_err(short)?);
                let Some(font_codes) = font_codes else {
                    continue;
                };
                let code = usize::try_from(glyph)
                    .ok()
                    .and_then(|glyph| font_codes.code(glyph))
                    .ok_or_else(|| {
                        tag.malformed(format_args!(
                            "sets the glyph {glyph} of the font {font}, which has {} glyphs",
                            font_codes.len()
                        ))
                    })?;
                codes.push(code);
            }
            bytes.align();
            if let Some(font_codes) = font_codes {
                font_codes.write(&codes, &mut lines.line);
            }
        }
        write_lines(self.xhtml, lines.finish().iter().map(String::as_str))?;
        Ok(())
    }

    /// The code table of the font `id`, if the movie defines the font with one; else none, and
    /// a warning that the text set in it is not written.
    fn font_codes(&mut self, id: u16) -> Option<Codes<'d>> {
        let codes = match self.fonts.0.get(&id) {
            Some(&Some(codes)) => return Some(codes),
            Some(None) => "has no code table",
            None => "is not defined in the movie",
        };
        self.warn(Warning::Font(id), || {
            format!("{NAME} does not write the text set in the font {id}, which {codes}")
        });
        None
    }

    /// `DefineEditText`: the initial text of a text field, unless it is a password.
    pub(super) fn define_edit_text(&mut self, tag: Tag<'d>) -> Result<(), Failure> {
        let short = |short| tag.short(short);
        let mut bytes = tag.bytes();
        bytes.skip(2, "character id").map_err(short)?;
        bytes.skip_rect("bounds").map_err(short)?;
        let flags = bytes.u8("flags").map_err(short)?;
        let more = bytes.u8("flags").map_err(short)?;
        let has_font = flags & EDIT_HAS_FONT != 0;
        let has_font_class = more & EDIT_HAS_FONT_CLASS != 0;
        if has_font {
            bytes.skip(2, "font id").map_err(short)?;
        }
        if has_font_class {
            bytes.string("font class").map_err(short)?;
        }
        if has_font || has_font_class {
            bytes.skip(2, "font height").map_err(short)?;
        }
        if flags & EDIT_HAS_TEXT_COLOR != 0 {
            bytes.skip(4, "text color").map_err(short)?;
        }
        if flags & EDIT_HAS_MAX_LENGTH != 0 {
            bytes.skip(2, "maximum length").map_err(short)?;
        }
        if more & EDIT_HAS_LAYOUT != 0 {
            bytes.skip(EDIT_LAYOUT_BYTES, "layout").map_err(short)?;
        }
        bytes.string("variable name").map_err(short)?;
        if flags & EDIT_HAS_TEXT == 0 {
            return Ok(());
        }
        let text = self.string(bytes.string("initial text").map_err(short)?);
        if flags & EDIT_PASSWORD != 0 {
            return Ok(());
        }
        match more & EDIT_HTML != 0 {
            true => html::write(&text, self.xhtml)?,
            false => write_lines(self.xhtml, plain_lines(&text))?,
        }
        Ok(())
    }
}
