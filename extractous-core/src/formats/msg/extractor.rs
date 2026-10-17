//! Tika's `OutlookExtractor`: a message's sender, recipients, subject, dates, transport headers
//! and named properties as metadata, its body from its HTML, its compressed RTF, which may
//! encapsulate HTML, or its text, and its attachments as embedded documents.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

use super::chunks::{Chunk, Message, Submission};
use super::named;
use super::properties::Value;
use super::types::{property, Type};
use crate::config::ParserConfigs;
use crate::formats::embedded::{ResourceType, Wrapping, RESOURCE_NAME};
use crate::formats::handlers::parse_inline;
use crate::formats::mail::{decode_encoded_words, parse_date, MailDate};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{codepage_encoding, office_document, Embedded, Handed};
use crate::formats::poi::{Read, Thrown};
use crate::formats::xhtml::Xhtml;
use crate::formats::{html, rtf, Failure, Prepared, Unported};
use crate::java::is_blank;
use crate::java::time::{format_instant, CalendarFields, TimeZone};
use crate::java::{equals_ignore_case, trim};
use crate::ole2::Ole2;
use crate::text::charset::{self, Charset};

const MESSAGE_CLASSES: &str = include_str!("mapi_message_classes.properties");

/// The metadata names the extractor sets.
mod name {
    pub(super) const TITLE: &str = "dc:title";
    pub(super) const SUBJECT: &str = "dc:subject";
    pub(super) const DESCRIPTION: &str = "dc:description";
    pub(super) const CREATOR: &str = "dc:creator";
    pub(super) const CREATED: &str = "dcterms:created";
    pub(super) const MODIFIED: &str = "dcterms:modified";
    pub(super) const FROM: &str = "message:from";
    pub(super) const TO: &str = "message:to";
    pub(super) const CC: &str = "message:cc";
    pub(super) const BCC: &str = "message:bcc";
    pub(super) const FROM_NAME: &str = "message:from-name";
    pub(super) const FROM_EMAIL: &str = "message:from-email";
    pub(super) const RECIPIENT_ADDRESS: &str = "message:recipient-address";
    pub(super) const RAW_HEADER: &str = "message:raw-header:";
    pub(super) const CONVERSATION_TOPIC: &str = "mapi:conversation-topic";
    pub(super) const CONVERSATION_INDEX: &str = "mapi:conversation-index";
    pub(super) const INTERNET_MESSAGE_ID: &str = "mapi:internet-message-id";
    pub(super) const INTERNET_REFERENCES: &str = "mapi:internet-references";
    pub(super) const IN_REPLY_TO_ID: &str = "mapi:in-reply-to-id";
    pub(super) const MESSAGE_CLASS: &str = "mapi:message-class";
    pub(super) const MESSAGE_CLASS_RAW: &str = "mapi:message-class-raw";
    pub(super) const SENT_BY_SERVER_TYPE: &str = "mapi:sent-by-server-type";
    pub(super) const FROM_REPRESENTING_NAME: &str = "mapi:from-representing-name";
    pub(super) const FROM_REPRESENTING_EMAIL: &str = "mapi:from-representing-email";
    pub(super) const SUBMISSION_ID: &str = "mapi:msg-submission-id";
    pub(super) const SUBMISSION_ACCEPTED_AT_TIME: &str = "mapi:msg-submission-accepted-at-time";
    pub(super) const BODY_TYPES_PROCESSED: &str = "mapi:body-types-processed";
    pub(super) const CONTAINS_ENCAPSULATED_HTML: &str = "rtf:contains-encapsulated-html";
    pub(super) const ATTACH_LONG_PATH_NAME: &str = "mapi:attach:long-path-name";
    pub(super) const ATTACH_LONG_FILE_NAME: &str = "mapi:attach:long-file-name";
    pub(super) const ATTACH_FILE_NAME: &str = "mapi:attach:file-name";
    pub(super) const ATTACH_CONTENT_ID: &str = "mapi:attach:content-id";
    pub(super) const ATTACH_CONTENT_LOCATION: &str = "mapi:attach:content-location";
    pub(super) const ATTACH_DISPLAY_NAME: &str = "mapi:attach:display-name";
    pub(super) const ATTACH_EXTENSION: &str = "mapi:attach:extension";
    pub(super) const ATTACH_MIME: &str = "mapi:attach:mime";
    pub(super) const ATTACH_LANGUAGE: &str = "mapi:attach:language";
    pub(super) const ATTACH_FLAGS: &str = "mapi:attach:flags";
    pub(super) const ATTACH_HIDDEN: &str = "mapi:attach:hidden";
}

/// `LITERAL_TIME_PROPERTIES`: the time properties and the names Tika gives them.
const TIMES: [(i32, &str); 15] = [
    (property::CLIENT_SUBMIT_TIME, "mapi:msg-client-submit-time"),
    (property::CREATION_TIME, "mapi:creation-time"),
    (
        property::DEFERRED_DELIVERY_TIME,
        "mapi:deferred-delivery-time",
    ),
    (property::DELIVER_TIME, "mapi:deliver-time"),
    (property::EXPIRY_TIME, "mapi:expiry-time"),
    (
        property::LAST_MODIFICATION_TIME,
        "mapi:last-modification-time",
    ),
    (property::LATEST_DELIVERY_TIME, "mapi:latest-delivery-time"),
    (
        property::MESSAGE_DELIVERY_TIME,
        "mapi:message-delivery-time",
    ),
    (
        property::MESSAGE_DOWNLOAD_TIME,
        "mapi:message-download-time",
    ),
    (
        property::ORIGINAL_DELIVERY_TIME,
        "mapi:original-delivery-time",
    ),
    (property::ORIGINAL_SUBMIT_TIME, "mapi:original-submit-time"),
    (property::PROVIDER_SUBMIT_TIME, "mapi:provider-submit-time"),
    (property::RECEIPT_TIME, "mapi:receipt-time"),
    (property::REPLY_TIME, "mapi:reply-time"),
    (property::REPORT_TIME, "mapi:report-time"),
];
/// `PidTagAttachFlags`, its flag of an attachment rendered in the body, and
/// `PidTagAttachmentHidden`.
const ATTACH_FLAGS: u16 = 0x3714;
const RENDERED_IN_BODY: i64 = 0x4;
const ATTACHMENT_HIDDEN: u16 = 0x7FFE;
const INLINEABLE_TYPES: [&str; 7] = [
    "application/x-ms-wmz",
    "application/x-ms-emz",
    "application/x-msmetafile",
    "image/x-wmf",
    "image/x-emf",
    "image/wmf",
    "image/emf",
];
const PROPERTIES: &str = "__properties_version1.0";

/// The body as `handleBodyChunks` writes it.
enum Body {
    /// HTML, or the HTML an RTF body encapsulates, parsed with Tika's HTML parser.
    Html {
        prepared: Prepared<'static>,
        from_rtf: bool,
    },
    /// RTF, parsed inline by Tika's RTF parser.
    Rtf(Vec<u8>),
    Text(String),
    None,
}

/// A message read as far as writing it: the metadata Tika sets before the body, the body, and
/// the attachments.
pub(super) struct Extracted {
    message: Message,
    body: Body,
    attachments: Vec<Handed>,
}

/// `OutlookExtractor` of the message in `file`'s root, its strings decoded as Tika guesses.
pub(super) fn extract(
    file: &Ole2<'_>,
    mut message: Message,
    configs: &ParserConfigs,
) -> Result<Extracted, Unported> {
    if message.has_7_bit_strings() {
        guess_7_bit_encoding(&mut message).map_err(unported)?;
    }
    let mut scratch = TikaMetadata::new();
    write_metadata(&message, &mut scratch).map_err(unported)?;
    let (body, content_ids) = body(&message, configs)?;
    let attachments = attachments(file, &message, &content_ids, configs)?;
    Ok(Extracted {
        message,
        body,
        attachments,
    })
}

fn unported(Thrown(reason): Thrown) -> Unported {
    Unported(format!("POI or Tika throws on the message: {reason}"))
}

impl Extracted {
    /// Writes the message as `OutlookExtractor.parse` does.
    pub(super) fn write(self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        write_metadata(&self.message, xhtml.metadata_mut())
            .map_err(|Thrown(reason)| Failure::Malformed(reason))?;
        match self.body {
            Body::Html { prepared, from_rtf } => {
                parse_inline(prepared, xhtml)?;
                let metadata = xhtml.metadata_mut();
                if from_rtf {
                    metadata.add(name::BODY_TYPES_PROCESSED, "RTF");
                    metadata.set(name::CONTAINS_ENCAPSULATED_HTML, "true");
                } else {
                    metadata.add(name::BODY_TYPES_PROCESSED, "HTML");
                }
            }
            Body::Rtf(rtf) => {
                rtf::write_inline(&rtf, xhtml)?;
                let metadata = xhtml.metadata_mut();
                metadata.add(name::BODY_TYPES_PROCESSED, "RTF");
                metadata.set(name::CONTAINS_ENCAPSULATED_HTML, "false");
            }
            Body::Text(text) => {
                xhtml.element("p", &text)?;
                xhtml.metadata_mut().add(name::BODY_TYPES_PROCESSED, "TEXT");
            }
            Body::None => {}
        }
        for attachment in self.attachments {
            attachment.write(xhtml, Wrapping::PackageEntry)?;
        }
        Ok(())
    }
}

/// `set(property, value)`: a missing value removes the property.
fn set(metadata: &mut TikaMetadata, name: &str, value: Option<&str>) {
    match value {
        Some(value) => metadata.set(name, value),
        None => metadata.remove(name),
    }
}

fn string(message: &Message, id: i32) -> Option<&str> {
    message.main.string(id).map(|text| text.value.as_str())
}

/// The metadata `_parse` sets before the body: `handleFromTo`, `handleMessageInfo`, the named
/// properties, the recipients' addresses, the transport headers and `handleGeneralDates`.
fn write_metadata(message: &Message, metadata: &mut TikaMetadata) -> Read<()> {
    let headers = normalize_headers(message.headers().as_deref().unwrap_or_default())?;
    from_to(message, metadata)?;
    message_info(message, metadata)?;
    named::extract(message, metadata)?;
    if !message.recipients.is_empty() {
        for recipient in &message.recipients {
            if let Some(address) = recipient.email_address()? {
                metadata.add(name::RECIPIENT_ADDRESS, address);
            }
        }
    }
    for (key, values) in &headers {
        for value in values {
            metadata.add_prefixed(name::RAW_HEADER, key, value.clone());
        }
    }
    let date = match message_date(message)? {
        Some(date) => Some(format_instant(date)),
        None => header_date(&headers)?,
    };
    if let Some(date) = date {
        metadata.set(name::CREATED, date.clone());
        metadata.set(name::MODIFIED, date);
    }
    Ok(())
}

/// The date of the first `Date` transport header, for a message without a date of its own.
/// Tika looks for a header named `date:`, which no header's name is, so that it never falls back
/// to it (#112).
fn header_date(headers: &[(String, Vec<String>)]) -> Read<Option<String>> {
    let Some(value) = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("date"))
        .and_then(|(_, values)| values.first())
    else {
        return Ok(None);
    };
    let date = parse_date(value)
        .map_err(|_| Thrown::new("a Date header naming a time zone Tika looks up"))?;
    Ok(date.map(MailDate::format))
}

/// `handleFromTo`.
fn from_to(message: &Message, metadata: &mut TikaMetadata) -> Read<()> {
    let from = string(message, property::SENDER_NAME);
    set(metadata, name::CREATOR, from);
    set(metadata, name::FROM, from);
    set(metadata, name::TO, string(message, property::DISPLAY_TO));
    set(metadata, name::CC, string(message, property::DISPLAY_CC));
    set(metadata, name::BCC, string(message, property::DISPLAY_BCC));
    if let Some(server) = string(message, property::RECEIVED_BY_ADDRTYPE) {
        metadata.set(name::SENT_BY_SERVER_TYPE, server);
    }
    for (id, key) in [
        (property::SENDER_NAME, name::FROM_NAME),
        (
            property::SENT_REPRESENTING_NAME,
            name::FROM_REPRESENTING_NAME,
        ),
        (property::SENDER_EMAIL_ADDRESS, name::FROM_EMAIL),
        (
            property::SENT_REPRESENTING_EMAIL_ADDRESS,
            name::FROM_REPRESENTING_EMAIL,
        ),
    ] {
        if let Some(chunk) = message.main.all(id).next() {
            metadata.set(key, chunk_string(chunk));
        }
    }
    for recipient in &message.recipients {
        let kind = match recipient
            .group
            .properties()
            .and_then(|properties| properties.get(property::RECIPIENT_TYPE))
        {
            Some(Value::Long(1)) => "to",
            Some(Value::Long(2)) => "cc",
            Some(Value::Long(3)) => "bcc",
            _ => continue,
        };
        let text = |id| {
            recipient
                .group
                .last(id)
                .map(chunk_string)
                .unwrap_or_default()
        };
        // POI swaps these, taking the name from `PidTagRecipientDisplayName` and the display
        // name from `PidTagDisplayName` (#112).
        metadata.add(
            &format!("message:{kind}-name"),
            text(property::DISPLAY_NAME),
        );
        metadata.add(
            &format!("message:{kind}-display-name"),
            text(property::RECIPIENT_DISPLAY_NAME),
        );
        metadata.add(
            &format!("message:{kind}-email"),
            recipient.email_address()?.unwrap_or_default(),
        );
    }
    Ok(())
}

/// `Chunk.toString`: a string's text, a binary chunk's debugging form.
fn chunk_string(chunk: &Chunk) -> String {
    match (chunk.text(), chunk.bytes()) {
        (Some(text), _) => text.value.clone(),
        (None, Some(bytes)) => named::debug_string(bytes),
        _ => String::new(),
    }
}

/// `handleMessageInfo`.
fn message_info(message: &Message, metadata: &mut TikaMetadata) -> Read<()> {
    set(metadata, name::TITLE, string(message, property::SUBJECT));
    let topic = string(message, property::CONVERSATION_TOPIC);
    set(metadata, name::SUBJECT, topic);
    set(metadata, name::DESCRIPTION, topic);
    set(metadata, name::CONVERSATION_TOPIC, topic);
    if let Some(id) = string(message, property::INTERNET_MESSAGE_ID) {
        metadata.set(name::INTERNET_MESSAGE_ID, id);
    }
    let class = string(message, property::MESSAGE_CLASS);
    if let Some(class) = class {
        metadata.set(name::MESSAGE_CLASS_RAW, class);
    }
    metadata.set(name::MESSAGE_CLASS, normalized_class(class));
    if let Some(bytes) = message
        .main
        .all(property::CONVERSATION_INDEX)
        .next()
        .and_then(Chunk::bytes)
    {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        metadata.set(name::CONVERSATION_INDEX, hex);
    }
    for chunk in message.main.all(property::INTERNET_REFERENCES) {
        if let Some(text) = chunk.text() {
            metadata.add(name::INTERNET_REFERENCES, text.value.clone());
        }
    }
    if let Some(chunk) = message.main.all(property::IN_REPLY_TO_ID).next() {
        metadata.add(name::IN_REPLY_TO_ID, chunk_string(chunk));
    }
    if let Some(properties) = message.main.properties() {
        for (id, key) in TIMES {
            match properties.get(id) {
                None => {}
                Some(Value::Time(millis)) => metadata.set(key, format_instant(*millis)),
                Some(_) => {
                    return Err(Thrown(format!(
                        "the time property {id} is of another type, which Tika casts"
                    )))
                }
            }
        }
    }
    if let Some(submission) = submission(message) {
        metadata.set(name::SUBMISSION_ID, submission.id.clone());
        set(
            metadata,
            name::SUBMISSION_ACCEPTED_AT_TIME,
            submission_date(submission)?.map(format_instant).as_deref(),
        );
    }
    Ok(())
}

fn submission(message: &Message) -> Option<&Submission> {
    message
        .main
        .chunks
        .iter()
        .rev()
        .find_map(|chunk| match &chunk.value {
            super::chunks::Value::Submission(submission) => Some(submission),
            _ => None,
        })
}

/// The date of a submission id, in Java's default time zone.
fn submission_date(submission: &Submission) -> Read<Option<i64>> {
    let Some([year, month, day, hour, minute, second]) = submission.date else {
        return Ok(None);
    };
    let zone = TimeZone::java_default()
        .map_err(|reason| Thrown(format!("Java's default time zone cannot be read: {reason}")))?;
    Ok(Some(
        CalendarFields {
            year,
            month: month - 1,
            day,
            hour,
            minute,
            second,
            millisecond: 0,
        }
        .epoch_millis(zone),
    ))
}

/// `getMessageDate`: the submission's date where the message has a submission id, else the
/// first of its client submit, last modification and creation times.
fn message_date(message: &Message) -> Read<Option<i64>> {
    if let Some(submission) = submission(message) {
        return submission_date(submission);
    }
    let Some(properties) = message.main.properties() else {
        return Ok(None);
    };
    for id in [
        property::CLIENT_SUBMIT_TIME,
        property::LAST_MODIFICATION_TIME,
        property::CREATION_TIME,
    ] {
        match properties.get(id) {
            None => {}
            Some(Value::Time(millis)) => return Ok(Some(*millis)),
            Some(_) => {
                return Err(Thrown(format!(
                    "the time property {id} is of another type, which POI casts"
                )))
            }
        }
    }
    Ok(None)
}

/// `getNormalizedMessageClass`: the name Tika's table gives a message class, the last column of
/// its line. The read receipts' lines have three columns, and Tika takes the second, so that
/// they come out as `IPNRN` and `IPNNRN` (#112).
pub(in crate::formats) fn normalized_class(class: Option<&str>) -> &'static str {
    let Some(class) = class.filter(|class| !is_blank(class)) else {
        return "UNSPECIFIED";
    };
    let lower = crate::java::to_lower_case(class);
    MESSAGE_CLASSES
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .find_map(|line| {
            let mut columns = line.split_whitespace();
            let key = columns.next()?;
            let value = columns.last()?;
            (crate::java::to_lower_case(key) == lower).then_some(value)
        })
        .unwrap_or("UNKNOWN")
}

/// `normalizeHeaders`: each header line `key: value` and the lines continuing it, the values
/// of a key in order, decoded of their encoded words.
fn normalize_headers(rows: &[String]) -> Read<Vec<(String, Vec<String>)>> {
    let mut headers: Vec<(String, Vec<String>)> = Vec::new();
    let mut add = |key: &str, value: &str| -> Read<()> {
        let value = decode_encoded_words(value)
            .map_err(|_| Thrown::new("a header in a charset the native decoders do not decode"))?;
        match headers.iter_mut().find(|(known, _)| known == key) {
            Some((_, values)) => values.push(value),
            None => headers.push((key.to_owned(), vec![value])),
        }
        Ok(())
    };
    let mut pending = String::new();
    let mut last_key: Option<String> = None;
    let mut consecutive = 0;
    for row in rows {
        if let Some((key, value)) = header_line(row) {
            if let Some(key) = &last_key {
                add(key, &pending)?;
            }
            pending.clear();
            last_key = Some(trim(key).to_owned());
            pending.push_str(trim(value));
            consecutive = 0;
        } else {
            if consecutive > 0 {
                pending.push('\n');
            }
            pending.push_str(row);
        }
        consecutive += 1;
    }
    if !pending.is_empty() {
        if let Some(key) = &last_key {
            add(key, &pending)?;
        }
    }
    Ok(headers)
}

/// `HEADER_KEY_PAT`, `\A([\x21-\x39\x3B-\x7E]+):(.*?)\Z`: a key of printable characters but the
/// colon, and a value of no line terminator but a final one, which is left out.
fn header_line(row: &str) -> Option<(&str, &str)> {
    let key_end = row
        .find(|c: char| !matches!(c, '\x21'..='\x39' | '\x3B'..='\x7E'))
        .unwrap_or(row.len());
    if key_end == 0 || !row[key_end..].starts_with(':') {
        return None;
    }
    let value = &row[key_end + 1..];
    let is_terminator = |c: char| matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}');
    let value = if let Some(stripped) = value.strip_suffix("\r\n") {
        stripped
    } else {
        match value.chars().last() {
            Some(last) if is_terminator(last) => &value[..value.len() - last.len_utf8()],
            _ => value,
        }
    };
    if value.contains(is_terminator) {
        return None;
    }
    Some((&row[..key_end], value))
}

/// `guess7BitEncoding`: Tika decodes the 7-bit strings with the charset it detects in the body,
/// where it is sure enough of it, else with the one the message declares. Where the strings
/// read the same in every charset it may pick, this port reads them in any; where they do not,
/// the message is left to Tika.
fn guess_7_bit_encoding(message: &mut Message) -> Read<()> {
    let declared = declared_charset(message)?;
    let body = message
        .main
        .all(property::BODY_HTML)
        .filter_map(Chunk::bytes)
        .last()
        .map(<[u8]>::to_vec)
        .or_else(|| {
            message
                .main
                .string(property::BODY)
                .map(|text| text.raw.clone())
        });
    let mut candidates: Vec<String> = Vec::new();
    if let Some(body) = body.filter(|body| !body.is_empty()) {
        let detection = match &declared {
            Some(declared) => {
                charset::detect_hinted(&body, &format!("text/plain; charset={declared}"))
            }
            None => charset::detect(&body, None),
        };
        candidates.push(detection.decode_as.name().to_owned());
    }
    candidates.extend(declared);
    let candidates: Vec<String> = candidates
        .into_iter()
        .filter(|name| !equals_ignore_case(name, "utf-8") && Charset::for_name(name).is_some())
        .collect();
    let read = |message: &Message| -> Vec<String> {
        let groups = std::iter::once(&message.main)
            .chain(message.name_ids.iter())
            .chain(message.recipients.iter().map(|recipient| &recipient.group));
        groups
            .flat_map(|group| &group.chunks)
            .filter(|chunk| chunk.kind == Type::ASCII_STRING)
            .filter_map(|chunk| chunk.text().map(|text| text.value.clone()))
            .collect()
    };
    let default = read(message);
    for candidate in candidates {
        let mut decoded = message.clone();
        decoded.set_7_bit_encoding(&candidate)?;
        if read(&decoded) != default {
            return Err(Thrown(format!(
                "7-bit strings that read otherwise in {candidate}, which Tika may guess"
            )));
        }
    }
    Ok(())
}

/// `declaredCharset`: the charset of the message's code page or internet code page, else of
/// its `Content-Type` header.
fn declared_charset(message: &Message) -> Read<Option<String>> {
    if let Some(properties) = message.main.properties() {
        for id in [property::MESSAGE_CODEPAGE, property::INTERNET_CPID] {
            match properties.get(id) {
                None => {}
                Some(Value::Long(codepage)) => {
                    if let Some(name) = codepage_encoding(*codepage) {
                        if Charset::for_name(&name).is_some() {
                            return Ok(Some(name.into_owned()));
                        }
                    }
                }
                Some(_) => {
                    return Err(Thrown(format!(
                        "the code page property {id} is of another type, which Tika casts"
                    )))
                }
            }
        }
    }
    static CONTENT_TYPE: OnceLock<Regex> = OnceLock::new();
    let pattern = CONTENT_TYPE.get_or_init(|| {
        Regex::new(r#"(?is)\AContent-Type:.*?charset=["']?([^;'"]+)["']?\z"#)
            .expect("a valid pattern")
    });
    for header in message.headers().unwrap_or_default() {
        if header.starts_with("Content-Type") {
            if let Some(captures) = pattern.captures(&header) {
                return Ok(Some(captures[1].to_owned()));
            }
        }
    }
    Ok(None)
}

/// `handleBodyChunks`'s reading: the body and the content ids of the pictures it shows.
fn body(message: &Message, configs: &ParserConfigs) -> Result<(Body, HashSet<String>), Unported> {
    let mut content_ids = HashSet::new();
    let html = message.main.last(property::BODY_HTML);
    if let Some(data) = html.and_then(|chunk| match (chunk.bytes(), chunk.text()) {
        (Some(bytes), _) => Some(bytes.to_vec()),
        (None, Some(text)) => Some(text.raw.clone()),
        _ => None,
    }) {
        let prepared = html::prepare(&data, &TikaMetadata::new())
            .map_err(|reason| Unported(format!("the HTML body defers: {reason}")))?;
        image_content_ids(&String::from_utf8_lossy(&data), &mut content_ids);
        return Ok((
            Body::Html {
                prepared,
                from_rtf: false,
            },
            content_ids,
        ));
    }
    if let Some(chunk) = message.main.last(property::RTF_COMPRESSED) {
        let data = chunk
            .bytes()
            .expect("the group checked the RTF body is binary");
        if !data.is_empty() {
            let rtf = rtf::decompress(data).map_err(unported)?;
            if let Some(html) = rtf::decapsulate(&rtf)? {
                let prepared = html::prepare_string(&html).map_err(|reason| {
                    Unported(format!(
                        "the HTML the RTF body encapsulates defers: {reason}"
                    ))
                })?;
                image_content_ids(&String::from_utf16_lossy(&html), &mut content_ids);
                return Ok((
                    Body::Html {
                        prepared,
                        from_rtf: true,
                    },
                    content_ids,
                ));
            }
            if let Some(reason) = rtf::deferral(&rtf, configs) {
                return Err(Unported(format!("the RTF body defers: {reason}")));
            }
            image_content_ids(&String::from_utf8_lossy(&rtf), &mut content_ids);
            return Ok((Body::Rtf(rtf), content_ids));
        }
    }
    if let Some(chunk) = message.main.last(property::BODY) {
        let text = chunk
            .text()
            .expect("the group checked the text body is a string")
            .value
            .clone();
        static CID: OnceLock<Regex> = OnceLock::new();
        let pattern =
            CID.get_or_init(|| Regex::new(r"\[cid:([^\]]{0,1000})\]").expect("a valid pattern"));
        for captures in pattern.captures_iter(&text) {
            content_ids.insert(captures[1].to_owned());
        }
        return Ok((Body::Text(text), content_ids));
    }
    Ok((Body::None, content_ids))
}

/// `extractContentIdNames`: the content ids of each `<img>`'s `src="cid:..."`.
fn image_content_ids(html: &str, content_ids: &mut HashSet<String>) {
    static PATTERNS: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (image, source) = PATTERNS.get_or_init(|| {
        (
            Regex::new(r"(?s)<img ([^>]{0,1000})>").expect("a valid pattern"),
            Regex::new(r#"(?s)src="cid:([^"]{0,1000})""#).expect("a valid pattern"),
        )
    });
    for captures in image.captures_iter(html) {
        for cid in source.captures_iter(&captures[1]) {
            content_ids.insert(trim(&cid[1]).to_owned());
        }
    }
}

/// The attachments as `_parse` hands them on: each one's data, then its storage of an embedded
/// object.
fn attachments(
    file: &Ole2<'_>,
    message: &Message,
    content_ids: &HashSet<String>,
    configs: &ParserConfigs,
) -> Result<Vec<Handed>, Unported> {
    let mut embedded = Vec::new();
    for attachment in &message.attachments {
        let mut metadata = attachment_metadata(file, attachment, content_ids);
        let group = &attachment.group;
        let filename = [
            name::ATTACH_LONG_FILE_NAME,
            name::ATTACH_DISPLAY_NAME,
            name::ATTACH_FILE_NAME,
        ]
        .iter()
        .find_map(|key| metadata.get(key).filter(|value| !is_blank(value)))
        .map(str::to_owned);
        let mime = metadata.get(name::ATTACH_MIME).map(str::to_owned);
        if let Some(data) = group
            .all(property::ATTACH_DATA)
            .filter_map(Chunk::bytes)
            .last()
        {
            if let Some(filename) = &filename {
                metadata.set(RESOURCE_NAME, filename.clone());
            }
            if let Some(mime) = &mime {
                metadata.set("Content-Type", mime.clone());
            }
            embedded.push(Handed::Document(Embedded {
                data: data.to_vec(),
                metadata: metadata.clone(),
            }));
        }
        let directory = group
            .all(property::ATTACH_DATA)
            .filter_map(|chunk| match chunk.value {
                super::chunks::Value::Directory(entry) => Some(entry),
                _ => None,
            })
            .last();
        if let Some(directory) = directory {
            if let Some(document) =
                office_document(file, directory, metadata, filename.as_deref(), configs)?
            {
                embedded.push(document);
            }
        }
    }
    Ok(embedded)
}

/// `updateAttachmentMetadata`.
fn attachment_metadata(
    file: &Ole2<'_>,
    attachment: &super::chunks::Attachment,
    content_ids: &HashSet<String>,
) -> TikaMetadata {
    let group = &attachment.group;
    let mut metadata = TikaMetadata::new();
    for (id, key) in [
        (property::ATTACH_LONG_PATHNAME, name::ATTACH_LONG_PATH_NAME),
        (property::ATTACH_LONG_FILENAME, name::ATTACH_LONG_FILE_NAME),
        (property::ATTACH_FILENAME, name::ATTACH_FILE_NAME),
        (
            property::ATTACH_CONTENT_LOCATION,
            name::ATTACH_CONTENT_LOCATION,
        ),
        (property::DISPLAY_NAME, name::ATTACH_DISPLAY_NAME),
        (property::ATTACH_EXTENSION, name::ATTACH_EXTENSION),
        (property::ATTACH_MIME_TAG, name::ATTACH_MIME),
        (property::LANGUAGE, name::ATTACH_LANGUAGE),
    ] {
        if let Some(text) = group.string(id) {
            if !is_blank(&text.value) {
                metadata.set(key, text.value.clone());
            }
        }
    }
    let properties = attachment_properties(file, attachment);
    let flags = properties
        .iter()
        .rev()
        .find(|(id, _)| *id == ATTACH_FLAGS)
        .map(|&(_, value)| value);
    if let Some(flags) = flags {
        metadata.set(name::ATTACH_FLAGS, (flags as i32).to_string());
    }
    if let Some(&(_, hidden)) = properties
        .iter()
        .rev()
        .find(|(id, _)| *id == ATTACHMENT_HIDDEN)
    {
        metadata.set(name::ATTACH_HIDDEN, ((hidden as i32) != 0).to_string());
    }
    let content_id = group
        .string(property::ATTACH_CONTENT_ID)
        .filter(|text| !is_blank(&text.value))
        .map(|text| trim(&text.value).to_owned());
    if let Some(content_id) = &content_id {
        metadata.set(name::ATTACH_CONTENT_ID, content_id.clone());
        let inline = content_ids.contains(content_id)
            || (flags.is_some_and(|flags| flags & RENDERED_IN_BODY != 0)
                && metadata.get(name::ATTACH_MIME).is_some_and(is_inlineable));
        if inline {
            ResourceType::Inline.set(&mut metadata);
        }
    }
    metadata
}

/// `isInlineableMimeType`.
fn is_inlineable(media_type: &str) -> bool {
    if is_blank(media_type) {
        return false;
    }
    let lower = trim(&crate::java::to_lower_case(media_type)).to_owned();
    lower.starts_with("image/") || INLINEABLE_TYPES.contains(&lower.as_str())
}

/// `readAttachmentProperties`: the 32-bit, boolean and 64-bit properties of an attachment's
/// properties stream, by id in the order read; none where Tika fails to read them.
fn attachment_properties(
    file: &Ole2<'_>,
    attachment: &super::chunks::Attachment,
) -> Vec<(u16, i64)> {
    let Some(entry) = file.child(attachment.entry, PROPERTIES) else {
        return Vec::new();
    };
    let Ok(data) = file.read(entry) else {
        return Vec::new();
    };
    let mut properties = Vec::new();
    if data.len() < 8 {
        return properties;
    }
    let mut offset = 8;
    while offset + 16 <= data.len() {
        let field = &data[offset..offset + 16];
        let kind = u16::from_le_bytes([field[0], field[1]]);
        let id = u16::from_le_bytes([field[2], field[3]]);
        let value = match kind {
            0x0003 => Some(i64::from(u32::from_le_bytes(
                field[8..12].try_into().expect("four bytes"),
            ))),
            0x000B => Some(i64::from(u16::from_le_bytes([field[8], field[9]]))),
            0x0014 => Some(i64::from_le_bytes(
                field[8..16].try_into().expect("eight bytes"),
            )),
            _ => None,
        };
        if let Some(value) = value {
            properties.push((id, value));
        }
        offset += 16;
    }
    properties
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_header_lines_as_tikas_pattern_does() {
        assert_eq!(header_line("Subject: hi"), Some(("Subject", " hi")));
        assert_eq!(header_line("Subject: hi\r"), Some(("Subject", " hi")));
        assert_eq!(header_line("Sub ject: hi"), None);
        assert_eq!(header_line(": hi"), None);
        assert_eq!(header_line(" continued"), None);
        assert_eq!(header_line("A: b\rc"), None);
    }

    #[test]
    fn joins_continued_headers() {
        let rows: Vec<String> = ["To: a", "  b", "Subject: =?UTF-8?Q?caf=C3=A9?=", "To: c"]
            .iter()
            .map(|row| row.to_string())
            .collect();
        let headers = normalize_headers(&rows).unwrap();
        assert_eq!(
            headers,
            [
                ("To".to_owned(), vec!["a\n  b".to_owned(), "c".to_owned()]),
                ("Subject".to_owned(), vec!["café".to_owned()]),
            ]
        );
    }

    #[test]
    fn normalizes_message_classes_as_tika_does() {
        assert_eq!(normalized_class(Some("ipm.note")), "NOTE");
        assert_eq!(
            normalized_class(Some("REPORT.IPM.Note.IPNRN")),
            "READ_RECEIPT"
        );
        assert_eq!(
            normalized_class(Some("REPORT.IPM.Note.IPNNRN")),
            "NOT_READ_RECEIPT"
        );
        assert_eq!(normalized_class(Some("IPM.Other")), "UNKNOWN");
        assert_eq!(normalized_class(None), "UNSPECIFIED");
    }

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn unicode(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn property(kind: u16, id: u16, value: &[u8]) -> Vec<u8> {
        let mut bytes = kind.to_le_bytes().to_vec();
        bytes.extend(id.to_le_bytes());
        bytes.extend([6, 0, 0, 0]);
        let mut padded = value.to_vec();
        padded.resize(8, 0);
        bytes.extend(padded);
        bytes
    }

    /// A message from Ann to Bob, sent on 1 January 2020, with a text body.
    fn message() -> Vec<u8> {
        use crate::ole2::{write, Node, Storage};
        let document = |name: &str, data: Vec<u8>| (units(name), Node::Document(data));
        let mut properties = vec![0; 32];
        let sent = (1_577_836_800_000i64 + 11_644_473_600_000) * 10_000;
        properties.extend(property(0x0040, 0x0039, &sent.to_le_bytes()));
        let mut recipient_properties = vec![0; 8];
        recipient_properties.extend(property(0x0003, 0x0C15, &1i32.to_le_bytes()));
        let recipient = Storage {
            class_id: [0; 16],
            entries: vec![
                document("__substg1.0_3001001F", unicode("Bob")),
                document("__substg1.0_39FE001F", unicode("bob@example.com")),
                document("__properties_version1.0", recipient_properties),
            ],
        };
        write(&Storage {
            class_id: [0; 16],
            entries: vec![
                document("__substg1.0_0037001F", unicode("Hello")),
                document("__substg1.0_0C1A001F", unicode("Ann")),
                document("__substg1.0_0E04001F", unicode("Bob")),
                document("__substg1.0_1000001F", unicode("Hi Bob")),
                document("__substg1.0_001A001F", unicode("IPM.Note")),
                document("__properties_version1.0", properties),
                (
                    units("__recip_version1.0_#00000000"),
                    Node::Storage(recipient),
                ),
            ],
        })
        .unwrap()
    }

    #[test]
    fn reads_a_message_as_tikas_extractor_does() {
        let data = message();
        let file = Ole2::open(&data).unwrap();
        let read = Message::read(&file, file.root(), false).unwrap();
        let mut metadata = TikaMetadata::new();
        write_metadata(&read, &mut metadata).unwrap();
        assert_eq!(metadata.get(name::TITLE), Some("Hello"));
        assert_eq!(metadata.get(name::CREATOR), Some("Ann"));
        assert_eq!(metadata.get(name::TO), Some("Bob"));
        assert_eq!(metadata.get(name::MESSAGE_CLASS), Some("NOTE"));
        assert_eq!(metadata.get("message:to-email"), Some("bob@example.com"));
        assert_eq!(metadata.get("message:to-name"), Some("Bob"));
        assert_eq!(metadata.get("message:to-display-name"), Some(""));
        assert_eq!(metadata.get(name::CREATED), Some("2020-01-01T00:00:00Z"));
        assert_eq!(
            metadata.get("mapi:msg-client-submit-time"),
            Some("2020-01-01T00:00:00Z")
        );
        let configs = ParserConfigs::default();
        let (body, _) = body(&read, &configs).unwrap();
        assert!(matches!(body, Body::Text(text) if text == "Hi Bob"));
    }

    #[test]
    fn falls_back_to_the_date_header() {
        use crate::ole2::{write, Node, Storage};
        let document = |name: &str, data: Vec<u8>| (units(name), Node::Document(data));
        let data = write(&Storage {
            class_id: [0; 16],
            entries: vec![
                document("__substg1.0_0037001F", unicode("Hello")),
                document(
                    "__substg1.0_007D001F",
                    unicode("Subject: Hello\r\nDate: Tue, 05 Mar 2024 14:30:00 +0100\r\n"),
                ),
                document("__substg1.0_001A001F", unicode("REPORT.IPM.Note.IPNRN")),
                document("__properties_version1.0", vec![0; 32]),
            ],
        })
        .unwrap();
        let file = Ole2::open(&data).unwrap();
        let read = Message::read(&file, file.root(), false).unwrap();
        let mut metadata = TikaMetadata::new();
        write_metadata(&read, &mut metadata).unwrap();
        assert_eq!(metadata.get(name::CREATED), Some("2024-03-05T13:30:00Z"));
        assert_eq!(metadata.get(name::MODIFIED), Some("2024-03-05T13:30:00Z"));
        assert_eq!(metadata.get(name::MESSAGE_CLASS), Some("READ_RECEIPT"));
    }

    #[test]
    fn finds_the_content_ids_of_pictures() {
        let mut ids = HashSet::new();
        image_content_ids(
            r#"<img src="cid: one ">x<img alt=a src="cid:two"><p src="cid:no">"#,
            &mut ids,
        );
        assert_eq!(ids, HashSet::from(["one".to_owned(), "two".to_owned()]));
    }
}
