//! Tika's `OneNotePtr`: the header of a OneNote file and the MS-ONESTORE file node lists from
//! its root on, with what each node declares of the revision store, read as Tika reads them,
//! a byte read past the end read as -1; but each GUID read where the pointer is, and the IDs of
//! an object's streams taken by its properties in order, as MS-ONESTORE says (#122).

use std::collections::{HashMap, HashSet};

use super::file::{memory_limit, tika, Dif, Thrown};
use super::guid::{ExtendedGuid, Guid};

const FOOTER_CONST: i64 = 0x8BC2_15C3_8233_BA4B_u64 as i64;
const UNIT_MAGIC_CONSTANT: i64 = 0xA456_7AB1_F5F7_F4C4_u64 as i64;
const IFNDF_GUID_LENGTH: usize = 38;
const NUM_RESERVED_BYTES_AT_END_OF_HEADER: usize = 728;
/// `<ifndf>` in UTF-16LE: the start of a file data reference's name.
const IFNDF: [u8; 14] = [60, 0, 105, 0, 102, 0, 110, 0, 100, 0, 102, 0, 62, 0];
const PACKAGE_STORAGE_FILE_FORMAT_GUID: &str = "{638DE92F-A6D4-4BC1-9A36-B3FC2511A5B7}";
const MAX_PROPERTY_VALUES: i64 = 100_000;
const MAX_PROPERTY_SET_DEPTH: usize = 1000;
const MAX_FILE_NODE_LIST_DEPTH: usize = 100;

const OBJECT_SPACE_MANIFEST_ROOT_FND: i64 = 0x04;
const OBJECT_SPACE_MANIFEST_LIST_REFERENCE_FND: i64 = 0x08;
const OBJECT_SPACE_MANIFEST_LIST_START_FND: i64 = 0x0c;
const REVISION_MANIFEST_LIST_REFERENCE_FND: i64 = 0x10;
const REVISION_MANIFEST_LIST_START_FND: i64 = 0x14;
const REVISION_MANIFEST_START4_FND: i64 = 0x1b;
const REVISION_MANIFEST_START6_FND: i64 = 0x1e;
const REVISION_MANIFEST_START7_FND: i64 = 0x1f;
const REVISION_MANIFEST_END_FND: i64 = 0x1c;
const GLOBAL_ID_TABLE_START_FNDX: i64 = 0x21;
const GLOBAL_ID_TABLE_ENTRY_FNDX: i64 = 0x24;
const GLOBAL_ID_TABLE_ENTRY2_FNDX: i64 = 0x25;
const GLOBAL_ID_TABLE_ENTRY3_FNDX: i64 = 0x26;
const OBJECT_DECLARATION_WITH_REF_COUNT_FNDX: i64 = 0x2d;
const OBJECT_DECLARATION_WITH_REF_COUNT2_FNDX: i64 = 0x2e;
const OBJECT_REVISION_WITH_REF_COUNT_FNDX: i64 = 0x41;
const OBJECT_REVISION_WITH_REF_COUNT2_FNDX: i64 = 0x42;
const ROOT_OBJECT_REFERENCE2_FNDX: i64 = 0x59;
const ROOT_OBJECT_REFERENCE3_FND: i64 = 0x5a;
const REVISION_ROLE_DECLARATION_FND: i64 = 0x5c;
const REVISION_ROLE_AND_CONTEXT_DECLARATION_FND: i64 = 0x5d;
const OBJECT_DECLARATION_FILE_DATA3_REF_COUNT_FND: i64 = 0x72;
const OBJECT_DECLARATION_FILE_DATA3_LARGE_REF_COUNT_FND: i64 = 0x73;
const OBJECT_INFO_DEPENDENCY_OVERRIDES_FND: i64 = 0x84;
const DATA_SIGNATURE_GROUP_DEFINITION_FND: i64 = 0x8c;
const FILE_DATA_STORE_OBJECT_REFERENCE_FND: i64 = 0x94;
const OBJECT_DECLARATION2_REF_COUNT_FND: i64 = 0xa4;
const OBJECT_DECLARATION2_LARGE_REF_COUNT_FND: i64 = 0xa5;
const FILE_DATA_STORE_LIST_REFERENCE_FND: i64 = 0x90;
const OBJECT_GROUP_LIST_REFERENCE_FND: i64 = 0xb0;
const OBJECT_GROUP_START_FND: i64 = 0xb4;
const READ_ONLY_OBJECT_DECLARATION2_REF_COUNT_FND: i64 = 0xc4;
const READ_ONLY_OBJECT_DECLARATION2_LARGE_REF_COUNT_FND: i64 = 0xc5;
const CHUNK_TERMINATOR_FND: i64 = 0xff;

/// A `FileChunkReference`: where something is, and how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Chunk {
    pub(super) stp: i64,
    pub(super) cb: i64,
}

impl Chunk {
    pub(super) const NIL: Chunk = Chunk { stp: -1, cb: 0 };
    /// `new FileChunkReference()`, where a property's raw data is until it has some.
    pub(super) const EMPTY: Chunk = Chunk { stp: 0, cb: 0 };
}

/// What `OneNoteHeader` holds that the parser reads.
#[derive(Debug, Clone)]
pub(super) struct Header {
    guid_legacy_file_version: Guid,
    pub(super) alternative_packaging: bool,
    pub(super) ffv_last_code_that_wrote_to_this_file: i64,
    pub(super) ffv_oldest_code_that_has_written_to_this_file: i64,
    pub(super) ffv_newest_code_that_has_written_to_this_file: i64,
    pub(super) ffv_oldest_code_that_may_read_this_file: i64,
    pub(super) c_transactions_in_log: i64,
    pub(super) cb_legacy_expected_file_length: i64,
    pub(super) cb_expected_file_length: i64,
    pub(super) rgb_placeholder: i64,
    pub(super) cb_legacy_free_space_in_free_chunk_list: i64,
    pub(super) crc_name: i64,
    pub(super) fcr_file_node_list_root: Chunk,
    pub(super) cb_free_space_in_free_chunk_list: i64,
    pub(super) n_file_version_generation: i64,
    pub(super) grf_debug_log_flags: i64,
    pub(super) build_number_created: i64,
    pub(super) build_number_last_wrote_to_file: i64,
    pub(super) build_number_oldest_written: i64,
    pub(super) build_number_newest_written: i64,
}

impl Header {
    /// `isMsOneStoreFormat`: no legacy file version.
    pub(super) fn is_ms_one_store(&self) -> bool {
        self.guid_legacy_file_version == Guid::NIL
    }
}

/// A `FileNodeList`: its nodes, and whether a fragment's header was read for it.
#[derive(Debug, Default)]
pub(super) struct FileNodeList {
    pub(super) has_header: bool,
    pub(super) children: Vec<FileNode>,
}

/// A `FileNode`: its type, the file node list it references, and what it declares.
#[derive(Debug, Default)]
pub(super) struct FileNode {
    pub(super) id: i64,
    pub(super) child_list: FileNodeList,
    pub(super) declares: Declares,
}

impl FileNode {
    /// Whether it is a `RevisionManifestEndFND`, which ends the revision manifest it is in.
    pub(super) fn ends_revision_manifest(&self) -> bool {
        self.id == REVISION_MANIFEST_END_FND
    }
}

/// What a file node declares of the revision store: its object spaces, their revisions and the
/// objects and roots of those, and the file data the objects refer to.
#[derive(Debug, Default)]
pub(super) enum Declares {
    #[default]
    Nothing,
    /// `ObjectSpaceManifestRootFND`: the object space the section starts from.
    RootObjectSpace(ExtendedGuid),
    /// `ObjectSpaceManifestListReferenceFND`: an object space, its manifest list the node's.
    ObjectSpace(ExtendedGuid),
    /// `RevisionManifestListReferenceFND`: the revision manifest list of an object space, the
    /// node's, the last one of which is in use.
    RevisionManifestList,
    /// `RevisionManifestStart4FND`, `6FND` or `7FND`: the start of a revision, its role, and the
    /// revision it is based on, nil for none.
    Revision {
        role: RevisionRole,
        dependent: ExtendedGuid,
    },
    /// `RevisionRoleDeclarationFND` or `RevisionRoleAndContextDeclarationFND`: a role a revision
    /// declared earlier takes in a context.
    RevisionRole(RevisionRole),
    /// `RootObjectReference2FNDX` or `3FND`: the root object of a role in the revision.
    Root { object: ExtendedGuid, role: i64 },
    /// An object declaration with a property set.
    Object {
        id: ExtendedGuid,
        properties: Vec<PropertyValue>,
    },
    /// An object declaration of file data, with the file data store object its reference
    /// names, if it names one.
    FileDataObject {
        id: ExtendedGuid,
        store: Option<Guid>,
    },
    /// `FileDataStoreListReferenceFND`: the file data store objects, its list the node's.
    FileDataStoreList,
    /// `FileDataStoreObjectReferenceFND`: where the data of a file data store object is.
    FileDataStoreObject { id: Guid, data: Chunk },
}

/// A revision, its role and its context, the nil context for the default one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct RevisionRole {
    pub(super) revision: ExtendedGuid,
    pub(super) role: i64,
    pub(super) context: ExtendedGuid,
}

/// A `OneNotePropertyId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PropertyId {
    pub(super) pid: i64,
    pub(super) kind: i64,
    inline_bool: bool,
}

impl PropertyId {
    fn new(pid: i64) -> Result<Self, Thrown> {
        let kind = (pid >> 26) & 0x1f;
        let high = (pid >> 31) & 0x1 > 0;
        if kind != 0x2 && high {
            return Err(tika("Reserved non-zero"));
        }
        Ok(PropertyId {
            pid,
            kind,
            inline_bool: kind == 0x2 && high,
        })
    }
}

/// A `PropertyValue`.
#[derive(Debug)]
pub(super) struct PropertyValue {
    pub(super) id: PropertyId,
    pub(super) scalar: i64,
    pub(super) compact_ids: Vec<ExtendedGuid>,
    pub(super) property_set: Vec<PropertyValue>,
    pub(super) raw_data: Chunk,
}

impl PropertyValue {
    fn new(id: PropertyId) -> Self {
        PropertyValue {
            id,
            scalar: 0,
            compact_ids: Vec::new(),
            property_set: Vec::new(),
            raw_data: Chunk::EMPTY,
        }
    }
}

/// A `Revision`'s global ID table and the revision it depends on, the nil one until a
/// manifest names it.
#[derive(Debug, Default)]
struct Revision {
    global_id: HashMap<i64, Guid>,
    dependent: ExtendedGuid,
}

/// A `OneNoteDocument`: the root file node list, and the global ID tables of the revisions
/// read so far.
#[derive(Debug, Default)]
pub(super) struct Document {
    pub(super) root: FileNodeList,
    revisions: HashMap<ExtendedGuid, Revision>,
    current_revision: ExtendedGuid,
}

impl Document {
    fn current_revision(&mut self) -> Result<&mut Revision, Thrown> {
        self.revisions
            .get_mut(&self.current_revision)
            .ok_or_else(|| tika("Missing current revision"))
    }

    /// The global ID table of the revision the current one depends on.
    fn dependent_global_ids(&self) -> Result<&HashMap<i64, Guid>, Thrown> {
        let current = self
            .revisions
            .get(&self.current_revision)
            .ok_or_else(|| tika("Missing current revision"))?;
        self.revisions
            .get(&current.dependent)
            .map(|dependent| &dependent.global_id)
            .ok_or_else(|| tika("Missing dependent revision"))
    }

    /// The GUID of global ID `index` of the revision the current one depends on.
    fn dependent_global_id(&self, index: i64) -> Result<Guid, Thrown> {
        self.dependent_global_ids()?
            .get(&index)
            .copied()
            .ok_or_else(|| tika("COMPACT_ID_MISSING"))
    }
}

/// A `OneNotePtr`: where it reads, and where its structure ends.
#[derive(Debug, Clone, Copy)]
pub(super) struct Ptr {
    offset: i64,
    end: i64,
}

impl Ptr {
    /// `new OneNotePtr(document, dif)`: at the file's position, ending at its end.
    pub(super) fn at(dif: &Dif) -> Self {
        Ptr {
            offset: dif.position(),
            end: dif.size(),
        }
    }

    /// `reposition(FileChunkReference)`.
    pub(super) fn reposition(&mut self, dif: &mut Dif, chunk: Chunk) -> Result<(), Thrown> {
        self.offset = chunk.stp;
        dif.seek(chunk.stp)?;
        self.end = self.offset + chunk.cb;
        Ok(())
    }

    /// `size()`: what is left to its end, as an `int`.
    pub(super) fn size(&self) -> i32 {
        (self.end - self.offset) as i32
    }
}

/// The property values a file node list may hold, which every list starts afresh.
struct Budget {
    remaining: i64,
}

/// `PropertyValueBudget`'s and `checkedCount`'s limit on a count.
fn checked_count(count: i64, description: &str) -> Result<i64, Thrown> {
    if !(0..=MAX_PROPERTY_VALUES).contains(&count) {
        return Err(memory_limit(format!(
            "{description} count {count} exceeds limit {MAX_PROPERTY_VALUES}"
        )));
    }
    Ok(count)
}

/// `ObjectSpaceObjectStreamOfOIDsOSIDsOrContextIDs`, and how many of its IDs the properties
/// read so far took.
#[derive(Default)]
struct IdStream {
    extended_streams_present: i64,
    osids_stream_not_present: i64,
    data: Vec<ExtendedGuid>,
    taken: usize,
}

impl IdStream {
    /// The next `count` IDs, as MS-ONESTORE gives them to the properties in order, where Tika
    /// gives each property the stream's first IDs again (#122).
    fn take(&mut self, count: i64) -> Result<Vec<ExtendedGuid>, Thrown> {
        let ids = usize::try_from(count)
            .ok()
            .and_then(|count| self.data.get(self.taken..self.taken.checked_add(count)?))
            .ok_or_else(|| tika("SEGV"))?
            .to_vec();
        self.taken += ids.len();
        Ok(ids)
    }
}

/// `ObjectSpaceObjectPropSet`'s streams of object, object space and context IDs.
struct PropSetStreams {
    oids: IdStream,
    osids: IdStream,
    context_ids: IdStream,
}

/// A `JCID`, its flags unpacked.
#[derive(Default)]
struct Jcid {
    index: i64,
    is_binary: bool,
    is_property_set: bool,
    is_graph_node: bool,
    is_file_data: bool,
    is_read_only: bool,
}

impl Jcid {
    fn from_32bit_index(full: i64) -> Result<Self, Thrown> {
        if full >> 21 != 0 {
            return Err(tika("RESERVED_NONZERO"));
        }
        Ok(Jcid {
            index: full & 0xffff,
            is_binary: (full >> 16) & 1 == 1,
            is_property_set: (full >> 17) & 1 == 1,
            is_graph_node: (full >> 18) & 1 == 1,
            is_file_data: (full >> 19) & 1 == 1,
            is_read_only: (full >> 20) & 1 == 1,
        })
    }

    fn is_object_space_object_prop_set(&self) -> bool {
        self.is_property_set
            || !self.is_binary
                && !self.is_graph_node
                && !self.is_file_data
                && !self.is_read_only
                && self.index > 0
    }
}

/// The reader of a file's structures: the file, the document it fills, and the file node
/// lists being read, which may not nest too deep or come round again.
pub(super) struct Reader<'a, 'd> {
    pub(super) dif: Dif<'a>,
    document: &'d mut Document,
    depth: usize,
    active_lists: HashSet<i64>,
}

impl<'a, 'd> Reader<'a, 'd> {
    pub(super) fn new(dif: Dif<'a>, document: &'d mut Document) -> Self {
        Reader {
            dif,
            document,
            depth: 0,
            active_lists: HashSet::new(),
        }
    }

    fn sync(&mut self, ptr: &Ptr) -> Result<(), Thrown> {
        if self.dif.position() != ptr.offset {
            self.dif.seek(ptr.offset)?;
        }
        Ok(())
    }

    /// `deserializeLittleEndianChar`: a byte, `0xFFFF` past the end.
    fn char(&mut self, ptr: &mut Ptr) -> Result<i64, Thrown> {
        self.sync(ptr)?;
        let value = self.dif.read_byte() as u16;
        ptr.offset += 1;
        Ok(i64::from(value))
    }

    /// `deserializeLittleEndianShort`: two bytes, each 255 past the end.
    fn short(&mut self, ptr: &mut Ptr) -> Result<i64, Thrown> {
        self.sync(ptr)?;
        let low = self.dif.read_byte() & 0xff;
        let high = self.dif.read_byte() & 0xff;
        ptr.offset = self.dif.position();
        Ok(i64::from(low + (high << 8)))
    }

    fn int(&mut self, ptr: &mut Ptr) -> Result<i64, Thrown> {
        self.sync(ptr)?;
        let bytes = self.dif.read(4)?;
        ptr.offset = self.dif.position();
        Ok(i64::from(u32::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ])))
    }

    fn long(&mut self, ptr: &mut Ptr) -> Result<i64, Thrown> {
        self.sync(ptr)?;
        let bytes = self.dif.read(8)?;
        ptr.offset = self.dif.position();
        let mut array = [0; 8];
        array.copy_from_slice(bytes);
        Ok(i64::from_le_bytes(array))
    }

    fn bytes(&mut self, ptr: &mut Ptr, length: usize) -> Result<&'a [u8], Thrown> {
        self.sync(ptr)?;
        let bytes = self.dif.read(length)?;
        ptr.offset = self.dif.position();
        Ok(bytes)
    }

    /// `deserializeGUID`: sixteen bytes at the pointer, where Tika reads them from wherever the
    /// file's position is, past its end as `0xFF` bytes (#122).
    fn guid(&mut self, ptr: &mut Ptr) -> Result<Guid, Thrown> {
        let raw = self.bytes(ptr, 16)?;
        Ok(Guid([
            raw[3], raw[2], raw[1], raw[0], raw[5], raw[4], raw[7], raw[6], raw[8], raw[9],
            raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
        ]))
    }

    fn extended_guid(&mut self, ptr: &mut Ptr) -> Result<ExtendedGuid, Thrown> {
        let guid = self.guid(ptr)?;
        let n = self.int(ptr)?;
        Ok(ExtendedGuid { guid, n })
    }

    fn chunk64(&mut self, ptr: &mut Ptr) -> Result<Chunk, Thrown> {
        let stp = self.int(ptr)?;
        let cb = self.int(ptr)?;
        Ok(Chunk { stp, cb })
    }

    fn chunk64x32(&mut self, ptr: &mut Ptr) -> Result<Chunk, Thrown> {
        let stp = self.long(ptr)?;
        let cb = self.int(ptr)?;
        Ok(Chunk { stp, cb })
    }

    /// `deserializeCompactID`: an object's ID, its GUID from the current revision's global ID
    /// table.
    fn compact_id(&mut self, ptr: &mut Ptr) -> Result<ExtendedGuid, Thrown> {
        let n = self.char(ptr)?;
        let b1 = self.char(ptr)? as i32;
        let b2 = self.char(ptr)? as i32;
        let b3 = self.char(ptr)? as i32;
        let index = i64::from((((b3 << 8) | b2) << 8) | b1);
        let guid = self
            .document
            .current_revision()?
            .global_id
            .get(&index)
            .copied()
            .ok_or_else(|| tika("COMPACT ID MISSING"))?;
        Ok(ExtendedGuid { guid, n })
    }

    /// `deserializeHeader`: the header's fields in order, the legacy expected file length its
    /// own, where Tika reads it into the expected one, which the 64-bit field then replaces
    /// (#122); the reserved bytes after it unless the file is in the alternative packaging.
    pub(super) fn header(&mut self) -> Result<Header, Thrown> {
        let mut ptr = Ptr::at(&self.dif);
        let p = &mut ptr;
        self.guid(p)?;
        self.guid(p)?;
        let guid_legacy_file_version = self.guid(p)?;
        let file_format = self.guid(p)?;
        let ffv_last_code_that_wrote_to_this_file = self.int(p)?;
        let ffv_oldest_code_that_has_written_to_this_file = self.int(p)?;
        let ffv_newest_code_that_has_written_to_this_file = self.int(p)?;
        let ffv_oldest_code_that_may_read_this_file = self.int(p)?;
        self.chunk64(p)?;
        self.chunk64(p)?;
        let c_transactions_in_log = self.int(p)?;
        let cb_legacy_expected_file_length = self.int(p)?;
        let rgb_placeholder = self.long(p)?;
        self.chunk64(p)?;
        let cb_legacy_free_space_in_free_chunk_list = self.int(p)?;
        for _ in 0..4 {
            self.char(p)?;
        }
        self.guid(p)?;
        let crc_name = self.int(p)?;
        self.chunk64x32(p)?;
        self.chunk64x32(p)?;
        let fcr_file_node_list_root = self.chunk64x32(p)?;
        self.chunk64x32(p)?;
        let cb_expected_file_length = self.long(p)?;
        let cb_free_space_in_free_chunk_list = self.long(p)?;
        self.guid(p)?;
        let n_file_version_generation = self.long(p)?;
        self.guid(p)?;
        let grf_debug_log_flags = self.int(p)?;
        self.chunk64x32(p)?;
        self.chunk64x32(p)?;
        let build_number_created = self.int(p)?;
        let build_number_last_wrote_to_file = self.int(p)?;
        let build_number_oldest_written = self.int(p)?;
        let build_number_newest_written = self.int(p)?;
        let alternative_packaging = file_format.to_string() == PACKAGE_STORAGE_FILE_FORMAT_GUID;
        if !alternative_packaging {
            self.bytes(p, NUM_RESERVED_BYTES_AT_END_OF_HEADER)?;
        }
        Ok(Header {
            guid_legacy_file_version,
            alternative_packaging,
            ffv_last_code_that_wrote_to_this_file,
            ffv_oldest_code_that_has_written_to_this_file,
            ffv_newest_code_that_has_written_to_this_file,
            ffv_oldest_code_that_may_read_this_file,
            c_transactions_in_log,
            cb_legacy_expected_file_length,
            cb_expected_file_length,
            rgb_placeholder,
            cb_legacy_free_space_in_free_chunk_list,
            crc_name,
            fcr_file_node_list_root,
            cb_free_space_in_free_chunk_list,
            n_file_version_generation,
            grf_debug_log_flags,
            build_number_created,
            build_number_last_wrote_to_file,
            build_number_oldest_written,
            build_number_newest_written,
        })
    }

    /// The root file node list, from `chunk` on.
    pub(super) fn root(&mut self, chunk: Chunk) -> Result<(), Thrown> {
        let mut ptr = Ptr::at(&self.dif);
        ptr.reposition(&mut self.dif, chunk)?;
        let mut root = FileNodeList::default();
        let read = self.file_node_list(ptr, &mut root);
        self.document.root = root;
        read
    }

    /// `deserializeFileNodeList`: a list of one fragment or more, with a fresh property value
    /// budget.
    fn file_node_list(&mut self, ptr: Ptr, list: &mut FileNodeList) -> Result<(), Thrown> {
        if self.depth >= MAX_FILE_NODE_LIST_DEPTH {
            return Err(memory_limit(format!(
                "OneNote file node list nesting exceeds depth limit {MAX_FILE_NODE_LIST_DEPTH}"
            )));
        }
        let list_offset = ptr.offset;
        if !self.active_lists.insert(list_offset) {
            return Err(tika(format!(
                "OneNote file node list cycle detected at offset {list_offset}"
            )));
        }
        self.depth += 1;
        let mut budget = Budget {
            remaining: MAX_PROPERTY_VALUES,
        };
        let read = self.fragments(ptr, list, &mut budget);
        self.depth -= 1;
        self.active_lists.remove(&list_offset);
        read
    }

    fn fragments(
        &mut self,
        ptr: Ptr,
        list: &mut FileNodeList,
        budget: &mut Budget,
    ) -> Result<(), Thrown> {
        let mut seen = HashSet::from([ptr.offset]);
        let mut current = ptr;
        let local = ptr;
        loop {
            let next = self.fragment(&mut current, list, budget)?;
            if next == Chunk::NIL {
                return Ok(());
            }
            if !seen.insert(next.stp) {
                return Err(tika(format!(
                    "OneNote file node list fragment cycle detected at offset {}",
                    next.stp
                )));
            }
            current = local;
            current.reposition(&mut self.dif, next)?;
        }
    }

    /// `deserializeFileNodeListFragment`: the nodes up to a terminator or the end of the
    /// fragment, and the next fragment's reference, before the footer.
    fn fragment(
        &mut self,
        ptr: &mut Ptr,
        list: &mut FileNodeList,
        budget: &mut Budget,
    ) -> Result<Chunk, Thrown> {
        let magic = self.long(ptr)?;
        let list_id = self.int(ptr)?;
        self.int(ptr)?;
        if magic != UNIT_MAGIC_CONSTANT {
            return Err(tika(format!(
                "unitMagic must always be: 0x{:x}",
                UNIT_MAGIC_CONSTANT as u64
            )));
        }
        if list_id < 0x10 {
            return Err(tika(
                "FileNodeListHeader.fileNodeListId MUST be equal to or greater than 0x00000010",
            ));
        }
        list.has_header = true;
        while ptr.offset + 24 <= ptr.end {
            let initial = ptr.offset;
            let mut node = FileNode::default();
            self.file_node(ptr, &mut node, budget)?;
            if initial == ptr.offset {
                break;
            }
            if node.id == CHUNK_TERMINATOR_FND || node.id == 0 {
                break;
            }
            list.children.push(node);
        }
        ptr.offset = ptr.end - 20;
        self.dif.seek(ptr.offset)?;
        let next = self.chunk64x32(ptr)?;
        let footer = self.long(ptr)?;
        if footer != FOOTER_CONST {
            return Err(tika(format!(
                "Invalid footer constant. Expected {FOOTER_CONST} but was {footer}"
            )));
        }
        Ok(next)
    }

    /// `deserializeFileNode`.
    fn file_node(
        &mut self,
        ptr: &mut Ptr,
        data: &mut FileNode,
        budget: &mut Budget,
    ) -> Result<(), Thrown> {
        let backup = *ptr;
        let header = self.int(ptr)?;
        data.id = header & 0x3ff;
        if data.id == 0 {
            return Ok(());
        }
        let size = (header >> 10) & 0x1fff;
        ptr.end = backup.offset + size;
        let stp_format = (header >> 23) & 0x3;
        let cb_format = (header >> 25) & 0x3;
        let base_type = (header >> 27) & 0xf;
        let reserved = header >> 31;
        let mut reference = Chunk::NIL;
        if base_type == 1 || base_type == 2 {
            reference = self.var_chunk(ptr, stp_format, cb_format)?;
        }
        if base_type == 1 && reference != Chunk::NIL {
            let mut content = *ptr;
            content.reposition(&mut self.dif, reference)?;
        }
        self.file_node_body(ptr, data, reference, budget)?;
        if base_type == 2 {
            let mut sub_list = *ptr;
            sub_list.reposition(&mut self.dif, reference)?;
            let mut child_list = std::mem::take(&mut data.child_list);
            let read = self.file_node_list(sub_list, &mut child_list);
            data.child_list = child_list;
            read?;
        }
        ptr.offset = backup.offset + size;
        ptr.end = backup.end;
        if reserved != 1 {
            return Err(tika("RESERVED_NONZERO"));
        }
        if base_type == 1 && reference != Chunk::NIL {
            let mut content = *ptr;
            content.reposition(&mut self.dif, reference)?;
        }
        Ok(())
    }

    /// The structure of a file node of `data.id`, after its header and reference.
    fn file_node_body(
        &mut self,
        ptr: &mut Ptr,
        data: &mut FileNode,
        reference: Chunk,
        budget: &mut Budget,
    ) -> Result<(), Thrown> {
        match data.id {
            OBJECT_SPACE_MANIFEST_ROOT_FND => {
                data.declares = Declares::RootObjectSpace(self.extended_guid(ptr)?);
            }
            OBJECT_SPACE_MANIFEST_LIST_REFERENCE_FND => {
                data.declares = Declares::ObjectSpace(self.extended_guid(ptr)?);
            }
            OBJECT_GROUP_START_FND
            | OBJECT_SPACE_MANIFEST_LIST_START_FND
            | OBJECT_GROUP_LIST_REFERENCE_FND
            | DATA_SIGNATURE_GROUP_DEFINITION_FND => {
                self.extended_guid(ptr)?;
            }
            REVISION_MANIFEST_LIST_START_FND => {
                self.extended_guid(ptr)?;
                self.int(ptr)?;
            }
            REVISION_MANIFEST_START4_FND => {
                let revision = self.extended_guid(ptr)?;
                let dependent = self.extended_guid(ptr)?;
                self.long(ptr)?;
                let role = self.int(ptr)?;
                self.short(ptr)?;
                self.register_revision_manifest(revision, dependent);
                data.declares = Declares::Revision {
                    role: RevisionRole {
                        revision,
                        role,
                        context: ExtendedGuid::NIL,
                    },
                    dependent,
                };
            }
            REVISION_MANIFEST_START6_FND | REVISION_MANIFEST_START7_FND => {
                let revision = self.extended_guid(ptr)?;
                let dependent = self.extended_guid(ptr)?;
                let role = self.int(ptr)?;
                self.short(ptr)?;
                let context = if data.id == REVISION_MANIFEST_START7_FND {
                    self.extended_guid(ptr)?
                } else {
                    ExtendedGuid::NIL
                };
                self.register_revision_manifest(revision, dependent);
                data.declares = Declares::Revision {
                    role: RevisionRole {
                        revision,
                        role,
                        context,
                    },
                    dependent,
                };
            }
            GLOBAL_ID_TABLE_START_FNDX => {
                self.char(ptr)?;
            }
            GLOBAL_ID_TABLE_ENTRY_FNDX => {
                let index = self.int(ptr)?;
                let guid = self.guid(ptr)?;
                self.document
                    .current_revision()?
                    .global_id
                    .insert(index, guid);
            }
            GLOBAL_ID_TABLE_ENTRY2_FNDX => {
                let from = self.int(ptr)?;
                let to = self.int(ptr)?;
                let guid = self.document.dependent_global_id(from)?;
                self.document.current_revision()?.global_id.insert(to, guid);
            }
            GLOBAL_ID_TABLE_ENTRY3_FNDX => {
                let copy_from = self.int(ptr)?;
                let entries = self.int(ptr)?;
                let copy_to = self.int(ptr)?;
                self.document.dependent_global_ids()?;
                let entries = checked_count(entries, "global ID entries")?;
                for i in 0..entries {
                    let guid = self.document.dependent_global_id(copy_from + i)?;
                    self.document
                        .current_revision()?
                        .global_id
                        .insert(copy_to + i, guid);
                }
            }
            OBJECT_REVISION_WITH_REF_COUNT_FNDX | OBJECT_REVISION_WITH_REF_COUNT2_FNDX => {
                self.compact_id(ptr)?;
                if data.id == OBJECT_REVISION_WITH_REF_COUNT_FNDX {
                    self.char(ptr)?;
                } else {
                    let references = self.int(ptr)?;
                    if references >> 2 != 0 {
                        return Err(tika("Reserved non-zero"));
                    }
                    self.int(ptr)?;
                }
            }
            ROOT_OBJECT_REFERENCE2_FNDX | ROOT_OBJECT_REFERENCE3_FND => {
                let object = if data.id == ROOT_OBJECT_REFERENCE2_FNDX {
                    self.compact_id(ptr)?
                } else {
                    self.extended_guid(ptr)?
                };
                let role = self.int(ptr)?;
                data.declares = Declares::Root { object, role };
            }
            REVISION_ROLE_DECLARATION_FND | REVISION_ROLE_AND_CONTEXT_DECLARATION_FND => {
                let revision = self.extended_guid(ptr)?;
                let role = self.int(ptr)?;
                let context = if data.id == REVISION_ROLE_AND_CONTEXT_DECLARATION_FND {
                    self.extended_guid(ptr)?
                } else {
                    ExtendedGuid::NIL
                };
                data.declares = Declares::RevisionRole(RevisionRole {
                    revision,
                    role,
                    context,
                });
            }
            OBJECT_INFO_DEPENDENCY_OVERRIDES_FND => {
                let mut content = *ptr;
                if reference != Chunk::NIL {
                    content.reposition(&mut self.dif, reference)?;
                }
                let eight_bit = self.int(&mut content)?;
                let thirty_two_bit = self.int(&mut content)?;
                self.int(&mut content)?;
                for _ in 0..eight_bit {
                    self.char(&mut content)?;
                }
                for _ in 0..thirty_two_bit {
                    self.int(&mut content)?;
                }
            }
            FILE_DATA_STORE_LIST_REFERENCE_FND => data.declares = Declares::FileDataStoreList,
            FILE_DATA_STORE_OBJECT_REFERENCE_FND => {
                let id = self.guid(ptr)?;
                let mut store = *ptr;
                store.reposition(&mut self.dif, reference)?;
                let data_chunk = self.file_data_store_object(&mut store)?;
                data.declares = Declares::FileDataStoreObject {
                    id,
                    data: data_chunk,
                };
            }
            OBJECT_DECLARATION_WITH_REF_COUNT_FNDX
            | OBJECT_DECLARATION_WITH_REF_COUNT2_FNDX
            | OBJECT_DECLARATION2_REF_COUNT_FND
            | OBJECT_DECLARATION2_LARGE_REF_COUNT_FND
            | READ_ONLY_OBJECT_DECLARATION2_REF_COUNT_FND
            | READ_ONLY_OBJECT_DECLARATION2_LARGE_REF_COUNT_FND => {
                let (oid, jcid) = if data.id == OBJECT_DECLARATION_WITH_REF_COUNT_FNDX
                    || data.id == OBJECT_DECLARATION_WITH_REF_COUNT2_FNDX
                {
                    self.declaration_with_ref_count_body(ptr)?
                } else {
                    self.declaration2_body(ptr)?
                };
                if matches!(
                    data.id,
                    OBJECT_DECLARATION_WITH_REF_COUNT_FNDX
                        | OBJECT_DECLARATION2_REF_COUNT_FND
                        | READ_ONLY_OBJECT_DECLARATION2_REF_COUNT_FND
                ) {
                    self.char(ptr)?;
                } else {
                    self.int(ptr)?;
                }
                if matches!(
                    data.id,
                    READ_ONLY_OBJECT_DECLARATION2_REF_COUNT_FND
                        | READ_ONLY_OBJECT_DECLARATION2_LARGE_REF_COUNT_FND
                ) {
                    self.bytes(ptr, 16)?;
                }
                if jcid.is_object_space_object_prop_set() {
                    let mut prop_set = *ptr;
                    prop_set.reposition(&mut self.dif, reference)?;
                    let mut streams = self.object_space_object_prop_set(&mut prop_set)?;
                    data.declares = Declares::Object {
                        id: oid,
                        properties: self.property_set(&mut prop_set, &mut streams, 0, budget)?,
                    };
                } else if jcid.is_file_data {
                    data.declares = Declares::FileDataObject {
                        id: oid,
                        store: None,
                    };
                } else {
                    return Err(tika(
                        "JCID must be file data when !isObjectSpaceObjectPropSet.",
                    ));
                }
            }
            OBJECT_DECLARATION_FILE_DATA3_REF_COUNT_FND
            | OBJECT_DECLARATION_FILE_DATA3_LARGE_REF_COUNT_FND => {
                let oid = self.compact_id(ptr)?;
                let jcid = self.int(ptr)?;
                Jcid::from_32bit_index(jcid)?;
                if data.id == OBJECT_DECLARATION_FILE_DATA3_REF_COUNT_FND {
                    self.char(ptr)?;
                } else {
                    self.int(ptr)?;
                }
                let cch = self.int(ptr)?;
                let room_left = ptr.end - ptr.offset;
                if cch > room_left {
                    return Err(tika(format!(
                        "Data out of bounds - cch {cch} is > room left = {room_left}"
                    )));
                }
                if cch > self.dif.size() || cch > i64::from(i32::MAX / 2) {
                    return Err(memory_limit(format!(
                        "CCH={cch} cannot be safely allocated for file size {}",
                        self.dif.size()
                    )));
                }
                let name = self.dif.read(cch as usize * 2)?;
                ptr.offset += name.len() as i64;
                let store = if name.len() == IFNDF_GUID_LENGTH * 2 + IFNDF.len()
                    && name.starts_with(&IFNDF)
                {
                    Some(Guid::from_curly_brace_utf16(&name[IFNDF.len()..])?)
                } else {
                    None
                };
                data.declares = Declares::FileDataObject { id: oid, store };
            }
            REVISION_MANIFEST_LIST_REFERENCE_FND => {
                let current = self.document.current_revision;
                self.document.revisions.entry(current).or_default();
                data.declares = Declares::RevisionManifestList;
            }
            _ => {}
        }
        Ok(())
    }

    /// `registerRevisionManifest`: the revision made current, with the one it depends on.
    fn register_revision_manifest(&mut self, gosid: ExtendedGuid, dependent: ExtendedGuid) {
        self.document.revisions.entry(gosid).or_default().dependent = dependent;
        self.document.current_revision = gosid;
    }

    /// `deserializeVarFileChunkReference`.
    fn var_chunk(
        &mut self,
        ptr: &mut Ptr,
        stp_format: i64,
        cb_format: i64,
    ) -> Result<Chunk, Thrown> {
        let stp = match stp_format {
            0 => self.long(ptr)?,
            1 => self.int(ptr)?,
            2 => self.short(ptr)? << 3,
            _ => self.int(ptr)? << 3,
        };
        let cb = match cb_format {
            0 => self.int(ptr)?,
            1 => self.long(ptr)?,
            2 => self.char(ptr)? << 3,
            _ => self.short(ptr)? << 3,
        };
        Ok(Chunk { stp, cb })
    }

    fn declaration_with_ref_count_body(
        &mut self,
        ptr: &mut Ptr,
    ) -> Result<(ExtendedGuid, Jcid), Thrown> {
        let oid = self.compact_id(ptr)?;
        let fields = self.int(ptr)?;
        let reserved = self.short(ptr)?;
        let jcid = Jcid {
            index: fields & 0x3ff,
            ..Jcid::default()
        };
        let must_be_zero = (fields >> 10) & 0xf;
        let must_be_zero_a = (fields >> 14) & 0x3;
        if fields >> 18 > 0 || reserved != 0 || must_be_zero_a != 0 || must_be_zero != 0 {
            return Err(tika("RESERVED_NONZERO"));
        }
        Ok((oid, jcid))
    }

    fn declaration2_body(&mut self, ptr: &mut Ptr) -> Result<(ExtendedGuid, Jcid), Thrown> {
        let oid = self.compact_id(ptr)?;
        let jcid = self.int(ptr)?;
        let jcid = Jcid::from_32bit_index(jcid)?;
        self.char(ptr)?;
        Ok((oid, jcid))
    }

    /// `deserializeFileDataStoreObject`: where its data is, past its header.
    fn file_data_store_object(&mut self, ptr: &mut Ptr) -> Result<Chunk, Thrown> {
        self.guid(ptr)?;
        let length = self.long(ptr)?;
        let unused = self.int(ptr)?;
        let reserved = self.long(ptr)?;
        if ptr.offset.wrapping_add(length).wrapping_add(16) > ptr.end || unused > 0 || reserved > 0
        {
            return Err(tika("SEGV error"));
        }
        let data = Chunk {
            stp: ptr.offset,
            cb: length,
        };
        self.guid(ptr)?;
        Ok(data)
    }

    fn object_space_object_prop_set(&mut self, ptr: &mut Ptr) -> Result<PropSetStreams, Thrown> {
        let oids = self.id_stream(ptr)?;
        let mut streams = PropSetStreams {
            osids: IdStream {
                osids_stream_not_present: 1,
                ..IdStream::default()
            },
            context_ids: IdStream::default(),
            oids,
        };
        if streams.oids.osids_stream_not_present == 0 {
            streams.osids = self.id_stream(ptr)?;
        }
        if streams.osids.extended_streams_present != 0 {
            streams.context_ids = self.id_stream(ptr)?;
        }
        Ok(streams)
    }

    fn id_stream(&mut self, ptr: &mut Ptr) -> Result<IdStream, Thrown> {
        let header = self.int(ptr)?;
        let count = checked_count(header & 0xff_ffff, "object stream")?;
        let mut stream = IdStream {
            osids_stream_not_present: (header >> 31) & 0x1,
            extended_streams_present: (header >> 30) & 0x1,
            data: Vec::new(),
            taken: 0,
        };
        for _ in 0..count {
            stream.data.push(self.compact_id(ptr)?);
        }
        Ok(stream)
    }

    fn reserve(budget: &mut Budget, count: i64, description: &str) -> Result<i64, Thrown> {
        let checked = checked_count(count, description)?;
        if count > budget.remaining {
            return Err(memory_limit(format!(
                "{description} count {count} exceeds remaining property value budget {}",
                budget.remaining
            )));
        }
        budget.remaining -= count;
        Ok(checked)
    }

    fn property_set(
        &mut self,
        ptr: &mut Ptr,
        streams: &mut PropSetStreams,
        depth: usize,
        budget: &mut Budget,
    ) -> Result<Vec<PropertyValue>, Thrown> {
        if depth >= MAX_PROPERTY_SET_DEPTH {
            return Err(tika(format!(
                "Property set nesting exceeds limit {MAX_PROPERTY_SET_DEPTH}"
            )));
        }
        let count = self.short(ptr)?;
        let count = Self::reserve(budget, count, "property")?;
        let mut ids = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let pid = self.int(ptr)?;
            ids.push(PropertyId::new(pid)?);
        }
        ids.into_iter()
            .map(|id| self.property_value(ptr, id, streams, depth, budget))
            .collect()
    }

    fn property_value(
        &mut self,
        ptr: &mut Ptr,
        id: PropertyId,
        streams: &mut PropSetStreams,
        depth: usize,
        budget: &mut Budget,
    ) -> Result<PropertyValue, Thrown> {
        if depth >= MAX_PROPERTY_SET_DEPTH {
            return Err(tika(format!(
                "Property value nesting exceeds limit {MAX_PROPERTY_SET_DEPTH}"
            )));
        }
        let mut data = PropertyValue::new(id);
        match id.kind {
            0x1 => {}
            0x2 => data.scalar = i64::from(id.inline_bool),
            0x3 => data.scalar = self.char(ptr)?,
            0x4 => data.scalar = self.short(ptr)?,
            0x5 => data.scalar = self.int(ptr)?,
            0x6 => data.scalar = self.long(ptr)?,
            0x7 => {
                let length = self.int(ptr)?;
                data.raw_data.stp = ptr.offset;
                if ptr.offset + length > ptr.end {
                    return Err(tika("Offset is past end of file."));
                }
                data.raw_data.cb = length;
                ptr.offset += length;
            }
            0x8..=0xd => {
                let count = if matches!(id.kind, 0x9 | 0xb | 0xd) {
                    self.int(ptr)?
                } else {
                    1
                };
                let stream = match id.kind {
                    0x8 | 0x9 => &mut streams.oids,
                    0xa | 0xb => &mut streams.osids,
                    _ => &mut streams.context_ids,
                };
                data.compact_ids = stream.take(count)?;
            }
            0x10 => {
                let count = self.int(ptr)?;
                let pid = self.int(ptr)?;
                let element = PropertyId::new(pid)?;
                let count = Self::reserve(budget, count, "unified property")?;
                for _ in 0..count {
                    let value = self.property_value(ptr, element, streams, depth + 1, budget)?;
                    data.property_set.push(value);
                }
            }
            0x11 => {
                data.property_set = self.property_set(ptr, streams, depth + 1, budget)?;
            }
            kind => return Err(tika(format!("Invalid type: {kind}"))),
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_the_properties_the_ids_of_a_stream_in_order() {
        let id = |n| ExtendedGuid { guid: Guid::NIL, n };
        let mut stream = IdStream {
            data: vec![id(1), id(2), id(3)],
            ..IdStream::default()
        };
        assert_eq!(stream.take(1), Ok(vec![id(1)]));
        assert_eq!(stream.take(2), Ok(vec![id(2), id(3)]));
        assert_eq!(stream.take(0), Ok(vec![]));
        assert_eq!(stream.take(1), Err(tika("SEGV")));
        assert_eq!(stream.take(-1), Err(tika("SEGV")));
    }
}
