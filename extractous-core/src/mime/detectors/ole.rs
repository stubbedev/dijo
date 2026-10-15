//! OLE2 compound documents as POI's `POIFSFileSystem` opens a file, failing where it fails, and
//! the two detectors that name them by their entries: Tika's `POIFSContainerDetector` and
//! `MiscOLEDetector`.

use std::collections::HashSet;

use crate::java;
use crate::mime::media_type::MediaType;

const SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const END_OF_CHAIN: i32 = -2;
const UNUSED_BLOCK: i32 = -1;
const NO_INDEX: i32 = -1;
const PROPERTY_SIZE: usize = 128;
const SMALL_BLOCK_SIZE: usize = 64;
const BIG_BLOCK_MINIMUM_DOCUMENT_SIZE: i32 = 4096;
const MAX_PROPERTY_DEPTH: usize = 1000;
const MAX_BATS_IN_HEADER: i32 = 109;

/// Why POI cannot open a file, which Tika's detectors read as having no entries.
#[derive(Debug)]
struct Unreadable;

type Result<T> = std::result::Result<T, Unreadable>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Directory,
    Document,
    Root,
}

/// A directory entry of the compound document.
#[derive(Debug)]
struct Property {
    name: Vec<u16>,
    kind: Kind,
    previous: i32,
    next: i32,
    child: i32,
    start: i32,
    size: i32,
    /// The entries POI's property tree gives a directory, in its order.
    children: Vec<usize>,
    child_names: HashSet<Vec<u16>>,
}

impl Property {
    fn name(&self) -> String {
        String::from_utf16_lossy(&self.name)
    }
}

fn int_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// How POI's `DirectoryNode` finds an entry by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// `getEntry`: the entry of exactly that name.
    Exact,
    /// `getEntryCaseInsensitive`: the last entry whose name upper-cases to the name's.
    IgnoringCase,
}

/// A compound document POI opened.
pub(crate) struct OleFile<'a> {
    data: &'a [u8],
    block_size: usize,
    bats: Vec<Vec<i32>>,
    sbats: Vec<Vec<i32>>,
    properties: Vec<Option<Property>>,
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
        let slot = usize::try_from(block).map_err(|_| Unreadable)?;
        if std::mem::replace(&mut self.used[slot], true) {
            return Err(Unreadable);
        }
        Ok(())
    }
}

impl<'a> OleFile<'a> {
    /// `new POIFSFileSystem(file, true)`.
    pub(crate) fn open(data: &'a [u8]) -> Option<OleFile<'a>> {
        Self::read(data).ok()
    }

    fn read(data: &'a [u8]) -> Result<OleFile<'a>> {
        if data.is_empty() {
            return Err(Unreadable);
        }
        let mut header = [0u8; 512];
        let available = data.len().min(512);
        header[..available].copy_from_slice(&data[..available]);
        if header[..8] != SIGNATURE {
            return Err(Unreadable);
        }
        let block_size = match header[30] {
            12 => 4096,
            9 => 512,
            _ => return Err(Unreadable),
        };
        let mut file = OleFile {
            data,
            block_size,
            bats: Vec::new(),
            sbats: Vec::new(),
            properties: Vec::new(),
        };
        let bat_count = int_at(&header, 0x2C);
        let property_start = int_at(&header, 0x30);
        let sbat_start = int_at(&header, 0x3C);
        let sbat_count = int_at(&header, 0x40);
        let xbat_start = int_at(&header, 0x44);
        let xbat_count = int_at(&header, 0x48);
        let in_header = bat_count.min(MAX_BATS_IN_HEADER);
        if in_header < 0 {
            return Err(Unreadable);
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
            return Err(Unreadable);
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
        let block = usize::try_from(block).map_err(|_| Unreadable)?;
        table
            .get(block / entries)
            .map(|bat| bat[block % entries])
            .ok_or(Unreadable)
    }

    fn next_block(&self, block: i32) -> Result<i32> {
        Self::next_in(&self.bats, self.block_size / 4, block)
    }

    /// The big blocks of a chain, as `POIFSStream` iterates them.
    fn chain(&self, start: i32) -> Result<Vec<i32>> {
        if start == END_OF_CHAIN {
            return Err(Unreadable);
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
        if start == END_OF_CHAIN {
            return Err(Unreadable);
        }
        let mut detector = LoopDetector::new(self.data.len(), self.block_size);
        let mut next = start;
        for _ in 0..n {
            if next == END_OF_CHAIN {
                return Err(Unreadable);
            }
            detector.claim(next)?;
            next = self.next_block(next)?;
        }
        if next == END_OF_CHAIN {
            return Err(Unreadable);
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
                    1 => Kind::Directory,
                    2 => Kind::Document,
                    5 => Kind::Root,
                    _ => {
                        self.properties.push(None);
                        continue;
                    }
                };
                let name_length = i32::from(i16::from_le_bytes([raw[0x40], raw[0x41]])) / 2 - 1;
                let name = if name_length < 1 {
                    Vec::new()
                } else if name_length as usize * 2 > PROPERTY_SIZE {
                    return Err(Unreadable);
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
            Some(Some(root)) if root.kind == Kind::Root => self.populate(0, 0),
            _ => Err(Unreadable),
        }
    }

    fn property(&self, index: i32) -> Result<Option<usize>> {
        let index = usize::try_from(index).map_err(|_| Unreadable)?;
        match self.properties.get(index) {
            None => Err(Unreadable),
            Some(None) => Ok(None),
            Some(Some(_)) => Ok(Some(index)),
        }
    }

    /// `PropertyTable.populatePropertyTree`.
    fn populate(&mut self, directory: usize, depth: usize) -> Result<()> {
        if depth > MAX_PROPERTY_DEPTH {
            return Err(Unreadable);
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
                return Err(Unreadable);
            }
            parent.children.push(index);
            if kind != Kind::Document {
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

    fn entry(&self, index: usize) -> &Property {
        self.properties[index]
            .as_ref()
            .expect("an entry is a property")
    }

    fn children(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        self.entry(index).children.iter().copied()
    }

    /// The names of the root's entries.
    pub(crate) fn top_level_names(&self) -> Vec<String> {
        self.children(0).map(|i| self.entry(i).name()).collect()
    }

    /// `DirectoryNode.hasEntry` on the root: whether it has an entry of exactly this name.
    pub(crate) fn has_root_entry(&self, name: &str) -> bool {
        self.children(0).any(|i| self.entry(i).name() == name)
    }

    /// The content of the root's document `name`, found as `lookup` finds it, or why POI cannot
    /// read it.
    pub(crate) fn root_document(
        &self,
        name: &str,
        lookup: Lookup,
    ) -> std::result::Result<Vec<u8>, String> {
        let upper = java::to_upper_case(name);
        let entry = match lookup {
            Lookup::Exact => self.children(0).find(|&i| self.entry(i).name() == name),
            Lookup::IgnoringCase => self
                .children(0)
                .filter(|&i| java::to_upper_case(&self.entry(i).name()) == upper)
                .last(),
        }
        .ok_or_else(|| format!("the compound document has no entry {name:?}"))?;
        if self.entry(entry).kind != Kind::Document {
            return Err(format!(
                "the compound document's entry {name:?} is not a document"
            ));
        }
        self.document(entry)
            .map_err(|Unreadable| format!("the compound document's entry {name:?} is unreadable"))
    }

    /// Whether an entry named `upper` in upper case is within `max_depth` levels of `index`.
    fn find_recursively(&self, index: usize, upper: &str, depth: usize, max_depth: usize) -> bool {
        let entry = self.entry(index);
        if java::to_upper_case(&entry.name()) == upper {
            return true;
        }
        depth < max_depth
            && entry.kind != Kind::Document
            && self
                .children(index)
                .any(|child| self.find_recursively(child, upper, depth + 1, max_depth))
    }

    /// The content of a document entry, read as `DocumentInputStream` reads it.
    fn document(&self, index: usize) -> Result<Vec<u8>> {
        let entry = self.entry(index);
        let size = usize::try_from(entry.size).map_err(|_| Unreadable)?;
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut content = Vec::with_capacity(size);
        if entry.size < BIG_BLOCK_MINIMUM_DOCUMENT_SIZE {
            let root = self.entry(0);
            let root_size = usize::try_from(root.size).map_err(|_| Unreadable)?;
            let mut detector = LoopDetector::new(root_size, SMALL_BLOCK_SIZE);
            let mut next = entry.start;
            while content.len() < size {
                if next == END_OF_CHAIN {
                    return Err(Unreadable);
                }
                detector.claim(next)?;
                let byte_offset = i64::from(next) * SMALL_BLOCK_SIZE as i64;
                let big = usize::try_from(byte_offset / self.block_size as i64)
                    .map_err(|_| Unreadable)?;
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
                    return Err(Unreadable);
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

const OLE: &str = "application/x-tika-msoffice";
const WPS: &str = "application/vnd.ms-works";
const MS_GRAPH_CHART: &str = "application/vnd.ms-graph";
const COMP_OBJ_NAME: &str = "\u{1}COMPOBJ";

/// `POIFSContainerDetector.detect(names, root)` for the root's entry names, upper-cased.
fn by_names(names: &HashSet<String>, file: Option<&OleFile<'_>>) -> MediaType {
    let has = |name: &str| names.contains(name);
    let comp_obj = || file.map_or(OLE, comp_obj_type);
    let found = if names.is_empty() {
        OLE
    } else if let Some(encrypted) = encrypted(names, file) {
        encrypted
    } else if has("WORKBOOK") || has("BOOK") {
        if comp_obj() == MS_GRAPH_CHART {
            MS_GRAPH_CHART
        } else {
            "application/vnd.ms-excel"
        }
    } else if has("SWDOCCONTENTMGR") && has("SWDOCMGRTEMPSTORAGE") {
        "application/sldworks"
    } else if has("STARCALCDOCUMENT") {
        "application/vnd.stardivision.calc"
    } else if has("STARWRITERDOCUMENT") {
        "application/vnd.stardivision.writer"
    } else if has("STARDRAWDOCUMENT3") {
        comp_obj()
    } else if has("WKSSSWORKBOOK") {
        "application/x-tika-msworks-spreadsheet"
    } else if has("WORDDOCUMENT") {
        "application/msword"
    } else if has("QUILL") {
        "application/x-mspublisher"
    } else if has("POWERPOINT DOCUMENT") {
        "application/vnd.ms-powerpoint"
    } else if has("VISIODOCUMENT") {
        "application/vnd.visio"
    } else if has("\u{1}OLE10NATIVE") {
        return ole10_native();
    } else if has("MATOST") || (has("CONTENTS") && has("SPELLING")) {
        WPS
    } else if has("EQUATION NATIVE") {
        "application/vnd.ms-equation"
    } else if has("\u{3}OCXNAME") {
        OLE
    } else if has("CONTENTS") && has("\u{3}OBJINFO") {
        return comp_obj_embedded();
    } else if has("CONTENTS") && has(COMP_OBJ_NAME) {
        if file.is_some() && comp_obj() == WPS {
            WPS
        } else {
            return comp_obj_embedded();
        }
    } else if has("CONTENTS") {
        OLE
    } else if has(COMP_OBJ_NAME) && (has("PROPS") || has("PROPS9") || has("PROPS12")) {
        if names.iter().any(|name| is_project_data(name)) {
            "application/vnd.ms-project"
        } else {
            OLE
        }
    } else if has("LAYER") {
        "application/x-esri-layer"
    } else if has("DGN~MF") && has("DGN~S") && has("DGN~H") {
        "image/vnd.dgn; version=8"
    } else if names.iter().any(|name| name.starts_with("__SUBSTG1.0_")) {
        "application/vnd.ms-outlook"
    } else {
        OLE
    };
    MediaType::named(found)
}

fn ole10_native() -> MediaType {
    MediaType::named("application/x-tika-msoffice-embedded")
        .with_parameter("format", "ole10_native")
}

/// `POIFSDocumentType.detectType(root) == OLE10_NATIVE`: whether `POIFSContainerDetector` names
/// the document an OLE 1.0 object.
pub(crate) fn is_ole10_native(file: &OleFile<'_>) -> bool {
    by_names(&upper_names(Some(file)), Some(file)) == ole10_native()
}

fn comp_obj_embedded() -> MediaType {
    MediaType::named("application/x-tika-msoffice-embedded").with_parameter("format", "comp_obj")
}

/// Java's `\s\s\s\d+`, matched in full.
fn is_project_data(name: &str) -> bool {
    let mut chars = name.chars();
    (0..3).all(|_| chars.next().is_some_and(java::is_regex_space)) && {
        let rest = chars.as_str();
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
    }
}

/// `POIFSContainerDetector.checkEncrypted`.
fn encrypted(names: &HashSet<String>, file: Option<&OleFile<'_>>) -> Option<&'static str> {
    const DRM: &str = "application/x-tika-ole-drm-encrypted";
    let find = |target: &str| file.is_some_and(|f| f.find_recursively(0, target, 0, 10));
    if names.contains("\u{6}DATASPACES") && find("\tDRMDATASPACE") {
        return Some(DRM);
    }
    if names.contains("ENCRYPTEDPACKAGE") {
        if names.contains("ENCRYPTIONINFO") {
            return Some("application/x-tika-ooxml-protected");
        }
        if names.contains("\u{6}DATASPACES") && find("DRMENCRYPTEDDATASPACE") {
            return Some(DRM);
        }
    }
    None
}

/// `processCompObjFormatType`: the application the `\1CompObj` stream names, else
/// `x-tika-msoffice`.
fn comp_obj_type(file: &OleFile<'_>) -> &'static str {
    let Some(entry) = file
        .children(0)
        .find(|&i| java::to_upper_case(&file.entry(i).name()) == COMP_OBJ_NAME)
    else {
        return OLE;
    };
    if file.entry(entry).kind != Kind::Document {
        return OLE;
    }
    let Ok(content) = file.document(entry) else {
        return OLE;
    };
    let contains = |needle: &[u8]| content.windows(needle.len()).any(|w| w == needle);
    if contains(b"MSGraph.Chart") {
        MS_GRAPH_CHART
    } else if contains(b"StarDraw") {
        "application/vnd.stardivision.draw"
    } else if contains(b"StarImpress") {
        "application/vnd.stardivision.impress"
    } else if contains(b"Quill96") {
        WPS
    } else {
        OLE
    }
}

fn upper_names(file: Option<&OleFile<'_>>) -> HashSet<String> {
    file.map_or_else(HashSet::new, |f| {
        f.top_level_names()
            .iter()
            .map(|name| java::to_upper_case(name))
            .collect()
    })
}

/// Whether a document starts with the OLE2 signature.
pub(crate) fn is_ole(data: &[u8]) -> bool {
    data.starts_with(&SIGNATURE)
}

/// `POIFSContainerDetector`: `None` for a document without the OLE2 signature.
pub(crate) fn poifs(data: &[u8], file: Option<&OleFile<'_>>) -> Option<MediaType> {
    if !is_ole(data) {
        return None;
    }
    Some(by_names(&upper_names(file), file))
}

/// `MiscOLEDetector`: Hancom, Corel and Quattro Pro documents.
pub(crate) fn misc(data: &[u8], file: Option<&OleFile<'_>>) -> Option<MediaType> {
    if !is_ole(data) {
        return None;
    }
    let names: HashSet<String> = file
        .map(|f| f.top_level_names().into_iter().collect())
        .unwrap_or_default();
    let quattro = || MediaType::named("application/x-quattro-pro");
    Some(if names.contains("\u{5}HwpSummaryInformation") {
        MediaType::named("application/x-hwp-v5")
    } else if names.contains("PerfectOffice_MAIN") && names.contains("SlideShow") {
        MediaType::named("application/x-corelpresentations")
    } else if names.contains("PerfectOffice_MAIN") && names.contains("PerfectOffice_OBJECTS") {
        quattro().with_parameter("version", "7-8")
    } else if !names.contains("PerfectOffice_MAIN") && names.contains("NativeContent_MAIN") {
        quattro().with_parameter("version", "9")
    } else {
        MediaType::named(OLE)
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 512-byte-sector compound document with a root holding the named streams, each the
    /// same `content` of at most 64 bytes in the mini stream.
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
            entry(i + 1, name, 2, NO_INDEX, next, 0, content.len() as i32);
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

    fn detect(data: &[u8]) -> (String, String) {
        let file = OleFile::open(data);
        (
            poifs(data, file.as_ref()).unwrap().to_string(),
            misc(data, file.as_ref()).unwrap().to_string(),
        )
    }

    #[test]
    fn names_documents_by_their_streams() {
        assert_eq!(
            detect(&compound(&["WordDocument"], b"")).0,
            "application/msword"
        );
        assert_eq!(
            detect(&compound(&["Workbook"], b"")).0,
            "application/vnd.ms-excel"
        );
        assert_eq!(
            detect(&compound(&["EncryptionInfo", "EncryptedPackage"], b"")).0,
            "application/x-tika-ooxml-protected"
        );
        assert_eq!(
            detect(&compound(&["\u{5}HwpSummaryInformation"], b"")).1,
            "application/x-hwp-v5"
        );
        assert_eq!(
            detect(&compound(&["\u{1}Ole10Native"], b"")).0,
            "application/x-tika-msoffice-embedded; format=ole10_native"
        );
    }

    #[test]
    fn reads_the_comp_obj_stream() {
        let data = compound(&["StarDrawDocument3", "\u{1}CompObj"], b"..StarImpress..");
        assert_eq!(detect(&data).0, "application/vnd.stardivision.impress");
    }

    #[test]
    fn falls_back_to_ole_for_unreadable_files() {
        let mut data = compound(&["WordDocument"], b"");
        data.truncate(600);
        assert_eq!(detect(&data), (OLE.to_owned(), OLE.to_owned()));
        assert!(poifs(b"PK\x03\x04", None).is_none());
    }

    #[test]
    fn matches_project_names() {
        assert!(is_project_data("   112"));
        assert!(!is_project_data("   "));
        assert!(!is_project_data("  1"));
    }
}
