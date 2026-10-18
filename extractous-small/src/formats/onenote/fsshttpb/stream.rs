//! The stream objects of MS-FSSHTTPB as the protocol suite's `StreamObject` reads them: a 16-bit
//! or 32-bit header of the object's type and length, its items, and for a compound object the
//! objects in it and an end header of its type; and the alternative packaging, whose data
//! element package holds the data elements of the revision store.

use super::{
    compact, out_of_bounds, read_guid, thrown, uint32, wrapping, Bits, CellId, Cursor, ExGuid,
    Failure, DATA_ELEMENT_PARSE_ERROR, STREAM_OBJECT_PARSE_ERROR, TIKA_EXCEPTION,
};
use crate::formats::onenote::file::Thrown;

/// The values of `StreamObjectTypeHeaderStart`.
mod kind {
    pub(super) const DATA_ELEMENT: u16 = 0x01;
    pub(super) const OBJECT_DATA_BLOB: u16 = 0x02;
    pub(super) const OBJECT_GROUP_OBJECT_BLOB_DATA_DECLARATION: u16 = 0x05;
    pub(super) const DATA_ELEMENT_HASH: u16 = 0x06;
    pub(super) const STORAGE_MANIFEST_ROOT_DECLARE: u16 = 0x07;
    pub(super) const REVISION_MANIFEST_ROOT_DECLARE: u16 = 0x0A;
    pub(super) const CELL_MANIFEST_CURRENT_REVISION: u16 = 0x0B;
    pub(super) const STORAGE_MANIFEST_SCHEMA_GUID: u16 = 0x0C;
    pub(super) const STORAGE_INDEX_REVISION_MAPPING: u16 = 0x0D;
    pub(super) const STORAGE_INDEX_CELL_MAPPING: u16 = 0x0E;
    pub(super) const STORAGE_INDEX_MANIFEST_MAPPING: u16 = 0x11;
    pub(super) const DATA_ELEMENT_PACKAGE: u16 = 0x15;
    pub(super) const OBJECT_GROUP_OBJECT_DATA: u16 = 0x16;
    pub(super) const OBJECT_GROUP_OBJECT_DECLARE: u16 = 0x18;
    pub(super) const REVISION_MANIFEST_OBJECT_GROUP_REFERENCES: u16 = 0x19;
    pub(super) const REVISION_MANIFEST: u16 = 0x1A;
    pub(super) const OBJECT_GROUP_OBJECT_DATA_BLOB_REFERENCE: u16 = 0x1C;
    pub(super) const OBJECT_GROUP_DECLARATIONS: u16 = 0x1D;
    pub(super) const OBJECT_GROUP_DATA: u16 = 0x1E;
    pub(super) const LEAF_NODE_OBJECT: u16 = 0x1F;
    pub(super) const INTERMEDIATE_NODE_OBJECT: u16 = 0x20;
    pub(super) const SIGNATURE_OBJECT: u16 = 0x21;
    pub(super) const DATA_SIZE_OBJECT: u16 = 0x22;
    pub(super) const DATA_HASH_OBJECT: u16 = 0x2F;
    pub(super) const OBJECT_GROUP_METADATA: u16 = 0x78;
    pub(super) const OBJECT_GROUP_METADATA_DECLARATIONS: u16 = 0x79;

    /// Every value the enum defines, which a header must name.
    pub(super) const DEFINED: [u16; 82] = [
        0x4E, 0x01, 0x02, 0x04, 0x05, 0x06, 0x07, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11,
        0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x2F,
        0x29, 0x2D, 0x2E, 0x30, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x49, 0x4B, 0x4D,
        0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x57, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5F, 0x60,
        0x62, 0x65, 0x66, 0x68, 0x6A, 0x6B, 0x6C, 0x79, 0x78, 0x7A, 0x80, 0x81, 0x88, 0x83, 0x87,
        0x89, 0x8A, 0x86, 0x85, 0x8C, 0x8D, 0x8E,
    ];

    /// `StreamObject.compoundTypes`: the types that hold objects and end with an end header.
    pub(super) const COMPOUND: [u16; 22] = [
        0x01, 0x10, 0x14, 0x15, 0x1D, 0x1E, 0x29, 0x2D, 0x40, 0x41, 0x42, 0x43, 0x44, 0x46, 0x47,
        0x4D, 0x5D, 0x6B, 0x79, 0x1F, 0x20, 0x83,
    ];

    /// `StreamObject.streamObjectTypeMapping`: the types the suite has a class of, which a
    /// header must name to be read.
    pub(super) const MAPPED: [u16; 26] = [
        DATA_ELEMENT,
        OBJECT_DATA_BLOB,
        OBJECT_GROUP_OBJECT_BLOB_DATA_DECLARATION,
        DATA_ELEMENT_HASH,
        STORAGE_MANIFEST_ROOT_DECLARE,
        REVISION_MANIFEST_ROOT_DECLARE,
        CELL_MANIFEST_CURRENT_REVISION,
        STORAGE_MANIFEST_SCHEMA_GUID,
        STORAGE_INDEX_REVISION_MAPPING,
        STORAGE_INDEX_CELL_MAPPING,
        STORAGE_INDEX_MANIFEST_MAPPING,
        DATA_ELEMENT_PACKAGE,
        OBJECT_GROUP_OBJECT_DATA,
        OBJECT_GROUP_OBJECT_DECLARE,
        REVISION_MANIFEST_OBJECT_GROUP_REFERENCES,
        REVISION_MANIFEST,
        OBJECT_GROUP_OBJECT_DATA_BLOB_REFERENCE,
        OBJECT_GROUP_DECLARATIONS,
        OBJECT_GROUP_DATA,
        LEAF_NODE_OBJECT,
        INTERMEDIATE_NODE_OBJECT,
        SIGNATURE_OBJECT,
        DATA_SIZE_OBJECT,
        DATA_HASH_OBJECT,
        OBJECT_GROUP_METADATA,
        OBJECT_GROUP_METADATA_DECLARATIONS,
    ];

    /// The values of `StreamObjectTypeHeaderEnd`.
    pub(super) const ENDS: [u16; 26] = [
        0x01, 0x10, 0x14, 0x15, 0x1D, 0x1E, 0x1F, 0x20, 0x29, 0x2D, 0x40, 0x41, 0x42, 0x43, 0x44,
        0x46, 0x47, 0x4D, 0x51, 0x5D, 0x62, 0x6B, 0x79, 0x7A, 0x83, 0x8B,
    ];
}

/// A stream object header start: the object's type, the length of its items, and the bytes
/// the header takes.
#[derive(Debug, Clone, Copy)]
struct Header {
    kind: u16,
    length: usize,
    size: usize,
}

/// `StreamObjectHeaderStart.tryParse`: the header at `index`, or none where its first byte names
/// no header or reading it throws. Reading the first byte past the end throws.
fn try_header(data: &[u8], index: usize) -> Result<Option<Header>, Failure> {
    let first = *data
        .get(index)
        .ok_or_else(|| out_of_bounds(index, data.len()))?;
    let parsed = match first & 0x03 {
        0x0 => header_16(data, index),
        0x2 => header_32(data, index),
        _ => return Ok(None),
    };
    Ok(parsed.ok().flatten())
}

/// A 16-bit header start, or none where its fields are not valid.
fn header_16(data: &[u8], index: usize) -> Result<Option<Header>, Thrown> {
    let mut bits = Bits::new(data, index);
    bits.int(2)?;
    let compound = bits.int(1)?;
    let kind = bits.int(6)? as u16;
    if !kind::DEFINED.contains(&kind) || (kind::COMPOUND.contains(&kind) && compound != 1) {
        return Ok(None);
    }
    let length = bits.int(7)? as usize;
    Ok(Some(Header {
        kind,
        length,
        size: 2,
    }))
}

/// A 32-bit header start, its length after it as a compact number where the field holds
/// 32767, or none where its fields are not valid.
fn header_32(data: &[u8], index: usize) -> Result<Option<Header>, Thrown> {
    let mut bits = Bits::new(data, index);
    bits.int(2)?;
    let compound = bits.int(1)?;
    let kind = bits.int(14)? as u16;
    if !kind::DEFINED.contains(&kind) || (kind::COMPOUND.contains(&kind) && compound != 1) {
        return Ok(None);
    }
    let length = bits.int(15)?;
    if length != 32767 {
        return Ok(Some(Header {
            kind,
            length: length as usize,
            size: 4,
        }));
    }
    let (large, consumed) = compact(data, index + 4)?;
    Ok(Some(Header {
        kind,
        length: large as i32 as usize,
        size: 4 + consumed,
    }))
}

/// A data element of the package, by its ID, with what it holds.
#[derive(Debug)]
pub(super) struct DataElement {
    pub(super) id: ExGuid,
    pub(super) data: ElementData,
}

/// What a data element holds, by its type.
#[derive(Debug)]
pub(super) enum ElementData {
    StorageIndex(StorageIndex),
    /// The cells its root declarations name, in order.
    StorageManifest(Vec<CellId>),
    /// Its current revision.
    CellManifest(ExGuid),
    RevisionManifest(RevisionManifest),
    ObjectGroup(ObjectGroup),
    /// The data of its BLOB.
    ObjectDataBlob(Vec<u8>),
}

/// `StorageIndexDataElementData`.
#[derive(Debug, Default)]
pub(super) struct StorageIndex {
    /// Each cell and the cell manifest it maps to.
    pub(super) cells: Vec<(CellId, ExGuid)>,
    /// Each revision and the revision manifest it maps to.
    pub(super) revisions: Vec<(ExGuid, ExGuid)>,
}

/// `RevisionManifestDataElementData`.
#[derive(Debug)]
pub(super) struct RevisionManifest {
    pub(super) revision: ExGuid,
    pub(super) base: ExGuid,
    /// Each root declared, and its object.
    pub(super) roots: Vec<(ExGuid, ExGuid)>,
    /// The object groups it references.
    pub(super) object_groups: Vec<ExGuid>,
}

/// `ObjectGroupDataElementData`.
#[derive(Debug, Default)]
pub(super) struct ObjectGroup {
    pub(super) declarations: Vec<Declaration>,
    pub(super) blob_declarations: Vec<Declaration>,
    pub(super) objects: Vec<ObjectData>,
    /// The BLOB each BLOB declaration's object references.
    pub(super) blob_references: Vec<ExGuid>,
}

/// An object declaration: its ID and partition.
#[derive(Debug, Clone, Copy)]
pub(super) struct Declaration {
    pub(super) id: ExGuid,
    pub(super) partition: u64,
}

/// `ObjectGroupObjectData`: the objects and object spaces an object references, and its data.
#[derive(Debug)]
pub(super) struct ObjectData {
    pub(super) objects: Vec<ExGuid>,
    pub(super) spaces: Vec<CellId>,
    pub(super) data: Vec<u8>,
}

/// The stream object at a cursor, read once its header is known.
struct Reader<'a> {
    data: &'a [u8],
}

/// A `StreamObjectParseErrorException` with a message.
fn stream_error(message: impl std::fmt::Display) -> Failure {
    thrown(STREAM_OBJECT_PARSE_ERROR, message).into()
}

/// The error of an object whose items are not the length its header gives.
fn over_parse() -> Failure {
    stream_error("Stream object over-parse error")
}

/// A `DataElementParseErrorException` with a message.
fn element_error(message: impl std::fmt::Display) -> Failure {
    thrown(DATA_ELEMENT_PARSE_ERROR, message).into()
}

/// A `DataElementParseErrorException` wrapping an exception thrown reading a data element.
fn wrapped_in_element_error(failure: Failure) -> Failure {
    match failure {
        Failure::Thrown(inner) => wrapping(DATA_ELEMENT_PARSE_ERROR, inner).into(),
        unported => unported,
    }
}

impl<'a> Reader<'a> {
    /// `StreamObject.getCurrent`: the object of type `expected` at `index`, which moves past it.
    fn current<T>(
        &self,
        index: &mut usize,
        expected: u16,
        name: &str,
        read: impl FnOnce(&Self, &mut usize, usize) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let Some(header) = try_header(self.data, *index)? else {
            return Err(stream_error(
                "Failed to extract either 16bit or 32bit stream object header in the current \
                 index.",
            ));
        };
        if header.kind != expected {
            if kind::MAPPED.contains(&header.kind) {
                return Err(Failure::Unported(format!(
                    "a stream object of type {:#x} where the suite expects {name}, which it \
                     reads before it fails",
                    header.kind
                )));
            }
            return Err(stream_error(format_args!(
                "Failed to create the specified stream object instance, the type {} of stream \
                 object header in the current index is not defined",
                header.kind
            )));
        }
        let mut at = *index + header.size;
        let value = self.object(header, &mut at, read)?;
        *index = at;
        Ok(value)
    }

    /// `StreamObject.tryGetCurrent`: the object of type `expected` at `index`, if the header
    /// there names one, which moves past it.
    fn try_current<T>(
        &self,
        index: &mut usize,
        expected: u16,
        read: impl FnOnce(&Self, &mut usize, usize) -> Result<T, Failure>,
    ) -> Result<Option<T>, Failure> {
        let Some(header) = try_header(self.data, *index)? else {
            return Ok(None);
        };
        if header.kind != expected {
            return Ok(None);
        }
        let mut at = *index + header.size;
        let value = self.object(header, &mut at, read)?;
        *index = at;
        Ok(Some(value))
    }

    /// `StreamObject.deserializeFromByteArray`: the items `read` reads from `index`, of the
    /// length the header gives, then the end header of a compound object.
    fn object<T>(
        &self,
        header: Header,
        index: &mut usize,
        read: impl FnOnce(&Self, &mut usize, usize) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let value = read(self, index, header.length)?;
        if kind::COMPOUND.contains(&header.kind) {
            self.end(index, header.kind)?;
        }
        Ok(value)
    }

    /// The end header of a compound object of `kind`, 8-bit or 16-bit.
    fn end(&self, index: &mut usize, object: u16) -> Result<(), Failure> {
        let mut bits = Bits::new(self.data, *index);
        let end =
            match bits.int(2)? {
                0x1 => {
                    let value = bits.int(6)? as u16;
                    if !kind::ENDS.contains(&value) {
                        return Err(thrown(
                            TIKA_EXCEPTION,
                            format_args!(
                            "Failed to get the StreamObjectHeaderEnd8bit type value, the value \
                             {value} is not defined"
                        ),
                        )
                        .into());
                    }
                    *index += 1;
                    value
                }
                0x3 => {
                    let value = bits.int(14)? as u16;
                    if !kind::ENDS.contains(&value) {
                        return Err(thrown(
                            TIKA_EXCEPTION,
                            format_args!(
                            "Failed to get the StreamObjectHeaderEnd16bit type value, the value \
                             {value} is not defined"
                        ),
                        )
                        .into());
                    }
                    *index += 2;
                    value
                }
                _ => return Err(Failure::Unported(
                    "a compound stream object without an end header, on which the suite throws a \
                     NullPointerException"
                        .to_owned(),
                )),
            };
        if end != object {
            return Err(stream_error(format_args!(
                "Unexpected the stream header end value {object}"
            )));
        }
        Ok(())
    }

    /// The headers of the objects a data element holds, read in turn while there is one: each
    /// handed to `read`, which reads the object, or says what to do with one of another type.
    fn objects(
        &self,
        index: &mut usize,
        mut read: impl FnMut(&Self, Header, &mut usize) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        while let Some(header) = try_header(self.data, *index)? {
            read(self, header, index)?;
        }
        Ok(())
    }

    /// The items of an object that are basic objects, read by `read` from a cursor, which must
    /// cover the length the header gives.
    fn items<T>(
        &self,
        index: &mut usize,
        length: usize,
        read: impl FnOnce(&mut Cursor<'a>) -> Result<T, Thrown>,
    ) -> Result<T, Failure> {
        let mut cursor = Cursor::new(self.data, *index);
        let value = read(&mut cursor)?;
        if cursor.index - *index != length {
            return Err(over_parse());
        }
        *index = cursor.index;
        Ok(value)
    }

    /// `DataElement`'s items: its ID, serial number and type, then its data.
    fn data_element(&self, index: &mut usize, length: usize) -> Result<DataElement, Failure> {
        let start = *index;
        let mut cursor = Cursor::new(self.data, start);
        let header = (|| -> Result<(ExGuid, u64), Thrown> {
            let id = cursor.ex_guid()?;
            cursor.serial_number()?;
            Ok((id, cursor.compact()?))
        })();
        let (id, element_type) =
            header.map_err(|inner| Failure::from(wrapping(DATA_ELEMENT_PARSE_ERROR, inner)))?;
        if cursor.index - start != length {
            return Err(element_error(
                "Failed to check the data element header length, whose value does not cover \
                 the dataElementExGUID, SerialNumber and DataElementType",
            ));
        }
        *index = cursor.index;
        let named = match element_type as i32 {
            0 => "None",
            6 => "FragmentDataElementData",
            1..=5 | 10 => "",
            _ => "null",
        };
        if !named.is_empty() {
            return Err(element_error(format_args!(
                "Failed to create specific data element instance with the type {named}"
            )));
        }
        let data = self
            .element_data(index, element_type as i32)
            .map_err(wrapped_in_element_error)?;
        Ok(DataElement { id, data })
    }

    fn element_data(&self, index: &mut usize, element_type: i32) -> Result<ElementData, Failure> {
        Ok(match element_type {
            1 => ElementData::StorageIndex(self.storage_index(index)?),
            2 => ElementData::StorageManifest(self.storage_manifest(index)?),
            3 => ElementData::CellManifest(self.current(
                index,
                kind::CELL_MANIFEST_CURRENT_REVISION,
                "CellManifestCurrentRevision",
                |reader, index, length| reader.items(index, length, Cursor::ex_guid),
            )?),
            4 => ElementData::RevisionManifest(self.revision_manifest(index)?),
            5 => ElementData::ObjectGroup(self.object_group(index)?),
            _ => ElementData::ObjectDataBlob(self.current(
                index,
                kind::OBJECT_DATA_BLOB,
                "ObjectDataBLOB",
                |reader, index, length| reader.items(index, length, Cursor::binary),
            )?),
        })
    }

    fn storage_index(&self, index: &mut usize) -> Result<StorageIndex, Failure> {
        let mut storage_index = StorageIndex::default();
        let mut manifest_mapping = false;
        self.objects(index, |reader, header, index| {
            *index += header.size;
            match header.kind {
                kind::STORAGE_INDEX_MANIFEST_MAPPING => {
                    if manifest_mapping {
                        return Err(element_error(
                            "Failed to parse StorageIndexDataElement, only can contain zero or \
                             one StorageIndexManifestMapping",
                        ));
                    }
                    reader.object(header, index, |reader, index, length| {
                        reader.items(index, length, |cursor| {
                            cursor.ex_guid()?;
                            cursor.serial_number()
                        })
                    })?;
                    manifest_mapping = true;
                }
                kind::STORAGE_INDEX_CELL_MAPPING => {
                    let mapping = reader.object(header, index, |reader, index, length| {
                        reader.items(index, length, |cursor| {
                            let cell = cursor.cell_id()?;
                            let manifest = cursor.ex_guid()?;
                            cursor.serial_number()?;
                            Ok((cell, manifest))
                        })
                    })?;
                    storage_index.cells.push(mapping);
                }
                kind::STORAGE_INDEX_REVISION_MAPPING => {
                    let mapping = reader.object(header, index, |reader, index, length| {
                        reader.items(index, length, |cursor| {
                            let revision = cursor.ex_guid()?;
                            let manifest = cursor.ex_guid()?;
                            cursor.serial_number()?;
                            Ok((revision, manifest))
                        })
                    })?;
                    storage_index.revisions.push(mapping);
                }
                other => {
                    return Err(Failure::Unported(format!(
                        "a storage index holding a stream object of type {other:#x}, which the \
                         suite names by its enum constant as it fails"
                    )))
                }
            }
            Ok(())
        })?;
        Ok(storage_index)
    }

    fn storage_manifest(&self, index: &mut usize) -> Result<Vec<CellId>, Failure> {
        self.current(
            index,
            kind::STORAGE_MANIFEST_SCHEMA_GUID,
            "StorageManifestSchemaGUID",
            |_, index, length| {
                *index += 16;
                if length != 16 {
                    return Err(over_parse());
                }
                Ok(())
            },
        )?;
        let mut roots = Vec::new();
        self.objects(index, |reader, header, index| {
            if header.kind != kind::STORAGE_MANIFEST_ROOT_DECLARE {
                return Err(Failure::Unported(format!(
                    "a storage manifest holding a stream object of type {:#x}, which the suite \
                     names by its enum constant as it fails",
                    header.kind
                )));
            }
            *index += header.size;
            roots.push(reader.object(header, index, |reader, index, length| {
                reader.items(index, length, |cursor| {
                    cursor.ex_guid()?;
                    cursor.cell_id()
                })
            })?);
            Ok(())
        })?;
        Ok(roots)
    }

    fn revision_manifest(&self, index: &mut usize) -> Result<RevisionManifest, Failure> {
        let (revision, base) = self.current(
            index,
            kind::REVISION_MANIFEST,
            "RevisionManifest",
            |reader, index, length| {
                reader.items(index, length, |cursor| {
                    Ok((cursor.ex_guid()?, cursor.ex_guid()?))
                })
            },
        )?;
        let mut manifest = RevisionManifest {
            revision,
            base,
            roots: Vec::new(),
            object_groups: Vec::new(),
        };
        self.objects(index, |reader, header, index| {
            match header.kind {
                kind::REVISION_MANIFEST_ROOT_DECLARE => {
                    *index += header.size;
                    manifest.roots.push(reader.object(
                        header,
                        index,
                        |reader, index, length| {
                            reader.items(index, length, |cursor| {
                                Ok((cursor.ex_guid()?, cursor.ex_guid()?))
                            })
                        },
                    )?);
                }
                kind::REVISION_MANIFEST_OBJECT_GROUP_REFERENCES => {
                    *index += header.size;
                    manifest.object_groups.push(reader.object(
                        header,
                        index,
                        |reader, index, length| reader.items(index, length, Cursor::ex_guid),
                    )?);
                }
                other => {
                    return Err(Failure::Unported(format!(
                        "a revision manifest holding a stream object of type {other:#x}, which \
                         the suite names by its enum constant as it fails"
                    )))
                }
            }
            Ok(())
        })?;
        Ok(manifest)
    }

    fn object_group(&self, index: &mut usize) -> Result<ObjectGroup, Failure> {
        self.try_current(index, kind::DATA_ELEMENT_HASH, |reader, index, length| {
            reader.items(index, length, |cursor| {
                cursor.compact()?;
                cursor.binary()
            })
        })?;
        let mut group = ObjectGroup::default();
        let (declarations, blob_declarations) = self.current(
            index,
            kind::OBJECT_GROUP_DECLARATIONS,
            "ObjectGroupDeclarations",
            |reader, index, length| reader.declarations(index, length),
        )?;
        group.declarations = declarations;
        group.blob_declarations = blob_declarations;
        self.try_current(
            index,
            kind::OBJECT_GROUP_METADATA_DECLARATIONS,
            |reader, index, length| reader.metadata_declarations(index, length),
        )?;
        let (objects, blob_references) = self.current(
            index,
            kind::OBJECT_GROUP_DATA,
            "ObjectGroupData",
            |reader, index, length| reader.group_data(index, length),
        )?;
        group.objects = objects;
        group.blob_references = blob_references;
        Ok(group)
    }

    /// `ObjectGroupDeclarations`' items: its object and object BLOB declarations.
    fn declarations(
        &self,
        index: &mut usize,
        length: usize,
    ) -> Result<(Vec<Declaration>, Vec<Declaration>), Failure> {
        if length != 0 {
            return Err(over_parse());
        }
        let (mut declarations, mut blobs) = (Vec::new(), Vec::new());
        self.objects(index, |reader, header, index| {
            match header.kind {
                kind::OBJECT_GROUP_OBJECT_DECLARE => {
                    *index += header.size;
                    declarations.push(reader.object(header, index, |reader, index, length| {
                        reader.items(index, length, |cursor| {
                            let id = cursor.ex_guid()?;
                            let partition = cursor.compact()?;
                            for _ in 0..3 {
                                cursor.compact()?;
                            }
                            Ok(Declaration { id, partition })
                        })
                    })?);
                }
                kind::OBJECT_GROUP_OBJECT_BLOB_DATA_DECLARATION => {
                    *index += header.size;
                    blobs.push(reader.object(header, index, |reader, index, length| {
                        reader.items(index, length, |cursor| {
                            let id = cursor.ex_guid()?;
                            cursor.ex_guid()?;
                            let partition = cursor.compact()?;
                            cursor.compact()?;
                            cursor.compact()?;
                            Ok(Declaration { id, partition })
                        })
                    })?);
                }
                other => {
                    return Err(Failure::Unported(format!(
                        "object group declarations holding a stream object of type {other:#x}, \
                         which the suite names by its enum constant as it fails"
                    )))
                }
            }
            Ok(())
        })?;
        Ok((declarations, blobs))
    }

    /// `ObjectGroupMetadataDeclarations`' items, read to be passed over.
    fn metadata_declarations(&self, index: &mut usize, length: usize) -> Result<(), Failure> {
        if length != 0 {
            return Err(over_parse());
        }
        self.objects(index, |reader, header, index| {
            *index += header.size;
            if header.kind != kind::OBJECT_GROUP_METADATA {
                return Err(Failure::Unported(format!(
                    "object group metadata declarations holding a stream object of type \
                     {:#x}, which the suite names by its enum constant as it fails",
                    header.kind
                )));
            }
            reader.object(header, index, |reader, index, length| {
                reader.items(index, length, Cursor::compact)
            })?;
            Ok(())
        })
    }

    /// `ObjectGroupData`'s items: its objects' data and BLOB references.
    fn group_data(
        &self,
        index: &mut usize,
        length: usize,
    ) -> Result<(Vec<ObjectData>, Vec<ExGuid>), Failure> {
        if length != 0 {
            return Err(over_parse());
        }
        let (mut objects, mut references) = (Vec::new(), Vec::new());
        self.objects(index, |reader, header, index| {
            match header.kind {
                kind::OBJECT_GROUP_OBJECT_DATA => {
                    *index += header.size;
                    objects.push(reader.object(header, index, |reader, index, length| {
                        reader.items(index, length, |cursor| {
                            Ok(ObjectData {
                                objects: cursor.ex_guids()?,
                                spaces: cursor.cell_ids()?,
                                data: cursor.binary()?,
                            })
                        })
                    })?);
                }
                kind::OBJECT_GROUP_OBJECT_DATA_BLOB_REFERENCE => {
                    *index += header.size;
                    references.push(reader.object(header, index, |reader, index, length| {
                        reader.items(index, length, |cursor| {
                            cursor.ex_guids()?;
                            cursor.cell_ids()?;
                            cursor.ex_guid()
                        })
                    })?);
                }
                other => {
                    return Err(Failure::Unported(format!(
                        "object group data holding a stream object of type {other:#x}, which the \
                         suite names by its enum constant as it fails"
                    )))
                }
            }
            Ok(())
        })?;
        Ok((objects, references))
    }
}

/// `AlternativePackaging.doDeserializeFromByteArray`: the data elements of the package, in
/// order, or why Tika's reading of it throws.
pub(super) fn package(data: &[u8]) -> Result<Vec<DataElement>, Failure> {
    for at in [0, 16, 32, 48] {
        read_guid(data, at)?;
    }
    uint32(data, 64)?;
    packaging_start(data, 68)?;
    let mut index = 72;
    let (_, length) = super::ex_guid(data, index)?;
    index += length;
    read_guid(data, index)?;
    index += 16;
    let reader = Reader { data };
    let elements = reader.try_current(
        &mut index,
        kind::DATA_ELEMENT_PACKAGE,
        |reader, index, length| {
            if length != 1 {
                return Err(over_parse());
            }
            *index += 1;
            if *index > data.len() {
                return Err(out_of_bounds(*index - 1, data.len()).into());
            }
            let mut elements = Vec::new();
            while let Some(element) =
                reader.try_current(index, kind::DATA_ELEMENT, |reader, index, length| {
                    reader.data_element(index, length)
                })?
            {
                elements.push(element);
            }
            Ok(elements)
        },
    )?;
    packaging_end(data, index)?;
    elements.ok_or_else(|| {
        Failure::Unported(
            "an alternative packaging without a data element package, on which the suite throws \
             a NullPointerException"
                .to_owned(),
        )
    })
}

/// `StreamObjectHeaderStart32bit.doDeserializeFromByteArray` of the packaging's start, which
/// throws where it is not a valid 32-bit header.
fn packaging_start(data: &[u8], index: usize) -> Result<(), Failure> {
    let mut bits = Bits::new(data, index);
    let header_type = bits.int(2)?;
    if header_type != 0x2 {
        return Err(thrown(
            TIKA_EXCEPTION,
            format_args!(
                "Failed to get the StreamObjectHeaderStart32bit header type value, expect value \
                 2, but actual value is {header_type}"
            ),
        )
        .into());
    }
    let compound = bits.int(1)?;
    let start_kind = bits.int(14)? as u16;
    if !kind::DEFINED.contains(&start_kind) {
        return Err(thrown(
            TIKA_EXCEPTION,
            format_args!(
                "Failed to get the StreamObjectHeaderStart32bit type value, the value \
                 {start_kind} is not defined"
            ),
        )
        .into());
    }
    if kind::COMPOUND.contains(&start_kind) && compound != 1 {
        return Err(thrown(
            TIKA_EXCEPTION,
            format_args!(
                "Failed to parse the StreamObjectHeaderStart32bit header. If the type value is \
                 {start_kind} then the compound value should 1, but actual value is 0"
            ),
        )
        .into());
    }
    if bits.int(15)? == 32767 {
        compact(data, index + 4)?;
    }
    Ok(())
}

/// `StreamObjectHeaderEnd16bit.doDeserializeFromByteArray` of the packaging's end, which throws
/// where it is not a valid 16-bit end header.
fn packaging_end(data: &[u8], index: usize) -> Result<(), Failure> {
    let mut bits = Bits::new(data, index);
    let header_type = bits.int(2)?;
    if header_type != 0x3 {
        return Err(thrown(
            TIKA_EXCEPTION,
            format_args!(
                "Failed to get the StreamObjectHeaderEnd16bit header type value, expect value 3, \
                 but actual value is {header_type}"
            ),
        )
        .into());
    }
    let end = bits.int(14)? as u16;
    if !kind::ENDS.contains(&end) {
        return Err(thrown(
            TIKA_EXCEPTION,
            format_args!(
                "Failed to get the StreamObjectHeaderEnd16bit type value, the value {end} is not \
                 defined"
            ),
        )
        .into());
    }
    Ok(())
}
