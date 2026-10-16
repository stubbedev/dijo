//! The Escher records of Office drawings as POI's `DefaultEscherRecordFactory`, or for slides its
//! `HSLFEscherRecordFactory`, makes them and their `fillFields` read them: containers with their
//! children, stored pictures (BSE records with their blips), blips, shapes, shape options,
//! anchors, text boxes and client data, and every other record read past as POI reads it,
//! failing where POI throws.

use crate::formats::poi::{i16_at, i32_at, safely_clone, thrown, Read, Thrown};

/// Escher record types, as POI's `EscherRecordTypes` names them.
pub(crate) mod kind {
    pub(crate) const DGG: u16 = 0xF006;
    pub(crate) const BSE: u16 = 0xF007;
    pub(crate) const DG: u16 = 0xF008;
    pub(crate) const SPGR: u16 = 0xF009;
    pub(crate) const SP: u16 = 0xF00A;
    pub(crate) const OPT: u16 = 0xF00B;
    pub(crate) const TEXTBOX: u16 = 0xF00C;
    pub(crate) const CLIENT_TEXTBOX: u16 = 0xF00D;
    pub(crate) const CHILD_ANCHOR: u16 = 0xF00F;
    pub(crate) const CLIENT_ANCHOR: u16 = 0xF010;
    pub(crate) const CLIENT_DATA: u16 = 0xF011;
    /// The record of a slide's placeholder, which `HSLFEscherRecordFactory` reads as Escher.
    pub(crate) const PLACEHOLDER: u16 = 0x0BC3;
    pub(crate) const BLIP_START: u16 = 0xF018;
    pub(crate) const BLIP_EMF: u16 = 0xF01A;
    pub(crate) const BLIP_WMF: u16 = 0xF01B;
    pub(crate) const BLIP_PICT: u16 = 0xF01C;
    pub(crate) const BLIP_JPEG: u16 = 0xF01D;
    pub(crate) const BLIP_PNG: u16 = 0xF01E;
    pub(crate) const BLIP_DIB: u16 = 0xF01F;
    pub(crate) const BLIP_TIFF: u16 = 0xF029;
    pub(crate) const BLIP_END: u16 = 0xF117;
    pub(crate) const SPLIT_MENU_COLORS: u16 = 0xF11E;
    pub(crate) const TERTIARY_OPT: u16 = 0xF122;
    pub(crate) const UNKNOWN: u16 = 0xFFFF;
    /// The types `EscherRecordTypes` names without a class of their own.
    pub(crate) const WITHOUT_CLASS: [u16; 15] = [
        0xF00E, 0xF012, 0xF013, 0xF014, 0xF015, 0xF016, 0xF017, 0xF018, 0xF117, 0xF118, 0xF119,
        0xF11A, 0xF11D, 0xF11F, 0xF120,
    ];
}

/// The property of a shape's options that names the picture it shows.
pub(crate) const PICTURE_PROPERTY: u16 = 0x0104;
const MAX_NESTED_CHILD_NODES: usize = 1000;
const MAX_NUMBER_OF_CHILDREN: usize = 100_000;
const MAX_SMALL_RECORD: i64 = 100_000;
const MAX_LARGE_RECORD: i64 = 100_000_000;
const MAX_BLIP: i64 = 104_857_600;
/// `RecordAtom.getMaxRecordLength`, which bounds the client data of a slide's shape.
const MAX_SLIDE_RECORD: i64 = 1_000_000;

/// The factory that makes the records: POI's `DefaultEscherRecordFactory`, or for slides its
/// `HSLFEscherRecordFactory`, which reads a placeholder and the client data of shapes itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Factory {
    Default,
    Slides,
}

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
    /// `getUid`: the checksum of its picture.
    pub(crate) uid: [u8; 16],
    /// `getOffset`: where its blip is in the delay stream.
    pub(crate) offset: i32,
}

/// A property of a shape's options, as `EscherPropertyFactory` makes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Property {
    /// The property's number, without its flags.
    pub(crate) number: u16,
    /// Its value: of a simple property, or the length of a complex one's data.
    pub(crate) value: i32,
    /// The data of a complex property (`EscherComplexProperty` or `EscherArrayProperty`).
    pub(crate) complex: Option<Vec<u8>>,
}

/// Shape options (`EscherOptRecord` or `EscherTertiaryOptRecord`): their properties in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options(pub(crate) Vec<Property>);

impl Options {
    /// `lookup`: the first property of the number.
    pub(crate) fn lookup(&self, number: u16) -> Option<&Property> {
        self.0.iter().find(|property| property.number == number)
    }

    /// The picture numbers of the simple picture properties.
    pub(crate) fn pictures(&self) -> impl Iterator<Item = i32> + '_ {
        self.0
            .iter()
            .filter(|property| property.number == PICTURE_PROPERTY && property.complex.is_none())
            .map(|property| property.value)
    }
}

/// A shape (`EscherSpRecord`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shape {
    /// `getShapeType`: the record's instance.
    pub(crate) kind: u16,
    pub(crate) id: i32,
    pub(crate) flags: i32,
}

/// What of a record its readers look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Content {
    /// An `EscherContainerRecord`.
    Container,
    Stored(Stored),
    Picture(Blip),
    Shape(Shape),
    Options(Options),
    TertiaryOptions(Options),
    /// A client anchor's flag, first column, first x offset and first row, which a slide's
    /// shape reads as its rectangle; `None` where the record holds 4 bytes, Word's format.
    ClientAnchor(Option<[i16; 4]>),
    /// A child anchor's corners.
    ChildAnchor([i32; 4]),
    /// The data of a text box.
    Textbox(Vec<u8>),
    /// The data of a slide's shape's client data: the slide records `HSLFEscherClientDataRecord`
    /// holds.
    ClientData(Vec<u8>),
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
    SlidesClientData,
    Placeholder,
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

/// `getConstructor` of the factory.
fn class_of(options: u16, record_id: u16, factory: Factory) -> Class {
    if factory == Factory::Slides {
        match record_id {
            kind::PLACEHOLDER => return Class::Placeholder,
            kind::CLIENT_DATA => return Class::SlidesClientData,
            _ => {}
        }
    }
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

/// The record id `createRecord` reads at `offset`, failing where its header ends early.
pub(crate) fn record_id_at(data: &[u8], offset: usize) -> Read<u16> {
    i16_at(data, offset)?;
    Ok(i16_at(data, offset + 2)? as u16)
}

/// `createRecord` at `offset`.
fn create(data: &[u8], offset: usize, factory: Factory) -> Read<Class> {
    let options = i16_at(data, offset)? as u16;
    let record_id = i16_at(data, offset + 2)? as u16;
    Ok(class_of(options, record_id, factory))
}

/// `createRecord` and `fillFields` at `offset`: the record, and the length POI says it read.
pub(crate) fn record_at(data: &[u8], offset: usize) -> Read<(Escher, isize)> {
    made_at(data, offset, Factory::Default)
}

/// `createRecord` and `fillFields` at `offset`, by `factory`.
pub(crate) fn made_at(data: &[u8], offset: usize, factory: Factory) -> Read<(Escher, isize)> {
    fill(data, offset, create(data, offset, factory)?, 0, factory)
}

/// `new EscherContainerRecord().fillFields` at `offset`: the record read as a container
/// whatever its type, and the length POI says it read.
pub(crate) fn container_at(data: &[u8], offset: usize) -> Read<(Escher, isize)> {
    container_made_at(data, offset, Factory::Default)
}

/// `new EscherContainerRecord().fillFields` at `offset`, its children made by `factory`.
pub(crate) fn container_made_at(
    data: &[u8],
    offset: usize,
    factory: Factory,
) -> Read<(Escher, isize)> {
    fill(data, offset, Class::Container, 0, factory)
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

    /// `getChildById`: the first child of the type.
    pub(crate) fn child(&self, record_id: u16) -> Option<&Escher> {
        self.children
            .iter()
            .find(|child| child.record_id == record_id)
    }
}

/// `fillFields` of a record of `class` at `offset`: the record, and the length POI says it
/// read.
fn fill(
    data: &[u8],
    offset: usize,
    class: Class,
    nesting: usize,
    factory: Factory,
) -> Read<(Escher, isize)> {
    let options = i16_at(data, offset)? as u16;
    let record_id = i16_at(data, offset + 2)? as u16;
    let length = i64::from(i32_at(data, offset + 4)?);
    let body = offset + 8;
    let read = |consumed: i64| consumed as isize;
    let other = || Escher::other(record_id);
    let with = |content| Escher {
        record_id,
        content,
        children: Vec::new(),
    };
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
                let child_class = create(data, at, factory)?;
                let (child, consumed) = fill(data, at, child_class, nesting + 1, factory)?;
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
                    content: Content::Container,
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
                    let child_class = create(data, at, factory)?;
                    let (child, consumed) = fill(data, at, child_class, nesting + 1, factory)?;
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
            let uid: [u8; 16] = data[body + 2..body + 18].try_into().expect("16 bytes");
            let blip_offset = i32_at(data, body + 28)?;
            let mut remaining = length - 36;
            let mut blip = None;
            let mut blip_size = 0;
            if remaining > 0 {
                let blip_class = create(data, body + 36, factory)?;
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
                        uid,
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
        Class::Dg => {
            i32_at(data, body)?;
            i32_at(data, body + 4)?;
            (other(), 16)
        }
        Class::Sp => {
            let shape = Shape {
                kind: options >> 4,
                id: i32_at(data, body)?,
                flags: i32_at(data, body + 4)?,
            };
            (with(Content::Shape(shape)), 16)
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
            let properties = Options(properties(data, body, count)?);
            let content = if class == Class::Options {
                Content::Options(properties)
            } else {
                Content::TertiaryOptions(properties)
            };
            (with(content), read(length + 8))
        }
        Class::Textbox => {
            let text = clone(data, body, length, MAX_SMALL_RECORD)?;
            (with(Content::Textbox(text)), read(length + 8))
        }
        Class::ChildAnchor => {
            let corners = match length {
                16 => [0, 4, 8, 12].map(|at| i32_at(data, body + at)),
                8 => [0, 2, 4, 6].map(|at| i16_at(data, body + at).map(i32::from)),
                _ => return thrown("Invalid EscherChildAnchorRecord - neither 8 nor 16 bytes."),
            };
            let [x1, y1, x2, y2] = corners;
            (
                with(Content::ChildAnchor([x1?, y1?, x2?, y2?])),
                read(8 + length),
            )
        }
        Class::ClientAnchor => {
            let mut size = 0i64;
            let mut corners = None;
            if length != 4 {
                size = if length >= 18 { 18 } else { 8 };
                if body + size as usize > data.len() {
                    return thrown("client anchor ends before its fields");
                }
                let [flag, col1, dx1, row1] = [0, 2, 4, 6].map(|at| i16_at(data, body + at));
                corners = Some([flag?, col1?, dx1?, row1?]);
            }
            clone(data, body + size as usize, length - size, MAX_SMALL_RECORD)?;
            (with(Content::ClientAnchor(corners)), read(8 + length))
        }
        Class::ClientData => {
            if length != 0 {
                clone(data, body, length, MAX_SMALL_RECORD)?;
            }
            (other(), read(8 + length))
        }
        Class::SlidesClientData => {
            let records = clone(data, body, length, MAX_SLIDE_RECORD)?;
            (with(Content::ClientData(records)), read(length + 8))
        }
        Class::Placeholder => {
            i32_at(data, body)?;
            i16_at(data, body + 6)?;
            (other(), read(length + 8))
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

/// `EscherPropertyFactory.createProperties`: the properties, past whose complex data POI reads
/// as it does.
fn properties(data: &[u8], offset: usize, count: u16) -> Read<Vec<Property>> {
    #[derive(Clone, Copy)]
    enum Kind {
        Simple,
        Complex,
        Array,
    }
    let mut kinds = Vec::with_capacity(usize::from(count));
    let mut properties = Vec::with_capacity(usize::from(count));
    let mut position = offset;
    for _ in 0..count {
        let id = i16_at(data, position)? as u16;
        let value = i32_at(data, position + 2)?;
        let number = id & 0x3FFF;
        let complex = id & 0x8000 != 0;
        let kind = match holder(number) {
            Holder::Simple => Kind::Simple,
            holder if complex => {
                if !(0..=MAX_LARGE_RECORD).contains(&i64::from(value)) {
                    return thrown(format!("Tried to allocate an array of length {value}"));
                }
                if holder == Holder::Array {
                    Kind::Array
                } else {
                    Kind::Complex
                }
            }
            _ => Kind::Simple,
        };
        kinds.push(kind);
        properties.push(Property {
            number,
            value,
            complex: None,
        });
        position += 6;
    }
    for (property, kind) in properties.iter_mut().zip(kinds) {
        let length = i64::from(property.value);
        match kind {
            Kind::Simple => {}
            Kind::Complex => {
                let left = data.len() as i64 - position as i64;
                if left < length {
                    return thrown(format!(
                        "Could not read complex escher property, length was {length}, but had \
                         only {left} bytes left"
                    ));
                }
                let start = position;
                position += length as usize;
                property.complex = Some(data[start..position].to_vec());
            }
            Kind::Array => {
                property.complex = Some(Vec::new());
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
    Ok(properties)
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
                uid: [0; 16],
                offset: 0,
            })
        };
        assert_eq!(stored, [&stored_jpeg(b"jpeg"), &stored_jpeg(b"two")]);
        assert_eq!(
            records[0].child(0xF001).map(|store| store.children.len()),
            Some(2)
        );
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
            Content::Options(Options(vec![Property {
                number: PICTURE_PROPERTY,
                value: 3,
                complex: None
            }]))
        );
    }

    #[test]
    fn reads_a_record_as_a_container_when_asked_to() {
        let blip = record(0, kind::BLIP_PNG, &[0; 20]);
        let (container, _) = container_at(&record(0, 0xF00B, &blip), 0).unwrap();
        assert_eq!(container.record_id, 0xF00B);
        assert!(matches!(
            container.children[0].content,
            Content::Picture(Blip {
                record_id: kind::BLIP_PNG,
                ..
            })
        ));
        assert_eq!(record_id_at(&blip, 0), Ok(kind::BLIP_PNG));
        assert!(record_id_at(&blip, 25).is_err());
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
