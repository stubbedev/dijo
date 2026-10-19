//! CADKEY part files, as Tika's `PRTParser` reads them: the date and description of the header,
//! and the note texts and view names it finds by their markers and the five bytes before them,
//! decoded as code page 437. A description that fills its field keeps its last byte, which
//! Tika's drops, and an empty text is skipped, where Tika's fails on it (#118).

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
use crate::java;
use crate::text::charset::Charset;

/// `MAX_TEXT_LENGTH`.
const MAX_TEXT_LENGTH: u16 = 0x0800;

/// Tika's `PRTParser`.
pub struct PrtParser;

impl Parser for PrtParser {
    fn name(&self) -> &'static str {
        "extractous::PRTParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.prt.PRTParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-prt"]
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
        Ok(Box::new(move |xhtml| {
            let mut stream = Stream {
                bytes: input,
                at: 0,
                paging: Paging::new(pages),
            };
            let parsed = parse(&mut stream, xhtml);
            xhtml.end_document()?;
            parsed
        }))
    }
}

/// The part file as Tika reads its stream, whose pages it releases as it reads on.
struct Stream<'a> {
    bytes: &'a [u8],
    at: usize,
    paging: Paging<'a>,
}

impl<'a> Stream<'a> {
    /// `InputStream.read()`: a byte, or `None` at the end, where Java reads -1.
    fn read(&mut self) -> Option<u8> {
        let byte = self.bytes.get(self.at).copied()?;
        self.at += 1;
        self.paging.read(1);
        Some(byte)
    }

    /// `IOUtils.readFully`.
    fn read_fully(&mut self, length: usize) -> Result<&'a [u8], Failure> {
        let read = self
            .bytes
            .get(self.at..self.at + length)
            .ok_or_else(|| Failure::Malformed("EOFException: the part file ends early".into()))?;
        self.at += length;
        self.paging.read(length);
        Ok(read)
    }

    /// `EndianUtils.readUShortLE`.
    fn u16_le(&mut self) -> Result<u16, Failure> {
        let bytes = self.read_fully(2).map_err(|_| {
            self.at = self.bytes.len();
            Failure::Malformed("BufferUnderrunException".into())
        })?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Whether the next `count` bytes are all at most 0x0F, read up to the first that is not.
    fn small_bytes(&mut self, count: usize) -> bool {
        (0..count).all(|_| self.read().is_some_and(|byte| byte <= 0x0f))
    }
}

/// `Last5`: the bytes recorded last, in a ring of five.
#[derive(Debug, Default)]
struct Last5 {
    data: [u8; 5],
    position: usize,
}

impl Last5 {
    fn record(&mut self, byte: u8) {
        self.data[self.position] = byte;
        self.position = (self.position + 1) % self.data.len();
    }

    /// `get`: the ring from the next position backwards, which is all five.
    fn all(&self, byte: u8) -> bool {
        (0..self.data.len()).all(|back| {
            self.data[(self.position + self.data.len() - back) % self.data.len()] == byte
        })
    }
}

/// `extractText`: `data` as code page 437, with φ read as Ø.
fn text(data: &[u8]) -> String {
    Charset::named("cp437")
        .decode(data)
        .expect("the JDK decoders decode code page 437")
        .replace('\u{3C6}', "\u{D8}")
}

fn parse(stream: &mut Stream<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    stream.read_fully(30)?;
    let date = java::ascii(stream.read_fully(12)?);
    let metadata = xhtml.metadata_mut();
    if date.starts_with("19") || date.starts_with("20") {
        let date: Vec<char> = date.chars().collect();
        let part = |range: std::ops::Range<usize>| date[range].iter().collect::<String>();
        let formatted = format!(
            "{}-{}-{}T{}:{}:00",
            part(0..4),
            part(4..6),
            part(6..8),
            part(8..10),
            part(10..12)
        );
        metadata.set("dcterms:created", formatted.as_str());
        metadata.set("dcterms:modified", formatted);
    }
    metadata.set("Content-Type", "application/x-prt");
    let field = stream.read_fully(500)?;
    let description = text(&field[..memchr::memchr(0, field).unwrap_or(field.len())]);
    if !description.is_empty() {
        metadata.set("dc:description", description);
    }
    let mut last = Last5::default();
    while let Some(byte) = stream.read() {
        if !matches!(byte, 0xe0 | 0xe3 | 0xf0) {
            last.record(byte);
            continue;
        }
        let next = stream.read();
        if !matches!(next, Some(0x3f | 0xbf)) {
            continue;
        }
        if byte == 0xe3 && next == Some(0x3f) {
            if last.all(0x33) {
                note(stream, xhtml)?;
            }
        } else if last.all(0x00) {
            view_name(stream, xhtml, &mut last)?;
        }
    }
    Ok(())
}

/// `handleNoteText`.
fn note(stream: &mut Stream<'_>, xhtml: &mut Xhtml) -> Result<(), Failure> {
    if !stream.small_bytes(10) || stream.read() != Some(0x1f) {
        return Ok(());
    }
    let length = stream.u16_le()?;
    if length <= MAX_TEXT_LENGTH {
        paragraph(stream, usize::from(length), xhtml)?;
    }
    Ok(())
}

/// `handleViewName`.
fn view_name(stream: &mut Stream<'_>, xhtml: &mut Xhtml, last: &mut Last5) -> Result<(), Failure> {
    let maybe_length = stream.u16_le()?;
    if maybe_length == 0 {
        if !stream.small_bytes(6) {
            return Ok(());
        }
        let bytes = stream.read_fully(2)?;
        let length = u16::from_le_bytes([bytes[0], bytes[1]]);
        if length > 1 && length <= MAX_TEXT_LENGTH {
            paragraph(stream, usize::from(length), xhtml)?;
        } else {
            last.record(bytes[0]);
            last.record(bytes[1]);
        }
    } else if maybe_length < MAX_TEXT_LENGTH {
        paragraph(stream, usize::from(maybe_length), xhtml)?;
    }
    Ok(())
}

/// `handleText`: a paragraph of `length` bytes that end in a zero.
fn paragraph(stream: &mut Stream<'_>, length: usize, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let data = stream.read_fully(length)?;
    let Some((0, text_bytes)) = data.split_last() else {
        return Ok(());
    };
    xhtml.start_element("p")?;
    xhtml.characters(&text(text_bytes))?;
    xhtml.end_element("p")?;
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;

    fn part(date: &[u8; 12], description: &[u8], body: &[u8]) -> Vec<u8> {
        let mut file = b"CADKEY\x00\x000M3C".to_vec();
        file.resize(30, 0);
        file.extend_from_slice(date);
        let mut header = description.to_vec();
        header.resize(500, 0);
        file.extend(header);
        file.extend_from_slice(body);
        file
    }

    fn note(text: &[u8]) -> Vec<u8> {
        let mut note = vec![0x33; 5];
        note.extend_from_slice(&[0xe3, 0x3f]);
        note.extend_from_slice(&[0; 10]);
        note.push(0x1f);
        note.extend_from_slice(&u16::try_from(text.len() + 1).unwrap().to_le_bytes());
        note.extend_from_slice(text);
        note.push(0);
        note
    }

    /// A part file too large to read whole is read a byte at a time, each note written as it
    /// is found.
    #[test]
    fn reads_part_files_too_large_to_read_whole() {
        let body = |index: u64| note(format!("Note {index}").as_bytes());
        let count = extractous_core::large::bodies_past_limit(body(0).len());
        let head = part(b"200803201052", b"Bolt", &[]);
        let file = extractous_core::large::LargeFile::new("long.prt", &head, count, body, b"");
        let mut notes = 0;
        let metadata = file.extract_lines(PrtParser.name(), |line| {
            if !line.is_empty() {
                assert_eq!(line, format!("Note {notes}"));
                notes += 1;
            }
        });
        assert_eq!(notes, count);
        assert_eq!(metadata["dc:description"], ["Bolt"]);
    }

    #[test]
    fn keeps_the_whole_of_a_description_that_fills_its_field() {
        let (_, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&part(b"200803201052", &[b'M'; 500], &note(b"Bolt")))
            .unwrap();
        assert_eq!(metadata["dc:description"], ["M".repeat(500)]);
    }

    #[test]
    fn skips_an_empty_note() {
        let empty = [note(b"")[..18].to_vec(), vec![0, 0], note(b"Bolt")].concat();
        let (text, _) = crate::Extractor::new()
            .extract_bytes_to_string(&part(b"200803201052", b"Bolt", &empty))
            .unwrap();
        assert_eq!(text.trim(), "Bolt");
    }
}
