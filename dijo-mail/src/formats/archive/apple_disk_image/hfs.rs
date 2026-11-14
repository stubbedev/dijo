//! HFS Plus and HFSX volumes (Apple TN1150), read through the disk they are on: the volume
//! header, the catalog B-tree, whose leaf records give each folder's and file's name under its
//! parent folder, and the extents overflow B-tree, which gives the extents of a fork beyond
//! the eight its catalog record holds. An HFS volume wrapping an HFS Plus one is read for the
//! volume it wraps; an HFS volume of its own is told apart and not read, as is an APFS
//! container.
//!
//! A folder's items are found by searching the catalog for its thread record, whose key, its
//! own identifier and an empty name, comes just before the keys of its items, and read on in
//! key order from there along the leaf nodes' links, each leaf node read once. Names are
//! decoded from UTF-16, composed (HFS Plus stores them decomposed), and given a colon for each
//! slash, as macOS shows them in paths.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::ops::ControlFlow;

use unicode_normalization::UnicodeNormalization;

use super::disk::{Disk, Fault};

/// The identifier of the root folder's parent.
pub(super) const ROOT_PARENT: u32 = 1;
/// The identifier of the root folder.
pub(super) const ROOT_FOLDER: u32 = 2;
const CATALOG_FILE: u32 = 4;
/// The seconds from 1904, when HFS dates start, to 1970.
const SECONDS_TO_1970: i64 = 2_082_844_800;
const JOURNALED: u32 = 1 << 13;
const BIG_KEYS: u32 = 1 << 1;
const VARIABLE_INDEX_KEYS: u32 = 1 << 2;
const LEAF: i8 = -1;
const INDEX: i8 = 0;
const HEADER: i8 = 1;
const NODE_DESCRIPTOR: usize = 14;
const MAX_DEPTH: usize = 16;
const FOLDER_RECORD: u16 = 1;
const FILE_RECORD: u16 = 2;
const FOLDER_THREAD: u16 = 3;
const FILE_THREAD: u16 = 4;
const UF_COMPRESSED: u8 = 0x20;
const HAS_LINK_CHAIN: u16 = 0x0020;
const S_IFMT: u16 = 0o170_000;
const S_IFLNK: u16 = 0o120_000;
/// The folder of the files hard links stand for.
const PRIVATE_DATA: &str = "\0\0\0\0HFS+ Private Data";
/// The folder of the folders folder hard links stand for.
const PRIVATE_DIRECTORIES: &str = ".HFS+ Private Directory Data\r";
const JOURNAL_FILES: [&str; 2] = [".journal", ".journal_info_block"];

/// What stands at the start of a partition.
pub(super) enum Found {
    Volume(Volume),
    /// An APFS container.
    Apfs,
    /// An HFS volume wrapping no HFS Plus one.
    Hfs,
    Nothing,
}

/// A run of blocks of a volume.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Extent {
    start: u32,
    count: u32,
}

/// A fork as a catalog record or the volume header gives it: its length, its blocks and the
/// first eight of its extents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Fork {
    pub(super) size: u64,
    blocks: u32,
    extents: [Extent; 8],
}

impl Fork {
    fn parse(bytes: &[u8]) -> Fork {
        let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four"));
        Fork {
            size: u64::from_be_bytes(bytes[..8].try_into().expect("eight")),
            blocks: word(12),
            extents: extents(&bytes[16..80]),
        }
    }
}

/// The eight extents of an extent record.
fn extents(bytes: &[u8]) -> [Extent; 8] {
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four"));
    std::array::from_fn(|index| Extent {
        start: word(8 * index),
        count: word(8 * index + 4),
    })
}

/// A run of bytes of the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) offset: u64,
    pub(super) length: u64,
}

/// Where a volume's blocks are on its disk.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    start: u64,
    block_size: u64,
    blocks: u64,
}

impl Geometry {
    /// The bytes on the disk of the first `size` bytes of `extents`, adjacent runs joined.
    fn spans(&self, extents: &[Extent], size: u64) -> Result<Vec<Span>, String> {
        let mut spans: Vec<Span> = Vec::new();
        let mut left = size;
        for extent in extents {
            if left == 0 {
                break;
            }
            let (start, count) = (u64::from(extent.start), u64::from(extent.count));
            if start + count > self.blocks {
                return Err(format!(
                    "its extent of {count} blocks at block {start} runs past the volume's {} \
                     blocks",
                    self.blocks
                ));
            }
            let length = left.min(count * self.block_size);
            let offset = self.start + start * self.block_size;
            match spans.last_mut() {
                Some(last) if last.offset + last.length == offset => last.length += length,
                _ => spans.push(Span { offset, length }),
            }
            left -= length;
        }
        if left > 0 {
            return Err(format!(
                "its extents hold {} of its {size} bytes",
                size - left
            ));
        }
        Ok(spans)
    }
}

/// A B-tree of a volume, by its fork's spans on the disk.
struct Tree {
    name: &'static str,
    spans: Vec<Span>,
    node_size: usize,
    root: u32,
    nodes: u32,
    /// The bytes of each index record's key, if the tree's index keys are not of their own
    /// lengths.
    fixed_index_keys: Option<usize>,
}

/// A node of a B-tree.
struct Node<'b> {
    bytes: &'b [u8],
    kind: i8,
    next: u32,
    count: usize,
}

impl<'b> Node<'b> {
    fn parse(bytes: &'b [u8]) -> Result<Node<'b>, String> {
        let count = usize::from(u16::from_be_bytes([bytes[10], bytes[11]]));
        if NODE_DESCRIPTOR + 2 * (count + 1) > bytes.len() {
            return Err(format!("it claims {count} records, more than it holds"));
        }
        Ok(Node {
            bytes,
            kind: bytes[8] as i8,
            next: u32::from_be_bytes(bytes[..4].try_into().expect("four")),
            count,
        })
    }

    /// The offset its table gives for record `index`, `count` for its free space.
    fn offset(&self, index: usize) -> usize {
        let at = self.bytes.len() - 2 * (index + 1);
        usize::from(u16::from_be_bytes([self.bytes[at], self.bytes[at + 1]]))
    }

    /// Record `index`'s key and data, its key taking `fixed_key` bytes if given, else its own
    /// length.
    fn record(
        &self,
        index: usize,
        fixed_key: Option<usize>,
    ) -> Result<(&'b [u8], &'b [u8]), String> {
        let (start, end) = (self.offset(index), self.offset(index + 1));
        let table = self.bytes.len() - 2 * (self.count + 1);
        if start < NODE_DESCRIPTOR || start > end || end > table {
            return Err(format!(
                "its record {index} is placed at bytes {start} to {end}, outside its records"
            ));
        }
        let record = &self.bytes[start..end];
        if record.len() < 2 {
            return Err(format!("its record {index} has no key length"));
        }
        let length = usize::from(u16::from_be_bytes([record[0], record[1]]));
        let area = fixed_key.unwrap_or(length);
        if length > area || 2 + area > record.len() {
            return Err(format!(
                "its record {index}'s key of {length} bytes runs past the record"
            ));
        }
        Ok((&record[2..2 + length], &record[2 + area..]))
    }
}

/// A run of bytes of `spans` read from `disk` into `out`, from `offset` on.
fn read_spans(disk: &mut Disk, spans: &[Span], offset: u64, out: &mut [u8]) -> Result<(), Fault> {
    let mut skipped = 0;
    let mut done = 0;
    for span in spans {
        if done == out.len() {
            break;
        }
        let at = offset + done as u64;
        if at >= skipped + span.length {
            skipped += span.length;
            continue;
        }
        let within = at - skipped;
        let length = (out.len() - done).min((span.length - within) as usize);
        disk.read(span.offset + within, &mut out[done..done + length])?;
        done += length;
        skipped += span.length;
    }
    if done < out.len() {
        return Err(Fault::Corrupt(format!(
            "its bytes {offset} to {} run past its fork",
            offset + out.len() as u64
        )));
    }
    Ok(())
}

impl Tree {
    /// The B-tree `name` of the fork whose `size` bytes `spans` hold.
    fn open(
        disk: &mut Disk,
        name: &'static str,
        spans: Vec<Span>,
        size: u64,
    ) -> Result<Tree, Fault> {
        let mut sorted = spans.clone();
        sorted.sort_unstable_by_key(|span| span.offset);
        if sorted
            .windows(2)
            .any(|pair| pair[0].offset + pair[0].length > pair[1].offset)
        {
            return Err(Fault::Corrupt("its extents overlap".into()));
        }
        let mut head = [0u8; 512];
        read_spans(disk, &spans, 0, &mut head)?;
        if head[8] as i8 != HEADER {
            return Err(Fault::Corrupt("its node 0 is no header node".into()));
        }
        let half = |at: usize| u16::from_be_bytes([head[at], head[at + 1]]);
        let word = |at: usize| u32::from_be_bytes(head[at..at + 4].try_into().expect("four"));
        let (root, node_size, max_key, nodes, attributes) =
            (word(16), half(32), half(34), word(36), word(52));
        if !node_size.is_power_of_two() || node_size < 512 {
            return Err(Fault::Corrupt(format!(
                "its nodes are {node_size} bytes, not a power of two from 512 to 32768"
            )));
        }
        if u64::from(nodes) * u64::from(node_size) > size {
            return Err(Fault::Corrupt(format!(
                "its {nodes} nodes of {node_size} bytes run past its fork of {size} bytes"
            )));
        }
        if root >= nodes && root != 0 {
            return Err(Fault::Corrupt(format!(
                "its root is node {root}, of its {nodes}"
            )));
        }
        if attributes & BIG_KEYS == 0 {
            return Err(Fault::Corrupt(
                "its keys have lengths of one byte, which HFS Plus B-trees do not have".into(),
            ));
        }
        Ok(Tree {
            name,
            spans,
            node_size: usize::from(node_size),
            root,
            nodes,
            fixed_index_keys: (attributes & VARIABLE_INDEX_KEYS == 0)
                .then_some(usize::from(max_key)),
        })
    }

    /// Reads node `number` into `buffer`.
    fn node<'b>(
        &self,
        disk: &mut Disk,
        number: u32,
        buffer: &'b mut Vec<u8>,
    ) -> Result<Node<'b>, Fault> {
        let what = || format!("the node {number} of the {}", self.name);
        if number >= self.nodes {
            return Err(Fault::Corrupt(format!(
                "{} is past its {} nodes",
                what(),
                self.nodes
            )));
        }
        buffer.resize(self.node_size, 0);
        read_spans(
            disk,
            &self.spans,
            u64::from(number) * self.node_size as u64,
            buffer,
        )
        .map_err(|fault| fault.of(what()))?;
        Node::parse(buffer).map_err(|reason| Fault::Corrupt(format!("{}: {reason}", what())))
    }

    /// Where the first leaf record whose key `target` does not order before what is sought
    /// stands, as its leaf node and index there, the index past the node's records where the
    /// record is in a node after; `None` for an empty tree.
    fn seek(
        &self,
        disk: &mut Disk,
        target: impl Fn(&[u8]) -> Result<Ordering, String>,
    ) -> Result<Option<(u32, usize)>, Fault> {
        if self.root == 0 {
            return Ok(None);
        }
        let mut buffer = Vec::new();
        let mut number = self.root;
        for _ in 0..MAX_DEPTH {
            let node = self.node(disk, number, &mut buffer)?;
            let what = |reason: String| {
                Fault::Corrupt(format!("the node {number} of the {}: {reason}", self.name))
            };
            match node.kind {
                LEAF => {
                    for index in 0..node.count {
                        let (key, _) = node.record(index, None).map_err(what)?;
                        if target(key).map_err(what)? != Ordering::Less {
                            return Ok(Some((number, index)));
                        }
                    }
                    return Ok(Some((number, node.count)));
                }
                INDEX => {
                    if node.count == 0 {
                        return Err(what("it is an index node of no records".into()));
                    }
                    let mut chosen = None;
                    for index in 0..node.count {
                        let (key, data) =
                            node.record(index, self.fixed_index_keys).map_err(what)?;
                        if index > 0 && target(key).map_err(what)? == Ordering::Greater {
                            break;
                        }
                        chosen = Some(data);
                    }
                    let data = chosen.expect("an index node of records");
                    let child = data
                        .get(..4)
                        .ok_or_else(|| what("its index record points to no node".into()))?;
                    number = u32::from_be_bytes(child.try_into().expect("four"));
                }
                kind => {
                    return Err(what(format!(
                        "it is of kind {kind}, where an index or leaf node belongs"
                    )))
                }
            }
        }
        Err(Fault::Corrupt(format!(
            "the {} is deeper than {MAX_DEPTH} levels",
            self.name
        )))
    }

    /// Hands `each` the key and data of each leaf record from `from` on, along the leaf nodes'
    /// links, until it breaks or the records end; a leaf node is read at most once.
    fn scan<E: From<Fault>>(
        &self,
        disk: &mut Disk,
        from: (u32, usize),
        mut each: impl FnMut(&mut Disk, &[u8], &[u8]) -> Result<ControlFlow<()>, E>,
    ) -> Result<(), E> {
        let mut buffer = Vec::new();
        let mut read = BTreeSet::new();
        let (mut number, mut first) = from;
        loop {
            if !read.insert(number) {
                return Err(Fault::Corrupt(format!(
                    "the {}'s leaf nodes link back to node {number}",
                    self.name
                ))
                .into());
            }
            let node = self.node(disk, number, &mut buffer)?;
            let what = |reason: String| {
                Fault::Corrupt(format!("the node {number} of the {}: {reason}", self.name))
            };
            if node.kind != LEAF {
                return Err(what(format!(
                    "it is of kind {}, where a leaf node belongs",
                    node.kind
                ))
                .into());
            }
            for index in first..node.count {
                let (key, data) = node.record(index, None).map_err(what)?;
                if each(disk, key, data)?.is_break() {
                    return Ok(());
                }
            }
            if node.next == 0 {
                return Ok(());
            }
            number = node.next;
            first = 0;
        }
    }
}

/// How a file is to be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FileKind {
    /// For its data fork.
    Regular,
    /// For the path its data fork holds.
    SymbolicLink,
    /// For the file of the private data folder named for this number.
    HardLink(u32),
    /// Not: it is a hard link to a folder.
    FolderLink,
    /// Not: its data is compressed into its resource fork or extended attributes.
    Compressed,
}

/// A file of a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct File {
    pub(super) id: u32,
    pub(super) kind: FileKind,
    pub(super) fork: Fork,
}

/// What an item of a folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A folder, by its identifier.
    Folder(u32),
    File(File),
}

/// An item of a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Item {
    pub(super) name: String,
    pub(super) created: Option<i64>,
    pub(super) modified: Option<i64>,
    pub(super) kind: Kind,
}

/// A date of seconds since 1904 in UTC, in milliseconds since 1970, none for 0.
fn date(seconds: u32) -> Option<i64> {
    (seconds != 0).then(|| (i64::from(seconds) - SECONDS_TO_1970) * 1000)
}

/// The parent folder and the name, in UTF-16, of the catalog key `key`.
fn catalog_key(key: &[u8]) -> Result<(u32, &[u8]), String> {
    if key.len() < 6 {
        return Err(format!("its key of {} bytes is too short", key.len()));
    }
    let parent = u32::from_be_bytes(key[..4].try_into().expect("four"));
    let units = usize::from(u16::from_be_bytes([key[4], key[5]]));
    let name = key
        .get(6..6 + 2 * units)
        .ok_or_else(|| format!("its name of {units} characters runs past its key"))?;
    Ok((parent, name))
}

/// The name `utf16` as macOS shows it in a path.
fn name(utf16: &[u8]) -> String {
    let units = utf16
        .chunks_exact(2)
        .map(|unit| u16::from_be_bytes([unit[0], unit[1]]));
    char::decode_utf16(units)
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .nfc()
        .map(|character| if character == '/' { ':' } else { character })
        .collect()
}

/// The item the catalog leaf record `data` of the name `utf16` stands for; none for a thread
/// record.
fn item(utf16: &[u8], data: &[u8]) -> Result<Option<Item>, String> {
    let half = |at: usize| u16::from_be_bytes([data[at], data[at + 1]]);
    let word = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().expect("four"));
    let kind = data
        .get(..2)
        .map(|_| half(0))
        .ok_or("its record is empty")?;
    let needed = match kind {
        FOLDER_RECORD => 88,
        FILE_RECORD => 248,
        FOLDER_THREAD | FILE_THREAD => return Ok(None),
        other => {
            return Err(format!(
                "its record is of type {other}, which HFS Plus does not define"
            ))
        }
    };
    if data.len() < needed {
        return Err(format!(
            "its record of type {kind} is {} bytes, shorter than its {needed}",
            data.len()
        ));
    }
    let kind = if kind == FOLDER_RECORD {
        Kind::Folder(word(8))
    } else {
        let flags = half(2);
        let (owner_flags, mode, special) = (data[41], half(42), word(44));
        let (file_type, creator) = (&data[48..52], &data[52..56]);
        let kind = if file_type == b"hlnk" && creator == b"hfs+" {
            FileKind::HardLink(special)
        } else if file_type == b"fdrp" && creator == b"MACS" && flags & HAS_LINK_CHAIN != 0 {
            FileKind::FolderLink
        } else if mode & S_IFMT == S_IFLNK {
            FileKind::SymbolicLink
        } else if owner_flags & UF_COMPRESSED != 0 {
            FileKind::Compressed
        } else {
            FileKind::Regular
        };
        Kind::File(File {
            id: word(8),
            kind,
            fork: Fork::parse(&data[88..168]),
        })
    };
    Ok(Some(Item {
        name: name(utf16),
        created: date(word(12)),
        modified: date(word(16)),
        kind,
    }))
}

/// An HFS Plus or HFSX volume.
pub(super) struct Volume {
    geometry: Geometry,
    journaled: bool,
    /// When the volume was last modified.
    pub(super) modified: Option<i64>,
    extents: Option<Tree>,
    catalog: Tree,
}

/// What stands at the byte `start` of `disk`: an HFS Plus volume, opened, or else what.
pub(super) fn probe(disk: &mut Disk, start: u64) -> Result<Found, Fault> {
    let mut header = [0u8; 512];
    disk.read(start + 1024, &mut header)?;
    let signature = [header[0], header[1]];
    match &signature {
        b"H+" | b"HX" => Volume::open(disk, start, &header).map(Found::Volume),
        b"BD" if &header[0x7C..0x7E] == b"H+" => {
            let block = u64::from(u32::from_be_bytes(
                header[0x14..0x18].try_into().expect("4"),
            ));
            let first = u64::from(u16::from_be_bytes([header[0x1C], header[0x1D]]));
            let embedded = u64::from(u16::from_be_bytes([header[0x7E], header[0x7F]]));
            let start = start + first * 512 + embedded * block;
            disk.read(start + 1024, &mut header)?;
            if !matches!(&header[..2], b"H+" | b"HX") {
                return Err(Fault::Corrupt(format!(
                    "the HFS volume wraps no HFS Plus volume at byte {start}"
                )));
            }
            Volume::open(disk, start, &header).map(Found::Volume)
        }
        b"BD" => Ok(Found::Hfs),
        _ => {
            let mut superblock = [0u8; 64];
            disk.read(start, &mut superblock)?;
            Ok(if &superblock[32..36] == b"NXSB" {
                Found::Apfs
            } else {
                Found::Nothing
            })
        }
    }
}

impl Volume {
    /// The volume at byte `start` of `disk`, whose header is `header`.
    fn open(disk: &mut Disk, start: u64, header: &[u8; 512]) -> Result<Volume, Fault> {
        let word = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().expect("four"));
        let version = u16::from_be_bytes([header[2], header[3]]);
        let expected = if &header[..2] == b"H+" { 4 } else { 5 };
        if version != expected {
            return Err(Fault::Corrupt(format!(
                "its volume header is of version {version}, not {expected}"
            )));
        }
        let block_size = u64::from(word(40));
        if !block_size.is_power_of_two() || !(512..=1 << 30).contains(&block_size) {
            return Err(Fault::Corrupt(format!(
                "its blocks are {block_size} bytes, not a power of two from 512 on"
            )));
        }
        let geometry = Geometry {
            start,
            block_size,
            blocks: u64::from(word(44)),
        };
        let extents_fork = Fork::parse(&header[192..272]);
        let catalog_fork = Fork::parse(&header[272..352]);
        let extents = if extents_fork.size == 0 {
            None
        } else {
            let spans = geometry
                .spans(&extents_fork.extents, extents_fork.size)
                .map_err(|reason| Fault::Corrupt(format!("its extents overflow file: {reason}")))?;
            Some(
                Tree::open(disk, "extents overflow file", spans, extents_fork.size)
                    .map_err(|fault| fault.of("its extents overflow file"))?,
            )
        };
        let spans = fork_spans(
            &geometry,
            extents.as_ref(),
            disk,
            CATALOG_FILE,
            &catalog_fork,
        )
        .map_err(|fault| fault.of("its catalog file"))?;
        let catalog = Tree::open(disk, "catalog", spans, catalog_fork.size)
            .map_err(|fault| fault.of("its catalog file"))?;
        Ok(Volume {
            geometry,
            journaled: word(4) & JOURNALED != 0,
            modified: date(word(20)),
            extents,
            catalog,
        })
    }

    /// Hands `each` the items of the folder `folder`, in the catalog's order of their names.
    pub(super) fn list<E: From<Fault>>(
        &self,
        disk: &mut Disk,
        folder: u32,
        mut each: impl FnMut(&mut Disk, Item) -> Result<(), E>,
    ) -> Result<(), E> {
        let from = self.catalog.seek(disk, |key| {
            let (parent, name) = catalog_key(key)?;
            Ok(parent.cmp(&folder).then(if name.is_empty() {
                Ordering::Equal
            } else {
                Ordering::Greater
            }))
        })?;
        let Some(from) = from else {
            return Ok(());
        };
        self.catalog.scan(disk, from, |disk, key, data| {
            let corrupt = |reason: String| {
                Fault::Corrupt(format!("a catalog record of the folder {folder}: {reason}"))
            };
            let (parent, utf16) = catalog_key(key).map_err(corrupt)?;
            if parent != folder {
                return Ok(ControlFlow::Break(()));
            }
            if let Some(item) = item(utf16, data).map_err(corrupt)? {
                each(disk, item)?;
            }
            Ok(ControlFlow::Continue(()))
        })
    }

    /// The root folder, as its parent lists it.
    pub(super) fn root(&self, disk: &mut Disk) -> Result<Option<Item>, Fault> {
        let mut root = None;
        self.list::<Fault>(disk, ROOT_PARENT, |_, item| {
            if item.kind == Kind::Folder(ROOT_FOLDER) {
                root = Some(item);
            }
            Ok(())
        })?;
        Ok(root)
    }

    /// Whether the item `name` of the folder `parent` is the file system's own, not a file or
    /// folder of the volume's: the private folders of hard links and the journal's files.
    pub(super) fn is_own(&self, parent: u32, name: &str) -> bool {
        parent == ROOT_FOLDER
            && (name == PRIVATE_DATA
                || name == PRIVATE_DIRECTORIES
                || (self.journaled && JOURNAL_FILES.contains(&name)))
    }

    /// The files of the private data folder that hard links stand for, by their numbers.
    pub(super) fn link_targets(
        &self,
        disk: &mut Disk,
    ) -> Result<std::collections::BTreeMap<u32, Item>, Fault> {
        let mut private = None;
        self.list::<Fault>(disk, ROOT_FOLDER, |_, item| {
            if let (PRIVATE_DATA, Kind::Folder(id)) = (item.name.as_str(), item.kind) {
                private = Some(id);
            }
            Ok(())
        })?;
        let mut targets = std::collections::BTreeMap::new();
        let Some(private) = private else {
            return Ok(targets);
        };
        self.list::<Fault>(disk, private, |_, item| {
            let number = item
                .name
                .strip_prefix("iNode")
                .and_then(|number| number.parse().ok());
            if let (Some(number), Kind::File(_)) = (number, item.kind) {
                targets.insert(number, item);
            }
            Ok(())
        })?;
        Ok(targets)
    }

    /// Where the data fork `fork` of the file `id` is on the disk.
    pub(super) fn spans(&self, disk: &mut Disk, id: u32, fork: &Fork) -> Result<Vec<Span>, Fault> {
        fork_spans(&self.geometry, self.extents.as_ref(), disk, id, fork)
    }
}

/// Where the data fork `fork` of the file `id` of a volume of `geometry` is on its disk, its
/// extents beyond the first eight found in `overflow`.
fn fork_spans(
    geometry: &Geometry,
    overflow: Option<&Tree>,
    disk: &mut Disk,
    id: u32,
    fork: &Fork,
) -> Result<Vec<Span>, Fault> {
    let needed = fork.size.div_ceil(geometry.block_size);
    if needed > u64::from(fork.blocks) {
        return Err(Fault::Corrupt(format!(
            "its {} bytes need more than its {} blocks",
            fork.size, fork.blocks
        )));
    }
    let mut extents: Vec<Extent> = fork
        .extents
        .iter()
        .copied()
        .take_while(|extent| extent.count > 0)
        .collect();
    let mut covered: u64 = extents.iter().map(|extent| u64::from(extent.count)).sum();
    while covered < needed {
        let tree = overflow.ok_or_else(|| {
            Fault::Corrupt(format!(
                "its extents hold {covered} of its {needed} blocks, and the volume has no \
                 extents overflow file"
            ))
        })?;
        let more = overflow_extents(tree, disk, id, covered as u32)?;
        let added: u64 = more
            .iter()
            .take_while(|extent| extent.count > 0)
            .map(|extent| u64::from(extent.count))
            .sum();
        if added == 0 {
            return Err(Fault::Corrupt(format!(
                "its overflow extents from block {covered} hold no blocks"
            )));
        }
        extents.extend(more.iter().copied().take_while(|extent| extent.count > 0));
        covered += added;
    }
    geometry.spans(&extents, fork.size).map_err(Fault::Corrupt)
}

/// The extent record of the data fork of the file `id` that starts at its block `start`.
fn overflow_extents(
    tree: &Tree,
    disk: &mut Disk,
    id: u32,
    start: u32,
) -> Result<[Extent; 8], Fault> {
    let parse = |key: &[u8]| -> Result<(u32, u8, u32), String> {
        if key.len() < 10 {
            return Err(format!("its key of {} bytes is too short", key.len()));
        }
        let word = |at: usize| u32::from_be_bytes(key[at..at + 4].try_into().expect("four"));
        Ok((word(2), key[0], word(6)))
    };
    let sought = (id, 0u8, start);
    let missing = || {
        Fault::Corrupt(format!(
            "the extents overflow file holds no extents of file {id} from block {start}"
        ))
    };
    let from = tree
        .seek(disk, |key| Ok(parse(key)?.cmp(&sought)))?
        .ok_or_else(missing)?;
    let mut found = None;
    tree.scan::<Fault>(disk, from, |_, key, data| {
        let key = parse(key).map_err(Fault::Corrupt)?;
        if key == sought && data.len() >= 64 {
            found = Some(extents(&data[..64]));
        }
        Ok(ControlFlow::Break(()))
    })?;
    found.ok_or_else(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_names_and_shows_slashes_as_colons() {
        let utf16: Vec<u8> = "Cafe\u{301} a/b"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect();
        assert_eq!(name(&utf16), "Caf\u{e9} a:b");
        assert_eq!(name(&[0xD8, 0x00, 0x00, 0x41]), "\u{FFFD}A");
    }

    #[test]
    fn dates_from_1904() {
        assert_eq!(date(0), None);
        assert_eq!(date(2_082_844_800), Some(0));
        assert_eq!(date(3_870_000_000), Some(1_787_155_200_000));
    }

    #[test]
    fn joins_adjacent_extents_and_bounds_them_by_the_volume() {
        let geometry = Geometry {
            start: 4096,
            block_size: 512,
            blocks: 100,
        };
        let extents = [
            Extent {
                start: 10,
                count: 2,
            },
            Extent {
                start: 12,
                count: 1,
            },
            Extent {
                start: 50,
                count: 4,
            },
        ];
        assert_eq!(
            geometry.spans(&extents, 1600),
            Ok(vec![
                Span {
                    offset: 4096 + 5120,
                    length: 1536,
                },
                Span {
                    offset: 4096 + 25_600,
                    length: 64,
                },
            ])
        );
        assert!(geometry
            .spans(&extents, 4000)
            .unwrap_err()
            .contains("hold 3584 of its 4000 bytes"));
        assert!(geometry
            .spans(
                &[Extent {
                    start: 99,
                    count: 2
                }],
                1
            )
            .unwrap_err()
            .contains("runs past the volume's 100 blocks"));
    }

    #[test]
    fn reads_the_records_of_a_node_within_its_table() {
        let mut node = vec![0u8; 512];
        node[8] = LEAF as u8;
        node[10..12].copy_from_slice(&2u16.to_be_bytes());
        let records: [&[u8]; 2] = [&[0, 2, 0xAA, 0xBB, 1, 2, 3], &[0, 0, 9]];
        let mut at = NODE_DESCRIPTOR;
        for (index, record) in records.iter().enumerate() {
            node[at..at + record.len()].copy_from_slice(record);
            node[510 - 2 * index..512 - 2 * index].copy_from_slice(&(at as u16).to_be_bytes());
            at += record.len();
        }
        node[506..508].copy_from_slice(&(at as u16).to_be_bytes());
        let parsed = Node::parse(&node).unwrap();
        assert_eq!(
            parsed.record(0, None),
            Ok((&[0xAA, 0xBB][..], &[1, 2, 3][..]))
        );
        assert_eq!(parsed.record(1, None), Ok((&[][..], &[9][..])));
        assert_eq!(parsed.record(0, Some(4)), Ok((&[0xAA, 0xBB][..], &[3][..])));
        assert!(parsed.record(0, Some(9)).unwrap_err().contains("runs past"));
        node[506..508].copy_from_slice(&600u16.to_be_bytes());
        assert!(Node::parse(&node)
            .unwrap()
            .record(1, None)
            .unwrap_err()
            .contains("outside its records"));
        node[10..12].copy_from_slice(&300u16.to_be_bytes());
        assert!(Node::parse(&node).is_err());
    }
}
