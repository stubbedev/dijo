//! The content handlers extractous chains behind a parser, ported from Tika's and our own Java:
//! [`Body`] (`BodyContentHandler`), [`AnchoredBlocks`] (`AnchoredBlockHandler`), [`BlockBreaks`]
//! (`BlockBreakHandler`), [`WellFormed`] (`WellFormedContentHandler`), [`WriteLimit`]
//! (`WriteOutContentHandler`'s limit), and the writers [`ToText`] (`ToTextContentHandler`) and
//! [`ToXml`] (`ToXMLContentHandler`).

use crate::formats::xhtml::{ContentHandler, Stop, XHTML_NAMESPACE};

/// The block elements [`BlockBreaks`] ends with a line break and [`AnchoredBlocks`] holds back.
const BLOCKS: [&str; 16] = [
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "div",
    "tr",
    "table",
    "blockquote",
    "pre",
    "dt",
    "dd",
    "title",
];
const CELLS: [&str; 2] = ["td", "th"];
const PARAGRAPH: &str = "p";
const MAX_HELD_CHARACTERS: usize = 1 << 20;

/// Where written text goes: the encoded output of a [`ToText`] or [`ToXml`].
pub(crate) trait Write {
    fn write(&mut self, text: &str) -> Result<(), Stop>;
    fn finish(&mut self) -> Result<(), Stop>;
}

impl<W: Write + ?Sized> Write for &mut W {
    fn write(&mut self, text: &str) -> Result<(), Stop> {
        (**self).write(text)
    }

    fn finish(&mut self) -> Result<(), Stop> {
        (**self).finish()
    }
}

/// Passes on only what lies inside `html/body`, as Tika's `BodyContentHandler`.
pub(crate) struct Body<H> {
    next: H,
    open: Vec<bool>,
}

impl<H: ContentHandler> Body<H> {
    pub(crate) fn new(next: H) -> Self {
        Self {
            next,
            open: Vec::new(),
        }
    }

    fn in_body(&self) -> bool {
        self.open.len() >= 2 && self.open[0] && self.open[1]
    }
}

impl<H: ContentHandler> ContentHandler for Body<H> {
    fn start_document(&mut self) -> Result<(), Stop> {
        self.next.start_document()
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        let inside = self.in_body();
        let on_path = match self.open.len() {
            0 => name == "html",
            1 => self.open[0] && name == "body",
            _ => inside,
        };
        self.open.push(on_path);
        if inside {
            self.next.start_element(name, attributes)?;
        }
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        self.open.pop();
        if self.in_body() {
            self.next.end_element(name)?;
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        if self.in_body() {
            self.next.characters(text)?;
        }
        Ok(())
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        if self.in_body() {
            self.next.ignorable_whitespace(text)?;
        }
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        self.next.end_document()
    }
}

#[derive(Debug, Clone)]
enum Held {
    Start(String, Vec<(String, String)>),
    End(String),
    Characters(String),
    IgnorableWhitespace(String),
}

/// Writes a block opened in the middle of a paragraph after that paragraph, as our Java
/// `AnchoredBlockHandler`: OpenDocument comments and Word footnote references arrive inside the
/// paragraph they are anchored in and would otherwise cut its sentence in two.
pub(crate) struct AnchoredBlocks<H> {
    next: H,
    held: Vec<Held>,
    held_characters: usize,
    depth: usize,
    paragraph_depth: usize,
    paragraph_has_text: bool,
    holding_depth: usize,
    after_held: bool,
}

impl<H: ContentHandler> AnchoredBlocks<H> {
    pub(crate) fn new(next: H) -> Self {
        Self {
            next,
            held: Vec::new(),
            held_characters: 0,
            depth: 0,
            paragraph_depth: 0,
            paragraph_has_text: false,
            holding_depth: 0,
            after_held: false,
        }
    }

    fn release(&mut self) -> Result<(), Stop> {
        for event in std::mem::take(&mut self.held) {
            match event {
                Held::Start(name, attributes) => {
                    let borrowed: Vec<(&str, &str)> = attributes
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str()))
                        .collect();
                    self.next.start_element(&name, &borrowed)?;
                }
                Held::End(name) => self.next.end_element(&name)?,
                Held::Characters(text) => self.next.characters(&text)?,
                Held::IgnorableWhitespace(text) => self.next.ignorable_whitespace(&text)?,
            }
        }
        self.held_characters = 0;
        Ok(())
    }
}

impl<H: ContentHandler> ContentHandler for AnchoredBlocks<H> {
    fn start_document(&mut self) -> Result<(), Stop> {
        self.next.start_document()
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        self.depth += 1;
        self.after_held = false;
        if self.holding_depth == 0
            && self.paragraph_depth != 0
            && self.paragraph_has_text
            && BLOCKS.contains(&name)
        {
            self.holding_depth = self.depth;
        }
        if self.holding_depth != 0 {
            let owned = attributes
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect();
            self.held.push(Held::Start(name.to_owned(), owned));
            return Ok(());
        }
        self.next.start_element(name, attributes)?;
        if self.paragraph_depth == 0 && name == PARAGRAPH {
            self.paragraph_depth = self.depth;
            self.paragraph_has_text = false;
        }
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        if self.holding_depth != 0 {
            self.held.push(Held::End(name.to_owned()));
            if self.depth == self.holding_depth {
                self.holding_depth = 0;
                self.after_held = true;
            }
        } else {
            self.after_held = false;
            self.next.end_element(name)?;
            if self.depth == self.paragraph_depth {
                self.paragraph_depth = 0;
                self.release()?;
            }
        }
        self.depth -= 1;
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        let blank = is_blank(text);
        self.after_held &= blank;
        if self.holding_depth == 0 && !self.after_held {
            self.next.characters(text)?;
            self.paragraph_has_text |= self.paragraph_depth != 0 && !blank;
            return Ok(());
        }
        self.held.push(Held::Characters(text.to_owned()));
        self.held_characters += utf16_length(text);
        if self.held_characters > MAX_HELD_CHARACTERS {
            self.release()?;
            self.holding_depth = 0;
            self.after_held = false;
        }
        Ok(())
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        if self.holding_depth == 0 && !self.after_held {
            return self.next.ignorable_whitespace(text);
        }
        self.held.push(Held::IgnorableWhitespace(text.to_owned()));
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        self.release()?;
        self.next.end_document()
    }
}

#[derive(Debug, Default)]
struct Cell {
    has_text: bool,
    line_ended: bool,
}

/// Ends every block element with a line break in plain text unless the text already ends with
/// one, and within a table cell puts line breaks only between the cell's paragraphs, as our Java
/// `BlockBreakHandler`.
pub(crate) struct BlockBreaks<H> {
    next: H,
    cells: Vec<Cell>,
    at_line_start: bool,
}

impl<H: ContentHandler> BlockBreaks<H> {
    pub(crate) fn new(next: H) -> Self {
        Self {
            next,
            cells: Vec::new(),
            at_line_start: true,
        }
    }

    fn end_cell_line(&mut self) {
        if let Some(cell) = self.cells.last_mut() {
            cell.line_ended |= cell.has_text;
        }
    }

    fn keeps_in_cell(&mut self, text: &str) -> Result<bool, Stop> {
        let Some(cell) = self.cells.last_mut() else {
            return Ok(true);
        };
        let blank = is_blank(text);
        if blank && (!cell.has_text || cell.line_ended) {
            return Ok(false);
        }
        let line_ended = cell.line_ended;
        cell.line_ended = false;
        cell.has_text = true;
        if line_ended {
            self.next.characters("\n")?;
            self.at_line_start = true;
        }
        Ok(true)
    }

    fn track(&mut self, text: &str) {
        if let Some(last) = text.chars().next_back() {
            self.at_line_start = last == '\n';
        }
    }
}

impl<H: ContentHandler> ContentHandler for BlockBreaks<H> {
    fn start_document(&mut self) -> Result<(), Stop> {
        self.next.start_document()
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        self.next.start_element(name, attributes)?;
        if CELLS.contains(&name) {
            self.cells.push(Cell::default());
        } else if BLOCKS.contains(&name) {
            self.end_cell_line();
        }
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        self.next.end_element(name)?;
        if CELLS.contains(&name) {
            self.cells.pop();
            return Ok(());
        }
        if !BLOCKS.contains(&name) {
            return Ok(());
        }
        if !self.cells.is_empty() {
            self.end_cell_line();
            return Ok(());
        }
        if !self.at_line_start {
            self.next.characters("\n")?;
            self.at_line_start = true;
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        if self.keeps_in_cell(text)? {
            self.track(text);
            self.next.characters(text)?;
        }
        Ok(())
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        if self.keeps_in_cell(text)? {
            self.track(text);
            self.next.ignorable_whitespace(text)?;
        }
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        self.next.end_document()
    }
}

/// Keeps the elements passed on balanced, as our Java `WellFormedContentHandler`: an end tag
/// without an open element of its name is dropped, one that skips open elements closes them
/// first, and the elements still open at the end of the document are closed.
pub(crate) struct WellFormed<H> {
    next: H,
    open: Vec<String>,
}

impl<H: ContentHandler> WellFormed<H> {
    pub(crate) fn new(next: H) -> Self {
        Self {
            next,
            open: Vec::new(),
        }
    }
}

impl<H: ContentHandler> ContentHandler for WellFormed<H> {
    fn start_document(&mut self) -> Result<(), Stop> {
        self.next.start_document()
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        self.next.start_element(name, attributes)?;
        self.open.push(name.to_owned());
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        if !self.open.iter().any(|open| open == name) {
            return Ok(());
        }
        while let Some(open) = self.open.pop() {
            self.next.end_element(&open)?;
            if open == name {
                break;
            }
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        self.next.characters(text)
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        self.next.ignorable_whitespace(text)
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        while let Some(open) = self.open.pop() {
            self.next.end_element(&open)?;
        }
        self.next.end_document()
    }
}

/// Passes on at most `limit` UTF-16 code units of text, as the limit of Tika's
/// `WriteOutContentHandler`: the text that reaches the limit is cut there, and the parse stops
/// with [`Stop::LimitReached`]. A cut never splits a character, so a character that would
/// straddle the limit is left out.
pub(crate) struct WriteLimit<H> {
    next: H,
    remaining: usize,
}

impl<H: ContentHandler> WriteLimit<H> {
    pub(crate) fn new(next: H, limit: usize) -> Self {
        Self {
            next,
            remaining: limit,
        }
    }

    fn take<'t>(&mut self, text: &'t str) -> (&'t str, bool) {
        let length = utf16_length(text);
        if length <= self.remaining {
            self.remaining -= length;
            return (text, false);
        }
        let mut units = 0;
        let mut end = 0;
        for (index, character) in text.char_indices() {
            if units + character.len_utf16() > self.remaining {
                break;
            }
            units += character.len_utf16();
            end = index + character.len_utf8();
        }
        self.remaining = 0;
        (&text[..end], true)
    }
}

impl<H: ContentHandler> ContentHandler for WriteLimit<H> {
    fn start_document(&mut self) -> Result<(), Stop> {
        self.next.start_document()
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        self.next.start_element(name, attributes)
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        self.next.end_element(name)
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        let (kept, reached) = self.take(text);
        self.next.characters(kept)?;
        if reached {
            return Err(Stop::LimitReached);
        }
        Ok(())
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        let (kept, reached) = self.take(text);
        self.next.ignorable_whitespace(kept)?;
        if reached {
            return Err(Stop::LimitReached);
        }
        Ok(())
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        self.next.end_document()
    }
}

/// Writes the text and ignorable whitespace, except inside `style` and `script`, as Tika's
/// `ToTextContentHandler`.
pub(crate) struct ToText<W> {
    out: W,
    hidden_depth: usize,
}

impl<W: Write> ToText<W> {
    pub(crate) fn new(out: W) -> Self {
        Self {
            out,
            hidden_depth: 0,
        }
    }
}

fn is_hidden(name: &str) -> bool {
    name.eq_ignore_ascii_case("style") || name.eq_ignore_ascii_case("script")
}

impl<W: Write> ContentHandler for ToText<W> {
    fn start_document(&mut self) -> Result<(), Stop> {
        Ok(())
    }

    fn start_element(&mut self, name: &str, _attributes: &[(&str, &str)]) -> Result<(), Stop> {
        if is_hidden(name) {
            self.hidden_depth += 1;
        }
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        if is_hidden(name) {
            self.hidden_depth = self.hidden_depth.saturating_sub(1);
        }
        Ok(())
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        if self.hidden_depth != 0 {
            return Ok(());
        }
        self.out.write(text)
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        self.characters(text)
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        self.out.finish()
    }
}

/// Serialises the events as XML, as Tika's `ToXMLContentHandler`: an empty element is written
/// `<name />`, and the first element after the document starts declares the XHTML namespace
/// after its attributes.
pub(crate) struct ToXml<W> {
    out: W,
    prolog: Option<&'static str>,
    in_start_element: bool,
    namespace_pending: bool,
}

impl<W: Write> ToXml<W> {
    /// A writer whose document starts with an XML declaration naming `encoding`, when given.
    pub(crate) fn new(out: W, encoding: Option<&'static str>) -> Self {
        Self {
            out,
            prolog: encoding,
            in_start_element: false,
            namespace_pending: false,
        }
    }

    fn close_start_element(&mut self) -> Result<(), Stop> {
        if self.in_start_element {
            self.in_start_element = false;
            self.out.write(">")?;
        }
        Ok(())
    }

    fn write_escaped(&mut self, text: &str, attribute: bool) -> Result<(), Stop> {
        let mut start = 0;
        for (index, character) in text.char_indices() {
            let entity = match character {
                '<' => "&lt;",
                '>' => "&gt;",
                '&' => "&amp;",
                '"' if attribute => "&quot;",
                _ => continue,
            };
            self.out.write(&text[start..index])?;
            self.out.write(entity)?;
            start = index + 1;
        }
        self.out.write(&text[start..])
    }
}

impl<W: Write> ContentHandler for ToXml<W> {
    fn start_document(&mut self) -> Result<(), Stop> {
        self.namespace_pending = true;
        if let Some(encoding) = self.prolog {
            self.out.write("<?xml version=\"1.0\" encoding=\"")?;
            self.out.write(encoding)?;
            self.out.write("\"?>\n")?;
        }
        Ok(())
    }

    fn start_element(&mut self, name: &str, attributes: &[(&str, &str)]) -> Result<(), Stop> {
        self.close_start_element()?;
        self.out.write("<")?;
        self.out.write(name)?;
        for (attribute, value) in attributes {
            self.out.write(" ")?;
            self.out.write(attribute)?;
            self.out.write("=\"")?;
            self.write_escaped(value, true)?;
            self.out.write("\"")?;
        }
        if self.namespace_pending {
            self.namespace_pending = false;
            self.out.write(" xmlns=\"")?;
            self.out.write(XHTML_NAMESPACE)?;
            self.out.write("\"")?;
        }
        self.in_start_element = true;
        Ok(())
    }

    fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        if self.in_start_element {
            self.in_start_element = false;
            return self.out.write(" />");
        }
        self.out.write("</")?;
        self.out.write(name)?;
        self.out.write(">")
    }

    fn characters(&mut self, text: &str) -> Result<(), Stop> {
        self.close_start_element()?;
        self.write_escaped(text, false)
    }

    fn ignorable_whitespace(&mut self, text: &str) -> Result<(), Stop> {
        self.characters(text)
    }

    fn end_document(&mut self) -> Result<(), Stop> {
        self.out.finish()
    }
}

/// Java's `String.isBlank`: empty or only whitespace as `Character.isWhitespace` defines it.
pub(crate) fn is_blank(text: &str) -> bool {
    text.chars().all(is_java_whitespace)
}

/// Java's `Character.isWhitespace`.
pub(crate) fn is_java_whitespace(character: char) -> bool {
    matches!(character, '\t'..='\r' | '\u{1c}'..='\u{1f}')
        || (character.is_whitespace()
            && !matches!(character, '\u{a0}' | '\u{2007}' | '\u{202f}' | '\u{85}'))
}

/// The length of `text` in UTF-16 code units, which is how Java counts characters.
pub(crate) fn utf16_length(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

#[cfg(test)]
mod tests {
    use crate::config::ParserConfigs;
    use crate::formats::metadata::TikaMetadata;
    use crate::formats::xhtml::{Stop, Xhtml};
    use crate::formats::{Chain, Output, StringOut};

    const SCRIPTS: &str = include_str!("../../tests/fixtures/handlers/scripts.txt");
    const CHAINS: &str = include_str!("../../tests/fixtures/handlers/chains.tsv");
    const LARGE_LIMIT: usize = 1_000_000;
    const SMALL_LIMIT: usize = 40;

    fn unescape(field: &str) -> String {
        let mut out = String::new();
        let mut characters = field.chars();
        while let Some(character) = characters.next() {
            if character != '\\' {
                out.push(character);
                continue;
            }
            match characters.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(width @ ('u' | 'U')) => {
                    let digits: String = characters
                        .by_ref()
                        .take(if width == 'u' { 4 } else { 8 })
                        .collect();
                    let code = u32::from_str_radix(&digits, 16).unwrap();
                    out.push(char::from_u32(code).unwrap());
                }
                other => panic!("Unknown escape {other:?} in {field:?}"),
            }
        }
        out
    }

    fn scripts() -> Vec<(String, Vec<Vec<String>>)> {
        let mut scripts: Vec<(String, Vec<Vec<String>>)> = Vec::new();
        for line in SCRIPTS.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix("== ") {
                scripts.push((name.to_owned(), Vec::new()));
                continue;
            }
            let call = line.split('\t').map(unescape).collect();
            scripts.last_mut().unwrap().1.push(call);
        }
        scripts
    }

    fn play(script: &[Vec<String>], chain: Chain) -> (String, bool) {
        let mut metadata = TikaMetadata::new();
        let calls: Vec<&Vec<String>> = script
            .iter()
            .filter(|call| {
                let meta = call[0] == "meta";
                if meta {
                    metadata.add(&call[1], call[2].clone());
                }
                !meta
            })
            .collect();
        let (first, last) = (calls.first().unwrap(), calls.last().unwrap());
        assert!(first[0] == "start-document" && last[0] == "end-document");
        let body = &calls[1..calls.len() - 1];
        let configs = ParserConfigs::default();
        let mut out = StringOut::default();
        let (_, result) = chain.write_to(&mut out, |handler| {
            Xhtml::write(handler, metadata, &configs, |xhtml| {
                for call in body {
                    let field = |index: usize| call.get(index).map_or("", String::as_str);
                    match field(0) {
                        "start" => {
                            let attributes: Vec<(&str, &str)> = call[2..]
                                .chunks(2)
                                .map(|pair| (pair[0].as_str(), pair[1].as_str()))
                                .collect();
                            xhtml.start_element_with(field(1), &attributes)?;
                        }
                        "end" => xhtml.end_element(field(1))?,
                        "chars" => xhtml.characters(field(1))?,
                        "ws" => xhtml.ignorable_whitespace(field(1))?,
                        "newline" => xhtml.newline()?,
                        "element" => xhtml.element(field(1), field(2))?,
                        other => panic!("Unknown call {other}"),
                    }
                }
                Ok::<(), Stop>(())
            })
        });
        let stopped = match result {
            Ok(()) => false,
            Err(Stop::LimitReached) => true,
            Err(stop) => panic!("The output stopped: {stop:?}"),
        };
        (out.0, stopped)
    }

    fn chain(name: &str) -> Chain {
        match name {
            "stream-text" => Chain::Stream {
                output: Output::Text,
                encoding: "UTF-8",
            },
            "stream-xml" => Chain::Stream {
                output: Output::Xml,
                encoding: "UTF-8",
            },
            _ => {
                let (output, limit) = name
                    .strip_prefix("string-text-")
                    .map(|limit| (Output::Text, limit))
                    .or_else(|| {
                        name.strip_prefix("string-xml-")
                            .map(|limit| (Output::Xml, limit))
                    })
                    .unwrap();
                let max_length: usize = limit.parse().unwrap();
                assert!([LARGE_LIMIT, SMALL_LIMIT].contains(&max_length));
                Chain::String { output, max_length }
            }
        }
    }

    #[test]
    fn write_what_tikas_handler_chains_write() {
        let scripts = scripts();
        let mut compared = 0;
        let mut differences = Vec::new();
        for line in CHAINS.lines() {
            let fields: Vec<&str> = line.splitn(4, '\t').collect();
            let (script, chain_name, stopped, expected) =
                (fields[0], fields[1], fields[2], fields[3]);
            let calls = &scripts.iter().find(|(name, _)| name == script).unwrap().1;
            let (output, was_stopped) = play(calls, chain(chain_name));
            let expected = unescape(expected);
            compared += 1;
            if output != expected || was_stopped != (stopped == "true") {
                differences.push(format!(
                    "{script} {chain_name}: stopped {was_stopped}, expected {stopped}\n  got      {output:?}\n  expected {expected:?}"
                ));
            }
        }
        assert_eq!(compared, scripts.len() * 6);
        assert!(differences.is_empty(), "{}", differences.join("\n"));
    }
}
