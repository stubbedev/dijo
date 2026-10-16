//! POI's `POIFSChunkParser` and `MAPIMessage`: the streams of a message's storage read into
//! chunks by the property id and type their names give, grouped into the message's own, its
//! named properties', its recipients' and its attachments'; and the 7-bit strings decoded with
//! the code page they are set to.

use std::collections::BTreeMap;

use super::properties::{read_properties, PropertySet};
use super::types::{property, Type, MULTIVALUED_FLAG};
use crate::formats::poi::{Read, Thrown};
use crate::java::parse_hex_int;
use crate::ole2::{EntryId, EntryKind, Ole2};
use crate::text::charset::Charset;

const PROPERTIES: &str = "__properties_version1.0";
const ATTACHMENT_PREFIX: &str = "__attach_version1.0_#";
const RECIPIENT_PREFIX: &str = "__recip_version1.0_#";
const NAME_ID: &str = "__nameid_version1.0";
const DEFAULT_ENCODING: &str = "CP1252";

/// A string chunk, `StringChunk`: its bytes and its text as decoded.
#[derive(Debug, Clone)]
pub(super) struct Text {
    pub(super) raw: Vec<u8>,
    pub(super) value: String,
}

impl Text {
    fn read(raw: Vec<u8>, kind: Type) -> Read<Text> {
        let value = decode(&raw, kind, DEFAULT_ENCODING)?;
        Ok(Text { raw, value })
    }

    /// `set7BitEncoding`: an ASCII string decoded again with the charset named.
    fn set_encoding(&mut self, kind: Type, encoding: &str) -> Read<()> {
        if kind == Type::ASCII_STRING {
            self.value = decode(&self.raw, kind, encoding)?;
        }
        Ok(())
    }
}

/// `parseString`: an ASCII string in the charset named, a Unicode one as UTF-16LE, without
/// its NULs.
fn decode(raw: &[u8], kind: Type, encoding: &str) -> Read<String> {
    let text = if kind == Type::ASCII_STRING {
        let name = if encoding == "ansi" {
            DEFAULT_ENCODING
        } else {
            encoding
        };
        let charset =
            Charset::for_name(name).ok_or_else(|| Thrown(format!("Unsupported charset {name}")))?;
        charset
            .decode(raw)
            .ok_or_else(|| Thrown(format!("the charset {name} does not decode natively")))?
    } else {
        Charset::named("UTF-16LE")
            .decode(raw)
            .expect("the JDK decodes UTF-16LE")
    };
    Ok(text.replace('\0', ""))
}

/// What a chunk holds.
#[derive(Debug, Clone)]
pub(super) enum Value {
    Text(Text),
    Bytes(Vec<u8>),
    /// `MessageSubmissionChunk`: the submission id, with the date it names.
    Submission(Submission),
    /// `DirectoryChunk`: an embedded message's storage.
    Directory(EntryId),
    Properties(PropertySet),
}

/// A message submission id, and the date in its `l=` part, as local calendar fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Submission {
    pub(super) id: String,
    /// Year, month, day, hour, minute and second.
    pub(super) date: Option<[i32; 6]>,
}

/// A chunk: a stream, or a storage of an embedded message, by the property it holds.
#[derive(Debug, Clone)]
pub(super) struct Chunk {
    pub(super) id: i32,
    pub(super) kind: Type,
    pub(super) value: Value,
}

impl Chunk {
    pub(super) fn text(&self) -> Option<&Text> {
        match &self.value {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    pub(super) fn bytes(&self) -> Option<&[u8]> {
        match &self.value {
            Value::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }
}

/// A group of chunks: the message's own, a recipient's, an attachment's or the named
/// properties'.
#[derive(Debug, Clone, Default)]
pub(super) struct Group {
    /// The chunks in the order they were recorded.
    pub(super) chunks: Vec<Chunk>,
}

impl Group {
    /// The last chunk of a property id recorded, as the group's field of that property keeps
    /// it.
    pub(super) fn last(&self, id: i32) -> Option<&Chunk> {
        self.chunks.iter().rev().find(|chunk| chunk.id == id)
    }

    /// The last chunk of a property that is a string, as the group's field of it keeps it.
    pub(super) fn string(&self, id: i32) -> Option<&Text> {
        self.last(id).and_then(Chunk::text)
    }

    /// The chunks of a property, in the order they were recorded.
    pub(super) fn all(&self, id: i32) -> impl Iterator<Item = &Chunk> {
        self.chunks.iter().filter(move |chunk| chunk.id == id)
    }

    /// `record`'s casts of the chunks of the properties the group keeps in fields: a string
    /// property's chunk that is no string, or a binary one's that is no bytes, makes POI throw.
    fn check(&self, strings: &[i32], binaries: &[i32]) -> Read<()> {
        for chunk in &self.chunks {
            let fits = if strings.contains(&chunk.id) {
                chunk.text().is_some()
            } else if binaries.contains(&chunk.id) {
                chunk.bytes().is_some()
            } else {
                true
            };
            if !fits {
                return Err(Thrown(format!(
                    "Property and type of chunk did not match, had property {}",
                    chunk.id
                )));
            }
        }
        Ok(())
    }

    /// The property set of the group, the last recorded.
    pub(super) fn properties(&self) -> Option<&PropertySet> {
        self.chunks
            .iter()
            .rev()
            .find_map(|chunk| match &chunk.value {
                Value::Properties(properties) => Some(properties),
                _ => None,
            })
    }
}

/// A recipient's chunks, by the number of its storage.
#[derive(Debug, Clone)]
pub(super) struct Recipient {
    number: i32,
    pub(super) group: Group,
}

impl Recipient {
    /// `getRecipientEmailAddress`.
    pub(super) fn email_address(&self) -> Read<Option<String>> {
        let group = &self.group;
        if let Some(smtp) = group.string(property::SMTP_ADDRESS) {
            return Ok(Some(smtp.value.clone()));
        }
        if let Some(email) = group.string(property::EMAIL_ADDRESS) {
            let email = &email.value;
            return Ok(Some(match email.find("/CN=") {
                Some(at) => email[at + 4..].to_owned(),
                None => email.clone(),
            }));
        }
        if let Some(name) = group.string(property::RECIPIENT_DISPLAY_NAME) {
            let name = &name.value;
            if name.contains('@') {
                if name.starts_with('\'') && name.ends_with('\'') && name.len() >= 2 {
                    return Ok(Some(name[1..name.len() - 1].to_owned()));
                }
                return Ok(Some(name.clone()));
            }
        }
        if let Some(bytes) = group.last(property::SEARCH_KEY).and_then(Chunk::bytes) {
            let search = decode(bytes, Type::ASCII_STRING, DEFAULT_ENCODING)?;
            if let Some(at) = search.find("SMTP:") {
                return Ok(Some(search[at + 5..].to_owned()));
            }
        }
        Ok(None)
    }
}

/// An attachment's chunks, by the name of its storage.
#[derive(Debug, Clone)]
pub(super) struct Attachment {
    pub(super) name: String,
    pub(super) entry: EntryId,
    pub(super) group: Group,
}

/// The string properties `Chunks.record` keeps in fields.
const MESSAGE_STRINGS: [i32; 12] = [
    property::MESSAGE_CLASS,
    property::INTERNET_MESSAGE_ID,
    property::RECEIVED_BY_ADDRTYPE,
    property::TRANSPORT_MESSAGE_HEADERS,
    property::CONVERSATION_TOPIC,
    property::SUBJECT,
    property::DISPLAY_TO,
    property::DISPLAY_CC,
    property::DISPLAY_BCC,
    property::SENDER_EMAIL_ADDRESS,
    property::SENDER_NAME,
    property::BODY,
];
/// The string properties `RecipientChunks.record` keeps in fields.
const RECIPIENT_STRINGS: [i32; 5] = [
    property::DISPLAY_NAME,
    property::RECIPIENT_DISPLAY_NAME,
    property::EMAIL_ADDRESS,
    property::SMTP_ADDRESS,
    property::ADDRTYPE,
];
/// The string and binary properties `AttachmentChunks.record` keeps in fields.
const ATTACHMENT_STRINGS: [i32; 9] = [
    property::ATTACH_EXTENSION,
    property::ATTACH_FILENAME,
    property::ATTACH_LONG_FILENAME,
    property::ATTACH_MIME_TAG,
    property::ATTACH_CONTENT_ID,
    property::DISPLAY_NAME,
    property::LANGUAGE,
    property::ATTACH_LONG_PATHNAME,
    property::ATTACH_CONTENT_LOCATION,
];
const ATTACHMENT_BINARIES: [i32; 2] = [property::ATTACH_RENDERING, property::RECORD_KEY];

/// A message, `MAPIMessage`.
#[derive(Debug, Clone)]
pub(super) struct Message {
    pub(super) main: Group,
    pub(super) name_ids: Option<Group>,
    pub(super) recipients: Vec<Recipient>,
    pub(super) attachments: Vec<Attachment>,
}

impl Message {
    /// `new MAPIMessage(directory)`.
    pub(super) fn read(file: &Ole2<'_>, directory: EntryId, embedded: bool) -> Read<Message> {
        let mut name_ids = None;
        let mut recipients = Vec::new();
        let mut attachments = Vec::new();
        for entry in file.children(directory) {
            if file.kind(entry) == EntryKind::Document {
                continue;
            }
            let name = file.name(entry);
            if name.starts_with(ATTACHMENT_PREFIX) {
                let group = read_group(file, entry, GroupKind::Storage)?;
                group.check(&ATTACHMENT_STRINGS, &ATTACHMENT_BINARIES)?;
                attachments.push(Attachment { group, name, entry });
            } else if name.starts_with(NAME_ID) {
                name_ids = Some(read_group(file, entry, GroupKind::Storage)?);
            } else if name.starts_with(RECIPIENT_PREFIX) {
                let number = name
                    .rfind('#')
                    .and_then(|at| parse_hex_int(&name[at + 1..]))
                    .unwrap_or(-1);
                let group = read_group(file, entry, GroupKind::Storage)?;
                group.check(&RECIPIENT_STRINGS, &[property::SEARCH_KEY])?;
                recipients.push(Recipient { number, group });
            }
        }
        let main = read_group(file, directory, GroupKind::Message { embedded })?;
        main.check(&MESSAGE_STRINGS, &[property::RTF_COMPRESSED])?;
        attachments.sort_by(|a, b| compare_utf16(&a.name, &b.name));
        recipients.sort_by_key(|recipient| recipient.number);
        Ok(Message {
            main,
            name_ids,
            recipients,
            attachments,
        })
    }

    /// `has7BitEncodingStrings`.
    pub(super) fn has_7_bit_strings(&self) -> bool {
        let groups = std::iter::once(&self.main)
            .chain(self.name_ids.iter())
            .chain(self.recipients.iter().map(|recipient| &recipient.group));
        groups
            .flat_map(|group| &group.chunks)
            .any(|chunk| chunk.kind == Type::ASCII_STRING && chunk.text().is_some())
    }

    /// `set7BitEncoding(charset)`: every 7-bit string decoded with the charset.
    pub(super) fn set_7_bit_encoding(&mut self, encoding: &str) -> Read<()> {
        let groups = std::iter::once(&mut self.main)
            .chain(self.name_ids.iter_mut())
            .chain(
                self.recipients
                    .iter_mut()
                    .map(|recipient| &mut recipient.group),
            );
        for group in groups {
            for chunk in &mut group.chunks {
                let kind = chunk.kind;
                if let Value::Text(text) = &mut chunk.value {
                    text.set_encoding(kind, encoding)?;
                }
            }
        }
        Ok(())
    }

    /// `getHeaders`: the transport headers' lines.
    pub(super) fn headers(&self) -> Option<Vec<String>> {
        self.main
            .string(property::TRANSPORT_MESSAGE_HEADERS)
            .map(|headers| split_lines(&headers.value))
    }
}

/// `String.split("\\r?\\n")`: the lines, trailing empty ones dropped.
fn split_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();
    while lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    if lines.len() == 1 && lines[0].is_empty() && !text.is_empty() {
        lines.clear();
    }
    lines
}

/// `String.compareTo`: by UTF-16 units.
fn compare_utf16(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupKind {
    /// The message's own chunks, of a message stored in another's storage or not.
    Message {
        embedded: bool,
    },
    Storage,
}

/// `processChunks`: the chunks of a storage's streams and of its directories named as
/// directory properties, each multi-valued property's values after the others.
fn read_group(file: &Ole2<'_>, directory: EntryId, kind: GroupKind) -> Read<Group> {
    let mut group = Group::default();
    let mut multi: BTreeMap<i32, BTreeMap<i32, Chunk>> = BTreeMap::new();
    for entry in file.children(directory) {
        let name = file.name(entry);
        let is_document = file.kind(entry) == EntryKind::Document;
        if !is_document && !name.ends_with("000D") {
            continue;
        }
        if name == PROPERTIES {
            if !is_document {
                return Err(Thrown::new("a properties storage that is no stream"));
            }
            let data = read(file, entry)?;
            let properties = match kind {
                GroupKind::Message { embedded } => read_properties(&data, Some(embedded)),
                GroupKind::Storage => read_properties(&data, None),
            };
            group.chunks.push(Chunk {
                id: -1,
                kind: Type::UNKNOWN,
                value: Value::Properties(properties),
            });
            continue;
        }
        let Some(primitive) = read_primitive(&name)? else {
            continue;
        };
        let Primitive {
            id,
            kind,
            multi_index,
        } = primitive;
        if id == property::MESSAGE_SUBMISSION_ID {
            if !is_document {
                return Err(Thrown::new(
                    "a message submission id that is no stream, which POI leaves unread",
                ));
            }
            let value = Value::Submission(submission(&read(file, entry)?));
            if multi_index.is_none() {
                group.chunks.push(Chunk { id, kind, value });
            }
            continue;
        }
        if kind == Type::BINARY && id == property::ATTACH_DATA {
            if multi_index.is_none() {
                let value = Value::Bytes(read(file, entry)?);
                group.chunks.push(Chunk { id, kind, value });
            }
            continue;
        }
        let is_text = kind == Type::ASCII_STRING || kind == Type::UNICODE_STRING;
        match multi_index {
            Some(MultiIndex::Length) => {}
            Some(MultiIndex::At(index)) => {
                if kind != Type::BINARY && !is_text {
                    continue;
                }
                if !is_document {
                    return Err(Thrown::new(
                        "a directory named as a value of a multi-valued property, which POI \
                         leaves unread",
                    ));
                }
                let data = read(file, entry)?;
                let value = if is_text {
                    Value::Text(Text::read(data, kind)?)
                } else {
                    Value::Bytes(data)
                };
                multi
                    .entry(id)
                    .or_default()
                    .insert(index, Chunk { id, kind, value });
            }
            None => {
                let value = if kind == Type::DIRECTORY && !is_document {
                    Value::Directory(entry)
                } else if kind == Type::BINARY && is_document {
                    Value::Bytes(read(file, entry)?)
                } else if is_text && is_document {
                    Value::Text(Text::read(read(file, entry)?, kind)?)
                } else {
                    continue;
                };
                group.chunks.push(Chunk { id, kind, value });
            }
        }
    }
    for values in multi.into_values() {
        group.chunks.extend(values.into_values());
    }
    Ok(group)
}

/// A stream read whole; POI swallows a failure to read one, which this port leaves to Tika.
fn read(file: &Ole2<'_>, entry: EntryId) -> Read<Vec<u8>> {
    file.read(entry)
        .map_err(|_| Thrown::new("a message stream POI cannot read"))
}

/// Where a multi-valued chunk goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MultiIndex {
    /// The stream of the values' lengths, which is read and not recorded.
    Length,
    At(i32),
}

struct Primitive {
    id: i32,
    kind: Type,
    multi_index: Option<MultiIndex>,
}

/// `readPrimitiveChunk`'s reading of an entry's name: `None` where it names no chunk.
fn read_primitive(name: &str) -> Read<Option<Primitive>> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let Some(split) = units.iter().rposition(|&unit| unit == u16::from(b'_')) else {
        return Ok(None);
    };
    if units.len() < 9 {
        return Ok(None);
    }
    let prefix = String::from_utf16_lossy(&units[..split + 1]);
    let ids: Vec<u16> = units[split + 1..].to_vec();
    if prefix == "Olk10SideProps" || prefix == "Olk10SideProps_" {
        return Ok(None);
    }
    if split + 8 > units.len() {
        return Err(Thrown(format!("Invalid chunk name {name}")));
    }
    if ids.len() < 8 {
        return Err(Thrown(format!("begin 4, end 8, length {}", ids.len())));
    }
    let hex = |units: &[u16]| parse_hex_int(&String::from_utf16_lossy(units));
    let (Some(id), Some(type_id)) = (hex(&ids[..4]), hex(&ids[4..8])) else {
        return Ok(None);
    };
    let multi = type_id & MULTIVALUED_FLAG != 0;
    let kind = Type(type_id & !MULTIVALUED_FLAG);
    let multi_index = multi.then(|| {
        let ids = String::from_utf16_lossy(&ids);
        if !ids.contains('-') {
            return MultiIndex::Length;
        }
        let index = &ids[ids.rfind('-').expect("a dash") + 1..];
        match index.parse::<i64>() {
            Ok(value) => MultiIndex::At((value & 0xFFFF_FFFF) as u32 as i32),
            Err(_) => MultiIndex::Length,
        }
    });
    Ok(Some(Primitive {
        id,
        kind,
        multi_index,
    }))
}

/// `MessageSubmissionChunk.readValue`: the id, and the date of its `l=` part.
fn submission(data: &[u8]) -> Submission {
    let id: String = data
        .iter()
        .map(|&byte| {
            if byte < 0x80 {
                byte as char
            } else {
                '\u{FFFD}'
            }
        })
        .collect();
    let mut date = None;
    for part in id.split(';') {
        if !part.starts_with("l=") {
            continue;
        }
        let Some(number_begin) = part.rfind('-') else {
            continue;
        };
        let Some(date_begin) = part[..number_begin].rfind('-') else {
            continue;
        };
        let text = &part[date_begin + 1..number_begin];
        let text = text.strip_suffix('Z').unwrap_or(text);
        if text.len() == 12 && text.bytes().all(|byte| byte.is_ascii_digit()) {
            let field = |at: usize| text[at..at + 2].parse::<i32>().expect("two digits");
            let year = field(0);
            date = Some([
                year + if year > 80 { 1900 } else { 2000 },
                field(2),
                field(4),
                field(6),
                field(8),
                field(10),
            ]);
        }
    }
    Submission { id, date }
}
