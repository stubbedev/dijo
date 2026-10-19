//! What extracting a document writes, recorded in the order it is written: the XHTML, the
//! metadata, the files it embeds and where the pages begin and end. The document is read once,
//! into a recording, which decides whether the port reads it at all; the recording is then
//! replayed into the document's [`Xhtml`], exactly as the extraction would have written it there.
//! A document too large to record whole is streamed: its recording is flushed into the
//! [`Xhtml`] after each page.

use std::ops::Range;

use super::attachment::EmbeddedFile;
use super::ocr::{EndPage, PageCheck};
use crate::formats::embedded::{Wrapping, EXCEPTION_WARNING};
use crate::formats::metadata::TikaMetadata;
use crate::formats::thrown::Exception;
use crate::formats::xhtml::{within_budget, Stop, Xhtml};
use crate::formats::xmp;
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
    /// A page our `TextLayerCheckingPdfParser` reads by OCR ends: its page's index, what is
    /// done with it, and the content it held back, among the recording's held pages.
    PageRead {
        index: usize,
        check: PageCheck,
        held: usize,
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
    /// Whether pages are held back, as our `TextLayerCheckingPdfParser` holds each page until
    /// it ends, the documents it embeds parsed by then.
    holds_pages: bool,
    /// What the pages read by OCR held back, each written as the reading decides.
    read_pages: Vec<Vec<Event>>,
    text: String,
    spans: Vec<Span>,
    /// The pages Tika's PDF parser wants OCR for that no tesseract reads, so far.
    unrecognized: usize,
    /// The first `IOException` Tika's writer caught and recorded, which fails the extraction
    /// once the document is written.
    caught: Option<Exception>,
}

impl Recording {
    /// A recording of a document whose parser starts from `metadata`.
    pub(super) fn new(metadata: TikaMetadata) -> Self {
        Self {
            metadata,
            events: Vec::new(),
            held: None,
            holds_pages: false,
            read_pages: Vec::new(),
            text: String::new(),
            spans: Vec::new(),
            unrecognized: 0,
            caught: None,
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
        self.holds_pages = true;
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

    /// Keeps what was held back after the metadata recorded since for page `index`, which OCR
    /// reads, to be written as its reading decides when the recording is replayed.
    pub(super) fn release_read(&mut self, index: usize, check: PageCheck) {
        let start = self.held.take().unwrap_or(self.events.len());
        let (held, metadata): (Vec<Event>, Vec<Event>) = self
            .events
            .drain(start..)
            .partition(|event| !is_metadata(event));
        self.events.extend(metadata);
        self.events.push(Event::PageRead {
            index,
            check,
            held: self.read_pages.len(),
        });
        self.read_pages.push(held);
    }

    /// `AbstractPDF2XHTML.handleCatchableIOE`: the `IOException` `exception` recorded as a
    /// warning, the first kept to fail the extraction once the document is written.
    pub(super) fn catch_io(&mut self, exception: Exception) {
        self.add(EXCEPTION_WARNING, format!("{exception}\n"));
        self.caught.get_or_insert(exception);
    }

    /// The first `IOException` caught, if any.
    pub(super) fn caught(&self) -> Option<&Exception> {
        self.caught.as_ref()
    }

    /// Counts a page Tika's PDF parser wants OCR for that no tesseract reads.
    pub(super) fn count_unrecognized(&mut self) {
        self.unrecognized += 1;
    }

    /// The pages Tika's PDF parser wants OCR for that no tesseract reads.
    pub(super) fn unrecognized_pages(&self) -> usize {
        self.unrecognized
    }

    /// Whether a page is read by OCR.
    pub(super) fn reads_pages(&self) -> bool {
        !self.read_pages.is_empty()
    }

    /// The indices of the pages OCR reads, in order.
    pub(super) fn pages_read(&self) -> Vec<usize> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::PageRead { index, .. } => Some(*index),
                _ => None,
            })
            .collect()
    }

    /// About how many bytes the recording holds.
    pub(super) fn size(&self) -> usize {
        let events = self.events.len() + self.read_pages.iter().map(Vec::len).sum::<usize>();
        self.text.len() + events * size_of::<Event>() + self.spans.len() * size_of::<Span>()
    }

    /// Writes what is recorded to `xhtml`, until the output stops taking it, each page OCR
    /// reads ended by `page_ends`, and forgets it, the metadata as it stands kept. The output
    /// stopping at the write limit on a page is recorded as `PDF2XHTML.processPage` records it.
    /// No page may be held back.
    pub(super) fn flush(
        &mut self,
        xhtml: &mut Xhtml,
        mut page_ends: Option<&mut dyn EndPage>,
    ) -> Result<(), Failure> {
        assert!(self.held.is_none(), "a recording is flushed between pages");
        let mut in_page = false;
        let result = self.replay_events(&self.events, xhtml, &mut in_page, &mut page_ends);
        if in_page && matches!(result, Err(Failure::Stopped(Stop::LimitReached))) {
            xhtml
                .metadata_mut()
                .set("tk:exception:write-limit-reached", "true");
        }
        self.events.clear();
        self.read_pages.clear();
        self.text.clear();
        self.spans.clear();
        result
    }

    fn replay_events(
        &self,
        events: &[Event],
        xhtml: &mut Xhtml,
        in_page: &mut bool,
        page_ends: &mut Option<&mut dyn EndPage>,
    ) -> Result<(), Failure> {
        let mut attributes: Vec<(&str, &str)> = Vec::new();
        for (index, event) in events.iter().enumerate() {
            let replayed = self.replay_event(event, xhtml, in_page, page_ends, &mut attributes);
            if let Err(Failure::Stopped(Stop::LimitReached)) = replayed {
                if self.holds_pages && *in_page {
                    self.parse_page_embedded(&events[index + 1..], xhtml)?;
                }
            }
            replayed?;
        }
        Ok(())
    }

    /// Parses the documents a page embeds after the write limit was reached in it, as Tika's
    /// path parsed them while our `TextLayerCheckingPdfParser` held the page back, before it
    /// passed the page on and reached the limit. What they write is dropped at the limit.
    fn parse_page_embedded(&self, events: &[Event], xhtml: &mut Xhtml) -> Result<(), Failure> {
        for event in events {
            match event {
                Event::LeavePage => break,
                Event::Embedded(file) => {
                    match xhtml.parse_embedded(&file.data, &file.metadata, Wrapping::Bare) {
                        Ok(()) | Err(Failure::Stopped(Stop::LimitReached)) => {}
                        Err(failure) => return Err(failure),
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Replays one event.
    fn replay_event<'e>(
        &'e self,
        event: &'e Event,
        xhtml: &mut Xhtml,
        in_page: &mut bool,
        page_ends: &mut Option<&mut dyn EndPage>,
        attributes: &mut Vec<(&'e str, &'e str)>,
    ) -> Result<(), Failure> {
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
                xhtml.start_element_with(self.get_text(*name), attributes)?;
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
            Event::PageRead { index, check, held } => {
                let held = &self.read_pages[*held];
                let page_ends = page_ends
                    .as_deref_mut()
                    .expect("a recording with pages OCR reads is replayed with their ends");
                page_ends.end_page(xhtml, *index, *check, &mut |xhtml| {
                    self.replay_events(held, xhtml, &mut true, &mut None)
                })?;
            }
            Event::EnterPage => *in_page = true,
            Event::LeavePage => *in_page = false,
            Event::EndDocument => xhtml.end_document()?,
        }
        Ok(())
    }
}

impl xmp::Sink for Recording {
    fn get(&self, name: &str) -> Option<&str> {
        Recording::get(self, name)
    }

    fn values(&self, name: &str) -> &[String] {
        Recording::values(self, name)
    }

    fn set(&mut self, name: &str, value: String) {
        Recording::set(self, name, value);
    }

    fn add(&mut self, name: &str, value: String) {
        Recording::add(self, name, value);
    }
}

/// Whether an event changes the metadata, which a held page does not hold back.
fn is_metadata(event: &Event) -> bool {
    matches!(
        event,
        Event::Set { .. } | Event::Add { .. } | Event::ResetValues { .. } | Event::Delegate(_)
    )
}
