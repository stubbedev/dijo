//! Compound documents written from a tree of storages, so a storage of one document can be
//! handed on as a document of its own. Each storage's entries are linked so that POI's
//! `PropertyTable` iterates them in the order given, the order POI iterated the storage they
//! were copied from, which the parser of the storage sees: the entries are chained through
//! their right siblings rather than kept in the sorted red-black tree the format suggests,
//! which POI does not require.

use super::{EntryId, EntryKind, Ole2, Result, Unreadable, BIG_BLOCK_MINIMUM_DOCUMENT_SIZE};
use super::{END_OF_CHAIN, NO_INDEX, PROPERTY_SIZE, SIGNATURE, SMALL_BLOCK_SIZE, UNUSED_BLOCK};

const SECTOR_SIZE: usize = 512;
const ENTRIES_PER_SECTOR: usize = SECTOR_SIZE / 4;
const DIFAT_IN_HEADER: usize = 109;
const FAT_SECTOR: i32 = -3;
const DIFAT_SECTOR: i32 = -4;
/// The longest entry name a property holds, in UTF-16 units.
const MAX_NAME_LENGTH: usize = 31;

/// A storage: its class id and its entries in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Storage {
    pub class_id: [u8; 16],
    pub entries: Vec<(Vec<u16>, Node)>,
}

/// An entry of a storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Document(Vec<u8>),
    Storage(Storage),
}

impl Ole2<'_> {
    /// A copy of a directory and everything in it, its documents read as POI reads them.
    pub fn storage(&self, directory: EntryId) -> Result<Storage> {
        let mut entries = Vec::new();
        for child in self.children(directory) {
            let node = match self.kind(child) {
                EntryKind::Document => Node::Document(self.read(child)?),
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
struct Property<'a> {
    pub name: &'a [u16],
    kind: u8,
    class_id: [u8; 16],
    child: i32,
    next: i32,
    data: Option<&'a [u8]>,
    start: i32,
}

/// The properties of a storage's entries from `index` on, each storage's entries after it,
/// chained in order.
fn flatten<'a>(
    storage: &'a Storage,
    properties: &mut Vec<Property<'a>>,
    parent: usize,
) -> Result<()> {
    let first = properties.len();
    for (name, node) in &storage.entries {
        if name.is_empty() || name.len() > MAX_NAME_LENGTH {
            return Err(Unreadable("an entry's name does not fit a property"));
        }
        let (kind, class_id, data) = match node {
            Node::Document(data) => (2, [0; 16], Some(data.as_slice())),
            Node::Storage(storage) => (1, storage.class_id, None),
        };
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

/// Writes the storage as a compound document of 512-byte sectors whose root it is.
pub fn write(root: &Storage) -> Result<Vec<u8>> {
    const ROOT_NAME: [u16; 10] = [82, 111, 111, 116, 32, 69, 110, 116, 114, 121];
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

    let mut mini_stream: Vec<u8> = Vec::new();
    let mut mini_fat: Vec<i32> = Vec::new();
    for property in &mut properties {
        let Some(data) = property.data else {
            continue;
        };
        let size = i32::try_from(data.len())
            .map_err(|_| Unreadable("a document is too large for a property"))?;
        if size == 0 || size >= BIG_BLOCK_MINIMUM_DOCUMENT_SIZE {
            continue;
        }
        let blocks = data.len().div_ceil(SMALL_BLOCK_SIZE);
        let first = mini_fat.len();
        property.start = first as i32;
        mini_fat.extend((first + 1..first + blocks).map(|next| next as i32));
        mini_fat.push(END_OF_CHAIN);
        mini_stream.extend_from_slice(data);
        mini_stream.resize((first + blocks) * SMALL_BLOCK_SIZE, 0);
    }

    let sectors_of = |bytes: usize| bytes.div_ceil(SECTOR_SIZE);
    let directory_sectors = sectors_of(properties.len() * PROPERTY_SIZE);
    let mini_fat_sectors = sectors_of(mini_fat.len() * 4);
    let mini_stream_sectors = sectors_of(mini_stream.len());
    let big: Vec<usize> = properties
        .iter()
        .enumerate()
        .filter(|(_, property)| {
            property
                .data
                .is_some_and(|data| data.len() >= BIG_BLOCK_MINIMUM_DOCUMENT_SIZE as usize)
        })
        .map(|(index, _)| index)
        .collect();
    let big_sectors: usize = big
        .iter()
        .map(|&index| sectors_of(properties[index].data.map_or(0, <[u8]>::len)))
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

    let mut fat = vec![UNUSED_BLOCK; fat_sectors * ENTRIES_PER_SECTOR];
    let mut next_sector = 0usize;
    let mut allocate = |count: usize, fat: &mut Vec<i32>, mark: Option<i32>| -> i32 {
        let first = next_sector;
        for (sector, entry) in fat.iter_mut().enumerate().skip(first).take(count) {
            *entry = match mark {
                Some(mark) => mark,
                None if sector + 1 < first + count => (sector + 1) as i32,
                None => END_OF_CHAIN,
            };
        }
        next_sector += count;
        if count == 0 {
            END_OF_CHAIN
        } else {
            first as i32
        }
    };
    let fat_start = allocate(fat_sectors, &mut fat, Some(FAT_SECTOR));
    let difat_start = allocate(difat_sectors, &mut fat, Some(DIFAT_SECTOR));
    let directory_start = allocate(directory_sectors, &mut fat, None);
    let mini_fat_start = allocate(mini_fat_sectors, &mut fat, None);
    properties[0].start = allocate(mini_stream_sectors, &mut fat, None);
    for &index in &big {
        let length = properties[index].data.map_or(0, <[u8]>::len);
        properties[index].start = allocate(sectors_of(length), &mut fat, None);
    }

    let mut header = vec![0u8; SECTOR_SIZE];
    header[..8].copy_from_slice(&SIGNATURE);
    header[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes());
    header[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
    header[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
    header[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
    header[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
    put(&mut header, 0x2C, fat_sectors as i32);
    put(&mut header, 0x30, directory_start);
    put(&mut header, 0x38, BIG_BLOCK_MINIMUM_DOCUMENT_SIZE);
    put(&mut header, 0x3C, mini_fat_start);
    put(&mut header, 0x40, mini_fat_sectors as i32);
    put(&mut header, 0x44, difat_start);
    put(&mut header, 0x48, difat_sectors as i32);
    let fat_locations: Vec<i32> = (0..fat_sectors).map(|i| fat_start + i as i32).collect();
    for slot in 0..DIFAT_IN_HEADER {
        let value = fat_locations.get(slot).copied().unwrap_or(UNUSED_BLOCK);
        put(&mut header, 0x4C + 4 * slot, value);
    }

    let mut out = header;
    out.reserve((fat_sectors + difat_sectors + data_sectors) * SECTOR_SIZE);
    out.extend(fat.iter().flat_map(|entry| entry.to_le_bytes()));
    let rest = fat_locations.get(DIFAT_IN_HEADER..).unwrap_or(&[]);
    for (index, chunk) in rest.chunks(ENTRIES_PER_SECTOR - 1).enumerate() {
        let mut sector = vec![UNUSED_BLOCK; ENTRIES_PER_SECTOR];
        sector[..chunk.len()].copy_from_slice(chunk);
        sector[ENTRIES_PER_SECTOR - 1] = if index + 1 < difat_sectors {
            difat_start + index as i32 + 1
        } else {
            END_OF_CHAIN
        };
        out.extend(sector.iter().flat_map(|entry| entry.to_le_bytes()));
    }
    let mut directory = Vec::with_capacity(directory_sectors * SECTOR_SIZE);
    for property in &properties {
        directory.extend(property_bytes(property, mini_stream.len()));
    }
    pad(&mut directory);
    out.extend(directory);
    let mut mini_fat_bytes: Vec<u8> = mini_fat.iter().flat_map(|e| e.to_le_bytes()).collect();
    mini_fat_bytes.resize(mini_fat_sectors * SECTOR_SIZE, 0xFF);
    out.extend(mini_fat_bytes);
    let mut mini = mini_stream;
    pad(&mut mini);
    out.extend(mini);
    for &index in &big {
        let mut data = properties[index].data.unwrap_or(&[]).to_vec();
        pad(&mut data);
        out.extend(data);
    }
    Ok(out)
}

fn put(bytes: &mut [u8], at: usize, value: i32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Zero bytes to the end of the last sector.
fn pad(bytes: &mut Vec<u8>) {
    bytes.resize(bytes.len().div_ceil(SECTOR_SIZE) * SECTOR_SIZE, 0);
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
    let size = match (property.kind, property.data) {
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
            entries: vec![(units("Inner"), Node::Document(b"deep".to_vec()))],
        };
        let root = Storage {
            class_id: [3; 16],
            entries: vec![
                (units("Zeta"), Node::Document(b"small".to_vec())),
                (units("Alpha"), Node::Document(big.clone())),
                (units("Empty"), Node::Document(Vec::new())),
                (units("Folder"), Node::Storage(inner)),
            ],
        };
        let data = write(&root).unwrap();
        let file = Ole2::open(&data).unwrap();
        assert_eq!(file.top_level_names(), ["Zeta", "Alpha", "Empty", "Folder"]);
        assert_eq!(file.storage(file.root()).unwrap(), root);
        let read = |name: &str| file.read(file.child(file.root(), name).unwrap()).unwrap();
        assert_eq!(read("Alpha"), big);
        assert_eq!(read("Zeta"), b"small");
    }

    #[test]
    fn writes_more_allocation_tables_than_the_header_holds() {
        let large = vec![1u8; 7_500_000];
        let root = Storage {
            class_id: [0; 16],
            entries: vec![(units("Large"), Node::Document(large.clone()))],
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
