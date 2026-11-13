//! AutoCAD DXF drawings, ASCII or binary, read for their text, which Tika does not read: each
//! `TEXT`, `MTEXT`, `ATTRIB` and `ATTDEF` (its value, not its tag or prompt) and the text of each
//! dimension, in the order they are drawn in, those of the block definitions (other than the
//! anonymous blocks `*D…` a dimension is drawn with, which repeat its text) first. A text is a
//! paragraph, the paragraphs of an `MTEXT` one each; how the text of each is decoded is the
//! `text` module's. With an `MTEXT` read from its group 3 chunks and last group 1 as they come,
//! and every string a piece at a time, a drawing is read in memory bounded whatever its size.
//!
//! A dimension's text is its own (group 1), the measurement (group 42) in place of `<>`, or the
//! measurement alone where it has none, given to four decimals at most, an angle in degrees. A
//! single space suppresses it.
//!
//! The header's `$ACADVER` is the `dxf:version`, and its `$TDUCREATE` and `$TDUUPDATE`, Julian
//! days in UTC, the `dcterms:created` and `dcterms:modified`. Strings are in the code page
//! `$DWGCODEPAGE` names, or in UTF-8 from AutoCAD 2007 (`AC1021`) on; a drawing without a
//! header, as AutoCAD Release 12 could write one, is read in `ANSI_1252`, AutoCAD's default. The
//! drawing's properties, kept in the `XRECORD` the `DWGPROPS` command writes, are metadata as the
//! same properties of a DWG drawing are.

mod groups;
mod text;

use crate::config::ParserConfigs;
use crate::formats::dwg::{CUSTOM_PREFIX, HEADER_2000_PROPERTIES, MARKER_2000};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::Pages;
use crate::java::time::instant_string;
use crate::text::charset::Charset;
use groups::{Group, Groups};
use text::{Decoder, Paragraphs, Syntax};

const NAME: &str = "dijo::DxfParser";
/// The charset of a drawing whose header names none.
const DEFAULT_CODE_PAGE: &str = "windows-1252";
/// The first version whose strings are in UTF-8: AutoCAD 2007.
const FIRST_UNICODE_VERSION: u16 = 1021;
const UNIX_EPOCH_JULIAN_DAY: f64 = 2_440_587.5;
const SECONDS_PER_DAY: f64 = 86_400.0;
/// The Julian days of 0001-01-01 and 10000-01-01, the range of the dates read.
const JULIAN_DAYS: std::ops::Range<f64> = 1_721_425.5..5_373_484.5;
/// The group that ends the text of an entity whose embedded object follows.
const EMBEDDED_OBJECT: u16 = 101;
/// The dimension types (group 70, the lower four bits) that measure an angle, in radians.
const ANGULAR_DIMENSIONS: [i64; 2] = [2, 5];

/// The text of AutoCAD DXF drawings.
pub struct DxfParser;

impl Parser for DxfParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/vnd.dxf"]
    }

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

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        Ok(Box::new(move |xhtml| read_drawing(input, pages, xhtml)))
    }
}

/// Writes the drawing `input` as its groups are read, its `pages` released as they are.
fn read_drawing(input: &[u8], pages: Pages<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut groups = Groups::new(input, pages);
    let mut reading = Reading {
        section: Section::Outside,
        variable: Variable::Other,
        charset: Charset::named(DEFAULT_CODE_PAGE),
        unicode: false,
        entity: Entity::Other,
        dimension_block: false,
        out: String::new(),
        paragraphs: Paragraphs::default(),
    };
    while let Some(group) = groups.next_group()? {
        if group.code == 0 {
            reading.end_entity(xhtml)?;
            if reading.start(text_of(group)?) == Flow::End {
                return Ok(());
            }
            continue;
        }
        reading.read(group, xhtml)?;
    }
    reading.end_entity(xhtml)
}

/// Whether the drawing reads on after a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    On,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Outside,
    /// After a `SECTION`, before its name.
    Opening,
    Header,
    Blocks,
    Entities,
    Objects,
    Other,
}

impl Section {
    fn named(name: &[u8]) -> Section {
        match name {
            b"HEADER" => Section::Header,
            b"BLOCKS" => Section::Blocks,
            b"ENTITIES" => Section::Entities,
            b"OBJECTS" => Section::Objects,
            _ => Section::Other,
        }
    }
}

/// The header variable whose value groups follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variable {
    Version,
    CodePage,
    Created,
    Modified,
    Other,
}

impl Variable {
    fn named(name: &[u8]) -> Variable {
        match name {
            b"$ACADVER" => Variable::Version,
            b"$DWGCODEPAGE" => Variable::CodePage,
            b"$TDUCREATE" => Variable::Created,
            b"$TDUUPDATE" => Variable::Modified,
            _ => Variable::Other,
        }
    }
}

/// The entity or object whose groups are read, with what is read of it so far.
enum Entity<'a> {
    Other,
    Block,
    /// A `TEXT`, `ATTRIB` or `ATTDEF`, until its text is `read`.
    Text {
        read: bool,
    },
    /// An `MTEXT`, its text decoded as its chunks come until it is `closed`.
    Mtext {
        decoder: Decoder,
        closed: bool,
    },
    Dimension {
        text: Option<&'a [u8]>,
        measurement: Option<f64>,
        angular: bool,
        closed: bool,
    },
    /// An `XRECORD`, which holds the drawing's properties where its first string is their
    /// cookie: whether it is, once that is read.
    Record {
        cookie: Option<bool>,
    },
}

impl Entity<'_> {
    /// The entity of the type `name` in a block or the entities.
    fn drawn(name: &[u8]) -> Self {
        match name {
            b"TEXT" | b"ATTRIB" | b"ATTDEF" => Entity::Text { read: false },
            b"MTEXT" => Entity::Mtext {
                decoder: Decoder::new(Syntax::Mtext),
                closed: false,
            },
            b"DIMENSION" | b"ARC_DIMENSION" | b"LARGE_RADIAL_DIMENSION" => Entity::Dimension {
                text: None,
                measurement: None,
                angular: false,
                closed: false,
            },
            _ => Entity::Other,
        }
    }
}

struct Reading<'a> {
    section: Section,
    variable: Variable,
    charset: Charset,
    /// Whether the strings are in UTF-8, whatever code page the header names.
    unicode: bool,
    entity: Entity<'a>,
    /// Whether the block read is one a dimension is drawn with.
    dimension_block: bool,
    /// The text decoded and not yet written.
    out: String,
    paragraphs: Paragraphs,
}

impl<'a> Reading<'a> {
    /// Starts what the group 0 `name` starts.
    fn start(&mut self, name: &[u8]) -> Flow {
        match name {
            b"SECTION" => self.section = Section::Opening,
            b"ENDSEC" => self.section = Section::Outside,
            b"EOF" => return Flow::End,
            _ => match self.section {
                Section::Blocks if name == b"BLOCK" => self.entity = Entity::Block,
                Section::Blocks if name == b"ENDBLK" => self.dimension_block = false,
                Section::Blocks if self.dimension_block => {}
                Section::Blocks | Section::Entities => self.entity = Entity::drawn(name),
                Section::Objects if name == b"XRECORD" => {
                    self.entity = Entity::Record { cookie: None }
                }
                _ => {}
            },
        }
        Flow::On
    }

    fn read(&mut self, group: Group<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
        match self.section {
            Section::Opening if group.code == 2 => self.section = Section::named(text_of(group)?),
            Section::Header => self.header(group, xhtml)?,
            Section::Blocks | Section::Entities | Section::Objects => self.entity(group, xhtml)?,
            _ => {}
        }
        Ok(())
    }

    fn header(&mut self, group: Group<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
        match (self.variable, group.code) {
            (_, 9) => self.variable = Variable::named(text_of(group)?),
            (Variable::Version, 1) => {
                let version =
                    std::str::from_utf8(text_of(group)?).map_err(|_| malformed_version(group))?;
                let number = version
                    .strip_prefix("AC")
                    .and_then(|number| number.parse::<u16>().ok())
                    .ok_or_else(|| malformed_version(group))?;
                if number >= FIRST_UNICODE_VERSION {
                    self.unicode = true;
                    self.charset = Charset::named("UTF-8");
                }
                xhtml.metadata_mut().set("dxf:version", version);
            }
            (Variable::CodePage, 3) if !self.unicode => {
                let name = std::str::from_utf8(text_of(group)?).map_err(|_| {
                    Failure::Malformed(format!(
                        "The DXF code page {} is not one AutoCAD writes",
                        group.value.shown()
                    ))
                })?;
                self.charset = text::code_page(name)?;
            }
            (Variable::Created, 40) => date(group, "dcterms:created", xhtml)?,
            (Variable::Modified, 40) => date(group, "dcterms:modified", xhtml)?,
            _ => {}
        }
        Ok(())
    }

    fn entity(&mut self, group: Group<'a>, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let Self {
            entity,
            charset,
            out,
            paragraphs,
            dimension_block,
            ..
        } = self;
        let charset = *charset;
        match (entity, group.code) {
            (Entity::Block, 2) => *dimension_block = text_of(group)?.starts_with(b"*D"),
            (Entity::Text { read }, 1) if !*read => {
                *read = true;
                let mut decoder = Decoder::new(Syntax::Text);
                write(
                    &mut decoder,
                    string_of(group)?,
                    charset,
                    out,
                    paragraphs,
                    xhtml,
                )?;
            }
            (Entity::Text { read }, EMBEDDED_OBJECT) => *read = true,
            (Entity::Mtext { decoder, closed }, 1 | 3) if !*closed => {
                decoder.decode(string_of(group)?, charset, out, |out| {
                    paragraphs.write(out, xhtml)
                })?;
            }
            (Entity::Mtext { closed, .. }, EMBEDDED_OBJECT) => *closed = true,
            (Entity::Dimension { text, closed, .. }, 1) if !*closed => {
                *text = Some(string_of(group)?);
            }
            (Entity::Dimension { measurement, .. }, 42) => {
                *measurement = Some(
                    group
                        .value
                        .real()
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| malformed_number(group))?,
                );
            }
            (Entity::Dimension { angular, .. }, 70) => {
                let kind = group
                    .value
                    .integer()
                    .ok_or_else(|| malformed_number(group))?;
                *angular = ANGULAR_DIMENSIONS.contains(&(kind & 0x0f));
            }
            (Entity::Dimension { closed, .. }, EMBEDDED_OBJECT) => *closed = true,
            (
                Entity::Record {
                    cookie: cookie @ None,
                },
                1,
            ) => {
                *cookie = Some(text_of(group)? == MARKER_2000);
            }
            (Entity::Record { cookie: Some(true) }, code) => {
                property(code, group, charset, xhtml.metadata_mut())?
            }
            _ => {}
        }
        Ok(())
    }

    /// Writes what is left of the text of the entity read.
    fn end_entity(&mut self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        match std::mem::replace(&mut self.entity, Entity::Other) {
            Entity::Mtext { mut decoder, .. } => {
                decoder.finish(&mut self.out);
                self.paragraphs.write(&mut self.out, xhtml)?;
                self.paragraphs.end(xhtml)?;
            }
            Entity::Dimension {
                text,
                measurement,
                angular,
                ..
            } => {
                let text: &[u8] = match text.unwrap_or_default() {
                    b" " => return Ok(()),
                    b"" => b"<>",
                    text => text,
                };
                let measurement = measurement
                    .map(|value| measured(value, angular))
                    .unwrap_or_default();
                let mut decoder = Decoder::dimension(measurement);
                write(
                    &mut decoder,
                    text,
                    self.charset,
                    &mut self.out,
                    &mut self.paragraphs,
                    xhtml,
                )?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// Writes the string `bytes`, whole, in its paragraphs.
fn write(
    decoder: &mut Decoder,
    bytes: &[u8],
    charset: Charset,
    out: &mut String,
    paragraphs: &mut Paragraphs,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    decoder.decode(bytes, charset, out, |out| paragraphs.write(out, xhtml))?;
    decoder.finish(out);
    paragraphs.write(out, xhtml)?;
    paragraphs.end(xhtml)?;
    Ok(())
}

/// The text of the string `bytes`, of a header variable or property.
fn value_text(bytes: &[u8], charset: Charset) -> String {
    let mut decoder = Decoder::new(Syntax::Value);
    let mut text = String::new();
    let Ok(()) = decoder.decode(bytes, charset, &mut text, |_| {
        Ok::<(), std::convert::Infallible>(())
    });
    decoder.finish(&mut text);
    text
}

/// Gives the metadata the drawing property of group `code` of the `DWGPROPS` record.
fn property(
    code: u16,
    group: Group<'_>,
    charset: Charset,
    metadata: &mut TikaMetadata,
) -> Result<(), Failure> {
    let field = match code {
        300..=309 => None,
        _ => match HEADER_2000_PROPERTIES.get(usize::from(code)) {
            Some(Some(field)) => Some(*field),
            _ => return Ok(()),
        },
    };
    let value = value_text(string_of(group)?, charset);
    match field {
        Some(field) if !value.is_empty() => field.add(metadata, &value),
        Some(_) => {}
        None => {
            if let Some((name, value)) = value.split_once('=') {
                if !value.is_empty() {
                    metadata.add_prefixed(CUSTOM_PREFIX, name, value);
                }
            }
        }
    }
    Ok(())
}

/// Sets `key` to the date of the Julian day of `group`, unless it is zero, as it is unset.
fn date(group: Group<'_>, key: &str, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let day = group.value.real().ok_or_else(|| malformed_number(group))?;
    if day == 0.0 {
        return Ok(());
    }
    if !JULIAN_DAYS.contains(&day) {
        return Err(Failure::Malformed(format!(
            "The DXF date {day} is not a Julian day of the years 1 to 9999"
        )));
    }
    let seconds = ((day - UNIX_EPOCH_JULIAN_DAY) * SECONDS_PER_DAY).round() as i64;
    xhtml.metadata_mut().set(key, instant_string(seconds, 0));
    Ok(())
}

/// A dimension's measurement `value` as its text gives it: to four decimals at most, an angle
/// in degrees.
fn measured(value: f64, angular: bool) -> String {
    let value = if angular { value.to_degrees() } else { value };
    let mut text = format!("{value:.4}");
    let kept = text.trim_end_matches('0').trim_end_matches('.').len();
    text.truncate(kept);
    if text == "-0" {
        text.remove(0);
    }
    if angular {
        text.push('\u{b0}');
    }
    text
}

/// The bytes of the string `group` holds, its leading and trailing whitespace kept.
fn string_of(group: Group<'_>) -> Result<&[u8], Failure> {
    group.value.text().ok_or_else(|| {
        Failure::Malformed(format!(
            "The DXF group {} holds {} where it holds a string",
            group.code,
            group.value.shown()
        ))
    })
}

/// The bytes of the name `group` holds, without its surrounding whitespace.
fn text_of(group: Group<'_>) -> Result<&[u8], Failure> {
    string_of(group).map(<[u8]>::trim_ascii)
}

fn malformed_number(group: Group<'_>) -> Failure {
    Failure::Malformed(format!(
        "The DXF group {} holds {} where it holds a number",
        group.code,
        group.value.shown()
    ))
}

fn malformed_version(group: Group<'_>) -> Failure {
    Failure::Malformed(format!(
        "The DXF version {} is not an AutoCAD version",
        group.value.shown()
    ))
}

#[cfg(test)]
mod tests;
