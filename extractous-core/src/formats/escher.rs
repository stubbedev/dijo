//! The Escher records of Office drawings as POI's `DefaultEscherRecordFactory` makes them and
//! their `fillFields` read them: containers with their children, stored pictures (BSE records
//! with their blips), blips, shape options with the picture each shape shows, and every other
//! record read past as POI reads it, failing where POI throws.

use crate::formats::poi::{i16_at, i32_at, safely_clone, thrown, Read, Thrown};

/// Escher record types, as POI's `EscherRecordTypes` names them.
pub(crate) mod kind {
    pub(super) const DGG: u16 = 0xF006;
    pub(super) const BSE: u16 = 0xF007;
    pub(super) const DG: u16 = 0xF008;
    pub(super) const SPGR: u16 = 0xF009;
    pub(super) const SP: u16 = 0xF00A;
    pub(super) const OPT: u16 = 0xF00B;
    pub(super) const TEXTBOX: u16 = 0xF00C;
    pub(super) const CLIENT_TEXTBOX: u16 = 0xF00D;
    pub(super) const CHILD_ANCHOR: u16 = 0xF00F;
    pub(super) const CLIENT_ANCHOR: u16 = 0xF010;
    pub(super) const CLIENT_DATA: u16 = 0xF011;
    pub(super) const BLIP_START: u16 = 0xF018;
    pub(super) const BLIP_EMF: u16 = 0xF01A;
    pub(super) const BLIP_WMF: u16 = 0xF01B;
    pub(super) const BLIP_PICT: u16 = 0xF01C;
    pub(super) const BLIP_JPEG: u16 = 0xF01D;
    pub(super) const BLIP_PNG: u16 = 0xF01E;
    pub(super) const BLIP_DIB: u16 = 0xF01F;
    pub(super) const BLIP_TIFF: u16 = 0xF029;
    pub(super) const BLIP_END: u16 = 0xF117;
    pub(super) const SPLIT_MENU_COLORS: u16 = 0xF11E;
    pub(super) const TERTIARY_OPT: u16 = 0xF122;
    pub(super) const UNKNOWN: u16 = 0xFFFF;
    /// The types `EscherRecordTypes` names without a class of their own.
    pub(super) const WITHOUT_CLASS: [u16; 15] = [
        0xF00E, 0xF012, 0xF013, 0xF014, 0xF015, 0xF016, 0xF017, 0xF018, 0xF117, 0xF118, 0xF119,
        0xF11A, 0xF11D, 0xF11F, 0xF120,
    ];
}

/// The property of a shape's options that names the picture it shows.
const PICTURE_PROPERTY: u16 = 0x0104;
const MAX_NESTED_CHILD_NODES: usize = 1000;
const MAX_NUMBER_OF_CHILDREN: usize = 100_000;
const MAX_SMALL_RECORD: i64 = 100_000;
const MAX_LARGE_RECORD: i64 = 100_000_000;
const MAX_BLIP: i64 = 104_857_600;

/// A picture: the type of its blip record and its data, as `getPicturedata` gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Blip {
    pub(crate) record_id: u16,
    pub(crate) data: Vec<u8>,
}

/// A BSE record: the picture it stores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stored {
    /// Its blip, if it holds one.
    pub(crate) blip: Option<Blip>,
    /// `getBlipTypeWin32`.
    pub(crate) blip_type: u8,
    /// `getOffset`: where its blip is in the delay stream.
    pub(crate) offset: i32,
}

/// What of a record its readers look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Content {
    Stored(Stored),
    Picture(Blip),
    /// Shape options (`EscherOptRecord`), with the picture numbers of their simple properties.
    Options(Vec<i32>),
    Other,
}

/// An Escher record and the records it contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Escher {
    pub(crate) record_id: u16,
    pub(crate) content: Content,
    pub(crate) children: Vec<Escher>,
}

/// The class `DefaultEscherRecordFactory.createRecord` makes of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Container,
    Dgg,
    Bse,
    Dg,
    Spgr,
    Sp,
    Options,
    TertiaryOptions,
    Textbox,
    ChildAnchor,
    ClientAnchor,
    ClientData,
    MetafileBlip,
    BitmapBlip,
    Blip,
    SplitMenuColors,
    Unknown,
}

/// `IOUtils.safelyClone` from an offset into the data.
fn clone(data: &[u8], offset: usize, length: i64, max: i64) -> Read<Vec<u8>> {
    safely_clone(data, offset as i64, length, max)
}

/// `DefaultEscherRecordFactory.getConstructor`.
fn class_of(options: u16, record_id: u16) -> Class {
    let known = match record_id {
        0xF000..=0xF005 => Some(Class::Container),
        kind::DGG => Some(Class::Dgg),
        kind::BSE => Some(Class::Bse),
        kind::DG => Some(Class::Dg),
        kind::SPGR => Some(Class::Spgr),
        kind::SP => Some(Class::Sp),
        kind::OPT => Some(Class::Options),
        kind::TEXTBOX | kind::CLIENT_TEXTBOX => Some(Class::Textbox),
        kind::CHILD_ANCHOR => Some(Class::ChildAnchor),
        kind::CLIENT_ANCHOR => Some(Class::ClientAnchor),
        kind::CLIENT_DATA => Some(Class::ClientData),
        kind::BLIP_EMF | kind::BLIP_WMF | kind::BLIP_PICT => Some(Class::MetafileBlip),
        kind::BLIP_JPEG | kind::BLIP_PNG | kind::BLIP_DIB | kind::BLIP_TIFF => {
            Some(Class::BitmapBlip)
        }
        kind::SPLIT_MENU_COLORS => Some(Class::SplitMenuColors),
        kind::TERTIARY_OPT => Some(Class::TertiaryOptions),
        _ => None,
    };
    if let Some(class) = known {
        return class;
    }
    let named = kind::WITHOUT_CLASS.contains(&record_id) || record_id == kind::UNKNOWN;
    if (!named || record_id == kind::UNKNOWN) && options & 0x000F == 0x000F {
        return Class::Container;
    }
    if (kind::BLIP_START..=kind::BLIP_END).contains(&record_id) {
        return Class::Blip;
    }
    Class::Unknown
}

/// `createRecord` at `offset`.
fn create(data: &[u8], offset: usize) -> Read<Class> {
    let options = i16_at(data, offset)? as u16;
    let record_id = i16_at(data, offset + 2)? as u16;
    Ok(class_of(options, record_id))
}

/// `createRecord` and `fillFields` at `offset`: the record, and the length POI says it read.
pub(crate) fn record_at(data: &[u8], offset: usize) -> Read<(Escher, isize)> {
    fill(data, offset, create(data, offset)?, 0)
}

/// `AbstractEscherHolderRecord.decode`: the records of a drawing group's data.
pub(crate) fn decode(data: &[u8]) -> Read<Vec<Escher>> {
    let mut records = Vec::new();
    let mut position = 0;
    while position < data.len() {
        let (record, length) = record_at(data, position)?;
        records.push(record);
        position = position
            .checked_add_signed(length)
            .ok_or_else(|| Thrown::new("Escher records run back before the data"))?;
    }
    Ok(records)
}

impl Escher {
    fn other(record_id: u16) -> Escher {
        Escher {
            record_id,
            content: Content::Other,
            children: Vec::new(),
        }
    }
}

/// `fillFields` of a record of `class` at `offset`: the record, and the length POI says it
/// read.
fn fill(data: &[u8], offset: usize, class: Class, nesting: usize) -> Read<(Escher, isize)> {
    let options = i16_at(data, offset)? as u16;
    let record_id = i16_at(data, offset + 2)? as u16;
    let length = i64::from(i32_at(data, offset + 4)?);
    let body = offset + 8;
    let read = |consumed: i64| consumed as isize;
    let other = || Escher::other(record_id);
    Ok(match class {
        Class::Container => {
            if nesting > MAX_NESTED_CHILD_NODES {
                return thrown("Had more than the limit of 1000 nested child notes");
            }
            let mut remaining = length;
            let mut written: i64 = 8;
            let mut position = body as i64;
            let mut children = Vec::new();
            while remaining > 0 && position < data.len() as i64 {
                let at = position as usize;
                let child_class = create(data, at)?;
                let (child, consumed) = fill(data, at, child_class, nesting + 1)?;
                written += consumed as i64;
                position += consumed as i64;
                remaining -= consumed as i64;
                if children.len() >= MAX_NUMBER_OF_CHILDREN {
                    return thrown("Cannot add more than 100000 child records");
                }
                children.push(child);
                if position < 0 {
                    return thrown("Escher records run back before the data");
                }
            }
            (
                Escher {
                    record_id,
                    content: Content::Other,
                    children,
                },
                read(written),
            )
        }
        Class::Unknown => {
            if nesting > MAX_NESTED_CHILD_NODES {
                return thrown("Had more than the limit of 1000 nested child notes");
            }
            let available = data.len() as i64 - body as i64;
            let mut remaining = length.min(available);
            if options & 0x000F == 0x000F {
                let mut written: i64 = 8;
                let mut position = body as i64;
                let mut children = Vec::new();
                while remaining > 0 {
                    let at = usize::try_from(position)
                        .map_err(|_| Thrown::new("Escher records run back before the data"))?;
                    let child_class = create(data, at)?;
                    let (child, consumed) = fill(data, at, child_class, nesting + 1)?;
                    written += consumed as i64;
                    position += consumed as i64;
                    remaining -= consumed as i64;
                    children.push(child);
                }
                (
                    Escher {
                        record_id,
                        content: Content::Other,
                        children,
                    },
                    read(written),
                )
            } else {
                remaining = remaining.max(0);
                clone(data, body, remaining, MAX_LARGE_RECORD)?;
                (other(), read(remaining + 8))
            }
        }
        Class::Dgg => {
            for at in [0, 8, 12] {
                i32_at(data, body + at)?;
            }
            let mut size: i64 = 16;
            let clusters = (length - size) / 8;
            for _ in 0..clusters.max(0) {
                i32_at(data, body + size as usize)?;
                i32_at(data, body + size as usize + 4)?;
                size += 8;
            }
            if length - size != 0 {
                return thrown(format!(
                    "Expecting no remaining data but got {} byte(s).",
                    length - size
                ));
            }
            (other(), read(8 + size))
        }
        Class::Bse => {
            if body + 36 > data.len() {
                return thrown("BSE record ends before its fields");
            }
            let blip_type = data[body];
            let blip_offset = i32_at(data, body + 28)?;
            let mut remaining = length - 36;
            let mut blip = None;
            let mut blip_size = 0;
            if remaining > 0 {
                let blip_class = create(data, body + 36)?;
                if !matches!(
                    blip_class,
                    Class::Blip | Class::BitmapBlip | Class::MetafileBlip
                ) {
                    return thrown("Did not have a EscherBlipRecord");
                }
                let (found, read_length, size) = fill_blip(data, body + 36, blip_class)?;
                remaining -= read_length;
                blip_size = size;
                blip = Some(found);
            }
            safely_clone(
                data,
                body as i64 + 36 + length - 36 - remaining,
                remaining,
                MAX_SMALL_RECORD,
            )?;
            (
                Escher {
                    record_id,
                    content: Content::Stored(Stored {
                        blip,
                        blip_type,
                        offset: blip_offset,
                    }),
                    children: Vec::new(),
                },
                read(remaining + 8 + 36 + blip_size),
            )
        }
        Class::Blip | Class::BitmapBlip | Class::MetafileBlip => {
            let (blip, read_length, _) = fill_blip(data, offset, class)?;
            (
                Escher {
                    record_id,
                    content: Content::Picture(blip),
                    children: Vec::new(),
                },
                read(read_length),
            )
        }
        Class::Dg | Class::Sp => {
            i32_at(data, body)?;
            i32_at(data, body + 4)?;
            (other(), 16)
        }
        Class::Spgr | Class::SplitMenuColors => {
            for at in [0, 4, 8, 12] {
                i32_at(data, body + at)?;
            }
            if length != 16 {
                return thrown(format!(
                    "Expected no remaining bytes but got {}",
                    length - 16
                ));
            }
            (other(), 24)
        }
        Class::Options | Class::TertiaryOptions => {
            if length < 0 {
                return thrown(format!("Invalid value for bytesRemaining: {length}"));
            }
            let count = (options & 0xFFF0) >> 4;
            let pictures = picture_properties(data, body, count)?;
            let content = if class == Class::Options {
                Content::Options(pictures)
            } else {
                Content::Other
            };
            (
                Escher {
                    record_id,
                    content,
                    children: Vec::new(),
                },
                read(length + 8),
            )
        }
        Class::Textbox => {
            clone(data, body, length, MAX_SMALL_RECORD)?;
            (other(), read(length + 8))
        }
        Class::ChildAnchor => {
            let fields = match length {
                16 => 16,
                8 => 8,
                _ => return thrown("Invalid EscherChildAnchorRecord - neither 8 nor 16 bytes."),
            };
            if body + fields > data.len() {
                return thrown("child anchor ends before its fields");
            }
            (other(), read(8 + fields as i64))
        }
        Class::ClientAnchor => {
            let mut size = 0i64;
            if length != 4 {
                size = if length >= 18 { 18 } else { 8 };
                if body + size as usize > data.len() {
                    return thrown("client anchor ends before its fields");
                }
            }
            clone(data, body + size as usize, length - size, MAX_SMALL_RECORD)?;
            (other(), read(8 + length))
        }
        Class::ClientData => {
            if length != 0 {
                clone(data, body, length, MAX_SMALL_RECORD)?;
            }
            (other(), read(8 + length))
        }
    })
}

/// A blip record's `fillFields`: the blip, the length POI says it read, and its record size.
fn fill_blip(data: &[u8], offset: usize, class: Class) -> Read<(Blip, i64, i64)> {
    let options = i16_at(data, offset)? as u16;
    let record_id = i16_at(data, offset + 2)? as u16;
    let length = i64::from(i32_at(data, offset + 4)?);
    let body = offset + 8;
    let picture = match class {
        Class::BitmapBlip => {
            if body + 17 > data.len() {
                return thrown("bitmap blip ends before its fields");
            }
            let size = length - 17;
            let start = body + 17;
            if size < 0 || (data.len() as i64) < start as i64 + size {
                return thrown("picture data can't be null");
            }
            let picture = clone(data, start, size, MAX_BLIP)?;
            let record_size = 8 + 16 + 1 + picture.len() as i64;
            return Ok((
                Blip {
                    record_id,
                    data: picture,
                },
                length + 8,
                record_size,
            ));
        }
        Class::MetafileBlip => {
            let signature: u16 = match record_id {
                kind::BLIP_EMF => 0x3D40,
                kind::BLIP_WMF => 0x2160,
                _ => 0x5420,
            };
            let mut position = body + 16;
            let second_uid = options ^ signature == 0x10;
            if second_uid {
                position += 16;
            }
            let saved = i64::from(i32_at(data, position + 28)?);
            let compression = *data
                .get(position + 32)
                .ok_or_else(|| Thrown::new("metafile blip ends before its fields"))?;
            data.get(position + 33)
                .ok_or_else(|| Thrown::new("metafile blip ends before its fields"))?;
            position += 34;
            let raw = clone(data, position, saved, MAX_LARGE_RECORD)?;
            let picture = if compression == 0 {
                inflate(&raw)
            } else {
                raw.clone()
            };
            let end = position as i64 + saved;
            let remaining = length - end + offset as i64 + 8;
            let rest = if remaining > 0 {
                safely_clone(data, end, remaining, MAX_LARGE_RECORD)?.len() as i64
            } else {
                0
            };
            let record_size = 8 + 50 + if second_uid { 16 } else { 0 } + raw.len() as i64 + rest;
            return Ok((
                Blip {
                    record_id,
                    data: picture,
                },
                length + 8,
                record_size,
            ));
        }
        _ => clone(data, body, length, MAX_BLIP)?,
    };
    let record_size = 8 + picture.len() as i64;
    Ok((
        Blip {
            record_id,
            data: picture,
        },
        length + 8,
        record_size,
    ))
}

/// `EscherMetafileBlip.inflatePictureData`: the data inflated, or as it is where it is no
/// complete zlib stream.
fn inflate(data: &[u8]) -> Vec<u8> {
    let mut inflater = flate2::Decompress::new(true);
    let mut out = Vec::with_capacity(data.len().saturating_mul(4).max(64));
    loop {
        if out.capacity() == out.len() {
            out.reserve(out.len());
        }
        let (read, written) = (inflater.total_in(), inflater.total_out());
        let consumed = read as usize;
        match inflater.decompress_vec(&data[consumed..], &mut out, flate2::FlushDecompress::None) {
            Ok(flate2::Status::StreamEnd) => return out,
            Ok(_) if inflater.total_in() == read && inflater.total_out() == written => {
                return data.to_vec();
            }
            Ok(_) => {}
            Err(_) => return data.to_vec(),
        }
    }
}

/// The holders `EscherPropertyTypes` gives properties whose class does not follow from their
/// complex bit.
fn holder(number: u16) -> Holder {
    match number {
        0x007F | 0x00BF | 0x017F | 0x01BF | 0x01FF | 0x057F | 0x05BF | 0x063F | 0x05FF => {
            Holder::Simple
        }
        0x0181 | 0x0183 | 0x01C0 | 0x01C2 | 0x0201 | 0x0287 | 0x02BE | 0x0144 => Holder::Simple,
        0x0145 | 0x0146 | 0x0197 | 0x01CF | 0x0383 | 0x03A0 => Holder::Array,
        _ => Holder::Other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    /// Boolean, colour and shape path properties, simple whatever their complex bit says.
    Simple,
    Array,
    Other,
}

/// `EscherPropertyFactory.createProperties`: the picture numbers of the simple picture
/// properties, after reading past the complex data as POI does.
fn picture_properties(data: &[u8], offset: usize, count: u16) -> Read<Vec<i32>> {
    enum Property {
        Simple,
        Complex(i64),
        Array(i64),
    }
    let mut properties = Vec::with_capacity(usize::from(count));
    let mut pictures = Vec::new();
    let mut position = offset;
    for _ in 0..count {
        let id = i16_at(data, position)? as u16;
        let value = i32_at(data, position + 2)?;
        let number = id & 0x3FFF;
        let complex = id & 0x8000 != 0;
        let property = match holder(number) {
            Holder::Simple => Property::Simple,
            holder if complex => {
                if !(0..=MAX_LARGE_RECORD).contains(&i64::from(value)) {
                    return thrown(format!("Tried to allocate an array of length {value}"));
                }
                if holder == Holder::Array {
                    Property::Array(i64::from(value))
                } else {
                    Property::Complex(i64::from(value))
                }
            }
            _ => Property::Simple,
        };
        if matches!(property, Property::Simple) && number == PICTURE_PROPERTY {
            pictures.push(value);
        }
        properties.push(property);
        position += 6;
    }
    for property in properties {
        match property {
            Property::Simple => {}
            Property::Complex(length) => {
                let left = data.len() as i64 - position as i64;
                if left < length {
                    return thrown(format!(
                        "Could not read complex escher property, length was {length}, but had \
                         only {left} bytes left"
                    ));
                }
                position += length as usize;
            }
            Property::Array(length) => {
                if length == 0 {
                    continue;
                }
                let elements = i32::from(i16_at(data, position)?);
                let size = i16_at(data, position + 4)?;
                let actual = if size < 0 {
                    i32::from(((-i32::from(size)) >> 2) as i16)
                } else {
                    i32::from(size)
                };
                let array_size = i64::from(actual.wrapping_mul(elements));
                let mut length = length;
                if array_size == length {
                    length = array_size + 6;
                    if length > MAX_LARGE_RECORD {
                        return thrown(format!("Tried to allocate an array of length {length}"));
                    }
                }
                position += length as usize;
            }
        }
    }
    Ok(pictures)
}

/// `HSSFPictureData.getMimeType`: the media type of a blip by its record type.
pub(crate) fn media_type(record_id: u16) -> &'static str {
    match record_id {
        kind::BLIP_WMF => "image/x-wmf",
        kind::BLIP_EMF => "image/x-emf",
        kind::BLIP_PICT => "image/x-pict",
        kind::BLIP_PNG => "image/png",
        kind::BLIP_JPEG => "image/jpeg",
        kind::BLIP_DIB => "image/bmp",
        kind::BLIP_TIFF => "image/tiff",
        _ => "image/unknown",
    }
}

/// `ImageHeaderPNG.extractPNG`: a PNG after the 16 bytes of a header without them, any other
/// data as it is.
pub(crate) fn picture_data(data: &[u8]) -> &[u8] {
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    match data.get(16..) {
        Some(rest) if rest.starts_with(&PNG) => rest,
        _ => data,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An Escher record of a type with the options and body given.
    pub(crate) fn record(options: u16, kind: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = options.to_le_bytes().to_vec();
        bytes.extend(kind.to_le_bytes());
        bytes.extend((body.len() as i32).to_le_bytes());
        bytes.extend(body);
        bytes
    }

    /// A BSE record storing a JPEG blip of the picture.
    pub(crate) fn bse(picture: &[u8]) -> Vec<u8> {
        let mut blip = vec![0; 17];
        blip.extend(picture);
        let mut body = vec![0; 36];
        body[0] = 5;
        body.extend(record(0x46A0, kind::BLIP_JPEG, &blip));
        record(0x0062, kind::BSE, &body)
    }

    #[test]
    fn reads_the_pictures_of_a_drawing_group() {
        let store = record(0x000F, 0xF001, &[bse(b"jpeg"), bse(b"two")].concat());
        let group = record(0x000F, 0xF000, &store);
        let records = decode(&group).unwrap();
        let stored: Vec<&Content> = records[0].children[0]
            .children
            .iter()
            .map(|child| &child.content)
            .collect();
        let stored_jpeg = |data: &[u8]| {
            Content::Stored(Stored {
                blip: Some(Blip {
                    record_id: kind::BLIP_JPEG,
                    data: data.to_vec(),
                }),
                blip_type: 5,
                offset: 0,
            })
        };
        assert_eq!(stored, [&stored_jpeg(b"jpeg"), &stored_jpeg(b"two")]);
    }

    #[test]
    fn reads_the_pictures_shapes_show() {
        let mut options = 0x0104u16.to_le_bytes().to_vec();
        options.extend(3i32.to_le_bytes());
        let shape = record(0x000F, 0xF004, &record(0x0013, kind::OPT, &options));
        let (records, length) = record_at(&record(0x000F, 0xF002, &shape), 0).unwrap();
        assert_eq!(length, 30);
        assert_eq!(
            records.children[0].children[0].content,
            Content::Options(vec![3])
        );
    }

    #[test]
    fn fails_where_poi_fails() {
        assert!(decode(&record(0, kind::DGG, &[0; 15])).is_err());
        assert!(decode(&record(0, kind::SPLIT_MENU_COLORS, &[0; 12])).is_err());
        assert!(decode(&record(0x000F, 0xF000, &[0; 4])).is_err());
    }

    #[test]
    fn strips_the_header_of_png_pictures() {
        let mut data = vec![1; 16];
        data.extend([0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 9]);
        assert_eq!(picture_data(&data)[0], 0x89);
        assert_eq!(picture_data(b"jpeg"), b"jpeg");
    }
}
