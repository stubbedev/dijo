use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use extractous_core::{Extractor, Metadata, StreamReader};

use crate::cli::Format;
use crate::schema::{self, ExtractionError, Source, Status, SCHEMA_VERSION};

const READ_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    pub format: Format,
    pub metadata: bool,
    pub raw_metadata: bool,
}

/// What extracting one input produced besides the output written to the sink.
#[derive(Debug)]
pub struct Outcome {
    pub status: Status,
    pub errors: Vec<ExtractionError>,
    pub metadata: Option<Metadata>,
}

/// Text read from a stream until it ended or failed.
struct Copied {
    bytes: u64,
    error: Option<String>,
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
    let opened = path
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
        .and_then(|path| extractor.extract_file(path).map_err(|e| e.to_string()));
    match options.format {
        Format::Text | Format::Xhtml => render_text(opened, options, sink),
        Format::Json => render_json(opened, path, display, options, sink),
    }
}

fn outcome(
    copied: Option<&Copied>,
    open_error: Option<String>,
    metadata: Option<Metadata>,
) -> Outcome {
    let error = open_error.or_else(|| copied.and_then(|copied| copied.error.clone()));
    let produced_text = copied.is_some_and(|copied| copied.bytes > 0);
    let status = match (&error, produced_text) {
        (None, _) => Status::Complete,
        (Some(_), true) => Status::Partial,
        (Some(_), false) => Status::Failed,
    };
    Outcome {
        status,
        errors: error
            .into_iter()
            .map(ExtractionError::from_message)
            .collect(),
        metadata,
    }
}

/// Fetches the metadata Tika finished with; a failure to do so fails the extraction.
fn final_metadata(stream: &StreamReader, copied: &mut Copied) -> Option<Metadata> {
    if copied.error.is_some() {
        return None;
    }
    match stream.metadata() {
        Ok(metadata) => Some(metadata),
        Err(error) => {
            copied.error = Some(error.to_string());
            None
        }
    }
}

fn render_text(
    opened: Result<(StreamReader, Metadata), String>,
    options: RenderOptions,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    let mut stream = match opened {
        Ok((stream, _)) => stream,
        Err(error) => return Ok(outcome(None, Some(error), None)),
    };
    let mut copied = copy(&mut stream, sink, |chunk, sink| sink.write_all(chunk))?;
    let metadata = options
        .metadata
        .then(|| final_metadata(&stream, &mut copied))
        .flatten();
    Ok(outcome(Some(&copied), None, metadata))
}

fn render_json(
    opened: Result<(StreamReader, Metadata), String>,
    path: &Path,
    display: &str,
    options: RenderOptions,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    write!(sink, "{{\"schema_version\":{SCHEMA_VERSION},\"source\":")?;
    serde_json::to_writer(&mut *sink, &source(path, display))?;

    let (outcome, metadata) = match opened {
        Err(error) => (outcome(None, Some(error), None), None),
        Ok((mut stream, _)) => {
            sink.write_all(b",\"text\":\"")?;
            let mut copied = copy(&mut stream, sink, write_json_string_content)?;
            sink.write_all(b"\"")?;
            let metadata = final_metadata(&stream, &mut copied);
            (outcome(Some(&copied), None, None), metadata)
        }
    };

    if let Some(mime_type) = metadata.as_ref().and_then(schema::mime_type) {
        write_field(sink, "mime_type", &mime_type)?;
    }
    write_field(sink, "status", &outcome.status)?;
    write_field(sink, "errors", &outcome.errors)?;
    if let Some(metadata) = &metadata {
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
    sink.write_all(b"}\n")?;
    Ok(outcome)
}

fn write_field(sink: &mut dyn Write, name: &str, value: &impl serde::Serialize) -> io::Result<()> {
    write!(sink, ",\"{name}\":")?;
    serde_json::to_writer(&mut *sink, value)?;
    Ok(())
}

fn source(path: &Path, display: &str) -> Source {
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
        size,
        blake3,
    }
}

/// Copies the extracted text into the sink; fails only when the sink does.
fn copy(
    stream: &mut StreamReader,
    sink: &mut dyn Write,
    write: impl Fn(&[u8], &mut dyn Write) -> io::Result<()>,
) -> io::Result<Copied> {
    let mut buffer = vec![0; READ_BUFFER_SIZE];
    let mut bytes = 0;
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(Copied { bytes, error: None }),
            Ok(read) => {
                write(&buffer[..read], sink)?;
                bytes += read as u64;
            }
            Err(error) => {
                return Ok(Copied {
                    bytes,
                    error: Some(error.to_string()),
                })
            }
        }
    }
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
