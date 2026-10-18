//! Unix dump archives as commons-compress's `DumpArchiveInputStream` reads them for Tika's
//! `PackageParser`: a tape of 1,024-byte records in blocks, the blocks after the first of a
//! compressed tape each zlib-compressed or not; the summary, the clear-inode and bitmap
//! segments skipped; then each inode's header and its data records, the holes of a sparse file
//! zeros, a directory's entries naming the inodes. An entry is named by the path of names up
//! to the root; one whose path is not known yet waits until a later directory names it, and is
//! then given, in the order of its path, with no data, as the stream's state has moved on.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::zip::decode_name;
use super::{throws, Decoding, Limited, Undecoded};
use crate::input::Input;

/// `TP_SIZE`: the bytes of a record.
const RECORD: usize = 1024;
const NFS_MAGIC: i32 = 60012;
const CHECKSUM: i32 = 84446;
/// The records a header's map describes at most.
const MAP: usize = 512;
const MAP_OFFSET: usize = 164;
const ROOT_INO: i32 = 2;
const CURRENT: &str = ".";
const PARENT: &str = "..";

/// `SEGMENT_TYPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Segment {
    Tape,
    Inode,
    Bits,
    Addr,
    End,
    Clri,
}

impl Segment {
    fn find(code: i32) -> Option<Self> {
        Some(match code {
            1 => Segment::Tape,
            2 => Segment::Inode,
            3 => Segment::Bits,
            4 => Segment::Addr,
            5 => Segment::End,
            6 => Segment::Clri,
            _ => return None,
        })
    }
}

/// The type of a directory, from the high four bits of an inode's mode.
const TYPE_DIRECTORY: u16 = 4;

/// Why the stream does not read an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// Opening it throws this `ArchiveException` message, which Tika's parser reports as unable
    /// to unpack the document.
    Unopened(String),
    /// Reading it on throws.
    Fails(Undecoded),
}

/// An entry of the archive as `getNextEntry` gives it and Tika's parser reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpEntry {
    pub name: String,
    pub directory: bool,
    /// `getSize`: -1 for a directory.
    pub size: i64,
    pub modified_millis: i64,
    /// The data the stream reads for it.
    pub content: Input<'static>,
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn le64(bytes: &[u8], at: usize) -> i64 {
    i64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

/// `DumpArchiveUtil.verify`: the magic, and the checksum over the record's 256 words.
fn verify(record: &[u8]) -> bool {
    if le32(record, 24) != NFS_MAGIC {
        return false;
    }
    let sum = (0..256).fold(0i32, |sum, word| sum.wrapping_add(le32(record, 4 * word)));
    le32(record, 28) == CHECKSUM.wrapping_sub(sum.wrapping_sub(le32(record, 28)))
}

/// `DumpArchiveEntry.parse` of a header record, as far as the stream and Tika read it.
#[derive(Debug, Clone)]
struct Header {
    segment: Option<Segment>,
    ino: i32,
    directory: bool,
    size: i64,
    modified_millis: i64,
    count: i32,
    holes: i32,
    map: [u8; MAP],
}

impl Header {
    fn parse(record: &[u8]) -> Self {
        let mode = le16(record, 32);
        let count = le32(record, 160);
        let mut map = [0u8; MAP];
        map.copy_from_slice(&record[MAP_OFFSET..MAP_OFFSET + MAP]);
        let holes = map
            .iter()
            .take(usize::try_from(count).unwrap_or(0).min(MAP))
            .filter(|&&flag| flag == 0)
            .count() as i32;
        Header {
            segment: Segment::find(le32(record, 0)),
            ino: le32(record, 20),
            directory: mode >> 12 & 0x0F == TYPE_DIRECTORY,
            size: le64(record, 40),
            modified_millis: 1000 * i64::from(le32(record, 56))
                + i64::from(le32(record, 60) / 1000),
            count,
            holes,
            map,
        }
    }

    /// `isSparseRecord`: whether the record of `index` is a hole, which the tape does not hold;
    /// the map describes 512 records, past which Java's array index fails.
    fn is_sparse(&self, index: usize) -> Result<bool, Undecoded> {
        let flag = self.map.get(index).ok_or_else(|| {
            throws("java.lang.ArrayIndexOutOfBoundsException: a header's record map holds 512")
        })?;
        Ok(flag & 0x01 == 0)
    }
}

/// `ShortFileException`, where the tape ends inside a block.
fn short_file() -> Undecoded {
    throws("ShortFileException: unexpected EOF")
}

/// Why a block is not read: the tape ends inside it, which reads take for its end, or reading
/// it fails.
enum Unblocked {
    Short,
    Fails(Undecoded),
}

impl From<Undecoded> for Unblocked {
    fn from(undecoded: Undecoded) -> Self {
        Unblocked::Fails(undecoded)
    }
}

impl Unblocked {
    /// The failure of a read that the tape ending inside a block fails too.
    fn failure(self) -> Undecoded {
        match self {
            Unblocked::Short => short_file(),
            Unblocked::Fails(undecoded) => undecoded,
        }
    }
}

fn eof() -> Undecoded {
    throws("java.io.EOFException")
}

fn invalid_format() -> Undecoded {
    throws("InvalidFormatException: there was an error decoding a tape segment")
}

/// `TapeInputStream` over the archive's bytes: records read a block at a time.
struct Tape<'a> {
    data: &'a [u8],
    at: usize,
    block: Vec<u8>,
    block_size: usize,
    offset: usize,
    compressed: bool,
    /// Whether a block has been read, after which a compressed tape's blocks have headers.
    started: bool,
}

impl<'a> Tape<'a> {
    fn new(data: &'a [u8]) -> Self {
        Tape {
            data,
            at: 0,
            block: vec![0; RECORD],
            block_size: RECORD,
            offset: RECORD,
            compressed: false,
            started: false,
        }
    }

    fn raw(&mut self, length: usize) -> Result<&'a [u8], Unblocked> {
        let bytes = self
            .data
            .get(self.at..self.at.checked_add(length).ok_or(Unblocked::Short)?)
            .ok_or(Unblocked::Short)?;
        self.at += length;
        Ok(bytes)
    }

    /// `readBlock`: the next block, inflated where `decompress` asks and it is compressed, else
    /// zeros in its place.
    fn read_block(&mut self, decompress: bool) -> Result<(), Unblocked> {
        if !self.compressed || !self.started {
            let bytes = self.raw(self.block_size)?;
            self.block.copy_from_slice(bytes);
        } else {
            let header = le32(self.raw(4)?, 0);
            if header & 0x01 == 0 {
                let bytes = self.raw(self.block_size)?;
                self.block.copy_from_slice(bytes);
            } else {
                let flags = header >> 1 & 0x07;
                let length = usize::try_from(header >> 4 & 0x0FFF_FFFF).expect("28 bits");
                let compressed = self.raw(length)?;
                if !decompress {
                    self.block.fill(0);
                } else {
                    match flags & 0x03 {
                        0 => self.inflate(compressed)?,
                        1 => {
                            return Err(
                                throws("UnsupportedCompressionAlgorithmException: BZLIB2").into()
                            )
                        }
                        2 => {
                            return Err(
                                throws("UnsupportedCompressionAlgorithmException: LZO").into()
                            )
                        }
                        _ => return Err(throws("UnsupportedCompressionAlgorithmException").into()),
                    }
                }
            }
        }
        self.started = true;
        self.offset = 0;
        Ok(())
    }

    /// The zlib stream `compressed`, which must inflate into a whole block.
    fn inflate(&mut self, compressed: &[u8]) -> Result<(), Unblocked> {
        let mut inflater = flate2::Decompress::new(true);
        inflater
            .decompress(compressed, &mut self.block, flate2::FlushDecompress::Finish)
            .map_err(|_| throws("DumpArchiveException: Bad data"))?;
        if inflater.total_out() != self.block_size as u64 {
            return Err(Unblocked::Short);
        }
        Ok(())
    }

    /// `resetBlockSize`: blocks of `records`, the rest of the first read whole.
    fn reset_block_size(&mut self, records: i32, compressed: bool) -> Result<(), Undecoded> {
        self.compressed = compressed;
        let records = usize::try_from(records)
            .ok()
            .filter(|&records| records >= 1)
            .ok_or_else(|| {
                throws(format!(
                    "Block with {records} records found, must be at least 1"
                ))
            })?;
        self.block_size = RECORD.checked_mul(records).ok_or_else(short_file)?;
        let first = self.block[..RECORD].to_vec();
        let rest = self
            .raw(self.block_size - RECORD)
            .map_err(Unblocked::failure)?;
        self.block = [first.as_slice(), rest].concat();
        self.started = true;
        self.offset = RECORD;
        Ok(())
    }

    /// `read` of `records` records, none where the tape ends inside a block.
    fn read(&mut self, records: usize) -> Result<Option<Vec<u8>>, Undecoded> {
        let length = records * RECORD;
        let mut read = Vec::with_capacity(length.min(self.data.len()));
        while read.len() < length {
            if self.offset == self.block_size {
                match self.read_block(true) {
                    Ok(()) => {}
                    Err(Unblocked::Short) => return Ok(None),
                    Err(Unblocked::Fails(undecoded)) => return Err(undecoded),
                }
            }
            let count = (length - read.len()).min(self.block_size - self.offset);
            read.extend_from_slice(&self.block[self.offset..self.offset + count]);
            self.offset += count;
        }
        Ok(Some(read))
    }

    /// `readRecord`: the next record, which the tape must hold.
    fn record(&mut self) -> Result<Vec<u8>, Undecoded> {
        self.read(1)?.ok_or_else(short_file)
    }

    /// `skip` of `records` records, a block skipped whole not inflated; `false` where the tape
    /// ends inside a block.
    fn skip(&mut self, records: usize) -> Result<bool, Undecoded> {
        let length = records * RECORD;
        let mut skipped = 0;
        while skipped < length {
            if self.offset == self.block_size {
                match self.read_block(length - skipped < self.block_size) {
                    Ok(()) => {}
                    Err(Unblocked::Short) => return Ok(false),
                    Err(Unblocked::Fails(undecoded)) => return Err(undecoded),
                }
            }
            let count = (length - skipped).min(self.block_size - self.offset);
            self.offset += count;
            skipped += count;
        }
        Ok(true)
    }

    /// `peek`: the next record, without reading past it; none where the tape ends inside a
    /// block.
    fn peek(&mut self) -> Result<Option<Vec<u8>>, Undecoded> {
        if self.offset == self.block_size {
            match self.read_block(true) {
                Ok(()) => {}
                Err(Unblocked::Short) => return Ok(None),
                Err(Unblocked::Fails(undecoded)) => return Err(undecoded),
            }
        }
        Ok(Some(self.block[self.offset..self.offset + RECORD].to_vec()))
    }
}

/// A directory's entry: the inode it names, the directory's inode and the name.
#[derive(Debug, Clone)]
struct Dirent {
    ino: i32,
    parent: i32,
    name: String,
}

/// An entry waiting for its path, or given one.
#[derive(Debug, Clone)]
struct Named {
    header: Header,
    path: String,
}

/// `DumpArchiveInputStream`.
struct Stream<'a> {
    tape: Tape<'a>,
    active: Header,
    read_index: usize,
    entry_size: i64,
    entry_offset: i64,
    names: HashMap<i32, Dirent>,
    pending: BTreeMap<i32, Header>,
    /// Entries named once their directory was read, given in the order of their paths.
    queue: Vec<Named>,
    eof: bool,
}

/// The entries of the dump archive `data`, as `getNextEntry` gives them, with the data the
/// stream reads for each but the directories, up to the decode limit of the archive, at which
/// the entries end.
pub fn entries(data: &[u8]) -> Result<Limited<Vec<DumpEntry>>, Unread> {
    let mut stream =
        Stream::open(data).map_err(|undecoded| Unread::Unopened(undecoded.to_string()))?;
    let mut entries = Vec::new();
    while let Some(named) = stream.next_entry().map_err(Unread::Fails)? {
        let directory = named.header.directory;
        let mut name = named.path;
        if directory && !name.ends_with('/') {
            name.push('/');
        }
        if let Some(rest) = name.strip_prefix("./") {
            name = rest.to_owned();
        }
        let mut decoding = Decoding::spooled(data.len(), u64::try_from(named.header.size).ok());
        if !directory {
            stream.read_data(&mut decoding).map_err(Unread::Fails)?;
        }
        let cut = decoding.is_cut();
        let entry = DumpEntry {
            name,
            directory,
            size: if directory { -1 } else { named.header.size },
            modified_millis: named.header.modified_millis,
            content: match decoding.finish().map_err(Unread::Fails)? {
                Limited::Whole(content) | Limited::Cut(content) => content,
            },
        };
        entries.push(entry);
        if cut {
            return Ok(Limited::Cut(entries));
        }
    }
    Ok(Limited::Whole(entries))
}

impl<'a> Stream<'a> {
    /// The constructor: the summary, then the clear-inode and bitmap segments skipped.
    fn open(data: &'a [u8]) -> Result<Self, Undecoded> {
        let mut tape = Tape::new(data);
        let summary = tape.record()?;
        if !verify(&summary) {
            return Err(throws(
                "UnrecognizedFormatException: this is not a recognized format.",
            ));
        }
        let records = le32(&summary, 896);
        let compressed = le32(&summary, 888) & 0x0080 != 0;
        tape.reset_block_size(records, compressed)?;
        let mut stream = Stream {
            tape,
            active: Header::parse(&summary),
            read_index: 0,
            entry_size: 0,
            entry_offset: 0,
            names: HashMap::from([(
                ROOT_INO,
                Dirent {
                    ino: ROOT_INO,
                    parent: ROOT_INO,
                    name: CURRENT.to_owned(),
                },
            )]),
            pending: BTreeMap::new(),
            queue: Vec::new(),
            eof: false,
        };
        stream.skip_segment(Segment::Clri)?;
        stream.skip_segment(Segment::Bits)?;
        Ok(stream)
    }

    /// `readCLRI` and `readBITS`: a segment of `segment`, its records skipped.
    fn skip_segment(&mut self, segment: Segment) -> Result<(), Undecoded> {
        let record = self.tape.record()?;
        if !verify(&record) {
            return Err(invalid_format());
        }
        self.active = Header::parse(&record);
        if self.active.segment != Some(segment) {
            return Err(invalid_format());
        }
        let count = usize::try_from(self.active.count).unwrap_or(0);
        if !self.tape.skip(count)? {
            return Err(eof());
        }
        self.read_index = count;
        Ok(())
    }

    /// The next header record, which must verify.
    fn header(&mut self) -> Result<Header, Undecoded> {
        let record = self.tape.record()?;
        if !verify(&record) {
            return Err(invalid_format());
        }
        Ok(Header::parse(&record))
    }

    /// `getNextEntry`, with the entry's path.
    fn next_entry(&mut self) -> Result<Option<Named>, Undecoded> {
        if let Some(index) =
            (0..self.queue.len()).min_by(|&a, &b| self.queue[a].path.cmp(&self.queue[b].path))
        {
            return Ok(Some(self.queue.remove(index)));
        }
        loop {
            if self.eof {
                return Ok(None);
            }
            while (self.read_index as i64) < i64::from(self.active.count) {
                let sparse = self.active.is_sparse(self.read_index)?;
                self.read_index += 1;
                if !sparse && !self.tape.skip(1)? {
                    return Err(eof());
                }
            }
            self.read_index = 0;
            self.active = self.header()?;
            while self.active.segment == Some(Segment::Addr) {
                let records = usize::try_from(self.active.count - self.active.holes).unwrap_or(0);
                if !self.tape.skip(records)? {
                    return Err(eof());
                }
                self.active = self.header()?;
            }
            if self.active.segment == Some(Segment::End) {
                self.eof = true;
                return Ok(None);
            }
            let header = self.active.clone();
            if header.directory {
                self.read_directory(header.clone())?;
                self.entry_offset = 0;
                self.entry_size = 0;
                self.read_index = usize::try_from(self.active.count).unwrap_or(0);
            } else {
                self.entry_offset = 0;
                self.entry_size = header.size;
                self.read_index = 0;
            }
            match self.path(&header)? {
                Some(path) => return Ok(Some(Named { header, path })),
                None => {
                    self.pending.insert(header.ino, header);
                }
            }
        }
    }

    /// `getPath`: the names from the root down to the entry, none where an inode on the way
    /// has no name yet.
    fn path(&self, header: &Header) -> Result<Option<String>, Undecoded> {
        let mut elements = Vec::new();
        let mut visited = HashSet::new();
        let mut ino = header.ino;
        loop {
            let Some(dirent) = self.names.get(&ino) else {
                return Ok(None);
            };
            if !visited.insert(ino) {
                return Err(throws(format!(
                    "DumpArchiveException: Duplicate node {ino}"
                )));
            }
            elements.push(dirent.name.as_str());
            if dirent.ino == dirent.parent {
                break;
            }
            ino = dirent.parent;
        }
        elements.reverse();
        Ok(Some(elements.join("/")))
    }

    /// `readDirectoryEntry`: the directory's records and those of the address segments after
    /// it read, each entry naming an inode, which may give a waiting entry its path.
    fn read_directory(&mut self, mut header: Header) -> Result<(), Undecoded> {
        let mut size = header.size;
        let mut first = true;
        while first || header.segment == Some(Segment::Addr) {
            if !first {
                self.tape.record()?;
            }
            if !self.names.contains_key(&header.ino) && header.segment == Some(Segment::Inode) {
                self.pending.insert(header.ino, header.clone());
            }
            let records = usize::try_from(header.count).map_err(|_| eof())?;
            let block = self.tape.read(records)?.ok_or_else(eof)?;
            let length = block.len() as i64;
            let mut at = 0i64;
            while at < length - 8 && at < size - 8 {
                let index = at as usize;
                let ino = le32(&block, index);
                let reclen = le16(&block, index + 4);
                if reclen == 0 {
                    return Err(throws("DumpArchiveException: reclen cannot be 0"));
                }
                let name_length = block[index + 7] as i8;
                let name_length = usize::try_from(name_length)
                    .map_err(|_| throws("Invalid offset/length combination"))?;
                let mut name =
                    block[index + 8..(index + 8 + name_length).min(block.len())].to_vec();
                name.resize(name_length, 0);
                let name = decode_name(&name);
                at += i64::from(reclen);
                if name == CURRENT || name == PARENT {
                    continue;
                }
                self.names.insert(
                    ino,
                    Dirent {
                        ino,
                        parent: header.ino,
                        name,
                    },
                );
                let mut named = Vec::new();
                for (&waiting, pending) in &self.pending {
                    if let Some(path) = self.path(pending)? {
                        named.push((waiting, path));
                    }
                }
                for (waiting, path) in named {
                    let header = self.pending.remove(&waiting).expect("the entry waits");
                    self.queue.push(Named { header, path });
                }
            }
            let peeked = self
                .tape
                .peek()?
                .filter(|record| verify(record))
                .ok_or_else(invalid_format)?;
            header = Header::parse(&peeked);
            first = false;
            size -= RECORD as i64;
        }
        Ok(())
    }

    /// `read` of the current entry's data to its end into `decoding`: its records, a hole's
    /// zeros, and the records of the address segments that go on with it.
    fn read_data(&mut self, decoding: &mut Decoding<crate::input::Spool>) -> Result<(), Undecoded> {
        let zeros = [0u8; RECORD];
        while self.entry_offset < self.entry_size && !decoding.is_cut() {
            if self.read_index >= MAP {
                self.active = self.header()?;
                self.read_index = 0;
            }
            let sparse = self.active.is_sparse(self.read_index)?;
            self.read_index += 1;
            let record = if sparse {
                zeros.to_vec()
            } else {
                self.tape.read(1)?.ok_or_else(eof)?
            };
            let length = usize::try_from(self.entry_size - self.entry_offset)
                .map_or(RECORD, |left| left.min(RECORD));
            decoding.write(&record[..length])?;
            self.entry_offset += length as i64;
        }
        Ok(())
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    #[cfg(test)]
    use super::*;

    /// A record of the tape: a header of `segment`, `ino`, `mode`, `size` and modification time
    /// `mtime`, whose map has `count` records all on the tape, its checksum made.
    pub fn header(segment: i32, ino: i32, mode: u16, size: i64, count: i32) -> Vec<u8> {
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(&segment.to_le_bytes());
        record[20..24].copy_from_slice(&ino.to_le_bytes());
        record[24..28].copy_from_slice(&60012i32.to_le_bytes());
        record[32..34].copy_from_slice(&mode.to_le_bytes());
        record[40..48].copy_from_slice(&size.to_le_bytes());
        record[56..60].copy_from_slice(&1_700_000_000i32.to_le_bytes());
        record[60..64].copy_from_slice(&250_000i32.to_le_bytes());
        record[160..164].copy_from_slice(&count.to_le_bytes());
        for flag in &mut record[164..164 + usize::try_from(count).unwrap_or(0).min(512)] {
            *flag = 1;
        }
        sign(&mut record);
        record
    }

    /// `record` with the checksum `verify` checks.
    fn sign(record: &mut [u8]) {
        record[28..32].fill(0);
        let sum = (0..256).fold(0i32, |sum, word| {
            sum.wrapping_add(i32::from_le_bytes(
                record[4 * word..4 * word + 4].try_into().unwrap(),
            ))
        });
        record[28..32].copy_from_slice(&84446i32.wrapping_sub(sum).to_le_bytes());
    }

    /// The summary record of a tape of blocks of one record, uncompressed.
    fn summary() -> Vec<u8> {
        let mut record = header(1, 0, 0, 0, 0);
        record[896..900].copy_from_slice(&1i32.to_le_bytes());
        sign(&mut record);
        record
    }

    /// A directory's data record of `entries`, each an inode and a name, the last running to
    /// the record's end.
    pub fn directory(entries: &[(i32, &str)]) -> Vec<u8> {
        let mut record = Vec::new();
        for (index, (ino, name)) in entries.iter().enumerate() {
            let start = record.len();
            let length = (8 + name.len()).next_multiple_of(4);
            let reclen = if index + 1 == entries.len() {
                1024 - start
            } else {
                length
            };
            record.extend_from_slice(&ino.to_le_bytes());
            record.extend_from_slice(&(reclen as u16).to_le_bytes());
            record.push(4);
            record.push(name.len() as u8);
            record.extend_from_slice(name.as_bytes());
            record.resize(start + length, 0);
        }
        record.resize(1024, 0);
        record
    }

    /// A data record of `data`.
    pub fn data(data: &[u8]) -> Vec<u8> {
        let mut record = data.to_vec();
        record.resize(1024, 0);
        record
    }

    /// A dump tape of `records` after the summary and empty clear-inode and bitmap segments,
    /// ended by an end segment.
    pub fn dump(records: &[Vec<u8>]) -> Vec<u8> {
        let mut tape = summary();
        tape.extend(header(6, 0, 0, 0, 0));
        tape.extend(header(3, 0, 0, 0, 0));
        for record in records {
            tape.extend_from_slice(record);
        }
        tape.extend(header(5, 0, 0, 0, 0));
        tape
    }

    /// The mode of a directory and of a file.
    pub const DIRECTORY: u16 = 0o040_755;
    pub const FILE: u16 = 0o100_644;

    #[cfg(test)]
    fn read(tape: &[u8]) -> Vec<(String, bool, Vec<u8>)> {
        entries(tape)
            .unwrap()
            .whole()
            .unwrap()
            .into_iter()
            .map(|entry| (entry.name, entry.directory, entry.content.to_vec()))
            .collect()
    }

    #[test]
    fn names_inodes_by_the_directories_that_list_them() {
        let tape = dump(&[
            header(2, 2, DIRECTORY, 1024, 1),
            directory(&[(2, "."), (2, ".."), (10, "a.txt"), (20, "sub")]),
            header(2, 10, FILE, 11, 1),
            data(b"hello dump\n"),
        ]);
        assert_eq!(
            read(&tape),
            [
                (String::new(), true, Vec::new()),
                ("a.txt".to_owned(), false, b"hello dump\n".to_vec()),
            ]
        );
        let entries = entries(&tape).unwrap().whole().unwrap();
        assert_eq!(entries[1].size, 11);
        assert_eq!(entries[1].modified_millis, 1_700_000_000_250);
    }

    /// An inode met before the directory that names it waits, losing its data, and comes once
    /// the directory has been read, in the order of its path.
    #[test]
    fn gives_an_inode_named_late_once_its_directory_is_read_without_its_data() {
        let tape = dump(&[
            header(2, 2, DIRECTORY, 1024, 1),
            directory(&[(2, "."), (2, ".."), (20, "sub")]),
            header(2, 30, FILE, 4, 1),
            data(b"late"),
            header(2, 20, DIRECTORY, 1024, 1),
            directory(&[(20, "."), (2, ".."), (30, "b.txt")]),
        ]);
        assert_eq!(
            read(&tape),
            [
                (String::new(), true, Vec::new()),
                ("sub/".to_owned(), true, Vec::new()),
                ("sub/b.txt".to_owned(), false, Vec::new()),
            ]
        );
    }

    #[test]
    fn reads_holes_as_zeros_and_fails_on_a_tape_cut_short() {
        let mut sparse = header(2, 10, FILE, 2048 + 3, 3);
        sparse[164 + 1] = 0;
        sign(&mut sparse);
        let tape = dump(&[
            header(2, 2, DIRECTORY, 1024, 1),
            directory(&[(2, "."), (2, ".."), (10, "holes")]),
            sparse,
            data(b"first"),
            data(b"end"),
        ]);
        let read = read(&tape);
        let mut expected = data(b"first");
        expected.extend_from_slice(&[0; 1024]);
        expected.extend_from_slice(b"end");
        assert_eq!(read[1], ("holes".to_owned(), false, expected));
        assert!(matches!(
            entries(&tape[..tape.len() - 1500]),
            Err(Unread::Fails(_))
        ));
        assert!(matches!(entries(&tape[..100]), Err(Unread::Unopened(_))));
    }
}
