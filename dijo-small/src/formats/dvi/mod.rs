//! TeX's device-independent files (`.dvi`), which Tika does not read, read for their text: the
//! characters each page sets, by its `set_char`, `set` and `put` commands, in the order they are
//! set. Each page is a `<div class="page">` of its lines, a paragraph each.
//!
//! A DVI file places characters but says nothing of their widths, which are in the fonts' metric
//! files: a character is taken to be half an em of its font wide, the font's size an em. What
//! moves between two characters says what is between them: a move along the line of more than
//! [`SPACE`] of an em, or back by more than an em, is a space between words, and a move across the
//! line of more than [`LINE`] of an em is a new line; less is kerning, or a sub- or superscript.
//!
//! A character is decoded in its font's encoding, which its name tells ([`encoding`]): the TeX
//! text (OT1), typewriter and math encodings of Computer Modern, and Cork (T1) of the European
//! Computer Modern and Latin Modern fonts. An accent TeX sets apart from its letter is written as
//! the combining mark after the letter. The characters of a font whose encoding is not known are
//! not written, and the font is named in a warning; a code an encoding does not assign is written
//! as U+FFFD.
//!
//! The comment of the preamble is the `dvi:comment`, and the date TeX writes in it, `TeX output
//! 2026.11.13:1200`, in the local time it ran in, the `dcterms:created`, an ISO local date-time
//! without an offset; the number of pages is the `xmpTPg:NPages`. Specials (`\special`) are not
//! read.

mod encoding;

use std::collections::HashMap;

use unicode_normalization::char::compose;

use encoding::{Accent, Encoding, Glyph, STROKE};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::EXCEPTION_WARNING;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};

const NAME: &str = "dijo::DviParser";
const WHAT: &str = "The DVI file";

const SET_CHAR_127: u8 = 127;
const SET1: u8 = 128;
const SET4: u8 = 131;
const SET_RULE: u8 = 132;
const PUT1: u8 = 133;
const PUT4: u8 = 136;
const PUT_RULE: u8 = 137;
const NOP: u8 = 138;
const BOP: u8 = 139;
const EOP: u8 = 140;
const PUSH: u8 = 141;
const POP: u8 = 142;
const RIGHT1: u8 = 143;
const RIGHT4: u8 = 146;
const W0: u8 = 147;
const W1: u8 = 148;
const W4: u8 = 151;
const X0: u8 = 152;
const X1: u8 = 153;
const X4: u8 = 156;
const DOWN1: u8 = 157;
const DOWN4: u8 = 160;
const Y0: u8 = 161;
const Y1: u8 = 162;
const Y4: u8 = 165;
const Z0: u8 = 166;
const Z1: u8 = 167;
const Z4: u8 = 170;
const FNT_NUM_0: u8 = 171;
const FNT_NUM_63: u8 = 234;
const FNT1: u8 = 235;
const FNT4: u8 = 238;
const XXX1: u8 = 239;
const XXX4: u8 = 242;
const FNT_DEF1: u8 = 243;
const FNT_DEF4: u8 = 246;
const PRE: u8 = 247;
const POST: u8 = 248;
/// The version of DVI TeX writes.
const ID: u8 = 2;
/// The bytes of a `bop`'s counters and pointer.
const BOP_PARAMETERS: usize = 44;

/// A move along the line wider than this part of an em is a space: TeX's interword space is a
/// third of an em, which it shrinks to two ninths, and its kerns are a tenth of an em or less.
const SPACE: (i64, i64) = (3, 20);
/// A move across the line taller than this part of an em is a new line: TeX's lines are an em
/// and a fifth apart, its superscripts and subscripts moved by under half an em.
const LINE: (i64, i64) = (4, 5);
/// An accent set back over its letter by more than this part of an em is set after the letter.
const OVERLAP: (i64, i64) = (1, 4);
/// The width taken for every character, as a part of an em.
const WIDTH: (i64, i64) = (1, 2);
/// The deepest the stack of positions grows, as the postamble's two bytes count it.
const MAX_STACK: usize = u16::MAX as usize;
/// The most fonts a file defines: TeX's own limit is 9000.
const MAX_FONTS: usize = 10_000;

/// The part `(numerator, denominator)` of `em`.
fn part((numerator, denominator): (i64, i64), em: i64) -> i64 {
    em * numerator / denominator
}

/// The text of TeX DVI files.
pub struct DviParser;

impl Parser for DviParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-dvi"]
    }

    /// Bounded: the pages are read in turn, and only the fonts and the stack of the one being
    /// read are held, both bounded.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    /// Fails a file whose preamble is not that of a DVI file TeX writes before anything is
    /// written.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let mut cursor = Cursor { input, at: 0 };
        let comment = preamble(&mut cursor)
            .map_err(|Malformed(reason)| Decline::Fails(Error::ParseError(reason)))?;
        Ok(Box::new(move |xhtml| {
            write_metadata(comment, xhtml.metadata_mut());
            Reading::new(cursor, pages, xhtml).read()
        }))
    }
}

/// Where and how a file is not a DVI file.
#[derive(Debug)]
struct Malformed(String);

impl From<Malformed> for Failure {
    fn from(Malformed(reason): Malformed) -> Self {
        Failure::Malformed(reason)
    }
}

/// The bytes of a DVI file, read in turn.
struct Cursor<'a> {
    input: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Malformed> {
        let end = self
            .at
            .checked_add(count)
            .filter(|&end| end <= self.input.len())
            .ok_or_else(|| {
                Malformed(format!(
                    "{WHAT} ends early: {count} bytes are read at byte {} of its {}",
                    self.at,
                    self.input.len()
                ))
            })?;
        let taken = &self.input[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn byte(&mut self) -> Result<u8, Malformed> {
        Ok(self.take(1)?[0])
    }

    /// The big-endian unsigned number of `count` bytes, one to four.
    fn unsigned(&mut self, count: usize) -> Result<i64, Malformed> {
        Ok(self
            .take(count)?
            .iter()
            .fold(0, |value, &byte| (value << 8) | i64::from(byte)))
    }

    /// The big-endian two's complement number of `count` bytes, one to four.
    fn signed(&mut self, count: usize) -> Result<i64, Malformed> {
        let bits = 64 - 8 * (count as u32);
        Ok(self.unsigned(count)? << bits >> bits)
    }

    /// The parameter of `opcode`, the `n`th of the four commands from `first`, of `n` bytes:
    /// signed for the fourth and unsigned for the others, as `set`, `put`, `fnt` and `fnt_def`
    /// take theirs.
    fn parameter(&mut self, opcode: u8, first: u8) -> Result<i64, Malformed> {
        let count = usize::from(opcode - first) + 1;
        match count {
            4 => self.signed(4),
            _ => self.unsigned(count),
        }
    }

    /// The signed number of `opcode`, the `n`th of the four commands from `first`, of `n` bytes.
    fn movement(&mut self, opcode: u8, first: u8) -> Result<i64, Malformed> {
        self.signed(usize::from(opcode - first) + 1)
    }
}

/// The comment of the preamble at the start of `cursor`, which is read past it.
fn preamble(cursor: &mut Cursor<'_>) -> Result<String, Malformed> {
    let (opcode, id) = (cursor.byte()?, cursor.byte()?);
    if opcode != PRE {
        return Err(Malformed(format!(
            "{WHAT} does not begin with a preamble but the byte {opcode:#04x}"
        )));
    }
    if id != ID {
        return Err(Malformed(format!(
            "{WHAT} is of the version {id}, not of TeX's version {ID}"
        )));
    }
    let numerator = cursor.unsigned(4)?;
    let denominator = cursor.unsigned(4)?;
    let magnification = cursor.unsigned(4)?;
    if numerator == 0 || denominator == 0 || magnification == 0 {
        return Err(Malformed(format!(
            "{WHAT} has the units {numerator}/{denominator} magnified {magnification}"
        )));
    }
    let length = usize::from(cursor.byte()?);
    Ok(String::from_utf8_lossy(cursor.take(length)?).into_owned())
}

/// Sets the metadata of the preamble's `comment`.
fn write_metadata(comment: String, metadata: &mut TikaMetadata) {
    if let Some(created) = tex_date(&comment) {
        metadata.set("dcterms:created", created);
    }
    let comment = comment.trim();
    if !comment.is_empty() {
        metadata.set("dvi:comment", comment);
    }
}

/// The ISO local date-time of the date TeX writes in its comment, ` TeX output 2026.11.13:1200`,
/// if `comment` is one.
fn tex_date(comment: &str) -> Option<String> {
    let date = comment.trim().strip_prefix("TeX output ")?;
    let (day, time) = date.split_once(':')?;
    let mut fields = day.split('.');
    let number = |text: &str,
                  digits: std::ops::RangeInclusive<usize>,
                  range: std::ops::RangeInclusive<u32>| {
        (digits.contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u32>().ok())
            .flatten()
            .filter(|value| range.contains(value))
    };
    let year = number(fields.next()?, 1..=4, 0..=9999)?;
    let month = number(fields.next()?, 2..=2, 1..=12)?;
    let day = number(fields.next()?, 2..=2, 1..=31)?;
    if fields.next().is_some() || time.len() != 4 {
        return None;
    }
    let hour = number(time.get(..2)?, 2..=2, 0..=23)?;
    let minute = number(time.get(2..)?, 2..=2, 0..=59)?;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:00"
    ))
}

/// A font a file defines.
#[derive(Debug)]
struct Font {
    name: String,
    /// Its size, an em, in the file's units.
    size: i64,
    encoding: Option<Encoding>,
    /// Whether a warning named it, as one whose encoding is not known.
    reported: bool,
}

/// Where a page's commands place the next character, and the spaces its `w`, `x`, `y` and `z`
/// commands move by.
#[derive(Debug, Clone, Copy, Default)]
struct Position {
    h: i64,
    v: i64,
    w: i64,
    x: i64,
    y: i64,
    z: i64,
}

/// Where a character was set: where it is taken to end along its line, its line, and its em.
#[derive(Debug, Clone, Copy)]
struct Placed {
    end: i64,
    v: i64,
    em: i64,
}

/// How a character stands to the last set on its page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Apart {
    /// It is the first on its page.
    First,
    /// It is on a line of its own.
    Line,
    /// It is a word of its own.
    Word,
    /// It is in the same word, this far along the line from the last character's end.
    Close(i64),
}

/// The text of a page's line being written.
#[derive(Debug, Default)]
struct Line {
    text: String,
    /// An accent set before the letter it is placed over.
    accent: Option<Accent>,
}

impl Line {
    /// Writes an accent waiting for its letter alone.
    fn settle(&mut self) {
        if let Some(accent) = self.accent.take() {
            self.text.push_str(accent.alone);
        }
    }

    fn space(&mut self) {
        self.settle();
        if !self.text.is_empty() && !self.text.ends_with(' ') {
            self.text.push(' ');
        }
    }

    /// Writes the line as a paragraph, if it has any text, and starts the next.
    fn end(&mut self, xhtml: &mut Xhtml) -> Result<(), Stop> {
        self.settle();
        xhtml.element("p", &self.text)?;
        self.text.clear();
        Ok(())
    }

    /// Writes `text`, the accent waiting for a letter on its first character.
    fn text(&mut self, text: &str) {
        let mut chars = text.chars();
        let Some(first) = chars.next() else {
            return;
        };
        match self.accent.take() {
            None => self.text.push(first),
            Some(accent) => self.accented(first, accent.mark),
        }
        self.text.push_str(chars.as_str());
    }

    /// Puts `accent`, set back over the letter before it, on that letter, or keeps it for the next
    /// letter when the line has none yet.
    fn over_last(&mut self, accent: Accent) {
        match self.text.pop() {
            Some(letter) => self.accented(letter, accent.mark),
            None => self.accent = Some(accent),
        }
    }

    /// Writes `letter` with the combining `mark` on it, composed where Unicode has the character:
    /// `ł` and `Ł` for the stroke, and the dotless `ı` and `ȷ` TeX accents as `i` and `j`.
    fn accented(&mut self, letter: char, mark: char) {
        match (letter, mark) {
            ('l', STROKE) => self.text.push('\u{142}'),
            ('L', STROKE) => self.text.push('\u{141}'),
            _ => {
                let letter = match letter {
                    '\u{131}' => 'i',
                    '\u{237}' => 'j',
                    other => other,
                };
                match compose(letter, mark) {
                    Some(composed) => self.text.push(composed),
                    None => {
                        self.text.push(letter);
                        self.text.push(mark);
                    }
                }
            }
        }
    }
}

/// A DVI file being read and written, page by page.
struct Reading<'a, 'x, 'h> {
    cursor: Cursor<'a>,
    paging: Paging<'a>,
    xhtml: &'x mut Xhtml<'h>,
    fonts: HashMap<i64, Font>,
    /// The font characters are set in, by its number.
    font: Option<i64>,
    position: Position,
    stack: Vec<Position>,
    /// The last character set on the page.
    last: Option<Placed>,
    line: Line,
}

impl<'a, 'x, 'h> Reading<'a, 'x, 'h> {
    fn new(cursor: Cursor<'a>, pages: Pages<'a>, xhtml: &'x mut Xhtml<'h>) -> Self {
        Self {
            cursor,
            paging: Paging::new(pages),
            xhtml,
            fonts: HashMap::new(),
            font: None,
            position: Position::default(),
            stack: Vec::new(),
            last: None,
            line: Line::default(),
        }
    }

    /// Reads the pages and the font definitions between them, up to the postamble.
    fn read(mut self) -> Result<(), Failure> {
        let mut pages = 0u64;
        loop {
            let at = self.cursor.at;
            match self.cursor.byte()? {
                NOP => {}
                BOP => {
                    self.cursor.take(BOP_PARAMETERS)?;
                    pages += 1;
                    self.page()?;
                    self.paging.read(self.cursor.at - at);
                }
                opcode @ FNT_DEF1..=FNT_DEF4 => self.define_font(opcode)?,
                POST => break,
                other => {
                    return Err(Malformed(format!(
                        "{WHAT} has the command {other} outside a page at byte {at}"
                    ))
                    .into())
                }
            }
        }
        self.xhtml
            .metadata_mut()
            .set("xmpTPg:NPages", pages.to_string());
        Ok(())
    }

    /// Reads and writes a page, after its `bop`, up to its `eop`.
    fn page(&mut self) -> Result<(), Failure> {
        self.xhtml.start_element_with("div", &[("class", "page")])?;
        self.position = Position::default();
        self.font = None;
        self.last = None;
        loop {
            let at = self.cursor.at;
            let opcode = self.cursor.byte()?;
            match opcode {
                0..=SET_CHAR_127 => self.set(i64::from(opcode), true)?,
                SET1..=SET4 => {
                    let code = self.cursor.parameter(opcode, SET1)?;
                    self.set(code, true)?;
                }
                SET_RULE => {
                    self.cursor.take(4)?;
                    let width = self.cursor.signed(4)?;
                    self.right(width);
                }
                PUT1..=PUT4 => {
                    let code = self.cursor.parameter(opcode, PUT1)?;
                    self.set(code, false)?;
                }
                PUT_RULE => {
                    self.cursor.take(8)?;
                }
                NOP => {}
                EOP => break,
                PUSH => {
                    if self.stack.len() == MAX_STACK {
                        return Err(Malformed(format!(
                            "{WHAT} pushes more than {MAX_STACK} positions at byte {at}"
                        ))
                        .into());
                    }
                    self.stack.push(self.position);
                }
                POP => {
                    self.position = self.stack.pop().ok_or_else(|| {
                        Malformed(format!(
                            "{WHAT} pops a position it did not push at byte {at}"
                        ))
                    })?;
                }
                RIGHT1..=RIGHT4 => {
                    let by = self.cursor.movement(opcode, RIGHT1)?;
                    self.right(by);
                }
                W0 => self.right(self.position.w),
                W1..=W4 => {
                    self.position.w = self.cursor.movement(opcode, W1)?;
                    self.right(self.position.w);
                }
                X0 => self.right(self.position.x),
                X1..=X4 => {
                    self.position.x = self.cursor.movement(opcode, X1)?;
                    self.right(self.position.x);
                }
                DOWN1..=DOWN4 => {
                    let by = self.cursor.movement(opcode, DOWN1)?;
                    self.down(by);
                }
                Y0 => self.down(self.position.y),
                Y1..=Y4 => {
                    self.position.y = self.cursor.movement(opcode, Y1)?;
                    self.down(self.position.y);
                }
                Z0 => self.down(self.position.z),
                Z1..=Z4 => {
                    self.position.z = self.cursor.movement(opcode, Z1)?;
                    self.down(self.position.z);
                }
                FNT_NUM_0..=FNT_NUM_63 => self.select_font(i64::from(opcode - FNT_NUM_0), at)?,
                FNT1..=FNT4 => {
                    let number = self.cursor.parameter(opcode, FNT1)?;
                    self.select_font(number, at)?;
                }
                XXX1..=XXX4 => {
                    let length = self.cursor.unsigned(usize::from(opcode - XXX1) + 1)?;
                    let length = usize::try_from(length).map_err(|_| {
                        Malformed(format!(
                            "{WHAT} has a special of {length} bytes at byte {at}"
                        ))
                    })?;
                    self.cursor.take(length)?;
                }
                FNT_DEF1..=FNT_DEF4 => self.define_font(opcode)?,
                other => {
                    return Err(Malformed(format!(
                        "{WHAT} has the command {other} inside a page at byte {at}"
                    ))
                    .into())
                }
            }
        }
        if !self.stack.is_empty() {
            return Err(Malformed(format!(
                "{WHAT} ends a page with {} positions pushed and not popped at byte {}",
                self.stack.len(),
                self.cursor.at - 1
            ))
            .into());
        }
        self.line.end(self.xhtml)?;
        self.xhtml.end_element("div")?;
        Ok(())
    }

    fn right(&mut self, by: i64) {
        self.position.h = self.position.h.wrapping_add(by);
    }

    fn down(&mut self, by: i64) {
        self.position.v = self.position.v.wrapping_add(by);
    }

    /// Reads the definition of a font after its command `opcode`.
    fn define_font(&mut self, opcode: u8) -> Result<(), Malformed> {
        let at = self.cursor.at - 1;
        let number = self.cursor.parameter(opcode, FNT_DEF1)?;
        self.cursor.take(4)?;
        let size = self.cursor.signed(4)?;
        self.cursor.take(4)?;
        let area = usize::from(self.cursor.byte()?);
        let length = usize::from(self.cursor.byte()?);
        let name = self.cursor.take(area + length)?;
        let name = String::from_utf8_lossy(&name[area..]).into_owned();
        if size <= 0 {
            return Err(Malformed(format!(
                "{WHAT} defines the font {name} of the size {size} at byte {at}"
            )));
        }
        if self.fonts.len() == MAX_FONTS && !self.fonts.contains_key(&number) {
            return Err(Malformed(format!(
                "{WHAT} defines more than {MAX_FONTS} fonts at byte {at}"
            )));
        }
        let encoding = Encoding::of_font(&name);
        self.fonts.insert(
            number,
            Font {
                name,
                size,
                encoding,
                reported: false,
            },
        );
        Ok(())
    }

    fn select_font(&mut self, number: i64, at: usize) -> Result<(), Malformed> {
        if !self.fonts.contains_key(&number) {
            return Err(Malformed(format!(
                "{WHAT} selects the font {number}, which it has not defined, at byte {at}"
            )));
        }
        self.font = Some(number);
        Ok(())
    }

    /// Sets the character `code` of the current font at the current position, and moves past it
    /// if it `advances`.
    fn set(&mut self, code: i64, advances: bool) -> Result<(), Failure> {
        let at = self.cursor.at;
        let font = self
            .font
            .and_then(|number| self.fonts.get_mut(&number))
            .ok_or_else(|| {
                Malformed(format!(
                    "{WHAT} sets a character before it selects a font, before byte {at}"
                ))
            })?;
        let em = font.size;
        let glyph = match font.encoding {
            Some(encoding) => Some(encoding.glyph(code)),
            None => {
                if !font.reported {
                    font.reported = true;
                    let warning = format!(
                        "{NAME} does not know the encoding of the font {}: the characters set \
                         in it are not written",
                        font.name
                    );
                    self.xhtml.metadata_mut().add(EXCEPTION_WARNING, warning);
                }
                None
            }
        };
        let Position { h, v, .. } = self.position;
        let apart = self.separate(h, v, em)?;
        match glyph {
            None => {}
            Some(None) => self.line.text("\u{fffd}"),
            Some(Some(Glyph::Text(text))) => self.line.text(text),
            Some(Some(Glyph::Accent(accent))) => {
                self.line.settle();
                match apart {
                    Apart::Close(gap) if gap < -part(OVERLAP, em) => self.line.over_last(accent),
                    Apart::First | Apart::Line | Apart::Word | Apart::Close(_) => {
                        self.line.accent = Some(accent)
                    }
                }
            }
        }
        let width = part(WIDTH, em);
        self.last = Some(Placed {
            end: h.wrapping_add(width),
            v,
            em,
        });
        if advances {
            self.position.h = h.wrapping_add(width);
        }
        Ok(())
    }

    /// Ends the line or writes a space before a character of `em` at `h` and `v`, as it moved
    /// from the last.
    fn separate(&mut self, h: i64, v: i64, em: i64) -> Result<Apart, Stop> {
        let Some(last) = self.last else {
            return Ok(Apart::First);
        };
        let em = em.max(last.em);
        if v.wrapping_sub(last.v).unsigned_abs() > part(LINE, em).unsigned_abs() {
            self.line.end(self.xhtml)?;
            return Ok(Apart::Line);
        }
        let gap = h.wrapping_sub(last.end);
        if gap > part(SPACE, em) || gap < -em {
            self.line.space();
            return Ok(Apart::Word);
        }
        Ok(Apart::Close(gap))
    }
}

#[cfg(test)]
mod tests;
