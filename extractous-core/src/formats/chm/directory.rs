//! The structures of a CHM file as Tika's `ChmExtractor` reads them: the ITSF and ITSP headers,
//! the directory listing chunks (`ChmDirectoryListingSet`), the LZX control data
//! (`ChmLzxcControlData`) and the reset table (`ChmLzxcResetTable`).

use super::Thrown;

/// `CHM_ITSF_V3_LEN`, one byte less of which `ChmExtractor` hands the ITSF header.
const ITSF_V3_LENGTH: usize = 0x60;
const ITSF_V2_LENGTH: usize = 0x58;
const ITSP_LENGTH: i32 = 0x54;
const PMGL_LENGTH: usize = 0x14;
const LZXC_MINIMUM_LENGTH: usize = 0x18;
const LZXC_V2_LENGTH: usize = 0x1c;
/// `CHM_WINDOW_SIZE_BLOCK`, the unit of a version 2 control data's window size.
const WINDOW_SIZE_BLOCK: u64 = 0x8000;
/// The most reset table entries `ChmLzxcResetTable` reads.
const MAX_BLOCKS: u64 = 5000;
const RESET_TABLE_HEADER: usize = 40;

/// `ChmCommons.copyOfRange`: the bytes from `from` to `to`, which must lie in `data`.
pub(super) fn copy_of_range(data: &[u8], from: i32, to: i32) -> Result<&[u8], Thrown> {
    if from < 0 || to < 0 {
        return Err(Thrown::Parsing(format!("{from}..{to} should be > 0")));
    }
    if to as usize > data.len() {
        return Err(Thrown::Parsing("can't copy beyond array length".to_owned()));
    }
    if to < from {
        return Err(Thrown::Parsing(format!("{from} > {to}")));
    }
    Ok(&data[from as usize..to as usize])
}

/// Little-endian reads that fail as the headers' `unmarshal` methods do where a structure ends.
struct Fields<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn new(data: &'a [u8], at: usize) -> Self {
        Self { data, at }
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], Thrown> {
        let bytes = self
            .data
            .get(self.at..self.at + N)
            .ok_or_else(|| Thrown::Tika(format!("{N} > the bytes remaining")))?;
        self.at += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    fn int(&mut self) -> Result<i32, Thrown> {
        self.bytes().map(i32::from_le_bytes)
    }

    fn uint(&mut self) -> Result<u32, Thrown> {
        self.bytes().map(u32::from_le_bytes)
    }

    /// `unmarshalUint64`, which reads its bytes as a signed `long`.
    fn long(&mut self) -> Result<i64, Thrown> {
        self.bytes().map(i64::from_le_bytes)
    }
}

/// `ChmItsfHeader`: where the directory lies, and the content after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ItsfHeader {
    pub(super) directory_offset: i64,
    /// `data_offset`, which Tika computes as the end of the directory rather than read it.
    pub(super) data_offset: i64,
}

impl ItsfHeader {
    pub(super) fn read(data: &[u8]) -> Result<Self, Thrown> {
        let header = copy_of_range(data, 0, ITSF_V3_LENGTH as i32 - 1)?;
        debug_assert!(header.len() >= ITSF_V2_LENGTH);
        let mut fields = Fields::new(header, 0);
        let signature: [u8; 4] = fields.bytes()?;
        let version = fields.int()?;
        let header_length = fields.int()?;
        fields.int()?;
        fields.uint()?;
        fields.uint()?;
        fields.bytes::<32>()?;
        fields.long()?;
        fields.long()?;
        let directory_offset = fields.long()?;
        let directory_length = fields.long()?;
        if &signature != b"ITSF" {
            return Err(Thrown::Tika("seems not valid file".to_owned()));
        }
        match version {
            2 if header_length < ITSF_V2_LENGTH as i32 => {
                return Err(Thrown::Tika("something wrong with header".to_owned()))
            }
            3 if header_length < ITSF_V3_LENGTH as i32 => {
                return Err(Thrown::Tika("unknown v3 header lenght".to_owned()))
            }
            2 | 3 => {}
            _ => return Err(Thrown::Parsing("unsupported chm format".to_owned())),
        }
        Ok(Self {
            directory_offset,
            data_offset: directory_offset.wrapping_add(directory_length),
        })
    }
}

/// `ChmItspHeader`: the directory chunks' size and the first listing chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ItspHeader {
    block_length: u32,
    index_head: i32,
}

impl ItspHeader {
    pub(super) fn read(data: &[u8], itsf: &ItsfHeader) -> Result<Self, Thrown> {
        let from = itsf.directory_offset as i32;
        let header = copy_of_range(data, from, from.wrapping_add(ITSP_LENGTH))?;
        let mut fields = Fields::new(header, 0);
        let signature: [u8; 4] = fields.bytes()?;
        let version = fields.int()?;
        let header_length = fields.int()?;
        fields.int()?;
        let block_length = fields.uint()?;
        fields.int()?;
        fields.int()?;
        fields.int()?;
        let index_head = fields.int()?;
        if &signature != b"ITSP" {
            return Err(Thrown::Parsing("seems not valid signature".to_owned()));
        }
        if version != 1 {
            return Err(Thrown::Parsing("!=ChmConstants.CHM_VER_1".to_owned()));
        }
        if header_length != ITSP_LENGTH {
            return Err(Thrown::Parsing(
                "!= ChmConstants.CHM_ITSP_V1_LEN".to_owned(),
            ));
        }
        Ok(Self {
            block_length,
            index_head,
        })
    }
}

/// Whether an entry's data is stored as it is or in the LZX-compressed content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EntryType {
    Uncompressed,
    Compressed,
}

/// `DirectoryListingEntry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) name: String,
    pub(super) kind: EntryType,
    pub(super) offset: i32,
    pub(super) length: i32,
}

impl Entry {
    /// `ChmCommons.hasSkip`: a system entry, whose data Tika never extracts.
    pub(super) fn is_skipped(&self) -> bool {
        self.name.starts_with("/$") || self.name.starts_with("/#") || self.name.starts_with("::")
    }
}

/// `ChmDirectoryListingSet`: the entries of the listing chunks, in their order, and where the
/// control data and the reset table are among them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Listing {
    pub(super) entries: Vec<Entry>,
    pub(super) control_data: Option<usize>,
    pub(super) reset_table: Option<usize>,
}

impl Listing {
    /// `enumerateChmDirectoryListingList`: the listing chunks from the first on, each naming
    /// the next, until one names none; a `ChmParsingException` ends the listing with the entries
    /// read so far, any other failure fails it.
    pub(super) fn read(data: &[u8], itsf: &ItsfHeader, itsp: &ItspHeader) -> Result<Self, Thrown> {
        let mut listing = Self {
            entries: Vec::new(),
            control_data: None,
            reset_table: None,
        };
        match listing.chunks(data, itsf, itsp) {
            Ok(()) | Err(Thrown::Parsing(_)) => Ok(listing),
            Err(thrown) => Err(thrown),
        }
    }

    fn chunks(&mut self, data: &[u8], itsf: &ItsfHeader, itsp: &ItspHeader) -> Result<(), Thrown> {
        let block_length = itsp.block_length as i32;
        let directory = (itsf.directory_offset as i32).wrapping_add(ITSP_LENGTH);
        let mut processed = std::collections::HashSet::new();
        let mut chunk_number = itsp.index_head;
        while chunk_number >= 0 {
            let start = chunk_number
                .wrapping_mul(block_length)
                .wrapping_add(directory);
            let chunk = copy_of_range(data, start, start.wrapping_add(block_length))?;
            if chunk.len() < PMGL_LENGTH {
                return Err(Thrown::Tika(
                    "ChmPmglHeader we only know how to deal with a 0x14 byte structures".to_owned(),
                ));
            }
            let mut fields = Fields::new(chunk, 4);
            let free_space = fields.uint()?;
            fields.uint()?;
            fields.int()?;
            let next = fields.int()?;
            if !chunk.starts_with(b"PMGL") {
                return Err(Thrown::Parsing(
                    "ChmPmglHeader pmgl != pmgl.signature".to_owned(),
                ));
            }
            self.segment(chunk, free_space)?;
            processed.insert(chunk_number);
            if processed.contains(&next) {
                return Err(Thrown::Parsing(
                    "already processed block; avoiding cycle".to_owned(),
                ));
            }
            chunk_number = next;
        }
        Ok(())
    }

    /// `enumerateOneSegment`: the entries of one listing chunk, up to its free space.
    fn segment(&mut self, chunk: &[u8], free_space: u32) -> Result<(), Thrown> {
        let end = chunk.len() as i64 - i64::from(free_space);
        let mut at = PMGL_LENGTH;
        while at > 0 && (at as i64) < end {
            let length = encint(chunk, &mut at);
            if length < 0 || length as usize > chunk.len() {
                return Err(Thrown::Parsing("Bad data of a string length.".to_owned()));
            }
            let name = copy_of_range(chunk, at as i32, (at as i32).wrapping_add(length))?;
            let name = crate::java::utf8(name);
            if self.control_data.is_none() && name.contains("ControlData") {
                self.control_data = Some(self.entries.len());
            }
            if self.reset_table.is_none() && name.contains("ResetTable") {
                self.reset_table = Some(self.entries.len());
            }
            at += length as usize;
            let kind = if chunk.get(at) == Some(&0) {
                EntryType::Uncompressed
            } else {
                EntryType::Compressed
            };
            at += 1;
            let offset = encint(chunk, &mut at);
            let length = encint(chunk, &mut at);
            self.entries.push(Entry {
                name,
                kind,
                offset,
                length,
            });
        }
        Ok(())
    }

    /// `ChmCommons.indexOfDataSpaceStorageElement`: the first storage entry named with
    /// `pattern`.
    pub(super) fn storage(&self, pattern: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| {
            entry.name.starts_with("::DataSpace/Storage") && entry.name.contains(pattern)
        })
    }
}

/// `getEncint`: a big-endian base-128 number whose bytes but the last have their high bit set,
/// as a Java `int`; `at` moves past it.
fn encint(chunk: &[u8], at: &mut usize) -> i32 {
    let mut value: i64 = 0;
    while *at < chunk.len() && chunk[*at] >= 0x80 {
        value = (value << 7) | i64::from(chunk[*at] & 0x7f);
        *at += 1;
    }
    if *at < chunk.len() {
        value = (value << 7) | i64::from(chunk[*at] & 0x7f);
        *at += 1;
    }
    value as i32
}

/// `ChmLzxcControlData`: how often the LZX decoder resets, and its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ControlData {
    pub(super) reset_interval: u32,
    pub(super) window_size: u64,
}

impl ControlData {
    pub(super) fn read(data: Option<&[u8]>) -> Result<Self, Thrown> {
        let data = data
            .filter(|data| data.len() >= LZXC_MINIMUM_LENGTH)
            .ok_or_else(|| Thrown::Parsing("we want at least 0x18 bytes".to_owned()))?;
        let mut fields = Fields::new(data, 0);
        fields.uint()?;
        let signature: [u8; 4] = fields.bytes()?;
        let version = fields.uint()?;
        let reset_interval = fields.uint()?;
        let mut window_size = u64::from(fields.uint()?);
        fields.uint()?;
        if data.len() >= LZXC_V2_LENGTH {
            fields.uint()?;
        }
        if version == 2 {
            window_size *= WINDOW_SIZE_BLOCK;
        }
        if window_size == 0 || reset_interval == 0 {
            return Err(Thrown::Parsing(
                "window size / resetInterval should be more than zero".to_owned(),
            ));
        }
        if window_size == 1 {
            return Err(Thrown::Parsing(
                "window size / resetInterval should be more than 1".to_owned(),
            ));
        }
        if &signature != b"LZXC" {
            return Err(Thrown::Parsing(
                "the signature does not seem to be correct".to_owned(),
            ));
        }
        Ok(Self {
            reset_interval,
            window_size,
        })
    }
}

/// `ChmLzxcResetTable`: the uncompressed length, the block length, and where each block's
/// compressed data starts within the content, as far as Tika reads the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResetTable {
    pub(super) uncompressed_length: i64,
    pub(super) block_length: i64,
    pub(super) block_count: u64,
    pub(super) addresses: Vec<i64>,
}

impl ResetTable {
    pub(super) fn read(data: &[u8]) -> Result<Self, Thrown> {
        let short = || Thrown::Runtime("ArrayIndexOutOfBoundsException".to_owned());
        let mut fields = Fields::new(data, 0);
        let version = fields.uint().map_err(|_| short())?;
        let block_count = u64::from(fields.uint().map_err(|_| short())?).min(MAX_BLOCKS);
        fields.uint().map_err(|_| short())?;
        fields.uint().map_err(|_| short())?;
        let too_small = |_| Thrown::Tika("data is too small to calculate address block".to_owned());
        let uncompressed_length = fields.long().map_err(too_small)?;
        fields.long().map_err(too_small)?;
        let block_length = fields.long().map_err(too_small)?;
        let remaining = (data.len() as i64 - RESET_TABLE_HEADER as i64) / 8;
        let read = (block_count as i64).min(remaining).max(0) as usize;
        let mut addresses = vec![0; block_count as usize];
        for address in addresses.iter_mut().take(read) {
            *address = fields.long()?;
        }
        if version != 2 {
            return Err(Thrown::Parsing(
                "does not seem currect version of chmLzxcResetTable".to_owned(),
            ));
        }
        Ok(Self {
            uncompressed_length,
            block_length,
            block_count,
            addresses,
        })
    }
}

/// `ChmCommons.indexOfResetTableBlock`: four bytes before the first `LZXC` anywhere in the file.
pub(super) fn reset_data_index(data: &[u8]) -> i64 {
    data.windows(4)
        .position(|window| window == b"LZXC")
        .map_or(-1, |at| at as i64)
        - 4
}
