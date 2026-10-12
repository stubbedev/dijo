use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::path::Path;

use extractous_core::{EmbeddedSpool, Extractor, Metadata, SpooledEmbedded, StreamReader};

use crate::cli::Format;
use serde::Serialize;

use crate::language::{self, DetectedLanguage, Sample};
use crate::normalize::{self, Normalizer, Steps};
use crate::schema::{self, ErrorCategory, ExtractionError, Source, Status, SCHEMA_VERSION};
use crate::structure::{self, ElementKind, Unit};

const READ_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    pub format: Format,
    pub metadata: bool,
    pub raw_metadata: bool,
    pub normalization: Steps,
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

/// Starts extracting `path`; Tika has detected the type and found a parser once this returns.
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
    let status = match (errors.is_empty(), produced_text, encrypted) {
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

/// Fetches the metadata Tika finished with and the documents it spooled; a failure to do so
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
    let steps = match options.format {
        Format::Xhtml => Steps::NONE,
        _ => options.normalization,
    };
    let mut copied = copy(&mut stream, sink, steps, None, |chunk, sink| {
        sink.write_all(chunk)
    })?;
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
        "{{\"schema_version\":{SCHEMA_VERSION},\"kind\":\"document\",\"source\":"
    )?;
    serde_json::to_writer(&mut *sink, source)?;
    Ok(())
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
            let mut copied = copy(
                &mut stream,
                sink,
                options.normalization,
                Some(&mut sample),
                write_json_string_content,
            )?;
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
            let mut bytes = 0;
            let split = structure::split(BufReader::new(&mut stream), unit, &mut |part| {
                bytes += part.text.len() as u64;
                let text = normalize::normalize(options.normalization, &part.text);
                sample.push(text.as_bytes());
                sample.push(b"\n");
                let record = PartRecord {
                    schema_version: SCHEMA_VERSION,
                    kind: if part.element.is_some() {
                        "element"
                    } else {
                        "page"
                    },
                    source: PartSource {
                        path: &source.path,
                        embedded_path: source.embedded_path.as_deref(),
                    },
                    page: part.page,
                    element: part.element.map(|(kind, _)| kind),
                    level: part.element.and_then(|(_, level)| level),
                    text: &text,
                    detected_language: match unit {
                        Unit::Pages => language::detect(&text),
                        Unit::Elements => None,
                    },
                };
                serde_json::to_writer(&mut *sink, &record)?;
                sink.write_all(b"\n")
            })?;
            let mut copied = Copied {
                bytes,
                error: split.err().map(ExtractionError::from_message),
            };
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

/// Copies the extracted text into the sink, normalised; fails only when the sink does.
fn copy(
    stream: &mut StreamReader,
    sink: &mut dyn Write,
    steps: Steps,
    mut sample: Option<&mut Sample>,
    write: impl Fn(&[u8], &mut dyn Write) -> io::Result<()>,
) -> io::Result<Copied> {
    let mut buffer = vec![0; READ_BUFFER_SIZE];
    let mut normalizer = Normalizer::new(steps);
    let mut normalized = Vec::with_capacity(READ_BUFFER_SIZE);
    let mut bytes = 0;
    let error = loop {
        match stream.read(&mut buffer) {
            Ok(0) => break None,
            Ok(read) => {
                normalizer.push(&buffer[..read], &mut normalized);
                if let Some(sample) = sample.as_deref_mut() {
                    sample.push(&normalized);
                }
                write(&normalized, sink)?;
                normalized.clear();
                bytes += read as u64;
            }
            Err(error) => break Some(ExtractionError::from_io(&error)),
        }
    };
    normalizer.finish(&mut normalized);
    if let Some(sample) = sample {
        sample.push(&normalized);
    }
    write(&normalized, sink)?;
    Ok(Copied { bytes, error })
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

    #[test]
    fn json_escaping_matches_serde_across_chunk_boundaries() {
        let text = "quote \" backslash \\ newline \n tab \t bell \u{7} backspace \u{8} feed \u{c} æøå 漢字 🎉";
        let expected = serde_json::to_string(text).unwrap();
        assert_eq!(format!("\"{}\"", escape(text)), expected);
    }
}
