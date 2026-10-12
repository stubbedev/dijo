use std::io::{self, BufReader, Read};

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::Serialize;

const PAGE_CLASSES: [&str; 2] = ["page", "slide-content"];
const ARTIFACT_CLASS: &str = "artifact";

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
        let (text, artifacts) = (text.trim(), artifacts.trim());
        if text.is_empty() && artifacts.is_empty() {
            return Ok(());
        }
        let page = self.page.map(|(page, _)| page);
        let element = (self.unit == Unit::Elements).then_some((ElementKind::Text, None));
        (self.emit)(Part {
            page,
            element,
            text: text.to_string(),
            artifacts: artifacts.to_string(),
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
            "td" | "th" => self.current_text().push('\t'),
            "tr" => {
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

    fn text(&mut self, text: &str) {
        if !self.in_body {
            return;
        }
        let current = self.current_text();
        if text.trim().is_empty() && (current.is_empty() || current.ends_with('\n')) {
            return;
        }
        current.push_str(text);
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
    let mut reader = Reader::from_reader(BufReader::new(KeepReadError {
        inner: xhtml,
        error: None,
    }));
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
    };
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let event = match reader.read_event_into(&mut buffer) {
            Ok(event) => event,
            Err(error) => {
                splitter.flush_open()?;
                let read = reader.get_mut().get_mut().error.take();
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
            Event::Text(text) => splitter.text(&text.xml10_content()),
            Event::CData(data) => splitter.text(&data.xml10_content()),
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
                splitter.text(&resolved);
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
                },
                Part {
                    page: Some(2),
                    element: None,
                    text: "Head\na\tb\nc".to_string(),
                    artifacts: String::new(),
                },
            ]
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
                },
                Part {
                    page: Some(2),
                    element: None,
                    text: "More".to_string(),
                    artifacts: String::new(),
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
