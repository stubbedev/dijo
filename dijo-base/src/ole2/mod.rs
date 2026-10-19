//! OLE2 compound documents as POI's `POIFSFileSystem` reads them: the header, the block
//! allocation tables with their extension blocks, the mini stream and its table, and the tree of
//! directory entries, opened where POI opens one and failing where it fails. Tika's detectors
//! name a document by its entries, and its Office parsers read the documents in them.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::HashSet;

mod write;

pub use write::{write, Node, Storage};

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
pub struct Unreadable(pub &'static str);

type Result<T> = std::result::Result<T, Unreadable>;

/// What an entry of the directory tree is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    Document,
    Root,
}

/// A directory entry of the compound document.
#[derive(Debug)]
struct Property {
    pub name: Vec<u16>,
    kind: EntryKind,
    previous: i32,
    next: i32,
    child: i32,
    start: i32,
    size: i32,
    /// The storage class id, as the property holds it.
    class_id: [u8; 16],
    /// The entries POI's property tree gives a directory, in its order.
    children: Vec<EntryId>,
    child_names: HashSet<Vec<u16>>,
    /// How many directories lie between the root and the entry.
    depth: usize,
}

/// An entry of the directory tree, by its index in the property table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(usize);

fn int_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// POI's `ChainLoopDetector`: a block claimed twice is a loop; blocks past the data are not
/// tracked. A bit per block.
struct LoopDetector {
    used: Vec<u64>,
    blocks: usize,
}

impl LoopDetector {
    fn new(raw_size: usize, block_size: usize) -> LoopDetector {
        let blocks = raw_size.div_ceil(block_size);
        LoopDetector {
            used: vec![0; blocks.div_ceil(64)],
            blocks,
        }
    }

    fn claim(&mut self, block: i32) -> Result<()> {
        if block >= 0 && block as usize >= self.blocks {
            return Ok(());
        }
        let slot =
            usize::try_from(block).map_err(|_| Unreadable("a chain names a negative block"))?;
        let (word, bit) = (slot / 64, 1u64 << (slot % 64));
        if self.used[word] & bit != 0 {
            return Err(Unreadable("a chain loops"));
        }
        self.used[word] |= bit;
        Ok(())
    }
}

/// Where `POIFSFileSystem` reads a compound document from, which decides how it reads the block
/// the data ends inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// A file (`FileBackedDataSource`): a block is read whole, zeros past the end of the file.
    File,
    /// A stream read into memory (`ByteArrayBackedDataSource`): a block is read short, only as far
    /// as the data goes.
    Stream,
}

/// The big blocks of the mini stream, as far as `StreamBlockOffsetIterator` walks its chain
/// before it fails, and why it fails past them.
#[derive(Debug)]
struct MiniStream {
    blocks: Vec<i32>,
    past: Unreadable,
}

/// A compound document POI opened. Its blocks are read in place: the block allocation tables
/// are kept as the blocks that hold them, and a document's content as the runs of the data it
/// lies in.
pub struct Ole2<'a> {
    data: &'a [u8],
    source: Source,
    block_size: usize,
    /// The blocks of the block allocation table, in its order.
    bats: Vec<i32>,
    /// The blocks of the small block allocation table, in its order.
    sbats: Vec<i32>,
    properties: Vec<Option<Property>>,
    mini_stream: OnceCell<MiniStream>,
}

impl<'a> Ole2<'a> {
    /// `new POIFSFileSystem(file)`.
    pub fn open(data: &'a [u8]) -> Result<Ole2<'a>> {
        Self::open_as(data, Source::File)
    }

    /// `new POIFSFileSystem(stream)`, which reads the stream into memory first: only as much
    /// of it as the header's block allocation tables can address, and nothing when they count
    /// no block or too many. The block the stream ends inside is read short where a file's is
    /// padded: a block allocation table in it fails, a document reads on into the next block of
    /// its chain, and a small block past its end fails.
    pub fn open_stream(data: &'a [u8]) -> Result<Ole2<'a>> {
        Self::open_as(data, Source::Stream)
    }

    fn open_as(data: &'a [u8], source: Source) -> Result<Ole2<'a>> {
        if data.is_empty() {
            return Err(Unreadable("the document is empty"));
        }
        let mut header = [0u8; 512];
        let available = data.len().min(512);
        header[..available].copy_from_slice(&data[..available]);
        if header[..8] != SIGNATURE {
            return Err(Unreadable("the document has no OLE2 signature"));
        }
        let block_size = block_size(header[30])?;
        let bat_count = int_at(&header, 0x2C);
        let data = match source {
            Source::Stream => streamed(data, block_size, bat_count)?,
            Source::File => data,
        };
        let mut file = Ole2 {
            data,
            source,
            block_size,
            bats: Vec::new(),
            sbats: Vec::new(),
            properties: Vec::new(),
            mini_stream: OnceCell::new(),
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
        let entries = file.entries_per_block();
        let mut remaining = bat_count - in_header;
        let mut next = xbat_start;
        for _ in 0..xbat_count.max(0) {
            detector.claim(next)?;
            let xbat = file.int_block(next)?;
            next = file.table_entry(xbat, entries - 1);
            let count = remaining.min(entries as i32 - 1);
            for index in 0..count.max(0) as usize {
                let at = file.table_entry(xbat, index);
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

    fn entries_per_block(&self) -> usize {
        self.block_size / 4
    }

    /// Where a big block starts in the data, if it starts inside it.
    fn block_start(&self, index: i32) -> Result<usize> {
        let start = (i64::from(index) + 1) * self.block_size as i64;
        if start < 0 || start >= self.data.len() as i64 {
            return Err(Unreadable("a block lies outside the document"));
        }
        Ok(start as usize)
    }

    /// A big block as POI's data source reads it: padded with zeros past the end of a file, short
    /// past the end of a stream.
    fn block(&self, index: i32) -> Result<Cow<'a, [u8]>> {
        let start = self.block_start(index)?;
        let end = (start + self.block_size).min(self.data.len());
        let block = &self.data[start..end];
        if self.source == Source::Stream || block.len() == self.block_size {
            return Ok(Cow::Borrowed(block));
        }
        let mut padded = block.to_vec();
        padded.resize(self.block_size, 0);
        Ok(Cow::Owned(padded))
    }

    /// A block of a block allocation table, which `BATBlock.createBATBlock` reads whole: the
    /// block, whose entries are then read in place.
    fn int_block(&self, index: i32) -> Result<i32> {
        let start = self.block_start(index)?;
        if self.source == Source::Stream && start + self.block_size > self.data.len() {
            return Err(Unreadable(
                "a block allocation table lies in the block the stream ends inside",
            ));
        }
        Ok(index)
    }

    /// The `index`th entry of the block allocation table block `block`, zero past the end of a
    /// file.
    fn table_entry(&self, block: i32, index: usize) -> i32 {
        let at = (block as usize + 1) * self.block_size + 4 * index;
        let mut bytes = [0u8; 4];
        if let Some(available) = self.data.get(at..) {
            let length = available.len().min(4);
            bytes[..length].copy_from_slice(&available[..length]);
        }
        i32::from_le_bytes(bytes)
    }

    fn next_in(&self, table: &[i32], block: i32) -> Result<i32> {
        let block =
            usize::try_from(block).map_err(|_| Unreadable("a chain names a negative block"))?;
        let entries = self.entries_per_block();
        table
            .get(block / entries)
            .map(|&bat| self.table_entry(bat, block % entries))
            .ok_or(Unreadable("a block lies outside the allocation table"))
    }

    fn next_block(&self, block: i32) -> Result<i32> {
        self.next_in(&self.bats, block)
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
            self.block_start(next)?;
            blocks.push(next);
            next = self.next_block(next)?;
        }
        Ok(blocks)
    }

    /// The big blocks of the mini stream as `StreamBlockOffsetIterator` walks them, each block
    /// claimed and its successor looked up, up to the first it fails on: walked once, as every
    /// walk of it from its start is alike.
    fn mini_stream(&self) -> &MiniStream {
        self.mini_stream.get_or_init(|| {
            let past = Unreadable("the mini stream ends before a small block");
            let mut detector = LoopDetector::new(self.data.len(), self.block_size);
            let mut blocks = Vec::new();
            let mut next = self.entry(self.root()).start;
            loop {
                if next == END_OF_CHAIN {
                    return MiniStream { blocks, past };
                }
                let step = detector.claim(next).and_then(|()| self.next_block(next));
                match step {
                    Ok(successor) => {
                        blocks.push(next);
                        next = successor;
                    }
                    Err(unreadable) => {
                        return MiniStream {
                            blocks,
                            past: unreadable,
                        }
                    }
                }
            }
        })
    }

    /// The `n`th big block of the mini stream.
    fn nth_in_mini_stream(&self, n: usize) -> Result<i32> {
        let mini = self.mini_stream();
        mini.blocks.get(n).copied().ok_or_else(|| mini.past.clone())
    }

    fn read_properties(&mut self, start: i32) -> Result<()> {
        for block in self.chain(start)? {
            let mut data = self.block(block)?.into_owned();
            data.resize(self.block_size, 0);
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
                    class_id: raw[0x50..0x60].try_into().expect("16 bytes"),
                    children: Vec::new(),
                    child_names: HashSet::new(),
                    depth: 0,
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
            self.properties[index].as_mut().expect("a property").depth = depth;
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
    pub fn root(&self) -> EntryId {
        EntryId(0)
    }

    /// The entries of a directory, in the order POI iterates them.
    pub fn children(&self, directory: EntryId) -> impl Iterator<Item = EntryId> + '_ {
        self.entry(directory).children.iter().copied()
    }

    /// The entry of a directory named `name` exactly, as `getEntry` finds it.
    pub fn child(&self, directory: EntryId, name: &str) -> Option<EntryId> {
        let units: Vec<u16> = name.encode_utf16().collect();
        self.children(directory)
            .find(|&child| self.entry(child).name == units)
    }

    /// The entry of a directory named `name` in any case, as `getEntryCaseInsensitive` finds
    /// it: the last of its names upper-cased.
    pub fn child_ignoring_case(&self, directory: EntryId, name: &str) -> Option<EntryId> {
        let upper = name.to_uppercase();
        self.children(directory)
            .filter(|&child| self.name(child).to_uppercase() == upper)
            .last()
    }

    /// `DirectoryNode.hasEntry` on the root: whether it has an entry of exactly this name.
    pub fn has_root_entry(&self, name: &str) -> bool {
        self.child(self.root(), name).is_some()
    }

    /// `createDocumentInputStream(name)` on the root, read whole: the document found ignoring
    /// case, or why POI cannot read it.
    pub fn root_document(&self, name: &str) -> std::result::Result<Vec<u8>, String> {
        self.root_content(name).map(|content| content.to_vec())
    }

    /// [`Ole2::root_document`] in place.
    pub fn root_content(&self, name: &str) -> std::result::Result<Content<'a>, String> {
        let entry = self
            .child_ignoring_case(self.root(), name)
            .ok_or_else(|| format!("the compound document has no entry {name:?}"))?;
        if self.kind(entry) != EntryKind::Document {
            return Err(format!(
                "the compound document's entry {name:?} is not a document"
            ));
        }
        self.content(entry).map_err(|Unreadable(reason)| {
            format!("the compound document's entry {name:?} is unreadable: {reason}")
        })
    }

    /// An entry's name, as a Java string decoded, unpaired surrogates replaced.
    pub fn name(&self, id: EntryId) -> String {
        String::from_utf16_lossy(&self.entry(id).name)
    }

    /// How many directories lie between the root and an entry: none for the root's entries.
    pub fn depth(&self, id: EntryId) -> usize {
        self.entry(id).depth
    }

    pub fn kind(&self, id: EntryId) -> EntryKind {
        self.entry(id).kind
    }

    /// `getStorageClsid`: the class id of an entry, as its property holds it.
    pub fn class_id(&self, id: EntryId) -> [u8; 16] {
        self.entry(id).class_id
    }

    /// An entry's name as POI reads it, in UTF-16 units.
    pub(crate) fn name_units(&self, id: EntryId) -> &[u16] {
        &self.entry(id).name
    }

    /// The names of the root's entries.
    pub fn top_level_names(&self) -> Vec<String> {
        self.children(self.root()).map(|i| self.name(i)).collect()
    }

    /// Whether an entry named `upper` in upper case is within `max_depth` levels of `id`, with
    /// `upper_case` making a name upper case.
    pub fn find_recursively(
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
    pub fn read(&self, id: EntryId) -> Result<Vec<u8>> {
        self.content(id).map(|content| content.to_vec())
    }

    /// The content of a document entry as `DocumentInputStream` reads it, in place: its chain
    /// walked and checked as POI walks it, and its bytes left where they lie.
    pub fn content(&self, id: EntryId) -> Result<Content<'a>> {
        let entry = self.entry(id);
        let size =
            usize::try_from(entry.size).map_err(|_| Unreadable("a document's size is negative"))?;
        let mut runs = Runs::new(size);
        if size == 0 {
            return Ok(runs.into_content(self.data));
        }
        let past = Unreadable("a document's chain ends before its size");
        if entry.size < BIG_BLOCK_MINIMUM_DOCUMENT_SIZE {
            let root_size = usize::try_from(self.entry(self.root()).size)
                .map_err(|_| Unreadable("the mini stream's size is negative"))?;
            let mut detector = LoopDetector::new(root_size, SMALL_BLOCK_SIZE);
            let mut next = entry.start;
            while !runs.is_full() {
                if next == END_OF_CHAIN {
                    return Err(past);
                }
                detector.claim(next)?;
                let byte_offset = i64::from(next) * SMALL_BLOCK_SIZE as i64;
                let big = usize::try_from(byte_offset / self.block_size as i64)
                    .map_err(|_| Unreadable("a small block lies before the mini stream"))?;
                let within = (byte_offset % self.block_size as i64) as usize;
                let at = self.block_start(self.nth_in_mini_stream(big)?)? + within;
                let in_data = self.data.len().saturating_sub(at).min(SMALL_BLOCK_SIZE);
                if self.source == Source::Stream && in_data < SMALL_BLOCK_SIZE {
                    return Err(Unreadable(
                        "a small block lies past the end of the block the stream ends inside",
                    ));
                }
                runs.push(at, in_data, SMALL_BLOCK_SIZE - in_data);
                next = self.next_in(&self.sbats, next)?;
            }
        } else {
            let mut detector = LoopDetector::new(self.data.len(), self.block_size);
            let mut next = entry.start;
            while !runs.is_full() {
                if next == END_OF_CHAIN {
                    return Err(past);
                }
                detector.claim(next)?;
                let at = self.block_start(next)?;
                let in_data = (self.data.len() - at).min(self.block_size);
                let zeros = match self.source {
                    Source::File => self.block_size - in_data,
                    Source::Stream => 0,
                };
                runs.push(at, in_data, zeros);
                next = self.next_block(next)?;
            }
        }
        Ok(runs.into_content(self.data))
    }
}

/// Zeros, which a block read past the end of a file is padded with: as many as a block holds.
static ZEROS: [u8; 4096] = [0; 4096];

/// A run of a document's content: bytes of the compound document from `at`, or zeros.
#[derive(Debug, Clone, Copy)]
struct Run {
    /// Where the run starts in the content.
    start: usize,
    /// Where its bytes start in the compound document, `None` for zeros.
    at: Option<usize>,
    length: usize,
}

/// A document's content being gathered, up to its size.
struct Runs {
    runs: Vec<Run>,
    size: usize,
    length: usize,
}

impl Runs {
    fn new(size: usize) -> Self {
        Self {
            runs: Vec::new(),
            size,
            length: 0,
        }
    }

    fn is_full(&self) -> bool {
        self.length >= self.size
    }

    /// Adds `in_data` bytes of the compound document from `at` and `zeros` zeros after them, as
    /// far as the size goes, the bytes joined to the run before where they follow on from it.
    fn push(&mut self, at: usize, in_data: usize, zeros: usize) {
        let in_data = in_data.min(self.size - self.length);
        if in_data > 0 {
            match self.runs.last_mut() {
                Some(Run {
                    at: Some(previous),
                    length,
                    ..
                }) if *previous + *length == at => *length += in_data,
                _ => self.runs.push(Run {
                    start: self.length,
                    at: Some(at),
                    length: in_data,
                }),
            }
            self.length += in_data;
        }
        let zeros = zeros.min(self.size - self.length);
        if zeros > 0 {
            self.runs.push(Run {
                start: self.length,
                at: None,
                length: zeros,
            });
            self.length += zeros;
        }
    }

    fn into_content(self, data: &[u8]) -> Content<'_> {
        Content {
            data,
            runs: self.runs,
            length: self.length,
        }
    }
}

/// The content of a document entry, in place in the compound document: the runs of its bytes in
/// the order of its chain, and the zeros POI reads past the end of a file.
#[derive(Debug, Clone)]
pub struct Content<'a> {
    data: &'a [u8],
    runs: Vec<Run>,
    length: usize,
}

/// Bytes in memory as a content of one run.
impl<'a> From<&'a [u8]> for Content<'a> {
    fn from(bytes: &'a [u8]) -> Self {
        let mut runs = Runs::new(bytes.len());
        runs.push(0, bytes.len(), 0);
        runs.into_content(bytes)
    }
}

impl<'a> Content<'a> {
    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    fn bytes_of(&self, run: &Run) -> &'a [u8] {
        match run.at {
            Some(at) => &self.data[at..at + run.length],
            None => &ZEROS[..run.length],
        }
    }

    /// The content's pieces in order, each where it lies.
    pub fn pieces(&self) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.runs.iter().map(|run| self.bytes_of(run))
    }

    /// The content read in order, as `DocumentInputStream` reads it, each piece where it lies.
    pub fn reader(&self) -> ContentReader<'_, 'a> {
        ContentReader {
            content: self,
            run: 0,
            at: 0,
        }
    }

    /// `length` bytes from `offset`, in place where they lie in one run and copied together
    /// otherwise, or `None` where they run past the end.
    pub fn range(&self, offset: usize, length: usize) -> Option<Cow<'a, [u8]>> {
        let end = offset
            .checked_add(length)
            .filter(|&end| end <= self.length)?;
        if length == 0 {
            return Some(Cow::Borrowed(&[]));
        }
        let first = self
            .runs
            .partition_point(|run| run.start + run.length <= offset);
        let run = &self.runs[first];
        if end <= run.start + run.length {
            let bytes = self.bytes_of(run);
            return Some(Cow::Borrowed(&bytes[offset - run.start..end - run.start]));
        }
        let mut gathered = Vec::with_capacity(length);
        for run in &self.runs[first..] {
            if run.start >= end {
                break;
            }
            let bytes = self.bytes_of(run);
            let from = offset.saturating_sub(run.start);
            let to = (end - run.start).min(run.length);
            gathered.extend_from_slice(&bytes[from..to]);
        }
        Some(Cow::Owned(gathered))
    }

    /// All of the content: in place where it lies in one run.
    pub fn bytes(&self) -> Cow<'a, [u8]> {
        self.range(0, self.length)
            .expect("the content lies within itself")
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.bytes().into_owned()
    }
}

/// A [`Content`] read in order, by [`Content::reader`]: the run it is in, and how far into it.
pub struct ContentReader<'c, 'a> {
    content: &'c Content<'a>,
    run: usize,
    at: usize,
}

impl std::io::Read for ContentReader<'_, '_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while let Some(run) = self.content.runs.get(self.run) {
            let rest = &self.content.bytes_of(run)[self.at..];
            if rest.is_empty() {
                self.run += 1;
                self.at = 0;
                continue;
            }
            let length = rest.len().min(out.len());
            out[..length].copy_from_slice(&rest[..length]);
            self.at += length;
            return Ok(length);
        }
        Ok(0)
    }
}

/// The size of a big block, as the header's shift gives it.
fn block_size(shift: u8) -> Result<usize> {
    match shift {
        12 => Ok(4096),
        9 => Ok(512),
        _ => Err(Unreadable("the block size is neither 512 nor 4096")),
    }
}

/// `BATBlock.calculateMaximumSize`: the bytes `bat_count` block allocation table blocks address,
/// the header included.
fn maximum_size(block_size: usize, bat_count: i32) -> i64 {
    (1 + i64::from(bat_count) * (block_size / 4) as i64) * block_size as i64
}

/// What the header of a compound document declares of its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Declared {
    /// The size of a big block.
    pub block_size: usize,
    /// `BATBlock.calculateMaximumSize`: the bytes its block allocation tables address, the
    /// header included, which `new POIFSFileSystem(stream)` allocates.
    pub size: i64,
}

impl Declared {
    /// What the header `data` starts with declares, or `None` where `new HeaderBlock(data)`
    /// fails: no whole header, no signature, or a block size POI does not read.
    pub fn read(data: &[u8]) -> Option<Declared> {
        let header = data.get(..512)?;
        if header[..8] != SIGNATURE {
            return None;
        }
        let block_size = block_size(header[30]).ok()?;
        Some(Declared {
            block_size,
            size: maximum_size(block_size, int_at(header, 0x2C)),
        })
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
    let addressable = maximum_size(block_size, bat_count) as usize;
    if addressable > MAX_ALLOCATION_SIZE {
        return Err(Unreadable(
            "the block allocation tables address too much to read",
        ));
    }
    Ok(&data[..data.len().min(addressable)])
}

/// `ClassID.toString()`: 16 bytes as a stream holds a class id, written as a UUID in braces and
/// upper case, the first three fields read little-endian.
pub fn class_id_string(raw: &[u8; 16]) -> String {
    let mut bytes = *raw;
    bytes[..4].reverse();
    bytes[4..6].reverse();
    bytes[6..8].reverse();
    let hex: String = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
    format!(
        "{{{}-{}-{}-{}-{}}}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// Whether a document starts with the OLE2 signature.
pub fn is_ole(data: &[u8]) -> bool {
    data.starts_with(&SIGNATURE)
}

#[cfg(any(test, feature = "testing"))]
pub mod tests {
    use super::*;

    /// A compound document whose root holds the named entries, each a document of `content`,
    /// or an empty directory where the name ends in `/`.
    pub fn compound(names: &[&str], content: &[u8]) -> Vec<u8> {
        let entries = names
            .iter()
            .map(|name| match name.strip_suffix('/') {
                Some(directory) => (
                    directory.encode_utf16().collect(),
                    Node::Storage(Storage {
                        class_id: [0; 16],
                        entries: Vec::new(),
                    }),
                ),
                None => (name.encode_utf16().collect(), Node::Owned(content.to_vec())),
            })
            .collect();
        write::write(&Storage {
            class_id: [0; 16],
            entries,
        })
        .expect("names that fit a property")
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
    fn writes_class_ids_as_poi_does() {
        let raw = [
            0xE0, 0xC9, 0xEA, 0x79, 0xF9, 0xBA, 0xCE, 0x11, 0x8C, 0x82, 0x00, 0xAA, 0x00, 0x4B,
            0xA9, 0x0B,
        ];
        assert_eq!(
            class_id_string(&raw),
            "{79EAC9E0-BAF9-11CE-8C82-00AA004BA90B}"
        );
    }

    #[test]
    fn fails_where_poi_fails() {
        let mut data = compound(&["WordDocument"], b"");
        data.truncate(600);
        assert!(Ole2::open(&data).is_err());
        assert!(Ole2::open(b"PK\x03\x04").is_err());
    }

    /// A document in the block the data ends inside: a file pads the block with zeros, a stream
    /// reads it short and fails where the document's chain ends before its size.
    #[test]
    fn reads_the_last_block_short_from_a_stream_and_padded_from_a_file() {
        let content = vec![b'x'; 4096 + 100];
        let whole = compound(&["Big"], &content);
        let ends_with_content = whole.len() - 412;
        assert!(whole[ends_with_content..].iter().all(|&byte| byte == 0));
        assert_eq!(whole[ends_with_content - 1], b'x');
        let read = |data: &[u8], source: Source| {
            let file = Ole2::open_as(data, source)?;
            file.read(file.child(file.root(), "Big").unwrap())
        };
        let short = &whole[..ends_with_content];
        assert_eq!(read(short, Source::File).unwrap(), content);
        assert_eq!(read(short, Source::Stream).unwrap(), content);
        let cut = &whole[..ends_with_content - 10];
        let mut padded = content.clone();
        padded[4096 + 90..].fill(0);
        assert_eq!(read(cut, Source::File).unwrap(), padded);
        assert_eq!(
            read(cut, Source::Stream),
            Err(Unreadable("a document's chain ends before its size"))
        );
    }

    /// A document of big blocks is read where it lies, its chain checked as POI checks it.
    #[test]
    fn reads_documents_in_place() {
        let content: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let mut data = compound(&["Big", "Small"], &content[..100]);
        let file = Ole2::open(&data).unwrap();
        let small = file
            .content(file.child(file.root(), "Small").unwrap())
            .unwrap();
        assert_eq!(small.to_vec(), &content[..100]);
        let data_range = data.as_ptr_range();
        assert!(
            matches!(small.bytes(), Cow::Borrowed(bytes) if data_range.contains(&bytes.as_ptr()))
        );
        drop(file);
        data = compound(&["Big"], &content);
        let file = Ole2::open(&data).unwrap();
        let id = file.child(file.root(), "Big").unwrap();
        let read = file.content(id).unwrap();
        assert_eq!(read.len(), content.len());
        assert_eq!(read.pieces().count(), 1);
        let data_range = data.as_ptr_range();
        assert!(
            matches!(read.bytes(), Cow::Borrowed(bytes) if data_range.contains(&bytes.as_ptr()))
        );
        assert_eq!(read.range(9_990, 10).unwrap().as_ref(), &content[9_990..]);
        assert_eq!(read.range(9_990, 11), None);
        let start = file.entry(id).start;
        let bat = file.bats[0] as usize;
        drop(file);
        let at = (bat + 1) * 512 + 4 * start as usize;
        data[at..at + 4].copy_from_slice(&start.to_le_bytes());
        let file = Ole2::open(&data).unwrap();
        assert_eq!(
            file.content(file.child(file.root(), "Big").unwrap())
                .map(|_| ()),
            Err(Unreadable("a chain loops"))
        );
    }

    /// A content of runs gives a range in place where it lies in one run, and copied together
    /// across runs and the zeros past the end of a file; it reads as its runs one after the
    /// other.
    #[test]
    fn gathers_ranges_across_runs() {
        let data = b"abcdefghij";
        let mut runs = Runs::new(9);
        runs.push(0, 2, 0);
        runs.push(2, 1, 0);
        runs.push(6, 4, 2);
        runs.push(0, 5, 0);
        let content = runs.into_content(data);
        assert_eq!(content.len(), 9);
        assert_eq!(
            content.pieces().collect::<Vec<_>>(),
            [&b"abc"[..], b"ghij", b"\0\0"]
        );
        assert!(matches!(content.range(3, 4), Some(Cow::Borrowed(b"ghij"))));
        assert_eq!(content.range(1, 7).unwrap().as_ref(), b"bcghij\0");
        assert_eq!(content.to_vec(), b"abcghij\0\0");
        assert_eq!(content.range(8, 2), None);
        let mut read = Vec::new();
        std::io::Read::read_to_end(&mut content.reader(), &mut read).unwrap();
        assert_eq!(read, content.to_vec());
    }

    #[test]
    fn reads_what_a_header_declares_as_poi_does() {
        let data = compound(&["One"], b"content");
        assert_eq!(
            Declared::read(&data),
            Some(Declared {
                block_size: 512,
                size: (1 + 128) * 512
            })
        );
        assert_eq!(Declared::read(&data[..511]), None);
    }
}
