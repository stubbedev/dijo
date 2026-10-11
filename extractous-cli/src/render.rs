use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::Path;

use extractous_core::{Extractor, Metadata, StreamReader};

use crate::cli::Format;

const READ_BUFFER_SIZE: usize = 64 * 1024;

/// What extracting one input produced besides the output written to the sink.
#[derive(Debug, Default)]
pub struct Outcome {
    pub metadata: Option<Metadata>,
    pub error: Option<String>,
}

/// Writes the extraction of `path` to `sink` in `format`. Extraction failures are reported in
/// the returned outcome; an `Err` means the sink itself could not be written.
pub fn render(
    extractor: &Extractor,
    path: &Path,
    display: &str,
    format: Format,
    include_metadata: bool,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    let opened = path
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
        .and_then(|path| extractor.extract_file(path).map_err(|e| e.to_string()));
    match format {
        Format::Text | Format::Xhtml => render_text(opened, include_metadata, sink),
        Format::Json => render_json(opened, display, include_metadata, sink),
    }
}

fn render_text(
    opened: Result<(StreamReader, Metadata), String>,
    include_metadata: bool,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    let mut stream = match opened {
        Ok((stream, _)) => stream,
        Err(error) => return Ok(failed(error)),
    };
    if let Err(error) = copy(&mut stream, sink, |chunk, sink| sink.write_all(chunk))? {
        return Ok(failed(error));
    }
    Ok(finish(&stream, include_metadata))
}

fn render_json(
    opened: Result<(StreamReader, Metadata), String>,
    display: &str,
    include_metadata: bool,
    sink: &mut dyn Write,
) -> io::Result<Outcome> {
    sink.write_all(b"{\"source\":{\"path\":")?;
    serde_json::to_writer(&mut *sink, display)?;
    sink.write_all(b"}")?;

    let outcome = match opened {
        Err(error) => failed(error),
        Ok((mut stream, _)) => {
            sink.write_all(b",\"text\":\"")?;
            let copied = copy(&mut stream, sink, write_json_string_content)?;
            sink.write_all(b"\"")?;
            match copied {
                Err(error) => failed(error),
                Ok(()) => finish(&stream, include_metadata),
            }
        }
    };

    match &outcome.error {
        None => sink.write_all(b",\"status\":\"complete\",\"errors\":[]")?,
        Some(error) => {
            sink.write_all(b",\"status\":\"failed\",\"errors\":[")?;
            serde_json::to_writer(&mut *sink, error)?;
            sink.write_all(b"]")?;
        }
    }
    if let Some(metadata) = &outcome.metadata {
        sink.write_all(b",\"metadata\":")?;
        serde_json::to_writer(&mut *sink, &metadata.iter().collect::<BTreeMap<_, _>>())?;
    }
    sink.write_all(b"}\n")?;
    Ok(Outcome {
        metadata: None,
        error: outcome.error,
    })
}

fn failed(error: String) -> Outcome {
    Outcome {
        metadata: None,
        error: Some(error),
    }
}

fn finish(stream: &StreamReader, include_metadata: bool) -> Outcome {
    if !include_metadata {
        return Outcome::default();
    }
    match stream.metadata() {
        Ok(metadata) => Outcome {
            metadata: Some(metadata),
            error: None,
        },
        Err(error) => failed(error.to_string()),
    }
}

/// Copies the extracted text into the sink. The outer result fails when the sink does; the inner
/// one when extraction does.
fn copy(
    stream: &mut StreamReader,
    sink: &mut dyn Write,
    write: impl Fn(&[u8], &mut dyn Write) -> io::Result<()>,
) -> io::Result<Result<(), String>> {
    let mut buffer = vec![0; READ_BUFFER_SIZE];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(Ok(())),
            Ok(read) => write(&buffer[..read], sink)?,
            Err(error) => return Ok(Err(error.to_string())),
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
