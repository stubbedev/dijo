//! Tika's reading of a OneNote section in the alternative packaging of MS-FSSHTTPB, which
//! OneNote writes for files from OneDrive: the packaging's data element package
//! ([`stream`]), the revision store of its cells and objects ([`store`]), and the walk of each
//! page's objects ([`walk`]), as Tika's `MSOneStoreParser` and `MSOneStorePackage` port the
//! adapter of Microsoft's protocol test suites.
//!
//! The basic objects are read bit by bit, low bit first, as the suite's `BitReader` reads
//! them. GUIDs are read as the GUIDs they are, where Tika makes each the MD5 hash of its sixteen
//! bytes with `UUID.nameUUIDFromBytes`, so that the encryption key root it compares them with
//! never matched (#129).

mod store;
mod stream;
mod walk;

use std::fmt;

use super::file::Thrown;
use crate::formats::xhtml::Xhtml;
use crate::formats::Unported;

pub(super) use store::Package;
pub(super) use walk::Walked;

/// Why the reading stopped: an exception Tika's reading throws, after which it falls back to
/// the legacy dump, or something the port does not reproduce.
#[derive(Debug)]
enum Failure {
    Thrown(Thrown),
    Unported(String),
}

impl From<Thrown> for Failure {
    fn from(thrown: Thrown) -> Self {
        Failure::Thrown(thrown)
    }
}

/// `AlternativePackaging.doDeserializeFromByteArray` and `MSOneStoreParser.parse` of the whole
/// file: the package, or the exception the reading throws, or why the port leaves the file to
/// Tika.
pub(super) fn read(input: &[u8]) -> Result<Result<Package, Thrown>, Unported> {
    match stream::package(input).and_then(|elements| store::parse(&elements)) {
        Ok(package) => Ok(Ok(package)),
        Err(Failure::Thrown(thrown)) => Ok(Err(thrown)),
        Err(Failure::Unported(reason)) => Err(Unported(reason)),
    }
}

/// `MSOneStorePackage.walkTree` of the package: what the walk wrote, or the exception that ends
/// it, after which Tika falls back to the legacy dump.
pub(super) fn walk(package: &Package, xhtml: &mut Xhtml) -> Walked {
    walk::walk(package, xhtml)
}

/// A GUID, by its sixteen bytes in the order it is written as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Uuid([u8; 16]);

impl Uuid {
    /// The null GUID, `GuidUtil.emptyGuid`.
    const EMPTY: Uuid = Uuid([0; 16]);

    /// The GUID stored as `bytes`: its first three fields little-endian, the rest as written.
    fn stored(bytes: [u8; 16]) -> Uuid {
        let order = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
        Uuid(order.map(|at| bytes[at]))
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// What tells extended GUIDs apart: their value and GUID.
type GuidKey = (i32, Uuid);
/// What tells cell IDs apart.
type CellKey = (GuidKey, GuidKey);

/// `ExGuid`: an extended GUID, the empty GUID for the null one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ExGuid {
    /// `type`, which `toString` shows.
    kind: i32,
    value: i32,
    guid: Uuid,
}

impl ExGuid {
    /// `equals`, which leaves the type out.
    fn same(self, other: ExGuid) -> bool {
        self.guid == other.guid && self.value == other.value
    }

    /// The key `equals` and `hashCode` tell extended GUIDs apart by.
    fn key(self) -> GuidKey {
        (self.value, self.guid)
    }
}

impl fmt::Display for ExGuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ExGuid{{type={}, value={}, guid={}}}",
            self.kind, self.value, self.guid
        )
    }
}

/// `CellID`: two extended GUIDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CellId {
    first: ExGuid,
    second: ExGuid,
}

impl CellId {
    /// The key `equals` and `hashCode` tell cell IDs apart by.
    fn key(self) -> CellKey {
        (self.first.key(), self.second.key())
    }
}

impl fmt::Display for CellId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CellID{{extendGUID1={}, extendGUID2={}}}",
            self.first, self.second
        )
    }
}

/// The class names of the exceptions the reading throws, as `Throwable.toString` names them.
const IO_EXCEPTION: &str = "java.io.IOException";
const TIKA_EXCEPTION: &str = "org.apache.tika.exception.TikaException";
const ILLEGAL_ARGUMENT: &str = "java.lang.IllegalArgumentException";
const INVALID_OPERATION: &str = "org.apache.poi.openxml4j.exceptions.InvalidOperationException";
const STREAM_OBJECT_PARSE_ERROR: &str =
    "org.apache.tika.parser.microsoft.onenote.fsshttpb.streamobj.StreamObjectParseErrorException";
const DATA_ELEMENT_PARSE_ERROR: &str =
    "org.apache.tika.parser.microsoft.onenote.fsshttpb.exception.DataElementParseErrorException";

/// An exception of `class` with `message`.
fn thrown(class: &str, message: impl fmt::Display) -> Thrown {
    Thrown(format!("{class}: {message}"))
}

/// The `ArrayIndexOutOfBoundsException` of reading the byte at `index` of an array of `length`
/// bytes, with the message the JVM gives it, where the native image of Tika's path gives one
/// that says its stack trace is imprecise (#129).
fn out_of_bounds(index: usize, length: usize) -> Thrown {
    thrown(
        "java.lang.ArrayIndexOutOfBoundsException",
        format_args!("Index {index} out of bounds for length {length}"),
    )
}

/// An exception of `class` that wraps `cause` as its message and its cause, as
/// `new RuntimeException(cause)` does: its trace without frames.
fn wrapping(class: &str, Thrown(cause): Thrown) -> Thrown {
    let first = cause.lines().next().unwrap_or_default();
    Thrown(format!("{class}: {first}\nCaused by: {cause}"))
}

/// The protocol suite's `BitReader`: the bits of a byte array from a byte on, the low bit of
/// each byte first.
struct Bits<'a> {
    data: &'a [u8],
    /// The bit last read, one before the first while none is.
    offset: i64,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], index: usize) -> Self {
        Bits {
            data,
            offset: index as i64 * 8 - 1,
        }
    }

    /// `moveNext`: whether there is a next bit, moved to.
    fn move_next(&mut self) -> bool {
        self.offset += 1;
        self.offset < self.data.len() as i64 * 8
    }

    /// `getCurrent`.
    fn current(&self) -> bool {
        self.data[(self.offset >> 3) as usize] & (1 << (self.offset & 7)) != 0
    }

    /// `getBytes` of `count` bits, read as a number.
    fn read(&mut self, count: u32) -> Result<u64, Thrown> {
        let mut value = 0u64;
        for bit in 0..count {
            if !self.move_next() {
                return Err(thrown(
                    IO_EXCEPTION,
                    "Unexpected to meet the byte array end.",
                ));
            }
            if self.current() {
                value |= 1 << bit;
            }
        }
        Ok(value)
    }

    /// `readInt32` and `readUInt32`: up to 32 bits, as Java's `int` holds them.
    fn int(&mut self, count: u32) -> Result<i32, Thrown> {
        Ok(self.read(count)? as u32 as i32)
    }

    /// `readGuid`: the GUID of the next sixteen bytes.
    fn guid(&mut self) -> Result<Uuid, Thrown> {
        let mut bytes = [0u8; 16];
        for byte in &mut bytes {
            *byte = self.read(8)? as u8;
        }
        Ok(Uuid::stored(bytes))
    }

    /// How many zero bits come before the next one bit, at most eight: the type of a compact
    /// number or an extended GUID.
    fn leading_zeros(&mut self) -> u32 {
        let mut zeros = 0;
        while zeros < 8 && self.move_next() {
            if self.current() {
                break;
            }
            zeros += 1;
        }
        zeros
    }
}

/// `Compact64bitInt` at `index`: the number and the bytes it takes.
fn compact(data: &[u8], index: usize) -> Result<(u64, usize), Thrown> {
    let mut bits = Bits::new(data, index);
    let (count, length) = match bits.leading_zeros() {
        8 => return Ok((0, 1)),
        7 => (64, 9),
        zeros => (7 * (zeros + 1), zeros as usize + 1),
    };
    Ok((bits.read(count)?, length))
}

/// `ExGuid` at `index`: the extended GUID and the bytes it takes.
fn ex_guid(data: &[u8], index: usize) -> Result<(ExGuid, usize), Thrown> {
    let mut bits = Bits::new(data, index);
    let (count, kind, length) = match bits.leading_zeros() {
        2 => (5, 4, 17),
        5 => (10, 32, 18),
        6 => (17, 64, 19),
        7 => (32, 128, 21),
        8 => {
            return Ok((
                ExGuid {
                    kind: 0,
                    value: 0,
                    guid: Uuid::EMPTY,
                },
                1,
            ))
        }
        _ => {
            return Err(thrown(
                INVALID_OPERATION,
                "Failed to parse the ExGuid, the type value is unexpected",
            ))
        }
    };
    let value = bits.int(count)?;
    let guid = bits.guid()?;
    Ok((ExGuid { kind, value, guid }, length))
}

/// A cursor over the data the basic objects are read from, as `BasicObject.parse` moves an
/// `AtomicInteger`.
struct Cursor<'a> {
    data: &'a [u8],
    index: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8], index: usize) -> Self {
        Cursor { data, index }
    }

    fn compact(&mut self) -> Result<u64, Thrown> {
        let (value, length) = compact(self.data, self.index)?;
        self.index += length;
        Ok(value)
    }

    fn ex_guid(&mut self) -> Result<ExGuid, Thrown> {
        let (value, length) = ex_guid(self.data, self.index)?;
        self.index += length;
        Ok(value)
    }

    /// `SerialNumber`, read to be passed over.
    fn serial_number(&mut self) -> Result<(), Thrown> {
        let mut bits = Bits::new(self.data, self.index);
        match bits.int(8)? {
            0 => self.index += 1,
            128 => {
                bits.guid()?;
                bits.read(64)?;
                self.index += 25;
            }
            kind => {
                return Err(thrown(
                    ILLEGAL_ARGUMENT,
                    format_args!(
                        "Failed to parse SerialNumber object, Expect the type value is either 0 \
                         or 128, but the actual value is {kind}"
                    ),
                ))
            }
        }
        Ok(())
    }

    fn cell_id(&mut self) -> Result<CellId, Thrown> {
        Ok(CellId {
            first: self.ex_guid()?,
            second: self.ex_guid()?,
        })
    }

    /// `ExGUIDArray`.
    fn ex_guids(&mut self) -> Result<Vec<ExGuid>, Thrown> {
        let count = self.compact()?;
        let mut guids = Vec::new();
        for _ in 0..count {
            guids.push(self.ex_guid()?);
        }
        Ok(guids)
    }

    /// `CellIDArray`.
    fn cell_ids(&mut self) -> Result<Vec<CellId>, Thrown> {
        let count = self.compact()?;
        let mut ids = Vec::new();
        for _ in 0..count {
            ids.push(self.cell_id()?);
        }
        Ok(ids)
    }

    /// `BinaryItem`: its length, then as many bytes.
    fn binary(&mut self) -> Result<Vec<u8>, Thrown> {
        let length = self.compact()?;
        let end = usize::try_from(length)
            .ok()
            .and_then(|length| self.index.checked_add(length));
        match end.and_then(|end| self.data.get(self.index..end)) {
            Some(bytes) => {
                self.index += bytes.len();
                Ok(bytes.to_vec())
            }
            None => Err(out_of_bounds(
                self.data.len().max(self.index),
                self.data.len(),
            )),
        }
    }
}

/// `Arrays.copyOfRange(data, from, from + N)`: the bytes from `from`, padded with zeros past
/// the end. From past the end, `System.arraycopy` throws an exception the port does not name.
fn copy_of_range<const N: usize>(data: &[u8], from: usize) -> Result<[u8; N], Failure> {
    if from > data.len() {
        return Err(Failure::Unported(
            "a copy of a byte array from past its end, whose exception the port does not name"
                .to_owned(),
        ));
    }
    let mut bytes = [0u8; N];
    let available = (data.len() - from).min(N);
    bytes[..available].copy_from_slice(&data[from..from + available]);
    Ok(bytes)
}

/// `BitConverter.toUInt32`: the unsigned four bytes from `index`, padded past the end, where
/// Tika widens a signed `int` to its `long` (#129).
fn uint32(data: &[u8], index: usize) -> Result<u32, Failure> {
    Ok(u32::from_le_bytes(copy_of_range::<4>(data, index)?))
}

/// `BitConverter.toInt32`, which fails on an array of fewer than four bytes.
fn int32(data: &[u8], index: usize) -> Result<i32, Failure> {
    if data.len() < 4 {
        return Err(thrown(
            IO_EXCEPTION,
            "The length of the byte array must be at least 4 bytes long.",
        )
        .into());
    }
    Ok(uint32(data, index)? as i32)
}

/// `BitConverter.toUInt16`: the unsigned two bytes from `index`, padded past the end, failing on
/// an array of fewer than two bytes, where Tika reads them signed with `toInt16` (#129).
fn uint16(data: &[u8], index: usize) -> Result<u16, Failure> {
    if data.len() < 2 {
        return Err(thrown(
            IO_EXCEPTION,
            "The length of the byte array must be at least 2 bytes long.",
        )
        .into());
    }
    Ok(u16::from_le_bytes(copy_of_range::<2>(data, index)?))
}

/// `AdapterHelper.readGuid`: the GUID of the sixteen bytes at `index`, padded past the end.
fn read_guid(data: &[u8], index: usize) -> Result<Uuid, Failure> {
    Ok(Uuid::stored(copy_of_range::<16>(data, index)?))
}

/// `text` with the GUID of each embedded document's relationship ID, `_value_guid`, as the UUID
/// `UUID.nameUUIDFromBytes` makes of its stored bytes, and each array index out of bounds with
/// the message the native image of Tika's path gives it.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub(super) fn as_tika_names(text: &str) -> String {
    use md5::{Digest, Md5};
    use regex::{Captures, Regex};

    const IMPRECISE: &str = "Stack trace is imprecise, the top frames are missing and/or have \
                             wrong line numbers. To get precise stack traces, build the image \
                             with option -H:-ReduceImplicitExceptionStackTraceInformation";
    let relationship = Regex::new(
        r"(_-?\d+_)([0-9a-f]{8})-([0-9a-f]{4})-([0-9a-f]{4})-([0-9a-f]{4})-([0-9a-f]{12})\b",
    )
    .expect("the relationship ID pattern is valid");
    let named = relationship.replace_all(text, |groups: &Captures| {
        let hex: String = (2..=6).map(|group| &groups[group]).collect();
        let canonical: Vec<u8> = (0..16)
            .map(|at| u8::from_str_radix(&hex[2 * at..2 * at + 2], 16).expect("hex digits"))
            .collect();
        let order = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
        let stored = order.map(|at| canonical[at]);
        let mut hash: [u8; 16] = Md5::digest(stored).into();
        hash[6] = (hash[6] & 0x0f) | 0x30;
        hash[8] = (hash[8] & 0x3f) | 0x80;
        format!("{}{}", &groups[1], Uuid(hash))
    });
    let index =
        Regex::new(r"ArrayIndexOutOfBoundsException: Index \d+ out of bounds for length \d+")
            .expect("the index pattern is valid");
    index
        .replace_all(
            &named,
            format!("ArrayIndexOutOfBoundsException: {IMPRECISE}"),
        )
        .into_owned()
}

#[cfg(test)]
mod tests;
