//! java-libpst's `PSTFile`: the header, the lookups in the node and block B-trees, the tree of
//! nodes by parent, and the map of named properties, read as java-libpst reads them.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::fmt;

use super::node::NodeStream;
use super::table::PropertyContext;

const MAGIC: &[u8; 4] = b"!BDN";
const ANSI: u8 = 14;
const ANSI_2: u8 = 15;
const UNICODE: u8 = 23;
const UNICODE_2013: u8 = 36;
const ENCRYPTION_COMPRESSIBLE: u8 = 1;
const ENCRYPTION_HIGH: u8 = 2;
const NAME_TO_ID_MAP: i64 = 97;
const BLOCK_PAGE: u8 = 0x80;
const NODE_PAGE: u8 = 0x81;
/// How deep a B-tree lookup descends before it gives up on a tree whose pages lead in a circle,
/// where java-libpst would loop for ever.
const MAX_LEVELS: usize = 64;
/// `PSETID_Task`'s index among java-libpst's known property sets.
pub(super) const PSETID_TASK: i64 = 10;
/// The property sets java-libpst knows, by index.
const PROPERTY_SETS: [&str; 15] = [
    "00020329-0000-0000-C000-000000000046",
    "00062008-0000-0000-C000-000000000046",
    "00062004-0000-0000-C000-000000000046",
    "00020386-0000-0000-C000-000000000046",
    "00062002-0000-0000-C000-000000000046",
    "6ED8DA90-450B-101B-98DA-00AA003F1305",
    "0006200A-0000-0000-C000-000000000046",
    "41F28F13-83F4-4114-A584-EEDB5A6B0BFF",
    "0006200E-0000-0000-C000-000000000046",
    "00062041-0000-0000-C000-000000000046",
    "00062003-0000-0000-C000-000000000046",
    "4442858E-A9E3-4E80-B900-317A210CC15B",
    "00020328-0000-0000-C000-000000000046",
    "71035549-0739-4DCB-9163-00F0580DBBDF",
    "00062040-0000-0000-C000-000000000046",
];
const PS_PUBLIC_STRINGS: i64 = 0;
const PS_MAPI: i64 = 12;
/// java-libpst's `InternetCodepages.txt`: the charset named for each code page.
pub(super) const CODE_PAGES: [(i32, &str); 35] = [
    (28596, "iso-8859-6"),
    (1256, "windows-1256"),
    (28594, "iso-8859-4"),
    (1257, "windows-1257"),
    (28592, "iso-8859-2"),
    (1250, "windows-1250"),
    (936, "gb2312"),
    (52936, "hz-gb-2312"),
    (54936, "gb18030"),
    (950, "big5"),
    (28595, "iso-8859-5"),
    (20866, "koi8-r"),
    (21866, "koi8-u"),
    (1251, "windows-1251"),
    (28597, "iso-8859-7"),
    (1253, "windows-1253"),
    (38598, "iso-8859-8-i"),
    (1255, "windows-1255"),
    (51932, "euc-jp"),
    (50220, "iso-2022-jp"),
    (50221, "csISO2022JP"),
    (932, "iso-2022-jp"),
    (949, "ks_c_5601-1987"),
    (51949, "euc-kr"),
    (28593, "iso-8859-3"),
    (28605, "iso-8859-15"),
    (874, "windows-874"),
    (28599, "iso-8859-9"),
    (1254, "windows-1254"),
    (65000, "utf-7"),
    (65001, "utf-8"),
    (20127, "us-ascii"),
    (1258, "windows-1258"),
    (28591, "iso-8859-1"),
    (1252, "Windows-1252"),
];

/// The class of the exception java-libpst, or the JVM under it, throws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// `com.pff.PSTException`.
    Pst,
    /// `java.io.IOException`.
    Io,
    /// A `RuntimeException`: an index out of bounds, a null pointer, a division by zero.
    Runtime(&'static str),
}

/// An exception java-libpst throws, with its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Thrown {
    pub(super) kind: Kind,
    pub(super) message: String,
}

impl Thrown {
    pub(super) fn pst(message: impl Into<String>) -> Self {
        Self {
            kind: Kind::Pst,
            message: message.into(),
        }
    }

    pub(super) fn io(message: impl Into<String>) -> Self {
        Self {
            kind: Kind::Io,
            message: message.into(),
        }
    }

    pub(super) fn runtime(class: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind: Kind::Runtime(class),
            message: message.into(),
        }
    }

    pub(super) fn out_of_bounds(what: &str) -> Self {
        Self::runtime(
            "java.lang.ArrayIndexOutOfBoundsException",
            format!("reading {what}"),
        )
    }

    pub(super) fn null(what: &str) -> Self {
        Self::runtime("java.lang.NullPointerException", format!("{what} is null"))
    }

    pub(super) fn eof() -> Self {
        Self::io("unexpected EOF")
    }

    /// Whether `catch (PSTException | IOException e)` catches it.
    pub(super) fn is_checked(&self) -> bool {
        matches!(self.kind, Kind::Pst | Kind::Io)
    }
}

impl fmt::Display for Thrown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = match self.kind {
            Kind::Pst => "com.pff.PSTException",
            Kind::Io => "java.io.IOException",
            Kind::Runtime(class) => class,
        };
        write!(f, "{class}: {}", self.message)
    }
}

pub(super) type Read<T> = Result<T, Thrown>;

/// The PST layouts java-libpst reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Ansi,
    Unicode,
    /// Outlook 2013's offline store with 4 KiB pages, which Tika refuses.
    Unicode2013,
}

/// `DescriptorIndexNode`: an entry of the node B-tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DescriptorNode {
    pub(super) id: i32,
    pub(super) data: i64,
    pub(super) sub_nodes: i64,
    pub(super) parent: i32,
}

impl DescriptorNode {
    fn read(entry: &[u8], format: Format) -> Self {
        match format {
            Format::Ansi => Self {
                id: le(entry, 0, 4) as i32,
                data: i64::from(le(entry, 4, 8) as i32),
                sub_nodes: i64::from(le(entry, 8, 12) as i32),
                parent: le(entry, 12, 16) as i32,
            },
            Format::Unicode | Format::Unicode2013 => Self {
                id: le(entry, 0, 4) as i32,
                data: i64::from(le(entry, 8, 16) as i32),
                sub_nodes: i64::from(le(entry, 16, 24) as i32),
                parent: le(entry, 24, 28) as i32,
            },
        }
    }

    /// `getNodeType`.
    pub(super) fn node_type(self) -> i32 {
        self.id & 0x1F
    }
}

/// `OffsetIndexItem`: an entry of the block B-tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlockEntry {
    pub(super) id: i64,
    pub(super) offset: i64,
    pub(super) size: usize,
}

impl BlockEntry {
    fn read(entry: &[u8], format: Format) -> Self {
        match format {
            Format::Ansi => Self {
                id: le(entry, 0, 4),
                offset: le(entry, 4, 8),
                size: le(entry, 8, 10) as usize,
            },
            Format::Unicode | Format::Unicode2013 => Self {
                id: le(entry, 0, 8),
                offset: le(entry, 8, 16),
                size: le(entry, 16, 18) as usize,
            },
        }
    }
}

/// The `length` bytes of `data` at `offset`, as `seek` and `readCompletely` read them.
pub(super) fn slice(data: &[u8], offset: i64, length: usize) -> Read<&[u8]> {
    let start = usize::try_from(offset).map_err(|_| Thrown::io("Negative seek offset"))?;
    start
        .checked_add(length)
        .and_then(|end| data.get(start..end))
        .ok_or_else(Thrown::eof)
}

/// `convertLittleEndianBytesToLong(data, start, end)`, of a range known to be inside `data`.
pub(super) fn le(data: &[u8], start: usize, end: usize) -> i64 {
    data[start..end]
        .iter()
        .rev()
        .fold(0i64, |value, &byte| (value << 8) | i64::from(byte))
}

/// [`le`] of a range that may not be inside `data`, as Java's array access throws.
pub(super) fn le_checked(data: &[u8], start: usize, end: usize, what: &str) -> Read<i64> {
    if start > end || end > data.len() {
        return Err(Thrown::out_of_bounds(what));
    }
    Ok(le(data, start, end))
}

/// An open PST file.
pub(super) struct PstFile<'f> {
    data: &'f [u8],
    format: Format,
    encrypted: bool,
    named: HashMap<i64, i32>,
    children: OnceCell<HashMap<i32, Vec<DescriptorNode>>>,
}

impl<'f> PstFile<'f> {
    /// `new PSTFile(content)`: the header checked, and the map of named properties read.
    pub(super) fn open(data: &'f [u8]) -> Read<Self> {
        let magic = data
            .get(..4)
            .ok_or_else(|| Thrown::pst("Unable to read PST Sig: unexpected EOF"))?;
        if magic != MAGIC {
            return Err(Thrown::pst(format!(
                "Invalid file header: {}, expected: !BDN",
                crate::java::utf8(magic)
            )));
        }
        let version = *data
            .get(10)
            .ok_or_else(|| Thrown::pst("Unable to read PST Sig: unexpected EOF"))?;
        let format = match version {
            ANSI | ANSI_2 => Format::Ansi,
            UNICODE => Format::Unicode,
            UNICODE_2013 => Format::Unicode2013,
            other => {
                return Err(Thrown::pst(format!(
                    "Unrecognised PST File version: {}",
                    other as i8
                )))
            }
        };
        let encryption_at = if format == Format::Ansi { 461 } else { 513 };
        let encryption = *data
            .get(encryption_at)
            .ok_or_else(|| Thrown::pst("Unable to read PST Sig: unexpected EOF"))?;
        if encryption == ENCRYPTION_HIGH {
            return Err(Thrown::pst(
                "Only unencrypted and compressable PST files are supported at this time",
            ));
        }
        let mut file = Self {
            data,
            format,
            encrypted: encryption == ENCRYPTION_COMPRESSIBLE,
            named: HashMap::new(),
            children: OnceCell::new(),
        };
        file.named = file.name_to_id_map().map_err(|thrown| match thrown.kind {
            Kind::Io => Thrown::pst(format!("Unable to read PST Sig: {}", thrown.message)),
            _ => thrown,
        })?;
        Ok(file)
    }

    pub(super) fn format(&self) -> Format {
        self.format
    }

    pub(super) fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    pub(super) fn data(&self) -> &'f [u8] {
        self.data
    }

    /// The `length` bytes at `offset`.
    pub(super) fn bytes(&self, offset: i64, length: usize) -> Read<&'f [u8]> {
        slice(self.data, offset, length)
    }

    /// The byte at `offset`, as `read` reads it: -1 past the end.
    fn byte(&self, offset: i64) -> Read<i32> {
        let start = usize::try_from(offset).map_err(|_| Thrown::io("Negative seek offset"))?;
        Ok(self.data.get(start).map_or(-1, |&byte| i32::from(byte)))
    }

    /// `extractLEFileOffset`.
    fn file_offset(&self, at: i64) -> Read<i64> {
        let width = if self.format == Format::Ansi { 4 } else { 8 };
        Ok(le(self.bytes(at, width)?, 0, width))
    }

    /// `findBtreeItem`: the entry of `index` in the node B-tree (`descriptors`) or the block
    /// B-tree, found as java-libpst finds it.
    fn find(&self, index: i64, descriptors: bool) -> Read<&'f [u8]> {
        let ansi = self.format == Format::Ansi;
        let root_at = match (ansi, descriptors) {
            (true, false) => 196,
            (true, true) => 188,
            (false, false) => 240,
            (false, true) => 224,
        };
        let mut start = self.file_offset(root_at)?;
        let trailer = match self.format {
            Format::Ansi => 500,
            Format::Unicode2013 => 0x1000 - 24,
            Format::Unicode => 496,
        };
        let entries_length: i64 = match self.format {
            Format::Ansi => 496,
            Format::Unicode2013 => 4056,
            Format::Unicode => 488,
        };
        let page = if descriptors { NODE_PAGE } else { BLOCK_PAGE };
        let mut kind = self.bytes(start + trailer, 2)?;
        for _ in 0..MAX_LEVELS {
            if kind != [page, page] {
                break;
            }
            self.bytes(start, entries_length as usize)?;
            let mut at = start + entries_length;
            let count = if self.format == Format::Unicode2013 {
                let count = le(self.bytes(at, 2)?, 0, 2);
                self.bytes(at + 2, 2)?;
                at += 4;
                count
            } else {
                let count = i64::from(self.byte(at)?);
                at += 2;
                count
            };
            let levels = self.byte(at + 1)?;
            if levels > 0 {
                let width: i64 = if ansi { 12 } else { 24 };
                let pointer: i64 = if ansi { 8 } else { 16 };
                let mut child = None;
                for x in 0..count {
                    if self.file_offset(start + x * width)? > index {
                        child = Some(x - 1);
                        break;
                    }
                }
                let entry = child.unwrap_or(count - 1);
                start = self.file_offset(start + entry * width + pointer)?;
                kind = self.bytes(start + trailer, 2)?;
                continue;
            }
            let width: i64 = match (ansi, descriptors) {
                (true, true) => 16,
                (true, false) => 12,
                (false, true) => 32,
                (false, false) => 24,
            };
            for x in 0..count {
                let key = if descriptors {
                    le(self.bytes(start + x * width, 4)?, 0, 4)
                } else {
                    self.file_offset(start + x * width)?
                };
                if key == index {
                    return self.bytes(start + x * width, width as usize);
                }
            }
            return Err(Thrown::pst(format!(
                "Unable to find {index} is desc: {descriptors}"
            )));
        }
        Err(Thrown::pst(format!(
            "Unable to find node: {index} is desc: {descriptors}"
        )))
    }

    /// `getDescriptorIndexNode`.
    pub(super) fn descriptor(&self, id: i64) -> Read<DescriptorNode> {
        Ok(DescriptorNode::read(self.find(id, true)?, self.format))
    }

    /// `getOffsetIndexNode`.
    pub(super) fn block(&self, id: i64) -> Read<BlockEntry> {
        Ok(BlockEntry::read(self.find(id, false)?, self.format))
    }

    /// `getChildDescriptorTree`: the nodes of the node B-tree by their parent.
    pub(super) fn children(&self, parent: i32) -> Read<Vec<DescriptorNode>> {
        if let Some(children) = self.children.get() {
            return Ok(children.get(&parent).cloned().unwrap_or_default());
        }
        let root = self.file_offset(if self.format == Format::Ansi {
            188
        } else {
            224
        })?;
        let mut children = HashMap::new();
        self.collect_children(root, &mut children, 0)?;
        let found = children.get(&parent).cloned().unwrap_or_default();
        let _ = self.children.set(children);
        Ok(found)
    }

    /// `processDescriptorBTree`.
    fn collect_children(
        &self,
        start: i64,
        children: &mut HashMap<i32, Vec<DescriptorNode>>,
        depth: usize,
    ) -> Read<()> {
        if depth > MAX_LEVELS {
            return Err(Thrown::pst(
                "Unable to read descriptor node, is not a descriptor",
            ));
        }
        let ansi = self.format == Format::Ansi;
        let trailer = match self.format {
            Format::Ansi => 500,
            Format::Unicode2013 => 0x1000 - 24,
            Format::Unicode => 496,
        };
        if self.bytes(start + trailer, 2)? != [NODE_PAGE, NODE_PAGE] {
            return Err(Thrown::pst(
                "Unable to read descriptor node, is not a descriptor",
            ));
        }
        let mut at = start
            + match self.format {
                Format::Ansi => 496,
                Format::Unicode2013 => 4056,
                Format::Unicode => 488,
            };
        let count = if self.format == Format::Unicode2013 {
            let count = le(self.bytes(at, 2)?, 0, 2);
            at += 4;
            count
        } else {
            let count = i64::from(self.byte(at)?);
            at += 2;
            count
        };
        let levels = self.byte(at + 1)?;
        if levels > 0 {
            let (width, pointer): (i64, i64) = if ansi { (12, 8) } else { (24, 16) };
            for x in 0..count {
                let next = self.file_offset(start + width * x + pointer)?;
                self.collect_children(next, children, depth + 1)?;
            }
            return Ok(());
        }
        let width: i64 = if ansi { 16 } else { 32 };
        for x in 0..count {
            let node =
                DescriptorNode::read(self.bytes(start + x * width, width as usize)?, self.format);
            if node.parent != node.id {
                children.entry(node.parent).or_default().push(node);
            }
        }
        Ok(())
    }

    /// `readLeaf`: the data of a block, as a stream.
    pub(super) fn leaf(&self, id: i64) -> Read<NodeStream<'f>> {
        NodeStream::from_block(self, self.block(id)?)
    }

    /// `getPSTDescriptorItems`: the sub-nodes listed in the block `id`.
    pub(super) fn sub_nodes(&self, id: i64) -> Read<SubNodes> {
        let stream = self.leaf(id)?;
        if stream.byte(0)? != 0x2 {
            return Err(Thrown::pst(format!(
                "Unable to process descriptor node, bad signature: {}",
                stream.byte(0)?
            )));
        }
        let count = le(&stream.read(2, 2)?, 0, 2);
        let data = stream.read(0, stream.len())?;
        let (mut offset, width) = if self.format == Format::Ansi {
            (4, 12)
        } else {
            (8, 24)
        };
        let mut items = HashMap::new();
        for _ in 0..count {
            let item = SubNode::read(&data, offset, self.format)?;
            items.insert(item.id, item);
            offset += width;
        }
        Ok(SubNodes(items))
    }

    /// `getNameToIdMapItem`: the property id a named property of a known set has here, or -1.
    pub(super) fn named_property(&self, key: i32, set: i64) -> i32 {
        self.named
            .get(&((set << 32) | i64::from(key)))
            .copied()
            .unwrap_or(-1)
    }

    /// `processNameToIdMap`.
    fn name_to_id_map(&self) -> Read<HashMap<i64, i32>> {
        let node = self.descriptor(NAME_TO_ID_MAP)?;
        let local = if node.sub_nodes != 0 {
            Some(self.sub_nodes(node.sub_nodes)?)
        } else {
            None
        };
        let stream = NodeStream::from_block(self, self.block(node.data)?)?;
        let table = PropertyContext::read(self, stream)?;
        let data = |id: i32| -> Read<Vec<u8>> {
            let item = table
                .items
                .get(&id)
                .ok_or_else(|| Thrown::null("a name to id map entry"))?;
            if !item.data.is_empty() {
                return Ok(item.data.clone());
            }
            let local = local.as_ref().ok_or_else(|| {
                Thrown::pst("External reference but no localDescriptorItems in PSTFile.getData()")
            })?;
            if item.value_type != 0x0102 {
                return Err(Thrown::pst(
                    "Attempting to get non-binary data in PSTFile.getData()",
                ));
            }
            local
                .get(item.reference)
                .ok_or_else(|| Thrown::pst(format!("not here {}", item.reference)))?
                .data(self)
        };
        let guids = data(2)?;
        let sets: Vec<i64> = guids
            .as_chunks::<16>()
            .0
            .iter()
            .map(|guid| {
                let text = guid_string(guid);
                PROPERTY_SETS
                    .iter()
                    .position(|set| set.eq_ignore_ascii_case(&text))
                    .map_or(-1, |index| index as i64)
            })
            .collect();
        let entries = data(3)?;
        let strings = data(4)?;
        let mut named = HashMap::new();
        let mut x = 0;
        while x + 8 < entries.len() {
            let property = le(&entries, x, x + 4) as i32;
            let mut guid = le(&entries, x + 4, x + 6);
            let index = le(&entries, x + 6, x + 8) as i32 + 0x8000;
            if guid & 1 == 0 {
                guid >>= 1;
                let set = match guid {
                    1 => PS_MAPI,
                    2 => PS_PUBLIC_STRINGS,
                    _ => *usize::try_from(guid - 3)
                        .ok()
                        .and_then(|at| sets.get(at))
                        .ok_or_else(|| Thrown::out_of_bounds("the property sets"))?,
                };
                named.insert(i64::from(property) | (set << 32), index);
            } else {
                let at = usize::try_from(property)
                    .map_err(|_| Thrown::out_of_bounds("the names of named properties"))?;
                let length =
                    le_checked(&strings, at, at + 4, "the names of named properties")? as i32;
                let length = usize::try_from(length)
                    .map_err(|_| Thrown::runtime("java.lang.NegativeArraySizeException", ""))?;
                if at + 4 + length > strings.len() {
                    return Err(Thrown::out_of_bounds("the names of named properties"));
                }
            }
            x += 8;
        }
        Ok(named)
    }
}

/// The text `new UUID(most, least).toString()` gives of a GUID as the PST stores it.
fn guid_string(guid: &[u8]) -> String {
    let data1 = le(guid, 0, 4);
    let data2 = le(guid, 4, 6);
    let data3 = le(guid, 6, 8);
    let rest: String = guid[8..].iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{data1:08x}-{data2:04x}-{data3:04x}-{}-{}",
        &rest[..4],
        &rest[4..]
    )
}

/// `getInternetCodePageCharset`.
pub(super) fn code_page_charset(code_page: i32) -> Option<&'static str> {
    CODE_PAGES
        .iter()
        .find(|(page, _)| *page == code_page)
        .map(|(_, charset)| *charset)
}

/// `PSTDescriptorItem`: a sub-node of a node.
#[derive(Debug, Clone)]
pub(super) struct SubNode {
    pub(super) id: i32,
    pub(super) data: i64,
    pub(super) sub_nodes: i64,
}

impl SubNode {
    fn read(data: &[u8], offset: usize, format: Format) -> Read<Self> {
        let what = "a sub-node entry";
        Ok(match format {
            Format::Ansi => Self {
                id: le_checked(data, offset, offset + 4, what)? as i32,
                data: i64::from(le_checked(data, offset + 4, offset + 8, what)? as i32 & !1),
                sub_nodes: i64::from(le_checked(data, offset + 8, offset + 12, what)? as i32 & !1),
            },
            Format::Unicode | Format::Unicode2013 => Self {
                id: le_checked(data, offset, offset + 4, what)? as i32,
                data: i64::from(le_checked(data, offset + 8, offset + 16, what)? as i32 & !1),
                sub_nodes: i64::from(le_checked(data, offset + 16, offset + 24, what)? as i32 & !1),
            },
        })
    }

    /// `getData`.
    pub(super) fn data(&self, file: &PstFile<'_>) -> Read<Vec<u8>> {
        let stream = file.leaf(self.data)?;
        stream.read(0, stream.len())
    }
}

/// The sub-nodes of a node by their id.
#[derive(Debug, Clone, Default)]
pub(super) struct SubNodes(HashMap<i32, SubNode>);

impl SubNodes {
    pub(super) fn get(&self, id: i32) -> Option<&SubNode> {
        self.0.get(&id)
    }

    pub(super) fn extend(&mut self, other: SubNodes) {
        self.0.extend(other.0);
    }
}
