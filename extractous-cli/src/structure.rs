use std::io::{self, BufRead};

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::Serialize;

const PAGE_CLASSES: [&str; 2] = ["page", "slide-content"];

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

fn is_page(start: &BytesStart) -> bool {
    start.local_name().as_ref() == "div"
        && start
            .try_get_attribute("class")
            .ok()
            .flatten()
            .is_some_and(|class| {
                PAGE_CLASSES
                    .iter()
                    .any(|page| class.value.as_ref() == *page)
            })
}

fn end_line(text: &mut String) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
}

impl Splitter<'_> {
    fn current_text(&mut self) -> &mut String {
        match &mut self.element {
            Some(open) => &mut open.text,
            None => &mut self.loose,
        }
    }

    fn flush_loose(&mut self) -> io::Result<()> {
        let text = std::mem::take(&mut self.loose);
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let page = self.page.map(|(page, _)| page);
        let element = (self.unit == Unit::Elements).then_some((ElementKind::Text, None));
        (self.emit)(Part {
            page,
            element,
            text: text.to_string(),
        })
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
        if self.page.is_none() && is_page(start) {
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
                })?;
            }
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

/// Streams Tika's XHTML and calls `emit` with each page, or each element, as soon as it is
/// complete, so memory holds one page or element at a time. Text outside any page or element is
/// emitted where it occurs rather than dropped. The outer result fails when `emit` does; the
/// inner one when the XHTML cannot be read or parsed.
pub fn split(
    xhtml: impl BufRead,
    unit: Unit,
    emit: &mut dyn FnMut(Part) -> io::Result<()>,
) -> io::Result<Result<(), String>> {
    let mut reader = Reader::from_reader(xhtml);
    let mut splitter = Splitter {
        unit,
        emit,
        depth: 0,
        in_body: false,
        pages_seen: 0,
        page: None,
        loose: String::new(),
        element: None,
    };
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let event = match reader.read_event_into(&mut buffer) {
            Ok(event) => event,
            Err(error) => return Ok(Err(error.to_string())),
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
                    Err(error) => return Ok(Err(error.to_string())),
                };
                splitter.text(&resolved);
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
    }
    splitter.flush_loose()?;
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
    fn pages_are_numbered_in_order() {
        let pages = parts(PAGED, Unit::Pages);
        assert_eq!(
            pages,
            [
                Part {
                    page: Some(1),
                    element: None,
                    text: "First & one\nSecond".to_string()
                },
                Part {
                    page: Some(2),
                    element: None,
                    text: "Head\na\tb\nc".to_string()
                },
            ]
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
                text: "Title\nloose words\nBody".to_string()
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
}
