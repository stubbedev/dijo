//! Jupyter notebooks, as our Java `NotebookParser` reads them: the cells in order, markdown, raw
//! and heading cells as paragraphs split at blank lines, code and the text of its outputs as
//! preformatted text without terminal colour codes. Covers nbformat 4 (cells with a source) and
//! nbformat 3 (worksheets of cells with an input). Jackson reads the notebook there; serde_json
//! reads it here, as Jackson does: the first JSON value, whatever follows ignored, the last of
//! duplicate keys kept.
//!
//! Jackson reads the whole tree before the parser writes, and a later key replaces an earlier,
//! so the notebook is read twice in place, holding no more than one cell: once to find that
//! Jackson reads it and that each cell reads, and again to write the cells one at a time, each
//! object's members taken as raw JSON, the last of each name, and only those the parser reads
//! parsed.

use std::convert::Infallible;
use std::fmt;

use serde::de::{self, Deserializer as _, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::Value;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::java::is_blank;
use crate::java::is_regex_space;
use crate::java::strip;

const PLAIN_TEXT: &str = "text/plain";
const MARKDOWN: &str = "text/markdown";
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
/// Jackson's `StreamReadConstraints`: the longest number, string and name it reads.
const MAX_NUMBER_LENGTH: usize = 1000;
const MAX_STRING_LENGTH: usize = 20_000_000;
const MAX_NAME_LENGTH: usize = 50_000;
/// What the port reads differently from Jackson where serde_json rejects the notebook.
const REJECTED: Unread = Unread("JSON serde_json rejects, which Jackson may read");

/// What the port reads differently from Jackson, which it then does not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Unread(&'static str);

/// The notebook's first JSON value, raw, once Jackson's `readTree` would read it: `None` for
/// what is not JSON.
fn read(input: &[u8]) -> Result<Option<&RawValue>, Unread> {
    if input.starts_with(&[0xFE, 0xFF])
        || input.starts_with(&[0xFF, 0xFE])
        || input.iter().take(4).any(|&b| b == 0)
    {
        return Err(Unread("JSON in UTF-16 or UTF-32, which Jackson detects"));
    }
    let input = input.strip_prefix(UTF8_BOM).unwrap_or(input);
    within_jackson_limits(input)?;
    match serde_json::Deserializer::from_slice(input)
        .into_iter::<&RawValue>()
        .next()
    {
        Some(Ok(value)) => Ok(Some(value)),
        Some(Err(_)) => Err(REJECTED),
        None => Ok(None),
    }
}

/// Whether Jackson reads the numbers, strings and names of the first JSON value of `input`,
/// none being longer than it allows, scanned up to the end of that value.
fn within_jackson_limits(input: &[u8]) -> Result<(), Unread> {
    let mut depth = 0usize;
    let mut number = 0usize;
    let mut at = 0;
    while let Some(&byte) = input.get(at) {
        at += 1;
        if matches!(byte, b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
            number += 1;
            if number > MAX_NUMBER_LENGTH {
                return Err(Unread("a number longer than Jackson reads"));
            }
            continue;
        }
        let after_scalar = number > 0 && depth == 0;
        number = 0;
        match byte {
            b'"' => {
                let units;
                (units, at) = string_units(input, at);
                let name = input[at..]
                    .iter()
                    .find(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
                    == Some(&b':');
                let (limit, what) = if name {
                    (MAX_NAME_LENGTH, "a name longer than Jackson reads")
                } else {
                    (MAX_STRING_LENGTH, "a string longer than Jackson reads")
                };
                if units > limit {
                    return Err(Unread(what));
                }
                if depth == 0 {
                    return Ok(());
                }
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(());
                }
            }
            _ if after_scalar => return Ok(()),
            _ => {}
        }
    }
    Ok(())
}

/// The UTF-16 units of the JSON string whose text starts at `start` of `input`, and where the
/// string ends, past its closing quote.
fn string_units(input: &[u8], start: usize) -> (usize, usize) {
    let mut units = 0;
    let mut at = start;
    while let Some(&byte) = input.get(at) {
        at += 1;
        match byte {
            b'"' => return (units, at),
            b'\\' => {
                units += 1;
                at += if input.get(at) == Some(&b'u') { 5 } else { 1 };
            }
            0x80..=0xBF => {}
            0xF0..=0xFF => units += 2,
            _ => units += 1,
        }
    }
    (units, at)
}

/// Why a reading of the notebook stops: serde_json rejects what it reads, or what writes the
/// blocks fails.
enum Halt<E> {
    Rejected,
    Written(E),
}

impl<E> From<serde_json::Error> for Halt<E> {
    fn from(_: serde_json::Error) -> Self {
        Halt::Rejected
    }
}

/// The members of the JSON object `raw` named `names`, as raw JSON, each the last of its name,
/// as Jackson's `ObjectNode` keeps them; none of what is not an object.
fn members<'a, const N: usize>(
    raw: Option<&'a RawValue>,
    names: [&str; N],
) -> Result<[Option<&'a RawValue>; N], serde_json::Error> {
    struct Members<'n, 'f, 'a, const N: usize> {
        names: &'n [&'n str; N],
        found: &'f mut [Option<&'a RawValue>; N],
    }

    impl<'de, const N: usize> Visitor<'de> for Members<'_, '_, 'de, N> {
        type Value = ();

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a JSON object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            while let Some(name) = map.next_key::<String>()? {
                let value: &'de RawValue = map.next_value()?;
                if let Some(at) = self.names.iter().position(|wanted| *wanted == name) {
                    self.found[at] = Some(value);
                }
            }
            Ok(())
        }
    }

    let mut found = [None; N];
    let Some(raw) = raw.filter(|raw| raw.get().trim_start().starts_with('{')) else {
        return Ok(found);
    };
    serde_json::Deserializer::from_str(raw.get()).deserialize_map(Members {
        names: &names,
        found: &mut found,
    })?;
    Ok(found)
}

/// Hands `each` the elements of the JSON array `raw`, as raw JSON, one at a time; none of what
/// is not an array, an object included. Our Java parser iterates an object's values as though
/// they were an array's (#105).
fn each_element<'a, E>(
    raw: Option<&'a RawValue>,
    each: &mut dyn FnMut(&'a RawValue) -> Result<(), Halt<E>>,
) -> Result<(), Halt<E>> {
    struct Elements<'e, 'a, E> {
        each: &'e mut dyn FnMut(&'a RawValue) -> Result<(), Halt<E>>,
        halted: &'e mut Option<Halt<E>>,
    }

    impl<'de, E> Visitor<'de> for Elements<'_, 'de, E> {
        type Value = ();

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a JSON array")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            while let Some(element) = seq.next_element::<&'de RawValue>()? {
                if let Err(halt) = (self.each)(element) {
                    *self.halted = Some(halt);
                    return Err(de::Error::custom("the reading of the notebook halted"));
                }
            }
            Ok(())
        }
    }

    let Some(raw) = raw.filter(|raw| raw.get().trim_start().starts_with('[')) else {
        return Ok(());
    };
    let mut halted = None;
    let read = serde_json::Deserializer::from_str(raw.get()).deserialize_seq(Elements {
        each,
        halted: &mut halted,
    });
    match (halted, read) {
        (Some(halt), _) => Err(halt),
        (None, read) => Ok(read?),
    }
}

/// The value of raw JSON, if there is any.
fn value(raw: Option<&RawValue>) -> Result<Option<Value>, serde_json::Error> {
    raw.map(|raw| serde_json::from_str(raw.get())).transpose()
}

/// `JsonNode.asText`: a string itself, a number in its shortest form, a container nothing.
/// Our Java parser writes numbers as Jackson formats them, `1.0E10` for `1e10` (#105).
fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "null".to_owned(),
        Value::Number(number) => number.to_string(),
        Value::Array(_) | Value::Object(_) => String::new(),
    }
}

/// `text`: a notebook string, stored whole or as an array of lines.
fn text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::Array(lines)) => lines.iter().map(as_text).collect(),
        Some(value) => as_text(value),
    }
}

/// The text of the raw JSON `raw`, as [`text`] reads it.
fn raw_text(raw: Option<&RawValue>) -> Result<String, serde_json::Error> {
    Ok(text(value(raw)?.as_ref()))
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
                .take_while(|&&byte| is_regex_space(char::from(byte)))
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
fn output_text(output: &RawValue) -> Result<String, serde_json::Error> {
    let [text, data, traceback, ename, evalue] = members(
        Some(output),
        ["text", "data", "traceback", "ename", "evalue"],
    )?;
    if text.is_some() {
        return Ok(clean(&raw_text(text)?));
    }
    let [plain, markdown] = members(data, [PLAIN_TEXT, MARKDOWN])?;
    if plain.is_some() {
        return Ok(clean(&raw_text(plain)?));
    }
    if markdown.is_some() {
        return Ok(clean(&raw_text(markdown)?));
    }
    if let Some(traceback) = value(traceback)? {
        let lines: Vec<String> = match &traceback {
            Value::Array(lines) => lines.iter().map(as_text).collect(),
            _ => Vec::new(),
        };
        return Ok(clean(&lines.join("\n")));
    }
    if ename.is_some() {
        let name = value(ename)?.as_ref().map(as_text).unwrap_or_default();
        let value = value(evalue)?.as_ref().map(as_text).unwrap_or_default();
        return Ok(clean(&format!("{name}: {value}")));
    }
    Ok(String::new())
}

/// Hands `each` the blocks of `cell`.
fn cell_blocks<E>(
    cell: &RawValue,
    each: &mut impl FnMut(Block) -> Result<(), E>,
) -> Result<(), Halt<E>> {
    let [kind, source, input, outputs] =
        members(Some(cell), ["cell_type", "source", "input", "outputs"])?;
    let kind = value(kind)?.as_ref().map(as_text).unwrap_or_default();
    let source = raw_text(if source.is_some() { source } else { input })?;
    let mut write = |block| each(block).map_err(Halt::Written);
    if matches!(kind.as_str(), "markdown" | "raw" | "heading") {
        for paragraph in paragraphs(&source) {
            if !is_blank(paragraph) {
                write(Block::Paragraph(strip(paragraph).to_owned()))?;
            }
        }
        return Ok(());
    }
    if !is_blank(&source) {
        write(Block::Preformatted(source))?;
    }
    each_element(outputs, &mut |output| {
        let text = output_text(output)?;
        if !is_blank(&text) {
            write(Block::Preformatted(text))?;
        }
        Ok(())
    })
}

/// The members of a notebook its parser reads: its cells, and its worksheets of cells.
struct Notebook<'a> {
    cells: Option<&'a RawValue>,
    worksheets: Option<&'a RawValue>,
}

impl<'a> Notebook<'a> {
    /// The notebook `input` holds, or the failure Tika reports before it starts the
    /// document, once Jackson would read it and each of its cells reads.
    fn read(input: &'a [u8]) -> Result<Result<Self, Error>, Unread> {
        let not_an_object = || Error::ParseError("The notebook is not a JSON object".to_owned());
        let Some(raw) = read(input)?.filter(|raw| raw.get().trim_start().starts_with('{')) else {
            return Ok(Err(not_an_object()));
        };
        let [cells, worksheets] =
            members(Some(raw), ["cells", "worksheets"]).map_err(|_| REJECTED)?;
        let notebook = Self { cells, worksheets };
        match notebook.each_block(&mut |_| Ok::<_, Infallible>(())) {
            Ok(()) => Ok(Ok(notebook)),
            Err(Halt::Rejected) => Err(REJECTED),
            Err(Halt::Written(never)) => match never {},
        }
    }

    /// Hands `each` the blocks of the notebook, cell by cell: those of its cells, then those of
    /// its worksheets'.
    fn each_block<E>(&self, each: &mut impl FnMut(Block) -> Result<(), E>) -> Result<(), Halt<E>> {
        each_element(self.cells, &mut |cell| cell_blocks(cell, each))?;
        each_element(self.worksheets, &mut |worksheet| {
            let [cells] = members(Some(worksheet), ["cells"])?;
            each_element(cells, &mut |cell| cell_blocks(cell, each))
        })
    }
}

/// Our Java `NotebookParser`.
pub struct NotebookParser;

impl Parser for NotebookParser {
    fn name(&self) -> &'static str {
        "dijo::NotebookParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::NotebookParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-ipynb+json"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let notebook = Notebook::read(input)
            .map_err(|Unread(why)| Decline::Unread(why.to_owned()))?
            .map_err(Decline::Fails)?;
        Ok(Box::new(move |xhtml| {
            let written = notebook.each_block(&mut |block| match block {
                Block::Paragraph(text) => xhtml.element("p", &text),
                Block::Preformatted(text) => xhtml.element("pre", &text),
            });
            match written {
                Ok(()) => Ok(()),
                Err(Halt::Written(stop)) => Err(stop.into()),
                Err(Halt::Rejected) => Err(Failure::Malformed(
                    "the notebook is read otherwise the second time".to_owned(),
                )),
            }
        }))
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    /// The notebook's blocks, or the failure Tika reports before it starts the document.
    fn notebook_blocks(input: &[u8]) -> Result<Result<Vec<Block>, Error>, Unread> {
        Ok(Notebook::read(input)?.map(|notebook| {
            let mut blocks = Vec::new();
            let read = notebook.each_block(&mut |block| {
                blocks.push(block);
                Ok::<_, Infallible>(())
            });
            assert!(read.is_ok());
            blocks
        }))
    }

    #[test]
    fn keeps_the_last_of_duplicate_keys() {
        let input = br#"{"cells": [{"cell_type": "code", "source": "first"}],
            "cells": [{"cell_type": "code", "source": "a", "source": "b",
                "outputs": [{"data": {"text/plain": "x", "text/plain": "y"}}]}]}"#;
        assert_eq!(
            notebook_blocks(input).unwrap().unwrap(),
            [
                Block::Preformatted("b".to_owned()),
                Block::Preformatted("y".to_owned())
            ]
        );
    }

    /// A notebook too large to read whole, of more cells than an index of them fits the
    /// budget: each is written as it is read, to the last.
    #[test]
    fn writes_notebooks_too_large_to_read_whole_a_cell_at_a_time() {
        use dijo_core::large::{bodies_past_limit, LargeFile};

        let cell = |index: u64| {
            format!(r#"{{"cell_type": "markdown", "source": ["Linje {index}"]}},"#).into_bytes()
        };
        let count = bodies_past_limit(cell(0).len());
        let file = LargeFile::new(
            "large.ipynb",
            br#"{"nbformat": 4, "cells": ["#,
            count,
            cell,
            br#"{"cell_type": "code", "source": "the end"}], "metadata": {}}"#,
        );
        let mut lines = 0u64;
        let mut last = String::new();
        file.extract_lines(NotebookParser.name(), |line| {
            if line.starts_with("Linje") {
                assert_eq!(line, format!("Linje {lines}"));
                lines += 1;
            }
            if !line.trim().is_empty() {
                last = line.trim().to_owned();
            }
        });
        assert_eq!(lines, count);
        assert_eq!(last, "the end");
    }

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
    fn falls_short_on_what_jackson_reads_differently() {
        let long_number = format!(r#"{{"nbformat": {}}}"#, "1".repeat(MAX_NUMBER_LENGTH + 1));
        let long_name = format!(r#"{{"{}": 1}}"#, "n".repeat(MAX_NAME_LENGTH + 1));
        for input in [
            br#"{"cells": [{"cell_type": "code", "source": "\ud800"}]}"#.as_slice(),
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
    fn reads_numbers_as_written_and_only_arrays_as_lists() {
        // Our Java parser writes numbers as Jackson formats them and iterates an object as
        // though it were an array (#105).
        let blocks = |input: &[u8]| notebook_blocks(input).unwrap().unwrap();
        assert_eq!(
            blocks(br#"{"cells": [{"cell_type": "code", "source": ["x = ", 1e10, " + ", 2]}]}"#),
            [Block::Preformatted("x = 10000000000.0 + 2".to_owned())]
        );
        assert_eq!(
            blocks(br#"{"cells": {"a": {"cell_type": "code", "source": "x"}}}"#),
            []
        );
    }
}
