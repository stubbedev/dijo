//! Compound documents written from a tree of storages, so a storage of one document can be
//! handed on as a document of its own. Each storage's entries are linked so that POI's
//! `PropertyTable` iterates them in the order given, the order POI iterated the storage they
//! were copied from, which the parser of the storage sees: the entries are chained through
//! their right siblings rather than kept in the sorted red-black tree the format suggests,
//! which POI does not require. The documents are written from where they lie, a piece at a
//! time, so that writing a storage holds no more than its directory.

use std::borrow::Cow;
use std::io::{self, Write};

use std::sync::Arc;

use super::{Content, EntryId, EntryKind, Ole2, Result, Unreadable};
use super::{BIG_BLOCK_MINIMUM_DOCUMENT_SIZE, END_OF_CHAIN, NO_INDEX, PROPERTY_SIZE, SIGNATURE};
use super::{SMALL_BLOCK_SIZE, UNUSED_BLOCK};
use crate::input::{Input, Spool};

const SECTOR_SIZE: usize = 512;
const ENTRIES_PER_SECTOR: usize = SECTOR_SIZE / 4;
const DIFAT_IN_HEADER: usize = 109;
const FAT_SECTOR: i32 = -3;
const DIFAT_SECTOR: i32 = -4;
/// The longest entry name a property holds, in UTF-16 units.
const MAX_NAME_LENGTH: usize = 31;
const ROOT_NAME: [u16; 10] = [82, 111, 111, 116, 32, 69, 110, 116, 114, 121];
static ZEROS: [u8; SECTOR_SIZE] = [0; SECTOR_SIZE];
static UNUSED: [u8; SECTOR_SIZE] = [0xFF; SECTOR_SIZE];

/// A storage: its class id and its entries in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Storage<'a> {
    pub class_id: [u8; 16],
    pub entries: Vec<(Vec<u16>, Node<'a>)>,
}

/// An entry of a storage, and what a storage hands on as a document: a document in place in
/// the compound document it was read from, a document of bytes of its own, or a storage, which
/// is handed on as a compound document of its own whose root it is.
#[derive(Debug, Clone)]
pub enum Node<'a> {
    Document(Content<'a>),
    Owned(Vec<u8>),
    Storage(Storage<'a>),
}

/// Documents are equal where their bytes are, wherever they lie.
impl PartialEq for Node<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Node::Storage(one), Node::Storage(other)) => one == other,
            (Node::Storage(_), _) | (_, Node::Storage(_)) => false,
            _ => self.document().map(|one| one.bytes()) == other.document().map(|o| o.bytes()),
        }
    }
}

impl Eq for Node<'_> {}

impl From<Vec<u8>> for Node<'_> {
    fn from(bytes: Vec<u8>) -> Self {
        Node::Owned(bytes)
    }
}

/// A document of bytes borrowed where they lie, or of bytes of its own.
impl<'a> From<Cow<'a, [u8]>> for Node<'a> {
    fn from(bytes: Cow<'a, [u8]>) -> Self {
        match bytes {
            Cow::Borrowed(bytes) => Node::Document(Content::from(bytes)),
            Cow::Owned(bytes) => Node::Owned(bytes),
        }
    }
}

impl<'a> Node<'a> {
    /// The content of a document, borrowed where it is owned.
    fn document(&self) -> Option<Content<'_>> {
        match self {
            Node::Document(content) => Some(content.clone()),
            Node::Owned(bytes) => Some(Content::from(bytes.as_slice())),
            Node::Storage(_) => None,
        }
    }

    /// The same, owning what it borrowed.
    pub fn into_owned(self) -> Node<'static> {
        match self {
            Node::Document(content) => Node::Owned(content.to_vec()),
            Node::Owned(bytes) => Node::Owned(bytes),
            Node::Storage(storage) => Node::Storage(storage.into_owned()),
        }
    }

    /// The bytes it hands on, as one run: a document in place where it lies in one run, else
    /// copied together, and a storage written as a compound document, each spooled to a file
    /// of its own past what is read whole.
    pub fn into_input(self) -> io::Result<Input<'a>> {
        match self {
            Node::Document(content) => {
                let mut pieces = content.pieces();
                if let (Some(only), None) = (pieces.next(), pieces.next()) {
                    return Ok(Input::Borrowed(only));
                }
                let mut spool = Spool::new(Some(content.len() as u64));
                for piece in content.pieces() {
                    spool.write_all(piece)?;
                }
                spool.finish()
            }
            Node::Owned(bytes) => Ok(Input::Read(Arc::new(bytes))),
            Node::Storage(storage) => {
                let layout = Layout::of(&storage).map_err(|Unreadable(reason)| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("a storage cannot be written as a compound document: {reason}"),
                    )
                })?;
                let mut spool = io::BufWriter::new(Spool::new(Some(layout.size())));
                layout.write_to(&mut spool)?;
                spool
                    .into_inner()
                    .map_err(io::IntoInnerError::into_error)?
                    .finish()
            }
        }
    }
}

impl Storage<'_> {
    /// The same, owning the documents it borrowed.
    pub fn into_owned(self) -> Storage<'static> {
        Storage {
            class_id: self.class_id,
            entries: self
                .entries
                .into_iter()
                .map(|(name, node)| (name, node.into_owned()))
                .collect(),
        }
    }
}

impl<'a> Ole2<'a> {
    /// A directory and everything in it, its documents in place, as POI reads them.
    pub fn storage(&self, directory: EntryId) -> Result<Storage<'a>> {
        let mut entries = Vec::new();
        for child in self.children(directory) {
            let node = match self.kind(child) {
                EntryKind::Document => Node::Document(self.content(child)?),
                EntryKind::Directory | EntryKind::Root => Node::Storage(self.storage(child)?),
            };
            entries.push((self.name_units(child).to_vec(), node));
        }
        Ok(Storage {
            class_id: self.class_id(directory),
            entries,
        })
    }
}

/// A directory entry to write.
struct Property<'s> {
    name: &'s [u16],
    kind: u8,
    class_id: [u8; 16],
    child: i32,
    next: i32,
    data: Option<Content<'s>>,
    start: i32,
}

impl Property<'_> {
    fn length(&self) -> usize {
        self.data.as_ref().map_or(0, Content::len)
    }
}

/// The properties of a storage's entries from `index` on, each storage's entries after it,
/// chained in order.
fn flatten<'s>(
    storage: &'s Storage<'_>,
    properties: &mut Vec<Property<'s>>,
    parent: usize,
) -> Result<()> {
    let first = properties.len();
    for (name, node) in &storage.entries {
        if name.is_empty() || name.len() > MAX_NAME_LENGTH {
            return Err(Unreadable("an entry's name does not fit a property"));
        }
        let (kind, class_id) = match node {
            Node::Document(_) | Node::Owned(_) => (2, [0; 16]),
            Node::Storage(storage) => (1, storage.class_id),
        };
        let data = node.document();
        if data
            .as_ref()
            .is_some_and(|data| i32::try_from(data.len()).is_err())
        {
            return Err(Unreadable("a document is too large for a property"));
        }
        properties.push(Property {
            name,
            kind,
            class_id,
            child: NO_INDEX,
            next: NO_INDEX,
            data,
            start: END_OF_CHAIN,
        });
    }
    let indexes: Vec<usize> = (first..properties.len()).collect();
    if let Some(&head) = indexes.first() {
        properties[parent].child = head as i32;
    }
    for pair in indexes.windows(2) {
        properties[pair[0]].next = pair[1] as i32;
    }
    for (&index, (_, node)) in indexes.iter().zip(&storage.entries) {
        if let Node::Storage(child) = node {
            flatten(child, properties, index)?;
        }
    }
    Ok(())
}

/// A run of sectors the allocation table chains, or marks.
#[derive(Debug, Clone, Copy)]
struct Allocation {
    start: usize,
    count: usize,
    mark: Option<i32>,
}

impl Allocation {
    /// The table's entry of `sector`, if the run holds it.
    fn entry(&self, sector: usize) -> Option<i32> {
        if sector < self.start || sector >= self.start + self.count {
            return None;
        }
        Some(match self.mark {
            Some(mark) => mark,
            None if sector + 1 < self.start + self.count => (sector + 1) as i32,
            None => END_OF_CHAIN,
        })
    }

    /// The first sector, as a chain starts there.
    fn first(&self) -> i32 {
        if self.count == 0 {
            END_OF_CHAIN
        } else {
            self.start as i32
        }
    }
}

/// Where everything of a storage written as a compound document of 512-byte sectors goes.
struct Layout<'s> {
    properties: Vec<Property<'s>>,
    /// The documents kept in the mini stream, by property, in order.
    small: Vec<usize>,
    /// The documents of sectors of their own, by property, in order.
    big: Vec<usize>,
    mini_blocks: usize,
    fat_sectors: usize,
    difat_sectors: usize,
    directory_sectors: usize,
    mini_fat_sectors: usize,
    allocations: Vec<Allocation>,
}

fn sectors_of(bytes: usize) -> usize {
    bytes.div_ceil(SECTOR_SIZE)
}

impl<'s> Layout<'s> {
    /// The layout of `root` as the root of a compound document.
    fn of(root: &'s Storage<'_>) -> Result<Self> {
        let mut properties = vec![Property {
            name: &ROOT_NAME,
            kind: 5,
            class_id: root.class_id,
            child: NO_INDEX,
            next: NO_INDEX,
            data: None,
            start: END_OF_CHAIN,
        }];
        flatten(root, &mut properties, 0)?;
        let big_minimum = BIG_BLOCK_MINIMUM_DOCUMENT_SIZE as usize;
        let documents = || {
            properties
                .iter()
                .enumerate()
                .filter(|(_, property)| property.data.is_some())
        };
        let small: Vec<usize> = documents()
            .filter(|(_, property)| (1..big_minimum).contains(&property.length()))
            .map(|(index, _)| index)
            .collect();
        let big: Vec<usize> = documents()
            .filter(|(_, property)| property.length() >= big_minimum)
            .map(|(index, _)| index)
            .collect();
        let mut mini_blocks = 0;
        for &index in &small {
            properties[index].start = mini_blocks as i32;
            mini_blocks += properties[index].length().div_ceil(SMALL_BLOCK_SIZE);
        }
        let directory_sectors = sectors_of(properties.len() * PROPERTY_SIZE);
        let mini_fat_sectors = sectors_of(mini_blocks * 4);
        let mini_stream_sectors = sectors_of(mini_blocks * SMALL_BLOCK_SIZE);
        let big_sectors: usize = big
            .iter()
            .map(|&index| sectors_of(properties[index].length()))
            .sum();
        let data_sectors = directory_sectors + mini_fat_sectors + mini_stream_sectors + big_sectors;
        let (mut fat_sectors, mut difat_sectors) = (0, 0);
        loop {
            let fat = (data_sectors + fat_sectors + difat_sectors).div_ceil(ENTRIES_PER_SECTOR);
            let difat = fat
                .saturating_sub(DIFAT_IN_HEADER)
                .div_ceil(ENTRIES_PER_SECTOR - 1);
            if (fat, difat) == (fat_sectors, difat_sectors) {
                break;
            }
            (fat_sectors, difat_sectors) = (fat, difat);
        }
        let mut allocations = Vec::with_capacity(5 + big.len());
        let mut next_sector = 0;
        let mut allocate = |count: usize, mark: Option<i32>| {
            let allocation = Allocation {
                start: next_sector,
                count,
                mark,
            };
            next_sector += count;
            allocations.push(allocation);
            allocation.first()
        };
        allocate(fat_sectors, Some(FAT_SECTOR));
        allocate(difat_sectors, Some(DIFAT_SECTOR));
        allocate(directory_sectors, None);
        allocate(mini_fat_sectors, None);
        properties[0].start = allocate(mini_stream_sectors, None);
        for &index in &big {
            properties[index].start = allocate(sectors_of(properties[index].length()), None);
        }
        Ok(Layout {
            properties,
            small,
            big,
            mini_blocks,
            fat_sectors,
            difat_sectors,
            directory_sectors,
            mini_fat_sectors,
            allocations,
        })
    }

    /// The size of the compound document.
    fn size(&self) -> u64 {
        let sectors: usize = self
            .allocations
            .iter()
            .map(|allocation| allocation.count)
            .sum();
        ((1 + sectors) * SECTOR_SIZE) as u64
    }

    fn allocation(&self, index: usize) -> Allocation {
        self.allocations[index]
    }

    /// Writes the compound document to `out`.
    fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        let fat_start = self.allocation(0).first();
        let difat_start = self.allocation(1).first();
        let mut header = [0u8; SECTOR_SIZE];
        header[..8].copy_from_slice(&SIGNATURE);
        header[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes());
        header[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
        header[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
        header[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
        header[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
        put(&mut header, 0x2C, self.fat_sectors as i32);
        put(&mut header, 0x30, self.allocation(2).first());
        put(&mut header, 0x38, BIG_BLOCK_MINIMUM_DOCUMENT_SIZE);
        put(&mut header, 0x3C, self.allocation(3).first());
        put(&mut header, 0x40, self.mini_fat_sectors as i32);
        put(&mut header, 0x44, difat_start);
        put(&mut header, 0x48, self.difat_sectors as i32);
        let fat_location = |index: usize| fat_start + index as i32;
        for slot in 0..DIFAT_IN_HEADER {
            let value = if slot < self.fat_sectors {
                fat_location(slot)
            } else {
                UNUSED_BLOCK
            };
            put(&mut header, 0x4C + 4 * slot, value);
        }
        out.write_all(&header)?;

        for sector in 0..self.fat_sectors * ENTRIES_PER_SECTOR {
            let entry = self
                .allocations
                .iter()
                .find_map(|allocation| allocation.entry(sector))
                .unwrap_or(UNUSED_BLOCK);
            out.write_all(&entry.to_le_bytes())?;
        }
        let beyond_header = DIFAT_IN_HEADER..self.fat_sectors.max(DIFAT_IN_HEADER);
        let per_sector = ENTRIES_PER_SECTOR - 1;
        for index in 0..self.difat_sectors {
            let first = beyond_header.start + index * per_sector;
            for slot in 0..per_sector {
                let location = first + slot;
                let value = if beyond_header.contains(&location) {
                    fat_location(location)
                } else {
                    UNUSED_BLOCK
                };
                out.write_all(&value.to_le_bytes())?;
            }
            let next = if index + 1 < self.difat_sectors {
                difat_start + index as i32 + 1
            } else {
                END_OF_CHAIN
            };
            out.write_all(&next.to_le_bytes())?;
        }

        let mini_stream_size = self.mini_blocks * SMALL_BLOCK_SIZE;
        for property in &self.properties {
            out.write_all(&property_bytes(property, mini_stream_size))?;
        }
        pad(
            out,
            &ZEROS,
            self.directory_sectors * SECTOR_SIZE - self.properties.len() * PROPERTY_SIZE,
        )?;

        for &index in &self.small {
            let first = self.properties[index].start as usize;
            let blocks = self.properties[index].length().div_ceil(SMALL_BLOCK_SIZE);
            for block in first..first + blocks {
                let next = if block + 1 < first + blocks {
                    (block + 1) as i32
                } else {
                    END_OF_CHAIN
                };
                out.write_all(&next.to_le_bytes())?;
            }
        }
        pad(
            out,
            &UNUSED,
            self.mini_fat_sectors * SECTOR_SIZE - self.mini_blocks * 4,
        )?;

        for &index in &self.small {
            let length = self.write_document(out, index)?;
            pad(
                out,
                &ZEROS,
                length.next_multiple_of(SMALL_BLOCK_SIZE) - length,
            )?;
        }
        pad(
            out,
            &ZEROS,
            sectors_of(mini_stream_size) * SECTOR_SIZE - mini_stream_size,
        )?;

        for &index in &self.big {
            let length = self.write_document(out, index)?;
            pad(out, &ZEROS, sectors_of(length) * SECTOR_SIZE - length)?;
        }
        Ok(())
    }

    /// Writes the document of a property, a piece at a time: its length.
    fn write_document(&self, out: &mut impl Write, index: usize) -> io::Result<usize> {
        let data = self.properties[index]
            .data
            .as_ref()
            .expect("a document's property holds its data");
        for piece in data.pieces() {
            out.write_all(piece)?;
        }
        Ok(data.len())
    }
}

/// Writes the storage as a compound document of 512-byte sectors whose root it is.
pub fn write(root: &Storage<'_>) -> Result<Vec<u8>> {
    let layout = Layout::of(root)?;
    let mut out = Vec::with_capacity(usize::try_from(layout.size()).unwrap_or(0));
    layout
        .write_to(&mut out)
        .expect("writing to memory does not fail");
    Ok(out)
}

fn put(bytes: &mut [u8], at: usize, value: i32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes `count` bytes of `filler`, up to a sector's worth.
fn pad(out: &mut impl Write, filler: &[u8; SECTOR_SIZE], count: usize) -> io::Result<()> {
    out.write_all(&filler[..count])
}

/// A property of 128 bytes; the root's size is the mini stream's.
fn property_bytes(property: &Property<'_>, mini_stream_size: usize) -> [u8; PROPERTY_SIZE] {
    let mut raw = [0u8; PROPERTY_SIZE];
    for (i, unit) in property.name.iter().enumerate() {
        raw[2 * i..2 * i + 2].copy_from_slice(&unit.to_le_bytes());
    }
    raw[0x40..0x42].copy_from_slice(&(((property.name.len() + 1) * 2) as u16).to_le_bytes());
    raw[0x42] = property.kind;
    raw[0x43] = 1;
    put(&mut raw, 0x44, NO_INDEX);
    put(&mut raw, 0x48, property.next);
    put(&mut raw, 0x4C, property.child);
    raw[0x50..0x60].copy_from_slice(&property.class_id);
    let size = match (property.kind, &property.data) {
        (5, _) => mini_stream_size,
        (_, Some(data)) => data.len(),
        _ => 0,
    };
    let start = if size == 0 {
        END_OF_CHAIN
    } else {
        property.start
    };
    put(&mut raw, 0x74, start);
    put(&mut raw, 0x78, size as i32);
    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    #[test]
    fn writes_a_storage_poi_reads_back_in_order() {
        let big = vec![7u8; 5000];
        let inner = Storage {
            class_id: [9; 16],
            entries: vec![(units("Inner"), Node::Owned(b"deep".to_vec()))],
        };
        let root = Storage {
            class_id: [3; 16],
            entries: vec![
                (units("Zeta"), Node::Owned(b"small".to_vec())),
                (units("Alpha"), Node::Owned(big.clone())),
                (units("Empty"), Node::Owned(Vec::new())),
                (units("Folder"), Node::Storage(inner)),
            ],
        };
        let data = write(&root).unwrap();
        assert_eq!(Layout::of(&root).unwrap().size(), data.len() as u64);

        let file = Ole2::open(&data).unwrap();
        assert_eq!(file.top_level_names(), ["Zeta", "Alpha", "Empty", "Folder"]);
        assert_eq!(file.storage(file.root()).unwrap(), root);
        let read = |name: &str| file.read(file.child(file.root(), name).unwrap()).unwrap();
        assert_eq!(read("Alpha"), big);
        assert_eq!(read("Zeta"), b"small");
    }

    #[test]
    fn writes_a_storage_read_in_place_as_it_was_written() {
        let root = Storage {
            class_id: [3; 16],
            entries: vec![
                (units("Small"), Node::Owned(vec![1; 100])),
                (units("Big"), Node::Owned(vec![2; 9000])),
            ],
        };
        let data = write(&root).unwrap();
        let file = Ole2::open(&data).unwrap();
        let copied = file.storage(file.root()).unwrap();
        let input = Node::Storage(copied).into_input().unwrap();
        assert_eq!(&*input, data.as_slice());
    }

    #[test]
    fn writes_more_allocation_tables_than_the_header_holds() {
        let large = vec![1u8; 7_500_000];
        let root = Storage {
            class_id: [0; 16],
            entries: vec![(units("Large"), Node::Owned(large.clone()))],
        };
        let data = write(&root).unwrap();
        let file = Ole2::open(&data).unwrap();
        assert_eq!(
            file.read(file.child(file.root(), "Large").unwrap())
                .unwrap(),
            large
        );
    }
}
