//! TNEF messages (`winmail.dat`), as Tika's `TNEFParser` reads them with POI's HMEF: the
//! message's subject as its title, then its compressed RTF body and each attachment handed to
//! the embedded document extractor. POI reads the whole message before Tika writes anything, so a
//! message it throws on fails before any output, but for an attachment without data, which fails
//! the parse once the documents before it are written.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{Wrapping, RESOURCE_NAME};
use crate::formats::metadata::office::RESOURCE_NAME_EXTENSION_INFERRED;
use crate::formats::metadata::TikaMetadata;
use crate::formats::msg::types::{property, Type, MULTIVALUED_FLAG};
use crate::formats::poi::{thrown, Read, Thrown};
use crate::formats::xhtml::Xhtml;
use crate::formats::{rtf, Decline, Failure, Footprint, Opened, Parser, Prepared};
use crate::java;
use crate::text::charset::Charset;

/// `HMEFMessage.HEADER_SIGNATURE`.
const SIGNATURE: i32 = 0x223e_9f78;
/// `TNEFAttribute`'s largest record.
const MAX_ATTRIBUTE_LENGTH: i32 = 20_000_000;
/// `MAPIAttribute`'s largest record.
const MAX_MAPI_LENGTH: i32 = 1_000_000;
/// `MAPIAttribute`'s most values of one property.
const MAX_MAPI_VALUES: i32 = 10_000;
/// The resource name prefix of an attachment without a name.
const UNNAMED_PREFIX: &str = "embedded";

/// The levels an attribute is read at, `TNEFProperty.LEVEL_*`.
mod level {
    pub(super) const MESSAGE: u8 = 0x01;
    pub(super) const ATTACHMENT: u8 = 0x02;
}

/// The ids of the TNEF attributes Tika's path reads, `TNEFProperty.ID_*`.
mod id {
    pub(super) const ATTACH_DATA: u16 = 0x800F;
    pub(super) const ATTACH_TITLE: u16 = 0x8010;
    pub(super) const ATTACH_RENDER_DATA: u16 = 0x9002;
    pub(super) const MAPI_PROPERTIES: u16 = 0x9003;
    pub(super) const ATTACHMENT: u16 = 0x9005;
}

/// The types of TNEF attributes POI reads into values, `TNEFProperty.TYPE_*`.
mod kind {
    pub(super) const STRING: u16 = 0x0001;
    pub(super) const TEXT: u16 = 0x0002;
    pub(super) const DATE: u16 = 0x0003;
}

/// The id of the first MAPI property of a named one.
const FIRST_NAMED_ID: u16 = 0x8000;

/// Tika's `TNEFParser`.
pub struct TnefParser;

impl Parser for TnefParser {
    fn name(&self) -> &'static str {
        "extractous::TNEFParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.TNEFParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.ms-tnef",
            "application/ms-tnef",
            "application/x-tnef",
        ]
    }

    /// It reads the message's attributes in place and hands each attachment on from where it
    /// lies. What it holds is what POI holds: the MAPI properties' values, each at most a
    /// megabyte, the text attributes, each at most 20 MB, and the decompressed RTF body.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let message = Message::read(input)
            .map_err(|Thrown(reason)| Decline::Fails(Error::ParseError(reason)))?;
        Ok(Box::new(move |xhtml| message.write(xhtml)))
    }
}

/// A MAPI property's value as POI reads it into a `MAPIAttribute`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MapiValue {
    /// A `MAPIStringAttribute`.
    Text(String),
    /// A `MAPIRtfAttribute`: the decompressed RTF.
    Rtf(Vec<u8>),
    /// Any other attribute, which POI reads no string from.
    Other,
}

/// One value of a MAPI property.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MapiAttribute {
    id: u16,
    value: MapiValue,
}

/// `MAPIStringAttribute.getAsString`, for a property that is not compressed RTF.
fn mapi_string(attributes: &[MapiAttribute], id: i32) -> Option<&str> {
    let attribute = attributes
        .iter()
        .find(|attribute| i32::from(attribute.id) == id)?;
    match &attribute.value {
        MapiValue::Text(text) => Some(text),
        MapiValue::Rtf(_) | MapiValue::Other => None,
    }
}

/// A TNEF attribute's value as POI reads it into a `TNEFAttribute`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    /// A `TNEFStringAttribute`.
    Text(String),
    /// A `TNEFMAPIAttribute`'s properties.
    Mapi(Vec<MapiAttribute>),
    /// Any other attribute.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Attribute<'a> {
    id: u16,
    data: &'a [u8],
    value: Value,
}

/// `Attachment`: the attributes read at the attachment level until the next one starts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Attachment<'a> {
    attributes: Vec<Attribute<'a>>,
    mapi: Vec<MapiAttribute>,
}

impl<'a> Attachment<'a> {
    fn add(&mut self, attribute: Attribute<'a>) {
        if let Value::Mapi(mapi) = &attribute.value {
            self.mapi.extend(mapi.iter().cloned());
        }
        self.attributes.push(attribute);
    }

    fn attribute(&self, id: u16) -> Option<&Attribute<'a>> {
        self.attributes.iter().find(|attribute| attribute.id == id)
    }

    /// `getFilename`.
    fn filename(&self) -> Option<&str> {
        match &self.attribute(id::ATTACH_TITLE)?.value {
            Value::Text(text) => Some(text),
            Value::Mapi(_) | Value::Other => None,
        }
    }

    /// The name Tika gives the attachment, and whether it made it up: the long file name, else
    /// the file name, else one numbered by the attachments without a name before it, with the
    /// attachment's extension.
    fn name(&self, unnamed: &mut usize) -> (String, bool) {
        let named = mapi_string(&self.mapi, property::ATTACH_LONG_FILENAME)
            .filter(|name| !name.is_empty())
            .or_else(|| self.filename().filter(|name| !name.is_empty()));
        if let Some(name) = named {
            return (name.to_owned(), false);
        }
        let extension = mapi_string(&self.mapi, property::ATTACH_EXTENSION).unwrap_or_default();
        let name = format!("{UNNAMED_PREFIX}-{unnamed}{extension}");
        *unnamed += 1;
        (name, true)
    }

    /// `getContents`.
    fn contents(&self) -> Result<&'a [u8], Failure> {
        self.attribute(id::ATTACH_DATA)
            .map(|attribute| attribute.data)
            .ok_or_else(|| {
                Failure::Malformed(
                    "IllegalArgumentException: Attachment corrupt - no Data section".to_owned(),
                )
            })
    }
}

/// `HMEFMessage`: the MAPI properties of the message and its attachments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Message<'a> {
    mapi: Vec<MapiAttribute>,
    attachments: Vec<Attachment<'a>>,
}

impl<'a> Message<'a> {
    /// `new HMEFMessage(input)`.
    fn read(input: &'a [u8]) -> Read<Self> {
        let mut stream = Stream { data: input, at: 0 };
        let signature = stream.int()?;
        if signature != SIGNATURE {
            return thrown(format!(
                "IllegalArgumentException: TNEF signature not detected in file, expected \
                 {SIGNATURE} but got {signature}"
            ));
        }
        stream.ushort()?;
        let mut message = Message {
            mapi: Vec::new(),
            attachments: Vec::new(),
        };
        while let Some(level) = stream.read() {
            match level {
                level::MESSAGE => {
                    if let Value::Mapi(mapi) = Attribute::read(&mut stream)?.value {
                        message.mapi.extend(mapi);
                    }
                }
                level::ATTACHMENT => {
                    let attribute = Attribute::read(&mut stream)?;
                    if message.attachments.is_empty() || attribute.id == id::ATTACH_RENDER_DATA {
                        message.attachments.push(Attachment::default());
                    }
                    message
                        .attachments
                        .last_mut()
                        .expect("an attachment was just added")
                        .add(attribute);
                }
                b'\r' | b'\n' => {}
                other => return thrown(format!("IllegalStateException: Unhandled level {other}")),
            }
        }
        Ok(message)
    }

    /// `getSubject`.
    fn subject(&self) -> Option<&str> {
        mapi_string(&self.mapi, property::CONVERSATION_TOPIC)
    }

    /// The RTF of `getMessageMAPIAttribute(RTF_COMPRESSED)`, when that is compressed RTF.
    fn rtf(&self) -> Option<&[u8]> {
        let attribute = self
            .mapi
            .iter()
            .find(|attribute| i32::from(attribute.id) == property::RTF_COMPRESSED)?;
        match &attribute.value {
            MapiValue::Rtf(rtf) => Some(rtf),
            MapiValue::Text(_) | MapiValue::Other => None,
        }
    }

    /// `TNEFParser.parse` once POI has read the message.
    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        if let Some(subject) = self.subject().filter(|subject| !subject.is_empty()) {
            let metadata = xhtml.metadata_mut();
            metadata.set("dc:title", subject);
            metadata.set("dc:subject", subject);
        }
        if let Some(rtf) = self.rtf() {
            embed(xhtml, "message.rtf", Some("application/rtf"), false, rtf)?;
        }
        let mut unnamed = 0;
        for attachment in &self.attachments {
            let (name, inferred) = attachment.name(&mut unnamed);
            embed(xhtml, &name, None, inferred, attachment.contents()?)?;
        }
        Ok(())
    }
}

/// `handleEmbedded`: the document handed to the embedded document extractor in a stream
/// opened on its bytes alone.
fn embed(
    xhtml: &mut Xhtml,
    name: &str,
    content_type: Option<&str>,
    inferred: bool,
    contents: &[u8],
) -> Result<(), Failure> {
    let mut metadata = TikaMetadata::new();
    metadata.set(RESOURCE_NAME, name);
    if let Some(content_type) = content_type {
        metadata.set("Content-Type", content_type);
    }
    if inferred {
        metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
    }
    xhtml.parse_embedded_opened(contents, &metadata, Wrapping::PackageEntry, Opened::Alone)
}

impl<'a> Attribute<'a> {
    /// `TNEFAttribute.create`.
    fn read(stream: &mut Stream<'a>) -> Read<Self> {
        let id = stream.ushort()?;
        let kind = stream.ushort()?;
        let length = stream.int()?;
        allocatable(length, MAX_ATTRIBUTE_LENGTH)?;
        let data = stream.take(length as usize);
        if data.len() < length as usize {
            return thrown("BufferUnderrunException: the attribute's data ends the file");
        }
        stream.ushort()?;
        let value = if id == id::MAPI_PROPERTIES || id == id::ATTACHMENT {
            Value::Mapi(read_mapi(data)?)
        } else if kind == kind::TEXT {
            Value::Text(without_trailing_nul(utf16le(data)))
        } else if kind == kind::STRING {
            Value::Text(without_trailing_nul(java::latin1(data)))
        } else if kind == kind::DATE {
            if data.len() != 8 && data.len() != 14 {
                return thrown(format!(
                    "IllegalArgumentException: Invalid date, found {} bytes",
                    data.len()
                ));
            }
            Value::Other
        } else {
            Value::Other
        };
        Ok(Attribute { id, data, value })
    }
}

/// `MAPIAttribute.create`: the MAPI properties in the data of a TNEF attribute.
fn read_mapi(data: &[u8]) -> Read<Vec<MapiAttribute>> {
    let mut stream = Stream { data, at: 0 };
    let count = stream.int()?;
    let mut attributes = Vec::new();
    for _ in 0..count.max(0) {
        let type_and_multi = i32::from(stream.ushort()?);
        let id = stream.ushort()?;
        let multi = type_and_multi & MULTIVALUED_FLAG != 0;
        let type_id = if multi {
            type_and_multi - MULTIVALUED_FLAG
        } else {
            type_and_multi
        };
        let variable = [
            Type::ASCII_STRING,
            Type::UNICODE_STRING,
            Type::BINARY,
            Type::DIRECTORY,
        ]
        .contains(&Type(type_id));
        let kind = Type::built_in(type_id).unwrap_or(Type(type_id));
        if id >= FIRST_NAMED_ID {
            if stream.take(16).is_empty() {
                return thrown("IOException: Not enough data to read guid");
            }
            if stream.int()? == 0 {
                stream.int()?;
            } else {
                let length = stream.int()?;
                allocatable(length, MAX_MAPI_LENGTH)?;
                if length > 0 && stream.take(length as usize).is_empty() {
                    return thrown(format!(
                        "IOException: Not enough data to read {length} bytes for attribute name"
                    ));
                }
                if length < 2 {
                    return thrown(format!("IllegalArgumentException: Illegal length {length}"));
                }
                stream.skip_to_boundary(length)?;
            }
        }
        let values = if multi || variable {
            let values = stream.int()?;
            if !(0..=MAX_MAPI_VALUES).contains(&values) {
                return thrown(format!(
                    "RecordFormatException: Tried to allocate an array of length {values}"
                ));
            }
            values
        } else {
            1
        };
        if kind == Type::NULL && values > 1 {
            return thrown("IOException: Placeholder/NULL arrays aren't supported.");
        }
        for _ in 0..values {
            let length = if kind.is_fixed_length() {
                kind.length()
            } else if variable {
                stream.int()?
            } else {
                return thrown(format!(
                    "IllegalArgumentException: Unknown type {type_id:#x}"
                ));
            };
            allocatable(length, MAX_MAPI_LENGTH)?;
            let read = stream.take(length as usize);
            if length > 0 && read.is_empty() {
                return thrown(format!(
                    "IOException: Not enough data to read {length} bytes of attribute value"
                ));
            }
            let mut value = read.to_vec();
            value.resize(length as usize, 0);
            stream.skip_to_boundary(length)?;
            let value = if kind == Type::UNICODE_STRING {
                MapiValue::Text(without_trailing_nul(utf16le(&value)))
            } else if kind == Type::ASCII_STRING {
                MapiValue::Text(without_trailing_nul(
                    Charset::named("windows-1252")
                        .decode(&value)
                        .expect("the JDK decoders decode windows-1252"),
                ))
            } else if kind == Type::APP_TIME || kind == Type::TIME {
                MapiValue::Other
            } else if i32::from(id) == property::RTF_COMPRESSED {
                MapiValue::Rtf(rtf::decompress(&value)?)
            } else {
                MapiValue::Other
            };
            attributes.push(MapiAttribute { id, value });
        }
    }
    Ok(attributes)
}

/// `IOUtils.safelyAllocate`'s check of a record's length.
fn allocatable(length: i32, max: i32) -> Read<()> {
    if length < 0 || length > max {
        return thrown(format!(
            "RecordFormatException: Tried to allocate an array of length {length}, but the \
             maximum length for this record type is {max}"
        ));
    }
    Ok(())
}

/// `StringUtil.getFromUnicodeLE`.
fn utf16le(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The string without one NUL at its end, as POI's string attributes take it.
fn without_trailing_nul(mut text: String) -> String {
    if text.ends_with('\0') {
        text.pop();
    }
    text
}

/// The bytes POI reads from, which throw as `LittleEndian`'s reads do where they end.
struct Stream<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Stream<'a> {
    /// `InputStream.read`: the next byte, if any.
    fn read(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    /// As many of the next `length` bytes as there are.
    fn take(&mut self, length: usize) -> &'a [u8] {
        let end = self.at.saturating_add(length).min(self.data.len());
        let taken = &self.data[self.at..end];
        self.at = end;
        taken
    }

    fn exactly<const N: usize>(&mut self) -> Read<[u8; N]> {
        let taken = self.take(N);
        taken.try_into().map_err(|_| {
            Thrown::new(format!(
                "BufferUnderrunException: {N} bytes wanted where {} are left",
                taken.len()
            ))
        })
    }

    /// `LittleEndian.readInt`.
    fn int(&mut self) -> Read<i32> {
        self.exactly().map(i32::from_le_bytes)
    }

    /// `LittleEndian.readUShort`.
    fn ushort(&mut self) -> Read<u16> {
        self.exactly().map(u16::from_le_bytes)
    }

    /// `MAPIAttribute.skipToBoundary`: past the padding to the next four bytes after a value of
    /// `length` bytes.
    fn skip_to_boundary(&mut self, length: i32) -> Read<()> {
        let padding = (4 - length % 4) % 4;
        if self.take(padding as usize).len() != padding as usize {
            return thrown(format!(
                "IOException: tried to skip {padding} but the data ended"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
