//! metadata-extractor's `HeifMetadataReader`: the boxes of a HEIF file read in one pass, the file
//! type into a `HeifDirectory`, and once a picture handler box is found, the item properties
//! into it too, with the ICC profiles of colour boxes and the Exif of the media data's items.
//! The item info and location boxes are also read by Tika's `HeifXmp` to find an XMP item.
//!
//! metadata-extractor puts the item properties in a second `HeifDirectory`, ends the `meta` box
//! 4 bytes past its end, by its version and flags, and reads an item where the stream stands
//! when that is past the item's offset (#114).

use std::collections::HashMap;

use super::descriptions::{base, Described};
use super::{exif, icc, Directory, Kind, Metadata, Unported, Value};

const TAG_MAJOR_BRAND: i32 = 1;
const TAG_MINOR_VERSION: i32 = 2;
const TAG_COMPATIBLE_BRANDS: i32 = 3;
const TAG_IMAGE_WIDTH: i32 = 4;
const TAG_IMAGE_HEIGHT: i32 = 5;
const TAG_IMAGE_ROTATION: i32 = 6;
const TAG_BITS_PER_CHANNEL: i32 = 7;

/// `HeifDescriptor.getDescription`.
pub(super) fn describe(directory: &Directory, tag: i32) -> Described {
    Ok(match tag {
        TAG_IMAGE_WIDTH | TAG_IMAGE_HEIGHT => directory
            .string(tag)
            .map(|pixels| format!("{pixels} pixels")),
        TAG_IMAGE_ROTATION => directory
            .integer(tag)
            .map(|quarters| format!("{} degrees", quarters.wrapping_mul(90))),
        _ => base(directory, tag),
    })
}

/// Why reading stopped: an `IOException`, which the loop reading the boxes catches, or a runtime
/// exception, which Java throws out of the reader and the port does not read past.
#[derive(Debug)]
pub(crate) enum Stop {
    Io,
    Unported(Unported),
}

impl From<Unported> for Stop {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

fn unported(what: &str) -> Stop {
    Stop::Unported(Unported::new(format!(
        "a HEIF {what}, on which metadata-extractor throws a runtime exception"
    )))
}

/// A `SequentialReader`: big-endian reads from a position, failing at the end of the data with
/// the position moved there, as a `StreamReader` over the file leaves it.
#[derive(Debug, Clone)]
pub(crate) struct Sequential<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Sequential<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    fn available(&self) -> usize {
        self.data.len() - self.position
    }

    pub(crate) fn bytes(&mut self, count: usize) -> Result<&'a [u8], Stop> {
        if count > self.available() {
            self.position = self.data.len();
            return Err(Stop::Io);
        }
        let bytes = &self.data[self.position..self.position + count];
        self.position += count;
        Ok(bytes)
    }

    /// The `count` bytes at `offset` of the data, wherever the reader stands.
    fn at(&self, offset: i64, count: i32) -> Result<&'a [u8], Stop> {
        let count = usize::try_from(count).map_err(|_| unported("length below zero"))?;
        usize::try_from(offset)
            .ok()
            .and_then(|start| self.data.get(start..start.checked_add(count)?))
            .ok_or(Stop::Io)
    }

    /// `getBytes` of an `int` count, which Java cannot allocate when negative.
    fn int_bytes(&mut self, count: i32) -> Result<&'a [u8], Stop> {
        let count = usize::try_from(count).map_err(|_| unported("length below zero"))?;
        self.bytes(count)
    }

    fn skip(&mut self, count: i64) -> Result<(), Stop> {
        match usize::try_from(count) {
            Ok(count) if count <= self.available() => {
                self.position += count;
                Ok(())
            }
            _ => {
                self.position = self.data.len();
                Err(Stop::Io)
            }
        }
    }

    fn u8(&mut self) -> Result<u8, Stop> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Stop> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn i32(&mut self) -> Result<i32, Stop> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u32(&mut self) -> Result<u32, Stop> {
        Ok(self.i32()? as u32)
    }

    fn i64(&mut self) -> Result<i64, Stop> {
        let bytes = self.bytes(8)?;
        Ok(i64::from_be_bytes(bytes.try_into().expect("eight bytes")))
    }

    /// `getString` in the default charset, UTF-8.
    fn string(&mut self, count: usize) -> Result<String, Stop> {
        Ok(String::from_utf8_lossy(self.bytes(count)?).into_owned())
    }

    /// `getNullTerminatedString(maxLength, UTF_8, false)`: up to the first zero, which is read,
    /// within `max` bytes.
    fn null_terminated(&mut self, max: i64) -> Result<String, Stop> {
        let max =
            usize::try_from(max as i32).map_err(|_| unported("string of a negative length"))?;
        let mut bytes = Vec::new();
        while bytes.len() < max {
            let byte = self.u8()?;
            if byte == 0 {
                break;
            }
            bytes.push(byte);
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// `Box`: the size, which a size of 1 gives in 64 bits and a size of 0 makes -1, and the type.
#[derive(Debug, Clone)]
pub(crate) struct Box {
    pub(crate) size: i64,
    pub(crate) kind: String,
}

impl Box {
    pub(crate) fn read(reader: &mut Sequential) -> Result<Self, Stop> {
        let mut size = i64::from(reader.u32()?);
        let kind = reader.string(4)?;
        if size == 1 {
            size = reader.i64()?;
        } else if size == 0 {
            size = -1;
        }
        if kind == "uuid" {
            reader.string(16)?;
        }
        Ok(Self { size, kind })
    }

    /// `(int) box.size - 8`: the length of the payload `processBox` reads.
    fn payload_length(&self) -> i32 {
        (self.size as i32).wrapping_sub(8)
    }
}

/// `FullBox`'s version, after which its three bytes of flags are skipped.
fn full_box(reader: &mut Sequential) -> Result<u8, Stop> {
    let version = reader.u8()?;
    reader.bytes(3)?;
    Ok(version)
}

/// An `ItemInfoEntry`'s type, which only version 2 and later entries have.
#[derive(Debug, Clone)]
pub(crate) struct ItemInfo {
    pub(crate) item_type: Option<String>,
}

/// `ItemInfoBox`'s entries by item id, a later entry replacing an earlier one of its id.
pub(crate) fn item_info_box(
    reader: &mut Sequential,
    _box: &Box,
) -> Result<HashMap<i64, ItemInfo>, Stop> {
    let version = full_box(reader)?;
    let count = if version == 0 {
        i64::from(reader.u16()?)
    } else {
        i64::from(reader.u32()?)
    };
    let mut entries = HashMap::new();
    let mut index = 1i64;
    while index <= count {
        let entry_box = Box::read(reader)?;
        let mut payload = Sequential::new(reader.int_bytes(entry_box.payload_length())?);
        let (id, entry) = item_info_entry(&mut payload, &entry_box)?;
        entries.insert(id, entry);
        index += 1;
    }
    Ok(entries)
}

fn item_info_entry(reader: &mut Sequential, entry: &Box) -> Result<(i64, ItemInfo), Stop> {
    let version = full_box(reader)?;
    let remaining = |reader: &Sequential| entry.size - reader.position() as i64 - 8;
    let mut id = 0i64;
    let mut item_type = None;
    if version == 0 || version == 1 {
        id = i64::from(reader.u16()?);
        reader.u16()?;
        reader.null_terminated(remaining(reader))?;
        reader.null_terminated(remaining(reader))?;
        if remaining(reader) > 0 {
            reader.null_terminated(remaining(reader))?;
        }
    }
    if version == 1 && entry.size - 28 >= 4 {
        reader.string(4)?;
    }
    if version >= 2 {
        if version == 2 {
            id = i64::from(reader.u16()?);
        } else if version == 3 {
            id = i64::from(reader.u32()?);
        }
        reader.u16()?;
        let kind = reader.string(4)?;
        reader.null_terminated(remaining(reader))?;
        if kind == "mime" {
            reader.null_terminated(remaining(reader))?;
            if remaining(reader) > 0 {
                reader.null_terminated(remaining(reader))?;
            }
        } else if kind == "uri " {
            let length = usize::try_from(remaining(reader) as i32)
                .map_err(|_| unported("URI item of a negative length"))?;
            reader.string(length)?;
        }
        item_type = Some(kind);
    }
    Ok((id, ItemInfo { item_type }))
}

/// An `ItemLocationBox.Extent`: the item, and its offset with the base offset added.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Extent {
    pub(crate) item: i64,
    pub(crate) offset: i64,
    pub(crate) length: i64,
}

/// `ItemLocationBox`'s extents, in a `TreeSet` by offset that keeps the first of an offset.
pub(crate) fn item_location_box(reader: &mut Sequential, _box: &Box) -> Result<Vec<Extent>, Stop> {
    let version = full_box(reader)?;
    let holder = reader.u8()?;
    let offset_size = holder >> 4;
    let length_size = holder & 0x0F;
    let holder = reader.u8()?;
    let base_offset_size = holder >> 4;
    let index_size = if version == 1 || version == 2 {
        holder & 0x0F
    } else {
        0
    };
    let count = match version {
        0 | 1 => i64::from(reader.u16()?),
        2 => i64::from(reader.u32()?),
        _ => 0,
    };
    let mut extents: Vec<Extent> = Vec::new();
    let mut item = 0i64;
    for _ in 0..count {
        match version {
            0 | 1 => item = i64::from(reader.u16()?),
            2 => item = i64::from(reader.u32()?),
            _ => {}
        }
        if version == 1 || version == 2 {
            reader.u16()?;
        }
        reader.u16()?;
        let base_offset = match base_offset_size {
            4 => i64::from(reader.i32()?),
            8 => reader.i64()?,
            _ => 0,
        };
        let extent_count = reader.u16()?;
        for _ in 0..extent_count {
            if (version == 1 || version == 2) && index_size > 0 {
                sized(index_size, reader)?;
            }
            let offset = sized(offset_size, reader)?
                .ok_or_else(|| unported("extent offset of an unknown size"))?;
            let length = sized(length_size, reader)?
                .ok_or_else(|| unported("extent length of an unknown size"))?;
            let extent = Extent {
                item,
                offset: offset.wrapping_add(base_offset),
                length,
            };
            if let Err(at) = extents.binary_search_by_key(&extent.offset, |known| known.offset) {
                extents.insert(at, extent);
            }
        }
    }
    Ok(extents)
}

/// `getIntFromUnknownByte`: none for a size it does not read.
fn sized(size: u8, reader: &mut Sequential) -> Result<Option<i64>, Stop> {
    Ok(Some(match size {
        0 => 0,
        1 => i64::from(reader.u8()?),
        2 => i64::from(reader.u16()?),
        4 => i64::from(reader.u32()?),
        8 => reader.i64()?,
        _ => return Ok(None),
    }))
}

/// The handler reading the boxes, `HeifBoxHandler` until a picture handler box hands over to a
/// `HeifPictureHandler`, each with the index of the directory it fills.
#[derive(Debug)]
enum Handler {
    Boxes {
        directory: usize,
    },
    Picture {
        directory: usize,
        item_info: Option<HashMap<i64, ItemInfo>>,
        item_location: Option<Vec<Extent>>,
    },
}

impl Handler {
    fn new_boxes(metadata: &mut Metadata) -> Self {
        metadata.add(Directory::new(Kind::Heif));
        Self::Boxes {
            directory: metadata.directories.len() - 1,
        }
    }

    fn new_picture(directory: usize) -> Self {
        Self::Picture {
            directory,
            item_info: None,
            item_location: None,
        }
    }

    fn accepts_box(&self, kind: &str) -> bool {
        match self {
            Self::Boxes { .. } => matches!(kind, "ftyp" | "hdlr" | "hvc1"),
            Self::Picture { .. } => matches!(
                kind,
                "ipro" | "pitm" | "iinf" | "iloc" | "ispe" | "auxC" | "irot" | "colr" | "pixi"
            ),
        }
    }

    fn accepts_container(&self, kind: &str) -> bool {
        match self {
            Self::Boxes { .. } => matches!(kind, "meta" | "iprp" | "ipco"),
            Self::Picture { .. } => matches!(kind, "iprp" | "ipco" | "mdat"),
        }
    }
}

/// `HeifMetadataReader.readMetadata` over a stream that cannot be reset, as Tika passes the
/// spooled file's.
pub(crate) fn read(data: &[u8], metadata: &mut Metadata) -> Result<(), Unported> {
    let mut reader = Sequential::new(data);
    let handler = Handler::new_boxes(metadata);
    process_boxes(&mut reader, -1, handler, metadata).map(|_| ())
}

/// What stopped a box: an `IOException`, with the handler as it was before the box, or a runtime
/// exception.
enum Interrupted {
    Io(Handler),
    Unported(Unported),
}

impl Interrupted {
    fn of(handler: Handler, stop: Stop) -> Self {
        match stop {
            Stop::Io => Self::Io(handler),
            Stop::Unported(unported) => Self::Unported(unported),
        }
    }
}

/// `processBoxes` (and `processTopLevelBoxes`, which differs only in an error a stream that
/// cannot be reset leaves): boxes until the end, or until an `IOException` ends the loop.
fn process_boxes(
    reader: &mut Sequential,
    end: i64,
    mut handler: Handler,
    metadata: &mut Metadata,
) -> Result<Handler, Unported> {
    while end == -1 || (reader.position() as i64) < end {
        let found = match Box::read(reader) {
            Ok(found) => found,
            Err(Stop::Io) => return Ok(handler),
            Err(Stop::Unported(unported)) => return Err(unported),
        };
        handler = match process_box(reader, found, handler, metadata) {
            Ok(next) => next,
            Err(Interrupted::Io(returned)) => return Ok(returned),
            Err(Interrupted::Unported(unported)) => return Err(unported),
        };
    }
    Ok(handler)
}

/// `processBox`: a container's boxes, a box the handler reads from its payload, or a box
/// skipped.
fn process_box(
    reader: &mut Sequential,
    found: Box,
    mut handler: Handler,
    metadata: &mut Metadata,
) -> Result<Handler, Interrupted> {
    if handler.accepts_container(&found.kind) {
        let end = found.size + reader.position() as i64 - 8;
        if let Err(stop) = process_container(&found, reader, &mut handler, metadata) {
            return Err(Interrupted::of(handler, stop));
        }
        return process_boxes(reader, end, handler, metadata).map_err(Interrupted::Unported);
    }
    if handler.accepts_box(&found.kind) {
        let payload = match reader.int_bytes(found.payload_length()) {
            Ok(payload) => payload,
            Err(stop) => return Err(Interrupted::of(handler, stop)),
        };
        let mut payload = Sequential::new(payload);
        return match handler.process(&found, &mut payload, metadata) {
            Ok(Some(next)) => Ok(next),
            Ok(None) => Ok(handler),
            Err(stop) => Err(Interrupted::of(handler, stop)),
        };
    }
    if found.size > 1 {
        if let Err(stop) = reader.skip(found.size - 8) {
            return Err(Interrupted::of(handler, stop));
        }
    }
    Ok(handler)
}

impl Handler {
    /// The handler's `processBox`: the handler that reads the boxes after it, when not this one.
    fn process(
        &mut self,
        found: &Box,
        reader: &mut Sequential,
        metadata: &mut Metadata,
    ) -> Result<Option<Handler>, Stop> {
        match self {
            Self::Boxes { directory } => match found.kind.as_str() {
                "ftyp" => {
                    file_type(reader, found, &mut metadata.directories[*directory])?;
                    Ok(None)
                }
                "hdlr" if handler_box(reader, found)? => Ok(Some(Self::new_picture(*directory))),
                _ => Ok(None),
            },
            Self::Picture {
                directory,
                item_info,
                item_location,
            } => {
                picture_box(
                    reader,
                    found,
                    *directory,
                    item_info,
                    item_location,
                    metadata,
                )?;
                Ok(None)
            }
        }
    }
}

fn process_container(
    found: &Box,
    reader: &mut Sequential,
    handler: &mut Handler,
    metadata: &mut Metadata,
) -> Result<(), Stop> {
    match handler {
        Handler::Boxes { .. } => {
            if found.kind == "meta" {
                full_box(reader)?;
            }
            Ok(())
        }
        Handler::Picture {
            item_info: Some(item_info),
            item_location: Some(item_location),
            ..
        } if found.kind == "mdat" => {
            for extent in item_location.iter() {
                let info = item_info
                    .get(&extent.item)
                    .ok_or_else(|| unported("extent of an item without item info"))?;
                let skip = extent.offset - reader.position() as i64;
                if skip > 0 {
                    reader.skip(skip)?;
                }
                if info.item_type.as_deref() == Some("Exif") {
                    let length = extent.length as i32;
                    let item = reader.at(extent.offset, length)?;
                    if skip >= 0 {
                        reader.int_bytes(length)?;
                    }
                    exif_item(item, metadata)?;
                }
            }
            Ok(())
        }
        Handler::Picture { .. } => Ok(()),
    }
}

/// `handleItem` of an Exif item: the TIFF data after the offset its first four bytes give.
fn exif_item(item: &[u8], metadata: &mut Metadata) -> Result<(), Stop> {
    let mut reader = Sequential::new(item);
    let header = i64::from(reader.u32()?);
    if header > reader.available() as i64 {
        return Ok(());
    }
    reader.skip(header)?;
    let tiff = reader.bytes(reader.available())?;
    match exif::read_tiff(tiff, 0, metadata) {
        Ok(()) | Err(exif::TiffError::Bounds(_) | exif::TiffError::Processing(_)) => Ok(()),
        Err(exif::TiffError::Unported(unported)) => Err(Stop::Unported(unported)),
    }
}

/// `FileTypeBox` and its `addMetadata`.
fn file_type(reader: &mut Sequential, found: &Box, directory: &mut Directory) -> Result<(), Stop> {
    let major = reader.string(4)?;
    let minor = reader.u32()?;
    let mut compatible = Vec::new();
    let mut index = 16i64;
    while index < found.size {
        compatible.push(reader.string(4)?);
        index += 4;
    }
    let required = compatible.iter().any(|brand| brand == "mif1");
    directory.set(TAG_MAJOR_BRAND, Value::Text(major));
    directory.set(TAG_MINOR_VERSION, Value::Long(i64::from(minor)));
    directory.set(TAG_COMPATIBLE_BRANDS, Value::Texts(compatible));
    if !required {
        directory.add_error("File Type Box does not contain required brand, mif1");
    }
    Ok(())
}

/// `HandlerBox`: whether its handler type is a picture's.
fn handler_box(reader: &mut Sequential, found: &Box) -> Result<bool, Stop> {
    full_box(reader)?;
    reader.skip(4)?;
    let handler_type = reader.string(4)?;
    reader.skip(12)?;
    reader.null_terminated(i64::from((found.size as i32).wrapping_sub(32)))?;
    Ok(handler_type == "pict")
}

/// `HeifPictureHandler.processBox`.
fn picture_box(
    reader: &mut Sequential,
    found: &Box,
    directory: usize,
    item_info: &mut Option<HashMap<i64, ItemInfo>>,
    item_location: &mut Option<Vec<Extent>>,
    metadata: &mut Metadata,
) -> Result<(), Stop> {
    match found.kind.as_str() {
        "ipro" => {
            full_box(reader)?;
            reader.u16()?;
        }
        "pitm" => {
            if full_box(reader)? == 0 {
                reader.u16()?;
            } else {
                reader.u32()?;
            }
        }
        "iinf" => *item_info = Some(item_info_box(reader, found)?),
        "iloc" => *item_location = Some(item_location_box(reader, found)?),
        "ispe" => {
            full_box(reader)?;
            let width = reader.u32()?;
            let height = reader.u32()?;
            let directory = &mut metadata.directories[directory];
            if !directory.contains(TAG_IMAGE_WIDTH) && !directory.contains(TAG_IMAGE_HEIGHT) {
                directory.set(TAG_IMAGE_WIDTH, Value::Long(i64::from(width)));
                directory.set(TAG_IMAGE_HEIGHT, Value::Long(i64::from(height)));
            }
        }
        "auxC" => {
            full_box(reader)?;
            let max = (found.size as i32).wrapping_sub(12);
            let mut index = 0;
            while index < max {
                if reader.u8()? == 0 {
                    break;
                }
                index += 1;
            }
        }
        "irot" => {
            let angle = i32::from(reader.u8()? & 0x03);
            let directory = &mut metadata.directories[directory];
            if !directory.contains(TAG_IMAGE_ROTATION) {
                directory.set(TAG_IMAGE_ROTATION, Value::Int(angle));
            }
        }
        "colr" => {
            let colour_type = reader.string(4)?;
            match colour_type.as_str() {
                "nclx" => {
                    reader.u16()?;
                    reader.u16()?;
                    reader.u16()?;
                    reader.u8()?;
                }
                "rICC" | "prof" => {
                    let profile = reader.int_bytes(found.size.wrapping_sub(12) as i32)?;
                    icc::extract(profile, metadata);
                }
                _ => {}
            }
        }
        "pixi" => {
            full_box(reader)?;
            let count = reader.u8()?;
            let channels: Vec<i32> = (0..count)
                .map(|_| reader.u8().map(i32::from))
                .collect::<Result<_, _>>()?;
            let directory = &mut metadata.directories[directory];
            if !directory.contains(TAG_BITS_PER_CHANNEL) {
                directory.set(TAG_BITS_PER_CHANNEL, Value::Ints(channels));
            }
        }
        _ => {}
    }
    Ok(())
}
