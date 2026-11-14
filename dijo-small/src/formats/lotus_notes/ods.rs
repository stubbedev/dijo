//! The on-disk structure of a Notes database, little-endian throughout, as libnsfdb reads it.
//!
//! The file starts with a 1024-byte database header: its signature 0x1A 0x00 and size, the
//! database information of the ODS version, the information buffer whose first line is the
//! title, and the positions of up to four superblock copies and two bucket descriptor block
//! copies, the newest of each by its write count read. File positions are kept in units of 256
//! bytes. The superblock lists the summary and non-summary buckets in its pages of bucket
//! descriptors; the bucket descriptor block lists the RRV buckets, each flagged as of data or
//! non-data notes, and the unique name key table of the names and types of the items. An RRV
//! bucket maps the note identifiers from its first one, four apart, to a bucket and a slot of
//! it (a BSID) or to a file position, where an object such as an attachment lies. A bucket
//! keeps its slots' offsets and sizes in an index from its end back.

use super::cx;
use super::note::{FileObject, Header, NonSummaryAt, Note, NOTE_SIGNATURE};
use super::{offset, span, u16_at, u32_at, Problem};
use crate::formats::lmbcs;

const FILE_SIGNATURE: u16 = 0x001A;
const DATABASE_HEADER_SIZE: usize = 1024;
const VERSION_AT: usize = 6;
const RRV_BUCKET_SIZE_AT: usize = 76;
const INFORMATION_BUFFER_AT: usize = 200;
const INFORMATION_BUFFER_SIZE: usize = 128;
/// The superblocks' positions and sizes, in that order.
const SUPERBLOCKS_AT: usize = 560;
const SUPERBLOCK_COPIES: usize = 4;
/// The bucket descriptor blocks' sizes and positions, in that order.
const DESCRIPTOR_BLOCKS_AT: usize = 624;
const DESCRIPTOR_BLOCK_COPIES: usize = 2;
/// The last ODS version whose RRV entries are 4-byte file positions, which libnsfdb does not
/// read.
const LAST_SHORT_RRV_VERSION: u32 = 0x15;

/// The compression type of CX compression.
const CX: u16 = 1;
const FOOTER_SIZE: usize = 12;

const SUPERBLOCK_SIGNATURE: u16 = 0x000E;
const SUPERBLOCK_HEADER_SIZE: usize = 100;
const SUMMARY_PAGE_SIZE: usize = 8206;
const SUMMARY_DESCRIPTORS_AT: usize = 224;
const SUMMARY_DESCRIPTOR_SIZE: usize = 14;
const NON_SUMMARY_PAGE_SIZE: usize = 8198;
const NON_SUMMARY_DESCRIPTORS_AT: usize = 70;
const NON_SUMMARY_DESCRIPTOR_SIZE: usize = 6;

const DESCRIPTOR_BLOCK_SIGNATURE: u16 = 0x0001;
const DESCRIPTOR_BLOCK_VERSION: u16 = 2;
const DESCRIPTOR_BLOCK_HEADER_SIZE: usize = 66;
const DESCRIPTOR_BLOCK_CHECKSUM_AT: usize = 54;
const RRV_DESCRIPTOR_SIZE: usize = 8;
const NAME_KEY_SIZE: usize = 10;
/// The most a bucket descriptor block is read to: far more than the 65536 names its items can
/// refer to and the RRV buckets of the largest databases take.
const MAX_DESCRIPTOR_BLOCK: usize = 16 << 20;

const RRV_SIGNATURE: [u8; 2] = [0x06, 0x20];
const RRV_HEADER_SIZE: usize = 32;
const RRV_ENTRY_SIZE: usize = 8;
const NON_DATA_FLAG: u32 = 1;
/// The flag of a bucket slot identifier (BSID), of a bucket and a slot, rather than a file
/// position.
pub(super) const BSID_FLAG: u32 = 0x8000_0000;
/// The bucket of a BSID, counted from 1.
pub(super) const BUCKET_INDEX_MASK: u32 = 0x00FF_FFFF;
const SLOT_MASK: u32 = 0x07FF;

const BUCKET_SIGNATURE: [u8; 2] = [0x02, 0x42];
const BUCKET_HEADER_SIZE: usize = 66;

const OBJECT_SIGNATURE: u16 = 0x0007;
const OBJECT_HEADER_SIZE: usize = 22;

/// The name and type of the items of a unique name key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Name {
    pub(super) text: String,
    /// The item type, its class in the high byte.
    pub(super) kind: u16,
}

/// An RRV bucket: where it lies and the identifier of its first entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RrvBucket {
    at: usize,
    first: u32,
    data: bool,
}

/// Where an RRV entry puts its note or object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rrv {
    Unused,
    /// A slot of a summary bucket, both counted from 1.
    Slot {
        bucket: u32,
        slot: u16,
    },
    Position(usize),
}

/// A data note's place: its identifier and its summary bucket and slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NotePlace {
    id: u64,
    bucket: u32,
    slot: u16,
}

/// A database, read as far as its descriptors.
pub(super) struct Database<'a> {
    input: &'a [u8],
    pub(super) version: u32,
    pub(super) title: String,
    summary: Vec<usize>,
    non_summary: Vec<usize>,
    /// By their first identifiers, whose ranges do not overlap.
    rrv_buckets: Vec<RrvBucket>,
    rrv_entries: usize,
    names: Vec<Name>,
}

/// The XOR of the little-endian 32-bit words of `bytes`, the last one filled up with zeros.
fn checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0, |sum, word| {
        let mut padded = [0; 4];
        padded[..word.len()].copy_from_slice(word);
        sum ^ u32::from_le_bytes(padded)
    })
}

/// The stream of a compressed block's `body`: its length and then itself.
fn stream<'b>(body: &'b [u8], what: &str) -> Result<&'b [u8], Problem> {
    let length = u32_at(body, 0, what)?;
    span(body, 4, length as usize, what)
}

/// The name of the compression type `kind`.
fn compression(kind: u16) -> String {
    match kind {
        0 => "no compression".to_owned(),
        2 => "LZ1".to_owned(),
        kind => format!("compression type {kind}"),
    }
}

/// The bucket positions of a superblock: of its summary buckets and its non-summary buckets.
struct Buckets {
    summary: Vec<usize>,
    non_summary: Vec<usize>,
}

/// The superblock `region` holds, with its write count, unless it holds none whole.
fn superblock(region: &[u8]) -> Result<Option<(u32, Buckets)>, Problem> {
    let what = "the superblock";
    if u16_at(region, 0, what)? != SUPERBLOCK_SIGNATURE {
        return Ok(None);
    }
    let stored = u32_at(region, 64, what)?;
    let block = span(region, 0, stored as usize, what)?;
    let Some(body) = block.get(SUPERBLOCK_HEADER_SIZE..block.len().saturating_sub(FOOTER_SIZE))
    else {
        return Err(Problem::Malformed(format!(
            "the superblock of {stored} bytes is shorter than its header and footer"
        )));
    };
    if checksum(body) != u32_at(block, block.len() - 4, what)? {
        return Ok(None);
    }
    let kind = u16_at(region, 68, what)?;
    if kind != CX {
        return Err(Problem::Unread(format!(
            "a superblock of {}, of which libnsfdb reads only CX compression",
            compression(kind)
        )));
    }
    let declared = u32_at(region, 10, what)?;
    let summary_count = u32_at(region, 14, what)?;
    let non_summary_count = u32_at(region, 18, what)?;
    let write_count = u32_at(region, 60, what)?;
    let summary_pages = u32_at(region, 70, what)?;
    let non_summary_pages = u32_at(region, 74, what)?;
    if summary_pages > 1 || non_summary_pages > 1 {
        return Err(Problem::Unread(format!(
            "a superblock of {summary_pages} pages of summary bucket descriptors and \
             {non_summary_pages} of non-summary ones, of which libnsfdb reads one each"
        )));
    }
    let summary_page = usize::from(summary_pages == 1) * SUMMARY_PAGE_SIZE;
    let wanted = summary_page + usize::from(non_summary_pages == 1) * NON_SUMMARY_PAGE_SIZE;
    if usize::try_from(declared).is_ok_and(|declared| declared < wanted) {
        return Err(Problem::Malformed(format!(
            "the superblock holds {declared} bytes, short of its pages of bucket descriptors"
        )));
    }
    let data = cx::decompress(stream(body, what)?, wanted)?;
    let summary = positions(
        &data[..summary_page],
        summary_count,
        SUMMARY_DESCRIPTORS_AT,
        SUMMARY_DESCRIPTOR_SIZE,
        "summary",
    )?;
    let non_summary = positions(
        &data[summary_page..],
        non_summary_count,
        NON_SUMMARY_DESCRIPTORS_AT,
        NON_SUMMARY_DESCRIPTOR_SIZE,
        "non-summary",
    )?;
    Ok(Some((
        write_count,
        Buckets {
            summary,
            non_summary,
        },
    )))
}

/// The positions of the `count` buckets a page of bucket descriptors lists from `at`, each
/// `size` bytes long and starting with the bucket's file position.
fn positions(
    page: &[u8],
    count: u32,
    at: usize,
    size: usize,
    kind: &str,
) -> Result<Vec<usize>, Problem> {
    let room = page.len().saturating_sub(at) / size;
    let count = usize::try_from(count)
        .ok()
        .filter(|&count| count <= room)
        .ok_or_else(|| {
            Problem::Malformed(format!(
                "the superblock lists {count} {kind} buckets, more than its page of {room}"
            ))
        })?;
    (0..count)
        .map(|index| offset(u32_at(page, at + index * size, "a bucket descriptor")?))
        .collect()
}

/// What a bucket descriptor block holds: the RRV buckets' positions, with the non-data flag in
/// their lowest bit, and first identifiers, and the names of the items.
struct Descriptors {
    rrv_buckets: Vec<(u32, u32)>,
    names: Vec<Name>,
}

/// The bucket descriptor block `region` holds, with its write count, unless it holds none
/// whole.
fn descriptor_block(region: &[u8]) -> Result<Option<(u32, Descriptors)>, Problem> {
    let what = "the bucket descriptor block";
    let header = span(region, 0, DESCRIPTOR_BLOCK_HEADER_SIZE, what)?;
    if u16_at(header, 0, what)? != DESCRIPTOR_BLOCK_SIGNATURE {
        return Ok(None);
    }
    let checked = checksum(&header[..DESCRIPTOR_BLOCK_CHECKSUM_AT])
        ^ checksum(&header[DESCRIPTOR_BLOCK_CHECKSUM_AT + 4..]);
    if checked != u32_at(header, DESCRIPTOR_BLOCK_CHECKSUM_AT, what)? {
        return Ok(None);
    }
    let stored = u32_at(header, 14, what)?;
    let block = span(region, 0, stored as usize, what)?;
    let Some(body) =
        block.get(DESCRIPTOR_BLOCK_HEADER_SIZE..block.len().saturating_sub(FOOTER_SIZE))
    else {
        return Err(Problem::Malformed(format!(
            "the bucket descriptor block of {stored} bytes is shorter than its header and footer"
        )));
    };
    if checksum(body) != u32_at(block, block.len() - 4, what)? {
        return Ok(None);
    }
    let version = u16_at(header, 2, what)?;
    if version != DESCRIPTOR_BLOCK_VERSION {
        return Err(Problem::Unread(format!(
            "a bucket descriptor block of version {version}, of which libnsfdb reads version 2"
        )));
    }
    let kind = u16_at(header, 4, what)?;
    if kind != CX {
        return Err(Problem::Unread(format!(
            "a bucket descriptor block of {}, of which libnsfdb reads only CX compression",
            compression(kind)
        )));
    }
    let declared = u64::from(u32_at(header, 6, what)?);
    let write_count = u32_at(header, 10, what)?;
    let name_count = u64::from(u32_at(header, 26, what)?);
    let text_size = u32_at(header, 34, what)?;
    let rrv_count = u64::from(u32_at(header, 38, what)?);
    let wanted = rrv_count * RRV_DESCRIPTOR_SIZE as u64
        + name_count * NAME_KEY_SIZE as u64
        + 4
        + u64::from(text_size);
    if wanted > declared {
        return Err(Problem::Malformed(format!(
            "the bucket descriptor block holds {declared} bytes, short of the {wanted} of its \
             {rrv_count} RRV buckets and {name_count} names"
        )));
    }
    let wanted = usize::try_from(wanted)
        .ok()
        .filter(|&wanted| wanted <= MAX_DESCRIPTOR_BLOCK)
        .ok_or_else(|| {
            Problem::Unread(format!(
                "a bucket descriptor block of {wanted} bytes, more than the {MAX_DESCRIPTOR_BLOCK} \
                 the port reads"
            ))
        })?;
    let data = cx::decompress(stream(body, what)?, wanted)?;
    let names_at = rrv_count as usize * RRV_DESCRIPTOR_SIZE;
    let rrv_buckets = data[..names_at]
        .as_chunks::<RRV_DESCRIPTOR_SIZE>()
        .0
        .iter()
        .map(|&[a, b, c, d, e, f, g, h]| {
            (
                u32::from_le_bytes([a, b, c, d]),
                u32::from_le_bytes([e, f, g, h]),
            )
        })
        .collect();
    let names = names(&data[names_at..], text_size)?;
    Ok(Some((write_count, Descriptors { rrv_buckets, names })))
}

/// The unique name key table `table`, whose names take `text_size` bytes: its entries, then
/// the size of the names and the names.
fn names(table: &[u8], text_size: u32) -> Result<Vec<Name>, Problem> {
    let what = "the unique name key table";
    let entries_size = table.len() - 4 - text_size as usize;
    let (entries, text) = table.split_at(entries_size);
    if u32_at(text, 0, what)? != text_size {
        return Err(Problem::Malformed(format!(
            "the unique name key table gives its names {} bytes, not the {text_size} of its \
             header",
            u32_at(text, 0, what)?
        )));
    }
    let text = &text[4..];
    entries
        .as_chunks::<NAME_KEY_SIZE>()
        .0
        .iter()
        .map(|&[a, b, c, d, e, f, kind, class, ..]| {
            let at = u32::from_le_bytes([a, b, c, d]) as usize;
            let length = usize::from(u16::from_le_bytes([e, f]));
            Ok(Name {
                text: lmbcs::decode(span(text, at, length, what)?, lmbcs::LATIN),
                kind: u16::from_le_bytes([kind, class]),
            })
        })
        .collect()
}

/// The newest of the `copies` listed from `listed_at` of the database header, `region` read
/// from where each lies, which `read` reads with its write count unless it holds none whole.
fn newest<T>(
    input: &[u8],
    listed_at: usize,
    copies: usize,
    size_first: bool,
    what: &str,
    read: impl Fn(&[u8]) -> Result<Option<(u32, T)>, Problem>,
) -> Result<T, Problem> {
    let mut newest: Option<(u32, T)> = None;
    for copy in 0..copies {
        let first = u32_at(input, listed_at + copy * 8, "the database header")?;
        let second = u32_at(input, listed_at + copy * 8 + 4, "the database header")?;
        let (size, position) = match size_first {
            true => (first, second),
            false => (second, first),
        };
        if size == 0 {
            continue;
        }
        let region = span(input, offset(position)?, size as usize, what)?;
        if let Some((count, read)) = read(region)? {
            if newest.as_ref().is_none_or(|(newest, _)| count > *newest) {
                newest = Some((count, read));
            }
        }
    }
    newest
        .map(|(_, read)| read)
        .ok_or_else(|| Problem::Malformed(format!("the database holds no whole copy of {what}")))
}

/// The title the information buffer `buffer` starts with, up to its line break.
fn title(buffer: &[u8]) -> String {
    let end = buffer
        .iter()
        .position(|&byte| byte == b'\n' || byte == 0)
        .unwrap_or(buffer.len());
    lmbcs::decode(&buffer[..end], lmbcs::LATIN)
        .trim()
        .to_owned()
}

impl<'a> Database<'a> {
    /// Reads the database header and the newest whole superblock and bucket descriptor block of
    /// `input`, and checks its RRV buckets.
    pub(super) fn open(input: &'a [u8]) -> Result<Self, Problem> {
        let what = "the database header";
        if u16_at(input, 0, what)? != FILE_SIGNATURE
            || u32_at(input, 2, what)? as usize != DATABASE_HEADER_SIZE
        {
            return Err(Problem::Malformed(
                "the file does not start with the header of a Notes database".to_owned(),
            ));
        }
        let header = span(input, 0, DATABASE_HEADER_SIZE, what)?;
        let version = u32_at(header, VERSION_AT, what)?;
        if version <= LAST_SHORT_RRV_VERSION {
            return Err(Problem::Unread(format!(
                "a database of ODS {version}, of Notes 4 or before, whose record relocation \
                 vectors libnsfdb does not read either"
            )));
        }
        let rrv_bucket_size = usize::from(u16_at(header, RRV_BUCKET_SIZE_AT, what)?);
        let rrv_entries = rrv_bucket_size
            .checked_sub(RRV_HEADER_SIZE)
            .filter(|entries| entries % RRV_ENTRY_SIZE == 0 && *entries > 0)
            .ok_or_else(|| Problem::Malformed(format!("RRV buckets of {rrv_bucket_size} bytes")))?
            / RRV_ENTRY_SIZE;
        let buckets = newest(
            input,
            SUPERBLOCKS_AT,
            SUPERBLOCK_COPIES,
            false,
            "the superblock",
            superblock,
        )?;
        let descriptors = newest(
            input,
            DESCRIPTOR_BLOCKS_AT,
            DESCRIPTOR_BLOCK_COPIES,
            true,
            "the bucket descriptor block",
            descriptor_block,
        )?;
        let mut database = Database {
            input,
            version,
            title: title(&header[INFORMATION_BUFFER_AT..][..INFORMATION_BUFFER_SIZE]),
            summary: buckets.summary,
            non_summary: buckets.non_summary,
            rrv_buckets: Vec::with_capacity(descriptors.rrv_buckets.len()),
            rrv_entries,
            names: descriptors.names,
        };
        for (position, first) in descriptors.rrv_buckets {
            database.add_rrv_bucket(position, first, rrv_bucket_size)?;
        }
        database.rrv_buckets.sort_by_key(|bucket| bucket.first);
        let covered = 4 * database.rrv_entries as u64;
        for pair in database.rrv_buckets.windows(2) {
            if u64::from(pair[0].first) + covered > u64::from(pair[1].first) {
                return Err(Problem::Malformed(format!(
                    "the RRV buckets from {} and from {} overlap",
                    pair[0].first, pair[1].first
                )));
            }
        }
        Ok(database)
    }

    /// Adds the RRV bucket at `position`, with the non-data flag, from identifier `first`, of
    /// `size` bytes, after checking it, and that a file position in it of a data note is an
    /// object's.
    fn add_rrv_bucket(&mut self, position: u32, first: u32, size: usize) -> Result<(), Problem> {
        let what = "an RRV bucket";
        let at = offset(position & !NON_DATA_FLAG)?;
        let bucket = span(self.input, at, size, what)?;
        if bucket[..2] != RRV_SIGNATURE || u32_at(bucket, 6, what)? != first {
            return Err(Problem::Malformed(format!(
                "the RRV bucket at {at} is not the one of the identifiers from {first} its \
                 descriptor names"
            )));
        }
        if checksum(&bucket[RRV_HEADER_SIZE..]) != u32_at(bucket, 18, what)? {
            return Err(Problem::Malformed(format!(
                "the RRV bucket at {at} does not match its checksum"
            )));
        }
        let bucket = RrvBucket {
            at,
            first,
            data: position & NON_DATA_FLAG == 0,
        };
        if bucket.data {
            for index in 0..self.rrv_entries {
                if let Rrv::Position(at) = self.entry(bucket, index)? {
                    self.object(at)?;
                }
            }
        }
        self.rrv_buckets.push(bucket);
        Ok(())
    }

    /// The entry `index` of the RRV bucket `bucket`.
    fn entry(&self, bucket: RrvBucket, index: usize) -> Result<Rrv, Problem> {
        let at = bucket.at + RRV_HEADER_SIZE + index * RRV_ENTRY_SIZE;
        let place = u32_at(self.input, at, "an RRV entry")?;
        let slot = u32_at(self.input, at + 4, "an RRV entry")?;
        if place & BSID_FLAG != 0 {
            let bucket = place & BUCKET_INDEX_MASK;
            return Ok(match bucket {
                0 | BUCKET_INDEX_MASK => Rrv::Unused,
                bucket => Rrv::Slot {
                    bucket,
                    slot: (slot & SLOT_MASK) as u16,
                },
            });
        }
        Ok(match place {
            0 | 0x7FFF_FFFF => Rrv::Unused,
            position => Rrv::Position(offset(position)?),
        })
    }

    /// The data of the object at `at`.
    fn object(&self, at: usize) -> Result<&'a [u8], Problem> {
        let what = "an object";
        match u16_at(self.input, at, what)? {
            OBJECT_SIGNATURE => {}
            NOTE_SIGNATURE => {
                return Err(Problem::Unread(
                    "a document stored at a file position of its own rather than in a summary \
                     bucket, which libnsfdb does not read either"
                        .to_owned(),
                ))
            }
            signature => {
                return Err(Problem::Malformed(format!(
                    "the RRV entry of the file position {at} names a record of signature \
                     {signature:#06x}"
                )))
            }
        }
        let size = u32_at(self.input, at + 2, what)?;
        span(self.input, at, size as usize, what)?
            .get(OBJECT_HEADER_SIZE..)
            .ok_or_else(|| Problem::Malformed(format!("the object at {at} of {size} bytes")))
    }

    pub(super) fn names(&self) -> &[Name] {
        &self.names
    }

    /// The places of the data notes, in the order of their identifiers.
    fn places(&self) -> impl Iterator<Item = Result<NotePlace, Problem>> + '_ {
        self.rrv_buckets
            .iter()
            .filter(|bucket| bucket.data)
            .flat_map(move |&bucket| (0..self.rrv_entries).map(move |index| (bucket, index)))
            .filter_map(move |(bucket, index)| match self.entry(bucket, index) {
                Ok(Rrv::Slot { bucket: at, slot }) => Some(Ok(NotePlace {
                    id: u64::from(bucket.first) + 4 * index as u64,
                    bucket: at,
                    slot,
                })),
                Ok(Rrv::Unused | Rrv::Position(_)) => None,
                Err(problem) => Some(Err(problem)),
            })
    }

    /// The data notes in the order of their identifiers, none for a deleted note's stub.
    pub(super) fn notes(&self) -> impl Iterator<Item = Result<Option<Note<'a>>, Problem>> + '_ {
        self.places().map(move |place| self.note(place?))
    }

    /// The note at `place`, none for a deleted note's stub.
    fn note(&self, place: NotePlace) -> Result<Option<Note<'a>>, Problem> {
        let record = self.slot(&self.summary, "summary", place.bucket, place.slot)?;
        let Some(header) = Header::read(record).map_err(|problem| match problem {
            Problem::Malformed(reason) => {
                Problem::Malformed(format!("the note {}: {reason}", place.id))
            }
            problem => problem,
        })?
        else {
            return Ok(None);
        };
        let values = match header.non_summary_at {
            None => &[][..],
            Some(at) => self.non_summary_values(at, header.non_summary_size)?,
        };
        Ok(Some(Note::new(header, values)))
    }

    /// The slot `slot` of the bucket `bucket` of `kind` at the positions `list`, both counted
    /// from 1.
    fn slot(
        &self,
        list: &[usize],
        kind: &str,
        bucket: u32,
        slot: u16,
    ) -> Result<&'a [u8], Problem> {
        let what = "a bucket";
        let at = (bucket as usize)
            .checked_sub(1)
            .and_then(|index| list.get(index))
            .copied()
            .ok_or_else(|| {
                Problem::Malformed(format!(
                    "a note lies in {kind} bucket {bucket} of the {}",
                    list.len()
                ))
            })?;
        let header = span(self.input, at, BUCKET_HEADER_SIZE, what)?;
        if header[..2] != BUCKET_SIGNATURE {
            return Err(Problem::Malformed(format!(
                "the {kind} bucket at {at} does not start with a bucket's signature"
            )));
        }
        let size = u32_at(header, 6, what)?;
        let data = span(self.input, at, size as usize, what)?;
        let slots = u32_at(header, 44, what)? as usize;
        let footer = u32_at(header, 50, what)? as usize;
        let start = data
            .len()
            .checked_sub(footer)
            .and_then(|end| end.checked_sub(slots.checked_mul(4)?))
            .filter(|&start| start >= BUCKET_HEADER_SIZE)
            .ok_or_else(|| {
                Problem::Malformed(format!(
                    "the {kind} bucket at {at} of {size} bytes holds no index of {slots} slots"
                ))
            })?;
        let index = usize::from(slot)
            .checked_sub(1)
            .filter(|&index| index < slots)
            .ok_or_else(|| {
                Problem::Malformed(format!(
                    "a note lies in slot {slot} of the {slots} of the {kind} bucket at {at}"
                ))
            })?;
        let entry = start + (slots - 1 - index) * 4;
        let place = usize::from(u16_at(data, entry, what)?);
        let length = usize::from(u16_at(data, entry + 2, what)?);
        data.get(place..place + length)
            .filter(|_| place >= BUCKET_HEADER_SIZE && place + length <= start)
            .ok_or_else(|| {
                Problem::Malformed(format!(
                    "the slot {slot} of the {kind} bucket at {at} lies outside its data"
                ))
            })
    }

    /// The item values of the non-summary record of `size` bytes at `at`.
    fn non_summary_values(&self, at: NonSummaryAt, size: u32) -> Result<&'a [u8], Problem> {
        let record = match at {
            NonSummaryAt::Slot { bucket, slot } => {
                self.slot(&self.non_summary, "non-summary", bucket, slot)?
            }
            NonSummaryAt::Position(position) => {
                span(self.input, position, size as usize, "a non-summary record")?
            }
        };
        Header::non_summary_values(record, size)
    }

    /// Checks that the data notes are read: that none is encrypted, that each lies in a slot of
    /// its own, and that each attachment is whole and uncompressed in an object of its own.
    pub(super) fn check(&self) -> Result<(), Problem> {
        let mut summary_slots = vec![[0u64; 32]; self.summary.len()];
        let mut non_summary_slots = vec![[0u64; 2]; self.non_summary.len()];
        let mut non_summary_positions = Vec::new();
        let mut objects = Vec::new();
        for place in self.places() {
            let place = place?;
            let note = self.note(place)?;
            claim(&mut summary_slots, place.bucket, place.slot, "summary")?;
            let Some(note) = note else {
                continue;
            };
            match note.non_summary_at {
                Some(NonSummaryAt::Slot { bucket, slot }) => {
                    claim(&mut non_summary_slots, bucket, slot, "non-summary")?
                }
                Some(NonSummaryAt::Position(position)) => non_summary_positions.push(position),
                None => {}
            }
            for item in note.items(self.names()) {
                let item = item?;
                if item.is_sealed() {
                    return Err(Problem::Encrypted(
                        "a Lotus Notes database of documents encrypted for their readers"
                            .to_owned(),
                    ));
                }
                if let Some(attachment) = item.attachment()? {
                    self.attachment(&attachment)?;
                    objects.push(attachment.object);
                }
            }
        }
        distinct(non_summary_positions, "non-summary record at")?;
        distinct(objects, "attachment object")
    }

    /// The file `attachment` holds.
    pub(super) fn attachment(&self, attachment: &FileObject) -> Result<&'a [u8], Problem> {
        attachment.compression.check(&attachment.name)?;
        let id = attachment.object;
        let index = self
            .rrv_buckets
            .partition_point(|bucket| bucket.first <= id);
        let entry = match index.checked_sub(1).map(|index| self.rrv_buckets[index]) {
            Some(bucket) => {
                let distance = id - bucket.first;
                let index = (distance / 4) as usize;
                match distance.is_multiple_of(4) && index < self.rrv_entries {
                    true => self.entry(bucket, index)?,
                    false => Rrv::Unused,
                }
            }
            None => Rrv::Unused,
        };
        let data = match entry {
            Rrv::Position(at) => self.object(at)?,
            Rrv::Slot { .. } => {
                return Err(Problem::Unread(format!(
                    "the attachment {} kept in a bucket rather than in an object of its own, \
                     which no documentation describes",
                    attachment.name
                )))
            }
            Rrv::Unused => {
                return Err(Problem::Malformed(format!(
                    "the attachment {} is the object {id}, which the database does not hold",
                    attachment.name
                )))
            }
        };
        if data.len() as u64 != u64::from(attachment.size) {
            return Err(Problem::Unread(format!(
                "the attachment {} of {} bytes whose object holds {}, in parts the port does \
                 not read",
                attachment.name,
                attachment.size,
                data.len()
            )));
        }
        Ok(data)
    }
}

/// Marks the slot `slot` of the bucket `bucket` of `kind` as taken in `taken`, which a slot
/// taken twice fails.
fn claim<const WORDS: usize>(
    taken: &mut [[u64; WORDS]],
    bucket: u32,
    slot: u16,
    kind: &str,
) -> Result<(), Problem> {
    let index = usize::from(slot);
    let words = (bucket as usize)
        .checked_sub(1)
        .and_then(|bucket| taken.get_mut(bucket))
        .filter(|_| index < WORDS * 64)
        .ok_or_else(|| {
            Problem::Malformed(format!(
                "a note lies in slot {slot} of {kind} bucket {bucket}"
            ))
        })?;
    let bit = 1 << (index % 64);
    if words[index / 64] & bit != 0 {
        return Err(Problem::Malformed(format!(
            "two notes lie in slot {slot} of {kind} bucket {bucket}"
        )));
    }
    words[index / 64] |= bit;
    Ok(())
}

/// Fails `values` that hold one twice, each the `what` of a note of its own.
fn distinct<T: Ord + std::fmt::Display>(mut values: Vec<T>, what: &str) -> Result<(), Problem> {
    values.sort_unstable();
    match values.windows(2).find(|pair| pair[0] == pair[1]) {
        Some(pair) => Err(Problem::Malformed(format!(
            "two notes share the {what} {}",
            pair[0]
        ))),
        None => Ok(()),
    }
}
