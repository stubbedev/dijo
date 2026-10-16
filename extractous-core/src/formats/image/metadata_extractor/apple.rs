//! metadata-extractor's Apple maker note: the `AppleMakernoteDirectory` an IFD of Apple's fills,
//! and the `AppleRunTimeMakernoteDirectory` its run time tag holds as a binary property list,
//! which `AppleRunTimeReader` reads with metadata-extractor's own `BplistReader`.

use super::descriptions::{base, indexed, Described};
use super::{Directory, Kind, Metadata, Rational, Unported, Value};
use crate::formats::data_format::printf;

pub(super) const TAG_RUN_TIME: i32 = 0x0003;
const TAG_ACCELERATION_VECTOR: i32 = 0x0008;
const TAG_HDR_IMAGE_TYPE: i32 = 0x000a;
const CM_TIME_FLAGS: i32 = 1;
const CM_TIME_EPOCH: i32 = 2;
const CM_TIME_SCALE: i32 = 3;
const CM_TIME_VALUE: i32 = 4;
const BPLIST_HEADER: &[u8] = b"bplist00";
const TRAILER_SIZE: usize = 32;

/// `AppleMakernoteDescriptor.getDescription`.
pub(super) fn describe(directory: &Directory, tag: i32) -> Described {
    Ok(match tag {
        TAG_HDR_IMAGE_TYPE => indexed(
            directory,
            tag,
            3,
            &[Some("HDR Image"), Some("Original Image")],
        ),
        TAG_ACCELERATION_VECTOR => acceleration_vector(directory),
        _ => base(directory, tag),
    })
}

fn acceleration_vector(directory: &Directory) -> Option<String> {
    let [x, y, z] = directory.rationals(TAG_ACCELERATION_VECTOR)? else {
        return None;
    };
    let axis = |value: &Rational, positive: &str, negative: &str| {
        let absolute = Rational::new(
            value.numerator.wrapping_abs(),
            value.denominator.wrapping_abs(),
        );
        let direction = if is_positive(value) {
            positive
        } else {
            negative
        };
        format!(
            "{}g {direction}",
            printf::fixed(absolute.double_value(), 0, 2, false)
        )
    };
    Some(format!(
        "{}, {}, {}",
        axis(x, "left", "right"),
        axis(y, "down", "up"),
        axis(z, "forward", "backward")
    ))
}

/// `Rational.isPositive`.
fn is_positive(value: &Rational) -> bool {
    !value.is_zero() && (value.numerator > 0) == (value.denominator > 0)
}

/// `AppleRunTimeMakernoteDescriptor.getDescription`.
pub(super) fn describe_run_time(directory: &Directory, tag: i32) -> Described {
    match tag {
        CM_TIME_FLAGS => Ok(directory.integer(CM_TIME_FLAGS).map(flags)),
        CM_TIME_VALUE => {
            let (Some(value), Some(scale)) =
                (directory.long(CM_TIME_VALUE), directory.long(CM_TIME_SCALE))
            else {
                return Ok(None);
            };
            if scale == 0 {
                return Err(Unported::new(
                    "an Apple run time of scale zero, whose description divides by it",
                ));
            }
            Ok(Some(format!("{} seconds", value.wrapping_div(scale))))
        }
        _ => Ok(base(directory, tag)),
    }
}

fn flags(value: i32) -> String {
    let mut text = String::from(if value & 0x1 == 1 { "Valid" } else { "Invalid" });
    for (bit, name) in [
        (0x2, ", rounded"),
        (0x4, ", positive infinity"),
        (0x8, ", negative infinity"),
        (0x10, ", indefinite"),
    ] {
        if value & bit != 0 {
            text.push_str(name);
        }
    }
    text
}

/// An object of a binary property list, as `BplistReader` keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Object {
    /// A dictionary's key and value references, in order.
    Dict(Vec<(i8, i8)>),
    String(String),
    Data(Vec<u8>),
    /// An integer of one byte, a `Byte`.
    Byte(i8),
    /// An integer of two bytes, an `Integer`.
    Integer(i32),
    /// An integer of four or eight bytes, a `Long`.
    Long(i64),
}

/// The exception `BplistReader.parse` throws: an `IOException` for data that ends early or holds
/// an object it does not read, which `AppleRunTimeReader` records on the maker note.
#[derive(Debug)]
struct Malformed(String);

/// `AppleRunTimeReader.extract`: the run time `bytes` hold, as a directory after the maker
/// note's, or an error on the maker note.
pub(super) fn extract_run_time(
    bytes: &[u8],
    metadata: &mut Metadata,
    maker_note: usize,
) -> Result<(), Unported> {
    if !bytes.starts_with(BPLIST_HEADER) {
        metadata.directories[maker_note].add_error("Input array is not a bplist");
        return Ok(());
    }
    let objects = match parse(bytes)? {
        Ok(objects) => objects,
        Err(Malformed(message)) => {
            metadata.directories[maker_note]
                .add_error(format!("Error processing TAG_RUN_TIME: {message}"));
            return Ok(());
        }
    };
    let directory = run_time(&objects)?;
    if directory.tags().next().is_some() {
        metadata.add(directory);
    }
    Ok(())
}

/// `processAppleRunTime`: the flags, epoch, time scale and value of a valid time whose flags are
/// a one-byte integer. Flags given as a string, and values not of the types it casts them to,
/// which it parses or throws on, are unported.
fn run_time(objects: &(Vec<Object>, usize)) -> Result<Directory, Unported> {
    let (objects, top) = objects;
    let mut directory = Directory::new(Kind::AppleRunTime);
    let Some(Object::Dict(entries)) = objects.get(*top) else {
        return Ok(directory);
    };
    let object = |reference: i8| {
        usize::try_from(reference)
            .ok()
            .and_then(|index| objects.get(index))
            .ok_or_else(|| Unported::new("an Apple run time referring past its objects"))
    };
    let mut values: Vec<(String, &Object)> = Vec::new();
    for (key, value) in entries {
        let Object::String(key) = object(*key)? else {
            return Err(Unported::new(
                "an Apple run time with a key that is not a string",
            ));
        };
        let value = object(*value)?;
        match values.iter_mut().find(|(known, _)| known == key) {
            Some((_, slot)) => *slot = value,
            None => values.push((key.clone(), value)),
        }
    }
    let value = |name: &str| {
        values
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| *value)
    };
    let cast = || Unported::new("an Apple run time of values its reader casts to other types");
    match value("flags") {
        Some(Object::Byte(flags)) => {
            if flags & 0x1 != 0x1 {
                return Ok(directory);
            }
            let Some(Object::Byte(epoch)) = value("epoch") else {
                return Err(cast());
            };
            let Some(Object::Long(scale)) = value("timescale") else {
                return Err(cast());
            };
            let Some(Object::Long(time)) = value("value") else {
                return Err(cast());
            };
            directory.set(CM_TIME_FLAGS, Value::Int(i32::from(*flags)));
            directory.set(CM_TIME_EPOCH, Value::Int(i32::from(*epoch)));
            directory.set(CM_TIME_SCALE, Value::Long(*scale));
            directory.set(CM_TIME_VALUE, Value::Long(*time));
            Ok(directory)
        }
        Some(Object::String(_)) => Err(Unported::new(
            "an Apple run time whose flags are a string, which its reader parses",
        )),
        _ => Ok(directory),
    }
}

/// A reader of the property list's bytes from an offset, failing past their end as
/// `SequentialByteArrayReader` does.
struct Bytes<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Bytes<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], Malformed> {
        let end = self
            .position
            .checked_add(count)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| Malformed("End of data reached.".to_owned()))?;
        let bytes = &self.data[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn i8(&mut self) -> Result<i8, Malformed> {
        Ok(self.take(1)?[0] as i8)
    }

    fn u16(&mut self) -> Result<u16, Malformed> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, Malformed> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i64(&mut self) -> Result<i64, Malformed> {
        let bytes = self.take(8)?;
        Ok(i64::from_be_bytes(bytes.try_into().expect("eight bytes")))
    }
}

/// `BplistReader.parse`: the objects, and the index of the top one. Offsets and sizes the reader
/// would take as negative, which throw runtime exceptions in Java, are unported.
fn parse(bplist: &[u8]) -> Result<Result<(Vec<Object>, usize), Malformed>, Unported> {
    let unported = |what: &str| Unported::new(format!("an Apple run time property list {what}"));
    if bplist.len() < BPLIST_HEADER.len() + TRAILER_SIZE {
        return Err(unported("shorter than its trailer"));
    }
    let mut trailer = Bytes {
        data: bplist,
        position: bplist.len() - TRAILER_SIZE + 6,
    };
    let parsed = (|| {
        let offset_size = trailer.i8()?;
        trailer.i8()?;
        let count = trailer.i64()?;
        let top = trailer.i64()?;
        let table = trailer.i64()?;
        Ok::<_, Malformed>((offset_size, count, top, table))
    })();
    let (offset_size, count, top, table) = match parsed {
        Ok(trailer) => trailer,
        Err(malformed) => return Ok(Err(malformed)),
    };
    let start = usize::try_from(table.wrapping_add(top) as i32)
        .map_err(|_| unported("whose offset table starts before it"))?;
    let count = usize::try_from(count as i32)
        .ok()
        .filter(|count| *count <= bplist.len())
        .ok_or_else(|| unported("of an object count its reader cannot allocate"))?;
    let top = usize::try_from(top as i32).map_err(|_| unported("whose top object is negative"))?;
    let mut table = Bytes {
        data: bplist,
        position: start,
    };
    let mut offsets = Vec::with_capacity(count);
    for _ in 0..count {
        let offset = match offset_size {
            1 => table.i8().map(i32::from),
            2 => table.u16().map(i32::from),
            _ => Ok(0),
        };
        match offset {
            Ok(offset) => offsets.push(offset),
            Err(malformed) => return Ok(Err(malformed)),
        }
    }
    let mut objects = Vec::with_capacity(count);
    for offset in offsets {
        let position = usize::try_from(offset).map_err(|_| unported("of a negative offset"))?;
        let mut reader = Bytes {
            data: bplist,
            position,
        };
        match object(&mut reader)? {
            Ok(object) => objects.push(object),
            Err(malformed) => return Ok(Err(malformed)),
        }
    }
    if top >= objects.len() {
        return Err(unported("whose top object is past its objects"));
    }
    Ok(Ok((objects, top)))
}

/// One object at the reader's position.
fn object(reader: &mut Bytes) -> Result<Result<Object, Malformed>, Unported> {
    let marker = match reader.i8() {
        Ok(marker) => marker,
        Err(malformed) => return Ok(Err(malformed)),
    };
    let low = usize::from(marker as u8 & 0x0F);
    let read = match (marker as u8) >> 4 {
        0x0D => (|| {
            let keys: Vec<i8> = (0..low).map(|_| reader.i8()).collect::<Result<_, _>>()?;
            let mut entries = Vec::with_capacity(low);
            for key in keys {
                entries.push((key, reader.i8()?));
            }
            Ok(Object::Dict(entries))
        })(),
        0x05 => {
            if low == 0x0F {
                return Err(Unported::new(
                    "an Apple run time string of fifteen or more characters",
                ));
            }
            reader
                .take(low)
                .map(|bytes| Object::String(String::from_utf8_lossy(bytes).into_owned()))
        }
        0x04 => {
            if low == 0x0F {
                return Err(Unported::new("Apple run time data of an extended length"));
            }
            reader.take(low).map(|bytes| Object::Data(bytes.to_vec()))
        }
        0x01 => match low {
            0 => reader.i8().map(Object::Byte),
            1 => reader.u16().map(|value| Object::Integer(i32::from(value))),
            2 => reader.u32().map(|value| Object::Long(i64::from(value))),
            3 => reader.i64().map(Object::Long),
            _ => {
                return Err(Unported::new(
                    "an Apple run time integer of a size its reader skips",
                ))
            }
        },
        _ => Err(Malformed("Un-handled objectFormat encountered".to_owned())),
    };
    Ok(read)
}
