//! Windows metafiles, as Tika's `EMFParser` and `WMFParser` read them: the text of the text
//! records as paragraphs, and from an EMF picture the documents its comments embed and the name
//! of an icon-only OLE object it shows. The records are read as the specifications lay them out,
//! where Tika reads them through POI's HEMF and HWMF, and the text is decoded as its records and
//! the selected font say (#127).

mod emf;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod wmf;

use std::collections::BTreeMap;

use crate::config::ParserConfigs;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared, Unported};
use crate::text::charset::Charset;

use emf::{Comment, TextOut};

/// `EMFParser.EMF_ICON_ONLY`.
const ICON_ONLY: &str = "emf:icon-only";
/// `EMFParser.EMF_ICON_STRING`.
const ICON_STRING: &str = "emf:icon-string";
/// The private comment that says the next one names the file an icon-only OLE object shows.
const ICON_ONLY_COMMENT: &str = "IconOnly";
/// `fudgeFactorX`: the gap between two runs of text on a line that a space stands for.
const SPACE_GAP: f64 = 10.0;
/// The difference in height that starts a new paragraph.
const LINE_GAP: f64 = 0.0001;
/// The size below which `getCurrentRectangle` takes a coordinate of the bounds for zero.
const SMIDGE: f64 = 0.000_000_001;

/// A rectangle of a record, as `Rectangle2D.Double` holds it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// The records of a picture that matter to Tika, and why reading them stopped short of the
/// picture's end, if it did.
#[derive(Debug)]
struct Picture<R> {
    records: Vec<R>,
    broken: Option<String>,
}

impl<R> Default for Picture<R> {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            broken: None,
        }
    }
}

/// The `length` bytes of `data` at `at`, those of `what`.
fn slice<'a>(data: &'a [u8], at: usize, length: usize, what: &str) -> Result<&'a [u8], String> {
    at.checked_add(length)
        .and_then(|end| data.get(at..end))
        .ok_or_else(|| {
            format!(
                "{what} of {length} bytes at {at:#x} runs past the {} bytes it is in",
                data.len()
            )
        })
}

/// The little-endian 16-bit value of `what` at `at` of `data`.
fn u16_at(data: &[u8], at: usize, what: &str) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        slice(data, at, 2, what)?.try_into().expect("two bytes"),
    ))
}

/// The little-endian 32-bit value of `what` at `at` of `data`.
fn u32_at(data: &[u8], at: usize, what: &str) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        slice(data, at, 4, what)?.try_into().expect("four bytes"),
    ))
}

/// What a metafile parser writes, in order.
#[derive(Debug)]
enum Step<'a> {
    /// A paragraph.
    Paragraph(String),
    /// A document a comment embeds, with the content type Tika gives it.
    Embedded {
        data: &'a [u8],
        content_type: Option<&'static str>,
    },
    /// The name of the file an icon-only OLE object shows, set as the metadata Tika sets.
    IconString(String),
    /// Why the picture could not be read on, after the steps before it.
    Fails(String),
}

/// Writes the `steps`, and fails where one says the picture could not be read on.
fn write(xhtml: &mut Xhtml, steps: Vec<Step<'_>>) -> Result<(), Failure> {
    for step in steps {
        match step {
            Step::Paragraph(text) => {
                xhtml.start_element("p")?;
                if !text.is_empty() {
                    xhtml.characters(&text)?;
                }
                xhtml.end_element("p")?;
            }
            Step::Embedded { data, content_type } => {
                let mut metadata = TikaMetadata::new();
                if let Some(content_type) = content_type {
                    metadata.set("Content-Type", content_type);
                }
                xhtml.parse_embedded(data, &metadata, Wrapping::PackageEntry)?;
            }
            Step::IconString(name) => {
                let metadata = xhtml.metadata_mut();
                metadata.set(ICON_ONLY, "true");
                metadata.set(ICON_STRING, name);
            }
            Step::Fails(reason) => return Err(Failure::Malformed(reason)),
        }
    }
    Ok(())
}

/// Tika's `EMFParser`.
pub struct EmfParser;

impl Parser for EmfParser {
    fn name(&self) -> &'static str {
        "extractous::EMFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.EMFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/emf"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'static>> {
        let steps = emf_steps(emf::records(input));
        Ok(Box::new(move |xhtml| write(xhtml, steps)))
    }
}

/// `ParseState`, the text gathered into the paragraph being written, and the charset of the
/// font selected and of each font of the object table.
#[derive(Debug)]
struct EmfState {
    last: Rect,
    hit_icon_only: bool,
    last_was_icon_only: bool,
    icon_string: Option<String>,
    buffer: String,
    fonts: BTreeMap<u32, u8>,
    charset: Option<u8>,
}

/// What Tika's `EMFParser` writes of a `picture`.
fn emf_steps(picture: Picture<emf::Record<'_>>) -> Vec<Step<'_>> {
    let mut steps = Vec::new();
    let mut state = EmfState {
        last: Rect {
            x: -1.0,
            y: -1.0,
            width: 0.0,
            height: 0.0,
        },
        hit_icon_only: false,
        last_was_icon_only: false,
        icon_string: None,
        buffer: String::new(),
        fonts: BTreeMap::new(),
        charset: None,
    };
    for record in picture.records {
        let mut is_icon_only = false;
        match record {
            emf::Record::Comment(Comment::Formats(formats)) => {
                steps.extend(formats.into_iter().map(|data| Step::Embedded {
                    data,
                    content_type: None,
                }));
            }
            emf::Record::Comment(Comment::Wmf(data)) => steps.push(Step::Embedded {
                data,
                content_type: Some("image/wmf"),
            }),
            emf::Record::Comment(Comment::Generic(data)) => {
                let value = unicode_string(data);
                if value.as_deref() == Some(ICON_ONLY_COMMENT) && !state.hit_icon_only {
                    state.hit_icon_only = true;
                    is_icon_only = true;
                } else if state.last_was_icon_only && state.icon_string.is_none() {
                    state.icon_string = value;
                }
            }
            emf::Record::Comment(Comment::Other) => {}
            emf::Record::Text(text) => ext_text_out(&text, &mut state, &mut steps),
            emf::Record::Font { index, charset } => {
                state.fonts.insert(index, charset);
            }
            emf::Record::Select(index) => {
                if let Some(&charset) = state.fonts.get(&index) {
                    state.charset = Some(charset);
                } else if index & STOCK_OBJECT != 0 {
                    state.charset = None;
                }
            }
            emf::Record::Delete(index) => {
                state.fonts.remove(&index);
            }
        }
        state.last_was_icon_only = is_icon_only;
    }
    if let Some(name) = state.icon_string {
        steps.push(Step::IconString(name));
    }
    if !state.buffer.is_empty() {
        steps.push(Step::Paragraph(state.buffer));
    }
    if let Some(reason) = picture.broken {
        steps.push(Step::Fails(reason));
    }
    steps
}

/// The bit of an EMF object index that names a stock object, a stock font among them, in the
/// charset of the system.
const STOCK_OBJECT: u32 = 0x8000_0000;

/// The charset of the text of a font of the charset `id`, the system's where it names none.
fn text_charset(id: Option<u8>) -> &'static str {
    id.and_then(font_charset).unwrap_or(DEFAULT_CHARSET)
}

/// The charset of text whose font names none: the ANSI code page of Western Windows systems.
const DEFAULT_CHARSET: &str = "Cp1252";

/// `handleExtTextOut`: the record's text added to the paragraph, after a space where it starts
/// well right of the last text, or in a new paragraph where it starts at another height. Its
/// ANSI text is in the selected font's charset, where Tika decodes it as UTF-8.
fn ext_text_out(text: &TextOut, state: &mut EmfState, steps: &mut Vec<Step>) {
    let current = current_rectangle(text);
    if state.last.y > -1.0 && (state.last.y - current.y).abs() > LINE_GAP {
        steps.push(Step::Paragraph(std::mem::take(&mut state.buffer)));
    } else if state.last.x > -1.0
        && (current.x - (state.last.x + state.last.width)).abs() > SPACE_GAP
    {
        state.buffer.push(' ');
    }
    let charset = if text.unicode {
        "UTF-16LE"
    } else {
        text_charset(state.charset)
    };
    state.buffer.push_str(&decode(text.raw, charset));
    state.last = current;
}

/// `getCurrentRectangle`: the record's bounds, or the bounds it says to ignore where its own
/// are all zero.
fn current_rectangle(text: &TextOut) -> Rect {
    let bounds = text.bounds;
    let set = [bounds.x, bounds.y, bounds.width, bounds.height]
        .iter()
        .any(|value| value.abs() > SMIDGE);
    if set {
        bounds
    } else {
        text.bounds_ignored
    }
}

/// `bytes` decoded in a charset the port decodes.
fn decode(bytes: &[u8], charset: &str) -> String {
    Charset::named(charset)
        .decode(bytes)
        .unwrap_or_else(|| panic!("the port decodes {charset}"))
}

/// `tryToReadAsString`: private data read as a UTF-16LE string up to its first zero unit, or
/// `None` for data of fewer than two bytes. Tika reads it as POI's
/// `StringUtil.getFromUnicodeLE0Terminated`, which leaves out the last unit and puts a `?` for
/// a leading zero unit (#127).
fn unicode_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 {
        return None;
    }
    let units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .take_while(|unit| **unit != [0, 0])
        .count();
    Some(decode(&bytes[..units * 2], "UTF-16LE"))
}

/// What Tika's `EMFParser` records in `emf:icon-string` of the EMF picture `input`, parsed as
/// `resolveEmfNames` parses the icon of an OLE object, which swallows the parser's failure: the
/// name of the file the icon shows, or none where it names none or the parser fails. A picture
/// whose comments embed documents, which Tika parses there, is left to Tika.
pub fn emf_icon_name(input: &[u8]) -> Result<Option<String>, Unported> {
    let mut name = None;
    for step in emf_steps(emf::records(input)) {
        match step {
            Step::IconString(found) => name = Some(found),
            Step::Fails(_) => return Ok(None),
            Step::Embedded { .. } => {
                return Err(Unported::new(
                    "the EMF picture embeds documents, which Tika parses when it reads the \
                     picture as the icon of an OLE object",
                ))
            }
            Step::Paragraph(_) => {}
        }
    }
    Ok(name)
}

/// Tika's `WMFParser`.
pub struct WmfParser;

impl Parser for WmfParser {
    fn name(&self) -> &'static str {
        "extractous::WMFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.WMFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/wmf"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'static>> {
        let steps = wmf_steps(wmf::records(input));
        Ok(Box::new(move |xhtml| write(xhtml, steps)))
    }
}

/// `FontCharset.getCharset()`'s name of the charset with a WMF font's charset id, if POI maps
/// it to one.
fn font_charset(id: u8) -> Option<&'static str> {
    Some(match id {
        0x00 | 0x01 | 0xFF => "Cp1252",
        0x4D => "MacRoman",
        0x80 => "Shift_JIS",
        0x81 => "cp949",
        0x82 => "x-Johab",
        0x86 => "GB2312",
        0x88 => "Big5",
        0xA1 => "Cp1253",
        0xA2 => "Cp1254",
        0xA3 => "Cp1258",
        0xB1 => "Cp1255",
        0xB2 => "Cp1256",
        0xBA => "Cp1257",
        0xCC => "Cp1251",
        0xDE => "x-windows-874",
        0xEE => "Cp1250",
        _ => return None,
    })
}

/// An object of a WMF picture's object table.
#[derive(Debug, Clone, Copy)]
enum WmfObject {
    /// A font in a `Font` charset.
    Font(u8),
    Other,
}

/// What Tika's `WMFParser` writes of a `picture`: a paragraph of each text record, its bytes
/// decoded in the charset of the font selected. Tika decodes them in that of the last font
/// made, and cuts the text of a `META_EXTTEXTOUT` to its length after decoding it (#127).
fn wmf_steps(picture: Picture<wmf::Record<'_>>) -> Vec<Step<'static>> {
    let mut objects: Vec<Option<WmfObject>> = Vec::new();
    let mut charset = None;
    let mut steps = Vec::new();
    for record in picture.records {
        let made = match record {
            wmf::Record::Font { charset } => WmfObject::Font(charset),
            wmf::Record::Object => WmfObject::Other,
            wmf::Record::Select(index) => {
                if let Some(Some(WmfObject::Font(selected))) = objects.get(usize::from(index)) {
                    charset = Some(*selected);
                }
                continue;
            }
            wmf::Record::Delete(index) => {
                if let Some(object) = objects.get_mut(usize::from(index)) {
                    *object = None;
                }
                continue;
            }
            wmf::Record::Text(raw) => {
                steps.push(Step::Paragraph(decode(raw, text_charset(charset))));
                continue;
            }
        };
        match objects.iter_mut().find(|object| object.is_none()) {
            Some(free) => *free = Some(made),
            None => objects.push(Some(made)),
        }
    }
    if let Some(reason) = picture.broken {
        steps.push(Step::Fails(reason));
    }
    steps
}

#[cfg(test)]
mod tests;
