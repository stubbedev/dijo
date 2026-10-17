//! Jackcess's `OleUtil` as Tika's `handleOLE` reads an OLE column's value: the package header
//! Access wraps an object in, and the object as a link, a simple package of a file, a compound
//! document or other data.

use super::buffer::{Buffer, Thrown};
use crate::config::ParserConfigs;
use crate::formats::embedded::RESOURCE_NAME;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{office_document, Embedded, Handed};
use crate::java::{ascii, equals_ignore_case, utf16le};
use crate::ole2::Ole2;

const PACKAGE_SIGNATURE: i16 = 0x1C15;
const OLE_VERSION: i32 = 0x0501;
const SIMPLE_PACKAGE_TYPE: &str = "Package";
const COMPOUND_STORAGE_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const PACKAGE_STREAM_SIGNATURE: i16 = 0x02;
const PS_EMBEDDED_FILE: i32 = 0x03_0000;
const PS_LINKED_FILE: i32 = 0x01_0000;

/// What `handleOLE` writes of a value.
pub(super) enum Ole {
    /// Nothing: content of an unknown type, or a compound document Tika hands nothing of.
    Nothing,
    /// A link's path, as text.
    Link(String),
    /// A document handed on bare, its stream opened alone.
    Resource(Embedded),
    /// What `handleEmbeddedOfficeDoc` hands on of a compound document, in a package entry.
    Storage(Handed),
}

fn string_index(message: String) -> Thrown {
    Thrown::other(format!(
        "java.lang.StringIndexOutOfBoundsException: {message}"
    ))
}

/// `readStr`: `length` bytes from `offset` of the whole value decoded, the buffer then after
/// them, without one NUL at the end; an empty string throws where its last character is looked
/// at.
fn read_string(
    buffer: &mut Buffer<'_>,
    offset: i64,
    length: i64,
    decode: fn(&[u8]) -> String,
) -> Result<String, Thrown> {
    let bytes = buffer.bytes();
    let (Ok(start), Ok(count)) = (usize::try_from(offset), usize::try_from(length)) else {
        return Err(string_index(format!("offset {offset}, count {length}")));
    };
    let Some(slice) = bytes.get(start..start.saturating_add(count)) else {
        return Err(string_index(format!(
            "offset {offset}, count {length}, length {}",
            bytes.len()
        )));
    };
    let mut text = decode(slice);
    buffer.set_position(offset + length)?;
    if text.is_empty() {
        return Err(string_index("index -1, length 0".to_owned()));
    }
    if text.ends_with('\0') {
        text.pop();
    }
    Ok(text)
}

/// `readZeroTermStr`: up to and with the next NUL, or the limit.
fn read_zero_terminated(buffer: &mut Buffer<'_>) -> Result<String, Thrown> {
    let start = buffer.position() as i64;
    while buffer.has_remaining() {
        if buffer.u8()? == 0 {
            break;
        }
    }
    let length = buffer.position() as i64 - start;
    read_string(buffer, start, length, ascii)
}

/// `new ByteArrayInputStream(bytes, position, length)` read whole: from `position` to its
/// length's end or the array's, whichever comes first. A stream that starts before the array or
/// ends before it starts throws as Tika reads it.
fn stream(bytes: &[u8], position: i32, length: i32) -> Result<Vec<u8>, Thrown> {
    let count = i64::from(position.wrapping_add(length)).min(bytes.len() as i64);
    match (usize::try_from(position), usize::try_from(count)) {
        (Ok(start), Ok(end)) if start <= end => Ok(bytes[start..end].to_vec()),
        _ => Err(Thrown::unported(format!(
            "an OLE object's stream from {position} to {count}, which throws as Tika reads it"
        ))),
    }
}

/// `parseContent` and `handleOLE`: what Tika writes of an OLE value.
pub(super) fn read(value: &[u8], configs: &ParserConfigs) -> Result<Ole, Thrown> {
    let mut buffer = Buffer::new(value);
    if buffer.remaining() < 2 || buffer.i16()? != PACKAGE_SIGNATURE {
        return Ok(Ole::Nothing);
    }
    let header_size = buffer.i16()?;
    buffer.i32()?;
    let pretty_name_length = buffer.i16()?;
    let class_name_length = buffer.i16()?;
    let pretty_name_offset = buffer.i16()?;
    let class_name_offset = buffer.i16()?;
    buffer.i32()?;
    read_string(
        &mut buffer,
        i64::from(pretty_name_offset),
        i64::from(pretty_name_length),
        ascii,
    )?;
    read_string(
        &mut buffer,
        i64::from(class_name_offset),
        i64::from(class_name_length),
        ascii,
    )?;
    buffer.set_position(i64::from(header_size))?;
    let version = buffer.i32()?;
    buffer.i32()?;
    if version != OLE_VERSION {
        return Ok(Ole::Nothing);
    }
    let type_name_length = buffer.i32()?;
    let position = buffer.position() as i64;
    let type_name = read_string(&mut buffer, position, i64::from(type_name_length), ascii)?;
    buffer.take(8)?;
    let data_length = buffer.i32()?;
    let data_position = buffer.position() as i32;
    if equals_ignore_case(SIMPLE_PACKAGE_TYPE, &type_name) {
        return simple_package(value, &mut buffer, type_name, data_length);
    }
    if buffer.remaining() >= COMPOUND_STORAGE_SIGNATURE.len()
        && value[buffer.position()..].starts_with(&COMPOUND_STORAGE_SIGNATURE)
    {
        return compound(&stream(value, data_position, data_length)?, configs);
    }
    other(value, type_name, data_position, data_length)
}

/// `OtherContentImpl` handed on: the data block, typed by the type name.
fn other(value: &[u8], type_name: String, position: i32, length: i32) -> Result<Ole, Thrown> {
    let mut metadata = TikaMetadata::new();
    metadata.set("Content-Type", type_name);
    Ok(Ole::Resource(Embedded {
        data: stream(value, position, length)?,
        metadata,
    }))
}

/// `createSimplePackageContent`: an embedded file with its names, a link, or other data.
fn simple_package(
    value: &[u8],
    blob: &mut Buffer<'_>,
    type_name: String,
    block_length: i32,
) -> Result<Ole, Thrown> {
    let block_position = blob.position() as i32;
    let mut buffer = *blob;
    buffer.set_limit(i64::from(block_position.wrapping_add(block_length)))?;
    buffer.set_position(i64::from(block_position))?;
    if buffer.i16()? != PACKAGE_STREAM_SIGNATURE {
        return other(value, type_name, block_position, block_length);
    }
    let mut file_name = read_zero_terminated(&mut buffer)?;
    read_zero_terminated(&mut buffer)?;
    let package_type = buffer.i32()?;
    if package_type == PS_EMBEDDED_FILE {
        let local_path_length = buffer.i32()?;
        let position = buffer.position() as i64;
        read_string(&mut buffer, position, i64::from(local_path_length), ascii)?;
        let data_length = buffer.i32()?;
        let data_position = buffer.position() as i32;
        buffer.set_position(i64::from(data_length.wrapping_add(data_position)))?;
        let mut number = 0;
        while buffer.remaining() >= 4 {
            let length = buffer.i32()?;
            let position = buffer.position() as i64;
            let text = read_string(
                &mut buffer,
                position,
                i64::from(length.wrapping_mul(2)),
                utf16le,
            )?;
            if number == 1 {
                file_name = text;
            }
            number += 1;
        }
        let mut metadata = TikaMetadata::new();
        metadata.set(RESOURCE_NAME, file_name);
        metadata.set("Content-Type", type_name);
        return Ok(Ole::Resource(Embedded {
            data: stream(value, data_position, data_length)?,
            metadata,
        }));
    }
    if package_type == PS_LINKED_FILE {
        buffer.i16()?;
        return Ok(Ole::Link(read_zero_terminated(&mut buffer)?));
    }
    other(value, type_name, block_position, block_length)
}

/// `handleCompoundContent`: the compound document's root storage, as
/// `handleEmbeddedOfficeDoc` hands it on.
fn compound(data: &[u8], configs: &ParserConfigs) -> Result<Ole, Thrown> {
    let file = Ole2::open_stream(data).map_err(|unreadable| {
        Thrown::unported(format!(
            "an OLE object's compound document POI cannot open ({}), whose exception Tika \
             records",
            unreadable.0
        ))
    })?;
    match office_document(&file, file.root(), TikaMetadata::new(), None, configs) {
        Ok(Some(handed)) => Ok(Ole::Storage(handed)),
        Ok(None) => Ok(Ole::Nothing),
        Err(unported) => Err(Thrown::unported(unported.0)),
    }
}
