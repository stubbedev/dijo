//! What extracting a document writes, recorded in the order it is written: the XHTML, the
//! metadata, the files it embeds and where the pages begin and end. The document is read once,
//! into a recording, which decides whether Tika's path takes it instead; the recording is then
//! replayed into the document's [`Xhtml`], exactly as the extraction would have written it there.

use std::ops::Range;

use super::attachment::EmbeddedFile;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::{MetadataSink, TikaMetadata};
use crate::formats::xhtml::{within_budget, Stop, Xhtml};
use crate::formats::Failure;

/// A string the recording holds, as where it lies in the recording's text.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: u32,
    end: u32,
}

/// One thing the extraction wrote, its strings in the recording's text.
#[derive(Debug)]
enum Event {
    Start {
        name: Span,
        /// Each attribute's name and value, in the recording's spans.
        attributes: Range<u32>,
    },
    End(Span),
    Characters(Span),
    Newline,
    Embedded(Box<EmbeddedFile>),
    Delegate(&'static str),
    Set {
        name: Span,
        value: Span,
    },
    Add {
        name: Span,
        value: Span,
    },
    ResetValues {
        name: Span,
        values: Range<u32>,
    },
    /// `PDF2XHTML.processPage` starts, and with it its catch of the write limit.
    EnterPage,
    LeavePage,
    EndDocument,
}

/// The output of a document's extraction, in the order it was written, and the metadata as it
/// stands after it.
#[derive(Debug)]
pub(super) struct Recording {
    metadata: TikaMetadata,
    events: Vec<Event>,
    /// Where the events written while a page is held back start, if one is.
    held: Option<usize>,
    text: String,
    spans: Vec<Span>,
}

impl Recording {
    /// A recording of a document whose parser starts from `metadata`.
    pub(super) fn new(metadata: TikaMetadata) -> Self {
        Self {
            metadata,
            events: Vec::new(),
            held: None,
            text: String::new(),
            spans: Vec::new(),
        }
    }

    fn span(&mut self, text: &str) -> Span {
        let start = self.position();
        self.text.push_str(text);
        Span {
            start,
            end: self.position(),
        }
    }

    fn position(&self) -> u32 {
        u32::try_from(self.text.len()).expect("a recording holds less than 4 GiB of text")
    }

    fn spans_position(&self) -> u32 {
        u32::try_from(self.spans.len()).expect("a recording holds fewer than 2^32 strings")
    }

    fn get_text(&self, span: Span) -> &str {
        &self.text[span.start as usize..span.end as usize]
    }

    /// Records what is written, as long as the extraction keeps to its memory budget.
    fn push(&mut self, event: Event) -> Result<(), Stop> {
        within_budget()?;
        self.events.push(event);
        Ok(())
    }

    /// The first value of `name`, as the metadata stands.
    pub(super) fn get(&self, name: &str) -> Option<&str> {
        self.metadata.get(name)
    }

    /// Every value of `name`, as the metadata stands.
    pub(super) fn values(&self, name: &str) -> &[String] {
        self.metadata.values(name)
    }

    /// [`TikaMetadata::set`], recorded.
    pub(super) fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        let event = Event::Set {
            name: self.span(name),
            value: self.span(&value),
        };
        self.metadata.set(name, value);
        self.events.push(event);
    }

    /// [`TikaMetadata::add`], recorded.
    pub(super) fn add(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        let event = Event::Add {
            name: self.span(name),
            value: self.span(&value),
        };
        self.metadata.add(name, value);
        self.events.push(event);
    }

    /// [`TikaMetadata::reset_values`], recorded.
    pub(super) fn reset_values(&mut self, name: &str, values: Vec<String>) {
        let name_span = self.span(name);
        let start = self.spans_position();
        for value in &values {
            let span = self.span(value);
            self.spans.push(span);
        }
        let event = Event::ResetValues {
            name: name_span,
            values: start..self.spans_position(),
        };
        self.metadata.reset_values(name, values);
        self.events.push(event);
    }

    /// [`Xhtml::delegate_to`], recorded.
    pub(super) fn delegate_to(&mut self, parser: &'static str) {
        self.metadata.add("tk:parsed-by", parser);
        self.events.push(Event::Delegate(parser));
    }

    pub(super) fn start_element(&mut self, name: &str) -> Result<(), Stop> {
        self.start_element_with(name, &[])
    }

    pub(super) fn start_element_with(
        &mut self,
        name: &str,
        attributes: &[(&str, &str)],
    ) -> Result<(), Stop> {
        let name = self.span(name);
        let start = self.spans_position();
        for (attribute, value) in attributes {
            let attribute = self.span(attribute);
            let value = self.span(value);
            self.spans.extend([attribute, value]);
        }
        let attributes = start..self.spans_position();
        self.push(Event::Start { name, attributes })
    }

    pub(super) fn end_element(&mut self, name: &str) -> Result<(), Stop> {
        let name = self.span(name);
        self.push(Event::End(name))
    }

    /// Records characters; empty text writes nothing, as [`Xhtml::characters`].
    pub(super) fn characters(&mut self, text: &str) -> Result<(), Stop> {
        if text.is_empty() {
            return Ok(());
        }
        let text = self.span(text);
        self.push(Event::Characters(text))
    }

    pub(super) fn newline(&mut self) -> Result<(), Stop> {
        self.push(Event::Newline)
    }

    /// [`Xhtml::element`]: an element holding `value`, or nothing when `value` is empty.
    pub(super) fn element(&mut self, name: &str, value: &str) -> Result<(), Stop> {
        if value.is_empty() {
            return Ok(());
        }
        self.start_element(name)?;
        self.characters(value)?;
        self.end_element(name)
    }

    /// A file the document embeds, parsed where it is replayed.
    pub(super) fn embedded(&mut self, file: EmbeddedFile) -> Result<(), Stop> {
        self.push(Event::Embedded(Box::new(file)))
    }

    pub(super) fn enter_page(&mut self) {
        self.events.push(Event::EnterPage);
    }

    pub(super) fn leave_page(&mut self) {
        self.events.push(Event::LeavePage);
    }

    pub(super) fn end_document(&mut self) -> Result<(), Stop> {
        self.push(Event::EndDocument)
    }

    /// Holds back what is written from here, the metadata excepted, until [`Recording::release`].
    pub(super) fn hold(&mut self) {
        self.held = Some(self.events.len());
    }

    /// Writes what was held back after the metadata recorded since, as our
    /// `TextLayerCheckingPdfParser` passes a page on once it ends.
    pub(super) fn release(&mut self) {
        let Some(start) = self.held.take() else {
            return;
        };
        let (held, metadata): (Vec<Event>, Vec<Event>) = self
            .events
            .drain(start..)
            .partition(|event| !is_metadata(event));
        self.events.extend(metadata);
        self.events.extend(held);
    }

    /// Writes the recording to `xhtml`, until the output stops taking it. The output stopping
    /// at the write limit on a page is recorded as `PDF2XHTML.processPage` records it.
    pub(super) fn replay(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let mut in_page = false;
        let result = self.replay_events(xhtml, &mut in_page);
        if in_page && matches!(result, Err(Failure::Stopped(Stop::LimitReached))) {
            xhtml
                .metadata_mut()
                .set("tk:exception:write-limit-reached", "true");
        }
        result
    }

    fn replay_events(&self, xhtml: &mut Xhtml, in_page: &mut bool) -> Result<(), Failure> {
        let mut attributes: Vec<(&str, &str)> = Vec::new();
        for event in &self.events {
            match event {
                Event::Start {
                    name,
                    attributes: range,
                } => {
                    attributes.clear();
                    let spans = &self.spans[range.start as usize..range.end as usize];
                    attributes.extend(
                        spans
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|[name, value]| (self.get_text(*name), self.get_text(*value))),
                    );
                    xhtml.start_element_with(self.get_text(*name), &attributes)?;
                }
                Event::End(name) => xhtml.end_element(self.get_text(*name))?,
                Event::Characters(text) => xhtml.characters(self.get_text(*text))?,
                Event::Newline => xhtml.newline()?,
                Event::Embedded(file) => {
                    xhtml.parse_embedded(&file.data, &file.metadata, Wrapping::Bare)?;
                }
                Event::Delegate(parser) => xhtml.delegate_to(parser),
                Event::Set { name, value } => xhtml
                    .metadata_mut()
                    .set(self.get_text(*name), self.get_text(*value)),
                Event::Add { name, value } => xhtml
                    .metadata_mut()
                    .add(self.get_text(*name), self.get_text(*value)),
                Event::ResetValues { name, values } => {
                    let values = self.spans[values.start as usize..values.end as usize]
                        .iter()
                        .map(|span| self.get_text(*span).to_owned())
                        .collect();
                    xhtml
                        .metadata_mut()
                        .reset_values(self.get_text(*name), values);
                }
                Event::EnterPage => *in_page = true,
                Event::LeavePage => *in_page = false,
                Event::EndDocument => xhtml.end_document()?,
            }
        }
        Ok(())
    }
}

/// Whether an event changes the metadata, which a held page does not hold back.
fn is_metadata(event: &Event) -> bool {
    matches!(
        event,
        Event::Set { .. } | Event::Add { .. } | Event::ResetValues { .. } | Event::Delegate(_)
    )
}

impl MetadataSink for Recording {
    fn get(&self, name: &str) -> Option<&str> {
        Recording::get(self, name)
    }

    fn values(&self, name: &str) -> &[String] {
        Recording::values(self, name)
    }

    fn set(&mut self, name: &str, value: impl Into<String>) {
        Recording::set(self, name, value);
    }

    fn add(&mut self, name: &str, value: impl Into<String>) {
        Recording::add(self, name, value);
    }
}
