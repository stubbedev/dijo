//! The object store of a Word Pro document, as LibreOffice's `Lwp9Reader`, `LwpObjectFactory`
//! and `LwpIndexManager` read it: the file header object (`LWP7`) at 0x10, which names the
//! root document and where the object index starts; the index, a root object (`VO_ROOTOBJINDEX`
//! with leaves, or `VO_ROOTLEAFOBJINDEX` alone) that maps each object's id to its offset and
//! holds the time table compressed ids index into; and the objects, each a header and a body
//! of at most 0xFF00 bytes, run-length compressed where its header says so
//! (`LwpObjectStream::DecompressBuffer`).

use std::borrow::Cow;
use std::collections::BTreeMap;

use super::Malformed;
use crate::text::charset::Charset;

/// Where the stream every offset counts from starts (`LWP_STREAM_BASE`).
const BASE: usize = 0x10;
/// What the file header object is tagged with (`TAG_AMI`).
const FILE_HEADER_TAG: &[u8; 4] = b"LWP7";
/// The first file revision of Word Pro 97, whose object headers and ids are compressed and whose
/// index has a root.
const WORD_PRO_97: u16 = 0x000B;
/// The tags of the index objects.
const ROOT_LEAF_INDEX: u16 = 0xFFFB;
const ROOT_INDEX: u16 = 0xFFFC;
const OBJECT_INDEX: u16 = 0xFFFD;
const LEAF_INDEX: u16 = 0xFFFE;
/// The most leaves a root index has (`m_ChildIndex`).
const MAX_LEAVES: usize = 256;
/// What an object's body, compressed or not, must stay below (`IO_BUFFERSIZE`).
const MAX_BODY: usize = 0xFF00;
/// The bits of an object header's flags.
const VERSION_BITS: u8 = 0x03;
const REFERENCE_COUNT_BITS: u8 = 0x0C;
const HAS_PREVIOUS_OFFSET: u8 = 0x40;
const SIZE_BITS: u8 = 0x30;
const COMPRESSED: u8 = 0x80;

/// The id of an object: the time it was made and a serial number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ObjectId {
    low: u32,
    high: u16,
}

impl ObjectId {
    /// The id these halves make, none for the null id.
    fn of(low: u32, high: u16) -> Option<Self> {
        (low != 0 || high != 0).then_some(Self { low, high })
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:08x}:{}", self.low, self.high)
    }
}

/// An object as it is stored: the type its header gives and its body.
pub(super) struct Object<'a> {
    pub(super) tag: u16,
    pub(super) body: Cow<'a, [u8]>,
}

/// The objects of a document by their ids, and the document the file header names.
pub(super) struct Objects<'a> {
    data: &'a [u8],
    offsets: BTreeMap<ObjectId, u32>,
    times: Vec<u32>,
    root: ObjectId,
}

impl<'a> Objects<'a> {
    /// Reads the file header and the index of `data`, the whole of a document's stream with
    /// its first 16 bytes, of a file revision [`readable`] reads.
    pub(super) fn read(data: &'a [u8]) -> Result<Self, Malformed> {
        let header = Header::read_file_header(data).map_err(|unread| match unread {
            Unread::Revision(revision) => {
                Malformed(format!("the file revision {revision:#x} is not read"))
            }
            Unread::Malformed(malformed) => malformed,
        })?;
        let mut objects = Objects {
            data,
            offsets: BTreeMap::new(),
            times: Vec::new(),
            root: header.document,
        };
        objects.read_index(header.root_index)?;
        Ok(objects)
    }

    /// The root document.
    pub(super) fn root(&self) -> ObjectId {
        self.root
    }

    /// The ids of every object the index lists, in order.
    pub(super) fn ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.offsets.keys().copied()
    }

    /// The type of the object `id`; none for an id the index does not list.
    pub(super) fn tag(&self, id: ObjectId) -> Result<Option<u16>, Malformed> {
        Ok(self.header_of(id)?.map(|(header, _)| header.tag))
    }

    /// The object `id`; none for an id the index does not list, which LibreOffice's
    /// `QueryObject` reads as no object: a document may point to an object it did not keep.
    pub(super) fn object(&self, id: ObjectId) -> Result<Option<Object<'a>>, Malformed> {
        let Some((header, at)) = self.header_of(id)? else {
            return Ok(None);
        };
        let body = self
            .body(&header, at)
            .map_err(|malformed| malformed.within(&format!("object {id}")))?;
        Ok(Some(Object {
            tag: header.tag,
            body,
        }))
    }

    /// The object `id` of the index's ids.
    pub(super) fn indexed(&self, id: ObjectId) -> Result<Object<'a>, Malformed> {
        self.object(id)?
            .ok_or_else(|| Malformed(format!("no object {id} is in the index")))
    }

    /// The fields of `body`, whose compressed ids index into this document's time table.
    pub(super) fn fields<'b>(&'b self, body: &'b [u8]) -> Fields<'b> {
        Fields {
            bytes: body,
            at: 0,
            times: &self.times,
        }
    }

    /// The header of the object `id` and where its body starts, if the index lists it.
    fn header_of(&self, id: ObjectId) -> Result<Option<(Header, usize)>, Malformed> {
        let Some(&offset) = self.offsets.get(&id) else {
            return Ok(None);
        };
        let (header, at) = self
            .header_at(offset)
            .map_err(|malformed| malformed.within(&format!("object {id}")))?;
        if header.id != Some(id) {
            return Err(Malformed(format!(
                "the index places object {id} at {offset}, where object {} is",
                header
                    .id
                    .map_or_else(|| "0".into(), |found| found.to_string())
            )));
        }
        Ok(Some((header, at)))
    }

    /// The header of the object at `offset` from the base, and where its body starts.
    fn header_at(&self, offset: u32) -> Result<(Header, usize), Malformed> {
        let at = BASE + offset as usize;
        let mut fields = Fields {
            bytes: self.data,
            at,
            times: &self.times,
        };
        let header = Header::read(&mut fields)?;
        Ok((header, fields.at))
    }

    /// The body of the object `header` heads, which starts at `at`.
    fn body(&self, header: &Header, at: usize) -> Result<Cow<'a, [u8]>, Malformed> {
        if header.size >= MAX_BODY {
            return Err(Malformed(format!(
                "its body of {} bytes is not below {MAX_BODY}",
                header.size
            )));
        }
        let stored = Fields {
            bytes: self.data,
            at,
            times: &[],
        }
        .peek(header.size)?;
        if header.compressed {
            Ok(Cow::Owned(decompress(stored)?))
        } else {
            Ok(Cow::Borrowed(stored))
        }
    }

    /// Reads the index whose root is at `offset`, and its time table.
    fn read_index(&mut self, offset: u32) -> Result<(), Malformed> {
        let within = |malformed: Malformed| malformed.within("the object index");
        let (header, at) = self.header_at(offset).map_err(within)?;
        let body = self.body(&header, at).map_err(within)?;
        let mut root = Fields {
            bytes: &body,
            at: 0,
            times: &[],
        };
        match header.tag {
            ROOT_LEAF_INDEX => {
                let keys = leaf_keys(&mut root).map_err(within)?;
                self.offsets.extend(keys);
            }
            ROOT_INDEX => {
                let (keys, children) = branch(&mut root).map_err(within)?;
                if children.len() > MAX_LEAVES {
                    return Err(Malformed(format!(
                        "the object index has {} leaves, more than {MAX_LEAVES}",
                        children.len()
                    )));
                }
                self.offsets.extend(keys);
                for child in children {
                    self.read_child_index(child).map_err(within)?;
                }
            }
            tag => {
                return Err(Malformed(format!(
                    "the object index's root at {offset} is of type {tag:#x}"
                )))
            }
        }
        self.times = time_table(&mut root).map_err(within)?;
        Ok(())
    }

    /// Reads the index at `offset` a root index points to: a leaf, or an index of leaves
    /// (`LwpIndexManager::ReadObjIndex`).
    fn read_child_index(&mut self, offset: u32) -> Result<(), Malformed> {
        let (header, at) = self.header_at(offset)?;
        let body = self.body(&header, at)?;
        let mut fields = Fields {
            bytes: &body,
            at: 0,
            times: &[],
        };
        match header.tag {
            LEAF_INDEX => self.offsets.extend(leaf_keys(&mut fields)?),
            OBJECT_INDEX => {
                let (keys, children) = branch(&mut fields)?;
                self.offsets.extend(keys);
                for child in children {
                    let (header, at) = self.header_at(child)?;
                    let body = self.body(&header, at)?;
                    let keys = leaf_keys(&mut Fields {
                        bytes: &body,
                        at: 0,
                        times: &[],
                    })?;
                    self.offsets.extend(keys);
                }
            }
            tag => {
                return Err(Malformed(format!(
                    "the index at {offset} is of type {tag:#x}, no leaf"
                )))
            }
        }
        Ok(())
    }
}

/// Whether the file header of `data` is of a file revision this reader reads: one of Word Pro
/// 97 or later, as LibreOffice reads no earlier one.
pub(super) fn readable(data: &[u8]) -> Result<(), Unread> {
    Header::read_file_header(data).map(|_| ())
}

/// Why a document's objects are not read.
pub(super) enum Unread {
    /// The document is of a file revision this reader does not read.
    Revision(u16),
    /// It is not well-formed.
    Malformed(Malformed),
}

impl From<Malformed> for Unread {
    fn from(malformed: Malformed) -> Self {
        Self::Malformed(malformed)
    }
}

/// What the file header names.
struct FileHeader {
    document: ObjectId,
    root_index: u32,
}

/// An object's header (`LwpObjectHeader`).
struct Header {
    tag: u16,
    id: Option<ObjectId>,
    size: usize,
    compressed: bool,
}

impl Header {
    /// The file header object at the base, in the long header of the revisions before Word
    /// Pro 97, and the file header it holds (`Lwp9Reader::ReadFileHeader`).
    fn read_file_header(data: &[u8]) -> Result<FileHeader, Unread> {
        let mut fields = Fields {
            bytes: data,
            at: BASE,
            times: &[],
        };
        let within = |malformed: Malformed| malformed.within("the file header");
        let tag = fields.take(4).map_err(within)?;
        if tag != FILE_HEADER_TAG {
            return Err(Malformed(format!(
                "the file header at {BASE} is tagged {tag:02x?}, not LWP7"
            ))
            .into());
        }
        let skipped = 6 + 4 + 4 + 4 + 4;
        fields.skip(skipped).map_err(within)?;
        let size = fields.u32().map_err(within)?;
        let header_end = fields.at;
        let _application_revision = fields.u16().map_err(within)?;
        let revision = fields.u16().map_err(within)?;
        if revision < WORD_PRO_97 {
            return Err(Unread::Revision(revision));
        }
        fields.skip(2 + 2 + 2).map_err(within)?;
        let document = fields.full_id().map_err(within)?;
        let root_index = fields.u32().map_err(within)?;
        if fields.at - header_end > size as usize {
            return Err(Malformed(format!(
                "the file header's fields run past its size of {size} bytes"
            ))
            .into());
        }
        let document =
            document.ok_or_else(|| Malformed("the file header names no document".into()))?;
        Ok(FileHeader {
            document,
            root_index,
        })
    }

    /// The compressed header of Word Pro 97 and later.
    fn read(fields: &mut Fields<'_>) -> Result<Self, Malformed> {
        let tag = fields.u16()?;
        let flags = fields.u8()?;
        let id = fields.indexed_id()?;
        let sized = |bits: u8| match bits {
            1 => 1,
            2 => 2,
            _ => 4,
        };
        let version = flags & VERSION_BITS;
        if version != 0 {
            fields.skip(sized(version))?;
        }
        fields.skip(sized((flags & REFERENCE_COUNT_BITS) >> 2))?;
        if flags & HAS_PREVIOUS_OFFSET != 0 {
            fields.skip(4)?;
        }
        let size = match sized((flags & SIZE_BITS) >> 4) {
            1 => usize::from(fields.u8()?),
            2 => usize::from(fields.u16()?),
            _ => fields.u32()? as usize,
        };
        Ok(Header {
            tag,
            id,
            size,
            compressed: flags & COMPRESSED != 0,
        })
    }
}

/// The ids an index lists, each with its object's offset.
type Keys = Vec<(ObjectId, u32)>;

/// The ids and offsets of a leaf index (`ReadLeafData`).
fn leaf_keys(fields: &mut Fields<'_>) -> Result<Keys, Malformed> {
    let count = usize::from(fields.u16()?);
    let ids = key_ids(fields, count)?;
    ids.into_iter().map(|id| Ok((id, fields.u32()?))).collect()
}

/// The ids and offsets of an index with children, and the offsets of its children, one more
/// than its keys (`ReadRootData`, `ReadObjIndexData`).
fn branch(fields: &mut Fields<'_>) -> Result<(Keys, Vec<u32>), Malformed> {
    let count = usize::from(fields.u16()?);
    if count == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    let ids = key_ids(fields, count)?;
    let keys = ids
        .into_iter()
        .map(|id| Ok((id, fields.u32()?)))
        .collect::<Result<Vec<_>, Malformed>>()?;
    let children = (0..=count)
        .map(|_| fields.u32())
        .collect::<Result<Vec<_>, Malformed>>()?;
    Ok((keys, children))
}

/// The `count` ids of an index's keys: the first in full, each other the one before it with
/// its serial number the difference past it, or in full after 255 (`ReadCompressed`).
fn key_ids(fields: &mut Fields<'_>, count: usize) -> Result<Vec<ObjectId>, Malformed> {
    let mut ids: Vec<ObjectId> = Vec::with_capacity(count.min(fields.remaining() / 2));
    for index in 0..count {
        let id = match ids.last() {
            None => fields.full_id()?,
            Some(previous) => match fields.u8()? {
                255 => fields.full_id()?,
                difference => ObjectId::of(
                    previous.low,
                    previous
                        .high
                        .wrapping_add(u16::from(difference))
                        .wrapping_add(1),
                ),
            },
        };
        let id = id.ok_or_else(|| Malformed(format!("key {index} of an index is null")))?;
        ids.push(id);
    }
    Ok(ids)
}

/// The time table compressed ids index into.
fn time_table(fields: &mut Fields<'_>) -> Result<Vec<u32>, Malformed> {
    let count = usize::from(fields.u16()?);
    (0..count).map(|_| fields.u32()).collect()
}

/// An object body as LibreOffice's `DecompressBuffer` expands it: runs of zeros and of literal
/// bytes, by the top two bits of each control byte.
fn decompress(stored: &[u8]) -> Result<Vec<u8>, Malformed> {
    let mut output = Vec::with_capacity(stored.len() * 2);
    let mut at = 0;
    let literals = |output: &mut Vec<u8>, at: &mut usize, count: usize| {
        let run = stored
            .get(*at..*at + count)
            .ok_or_else(|| Malformed("its compressed body ends inside a run".into()))?;
        output.extend_from_slice(run);
        *at += count;
        Ok::<(), Malformed>(())
    };
    while let Some(&control) = stored.get(at) {
        at += 1;
        let count = usize::from(control & 0x3F) + 1;
        match control & 0xC0 {
            0x00 => output.resize(output.len() + count, 0),
            0x40 => {
                let zeros = usize::from((control & 0x38) >> 3) + 1;
                output.resize(output.len() + zeros, 0);
                literals(&mut output, &mut at, usize::from(control & 0x07) + 1)?;
            }
            0x80 => {
                output.push(0);
                literals(&mut output, &mut at, count)?;
            }
            _ => literals(&mut output, &mut at, count)?,
        }
        if output.len() >= MAX_BODY {
            return Err(Malformed(format!(
                "its body decompresses into {MAX_BODY} bytes or more"
            )));
        }
    }
    Ok(output)
}

/// Little-endian reads of an object's fields, each checked to lie in it.
pub(super) struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
    times: &'a [u32],
}

impl<'a> Fields<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }

    /// The next `length` bytes, which are not passed.
    fn peek(&self, length: usize) -> Result<&'a [u8], Malformed> {
        self.at
            .checked_add(length)
            .and_then(|end| self.bytes.get(self.at..end))
            .ok_or_else(|| {
                Malformed(format!(
                    "{length} bytes at {} lie past its end, at {}",
                    self.at,
                    self.bytes.len()
                ))
            })
    }

    pub(super) fn take(&mut self, length: usize) -> Result<&'a [u8], Malformed> {
        let taken = self.peek(length)?;
        self.at += length;
        Ok(taken)
    }

    pub(super) fn skip(&mut self, length: usize) -> Result<(), Malformed> {
        self.take(length).map(|_| ())
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Malformed> {
        Ok(self
            .take(N)?
            .try_into()
            .expect("a slice of the array's length"))
    }

    pub(super) fn u8(&mut self) -> Result<u8, Malformed> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u16(&mut self) -> Result<u16, Malformed> {
        self.array().map(u16::from_le_bytes)
    }

    pub(super) fn u32(&mut self) -> Result<u32, Malformed> {
        self.array().map(u32::from_le_bytes)
    }

    pub(super) fn i32(&mut self) -> Result<i32, Malformed> {
        self.array().map(i32::from_le_bytes)
    }

    /// An id in full, its time and serial number (`LwpObjectID::Read`); none if null.
    fn full_id(&mut self) -> Result<Option<ObjectId>, Malformed> {
        let low = self.u32()?;
        let high = self.u16()?;
        Ok(ObjectId::of(low, high))
    }

    /// An id whose time is given in full or by its place in the time table
    /// (`LwpObjectID::ReadIndexed`); none if null.
    pub(super) fn indexed_id(&mut self) -> Result<Option<ObjectId>, Malformed> {
        let index = self.u8()?;
        let low = match index {
            0 => self.u32()?,
            index => *self.times.get(usize::from(index) - 1).ok_or_else(|| {
                Malformed(format!(
                    "an id's time is entry {index} of a time table of {}",
                    self.times.len()
                ))
            })?,
        };
        let high = self.u16()?;
        Ok(ObjectId::of(low, high))
    }

    /// Skips extra data: words up to a zero one (`SkipExtra`).
    pub(super) fn skip_extra(&mut self) -> Result<(), Malformed> {
        while self.u16()? != 0 {}
        Ok(())
    }

    /// An atom holder's string, in code page 1252 (`LwpAtomHolder::Read`); none when it holds
    /// none, whose remaining bytes it does not take.
    pub(super) fn atom(&mut self) -> Result<Option<String>, Malformed> {
        let size = usize::from(self.u16()?);
        let length = self.u16()?;
        if length == 0 || size < 2 {
            return Ok(None);
        }
        let bytes = self.take(size - 2)?;
        Ok(Some(packed(bytes, Charset::named(WINDOWS_1252))?))
    }
}

/// The code page Word Pro's strings are in when nothing says otherwise.
pub(super) const WINDOWS_1252: &str = "windows-1252";

/// The text of `bytes` as `LwpTools::QuickReadUnicode` reads it: without a zero byte, all of
/// it in `charset`; with one, runs in `charset` and runs of UTF-16 code units, each run ended
/// by a zero byte or a zero unit.
pub(super) fn packed(bytes: &[u8], charset: Charset) -> Result<String, Malformed> {
    let decode = |run: &[u8]| {
        charset
            .decode(run)
            .ok_or_else(|| Malformed(format!("the charset {} is not decoded", charset.name())))
    };
    if !bytes.contains(&0) {
        return decode(bytes);
    }
    let mut text = String::with_capacity(bytes.len());
    let mut at = 0;
    let mut wide = false;
    while at < bytes.len() {
        if wide {
            let mut units = Vec::new();
            loop {
                let pair = bytes
                    .get(at..at + 2)
                    .ok_or_else(|| Malformed("a string ends inside a UTF-16 code unit".into()))?;
                at += 2;
                let unit = u16::from_le_bytes([pair[0], pair[1]]);
                if unit == 0 || at >= bytes.len() {
                    if unit != 0 {
                        units.push(unit);
                    }
                    break;
                }
                units.push(unit);
            }
            text.push_str(&String::from_utf16_lossy(&units));
        } else {
            let end = bytes[at..]
                .iter()
                .position(|&byte| byte == 0)
                .map_or(bytes.len(), |zero| at + zero);
            text.push_str(&decode(&bytes[at..end])?);
            at = end + 1;
        }
        wide = !wide;
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompresses_runs_of_zeros_and_literals() {
        let stored = [0x02, 0x49, b'a', b'b', 0x81, b'c', b'd', 0xC0, b'e'];
        assert_eq!(
            decompress(&stored).unwrap(),
            [0, 0, 0, 0, 0, b'a', b'b', 0, b'c', b'd', b'e']
        );
    }

    #[test]
    fn fails_a_body_that_decompresses_too_far() {
        let stored = [0x3F; 1100];
        let error = decompress(&stored).unwrap_err();
        assert!(error.0.contains("or more"), "{error:?}");
    }

    #[test]
    fn reads_packed_strings_of_bytes_and_utf16() {
        let charset = Charset::named(WINDOWS_1252);
        assert_eq!(packed(b"caf\xe9", charset).unwrap(), "caf\u{e9}");
        let bytes = [b'a', 0, 0x3B, 0x04, 0, 0, b'b'];
        assert_eq!(packed(&bytes, charset).unwrap(), "a\u{43b}b");
    }

    #[test]
    fn reads_the_ids_of_an_index_compressed_against_the_one_before() {
        let mut bytes = vec![];
        bytes.extend_from_slice(&7u32.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.push(0);
        bytes.push(255);
        bytes.extend_from_slice(&9u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        let mut fields = Fields {
            bytes: &bytes,
            at: 0,
            times: &[],
        };
        let ids = key_ids(&mut fields, 3).unwrap();
        assert_eq!(
            ids,
            [
                ObjectId { low: 7, high: 3 },
                ObjectId { low: 7, high: 4 },
                ObjectId { low: 9, high: 1 }
            ]
        );
    }
}
