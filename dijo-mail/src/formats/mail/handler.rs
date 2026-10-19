//! Tika's `MailContentHandler`, which turns the parts mime4j reports into the message's metadata
//! and body: the headers of the message itself into metadata, text and HTML parts at the top
//! level and the best alternative of a `multipart/alternative` parsed inline, each part of a
//! multipart in a `div` of class `email-entry`, and every other part handed to the embedded
//! document extractor.

use super::address::{flatten, parse_address_list, Address, Mailbox};
use super::codec::decode_encoded_words;
use super::date::parse_date;
use super::stream::{events, BodyDescriptor, Event, MimeError};
use super::Unported;
use crate::formats::embedded::ResourceType;
use crate::formats::metadata::TikaMetadata;
use crate::input::{Input, Paging};
use crate::java::is_blank;
use crate::mime::MediaType;

const MULTIPART_ALTERNATIVE: &str = "multipart/alternative";
const RAW_HEADER_PREFIX: &str = "message:raw-header:";
/// The longest name `Metadata.add(KeyPrefix, name, value)` accepts.
const MAX_PREFIXED_NAME_LENGTH: usize = 1024;

/// What the body of a message holds, in order.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Item {
    /// `<div class="email-entry"><p>`, which starts a part of a multipart.
    StartEntry,
    /// `</p></div>`.
    EndEntry,
    /// A part parsed inline, as part of the message's own text.
    Inline(Inline),
    /// A part handed to the embedded document extractor, with the metadata it is given.
    Embedded(Input<'static>, TikaMetadata),
}

/// A text or HTML part, which Tika parses with its text or HTML parser into the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Inline {
    Text(Input<'static>),
    Html(Input<'static>),
}

/// Where the handler puts what it reads of a message: the message's metadata, and the items of
/// its body, in order, which it may write as they come.
pub(super) trait Sink {
    /// What ends the reading: mime4j's failure, or the sink's own.
    type Error: From<MimeError>;

    /// The metadata of the message itself.
    fn metadata(&mut self) -> &mut TikaMetadata;

    /// The next item of the message's body.
    fn item(&mut self, item: Item) -> Result<(), Self::Error>;
}

/// The items of a message's body collected, with its metadata, before any is written.
#[cfg(test)]
pub(super) struct Collected<'m> {
    pub(super) metadata: &'m mut TikaMetadata,
    pub(super) items: Vec<Item>,
}

#[cfg(test)]
impl Sink for Collected<'_> {
    type Error = MimeError;

    fn metadata(&mut self) -> &mut TikaMetadata {
        self.metadata
    }

    fn item(&mut self, item: Item) -> Result<(), MimeError> {
        self.items.push(item);
        Ok(())
    }
}

/// A part of a `multipart/alternative`, kept until the alternative ends.
#[derive(Debug, Clone)]
enum Part {
    Multipart {
        mime_type: String,
        children: Vec<usize>,
    },
    Body(TikaMetadata, Input<'static>),
}

/// Reads a message from `input`, whose pages `paging` releases as they are read: its header
/// fields into the metadata of `sink` as Tika sets them, and the items of its body into `sink`
/// as they are read. It returns the first thing of the message the port does not read as Tika
/// does, which leaves out the header, or the rest of the part's `Content-Disposition`, it is in;
/// what ends it is mime4j's failure, which Tika reports, or the sink's.
pub(super) fn read<S: Sink>(
    input: &[u8],
    paging: &Paging<'_>,
    sink: &mut S,
) -> Result<Option<Unported>, S::Error> {
    let mut handler = Handler {
        sink,
        parts: Vec::new(),
        arena: Vec::new(),
        alternatives: Vec::new(),
        unported: None,
    };
    events(input, paging, |event| -> Result<(), S::Error> {
        match event {
            Event::Field(field) => {
                if handler.parts.is_empty() {
                    if let Err(unported) = handler.field(field.name(), &field.body()) {
                        handler.unported.get_or_insert(unported);
                    }
                }
            }
            Event::StartMultipart(body) => handler.start_multipart(body),
            Event::EndMultipart => handler.end_multipart()?,
            Event::StartBodyPart => {
                if handler.alternatives.is_empty() {
                    handler.sink.item(Item::StartEntry)?;
                }
            }
            Event::EndBodyPart => {
                if handler.alternatives.is_empty() {
                    handler.sink.item(Item::EndEntry)?;
                }
            }
            Event::Body(body, content) => handler.body(&body, content)?,
        }
        Ok(())
    })?;
    Ok(handler.unported)
}

struct Handler<'s, S> {
    sink: &'s mut S,
    /// The descriptors of the multiparts the stream is in.
    parts: Vec<BodyDescriptor>,
    arena: Vec<Part>,
    /// The parts of the `multipart/alternative` being read, innermost last.
    alternatives: Vec<usize>,
    unported: Option<Unported>,
}

impl<S: Sink> Handler<'_, S> {
    /// `field`: a header of the message itself.
    fn field(&mut self, name: &str, body: &str) -> Result<(), Unported> {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "from" => {
                for address in flatten(parse_address_list(body)?) {
                    if let Address::Mailbox(mailbox) = &address {
                        set_name_and_email(mailbox, self.sink.metadata());
                    }
                    let from = address.display();
                    self.sink.metadata().add("message:from", from.clone());
                    self.sink.metadata().add("dc:creator", from);
                }
            }
            "subject" => {
                // Tika also names the message after its subject, as `<subject>.eml` (#79).
                let subject = decode_encoded_words(body)?;
                self.sink.metadata().set("dc:title", subject.clone());
                self.sink.metadata().set("dc:subject", subject);
            }
            "to" | "cc" | "bcc" => {
                let property = format!("message:{lower}");
                for address in parse_address_list(body)? {
                    self.sink.metadata().add(&property, address.display());
                }
            }
            "content-type" => {
                let Some(content_type) = MediaType::parse(body) else {
                    return Ok(());
                };
                if content_type.kind() == "multipart" {
                    self.sink
                        .metadata()
                        .set("multipart:subtype", content_type.subtype().to_owned());
                    match content_type.parameter("boundary") {
                        Some(boundary) => self
                            .sink
                            .metadata()
                            .set("multipart:boundary", boundary.to_owned()),
                        None => self.sink.metadata().remove("multipart:boundary"),
                    }
                } else {
                    self.add_raw_header(name, body);
                }
            }
            // A date that does not parse leaves the one before it, which Tika removes (#80).
            "date" => {
                if let Some(date) = parse_date(body)? {
                    self.sink.metadata().set("dcterms:created", date.format());
                }
            }
            _ => self.add_raw_header(name, body),
        }
        Ok(())
    }

    fn add_raw_header(&mut self, name: &str, body: &str) {
        if crate::java::utf16_length(name) > MAX_PREFIXED_NAME_LENGTH {
            return;
        }
        self.sink
            .metadata()
            .add(&format!("{RAW_HEADER_PREFIX}{name}"), body.to_owned());
    }

    fn start_multipart(&mut self, body: BodyDescriptor) {
        let alternative = body.mime_type.eq_ignore_ascii_case(MULTIPART_ALTERNATIVE);
        let node = || Part::Multipart {
            mime_type: body.mime_type.clone(),
            children: Vec::new(),
        };
        if self.alternatives.is_empty() && alternative {
            self.arena.push(node());
            self.alternatives.push(self.arena.len() - 1);
        } else if let Some(&parent) = self.alternatives.last() {
            self.arena.push(node());
            let index = self.arena.len() - 1;
            self.alternatives.push(index);
            if let Part::Multipart { children, .. } = &mut self.arena[parent] {
                children.push(index);
            }
        }
        self.parts.push(body);
    }

    fn end_multipart(&mut self) -> Result<(), S::Error> {
        match self.alternatives.len() {
            1 => {
                let root = self.alternatives.pop().expect("one alternative is open");
                self.handle_best_parts(Some(root))?;
                self.arena.clear();
            }
            0 => {}
            _ => {
                self.alternatives.pop();
            }
        }
        self.parts.pop();
        Ok(())
    }

    /// `body`: a leaf part, kept while an alternative is open, parsed inline at the top level
    /// when it is text, and handed on otherwise.
    fn body(&mut self, body: &BodyDescriptor, content: Input<'static>) -> Result<(), S::Error> {
        let mut metadata = self.part_metadata(body);
        if let Err(unported) = disposition_metadata(body, &mut metadata) {
            self.unported.get_or_insert(unported);
        }
        if let Some(&open) = self.alternatives.last() {
            self.arena.push(Part::Body(metadata, content));
            let index = self.arena.len() - 1;
            if let Part::Multipart { children, .. } = &mut self.arena[open] {
                children.push(index);
            }
            Ok(())
        } else if self.parts.len() < 2 && is_inline_text(&metadata) {
            self.handle_inline(metadata, content)
        } else {
            self.handle_embedded(metadata, content)
        }
    }

    /// The metadata `body` gives a part: its type, charset and the multipart it is in.
    fn part_metadata(&self, body: &BodyDescriptor) -> TikaMetadata {
        let mut metadata = TikaMetadata::new();
        metadata.set("Content-Type", body.mime_type.clone());
        if let Some(charset) = &body.charset {
            metadata.set("Content-Encoding", charset.clone());
        }
        if let Some(parent) = self.parts.last() {
            metadata.set("multipart:subtype", parent.subtype.clone());
            match &parent.boundary {
                Some(boundary) => metadata.set("multipart:boundary", boundary.clone()),
                None => metadata.remove("multipart:boundary"),
            }
        }
        metadata
    }

    fn handle_best_parts(&mut self, part: Option<usize>) -> Result<(), S::Error> {
        let Some(part) = part else {
            return Ok(());
        };
        match self.arena[part].clone() {
            Part::Body(metadata, content) => self.handle_inline(metadata, content),
            Part::Multipart {
                mime_type,
                children,
            } => {
                if mime_type.eq_ignore_ascii_case(MULTIPART_ALTERNATIVE) {
                    let mut best = None;
                    let mut best_score = -1;
                    for child in children {
                        let score = self.score(child);
                        if score > best_score {
                            best = Some(child);
                            best_score = score;
                        }
                    }
                    self.handle_best_parts(best)
                } else {
                    for child in children {
                        self.handle_best_parts(Some(child))?;
                    }
                    Ok(())
                }
            }
        }
    }

    /// How good an alternative is: HTML the best of the bodies, then RTF, then plain text, and
    /// a multipart, which may hold the HTML with its images, better still. A body that is not
    /// text ranks with one without a type, below plain text: Tika ranks it above HTML, so an
    /// alternative of HTML and an image shows the image (#81).
    fn score(&self, part: usize) -> i32 {
        match &self.arena[part] {
            Part::Body(metadata, _) => match metadata.get("Content-Type") {
                Some(kind) if kind.eq_ignore_ascii_case("text/plain") => 1,
                Some(kind) if kind.eq_ignore_ascii_case("application/rtf") => 2,
                Some(kind) if kind.eq_ignore_ascii_case("text/html") => 3,
                _ => 0,
            },
            Part::Multipart { .. } => 4,
        }
    }

    /// `handleInlineBodyPart`: HTML and plain text parsed inline, anything else handed on.
    fn handle_inline(
        &mut self,
        metadata: TikaMetadata,
        content: Input<'static>,
    ) -> Result<(), S::Error> {
        let kind = metadata.get("Content-Type").unwrap_or_default();
        if kind.eq_ignore_ascii_case("text/html") {
            self.sink.item(Item::Inline(Inline::Html(content)))
        } else if kind.eq_ignore_ascii_case("text/plain") {
            self.sink.item(Item::Inline(Inline::Text(content)))
        } else {
            self.handle_embedded(metadata, content)
        }
    }

    /// `handleEmbedded`: a part handed to the embedded document extractor, an attachment unless
    /// its disposition says otherwise.
    fn handle_embedded(
        &mut self,
        mut metadata: TikaMetadata,
        content: Input<'static>,
    ) -> Result<(), S::Error> {
        if !ResourceType::is_set(&metadata) {
            ResourceType::Attachment.set(&mut metadata);
        }
        self.sink.item(Item::Embedded(content, metadata))
    }
}

/// What the `Content-Disposition` of `body` says of a part, added to its `metadata`.
fn disposition_metadata(
    body: &BodyDescriptor,
    metadata: &mut TikaMetadata,
) -> Result<(), Unported> {
    let Some(disposition) = body.disposition()? else {
        return Ok(());
    };
    if disposition.kind.is_empty() {
        return Ok(());
    }
    let mut description = disposition.kind.clone();
    if disposition.kind.eq_ignore_ascii_case("attachment") {
        ResourceType::Attachment.set(metadata);
    } else if disposition.kind.eq_ignore_ascii_case("inline") {
        ResourceType::Inline.set(metadata);
    }
    for (name, value) in disposition.parameters.iter() {
        description.push_str(&format!("; {name}=\"{value}\""));
        let property = if name.eq_ignore_ascii_case("creation-date") {
            Some("dcterms:created")
        } else if name.eq_ignore_ascii_case("modification-date") {
            Some("dcterms:modified")
        } else {
            None
        };
        if let Some(property) = property {
            if let Some(date) = parse_date(value)? {
                metadata.set(property, date.format());
            }
        }
    }
    if let Some(name) = body
        .content_type_parameter("name")
        .filter(|name| !is_blank(name))
    {
        metadata.set("tk:resource-name", name.to_owned());
    }
    if let Some(filename) = disposition.filename().filter(|name| !is_blank(name)) {
        metadata.set("tk:resource-name", filename.to_owned());
    }
    metadata.set("Content-Disposition", description);
    Ok(())
}

/// `detectInlineTextOrHtml` for a part whose type is known: text that is not an attachment.
fn is_inline_text(metadata: &TikaMetadata) -> bool {
    !ResourceType::Attachment.is_of(metadata)
        && metadata
            .get("Content-Type")
            .is_some_and(|kind| kind.starts_with("text"))
}

/// The sender's name and email address, each set when the mailbox has it. Tika takes them from
/// the address as it shows it, with a regular expression for the last thing that looks like an
/// address and the rest the name, so that a name holding an `@` loses part of itself (#77).
pub(super) fn set_name_and_email(mailbox: &Mailbox, metadata: &mut TikaMetadata) {
    if let Some(name) = mailbox.name() {
        metadata.set("message:from-name", name.to_owned());
    }
    if let Some(email) = mailbox.email() {
        metadata.set("message:from-email", email);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Pages;

    /// The items of `message`'s body, with its metadata read into `metadata`, and the first
    /// thing of it the port does not read as Tika does.
    fn collect(message: &[u8], metadata: &mut TikaMetadata) -> (Vec<Item>, Option<Unported>) {
        let mut collected = Collected {
            metadata,
            items: Vec::new(),
        };
        let unported = read(message, &Paging::new(Pages::IN_MEMORY), &mut collected).unwrap();
        (collected.items, unported)
    }

    #[test]
    fn takes_the_sender_from_its_mailbox() {
        let split = |text: &str| {
            let mut metadata = TikaMetadata::new();
            for address in flatten(parse_address_list(text).unwrap()) {
                if let Address::Mailbox(mailbox) = &address {
                    set_name_and_email(mailbox, &mut metadata);
                }
            }
            (
                metadata.get("message:from-name").map(str::to_owned),
                metadata.get("message:from-email").map(str::to_owned),
            )
        };
        assert_eq!(
            split("Ada Lovelace <ada@example.org>"),
            (Some("Ada Lovelace".into()), Some("ada@example.org".into()))
        );
        assert_eq!(
            split("ada@example.org"),
            (None, Some("ada@example.org".into()))
        );
        assert_eq!(
            split("\"ada@home\" <ada@example.org>"),
            (Some("ada@home".into()), Some("ada@example.org".into()))
        );
        assert_eq!(split("Name"), (Some("Name".into()), None));
    }

    #[test]
    fn reads_headers_and_parts_as_mail_content_handler_does() {
        let message = b"From: Ada <ada@example.org>\r\nSubject: =?utf-8?q?Caf=C3=A9?=\r\nDate: Tue, 05 Mar 2024 14:30:00 +0100\r\nDate: not a date\r\nX-Mailer: test\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nhello\r\n--b\r\nContent-Type: application/pdf; name=a.pdf\r\nContent-Disposition: attachment; filename=\"b.pdf\"\r\n\r\n%PDF\r\n--b--\r\n";
        let mut metadata = TikaMetadata::new();
        metadata.set("tk:resource-name", "notes.eml");
        let (items, unported) = collect(message, &mut metadata);
        assert_eq!(unported, None);
        assert_eq!(metadata.get("dc:title"), Some("Café"));
        assert_eq!(metadata.get("tk:resource-name"), Some("notes.eml"));
        assert_eq!(
            metadata.get("dcterms:created"),
            Some("2024-03-05T13:30:00Z")
        );
        assert_eq!(metadata.get("message:raw-header:X-Mailer"), Some("test"));
        assert_eq!(metadata.get("multipart:subtype"), Some("mixed"));
        assert_eq!(items.len(), 6);
        assert_eq!(
            items[1],
            Item::Inline(Inline::Text(Input::Borrowed(b"hello")))
        );
        let Item::Embedded(content, embedded) = &items[4] else {
            panic!("the attachment is embedded: {items:?}");
        };
        assert_eq!(content, b"%PDF");
        assert_eq!(embedded.get("tk:resource-name"), Some("b.pdf"));
        assert_eq!(
            embedded.get("Content-Disposition"),
            Some("attachment; filename=\"b.pdf\"")
        );
        assert!(ResourceType::Attachment.is_of(embedded));
    }

    #[test]
    fn keeps_the_best_alternative() {
        let message = b"Content-Type: multipart/alternative; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nplain\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>html</p>\r\n--b--\r\n";
        let (items, _) = collect(message, &mut TikaMetadata::new());
        assert_eq!(
            items,
            [Item::Inline(Inline::Html(Input::Borrowed(b"<p>html</p>")))]
        );
    }
}
