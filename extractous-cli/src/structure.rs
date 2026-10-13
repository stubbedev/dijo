use std::io::{self, BufReader, Read};

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::Serialize;

const PAGE_CLASSES: [&str; 2] = ["page", "slide-content"];
const ARTIFACT_CLASS: &str = "artifact";
/// The most text of a page, or outside every page, that is collected before it is passed on, so
/// a long page or a document without pages streams instead of being held whole.
const PIECE_BYTES: usize = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Pages,
    Elements,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementKind {
    Heading,
    Paragraph,
    ListItem,
    Table,
    Text,
}

/// One page, or one element, of a document's text in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub page: Option<u32>,
    pub element: Option<(ElementKind, Option<u8>)>,
    pub text: String,
    /// The text of a page that the document marks as furniture rather than content, such as a
    /// tagged PDF's running headers, footers and page numbers; empty for elements.
    pub artifacts: String,
    /// Whether the text goes on in the next part: a long page, and long text outside every page,
    /// is passed on in pieces of whole lines, the last one not continued.
    pub continued: bool,
}

/// An element whose text is being collected, opened at `depth`.
struct Open {
    depth: usize,
    kind: ElementKind,
    level: Option<u8>,
    text: String,
}

struct Splitter<'e> {
    unit: Unit,
    emit: &'e mut dyn FnMut(Part) -> io::Result<()>,
    depth: usize,
    in_body: bool,
    pages_seen: u32,
    page: Option<(u32, usize)>,
    loose: String,
    element: Option<Open>,
    artifact: Option<usize>,
    artifacts: String,
    in_pieces: bool,
    between_cells: bool,
}

fn element_kind(name: &str) -> Option<(ElementKind, Option<u8>)> {
    match name.as_bytes() {
        [b'h', level @ b'1'..=b'6'] => Some((ElementKind::Heading, Some(level - b'0'))),
        b"p" => Some((ElementKind::Paragraph, None)),
        b"li" => Some((ElementKind::ListItem, None)),
        b"table" => Some((ElementKind::Table, None)),
        _ => None,
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "li" | "tr" | "table" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
    )
}

fn is_div_of_class(start: &BytesStart, classes: &[&str]) -> bool {
    start.local_name().as_ref() == "div"
        && start
            .try_get_attribute("class")
            .ok()
            .flatten()
            .is_some_and(|class| classes.iter().any(|name| class.value.as_ref() == *name))
}

fn end_line(text: &mut String) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
}

impl Splitter<'_> {
    fn current_text(&mut self) -> &mut String {
        if self.artifact.is_some() {
            return &mut self.artifacts;
        }
        match &mut self.element {
            Some(open) => &mut open.text,
            None => &mut self.loose,
        }
    }

    fn flush_loose(&mut self) -> io::Result<()> {
        let text = std::mem::take(&mut self.loose);
        let artifacts = std::mem::take(&mut self.artifacts);
        let continues_run = std::mem::take(&mut self.in_pieces);
        let text = if continues_run {
            text.trim_end()
        } else {
            text.trim()
        };
        let artifacts = artifacts.trim();
        if text.is_empty() && artifacts.is_empty() && !continues_run {
            return Ok(());
        }
        let page = self.page.map(|(page, _)| page);
        let element = (self.unit == Unit::Elements).then_some((ElementKind::Text, None));
        (self.emit)(Part {
            page,
            element,
            text: text.to_string(),
            artifacts: artifacts.to_string(),
            continued: false,
        })
    }

    /// Passes on the text collected so far once it is long, up to its last line: the line still
    /// being written, and with it the last line of a page, comes in a later piece. Text without
    /// line breaks is passed on whole but for the whitespace it ends with, which the end of the
    /// text drops.
    fn pass_on_long_text(&mut self) -> io::Result<()> {
        if self.unit != Unit::Pages || self.loose.len() < PIECE_BYTES {
            return Ok(());
        }
        let content = self.loose.trim_end();
        let kept = match content.rfind('\n') {
            Some(newline) => newline + 1,
            None if self.loose.len() - content.len() < PIECE_BYTES => content.len(),
            None => self.loose.len(),
        };
        let rest = self.loose.split_off(kept);
        let piece = std::mem::replace(&mut self.loose, rest);
        let piece = if self.in_pieces {
            piece
        } else {
            piece.trim_start().to_string()
        };
        if piece.is_empty() {
            return Ok(());
        }
        self.in_pieces = true;
        (self.emit)(Part {
            page: self.page.map(|(page, _)| page),
            element: None,
            text: piece,
            artifacts: String::new(),
            continued: true,
        })
    }

    /// Emits the text collected so far when the XHTML ends early, so a cut-short extraction
    /// keeps the part it was in.
    fn flush_open(&mut self) -> io::Result<()> {
        if let Some(open) = self.element.take() {
            let text = open.text.trim();
            if !text.is_empty() {
                (self.emit)(Part {
                    page: self.page.map(|(page, _)| page),
                    element: Some((open.kind, open.level)),
                    text: text.to_string(),
                    artifacts: String::new(),
                    continued: false,
                })?;
            }
        }
        self.flush_loose()
    }

    fn start(&mut self, start: &BytesStart, empty: bool) -> io::Result<()> {
        let name = start.local_name();
        let name = name.as_ref();
        if name == "body" {
            self.in_body = true;
            return Ok(());
        }
        if !self.in_body {
            return Ok(());
        }
        if self.page.is_none() && is_div_of_class(start, &PAGE_CLASSES) {
            self.flush_loose()?;
            self.pages_seen += 1;
            if empty {
                return Ok(());
            }
            self.page = Some((self.pages_seen, self.depth));
            return Ok(());
        }
        match name {
            "tr" => self.between_cells = true,
            "td" | "th" => self.between_cells = false,
            _ => {}
        }
        if self.element.is_none() && is_block(name) {
            end_line(self.current_text());
        }
        if self.unit == Unit::Pages
            && self.page.is_some()
            && self.artifact.is_none()
            && !empty
            && is_div_of_class(start, &[ARTIFACT_CLASS])
        {
            self.artifact = Some(self.depth);
            return Ok(());
        }
        if self.unit == Unit::Elements && self.element.is_none() && !empty {
            if let Some((kind, level)) = element_kind(name) {
                self.flush_loose()?;
                self.element = Some(Open {
                    depth: self.depth,
                    kind,
                    level,
                    text: String::new(),
                });
            }
        }
        Ok(())
    }

    fn end(&mut self, name: &str) -> io::Result<()> {
        if !self.in_body {
            return Ok(());
        }
        match name {
            "td" | "th" => {
                self.between_cells = true;
                self.current_text().push('\t');
            }
            "tr" => {
                self.between_cells = false;
                let text = self.current_text();
                let trimmed = text.trim_end_matches('\t').len();
                text.truncate(trimmed);
                text.push('\n');
            }
            name if is_block(name) => end_line(self.current_text()),
            _ => {}
        }
        if self
            .element
            .as_ref()
            .is_some_and(|open| open.depth == self.depth)
        {
            let open = self.element.take().expect("checked above");
            let text = open.text.trim();
            if !text.is_empty() {
                (self.emit)(Part {
                    page: self.page.map(|(page, _)| page),
                    element: Some((open.kind, open.level)),
                    text: text.to_string(),
                    artifacts: String::new(),
                    continued: false,
                })?;
            }
        }
        if self.artifact == Some(self.depth) {
            self.artifact = None;
        }
        if self.page.is_some_and(|(_, depth)| depth == self.depth) {
            self.flush_loose()?;
            self.page = None;
        }
        if name == "body" {
            self.in_body = false;
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> io::Result<()> {
        if !self.in_body {
            return Ok(());
        }
        if text.trim().is_empty() && self.between_cells {
            return Ok(());
        }
        let current = self.current_text();
        if text.trim().is_empty() && (current.is_empty() || current.ends_with('\n')) {
            return Ok(());
        }
        current.push_str(text);
        self.pass_on_long_text()
    }
}

/// Why the XHTML could not be split to its end.
#[derive(Debug)]
pub enum SplitError {
    /// Reading the XHTML failed, as it does when the extraction times out.
    Read(io::Error),
    /// The XHTML is malformed.
    Malformed(String),
}

/// Keeps the error a read failed with, which the XML reader would only report as text.
struct KeepReadError<R> {
    inner: R,
    error: Option<io::Error>,
}

impl<R: Read> Read for KeepReadError<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer).map_err(|error| {
            let reported = io::Error::new(error.kind(), error.to_string());
            self.error = Some(error);
            reported
        })
    }
}

/// The most text the XML reader is handed in one run: a document without markup, such as a large
/// text file, would otherwise be read as one text event held whole in memory.
const MAX_TEXT_RUN_BYTES: usize = 1 << 16;
const TEXT_BREAK: &[u8] = b"<!---->";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lexed {
    Text,
    Markup { quote: Option<u8>, opened: u8 },
    Comment { dashes: u8 },
}

/// Ends long runs of text with an empty comment, which the XML reader reports between two text
/// events, so no single event outgrows [`MAX_TEXT_RUN_BYTES`] by much. The comment goes only
/// where it cannot change the text: outside markup, comments and entity references, at a
/// character boundary and after a byte that is not whitespace.
struct TextBreaker<R> {
    inner: R,
    buffer: Box<[u8]>,
    position: usize,
    filled: usize,
    state: Lexed,
    in_entity: bool,
    run: usize,
    previous: u8,
    pending_break: usize,
}

impl<R: Read> TextBreaker<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: vec![0; 1 << 16].into_boxed_slice(),
            position: 0,
            filled: 0,
            state: Lexed::Text,
            in_entity: false,
            run: 0,
            previous: b'>',
            pending_break: 0,
        }
    }

    fn breaks_before(&self, byte: u8) -> bool {
        self.state == Lexed::Text
            && self.run >= MAX_TEXT_RUN_BYTES
            && !self.in_entity
            && byte & 0xc0 != 0x80
            && byte != b'<'
            && !self.previous.is_ascii_whitespace()
    }

    fn advance(&mut self, byte: u8) {
        self.state = match self.state {
            Lexed::Text => match byte {
                b'<' => {
                    self.in_entity = false;
                    self.run = 0;
                    Lexed::Markup {
                        quote: None,
                        opened: 0,
                    }
                }
                _ => {
                    self.in_entity = match byte {
                        b'&' => true,
                        b';' => false,
                        _ => self.in_entity && !byte.is_ascii_whitespace(),
                    };
                    self.run += 1;
                    Lexed::Text
                }
            },
            Lexed::Markup {
                quote: Some(open),
                opened,
            } => Lexed::Markup {
                quote: (byte != open).then_some(open),
                opened,
            },
            Lexed::Markup {
                quote: None,
                opened,
            } => match (opened, byte) {
                (0, b'!') | (1, b'-') => Lexed::Markup {
                    quote: None,
                    opened: opened + 1,
                },
                (2, b'-') => Lexed::Comment { dashes: 0 },
                (_, b'>') => Lexed::Text,
                (_, b'"' | b'\'') => Lexed::Markup {
                    quote: Some(byte),
                    opened: 3,
                },
                _ => Lexed::Markup {
                    quote: None,
                    opened: 3,
                },
            },
            Lexed::Comment { dashes } => match byte {
                b'-' => Lexed::Comment {
                    dashes: (dashes + 1).min(2),
                },
                b'>' if dashes == 2 => Lexed::Text,
                _ => Lexed::Comment { dashes: 0 },
            },
        };
        self.previous = byte;
    }
}

impl<R: Read> Read for TextBreaker<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < out.len() {
            if self.pending_break > 0 {
                let from = TEXT_BREAK.len() - self.pending_break;
                let count = self.pending_break.min(out.len() - written);
                out[written..written + count].copy_from_slice(&TEXT_BREAK[from..from + count]);
                written += count;
                self.pending_break -= count;
                continue;
            }
            if self.position == self.filled {
                if written > 0 {
                    break;
                }
                self.filled = self.inner.read(&mut self.buffer)?;
                self.position = 0;
                if self.filled == 0 {
                    break;
                }
            }
            let byte = self.buffer[self.position];
            if self.breaks_before(byte) {
                self.pending_break = TEXT_BREAK.len();
                self.run = 0;
                continue;
            }
            out[written] = byte;
            written += 1;
            self.position += 1;
            self.advance(byte);
        }
        Ok(written)
    }
}

/// Streams Tika's XHTML and calls `emit` with each page, or each element, as soon as it is
/// complete, so memory holds one page or element at a time. Text outside any page or element is
/// emitted where it occurs rather than dropped, and so is the text collected when the XHTML
/// breaks off. The outer result fails when `emit` does; the inner one when the XHTML cannot be
/// read or parsed.
pub fn split(
    xhtml: impl Read,
    unit: Unit,
    emit: &mut dyn FnMut(Part) -> io::Result<()>,
) -> io::Result<Result<(), SplitError>> {
    let mut reader = Reader::from_reader(BufReader::new(TextBreaker::new(KeepReadError {
        inner: xhtml,
        error: None,
    })));
    let mut splitter = Splitter {
        unit,
        emit,
        depth: 0,
        in_body: false,
        pages_seen: 0,
        page: None,
        loose: String::new(),
        element: None,
        artifact: None,
        artifacts: String::new(),
        in_pieces: false,
        between_cells: false,
    };
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let event = match reader.read_event_into(&mut buffer) {
            Ok(event) => event,
            Err(error) => {
                splitter.flush_open()?;
                let read = reader.get_mut().get_mut().inner.error.take();
                return Ok(Err(read.map_or_else(
                    || SplitError::Malformed(error.to_string()),
                    SplitError::Read,
                )));
            }
        };
        match event {
            Event::Start(start) => {
                splitter.start(&start, false)?;
                splitter.depth += 1;
            }
            Event::Empty(start) => {
                splitter.start(&start, true)?;
                let name = start.local_name();
                if is_block(name.as_ref()) && splitter.in_body {
                    end_line(splitter.current_text());
                }
            }
            Event::End(end) => {
                splitter.depth -= 1;
                splitter.end(end.local_name().as_ref())?;
            }
            Event::Text(text) => splitter.text(&text.xml10_content())?,
            Event::CData(data) => splitter.text(&data.xml10_content())?,
            Event::GeneralRef(reference) => {
                let resolved = match reference.resolve_char_ref() {
                    Ok(Some(character)) => character.to_string(),
                    Ok(None) => {
                        let name = reference.xml10_content();
                        resolve_predefined_entity(&name)
                            .map_or_else(|| format!("&{name};"), str::to_string)
                    }
                    Err(error) => {
                        splitter.flush_open()?;
                        return Ok(Err(SplitError::Malformed(error.to_string())));
                    }
                };
                splitter.text(&resolved)?;
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
    }
    splitter.flush_open()?;
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(xhtml: &str, unit: Unit) -> Vec<Part> {
        let mut parts = Vec::new();
        split(xhtml.as_bytes(), unit, &mut |part| {
            parts.push(part);
            Ok(())
        })
        .unwrap()
        .unwrap();
        parts
    }

    const PAGED: &str = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>T</title></head>
<body><div class="page"><p />
<p>First &amp; one</p>
<p>Second</p></div>
<div class="page"><h2>Head</h2><table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table></div>
</body></html>"#;

    #[test]
    fn text_before_the_xhtml_breaks_off_is_kept() {
        struct Failing<'a>(&'a [u8]);
        impl io::Read for Failing<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
                }
                let read = self.0.len().min(buffer.len());
                buffer[..read].copy_from_slice(&self.0[..read]);
                self.0 = &self.0[read..];
                Ok(read)
            }
        }
        let cut = &PAGED[..PAGED.find("Second</p>").unwrap() + "Second</p>".len()];
        for unit in [Unit::Pages, Unit::Elements] {
            let mut parts = Vec::new();
            let result = split(Failing(cut.as_bytes()), unit, &mut |part| {
                parts.push(part);
                Ok(())
            })
            .unwrap();
            assert!(
                matches!(result, Err(SplitError::Read(error)) if error.kind() == io::ErrorKind::TimedOut)
            );
            let text: Vec<&str> = parts.iter().map(|part| part.text.as_str()).collect();
            assert_eq!(text.join("\n"), "First & one\nSecond", "{unit:?}");
            assert!(parts.iter().all(|part| part.page == Some(1)));
        }
    }

    #[test]
    fn pages_are_numbered_in_order() {
        let pages = parts(PAGED, Unit::Pages);
        assert_eq!(
            pages,
            [
                Part {
                    page: Some(1),
                    element: None,
                    text: "First & one\nSecond".to_string(),
                    artifacts: String::new(),
                    continued: false,
                },
                Part {
                    page: Some(2),
                    element: None,
                    text: "Head\na\tb\nc".to_string(),
                    artifacts: String::new(),
                    continued: false,
                },
            ]
        );
    }

    #[test]
    fn the_whitespace_tika_writes_between_cells_is_not_text() {
        let xhtml = "<html><body><table><tr>\t<td>Year</td>\t<td>Ships</td></tr>\n\
                     <tr>\t<td>1921</td>\t<td>412</td></tr></table></body></html>";
        assert_eq!(parts(xhtml, Unit::Pages)[0].text, "Year\tShips\n1921\t412");
        assert_eq!(
            parts(xhtml, Unit::Elements)[0].text,
            "Year\tShips\n1921\t412"
        );
    }

    #[test]
    fn artifacts_of_a_page_are_kept_apart_from_its_text() {
        let xhtml = r#"<html><body>
<div class="page"><h1>Part One</h1><p>Body</p>
<div class="artifact"><p>Running title</p><p>Page 1</p></div></div>
<div class="page"><p>More</p></div>
</body></html>"#;
        assert_eq!(
            parts(xhtml, Unit::Pages),
            [
                Part {
                    page: Some(1),
                    element: None,
                    text: "Part One\nBody".to_string(),
                    artifacts: "Running title\nPage 1".to_string(),
                    continued: false,
                },
                Part {
                    page: Some(2),
                    element: None,
                    text: "More".to_string(),
                    artifacts: String::new(),
                    continued: false,
                },
            ]
        );
        let texts: Vec<_> = parts(xhtml, Unit::Elements)
            .into_iter()
            .map(|part| part.text)
            .collect();
        assert_eq!(
            texts,
            ["Part One", "Body", "Running title", "Page 1", "More"]
        );
    }

    #[test]
    fn long_text_outside_pages_streams_in_pieces() {
        let line = "A line of a long document without pages.\n";
        let body = line.repeat(5000);
        let xhtml = format!("<html><body><p>\n{body}  </p></body></html>");
        let pieces = parts(&xhtml, Unit::Pages);
        assert!(pieces.len() > 2, "{} pieces", pieces.len());
        let (last, rest) = pieces.split_last().unwrap();
        assert!(rest.iter().all(|piece| piece.continued
            && piece.page.is_none()
            && piece.text.len() <= PIECE_BYTES + line.len()));
        assert!(!last.continued);
        let joined: String = pieces.iter().map(|piece| piece.text.as_str()).collect();
        assert_eq!(joined, body.trim());
    }

    #[test]
    fn a_long_page_streams_in_pieces_of_whole_lines() {
        let line = "A row of a sheet that runs to many rows.\n";
        let body = line.repeat(5000);
        let xhtml = format!(
            "<html><body><div class=\"page\"><p>Header line\n{body}Footer line</p>\
             <div class=\"artifact\"><p>Page 1</p></div></div></body></html>"
        );
        let pieces = parts(&xhtml, Unit::Pages);
        assert!(pieces.len() > 2, "{} pieces", pieces.len());
        let (last, rest) = pieces.split_last().unwrap();
        assert!(rest.iter().all(|piece| piece.continued
            && piece.page == Some(1)
            && piece.text.ends_with('\n')
            && piece.artifacts.is_empty()));
        assert!(rest[0].text.starts_with("Header line\n"));
        assert!(!last.continued);
        assert!(last.text.ends_with("Footer line"));
        assert_eq!(last.artifacts, "Page 1");
        let joined: String = pieces.iter().map(|piece| piece.text.as_str()).collect();
        assert_eq!(joined, format!("Header line\n{body}Footer line"));
    }

    #[test]
    fn breaking_long_text_for_the_xml_reader_never_changes_it() {
        let words = [
            "plain",
            "caf\u{e9}",
            "\u{6f22}\u{5b57}",
            "&amp;",
            "&#x1F389;",
            "a&lt;b",
        ];
        let unescaped = [
            "plain",
            "caf\u{e9}",
            "\u{6f22}\u{5b57}",
            "&",
            "\u{1F389}",
            "a<b",
        ];
        let mut escaped = String::new();
        let mut expected = String::new();
        for index in 0..40_000 {
            let separator = if index % 7 == 6 { "\n" } else { " " };
            escaped.push_str(words[index % words.len()]);
            escaped.push_str(separator);
            expected.push_str(unescaped[index % unescaped.len()]);
            expected.push_str(separator);
        }
        let xhtml = format!(
            "<html><body><!-- a > b -- c --><div title=\"x > y\"><p>{escaped}</p></div></body></html>"
        );
        let joined: String = parts(&xhtml, Unit::Pages)
            .iter()
            .map(|part| part.text.as_str())
            .collect();
        assert_eq!(joined, expected.trim());
    }

    #[test]
    fn elements_carry_their_kind_level_and_page() {
        let elements = parts(PAGED, Unit::Elements);
        let summary: Vec<_> = elements
            .iter()
            .map(|part| (part.page, part.element, part.text.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                (Some(1), Some((ElementKind::Paragraph, None)), "First & one"),
                (Some(1), Some((ElementKind::Paragraph, None)), "Second"),
                (Some(2), Some((ElementKind::Heading, Some(2))), "Head"),
                (Some(2), Some((ElementKind::Table, None)), "a\tb\nc"),
            ]
        );
    }

    #[test]
    fn unpaged_documents_are_one_part_and_loose_text_is_kept() {
        let xhtml = "<html><body><h1>Title</h1>loose words<p>Body</p></body></html>";
        assert_eq!(
            parts(xhtml, Unit::Pages),
            [Part {
                page: None,
                element: None,
                text: "Title\nloose words\nBody".to_string(),
                artifacts: String::new(),
                continued: false,
            }]
        );
        let kinds: Vec<_> = parts(xhtml, Unit::Elements)
            .into_iter()
            .map(|part| (part.element.map(|(kind, _)| kind), part.text))
            .collect();
        assert_eq!(
            kinds,
            [
                (Some(ElementKind::Heading), "Title".to_string()),
                (Some(ElementKind::Text), "loose words".to_string()),
                (Some(ElementKind::Paragraph), "Body".to_string()),
            ]
        );
    }

    const FRAGMENTS: &[&str] = &[
        "<html>",
        "</html>",
        "<body>",
        "</body>",
        "<div class=\"page\">",
        "<div class=\"slide-content\">",
        "<div class=\"artifact\">",
        "</div>",
        "<p>",
        "</p>",
        "<p/>",
        "<h1>",
        "</h1>",
        "<h7>",
        "<li>",
        "</li>",
        "<table>",
        "</table>",
        "<td>",
        "</td>",
        "text",
        " ",
        "\n",
        "\t",
        "&amp;",
        "&bogus;",
        "&#x1F389;",
        "&#0;",
        "<",
        ">",
        "</",
        "<!--",
        "-->",
        "<![CDATA[",
        "]]>",
        "<?xml version=\"1.0\"?>",
        "<!DOCTYPE html>",
        "\u{0}",
        "\u{fffd}",
        "é",
        "\"",
        "=",
        "<a href='x'>",
        "</a>",
    ];

    #[test]
    fn arbitrary_markup_never_panics() {
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let length = (next() % 60) as usize;
            let xhtml: String = (0..length)
                .map(|_| FRAGMENTS[(next() % FRAGMENTS.len() as u64) as usize])
                .collect();
            for unit in [Unit::Pages, Unit::Elements] {
                let mut parts = 0;
                let result = split(xhtml.as_bytes(), unit, &mut |_| {
                    parts += 1;
                    Ok(())
                });
                assert!(result.is_ok(), "{xhtml:?}");
                assert!(parts <= xhtml.len() + 1, "{xhtml:?}");
            }
        }
    }
}
