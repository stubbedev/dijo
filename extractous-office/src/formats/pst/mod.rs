//! Outlook data files (PST and OST) as Tika's `OutlookPSTParser` reads them with java-libpst:
//! each folder from the root as an `email-folder` div headed by its name, and each message in it
//! as an embedded document, which Tika's `PSTMailItemParser` writes: its properties as metadata,
//! its HTML body, or else its text, and its attachments, embedded messages included, as
//! embedded documents.
//!
//! java-libpst's reading is ported, as no Rust crate reads PST files as it does: the published
//! ones refuse what it reads (OST files, damaged headers) and cannot reach the attachments of an
//! embedded message. A file is read as Tika reads it, through its B-trees, block by block, so
//! that one of any size is read in place.
//!
//! A file with a message whose HTML body the native HTML parser leaves to Tika, or with a string
//! in a charset the port does not decode, is left to Tika.

mod file;
mod lzfu;
mod node;
mod object;
mod table;
#[cfg(test)]
mod tests;

use file::{Format, PstFile, Thrown};
use object::{Folder, Message, Undecodable};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{self, ResourceType, Wrapping};
use crate::formats::handlers::parse_inline_with;
use crate::formats::metadata::TikaMetadata;
use crate::formats::msg::extractor::normalized_class;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{html, Declined, Deferral, Failure, Footprint, Opened, Parser, Prepared};
use crate::java;
use crate::java::time::format_instant;

const PST: &str = "application/vnd.ms-outlook-pst";
const MAIL_ITEM: &str = "application/x-tika-pst-mail-item";
const MAIL_ITEM_PARSER: &str = "extractous::PSTMailItemParser";
const OST_2013: &str = "OST 2013 support not added yet. It will be when \
                        https://github.com/rjohnsondev/java-libpst/issues/60 is fixed.";
const SIZE_ESTIMATE_BASE: i64 = 100_000;
/// What `OutlookPSTParser` makes of the `WriteLimitReachedException` the text's length limit
/// throws: it catches every exception and throws it again as a `TikaException`, which fails the
/// extraction where the limit alone would end it.
const LIMIT_RETHROWN: &str = "org.apache.tika.exception.TikaException: the text reached the \
                              extraction's length limit, which OutlookPSTParser rethrows";

/// The metadata names Tika's PST parsers set.
mod name {
    pub(super) const CONTENT_TYPE: &str = "Content-Type";
    pub(super) const CONTENT_LENGTH: &str = "Content-Length";
    pub(super) const PARSER_OVERRIDE: &str = "tk:content-type-parser-override";
    pub(super) const IS_VALID: &str = "pst:is-valid";
    pub(super) const DESCRIPTOR_NODE_ID: &str = "pst:descriptor-node-id";
    pub(super) const IDENTIFIER: &str = "dc:identifier";
    pub(super) const TITLE: &str = "dc:title";
    pub(super) const SUBJECT: &str = "dc:subject";
    pub(super) const CREATOR: &str = "dc:creator";
    pub(super) const CREATED: &str = "dcterms:created";
    pub(super) const MODIFIED: &str = "dcterms:modified";
    pub(super) const COMMENTS: &str = "w:Comments";
    pub(super) const FROM: &str = "message:from";
    pub(super) const FROM_NAME: &str = "message:from-name";
    pub(super) const FROM_EMAIL: &str = "message:from-email";
    pub(super) const TO_DISPLAY_NAME: &str = "message:to-display-name";
    pub(super) const CC_DISPLAY_NAME: &str = "message:cc-display-name";
    pub(super) const BCC_DISPLAY_NAME: &str = "message:bcc-display-name";
    pub(super) const TO_EMAIL: &str = "message:to-email";
    pub(super) const CC_EMAIL: &str = "message:cc-email";
    pub(super) const BCC_EMAIL: &str = "message:bcc-email";
    pub(super) const CLIENT_SUBMIT_TIME: &str = "mapi:msg-client-submit-time";
    pub(super) const MESSAGE_DELIVERY_TIME: &str = "mapi:message-delivery-time";
    pub(super) const CREATION_TIME: &str = "mapi:creation-time";
    pub(super) const LAST_MODIFICATION_TIME: &str = "mapi:last-modification-time";
    pub(super) const IMPORTANCE: &str = "mapi:importance";
    pub(super) const PRIORITY: &str = "mapi:priority";
    pub(super) const IS_FLAGGED: &str = "mapi:is-flagged";
    pub(super) const MESSAGE_CLASS: &str = "mapi:message-class";
    pub(super) const MESSAGE_CLASS_RAW: &str = "mapi:message-class-raw";
    pub(super) const FROM_REPRESENTING_EMAIL: &str = "mapi:from-representing-email";
    pub(super) const FROM_REPRESENTING_NAME: &str = "mapi:from-representing-name";
}

/// The MAPI properties read by id.
mod property {
    pub(super) const IMPORTANCE: i32 = 0x0017;
    pub(super) const PRIORITY: i32 = 0x0026;
    pub(super) const MESSAGE_CLASS: i32 = 0x001A;
    pub(super) const CLIENT_SUBMIT_TIME: i32 = 0x0039;
    pub(super) const SENT_REPRESENTING_NAME: i32 = 0x0042;
    pub(super) const SENT_REPRESENTING_EMAIL_ADDRESS: i32 = 0x0065;
    pub(super) const SENDER_NAME: i32 = 0x0C1A;
    pub(super) const SENDER_EMAIL_ADDRESS: i32 = 0x0C1F;
    pub(super) const DISPLAY_BCC: i32 = 0x0E02;
    pub(super) const DISPLAY_CC: i32 = 0x0E03;
    pub(super) const DISPLAY_TO: i32 = 0x0E04;
    pub(super) const MESSAGE_DELIVERY_TIME: i32 = 0x0E06;
    pub(super) const INTERNET_MESSAGE_ID: i32 = 0x1035;
    pub(super) const COMMENT: i32 = 0x3004;
    pub(super) const CREATION_TIME: i32 = 0x3007;
    pub(super) const LAST_MODIFICATION_TIME: i32 = 0x3008;
}

/// Tika's `OutlookPSTParser`.
pub struct OutlookPstParser;

impl Parser for OutlookPstParser {
    fn name(&self) -> &'static str {
        "extractous::OutlookPSTParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.pst.OutlookPSTParser"
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &[(
            MAIL_ITEM_PARSER,
            "org.apache.tika.parser.microsoft.pst.PSTMailItemParser",
        )]
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[PST]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let file = PstFile::open(input).map_err(fails)?;
        if file.format() == Format::Unicode2013 {
            return Err(Declined::Fails(Error::ParseError(OST_2013.to_owned())));
        }
        let root = Folder::root(&file).map_err(fails)?;
        if let Some(reason) = scan_folder(&root) {
            return Err(Declined::Deferred(reason));
        }
        drop(root);
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            metadata.set(name::CONTENT_TYPE, PST);
            metadata.set(name::CONTENT_LENGTH, file.data().len().to_string());
            metadata.set(name::IS_VALID, "true");
            let root = Folder::root(&file).map_err(malformed)?;
            write_folder(&root, "/", xhtml).map_err(|failure| match failure {
                Failure::Stopped(Stop::LimitReached) => {
                    Failure::Malformed(LIMIT_RETHROWN.to_owned())
                }
                failure => failure,
            })
        }))
    }
}

fn fails(thrown: Thrown) -> Declined {
    Declined::Fails(Error::ParseError(thrown.to_string()))
}

fn malformed(thrown: Thrown) -> Failure {
    Failure::Malformed(thrown.to_string())
}

fn undecodable(Undecodable(reason): Undecodable) -> Failure {
    Failure::Malformed(format!(
        "{reason}, which the scan before the extraction missed"
    ))
}

/// What stops a message being written: a failure java-libpst throws, or text the port cannot
/// decode.
enum Unread {
    Thrown(Thrown),
    Undecodable(Undecodable),
}

impl From<Thrown> for Unread {
    fn from(thrown: Thrown) -> Self {
        Self::Thrown(thrown)
    }
}

impl From<Undecodable> for Unread {
    fn from(undecodable: Undecodable) -> Self {
        Self::Undecodable(undecodable)
    }
}

impl From<Unread> for Failure {
    fn from(unread: Unread) -> Self {
        match unread {
            Unread::Thrown(thrown) => malformed(thrown),
            Unread::Undecodable(reason) => undecodable(reason),
        }
    }
}

/// Why the native port leaves a file to Tika, found as the extraction will walk it: a message
/// whose HTML body the native HTML parser does not read, or text it cannot decode. A failure
/// ends the walk, as it ends the extraction.
fn scan_folder(folder: &Folder) -> Option<Deferral> {
    if folder.content_count() > 0 {
        let mut index = 0;
        while let Ok(Some(message)) = folder.child(index) {
            if let Some(reason) = scan_message(&message) {
                return Some(reason);
            }
            index += 1;
        }
    }
    if let Err(Undecodable(reason)) = folder.display_name() {
        return Some(reason);
    }
    if folder.has_subfolders() {
        for subfolder in folder.subfolders().ok()? {
            if let Some(reason) = scan_folder(&subfolder) {
                return Some(reason);
            }
        }
    }
    None
}

/// [`scan_folder`] of a message and the messages attached to it.
fn scan_message(message: &Message) -> Option<Deferral> {
    let read = match read_message(message) {
        Ok(read) => read,
        Err(Unread::Undecodable(Undecodable(reason))) => return Some(reason),
        Err(Unread::Thrown(_)) => return None,
    };
    if let Err(Unread::Undecodable(Undecodable(reason))) =
        write_metadata(message, &read, &mut TikaMetadata::new())
    {
        return Some(reason);
    }
    let html = read.html;
    if !java::is_blank(&html) {
        let units: Vec<u16> = html.encode_utf16().collect();
        if let Err(reason) = html::prepare_string(&units) {
            return Some(format!("a message's HTML body: {reason}"));
        }
    }
    for index in 0..message.attachment_count() {
        let Ok(mut attachment) = message.attachment(index) else {
            continue;
        };
        match attachment.embedded_message() {
            Ok(Some(embedded)) => {
                if let Some(reason) = scan_message(&embedded) {
                    return Some(reason);
                }
            }
            Ok(None) => {
                for name in [attachment.long_filename(), attachment.filename()] {
                    if let Err(Undecodable(reason)) = name {
                        return Some(reason);
                    }
                }
            }
            Err(_) => {}
        }
    }
    None
}

/// What `PSTMailItemParser` reads of a message's text.
struct ReadMessage {
    subject: String,
    html: String,
    body: String,
}

fn read_message(message: &Message) -> Result<ReadMessage, Unread> {
    Ok(ReadMessage {
        subject: message.subject()?,
        html: message.body_html()?,
        body: message.body()?,
    })
}

/// `parseFolder`: the folder's messages, then each subfolder in a div headed by its name.
fn write_folder(folder: &Folder, path: &str, xhtml: &mut Xhtml) -> Result<(), Failure> {
    if folder.content_count() > 0 {
        let mut index = 0;
        while let Some(message) = folder.child(index).map_err(malformed)? {
            let subject = message.subject().map_err(undecodable)?;
            let resource = format!("{subject}.msg");
            let internal_path = if path.ends_with('/') {
                format!("{path}{resource}")
            } else {
                format!("{path}/{resource}")
            };
            let mut metadata = TikaMetadata::new();
            metadata.set(name::CONTENT_TYPE, MAIL_ITEM);
            metadata.set(name::PARSER_OVERRIDE, MAIL_ITEM);
            metadata.set(embedded::INTERNAL_PATH, internal_path);
            metadata.set(embedded::RESOURCE_NAME, resource);
            metadata.set(
                name::CONTENT_LENGTH,
                estimate_size(&message).map_err(Failure::from)?.to_string(),
            );
            xhtml.parse_container_natively(
                &[],
                &metadata,
                Wrapping::PackageEntry,
                MAIL_ITEM_PARSER,
                |child| write_message(&message, child),
            )?;
            index += 1;
        }
    }
    if !folder.has_subfolders() {
        return Ok(());
    }
    for subfolder in folder.subfolders().map_err(malformed)? {
        xhtml.start_element_with("div", &[("class", "email-folder")])?;
        let name = subfolder.display_name().map_err(undecodable)?;
        xhtml.element("h1", &name)?;
        let subpath = if path.ends_with('/') {
            format!("{path}{name}")
        } else {
            format!("{path}/{}", folder.display_name().map_err(undecodable)?)
        };
        write_folder(&subfolder, &subpath, xhtml)?;
        xhtml.end_element("div")?;
    }
    Ok(())
}

/// `estimateSize`: the length of the message's texts, RTF included, plus a margin.
fn estimate_size(message: &Message) -> Result<i64, Unread> {
    let read = read_message(message)?;
    let rtf = match message.rtf_body() {
        Ok(rtf) => length(&rtf),
        Err(thrown) if thrown.is_checked() => 0,
        Err(thrown) => return Err(thrown.into()),
    };
    Ok(length(&read.body) + rtf + length(&read.html) + length(&read.subject) + SIZE_ESTIMATE_BASE)
}

/// `String.length`.
fn length(text: &str) -> i64 {
    text.encode_utf16().count() as i64
}

/// `PSTMailItemParser.parse`: the message's metadata, then its body and attachments in an
/// `embedded` div.
fn write_message(message: &Message, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let read = read_message(message)?;
    write_metadata(message, &read, xhtml.metadata_mut())?;
    let id = message
        .string(property::INTERNET_MESSAGE_ID)
        .map_err(undecodable)?;
    xhtml.start_element_with("div", &[("class", "embedded"), ("id", &id)])?;
    if java::is_blank(&read.html) {
        xhtml.start_element("p")?;
        xhtml.characters(&read.body)?;
        xhtml.end_element("p")?;
    } else {
        let units: Vec<u16> = read.html.encode_utf16().collect();
        let prepared = html::prepare_string(&units).map_err(|reason| {
            Failure::Malformed(format!(
                "a message's HTML body: {reason}, which the scan before the extraction missed"
            ))
        })?;
        let metadata = xhtml.metadata_mut().clone();
        let (metadata, result) = parse_inline_with(prepared, metadata, xhtml);
        *xhtml.metadata_mut() = metadata;
        result?;
    }
    for index in 0..message.attachment_count() {
        match write_attachment(message, index, xhtml) {
            Ok(()) => {}
            Err(Interrupted::Thrown(thrown)) => {
                embedded::record_stream_exception(xhtml.metadata_mut(), &thrown.to_string())
            }
            Err(Interrupted::Failure(failure)) => return Err(failure),
        }
    }
    xhtml.end_element("div")?;
    Ok(())
}

/// What ends an attachment: a failure java-libpst throws, which the message records, or a
/// failure of the extraction.
enum Interrupted {
    Thrown(Thrown),
    Failure(Failure),
}

impl From<Thrown> for Interrupted {
    fn from(thrown: Thrown) -> Self {
        Self::Thrown(thrown)
    }
}

impl From<Failure> for Interrupted {
    fn from(failure: Failure) -> Self {
        Self::Failure(failure)
    }
}

impl From<Stop> for Interrupted {
    fn from(stop: Stop) -> Self {
        Self::Failure(Failure::Stopped(stop))
    }
}

impl From<Undecodable> for Interrupted {
    fn from(reason: Undecodable) -> Self {
        Self::Failure(undecodable(reason))
    }
}

impl From<Unread> for Interrupted {
    fn from(unread: Unread) -> Self {
        match unread {
            Unread::Thrown(thrown) => Self::Thrown(thrown),
            Unread::Undecodable(reason) => reason.into(),
        }
    }
}

/// `parseMailAttachment`: an embedded message as an embedded document of its own, any other
/// attachment as its name and its data in an `embedded` div.
fn write_attachment(message: &Message, index: usize, xhtml: &mut Xhtml) -> Result<(), Interrupted> {
    let mut attachment = message.attachment(index)?;
    if let Some(embedded) = attachment.embedded_message()? {
        let size = estimate_size(&embedded)?.to_string();
        xhtml.metadata_mut().set(name::CONTENT_LENGTH, size.clone());
        let mut metadata = TikaMetadata::new();
        metadata.set(name::CONTENT_TYPE, MAIL_ITEM);
        metadata.set(name::PARSER_OVERRIDE, MAIL_ITEM);
        metadata.set(
            embedded::RESOURCE_NAME,
            format!("{}.msg", embedded.subject()?),
        );
        ResourceType::Attachment.set(&mut metadata);
        metadata.set(name::CONTENT_LENGTH, size);
        xhtml.parse_container_natively(
            &[],
            &metadata,
            Wrapping::PackageEntry,
            MAIL_ITEM_PARSER,
            |child| write_message(&embedded, child),
        )?;
        return Ok(());
    }
    let mut filename = attachment.long_filename()?;
    if filename.is_empty() {
        filename = attachment.filename()?;
    }
    xhtml.element("p", &filename)?;
    let mut metadata = TikaMetadata::new();
    metadata.set(embedded::RESOURCE_NAME, filename.clone());
    metadata.set(embedded::RELATIONSHIP_ID, filename.clone());
    ResourceType::Attachment.set(&mut metadata);
    metadata.set(name::CONTENT_LENGTH, attachment.size().to_string());
    xhtml.start_element_with("div", &[("class", "embedded"), ("id", &filename)])?;
    let data = attachment.data()?;
    xhtml.parse_embedded_opened(&data, &metadata, Wrapping::Bare, Opened::Alone)?;
    xhtml.end_element("div")?;
    Ok(())
}

/// `extractMetadata`.
fn write_metadata(
    message: &Message,
    read: &ReadMessage,
    metadata: &mut TikaMetadata,
) -> Result<(), Unread> {
    let string = |id| message.string(id);
    let id = string(property::INTERNET_MESSAGE_ID)?;
    let sender = string(property::SENDER_NAME)?;
    metadata.set(embedded::RESOURCE_NAME, format!("{}.msg", read.subject));
    metadata.set(embedded::RELATIONSHIP_ID, id.clone());
    ResourceType::Attachment.set(metadata);
    metadata.set(name::IDENTIFIER, id);
    metadata.set(name::TITLE, read.subject.clone());
    metadata.set(name::SUBJECT, read.subject.clone());
    metadata.set(name::FROM, sender.clone());
    metadata.set(name::CREATOR, sender.clone());
    let submitted = message.date(property::CLIENT_SUBMIT_TIME)?;
    let delivered = message.date(property::MESSAGE_DELIVERY_TIME)?;
    let date = submitted.or(delivered);
    set_date(metadata, name::CREATED, date);
    set_date(metadata, name::MODIFIED, date);
    set_date(metadata, name::CLIENT_SUBMIT_TIME, submitted);
    set_date(metadata, name::MESSAGE_DELIVERY_TIME, delivered);
    set_date(
        metadata,
        name::CREATION_TIME,
        message.date(property::CREATION_TIME)?,
    );
    set_date(
        metadata,
        name::LAST_MODIFICATION_TIME,
        message.date(property::LAST_MODIFICATION_TIME)?,
    );
    metadata.set(name::COMMENTS, string(property::COMMENT)?);
    metadata.set(
        name::DESCRIPTOR_NODE_ID,
        message.descriptor_node_id().to_string(),
    );
    let from_email = string(property::SENDER_EMAIL_ADDRESS)?;
    metadata.set(name::FROM_EMAIL, from_email.clone());
    metadata.set(name::TO_DISPLAY_NAME, string(property::DISPLAY_TO)?);
    metadata.set(name::CC_DISPLAY_NAME, string(property::DISPLAY_CC)?);
    metadata.set(name::BCC_DISPLAY_NAME, string(property::DISPLAY_BCC)?);
    metadata.set(
        name::IMPORTANCE,
        message.int_or(property::IMPORTANCE, 1).to_string(),
    );
    metadata.set(
        name::PRIORITY,
        message.int_or(property::PRIORITY, 0).to_string(),
    );
    metadata.set(name::IS_FLAGGED, message.is_flagged()?.to_string());
    let class = string(property::MESSAGE_CLASS)?;
    metadata.set(name::MESSAGE_CLASS, normalized_class(Some(&class)));
    metadata.set(name::MESSAGE_CLASS_RAW, class);
    metadata.set(name::FROM_EMAIL, from_email);
    metadata.set(
        name::FROM_REPRESENTING_EMAIL,
        string(property::SENT_REPRESENTING_EMAIL_ADDRESS)?,
    );
    metadata.set(name::FROM_NAME, sender);
    metadata.set(
        name::FROM_REPRESENTING_NAME,
        string(property::SENT_REPRESENTING_NAME)?,
    );
    for index in 0..message.recipient_count() {
        let recipient = match message.recipient(index) {
            Ok(recipient) => recipient,
            Err(thrown) if thrown.is_checked() => break,
            Err(thrown) => return Err(thrown.into()),
        };
        let (display_name, email) = match recipient.recipient_type() {
            1 => (name::TO_DISPLAY_NAME, name::TO_EMAIL),
            2 => (name::CC_DISPLAY_NAME, name::CC_EMAIL),
            3 => (name::BCC_DISPLAY_NAME, name::BCC_EMAIL),
            _ => continue,
        };
        metadata.add(display_name, recipient.display_name());
        metadata.add(email, recipient.email_address());
    }
    Ok(())
}

/// `set(property, date)`: the date in UTC, or no value where there is none.
fn set_date(metadata: &mut TikaMetadata, name: &str, millis: Option<i64>) {
    match millis {
        Some(millis) => metadata.set(name, format_instant(millis)),
        None => metadata.remove(name),
    }
}
