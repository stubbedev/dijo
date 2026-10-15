//! OLE2 compound documents as POI's `POIFSFileSystem` reads them: the header, the block
//! allocation tables with their extension blocks, the mini stream and its table, and the tree of
//! directory entries, opened where POI opens one and failing where it fails. Tika's detectors
//! name a document by its entries, and its Office parsers read the documents in them.

use std::collections::HashSet;

/// The signature an OLE2 compound document starts with.
pub(crate) const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const END_OF_CHAIN: i32 = -2;
const UNUSED_BLOCK: i32 = -1;
const NO_INDEX: i32 = -1;
const PROPERTY_SIZE: usize = 128;
const SMALL_BLOCK_SIZE: usize = 64;
const BIG_BLOCK_MINIMUM_DOCUMENT_SIZE: i32 = 4096;
const MAX_PROPERTY_DEPTH: usize = 1000;
const MAX_BATS_IN_HEADER: i32 = 109;
/// The most block allocation table blocks `POIFSFileSystem` reads a stream with.
const MAX_BLOCK_COUNT: i32 = 65_535;
/// The most bytes `POIFSFileSystem` reads a stream into.
const MAX_ALLOCATION_SIZE: usize = 250_000_000;

/// Why POI cannot open a compound document, or read a document in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unreadable(pub(crate) &'static str);

type Result<T> = std::result::Result<T, Unreadable>;

/// What an entry of the directory tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    Directory,
    Document,
    Root,
}

/// A directory entry of the compound document.
#[derive(Debug)]
struct Property {
    name: Vec<u16>,
    kind: EntryKind,
    previous: i32,
    next: i32,
    child: i32,
    start: i32,
    size: i32,
    /// The entries POI's property tree gives a directory, in its order.
    children: Vec<EntryId>,
    child_names: HashSet<Vec<u16>>,
}

/// An entry of the directory tree, by its index in the property table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct EntryId(usize);

fn int_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// POI's `ChainLoopDetector`: a block claimed twice is a loop; blocks past the data are not
/// tracked.
struct LoopDetector {
    used: Vec<bool>,
}

impl LoopDetector {
    fn new(raw_size: usize, block_size: usize) -> LoopDetector {
        LoopDetector {
            used: vec![false; raw_size.div_ceil(block_size)],
        }
    }

    fn claim(&mut self, block: i32) -> Result<()> {
        if block >= 0 && block as usize >= self.used.len() {
            return Ok(());
        }
        let slot =
            usize::try_from(block).map_err(|_| Unreadable("a chain names a negative block"))?;
        if std::mem::replace(&mut self.used[slot], true) {
            return Err(Unreadable("a chain loops"));
        }
        Ok(())
    }
}

/// A compound document POI opened.
pub(crate) struct Ole2<'a> {
    data: &'a [u8],
    block_size: usize,
    bats: Vec<Vec<i32>>,
    sbats: Vec<Vec<i32>>,
    properties: Vec<Option<Property>>,
}

impl<'a> Ole2<'a> {
    /// `new POIFSFileSystem(file)`.
    pub(crate) fn open(data: &'a [u8]) -> Result<Ole2<'a>> {
        Self::open_as(data, false)
    }

    /// `new POIFSFileSystem(stream)`, which reads the stream into memory first: only as much
    /// of it as the header's block allocation tables can address, and nothing when they count
    /// no block or too many. A stream ending in a partial block, which POI then reads short
    /// where it pads a file's, is unreadable here.
    pub(crate) fn open_stream(data: &'a [u8]) -> Result<Ole2<'a>> {
        Self::open_as(data, true)
    }

    fn open_as(data: &'a [u8], stream: bool) -> Result<Ole2<'a>> {
        if data.is_empty() {
            return Err(Unreadable("the document is empty"));
        }
        let mut header = [0u8; 512];
        let available = data.len().min(512);
        header[..available].copy_from_slice(&data[..available]);
        if header[..8] != SIGNATURE {
            return Err(Unreadable("the document has no OLE2 signature"));
        }
        let block_size = match header[30] {
            12 => 4096,
            9 => 512,
            _ => return Err(Unreadable("the block size is neither 512 nor 4096")),
        };
        let bat_count = int_at(&header, 0x2C);
        let data = if stream {
            streamed(data, block_size, bat_count)?
        } else {
            data
        };
        let mut file = Ole2 {
            data,
            block_size,
            bats: Vec::new(),
            sbats: Vec::new(),
            properties: Vec::new(),
        };
        let property_start = int_at(&header, 0x30);
        let sbat_start = int_at(&header, 0x3C);
        let sbat_count = int_at(&header, 0x40);
        let xbat_start = int_at(&header, 0x44);
        let xbat_count = int_at(&header, 0x48);
        let in_header = bat_count.min(MAX_BATS_IN_HEADER);
        if in_header < 0 {
            return Err(Unreadable("the block allocation table count is negative"));
        }
        let mut detector = LoopDetector::new(data.len(), block_size);
        for i in 0..in_header as usize {
            let at = int_at(&header, 0x4C + 4 * i);
            file.read_bat(at, &mut detector)?;
        }
        let entries = block_size / 4;
        let mut remaining = bat_count - in_header;
        let mut next = xbat_start;
        for _ in 0..xbat_count.max(0) {
            detector.claim(next)?;
            let xbat = file.int_block(next)?;
            next = xbat[entries - 1];
            let count = remaining.min(entries as i32 - 1);
            for &at in xbat.iter().take(count.max(0) as usize) {
                if at == UNUSED_BLOCK || at == END_OF_CHAIN {
                    break;
                }
                file.read_bat(at, &mut detector)?;
            }
            remaining -= count;
        }
        file.read_properties(property_start)?;
        let mut next = sbat_start;
        let mut read = 0;
        while read < sbat_count && next != END_OF_CHAIN {
            detector.claim(next)?;
            let sbat = file.int_block(next)?;
            file.sbats.push(sbat);
            next = file.next_block(next)?;
            read += 1;
        }
        Ok(file)
    }

    fn read_bat(&mut self, at: i32, detector: &mut LoopDetector) -> Result<()> {
        detector.claim(at)?;
        let bat = self.int_block(at)?;
        self.bats.push(bat);
        Ok(())
    }

    /// A big block, padded with zeros past the end of the file, as POI reads one.
    fn block(&self, index: i32) -> Result<Vec<u8>> {
        let start = (i64::from(index) + 1) * self.block_size as i64;
        if start < 0 || start >= self.data.len() as i64 {
            return Err(Unreadable("a block lies outside the document"));
        }
        let start = start as usize;
        let mut block = vec![0u8; self.block_size];
        let end = (start + self.block_size).min(self.data.len());
        block[..end - start].copy_from_slice(&self.data[start..end]);
        Ok(block)
    }

    fn int_block(&self, index: i32) -> Result<Vec<i32>> {
        let block = self.block(index)?;
        Ok(block
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes(*c))
            .collect())
    }

    fn next_in(table: &[Vec<i32>], entries: usize, block: i32) -> Result<i32> {
        let block =
            usize::try_from(block).map_err(|_| Unreadable("a chain names a negative block"))?;
        table
            .get(block / entries)
            .map(|bat| bat[block % entries])
            .ok_or(Unreadable("a block lies outside the allocation table"))
    }

    fn next_block(&self, block: i32) -> Result<i32> {
        Self::next_in(&self.bats, self.block_size / 4, block)
    }

    /// The big blocks of a chain, as `POIFSStream` iterates them.
    fn chain(&self, start: i32) -> Result<Vec<i32>> {
        if start == END_OF_CHAIN {
            return Err(Unreadable("a chain starts at its end"));
        }
        let mut detector = LoopDetector::new(self.data.len(), self.block_size);
        let mut blocks = Vec::new();
        let mut next = start;
        while next != END_OF_CHAIN {
            detector.claim(next)?;
            self.block(next)?;
            blocks.push(next);
            next = self.next_block(next)?;
        }
        Ok(blocks)
    }

    /// The `n`th big block of a chain, walked as `StreamBlockOffsetIterator` walks it: each
    /// block claimed and its successor looked up.
    fn nth_in_chain(&self, start: i32, n: usize) -> Result<i32> {
        let past = Unreadable("the mini stream ends before a small block");
        if start == END_OF_CHAIN {
            return Err(past);
        }
        let mut detector = LoopDetector::new(self.data.len(), self.block_size);
        let mut next = start;
        for _ in 0..n {
            if next == END_OF_CHAIN {
                return Err(past);
            }
            detector.claim(next)?;
            next = self.next_block(next)?;
        }
        if next == END_OF_CHAIN {
            return Err(past);
        }
        detector.claim(next)?;
        self.next_block(next)?;
        Ok(next)
    }

    fn read_properties(&mut self, start: i32) -> Result<()> {
        for block in self.chain(start)? {
            let data = self.block(block)?;
            for raw in data.as_chunks::<PROPERTY_SIZE>().0 {
                let kind = match raw[0x42] {
                    1 => EntryKind::Directory,
                    2 => EntryKind::Document,
                    5 => EntryKind::Root,
                    _ => {
                        self.properties.push(None);
                        continue;
                    }
                };
                let name_length = i32::from(i16::from_le_bytes([raw[0x40], raw[0x41]])) / 2 - 1;
                let name = if name_length < 1 {
                    Vec::new()
                } else if name_length as usize * 2 > PROPERTY_SIZE {
                    return Err(Unreadable("an entry's name runs past its property"));
                } else {
                    raw[..name_length as usize * 2]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes(*c))
                        .collect()
                };
                self.properties.push(Some(Property {
                    name,
                    kind,
                    previous: int_at(raw, 0x44),
                    next: int_at(raw, 0x48),
                    child: int_at(raw, 0x4C),
                    start: int_at(raw, 0x74),
                    size: int_at(raw, 0x78),
                    children: Vec::new(),
                    child_names: HashSet::new(),
                }));
            }
        }
        match self.properties.first() {
            Some(Some(root)) if root.kind == EntryKind::Root => self.populate(0, 0),
            _ => Err(Unreadable("the first entry is no root")),
        }
    }

    fn property(&self, index: i32) -> Result<Option<usize>> {
        let index =
            usize::try_from(index).map_err(|_| Unreadable("an entry names a negative entry"))?;
        match self.properties.get(index) {
            None => Err(Unreadable("an entry names an entry past the table")),
            Some(None) => Ok(None),
            Some(Some(_)) => Ok(Some(index)),
        }
    }

    /// `PropertyTable.populatePropertyTree`.
    fn populate(&mut self, directory: usize, depth: usize) -> Result<()> {
        if depth > MAX_PROPERTY_DEPTH {
            return Err(Unreadable("the directory tree is too deep"));
        }
        let first = self.properties[directory]
            .as_ref()
            .expect("a directory")
            .child;
        if first == NO_INDEX {
            return Ok(());
        }
        let mut stack = vec![self.property(first)?];
        while let Some(entry) = stack.pop() {
            let Some(index) = entry else {
                continue;
            };
            let (name, kind, previous, next) = {
                let p = self.properties[index].as_ref().expect("a property");
                (p.name.clone(), p.kind, p.previous, p.next)
            };
            let parent = self.properties[directory].as_mut().expect("a directory");
            if !parent.child_names.insert(name) {
                return Err(Unreadable("a directory has two entries of one name"));
            }
            parent.children.push(EntryId(index));
            if kind != EntryKind::Document {
                self.populate(index, depth + 1)?;
            }
            for sibling in [previous, next] {
                if sibling != NO_INDEX {
                    stack.push(self.property(sibling)?);
                }
            }
        }
        Ok(())
    }

    fn entry(&self, id: EntryId) -> &Property {
        self.properties[id.0]
            .as_ref()
            .expect("an entry is a property")
    }

    /// The root directory.
    pub(crate) fn root(&self) -> EntryId {
        EntryId(0)
    }

    /// The entries of a directory, in the order POI iterates them.
    pub(crate) fn children(&self, directory: EntryId) -> impl Iterator<Item = EntryId> + '_ {
        self.entry(directory).children.iter().copied()
    }

    /// The entry of a directory named `name` exactly, as `getEntry` finds it.
    pub(crate) fn child(&self, directory: EntryId, name: &str) -> Option<EntryId> {
        let units: Vec<u16> = name.encode_utf16().collect();
        self.children(directory)
            .find(|&child| self.entry(child).name == units)
    }

    /// The entry of a directory named `name` in any case, as `getEntryCaseInsensitive` finds
    /// it: the last of its names upper-cased.
    pub(crate) fn child_ignoring_case(&self, directory: EntryId, name: &str) -> Option<EntryId> {
        let upper = name.to_uppercase();
        self.children(directory)
            .filter(|&child| self.name(child).to_uppercase() == upper)
            .last()
    }

    /// An entry's name, as a Java string decoded, unpaired surrogates replaced.
    pub(crate) fn name(&self, id: EntryId) -> String {
        String::from_utf16_lossy(&self.entry(id).name)
    }

    pub(crate) fn kind(&self, id: EntryId) -> EntryKind {
        self.entry(id).kind
    }

    /// The names of the root's entries.
    pub(crate) fn top_level_names(&self) -> Vec<String> {
        self.children(self.root()).map(|i| self.name(i)).collect()
    }

    /// Whether an entry named `upper` in upper case is within `max_depth` levels of `id`, with
    /// `upper_case` making a name upper case.
    pub(crate) fn find_recursively(
        &self,
        id: EntryId,
        upper: &str,
        depth: usize,
        max_depth: usize,
        upper_case: &dyn Fn(&str) -> String,
    ) -> bool {
        let entry = self.entry(id);
        if upper_case(&self.name(id)) == upper {
            return true;
        }
        depth < max_depth
            && entry.kind != EntryKind::Document
            && self
                .children(id)
                .any(|child| self.find_recursively(child, upper, depth + 1, max_depth, upper_case))
    }

    /// The content of a document entry, read whole as `DocumentInputStream` reads it.
    pub(crate) fn read(&self, id: EntryId) -> Result<Vec<u8>> {
        let entry = self.entry(id);
        let size =
            usize::try_from(entry.size).map_err(|_| Unreadable("a document's size is negative"))?;
        if size == 0 {
            return Ok(Vec::new());
        }
        let past = Unreadable("a document's chain ends before its size");
        let mut content = Vec::with_capacity(size);
        if entry.size < BIG_BLOCK_MINIMUM_DOCUMENT_SIZE {
            let root = self.entry(self.root());
            let root_size = usize::try_from(root.size)
                .map_err(|_| Unreadable("the mini stream's size is negative"))?;
            let mut detector = LoopDetector::new(root_size, SMALL_BLOCK_SIZE);
            let mut next = entry.start;
            while content.len() < size {
                if next == END_OF_CHAIN {
                    return Err(past);
                }
                detector.claim(next)?;
                let byte_offset = i64::from(next) * SMALL_BLOCK_SIZE as i64;
                let big = usize::try_from(byte_offset / self.block_size as i64)
                    .map_err(|_| Unreadable("a small block lies before the mini stream"))?;
                let within = (byte_offset % self.block_size as i64) as usize;
                let block = self.block(self.nth_in_chain(root.start, big)?)?;
                let piece = &block[within..within + SMALL_BLOCK_SIZE];
                content.extend_from_slice(&piece[..piece.len().min(size - content.len())]);
                next = Self::next_in(&self.sbats, self.block_size / 4, next)?;
            }
        } else {
            let mut detector = LoopDetector::new(self.data.len(), self.block_size);
            let mut next = entry.start;
            while content.len() < size {
                if next == END_OF_CHAIN {
                    return Err(past);
                }
                detector.claim(next)?;
                let block = self.block(next)?;
                content.extend_from_slice(&block[..block.len().min(size - content.len())]);
                next = self.next_block(next)?;
            }
        }
        Ok(content)
    }
}

/// The part of a stream `POIFSFileSystem` reads into memory, as `sanityCheckBlockCount` and
/// `BATBlock.calculateMaximumSize` bound it.
fn streamed(data: &[u8], block_size: usize, bat_count: i32) -> Result<&[u8]> {
    if !(1..=MAX_BLOCK_COUNT).contains(&bat_count) {
        return Err(Unreadable(
            "the block allocation table count is out of range",
        ));
    }
    let addressable = (1 + bat_count as usize * (block_size / 4)) * block_size;
    if addressable > MAX_ALLOCATION_SIZE {
        return Err(Unreadable(
            "the block allocation tables address too much to read",
        ));
    }
    let data = &data[..data.len().min(addressable)];
    if !data.len().is_multiple_of(block_size) {
        return Err(Unreadable(
            "the stream ends in a partial block, which POI reads short",
        ));
    }
    Ok(data)
}

/// Whether a document starts with the OLE2 signature.
pub(crate) fn is_ole(data: &[u8]) -> bool {
    data.starts_with(&SIGNATURE)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A compound document of 512-byte sectors whose root holds the named entries, each a
    /// document of `content` in the mini stream, or a directory where the name ends in `/`.
    pub(crate) fn compound(names: &[&str], content: &[u8]) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..8].copy_from_slice(&SIGNATURE);
        header[0x1A] = 3;
        header[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
        header[30] = 9;
        header[32] = 6;
        header[0x2C..0x30].copy_from_slice(&1i32.to_le_bytes());
        header[0x30..0x34].copy_from_slice(&1i32.to_le_bytes());
        header[0x38..0x3C].copy_from_slice(&4096i32.to_le_bytes());
        header[0x3C..0x40].copy_from_slice(&2i32.to_le_bytes());
        header[0x40..0x44].copy_from_slice(&1i32.to_le_bytes());
        header[0x44..0x48].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
        for i in 0..109 {
            let value = if i == 0 { 0 } else { UNUSED_BLOCK };
            header[0x4C + 4 * i..0x50 + 4 * i].copy_from_slice(&value.to_le_bytes());
        }
        let mut fat = vec![UNUSED_BLOCK; 128];
        fat[0] = -3;
        fat[1] = END_OF_CHAIN;
        fat[2] = END_OF_CHAIN;
        fat[3] = END_OF_CHAIN;
        let mut directory = vec![0u8; 512];
        let mut entry =
            |slot: usize, name: &str, kind: u8, child: i32, next: i32, start: i32, size: i32| {
                let raw = &mut directory[slot * 128..(slot + 1) * 128];
                let units: Vec<u16> = name.encode_utf16().collect();
                for (i, unit) in units.iter().enumerate() {
                    raw[2 * i..2 * i + 2].copy_from_slice(&unit.to_le_bytes());
                }
                raw[0x40..0x42].copy_from_slice(&(((units.len() + 1) * 2) as u16).to_le_bytes());
                raw[0x42] = kind;
                raw[0x44..0x48].copy_from_slice(&NO_INDEX.to_le_bytes());
                raw[0x48..0x4C].copy_from_slice(&next.to_le_bytes());
                raw[0x4C..0x50].copy_from_slice(&child.to_le_bytes());
                raw[0x74..0x78].copy_from_slice(&start.to_le_bytes());
                raw[0x78..0x7C].copy_from_slice(&size.to_le_bytes());
            };
        entry(
            0,
            "Root Entry",
            5,
            if names.is_empty() { NO_INDEX } else { 1 },
            NO_INDEX,
            3,
            64,
        );
        for (i, name) in names.iter().enumerate().take(3) {
            let next = if i + 1 < names.len().min(3) {
                i as i32 + 2
            } else {
                NO_INDEX
            };
            match name.strip_suffix('/') {
                Some(directory) => entry(i + 1, directory, 1, NO_INDEX, next, 0, 0),
                None => entry(i + 1, name, 2, NO_INDEX, next, 0, content.len() as i32),
            }
        }
        let mut sbat = vec![UNUSED_BLOCK; 128];
        sbat[0] = END_OF_CHAIN;
        let mut mini = vec![0u8; 512];
        mini[..content.len()].copy_from_slice(content);
        let mut data = header;
        data.extend(fat.iter().flat_map(|v| v.to_le_bytes()));
        data.extend(directory);
        data.extend(sbat.iter().flat_map(|v| v.to_le_bytes()));
        data.extend(mini);
        data
    }

    #[test]
    fn reads_entries_and_documents_as_poi_does() {
        let data = compound(&["One", "Two", "Folder/"], b"content");
        let file = Ole2::open(&data).unwrap();
        let names: Vec<String> = file.top_level_names();
        assert_eq!(names, ["One", "Two", "Folder"]);
        let one = file.child(file.root(), "One").unwrap();
        assert_eq!(file.kind(one), EntryKind::Document);
        assert_eq!(file.read(one).unwrap(), b"content");
        assert!(file.child(file.root(), "one").is_none());
    }

    #[test]
    fn fails_where_poi_fails() {
        let mut data = compound(&["WordDocument"], b"");
        data.truncate(600);
        assert!(Ole2::open(&data).is_err());
        assert!(Ole2::open(b"PK\x03\x04").is_err());
    }
}
