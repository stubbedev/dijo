//! Jupyter notebooks, as our Java `NotebookParser` reads them: the cells in order, markdown, raw
//! and heading cells as paragraphs split at blank lines, code and the text of its outputs as
//! preformatted text without terminal colour codes. Covers nbformat 4 (cells with a source) and
//! nbformat 3 (worksheets of cells with an input). Jackson reads the notebook there; serde_json
//! reads it here, as Jackson does: the first JSON value, whatever follows ignored, the last of
//! duplicate keys kept.

use serde_json::Value;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::handlers::{is_blank, is_java_whitespace, utf16_length};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};

const PLAIN_TEXT: &str = "text/plain";
const MARKDOWN: &str = "text/markdown";
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
/// Jackson's `StreamReadConstraints`: the longest number, string and name it reads.
const MAX_NUMBER_LENGTH: usize = 1000;
const MAX_STRING_LENGTH: usize = 20_000_000;
const MAX_NAME_LENGTH: usize = 50_000;

/// What the port reads differently from Jackson, which Tika's path then reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unread(&'static str);

/// The notebook's JSON as Jackson's `readTree` reads it: `None` for what is not JSON.
fn read(input: &[u8]) -> Result<Option<Value>, Unread> {
    if input.starts_with(&[0xFE, 0xFF])
        || input.starts_with(&[0xFF, 0xFE])
        || input.iter().take(4).any(|&b| b == 0)
    {
        return Err(Unread("JSON in UTF-16 or UTF-32, which Jackson detects"));
    }
    let input = input.strip_prefix(UTF8_BOM).unwrap_or(input);
    if input
        .split(|b| !matches!(b, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'))
        .any(|run| run.len() > MAX_NUMBER_LENGTH)
    {
        return Err(Unread("a number longer than Jackson reads"));
    }
    match serde_json::Deserializer::from_slice(input)
        .into_iter::<Value>()
        .next()
    {
        Some(Ok(value)) if within_jackson_limits(&value) => Ok(Some(value)),
        Some(Ok(_)) => Err(Unread("a string or name longer than Jackson reads")),
        Some(Err(_)) => Err(Unread("JSON serde_json rejects, which Jackson may read")),
        None => Ok(None),
    }
}

/// Whether Jackson reads every string and name in `value`, none being longer than it allows.
fn within_jackson_limits(value: &Value) -> bool {
    match value {
        Value::String(text) => utf16_length(text) <= MAX_STRING_LENGTH,
        Value::Array(values) => values.iter().all(within_jackson_limits),
        Value::Object(members) => members.iter().all(|(name, value)| {
            utf16_length(name) <= MAX_NAME_LENGTH && within_jackson_limits(value)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
    }
}

/// `JsonNode.asText`: a string itself, a container nothing.
fn as_text(value: &Value) -> Result<String, Unread> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Bool(flag) => Ok(flag.to_string()),
        Value::Null => Ok("null".to_owned()),
        Value::Number(_) => Err(Unread(
            "numbers where text is expected, which Jackson writes",
        )),
        Value::Array(_) | Value::Object(_) => Ok(String::new()),
    }
}

/// `JsonNode.forEach` over an array; an object, whose values Jackson would iterate in its
/// order, is not read.
fn elements(value: Option<&Value>) -> Result<&[Value], Unread> {
    match value {
        Some(Value::Array(elements)) => Ok(elements),
        Some(Value::Object(_)) => Err(Unread("an object where an array is expected")),
        _ => Ok(&[]),
    }
}

/// `text`: a notebook string, stored whole or as an array of lines.
fn text(value: Option<&Value>) -> Result<String, Unread> {
    match value {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::Array(lines)) => lines.iter().map(as_text).collect(),
        Some(value) => as_text(value),
    }
}

/// `String.strip`, by `Character.isWhitespace`.
fn strip(text: &str) -> &str {
    text.trim_matches(is_java_whitespace)
}

/// `clean`: without the terminal codes `ESC [ digits-and-semicolons letter`, stripped.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\u{1B}[") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let parameters = after
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b';')
            .count();
        match after[parameters..].chars().next() {
            Some(letter) if letter.is_ascii_alphabetic() => {
                rest = &after[parameters + 1..];
            }
            _ => {
                out.push('\u{1B}');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    strip(&out).to_owned()
}

/// `split("\n\\s*\n")`: the text between line breaks with only whitespace between them, the
/// trailing empty pieces dropped.
fn paragraphs(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut pieces = Vec::new();
    let (mut start, mut at) = (0, 0);
    while at < bytes.len() {
        if bytes[at] == b'\n' {
            let run = bytes[at + 1..]
                .iter()
                .take_while(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\x0B' | b'\x0C' | b'\r'))
                .count();
            if let Some(last) = bytes[at + 1..at + 1 + run]
                .iter()
                .rposition(|&b| b == b'\n')
            {
                pieces.push(&text[start..at]);
                at += last + 2;
                start = at;
                continue;
            }
        }
        at += 1;
    }
    pieces.push(&text[start..]);
    while pieces.last().is_some_and(|piece| piece.is_empty()) {
        pieces.pop();
    }
    pieces
}

/// One block of the output: a paragraph or preformatted text.
#[derive(Debug, PartialEq, Eq)]
enum Block {
    Paragraph(String),
    Preformatted(String),
}

/// `outputText`: a stream, the plain or markdown form of a result, or an error.
fn output_text(output: &Value) -> Result<String, Unread> {
    if output.get("text").is_some() {
        return Ok(clean(&text(output.get("text"))?));
    }
    let data = output.get("data");
    if let Some(plain) = data.and_then(|data| data.get(PLAIN_TEXT)) {
        return Ok(clean(&text(Some(plain))?));
    }
    if let Some(markdown) = data.and_then(|data| data.get(MARKDOWN)) {
        return Ok(clean(&text(Some(markdown))?));
    }
    if output.get("traceback").is_some() {
        let lines: Vec<String> = elements(output.get("traceback"))?
            .iter()
            .map(as_text)
            .collect::<Result<_, _>>()?;
        return Ok(clean(&lines.join("\n")));
    }
    if output.get("ename").is_some() {
        let name = output
            .get("ename")
            .map(as_text)
            .transpose()?
            .unwrap_or_default();
        let value = output
            .get("evalue")
            .map(as_text)
            .transpose()?
            .unwrap_or_default();
        return Ok(clean(&format!("{name}: {value}")));
    }
    Ok(String::new())
}

/// The blocks of the notebook, cell by cell.
fn blocks(notebook: &Value) -> Result<Vec<Block>, Unread> {
    let mut cells: Vec<&Value> = elements(notebook.get("cells"))?.iter().collect();
    for worksheet in elements(notebook.get("worksheets"))? {
        cells.extend(elements(worksheet.get("cells"))?);
    }
    let mut blocks = Vec::new();
    for cell in cells {
        let kind = cell
            .get("cell_type")
            .map(as_text)
            .transpose()?
            .unwrap_or_default();
        let source = text(if cell.get("source").is_some() {
            cell.get("source")
        } else {
            cell.get("input")
        })?;
        if matches!(kind.as_str(), "markdown" | "raw" | "heading") {
            for paragraph in paragraphs(&source) {
                if !is_blank(paragraph) {
                    blocks.push(Block::Paragraph(strip(paragraph).to_owned()));
                }
            }
            continue;
        }
        if !is_blank(&source) {
            blocks.push(Block::Preformatted(source));
        }
        for output in elements(cell.get("outputs"))? {
            let text = output_text(output)?;
            if !is_blank(&text) {
                blocks.push(Block::Preformatted(text));
            }
        }
    }
    Ok(blocks)
}

/// The notebook's blocks, or the failure Tika's path reports before it starts the document.
fn notebook_blocks(input: &[u8]) -> Result<Result<Vec<Block>, Error>, Unread> {
    match read(input)? {
        Some(notebook @ Value::Object(_)) => blocks(&notebook).map(Ok),
        _ => Ok(Err(Error::ParseError(
            "The notebook is not a JSON object".to_owned(),
        ))),
    }
}

/// Our Java `NotebookParser`.
pub(crate) struct NotebookParser;

impl Parser for NotebookParser {
    fn name(&self) -> &'static str {
        "extractous::NotebookParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-ipynb+json"]
    }

    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<String> {
        notebook_blocks(input)
            .err()
            .map(|Unread(why)| why.to_owned())
    }

    fn fails_before_starting(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        _configs: &ParserConfigs,
    ) -> Option<Error> {
        notebook_blocks(input).ok().and_then(Result::err)
    }

    fn parse(
        &self,
        input: &[u8],
        _configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        let blocks = notebook_blocks(input)
            .map_err(|Unread(why)| Failure::Malformed(why.to_owned()))?
            .map_err(Failure::Extraction)?;
        for block in blocks {
            match block {
                Block::Paragraph(text) => xhtml.element("p", &text)?,
                Block::Preformatted(text) => xhtml.element("pre", &text)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_fails_as_tika, assert_same_as_tika, documents, Scratch};

    const TIKA_NAME: &str = "ai.yobix.NotebookParser";

    #[test]
    fn splits_and_cleans_as_java_does() {
        assert_eq!(paragraphs("a\n \n\nb\n\n"), ["a", "b"]);
        assert_eq!(paragraphs("\n\na"), ["", "a"]);
        assert_eq!(
            clean("\u{1B}[0;31mred\u{1B}[0m \u{1B}[1 \u{1B}[x"),
            "red \u{1B}[1"
        );
    }

    #[test]
    fn defers_what_jackson_reads_differently() {
        let long_number = format!(r#"{{"nbformat": {}}}"#, "1".repeat(MAX_NUMBER_LENGTH + 1));
        let long_name = format!(r#"{{"{}": 1}}"#, "n".repeat(MAX_NAME_LENGTH + 1));
        for input in [
            br#"{"cells": [{"cell_type": "code", "source": ["x = ", 1]}]}"#.as_slice(),
            br#"{"cells": [{"cell_type": "code", "source": "\ud800"}]}"#,
            br#"{"cells": {"a": {"cell_type": "code", "source": "x"}}}"#,
            b"\xFF\xFE{\0}\0",
            long_number.as_bytes(),
            long_name.as_bytes(),
        ] {
            assert!(notebook_blocks(input).is_err(), "{input:?}");
        }
        assert!(matches!(
            notebook_blocks(b" \n"),
            Ok(Err(Error::ParseError(_)))
        ));
    }

    #[test]
    fn extracts_notebooks_as_tikas_path_does() {
        let scratch = Scratch::new("notebooks");
        let v4 = r##"{"cells": [
            {"cell_type": "markdown", "source": ["# Title\n", "\n", "Some *text*.\n  \n  More."]},
            {"cell_type": "code", "source": "print(1)", "outputs": [
                {"output_type": "stream", "text": ["\u001b[31m1\u001b[0m\n"]},
                {"output_type": "execute_result", "data": {"text/plain": ["2"], "text/html": "<b>2</b>"}},
                {"output_type": "error", "ename": "ValueError", "evalue": "bad", "traceback": ["\u001b[1mTrace\u001b[0m", "line"]}
            ]},
            {"cell_type": "raw", "source": null},
            {"cell_type": "code", "source": "   "}
        ], "metadata": {}, "nbformat": 4}"##;
        let v3 = r#"{"worksheets": [{"cells": [
            {"cell_type": "heading", "source": "Heading", "level": 1},
            {"cell_type": "code", "input": ["x = 1\n", "x"], "outputs": [{"output_type": "pyout", "text": "1"}]}
        ]}], "nbformat": 3} trailing"#;
        let written = [
            scratch.write("v4.ipynb", v4.as_bytes()),
            scratch.write("v3.ipynb", v3.as_bytes()),
        ];
        let not_object = scratch.write("array.ipynb", b"[1, 2]");
        let (parsed, deferred) = documents(&NotebookParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 3);
        let paths: Vec<&Path> = written.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
        assert_fails_as_tika(&[&not_object]);
    }

    #[test]
    fn extracts_the_corpus_notebook_as_tikas_path_does() {
        let (parsed, deferred) =
            documents(&NotebookParser, &[Path::new("../test_files/documents")]);
        assert!(deferred.is_empty());
        assert_eq!(parsed.len(), 1);
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }
}
