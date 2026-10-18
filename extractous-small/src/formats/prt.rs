//! CADKEY part files, as Tika's `PRTParser` reads them: the date and description of the header,
//! and the note texts and view names it finds by their markers and the five bytes before them,
//! decoded as code page 437. A description that fills its field keeps its last byte, which
//! Tika's drops, and an empty text is skipped, where Tika's fails on it (#118).

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared};
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

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        Ok(Box::new(move |xhtml| {
            let mut stream = Stream {
                bytes: input,
                at: 0,
            };
            let parsed = parse(&mut stream, xhtml);
            xhtml.end_document()?;
            parsed
        }))
    }
}

/// The part file as Tika reads its stream.
struct Stream<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Stream<'a> {
    /// `InputStream.read()`: a byte, or `None` at the end, where Java reads -1.
    fn read(&mut self) -> Option<u8> {
        let byte = self.bytes.get(self.at).copied()?;
        self.at += 1;
        Some(byte)
    }

    /// `IOUtils.readFully`.
    fn read_fully(&mut self, length: usize) -> Result<&'a [u8], Failure> {
        let read = self
            .bytes
            .get(self.at..self.at + length)
            .ok_or_else(|| Failure::Malformed("EOFException: the part file ends early".into()))?;
        self.at += length;
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

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, assert_same_as_tika_departing,
        conformance_corpus, documents, Departure, Scratch,
    };

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

    fn view(text: &[u8]) -> Vec<u8> {
        let mut view = vec![0; 5];
        view.extend_from_slice(&[0xf0, 0xbf]);
        view.extend_from_slice(&u16::try_from(text.len() + 1).unwrap().to_le_bytes());
        view.extend_from_slice(text);
        view.push(0);
        view
    }

    #[test]
    fn extracts_part_files_as_tikas_path_does() {
        let scratch = Scratch::new("prt");
        let body = [
            note(b"Bolt M8 \xe8 galvaniseret"),
            vec![1, 2, 3],
            view(b"FRONT"),
            vec![0; 5],
            vec![0xe0, 0x3f, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0],
            b"TOP\x00\x00".to_vec(),
            view(b""),
        ]
        .concat();
        let read = [
            scratch.write("bolt.prt", &part(b"200803201052", b"Bolt \xe8", &body)),
            scratch.write("undated.prt", &part(b"xx0803201052", b"", &body)),
        ];
        let failing = [
            scratch.write("cut.prt", &part(b"200803201052", b"Bolt", &[])[..300]),
            scratch.write("cut-note.prt", &part(b"200803201052", b"Bolt", &body[..40])),
        ];
        let full = scratch.write("full.prt", &part(b"200803201052", &[b'M'; 500], &body));
        let (parsed, deferred) = documents(&PrtParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len() + failing.len() + 1, "{parsed:?}");
        assert_same_as_tika(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            PrtParser.tika_name(),
        );
        assert_same_as_tika_departing(
            &[full.as_path()],
            PrtParser.tika_name(),
            &[Departure::Values("dc:description", "#118")],
        );
        assert_fails_as_tika(&failing.iter().map(PathBuf::as_path).collect::<Vec<_>>());
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

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_part_files_as_tikas_path_does() {
        let (parsed, deferred) = documents(&PrtParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, PrtParser.tika_name());
    }
}
