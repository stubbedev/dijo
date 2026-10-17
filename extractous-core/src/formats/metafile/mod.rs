//! Windows metafiles, as Tika's `EMFParser` and `WMFParser` read them through POI's HEMF and
//! HWMF: every record read first, failing as POI fails; then the text of the text records as
//! paragraphs, and from an EMF picture the documents its comments embed and the name of an
//! icon-only OLE object it shows.

mod dib;
mod emf;
mod emf_plus;
mod stream;
#[cfg(test)]
pub(crate) mod testing;
mod wmf;

use std::borrow::Cow;

use crate::config::ParserConfigs;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::poi::Thrown;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::text::charset::Charset;
use crate::unicode::is_java_identifier_part;

use emf::{Comment, Rect, TextOut};

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

/// What a metafile parser writes, in order, once POI has read every record.
#[derive(Debug)]
enum Step<'a> {
    /// A paragraph of Java characters.
    Paragraph(Vec<u16>),
    /// A document a comment embeds, with the content type Tika gives it.
    Embedded {
        data: Cow<'a, [u8]>,
        content_type: Option<&'static str>,
    },
    /// The name of the file an icon-only OLE object shows, set as the metadata Tika sets.
    IconString(String),
    /// What Tika's parser throws there, after the steps before it.
    Fails(String),
}

/// Writes the `steps`, or fails where POI failed to read the picture.
fn write(xhtml: &mut Xhtml, steps: Result<Vec<Step<'_>>, Thrown>) -> Result<(), Failure> {
    let steps = steps.map_err(|Thrown(reason)| Failure::Malformed(reason))?;
    for step in steps {
        match step {
            Step::Paragraph(text) => {
                xhtml.start_element("p")?;
                if !text.is_empty() {
                    xhtml.characters_utf16(&text)?;
                }
                xhtml.end_element("p")?;
            }
            Step::Embedded { data, content_type } => {
                let mut metadata = TikaMetadata::new();
                if let Some(content_type) = content_type {
                    metadata.set("Content-Type", content_type);
                }
                xhtml.parse_embedded(&data, &metadata, Wrapping::PackageEntry)?;
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
pub(crate) struct EmfParser;

impl Parser for EmfParser {
    fn name(&self) -> &'static str {
        "extractous::EMFParser"
    }

    #[cfg(test)]
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
    ) -> Result<Prepared<'a>, Declined> {
        let steps = emf::records(input).map(emf_steps);
        Ok(Box::new(move |xhtml| write(xhtml, steps)))
    }
}

/// `ParseState`, and the text gathered into the paragraph being written.
#[derive(Debug)]
struct EmfState {
    last: Rect,
    hit_icon_only: bool,
    last_was_icon_only: bool,
    icon_string: Option<String>,
    buffer: Vec<u16>,
}

/// What Tika's `EMFParser` writes of the `records` of a picture.
fn emf_steps<'a>(records: Vec<emf::Record<'a>>) -> Vec<Step<'a>> {
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
        buffer: Vec::new(),
    };
    for record in records {
        let mut is_icon_only = false;
        match record {
            emf::Record::Comment(Comment::Formats(formats)) => {
                for data in formats {
                    let Some(data) = data else {
                        steps.push(Step::Fails(
                            "NullPointerException: a format of the comment POI read no data of"
                                .to_owned(),
                        ));
                        return steps;
                    };
                    steps.push(Step::Embedded {
                        data,
                        content_type: None,
                    });
                }
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
            emf::Record::Comment(Comment::Other) | emf::Record::Other => {}
            emf::Record::Text(text) => {
                if let Err(reason) = ext_text_out(&text, &mut state, &mut steps) {
                    steps.push(Step::Fails(reason));
                    return steps;
                }
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
    steps
}

/// `handleExtTextOut`: the record's text added to the paragraph, after a space where it starts
/// well right of the last text, or in a new paragraph where it starts at another height.
fn ext_text_out(text: &TextOut, state: &mut EmfState, steps: &mut Vec<Step>) -> Result<(), String> {
    let current = current_rectangle(text);
    if state.last.y > -1.0 && (state.last.y - current.y).abs() > LINE_GAP {
        steps.push(Step::Paragraph(std::mem::take(&mut state.buffer)));
    } else if state.last.x > -1.0
        && (current.x - (state.last.x + state.last.width)).abs() > SPACE_GAP
    {
        state.buffer.push(u16::from(b' '));
    }
    let charset = if text.unicode { "UTF-16LE" } else { "UTF-8" };
    let units = match text.raw {
        Some(raw) => java_prefix(&decode(raw, charset), text.string_length)?,
        None => Vec::new(),
    };
    state.buffer.extend(units);
    state.last = current;
    Ok(())
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

/// `new String(bytes, charset)` of a charset the JDK and this port both decode.
fn decode(bytes: &[u8], charset: &str) -> String {
    Charset::named(charset)
        .decode(bytes)
        .unwrap_or_else(|| panic!("the port decodes {charset}"))
}

/// `text.substring(0, Math.min(text.length(), length))`, in UTF-16 units, which fails for a
/// negative `length`.
fn java_prefix(text: &str, length: i32) -> Result<Vec<u16>, String> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = i64::from(length).min(units.len() as i64);
    if end < 0 {
        return Err(format!(
            "StringIndexOutOfBoundsException: begin 0, end {end}, length {}",
            units.len()
        ));
    }
    Ok(units[..end as usize].to_vec())
}

/// `tryToReadAsString`: private data read as a UTF-16LE string ended by a zero unit, as
/// `StringUtil.getFromUnicodeLE0Terminated` reads it, or `None` for data of fewer than two
/// bytes or where that throws.
fn unicode_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 {
        return None;
    }
    let length = (bytes.len() - 2) / 2;
    let unit = |index: usize| u16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]);
    let (start, limit, prefix) = if length > 0 && unit(0) == 0 {
        let next = if length > 1 { unit(1) as i16 } else { 0 };
        let limit = if is_java_identifier_part(i32::from(next)) {
            length - 1
        } else {
            0
        };
        (1, limit, "?")
    } else {
        (0, length, "")
    };
    let units = (0..limit)
        .map(|index| unit(start + index))
        .take_while(|&unit| unit != 0)
        .count();
    let text = decode(&bytes[start * 2..(start + units) * 2], "UTF-16LE");
    Some(format!("{prefix}{text}"))
}

/// Tika's `WMFParser`.
pub(crate) struct WmfParser;

impl Parser for WmfParser {
    fn name(&self) -> &'static str {
        "extractous::WMFParser"
    }

    #[cfg(test)]
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
    ) -> Result<Prepared<'a>, Declined> {
        let steps = wmf::records(input).map(wmf_steps);
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

/// What Tika's `WMFParser` writes of the `records` of a picture: a paragraph of each text
/// record, decoded in the charset of the last font created before it.
fn wmf_steps(records: Vec<wmf::Record<'_>>) -> Vec<Step<'static>> {
    let mut charset = "Cp1252";
    let mut steps = Vec::new();
    for record in records {
        match record {
            wmf::Record::Font { charset: id } => charset = font_charset(id).unwrap_or("Cp1252"),
            wmf::Record::Text(text) => match wmf_text(&text, charset) {
                Ok(units) => steps.push(Step::Paragraph(units)),
                Err(reason) => {
                    steps.push(Step::Fails(reason));
                    return steps;
                }
            },
            wmf::Record::Other => {}
        }
    }
    steps
}

/// `getText(charset)` of a WMF text record: its text, or the failure of a negative length.
fn wmf_text(text: &wmf::Text, charset: &str) -> Result<Vec<u16>, String> {
    let length = i32::from(text.string_length);
    if text.extended {
        return java_prefix(&decode(text.raw, charset), length);
    }
    if length < 0 {
        return Err(format!(
            "RecordFormatException: Invalid offset/length specified: offset: 0, lenght: {length}, \
             maxLength: 1000000"
        ));
    }
    let bytes = &text.raw[..text.raw.len().min(length as usize)];
    Ok(decode(bytes, charset).encode_utf16().collect())
}

#[cfg(test)]
mod tests;
