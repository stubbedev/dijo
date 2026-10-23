//! Tika's `MSOneStoreParser`: the cells of the revision store, each the objects of the object
//! groups its current revision and the revisions it is based on reference, an object declared
//! again in a later group replacing the earlier one; and the property sets of the objects, as
//! the suite's `ObjectSpaceObjectPropSet` reads them.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::stream::{DataElement, ElementData, ObjectData, ObjectGroup, RevisionManifest};
use super::{
    int32, out_of_bounds, thrown, uint16, uint32, Bits, CellId, CellKey, ExGuid, Failure, GuidKey,
    Uuid, IO_EXCEPTION,
};

/// `MSOneStorePackage.MAX_PARSE_WARNINGS`.
const MAX_PARSE_WARNINGS: usize = 100;
/// `PropertySet.MAX_PROPERTY_NESTING`.
const MAX_PROPERTY_NESTING: usize = 100;
/// `MSOneStoreParser.ENCRYPTION_KEY_ROOT_EXGUID`, {4A3717F8-1C14-49E7-9526-81D942DE1741}.
pub(super) const ENCRYPTION_KEY_ROOT: GuidKey = (
    3,
    Uuid([
        0x4A, 0x37, 0x17, 0xF8, 0x1C, 0x14, 0x49, 0xE7, 0x95, 0x26, 0x81, 0xD9, 0x42, 0xDE, 0x17,
        0x41,
    ]),
);

/// The warnings of the package, recorded as `recordParseWarning` records them: each once, up to
/// a hundred, then one that says the rest were suppressed.
#[derive(Debug, Default, Clone)]
pub(super) struct Warnings {
    keys: HashSet<String>,
    suppressed: bool,
    /// The warnings recorded before the walk writes them into the metadata.
    pub(super) recorded: Vec<String>,
}

impl Warnings {
    /// `recordParseWarning` of a warning of a kind keyed by `key`, the warning itself for most.
    pub(super) fn record(&mut self, key: &str, warning: String) {
        if self.keys.contains(key) {
            return;
        }
        if self.keys.len() >= MAX_PARSE_WARNINGS {
            if !self.suppressed {
                self.suppressed = true;
                self.recorded.push(format!(
                    "Additional OneNote parse warnings were suppressed after {MAX_PARSE_WARNINGS} \
                     distinct warnings"
                ));
            }
            return;
        }
        self.keys.insert(key.to_owned());
        self.recorded.push(warning);
    }

    /// `warningsSaturated`.
    pub(super) fn saturated(&self) -> bool {
        self.suppressed
    }
}

/// `MSOneStorePackage` as the parser leaves it.
#[derive(Debug)]
pub(crate) struct Package<'a> {
    /// The cells other than the header and the data root, in the order of the storage index.
    pub(super) cells: Vec<Cell<'a>>,
    pub(super) data_root: Cell<'a>,
    pub(super) warnings: Warnings,
}

/// `RevisionStoreCell`.
#[derive(Debug)]
pub(super) struct Cell<'a> {
    pub(super) id: CellId,
    /// The objects of its object groups, in order.
    pub(super) groups: Vec<Vec<Rc<Object<'a>>>>,
    /// The objects its revisions declare as roots, each root once, last declaration winning.
    pub(super) roots: Vec<ExGuid>,
}

/// `RevisionStoreObject`.
#[derive(Debug, Default)]
pub(super) struct Object<'a> {
    pub(super) id: Option<ExGuid>,
    pub(super) property_set: Option<PropertySet<'a>>,
    /// `referencedObjectID`, `referencedObjectSpacesID`.
    pub(super) references: Vec<ExGuid>,
    pub(super) space_references: Vec<CellId>,
    /// The data of its file data object, if it has one: `None` where the BLOB is missing.
    pub(super) file_data: Option<Option<&'a [u8]>>,
}

/// `PropertySet`: the IDs of its properties, each with its value.
#[derive(Debug)]
pub(super) struct PropertySet<'a> {
    pub(super) properties: Vec<(PropertyId, Property<'a>)>,
}

/// `PropertyID`: the property's type and its whole 32-bit ID.
#[derive(Debug, Clone, Copy)]
pub(super) struct PropertyId {
    pub(super) kind: u32,
    pub(super) value: u32,
}

/// The value of a property, as the reader its type names reads it.
#[derive(Debug)]
pub(super) enum Property<'a> {
    NoData,
    /// `ArrayNumber`: how many references it takes.
    ArrayNumber(i32),
    Fixed(Vec<u8>),
    /// `PrtFourBytesOfLengthFollowedByData`.
    Data(&'a [u8]),
    Values(Vec<PropertySet<'a>>),
    Set(PropertySet<'a>),
}

/// The property types of `PropertyType`.
pub(super) mod kind {
    pub(crate) const NO_DATA: u32 = 0x1;
    pub(crate) const BOOL: u32 = 0x2;
    pub(crate) const ONE_BYTE: u32 = 0x3;
    pub(crate) const TWO_BYTES: u32 = 0x4;
    pub(crate) const FOUR_BYTES: u32 = 0x5;
    pub(crate) const EIGHT_BYTES: u32 = 0x6;
    pub(crate) const DATA: u32 = 0x7;
    pub(crate) const OBJECT_ID: u32 = 0x8;
    pub(crate) const ARRAY_OF_OBJECT_IDS: u32 = 0x9;
    pub(crate) const OBJECT_SPACE_ID: u32 = 0xA;
    pub(crate) const ARRAY_OF_OBJECT_SPACE_IDS: u32 = 0xB;
    pub(crate) const CONTEXT_ID: u32 = 0xC;
    pub(crate) const ARRAY_OF_CONTEXT_IDS: u32 = 0xD;
    pub(crate) const ARRAY_OF_PROPERTY_VALUES: u32 = 0x10;
    pub(crate) const PROPERTY_SET: u32 = 0x11;
}

/// `ObjectSpaceObjectPropSet.doDeserializeFromByteArray` of an object's data: its streams of
/// object, object space and context IDs, read to be passed over, then its property set.
fn property_set_object(data: &[u8]) -> Result<PropertySet<'_>, Failure> {
    let mut index = 0;
    let (length, oids) = id_stream(data, index, "ObjectSpaceObjectStreamOfOIDs")?;
    index += length;
    if !oids.osid_stream_not_present {
        let (length, osids) = id_stream(data, index, "ObjectSpaceObjectStreamOfOSIDs")?;
        index += length;
        if osids.extended_streams_present {
            let (length, _) = id_stream(data, index, "ObjectSpaceObjectStreamOfContextIDs")?;
            index += length;
        }
    }
    Ok(property_set(data, index, 0)?.0)
}

/// The flags of an object space stream's header.
struct StreamFlags {
    extended_streams_present: bool,
    osid_stream_not_present: bool,
}

/// An object space stream of compact IDs at `index`: the bytes it takes, and its header's flags.
fn id_stream(data: &[u8], index: usize, name: &str) -> Result<(usize, StreamFlags), Failure> {
    let mut bits = Bits::new(data, index);
    let count = bits.read(24)?;
    bits.read(6)?;
    let flags = StreamFlags {
        extended_streams_present: bits.read(1)? == 1,
        osid_stream_not_present: bits.read(1)? == 1,
    };
    let after = index as i64 + 4;
    let remaining = data.len() as i64 - after;
    if count as i64 > remaining / 4 {
        return Err(thrown(
            IO_EXCEPTION,
            format_args!("{name} count {count} exceeds remaining data {remaining}"),
        )
        .into());
    }
    let mut at = after as usize;
    for _ in 0..count {
        let mut bits = Bits::new(data, at);
        bits.read(32)?;
        at += 4;
    }
    Ok((at - index, flags))
}

/// `PropertySet.doDeserializeFromByteArray` at `index`, `depth` sets deep: the set and the bytes
/// it takes.
fn property_set(
    data: &[u8],
    index: usize,
    depth: usize,
) -> Result<(PropertySet<'_>, usize), Failure> {
    if depth > MAX_PROPERTY_NESTING {
        return Err(thrown(
            IO_EXCEPTION,
            format_args!("PropertySet nesting exceeds {MAX_PROPERTY_NESTING}"),
        )
        .into());
    }
    let count = uint16(data, index)?;
    let mut at = index + 2;
    let mut ids = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        ids.push(property_id(data, at)?);
        at += 4;
    }
    let mut properties = Vec::with_capacity(ids.len());
    for id in ids {
        let (property, length) = property(data, at, id, depth)?;
        properties.push((id, property));
        at += length;
    }
    Ok((PropertySet { properties }, at - index))
}

/// `PropertyID.doDeserializeFromByteArray`.
fn property_id(data: &[u8], index: usize) -> Result<PropertyId, Failure> {
    let mut bits = Bits::new(data, index);
    bits.read(26)?;
    let kind = bits.read(5)? as u32;
    bits.read(1)?;
    Ok(PropertyId {
        kind,
        value: int32(data, index)? as u32,
    })
}

/// The value of property `id` at `index`, and the bytes it takes.
fn property(
    data: &[u8],
    index: usize,
    id: PropertyId,
    depth: usize,
) -> Result<(Property<'_>, usize), Failure> {
    let fixed = |length: usize| -> Result<(Property<'_>, usize), Failure> {
        if index > data.len() {
            return Err(Failure::Unported(
                "a copy of a byte array from past its end, whose exception the port does not \
                 name"
                    .to_owned(),
            ));
        }
        let mut bytes = vec![0u8; length];
        let available = (data.len() - index).min(length);
        bytes[..available].copy_from_slice(&data[index..index + available]);
        Ok((Property::Fixed(bytes), length))
    };
    Ok(match id.kind {
        kind::NO_DATA | kind::BOOL | kind::OBJECT_ID | kind::CONTEXT_ID | kind::OBJECT_SPACE_ID => {
            (Property::NoData, 0)
        }
        kind::ARRAY_OF_OBJECT_IDS
        | kind::ARRAY_OF_OBJECT_SPACE_IDS
        | kind::ARRAY_OF_CONTEXT_IDS => (Property::ArrayNumber(int32(data, index)?), 4),
        kind::ONE_BYTE => {
            let byte = *data
                .get(index)
                .ok_or_else(|| out_of_bounds(index, data.len()))?;
            (Property::Fixed(vec![byte]), 1)
        }
        kind::TWO_BYTES => {
            let first = *data
                .get(index)
                .ok_or_else(|| out_of_bounds(index, data.len()))?;
            let second = *data
                .get(index + 1)
                .ok_or_else(|| out_of_bounds(index + 1, data.len()))?;
            (Property::Fixed(vec![first, second]), 2)
        }
        kind::FOUR_BYTES => fixed(4)?,
        kind::EIGHT_BYTES => fixed(8)?,
        kind::DATA => {
            let length = uint32(data, index)?;
            let start = index + 4;
            let remaining = data.len() as i64 - start as i64;
            if i64::from(length) > remaining {
                return Err(thrown(
                    IO_EXCEPTION,
                    format_args!(
                        "prtFourBytesOfLengthFollowedByData length {length} exceeds remaining \
                         data {remaining}"
                    ),
                )
                .into());
            }
            let end = start + length as usize;
            (Property::Data(&data[start..end]), 4 + length as usize)
        }
        kind::ARRAY_OF_PROPERTY_VALUES => {
            let count = int32(data, index)?;
            let mut at = index + 4;
            property_id(data, at)?;
            at += 4;
            let remaining = data.len() as i64 - at as i64;
            if count < 0 || i64::from(count) > remaining / 2 {
                return Err(thrown(
                    IO_EXCEPTION,
                    format_args!(
                        "prtArrayOfPropertyValues count {count} exceeds remaining data \
                         {remaining}"
                    ),
                )
                .into());
            }
            let mut sets = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let (set, length) = property_set(data, at, depth + 1)?;
                sets.push(set);
                at += length;
            }
            (Property::Values(sets), at - index)
        }
        kind::PROPERTY_SET => {
            let (set, length) = property_set(data, index, depth + 1)?;
            (Property::Set(set), length)
        }
        other => {
            return Err(Failure::Unported(format!(
                "a property of type {other:#x}, on whose type the suite's switch throws a \
                 NullPointerException"
            )))
        }
    })
}

/// The data elements of the package by type, and the maps the parser looks them up in, each
/// keeping the first element of an ID.
struct Elements<'e, 'a> {
    cell_manifests: HashMap<GuidKey, ExGuid>,
    revision_manifests: HashMap<GuidKey, &'e RevisionManifest>,
    object_groups: HashMap<GuidKey, (ExGuid, &'e ObjectGroup<'a>)>,
    blobs: HashMap<GuidKey, &'a [u8]>,
    cell_mappings: HashMap<CellKey, ExGuid>,
    revision_mappings: HashMap<GuidKey, ExGuid>,
}

impl<'e> Elements<'e, '_> {
    /// The revision manifest the storage index maps `revision` to, if any.
    fn revision_manifest(&self, revision: ExGuid) -> Option<&'e RevisionManifest> {
        self.revision_mappings
            .get(&revision.key())
            .and_then(|mapping| self.revision_manifests.get(&mapping.key()).copied())
    }
}

/// `MSOneStoreParser.parse` of the data elements of a package.
pub(super) fn parse<'a>(elements: &[DataElement<'a>]) -> Result<Package<'a>, Failure> {
    let mut storage_index = None;
    let mut storage_manifest = None;
    let mut found = Elements {
        cell_manifests: HashMap::new(),
        revision_manifests: HashMap::new(),
        object_groups: HashMap::new(),
        blobs: HashMap::new(),
        cell_mappings: HashMap::new(),
        revision_mappings: HashMap::new(),
    };
    for element in elements {
        let key = element.id.key();
        match &element.data {
            ElementData::StorageIndex(index) => {
                storage_index.get_or_insert(index);
            }
            ElementData::StorageManifest(roots) => {
                storage_manifest.get_or_insert(roots);
            }
            ElementData::CellManifest(revision) => {
                found.cell_manifests.entry(key).or_insert(*revision);
            }
            ElementData::RevisionManifest(manifest) => {
                found.revision_manifests.entry(key).or_insert(manifest);
            }
            ElementData::ObjectGroup(group) => {
                found
                    .object_groups
                    .entry(key)
                    .or_insert((element.id, group));
            }
            ElementData::ObjectDataBlob(data) => {
                found.blobs.entry(key).or_insert(data);
            }
        }
    }
    let (Some(storage_index), Some(roots)) = (storage_index, storage_manifest) else {
        return Err(thrown(
            IO_EXCEPTION,
            "Missing OneNote storage index or storage manifest",
        )
        .into());
    };
    if roots.len() < 2 {
        return Err(thrown(
            IO_EXCEPTION,
            "Missing OneNote header or data-root declaration",
        )
        .into());
    }
    for (cell, mapping) in &storage_index.cells {
        found.cell_mappings.entry(cell.key()).or_insert(*mapping);
    }
    for (revision, mapping) in &storage_index.revisions {
        found
            .revision_mappings
            .entry(revision.key())
            .or_insert(*mapping);
    }
    let mut warnings = Warnings::default();
    let mut parsed = HashSet::new();
    let header = roots[0];
    parsed.insert(header.key());
    header_cell(&found, header)?;
    let data_root_id = roots[1];
    parsed.insert(data_root_id.key());
    let mut cache = HashMap::new();
    let data_root = cell(&found, data_root_id, &mut warnings, &mut cache)?.ok_or_else(|| {
        Failure::from(thrown(
            IO_EXCEPTION,
            "Could not parse the OneNote data root cell",
        ))
    })?;
    let mut cells = Vec::new();
    for (id, _) in &storage_index.cells {
        if parsed.insert(id.key()) {
            if let Some(cell) = cell(&found, *id, &mut warnings, &mut cache)? {
                cells.push(cell);
            }
        }
    }
    Ok(Package {
        cells,
        data_root,
        warnings,
    })
}

/// `parseHeaderCell`: the header cell's object read, which may throw, as nothing of it is kept.
fn header_cell(found: &Elements, header: CellId) -> Result<(), Failure> {
    let Some(manifest) = found.cell_mappings.get(&header.key()) else {
        return Ok(());
    };
    let Some(revision) = found.cell_manifests.get(&manifest.key()) else {
        return Ok(());
    };
    let Some(revision_manifest) = found.revision_manifest(*revision) else {
        return Ok(());
    };
    let Some(first) = revision_manifest.object_groups.first() else {
        return Ok(());
    };
    let Some((_, group)) = found.object_groups.get(&first.key()) else {
        return Ok(());
    };
    for (index, declaration) in group.declarations.iter().enumerate() {
        if declaration.partition == 1 {
            let data = object_data(group, index)?;
            property_set_object(data.data)?;
            break;
        }
    }
    Ok(())
}

/// `objectGroupObjectDataList.get(index)`.
fn object_data<'g, 'a>(
    group: &'g ObjectGroup<'a>,
    index: usize,
) -> Result<&'g ObjectData<'a>, Failure> {
    group.objects.get(index).ok_or_else(|| {
        thrown(
            "java.lang.IndexOutOfBoundsException",
            format_args!(
                "Index {index} out of bounds for length {}",
                group.objects.len()
            ),
        )
        .into()
    })
}

/// The object groups made once each, by the data element that holds them and whether its
/// revision is encrypted.
type GroupCache<'a> = HashMap<(GuidKey, bool), Rc<Vec<Rc<Object<'a>>>>>;

/// `parseCell`: the cell's objects, or none, with a warning, where the storage index does not
/// lead to its revision manifest.
fn cell<'a>(
    found: &Elements<'_, 'a>,
    id: CellId,
    warnings: &mut Warnings,
    cache: &mut GroupCache<'a>,
) -> Result<Option<Cell<'a>>, Failure> {
    let mut skip = |reason: &str| {
        let warning = format!("Skipping OneNote cell {id}: {reason}");
        warnings.record(&warning.clone(), warning);
        Ok(None)
    };
    let Some(manifest) = found.cell_mappings.get(&id.key()) else {
        return skip("no storage-index cell mapping");
    };
    let Some(revision) = found.cell_manifests.get(&manifest.key()) else {
        return skip("no current cell manifest");
    };
    if !found.revision_mappings.contains_key(&revision.key()) {
        return skip("no revision mapping");
    }
    let Some(mut revision_manifest) = found.revision_manifest(*revision) else {
        return skip("no revision manifest");
    };
    let mut chain: Vec<&RevisionManifest> = Vec::new();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(revision_manifest.revision.key()) {
            break;
        }
        chain.push(revision_manifest);
        let base = revision_manifest.base;
        if base.guid == Uuid::EMPTY {
            break;
        }
        match found.revision_manifest(base) {
            Some(manifest) => revision_manifest = manifest,
            None => break,
        }
    }
    chain.reverse();
    let mut seen_groups = HashSet::new();
    let mut roots: Vec<(ExGuid, ExGuid)> = Vec::new();
    let mut groups: Vec<Vec<Rc<Object<'a>>>> = Vec::new();
    for manifest in chain {
        let encrypted = manifest
            .roots
            .iter()
            .any(|(root, _)| root.key() == ENCRYPTION_KEY_ROOT);
        for &(root, object) in &manifest.roots {
            if root.key() == ENCRYPTION_KEY_ROOT {
                continue;
            }
            match roots.iter_mut().find(|(known, _)| known.same(root)) {
                Some(declared) => declared.1 = object,
                None => roots.push((root, object)),
            }
        }
        for reference in &manifest.object_groups {
            if !seen_groups.insert(reference.key()) {
                continue;
            }
            let Some((element_id, group)) = found.object_groups.get(&reference.key()) else {
                let warning = format!("OneNote object group {reference} could not be resolved");
                warnings.record(&warning.clone(), warning);
                continue;
            };
            let key = (element_id.key(), encrypted);
            let objects = match cache.get(&key) {
                Some(objects) => objects.clone(),
                None => {
                    let objects = Rc::new(objects(group, encrypted, &found.blobs)?);
                    cache.insert(key, objects.clone());
                    objects
                }
            };
            groups.push(objects.as_ref().clone());
        }
    }
    remove_superseded(&mut groups);
    Ok(Some(Cell {
        id,
        groups,
        roots: roots.into_iter().map(|(_, object)| object).collect(),
    }))
}

/// `RevisionStoreObjectGroup.createInstance`: the objects an object group declares, each once
/// in the order first declared, its property set or JCID and its file data object filled in;
/// none of an encrypted revision.
fn objects<'a>(
    group: &ObjectGroup<'a>,
    encrypted: bool,
    blobs: &HashMap<GuidKey, &'a [u8]>,
) -> Result<Vec<Rc<Object<'a>>>, Failure> {
    let mut order: Vec<GuidKey> = Vec::new();
    let mut objects: HashMap<GuidKey, Object<'a>> = HashMap::new();
    if encrypted {
        for index in 0..group.declarations.len() {
            missing_data(group, index)?;
        }
        return Ok(Vec::new());
    }
    for (index, declaration) in group.declarations.iter().enumerate() {
        missing_data(group, index)?;
        let data = &group.objects[index];
        let key = declaration.id.key();
        let object = objects.entry(key).or_insert_with(|| {
            order.push(key);
            Object {
                id: Some(declaration.id),
                ..Object::default()
            }
        });
        match declaration.partition {
            4 => {
                Bits::new(data.data, 0).read(32)?;
            }
            1 => {
                object.property_set = Some(property_set_object(data.data)?);
                object.references = data.objects.clone();
                object.space_references = data.spaces.clone();
            }
            _ => {}
        }
    }
    for (index, declaration) in group.blob_declarations.iter().enumerate() {
        let Some(blob) = group.blob_references.get(index) else {
            return Err(thrown(
                IO_EXCEPTION,
                format_args!("Missing BLOB reference for object declaration {index}"),
            )
            .into());
        };
        let key = declaration.id.key();
        let object = objects.entry(key).or_insert_with(|| {
            order.push(key);
            Object::default()
        });
        if declaration.partition == 2 {
            object.id = Some(declaration.id);
            object.file_data = Some(blobs.get(&blob.key()).copied());
        }
    }
    Ok(order
        .into_iter()
        .map(|key| Rc::new(objects.remove(&key).expect("an object of each key")))
        .collect())
}

/// The exception of an object declaration without its object's data.
fn missing_data(group: &ObjectGroup<'_>, index: usize) -> Result<(), Failure> {
    if index >= group.objects.len() {
        return Err(thrown(
            IO_EXCEPTION,
            format_args!("Missing object data for object declaration {index}"),
        )
        .into());
    }
    Ok(())
}

/// `removeSupersededObjects`: an object declared again in a later group takes the place of the
/// earlier one there, and leaves the later group.
fn remove_superseded(groups: &mut [Vec<Rc<Object<'_>>>]) {
    let mut places: HashMap<GuidKey, (usize, usize)> = HashMap::new();
    for group in 0..groups.len() {
        let mut kept = 0;
        for index in 0..groups[group].len() {
            if let Some(key) = groups[group][index].id.map(ExGuid::key) {
                match places.entry(key) {
                    Entry::Occupied(place) => {
                        let (earlier, at) = *place.get();
                        let object = groups[group][index].clone();
                        groups[earlier][at] = object;
                        continue;
                    }
                    Entry::Vacant(place) => {
                        place.insert((group, kept));
                    }
                }
            }
            groups[group].swap(kept, index);
            kept += 1;
        }
        groups[group].truncate(kept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remove_superseded_one_at_a_time(groups: &mut [Vec<Rc<Object<'_>>>]) {
        let mut places: HashMap<GuidKey, (usize, usize)> = HashMap::new();
        for group in 0..groups.len() {
            let mut index = 0;
            while index < groups[group].len() {
                let object = groups[group][index].clone();
                match object.id.map(ExGuid::key) {
                    Some(key) if places.contains_key(&key) => {
                        let (earlier, at) = places[&key];
                        groups[earlier][at] = object;
                        groups[group].remove(index);
                    }
                    Some(key) => {
                        places.insert(key, (group, index));
                        index += 1;
                    }
                    None => index += 1,
                }
            }
        }
    }

    #[test]
    fn removes_superseded_objects_as_removing_them_one_at_a_time_does() {
        let mut seed = 0x2545_F491_u32;
        for _ in 0..500 {
            let mut groups: Vec<Vec<Rc<Object<'_>>>> = Vec::new();
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            for _ in 0..(seed >> 16) % 5 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let group = (0..(seed >> 16) % 7)
                    .map(|_| {
                        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                        let pick = (seed >> 16) % 6;
                        let id = (pick > 0).then(|| ExGuid {
                            kind: i32::try_from(seed >> 29).unwrap(),
                            value: i32::try_from(pick).unwrap(),
                            guid: Uuid::EMPTY,
                        });
                        Rc::new(Object {
                            id,
                            ..Object::default()
                        })
                    })
                    .collect();
                groups.push(group);
            }
            let mut expected = groups.clone();
            remove_superseded_one_at_a_time(&mut expected);
            remove_superseded(&mut groups);
            assert_eq!(groups.len(), expected.len());
            for (group, expected) in groups.iter().zip(&expected) {
                assert_eq!(group.len(), expected.len());
                assert!(group
                    .iter()
                    .zip(expected)
                    .all(|(object, expected)| Rc::ptr_eq(object, expected)));
            }
        }
    }
}
