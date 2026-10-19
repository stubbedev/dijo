use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::num::NonZeroU64;
use std::path::Path;

use extractous_core::{EmbeddedSpool, Extractor, Metadata, SpooledEmbedded, StreamReader};

use crate::cli::Format;
use serde::{Deserialize, Serialize};

use crate::language::{self, DetectedLanguage, Sample};
use crate::limit::OutputLimit;
use crate::normalize::{self, Normalizer, Step, Steps};
use crate::running::{self, Placement, RunningLines};
use crate::schema::{self, ErrorCategory, ExtractionError, Source, Status, SCHEMA_VERSION};
use crate::structure::{self, ElementKind, SplitError, Unit};

const READ_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RenderOptions {
    pub format: Format,
    pub metadata: bool,
    pub raw_metadata: bool,
    pub normalization: Steps,
    /// The most characters of text the output may have; past them it is cut short.
    pub max_chars: Option<NonZeroU64>,
}

impl RenderOptions {
    /// Whether the text is rendered from Tika's XHTML: for the formats that are XHTML or split
    /// it, and for text and json when running headers and footers are moved out, which needs
    /// the page boundaries.
    pub fn needs_xhtml(self) -> bool {
        match self.format {
            Format::Xhtml | Format::Pages | Format::Elements => true,
            Format::Text | Format::Json => self.normalization.contains(Step::Headers),
        }
    }

    /// Whether untagged PDF pages are read in reading order: for every format that is
    /// normalised, which XHTML never is.
    pub fn reads_in_reading_order(self) -> bool {
        self.normalizes(Step::ReadingOrder)
    }

    /// Whether a PDF's bookmarks are text: for XHTML, which is never normalised, and unless
    /// they are left out.
    pub fn keeps_bookmarks(self) -> bool {
        !self.normalizes(Step::Bookmarks)
    }

    fn normalizes(self, step: Step) -> bool {
        self.format != Format::Xhtml && self.normalization.contains(step)
    }
}

/// Where a part sits in its document, and the page furniture taken out of its text.
struct PartPlace {
    page: Option<u32>,
    element: Option<(ElementKind, Option<u8>)>,
    /// The text the document marks as furniture of the page, such as a tagged PDF's running
    /// headers and footers, when running headers and footers are moved out.
    artifacts: Option<String>,
    /// Whether the part's text goes on in the next part.
    continued: bool,
}

/// What extracting one document produced besides the output written to the sink.
#[derive(Debug)]
pub struct Outcome {
    pub status: Status,
    pub errors: Vec<ExtractionError>,
    pub metadata: Option<Metadata>,
    /// The documents embedded in this one, when they were spooled rather than inlined.
    pub embedded: Vec<SpooledEmbedded>,
}

/// Text read from a stream until it ended or failed.
struct Copied {
    bytes: u64,
    error: Option<ExtractionError>,
}

/// An extraction whose text has not been read yet, or why it could not start.
pub type Opened = Result<(StreamReader, Metadata), ExtractionError>;

/// Starts extracting `path`; the type is detected and a parser found once this returns.
pub fn open(extractor: &Extractor, path: &Path) -> Opened {
    let path = path.to_str().ok_or_else(|| {
        ExtractionError::from_message(format!("{} is not valid UTF-8", path.display()))
    })?;
    extractor
        .extract_file(path)
        .map_err(|e| ExtractionError::from_error(&e))
}

/// Starts extracting `path` with the documents embedded in it copied to `spool` instead of
/// contributing their text.
pub fn open_spooling(extractor: &Extractor, path: &Path, spool: &EmbeddedSpool) -> Opened {
    let path = path.to_str().ok_or_else(|| {
        ExtractionError::from_message(format!("{} is not valid UTF-8", path.display()))
    })?;
    extractor
        .extract_file_spooling_embedded(path, spool)
        .map_err(|e| ExtractionError::from_error(&e))
}

/// Writes the extraction of `path` to `sink`. Extraction failures are reported in the returned
/// outcome; an `Err` means the sink itself could not be written.
pub fn render(
    extractor: &Extractor,
    path: &Path,
    display: &str,
    options: RenderOptions,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    let source = source(path, display, None);
    render_opened(open(extractor, path), &source, options, sink)
}

/// Writes an extraction that was already opened, reporting it under `source`.
pub fn render_opened(
    opened: Opened,
    source: &Source,
    options: RenderOptions,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    match options.format {
        Format::Text | Format::Xhtml => render_text(opened, options, sink),
        Format::Json => render_json(opened, source, options, sink),
        Format::Pages => render_parts(opened, source, options, Unit::Pages, sink),
        Format::Elements => render_parts(opened, source, options, Unit::Elements, sink),
    }
}

fn outcome(
    copied: Option<&Copied>,
    open_error: Option<ExtractionError>,
    final_metadata: Option<&Metadata>,
) -> Outcome {
    let error = open_error.or_else(|| copied.and_then(|copied| copied.error.clone()));
    let errors: Vec<ExtractionError> = error
        .into_iter()
        .chain(
            final_metadata
                .map(schema::recorded_failures)
                .unwrap_or_default(),
        )
        .collect();
    let produced_text = copied.is_some_and(|copied| copied.bytes > 0);
    let encrypted = errors
        .iter()
        .any(|error| error.category == ErrorCategory::Encrypted);
    let only_differences = errors
        .iter()
        .all(|error| error.category == ErrorCategory::DiffersFromTika);
    let status = match (
        errors.is_empty(),
        produced_text || only_differences,
        encrypted,
    ) {
        (true, _, _) => Status::Complete,
        (false, true, _) => Status::Partial,
        (false, false, true) => Status::Encrypted,
        (false, false, false) => Status::Failed,
    };
    Outcome {
        status,
        errors,
        metadata: None,
        embedded: Vec::new(),
    }
}

/// Fetches the metadata the parse finished with and the documents it spooled; a failure to do so
/// fails the extraction.
fn finish(stream: &StreamReader, copied: &mut Copied) -> (Option<Metadata>, Vec<SpooledEmbedded>) {
    if copied.error.is_some() {
        return (None, Vec::new());
    }
    match stream
        .metadata()
        .and_then(|metadata| Ok((metadata, stream.embedded()?)))
    {
        Ok((metadata, embedded)) => (Some(metadata), embedded),
        Err(error) => {
            copied.error = Some(ExtractionError::from_error(&error));
            (None, Vec::new())
        }
    }
}

fn render_text(
    opened: Opened,
    options: RenderOptions,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    let mut stream = match opened {
        Ok((stream, _)) => stream,
        Err(error) => return Ok(outcome(None, Some(error), None)),
    };
    let write = |chunk: &[u8], sink: &mut dyn Write| sink.write_all(chunk);
    let mut copied = match options.format {
        Format::Xhtml => copy(
            &mut stream,
            sink,
            Steps::NONE,
            OutputLimit::xhtml(options.max_chars),
            None,
            write,
        )?,
        _ if options.needs_xhtml() => copy_pages(&mut stream, sink, options, None, write)?,
        _ => copy(
            &mut stream,
            sink,
            options.normalization,
            OutputLimit::text(options.max_chars),
            None,
            write,
        )?,
    };
    let (metadata, embedded) = finish(&stream, &mut copied);
    Ok(Outcome {
        metadata: metadata.clone().filter(|_| options.metadata),
        embedded,
        ..outcome(Some(&copied), None, metadata.as_ref())
    })
}

pub fn write_document_start(sink: &mut dyn Write, source: &Source) -> io::Result<()> {
    write!(
        sink,
        "{{\"schema_version\":{SCHEMA_VERSION},\"kind\":\"document\","
    )?;
    sink.write_all(&serialized_field("source", source))
}

pub fn write_document_end(
    sink: &mut dyn Write,
    outcome: &Outcome,
    metadata: Option<&Metadata>,
    detected_language: Option<&DetectedLanguage>,
    options: RenderOptions,
) -> io::Result<()> {
    if let Some(mime_type) = metadata.and_then(schema::mime_type) {
        write_field(sink, "mime_type", &mime_type)?;
    }
    if let Some(detected_language) = detected_language {
        write_field(sink, "detected_language", detected_language)?;
    }
    write_field(sink, "status", &outcome.status)?;
    write_field(sink, "errors", &outcome.errors)?;
    if let Some(metadata) = metadata {
        if options.metadata {
            write_field(sink, "metadata", &schema::normalize(metadata))?;
        }
        if options.raw_metadata {
            write_field(
                sink,
                "raw_metadata",
                &metadata.iter().collect::<BTreeMap<_, _>>(),
            )?;
        }
    }
    sink.write_all(b"}\n")
}

fn render_json(
    opened: Opened,
    source: &Source,
    options: RenderOptions,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    write_document_start(sink, source)?;
    let mut sample = Sample::default();
    let (outcome, metadata) = match opened {
        Err(error) => (outcome(None, Some(error), None), None),
        Ok((mut stream, _)) => {
            sink.write_all(b",\"text\":\"")?;
            let mut copied = if options.needs_xhtml() {
                copy_pages(
                    &mut stream,
                    sink,
                    options,
                    Some(&mut sample),
                    write_json_string_content,
                )?
            } else {
                copy(
                    &mut stream,
                    sink,
                    options.normalization,
                    OutputLimit::text(options.max_chars),
                    Some(&mut sample),
                    write_json_string_content,
                )?
            };
            sink.write_all(b"\"")?;
            let (metadata, embedded) = finish(&stream, &mut copied);
            let outcome = Outcome {
                embedded,
                ..outcome(Some(&copied), None, metadata.as_ref())
            };
            (outcome, metadata)
        }
    };
    write_document_end(
        sink,
        &outcome,
        metadata.as_ref(),
        sample.detect().as_ref(),
        options,
    )?;
    Ok(outcome)
}

#[derive(Serialize)]
struct PartRecord<'a> {
    schema_version: u32,
    kind: &'static str,
    source: PartSource<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    element: Option<ElementKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<u8>,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    header: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    footer: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifacts: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detected_language: Option<DetectedLanguage>,
}

#[derive(Serialize)]
struct PartSource<'a> {
    path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    embedded_path: Option<&'a str>,
}

fn render_parts(
    opened: Opened,
    source: &Source,
    options: RenderOptions,
    unit: Unit,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    let mut sample = Sample::default();
    let (outcome, metadata) = match opened {
        Err(error) => (outcome(None, Some(error), None), None),
        Ok((mut stream, _)) => {
            let mut run = String::new();
            let mut run_header = None;
            let mut emit = |place: PartPlace, part: running::Page| {
                let PartPlace {
                    page,
                    element,
                    artifacts,
                    continued,
                } = place;
                run.push_str(&part.text);
                run_header = run_header.take().or(part.header);
                if continued {
                    return Ok(());
                }
                let text = std::mem::take(&mut run);
                let header = run_header.take();
                sample.push(text.as_bytes());
                sample.push(b"\n");
                let record = PartRecord {
                    schema_version: SCHEMA_VERSION,
                    kind: if element.is_some() { "element" } else { "page" },
                    source: PartSource {
                        path: &source.path,
                        embedded_path: source.embedded_path.as_deref(),
                    },
                    page,
                    element: element.map(|(kind, _)| kind),
                    level: element.and_then(|(_, level)| level),
                    detected_language: match unit {
                        Unit::Pages => language::detect(&text),
                        Unit::Elements => None,
                    },
                    text: &text,
                    header: header.as_deref(),
                    footer: part.footer.as_deref(),
                    artifacts: artifacts.as_deref(),
                };
                serde_json::to_writer(&mut *sink, &record)?;
                sink.write_all(b"\n")
            };
            let mut copied = split_parts(&mut stream, options, unit, &mut emit)?;
            let (metadata, embedded) = finish(&stream, &mut copied);
            let outcome = Outcome {
                embedded,
                ..outcome(Some(&copied), None, metadata.as_ref())
            };
            (outcome, metadata)
        }
    };
    write_document_start(sink, source)?;
    write_document_end(
        sink,
        &outcome,
        metadata.as_ref(),
        sample.detect().as_ref(),
        options,
    )?;
    Ok(outcome)
}

/// How records serialize their provenance: whole in document records, path and embedded path
/// in page and element records.
pub fn serialized_sources(source: &Source) -> [Vec<u8>; 2] {
    let part = PartSource {
        path: &source.path,
        embedded_path: source.embedded_path.as_deref(),
    };
    [
        serialized_field("source", source),
        serialized_field("source", &part),
    ]
}

fn serialized_field(name: &str, value: &impl serde::Serialize) -> Vec<u8> {
    let mut field = format!("\"{name}\":").into_bytes();
    serde_json::to_writer(&mut field, value).expect("serializing to memory cannot fail");
    field
}

fn write_field(sink: &mut dyn Write, name: &str, value: &impl serde::Serialize) -> io::Result<()> {
    write!(sink, ",\"{name}\":")?;
    serde_json::to_writer(&mut *sink, value)?;
    Ok(())
}

/// The provenance of a document: the input it came from, where inside it when embedded, and its
/// size and BLAKE3 hash.
pub fn source(path: &Path, display: &str, embedded_path: Option<String>) -> Source {
    let digest = fs::File::open(path).and_then(|mut file| {
        let mut hasher = blake3::Hasher::new();
        let size = io::copy(&mut file, &mut hasher)?;
        Ok((size, hasher.finalize().to_hex().to_string()))
    });
    let (size, blake3) = match digest {
        Ok((size, hash)) => (Some(size), Some(hash)),
        Err(_) => (None, None),
    };
    Source {
        path: display.to_string(),
        embedded_path,
        size,
        blake3,
    }
}

/// Splits the extracted XHTML into pages or elements, normalises each, moves running headers and
/// footers out of pages, and passes each part to `emit`; fails only when `emit` does.
fn split_parts(
    stream: &mut StreamReader,
    options: RenderOptions,
    unit: Unit,
    emit: &mut dyn FnMut(PartPlace, running::Page) -> io::Result<()>,
) -> io::Result<Copied> {
    let mut bytes = 0;
    let headers = unit == Unit::Pages && options.normalization.contains(Step::Headers);
    let mut running = RunningLines::new(emit, headers);
    let mut run: Option<Normalizer> = None;
    let mut page_begun = false;
    let mut limit = OutputLimit::text(options.max_chars);
    let cut = Cell::new(false);
    let reader = UntilCut {
        inner: stream,
        cut: &cut,
    };
    let split = structure::split(reader, unit, &mut |part| {
        if cut.get() {
            return Ok(());
        }
        bytes += (part.text.len() + part.artifacts.len()) as u64;
        let mut text = if part.continued || run.is_some() {
            let normalizer = run.get_or_insert_with(|| Normalizer::new(options.normalization));
            let mut normalized = Vec::with_capacity(part.text.len());
            normalizer.push(part.text.as_bytes(), &mut normalized);
            if !part.continued {
                normalizer.finish(&mut normalized);
                run = None;
            }
            String::from_utf8(normalized).expect("normalisation produces UTF-8")
        } else {
            normalize::normalize(options.normalization, &part.text)
        };
        let mut artifacts = normalize::normalize(options.normalization, &part.artifacts);
        text.truncate(limit.admit(text.as_bytes()).len());
        artifacts.truncate(limit.admit(artifacts.as_bytes()).len());
        let reached = limit.reached().is_some();
        cut.set(reached);
        if !headers && !artifacts.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&std::mem::take(&mut artifacts));
        }
        let placement = match part.page {
            Some(_) => {
                let first_line = !page_begun;
                page_begun = part.continued && (page_begun || !text.trim().is_empty());
                Placement::Page {
                    first_line,
                    last_line: !part.continued,
                }
            }
            None => Placement::Loose,
        };
        let place = PartPlace {
            page: part.page,
            element: part.element,
            artifacts: (!artifacts.is_empty()).then_some(artifacts),
            continued: part.continued && !reached,
        };
        running.push(place, text, placement)
    })?;
    running.finish()?;
    let error = limit.reached().or_else(|| {
        split.err().map(|error| match error {
            SplitError::Read(error) => ExtractionError::from_io(&error),
            SplitError::Malformed(message) => ExtractionError::from_message(message),
        })
    });
    Ok(Copied { bytes, error })
}

/// Reads a stream until the output is cut short, and then reads nothing more, which ends the
/// extraction.
struct UntilCut<'c, R> {
    inner: R,
    cut: &'c Cell<bool>,
}

impl<R: Read> Read for UntilCut<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cut.get() {
            return Ok(0);
        }
        self.inner.read(buffer)
    }
}

/// Copies the text of the extracted XHTML's pages into the sink, normalised and without their
/// running headers and footers, a blank line between pages; fails only when the sink does.
fn copy_pages(
    stream: &mut StreamReader,
    sink: &mut dyn Write,
    options: RenderOptions,
    mut sample: Option<&mut Sample>,
    write: impl Fn(&[u8], &mut dyn Write) -> io::Result<()>,
) -> io::Result<Copied> {
    let mut wrote = false;
    let mut in_run = false;
    let copied = split_parts(stream, options, Unit::Pages, &mut |place, page| {
        let text = if place.continued {
            page.text.as_str()
        } else {
            page.text.trim_end_matches('\n')
        };
        if text.is_empty() {
            in_run &= place.continued;
            return Ok(());
        }
        let separator: &[u8] = if wrote && !in_run { b"\n\n" } else { b"" };
        for chunk in [separator, text.as_bytes()] {
            if let Some(sample) = sample.as_deref_mut() {
                sample.push(chunk);
            }
            write(chunk, sink)?;
        }
        wrote = true;
        in_run = place.continued;
        Ok(())
    })?;
    if wrote {
        write(b"\n", sink)?;
    }
    Ok(copied)
}

/// Copies the extracted text into the sink, normalised, until it ends or reaches `limit`; fails
/// only when the sink does.
fn copy(
    stream: &mut StreamReader,
    sink: &mut dyn Write,
    steps: Steps,
    mut limit: OutputLimit,
    mut sample: Option<&mut Sample>,
    write: impl Fn(&[u8], &mut dyn Write) -> io::Result<()>,
) -> io::Result<Copied> {
    let mut buffer = vec![0; READ_BUFFER_SIZE];
    let mut normalizer = Normalizer::new(steps);
    let mut normalized = Vec::with_capacity(READ_BUFFER_SIZE);
    let mut bytes = 0;
    let mut pass = |normalized: &[u8], limit: &mut OutputLimit, sink: &mut dyn Write| {
        let admitted = limit.admit(normalized);
        if let Some(sample) = sample.as_deref_mut() {
            sample.push(admitted);
        }
        write(admitted, sink)
    };
    let error = loop {
        match stream.read(&mut buffer) {
            Ok(0) => break None,
            Ok(read) => {
                normalizer.push(&buffer[..read], &mut normalized);
                pass(&normalized, &mut limit, sink)?;
                normalized.clear();
                bytes += read as u64;
                if limit.reached().is_some() {
                    break None;
                }
            }
            Err(error) => break Some(ExtractionError::from_io(&error)),
        }
    };
    normalizer.finish(&mut normalized);
    pass(&normalized, &mut limit, sink)?;
    write(&limit.closing(), sink)?;
    Ok(Copied {
        bytes,
        error: limit.reached().or(error),
    })
}

/// Writes UTF-8 bytes as the inside of a JSON string. Only ASCII needs escaping, so this works on
/// any chunk boundary, even one that splits a multi-byte character.
fn write_json_string_content(bytes: &[u8], sink: &mut dyn Write) -> io::Result<()> {
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        let escaped: &[u8] = match byte {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x08 => b"\\b",
            0x0c => b"\\f",
            0x00..=0x1f => &[],
            _ => continue,
        };
        sink.write_all(&bytes[start..index])?;
        if escaped.is_empty() {
            write!(sink, "\\u{byte:04x}")?;
        } else {
            sink.write_all(escaped)?;
        }
        start = index + 1;
    }
    sink.write_all(&bytes[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escape(text: &str) -> String {
        let mut out = Vec::new();
        for chunk in text.as_bytes().chunks(3) {
            write_json_string_content(chunk, &mut out).unwrap();
        }
        String::from_utf8(out).unwrap()
    }

    fn error(message: &str) -> ExtractionError {
        ExtractionError::from_message(message.to_owned())
    }

    #[test]
    fn a_document_only_read_differently_from_tika_is_partial_not_failed() {
        let differs = error("extractous::FlvParser read the document differently from Tika: UTC");
        assert_eq!(
            outcome(None, Some(differs.clone()), None).status,
            Status::Partial
        );
        let failure = error("java.io.IOException: broken");
        assert_eq!(outcome(None, Some(failure), None).status, Status::Failed);
        let copied = Copied {
            bytes: 1,
            error: None,
        };
        assert_eq!(
            outcome(Some(&copied), Some(differs), None).status,
            Status::Partial
        );
    }

    #[test]
    fn json_escaping_matches_serde_across_chunk_boundaries() {
        let text = "quote \" backslash \\ newline \n tab \t bell \u{7} backspace \u{8} feed \u{c} æøå 漢字 🎉";
        let expected = serde_json::to_string(text).unwrap();
        assert_eq!(format!("\"{}\"", escape(text)), expected);
    }
}
