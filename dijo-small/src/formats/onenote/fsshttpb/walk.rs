//! `MSOneStorePackage.walkTree`: each page cell the section references, in order, in a div of
//! class `page`, its objects walked from its roots along the references of their properties,
//! writing texts, links and the files objects hold, and collecting the authors and times.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use regex::Regex;

use super::store::{kind, Cell, Object, Package, Property, PropertyId, PropertySet, Warnings};
use super::{thrown, CellId, ExGuid, GuidKey, TIKA_EXCEPTION};
use crate::formats::embedded::{self, ResourceType, Wrapping, RELATIONSHIP_ID, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::onenote::file::Thrown;
use crate::formats::onenote::walker::HYPERLINK;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Failure, Opened};
use crate::java::{compare, is_blank, utf16le};

/// `MAX_OBJECT_WALK_DEPTH` and `MAX_REFERENCE_COUNT`.
const MAX_DEPTH: usize = 1000;
const MAX_REFERENCES: i32 = 100_000;
/// Seconds from 1970 back to 1980, and to 1601.
const TIME32_EPOCH_DIFF_1980: i64 = 315_532_800;
const DATETIME_EPOCH_DIFF_1601: i64 = -11_644_473_600;

/// The property IDs `OneNotePropertyEnum.of` names that the walk tells apart.
mod property {
    pub(super) const RG_OUTLINE_INDENT_DISTANCE: u32 = 0x1C00_1C12;
    pub(super) const RICH_EDIT_TEXT_UNICODE: u32 = 0x1C00_1C22;
    pub(super) const NOTEBOOK_MANAGEMENT_ENTITY_GUID: u32 = 0x1C00_1C30;
    pub(super) const PICTURE_CONTAINER: u32 = 0x2000_1C3F;
    pub(super) const CREATION_TIME_STAMP: u32 = 0x1400_1D09;
    pub(super) const STRUCTURE_ELEMENT_CHILD_NODES: u32 = 0x2400_1D5F;
    pub(super) const AUTHOR: u32 = 0x1C00_1D75;
    pub(super) const LAST_MODIFIED_TIME_STAMP: u32 = 0x1800_1D77;
    pub(super) const AUTHOR_ORIGINAL: u32 = 0x2000_1D78;
    pub(super) const AUTHOR_MOST_RECENT: u32 = 0x2000_1D79;
    pub(super) const LAST_MODIFIED_TIME: u32 = 0x1400_1D7A;
    pub(super) const EMBEDDED_FILE_CONTAINER: u32 = 0x2000_1D9B;
    pub(super) const EMBEDDED_FILE_NAME: u32 = 0x1C00_1D9C;
    pub(super) const IMAGE_FILENAME: u32 = 0x1C00_1DD7;
    pub(super) const WEB_PICTURE_CONTAINER_14: u32 = 0x2000_34C8;
    pub(super) const TEXT_EXTENDED_ASCII: u32 = 0x1C00_3498;
}

/// How the walk ended: with whether it wrote any content, or with the exception after which
/// Tika falls back to the legacy dump, or with a failure of the output, which ends the document.
pub(crate) enum Walked {
    Done { content: bool },
    Thrown(Thrown),
    Failed(Failure),
}

/// What ends a walk early.
enum Ended {
    Thrown(Thrown),
    Failed(Failure),
}

impl From<Stop> for Ended {
    fn from(stop: Stop) -> Self {
        Ended::Failed(Failure::Stopped(stop))
    }
}

impl From<Failure> for Ended {
    fn from(failure: Failure) -> Self {
        Ended::Failed(failure)
    }
}

/// Whose author an object's `Author` property names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthorRole {
    None,
    MostRecent,
    Original,
}

/// The kinds of warning `recordParseWarning` records once whatever their text.
#[derive(Debug, Clone, Copy)]
enum WarningKind {
    ReferencesUnavailable,
    ReferencesCapped,
    SpaceReferencesUnavailable,
    SpaceReferencesCapped,
}

impl WarningKind {
    fn key(self) -> &'static str {
        match self {
            Self::ReferencesUnavailable => "OBJECT_REFERENCE_ARRAY_UNAVAILABLE",
            Self::ReferencesCapped => "OBJECT_REFERENCE_ARRAY_CAPPED",
            Self::SpaceReferencesUnavailable => "OBJECT_SPACE_REFERENCE_ARRAY_UNAVAILABLE",
            Self::SpaceReferencesCapped => "OBJECT_SPACE_REFERENCE_ARRAY_CAPPED",
        }
    }
}

/// `EmbeddedResourceInfo`: the name and type of a file an object holds.
#[derive(Debug, Clone)]
struct ResourceInfo {
    name: Option<String>,
    kind: ResourceType,
}

/// `PropertyAction`: a property of an object's property sets, flattened, with the object or
/// object space it references, and the author it names, decoded once.
#[derive(Debug, Clone)]
struct Action<'a> {
    id: PropertyId,
    property: Rc<PropertyValue<'a>>,
    child: Option<Option<ExGuid>>,
    space: Option<CellId>,
    author: OnceCell<String>,
}

impl Action<'_> {
    /// The author an `Author` property names, as `decodeOneNoteText` decodes it.
    fn author(&self) -> Option<&str> {
        match self.property.as_ref() {
            PropertyValue::Data(bytes) if self.id.value == property::AUTHOR => {
                Some(self.author.get_or_init(|| one_note_text(bytes)))
            }
            _ => None,
        }
    }
}

/// A property's value, kept for the actions that read it.
#[derive(Debug)]
enum PropertyValue<'a> {
    /// `PrtFourBytesOfLengthFollowedByData`.
    Data(&'a [u8]),
    /// `FourBytesOfData` or `EightBytesOfData`, as `getScalar` reads it.
    Scalar(i64),
    /// Of another class, which `getScalar` names as it throws.
    Other(&'static str),
}

/// The class `getScalar` names in the exception it throws on a property of type `kind`.
fn class_of(kind: u32) -> &'static str {
    match kind {
        kind::ONE_BYTE => "OneByteOfData",
        kind::TWO_BYTES => "TwoBytesOfData",
        kind::FOUR_BYTES => "FourBytesOfData",
        kind::EIGHT_BYTES => "EightBytesOfData",
        kind::DATA => "PrtFourBytesOfLengthFollowedByData",
        kind::ARRAY_OF_OBJECT_IDS
        | kind::ARRAY_OF_OBJECT_SPACE_IDS
        | kind::ARRAY_OF_CONTEXT_IDS => "ArrayNumber",
        kind::ARRAY_OF_PROPERTY_VALUES => "PrtArrayOfPropertyValues",
        _ => "NoData",
    }
}

/// The walk of a package's cells.
struct Walker<'p, 'a, 'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    warnings: Warnings,
    hyperlink: Regex,
    actions: HashMap<*const Object<'a>, Rc<Vec<Action<'a>>>>,
    authors: HashSet<String>,
    most_recent_authors: HashSet<String>,
    original_authors: HashSet<String>,
    /// `lastModifiedTimestamp`, in seconds, if any.
    last_modified_timestamp: Option<i64>,
    creation_timestamp: i64,
    last_modified: i64,
    content: bool,
    package: &'p Package<'a>,
}

/// The objects of a cell by their IDs, as `indexObjectsById` indexes them, the last of an ID kept
/// in the place of its first.
struct Objects<'a> {
    by_id: HashMap<GuidKey, Rc<Object<'a>>>,
    /// The IDs in the order the cell declares them, where Tika iterates them in its `HashMap`'s
    /// order (#129).
    order: Vec<GuidKey>,
}

impl<'a> Objects<'a> {
    fn of(groups: &[Vec<Rc<Object<'a>>>]) -> Self {
        let mut by_id = HashMap::new();
        let mut order = Vec::new();
        for object in groups.iter().flatten() {
            if let Some(id) = object.id {
                if by_id.insert(id.key(), object.clone()).is_none() {
                    order.push(id.key());
                }
            }
        }
        Objects { by_id, order }
    }

    fn get(&self, id: ExGuid) -> Option<&Rc<Object<'a>>> {
        self.by_id.get(&id.key())
    }
}

/// `walkTree`: the parse warnings, the page cells in div elements, the other cells, then the
/// authors.
pub(super) fn walk(package: &Package<'_>, xhtml: &mut Xhtml) -> Walked {
    let mut walker = Walker {
        xhtml,
        warnings: Warnings::default(),
        hyperlink: Regex::new(HYPERLINK).expect("the hyperlink pattern is valid"),
        actions: HashMap::new(),
        authors: HashSet::new(),
        most_recent_authors: HashSet::new(),
        original_authors: HashSet::new(),
        last_modified_timestamp: None,
        creation_timestamp: i64::MAX,
        last_modified: i64::MIN,
        content: false,
        package,
    };
    match walker.walk() {
        Ok(()) => Walked::Done {
            content: walker.content,
        },
        Err(Ended::Thrown(thrown)) => Walked::Thrown(thrown),
        Err(Ended::Failed(failure)) => Walked::Failed(failure),
    }
}

impl<'p, 'a> Walker<'p, 'a, '_, '_> {
    fn walk(&mut self) -> Result<(), Ended> {
        self.warnings = self.package.warnings.clone();
        self.flush_warnings();
        if !self.package.cells.is_empty() {
            let (pages, others) = self.split_cells();
            for cell in pages {
                self.xhtml.start_element_with("div", &[("class", "page")])?;
                let walked = self.walk_cell(cell);
                let ended = self.xhtml.end_element("div");
                walked?;
                ended?;
            }
            for cell in others {
                self.walk_cell(cell)?;
            }
        }
        let sorted = |values: &HashSet<String>| {
            let mut sorted: Vec<String> = values.iter().cloned().collect();
            sorted.sort_by(|a, b| compare(a, b));
            sorted
        };
        let metadata = self.xhtml.metadata_mut();
        for (name, values) in [
            ("dc:creator", &self.authors),
            ("onenote:mostRecentAuthors", &self.most_recent_authors),
            ("onenote:originalAuthors", &self.original_authors),
        ] {
            if !values.is_empty() {
                metadata.set_values(name, sorted(values));
            }
        }
        Ok(())
    }

    /// Writes the warnings recorded into the metadata.
    fn flush_warnings(&mut self) {
        let metadata = self.xhtml.metadata_mut();
        for warning in self.warnings.recorded.drain(..) {
            metadata.add(embedded::EXCEPTION_WARNING, warning);
        }
    }

    /// `recordParseWarning` of a warning keyed by its text.
    fn warn(&mut self, warning: String) {
        self.warnings.record(&warning.clone(), warning);
        self.flush_warnings();
    }

    fn warn_kind(&mut self, kind: WarningKind, warning: String) {
        self.warnings.record(kind.key(), warning);
        self.flush_warnings();
    }

    /// `splitCells`: the cells the section references, in the order it does, then the cells it
    /// does not, but for those of an object space it references.
    fn split_cells(&mut self) -> (Vec<&'p Cell<'a>>, Vec<&'p Cell<'a>>) {
        let package = self.package;
        let ordered = self.section_cells();
        if ordered.is_empty() {
            return (Vec::new(), package.cells.iter().collect());
        }
        let mut remaining: Vec<Option<&'p Cell<'a>>> = package.cells.iter().map(Some).collect();
        let mut pages = Vec::new();
        let mut covered = HashSet::new();
        for id in ordered {
            let found = remaining
                .iter_mut()
                .find(|cell| cell.is_some_and(|cell| cell.id.key() == id.key()));
            if let Some(cell) = found.and_then(Option::take) {
                pages.push(cell);
                covered.insert(id.second.key());
            }
        }
        let others = remaining
            .into_iter()
            .flatten()
            .filter(|cell| !covered.contains(&cell.id.second.key()))
            .collect();
        (pages, others)
    }

    /// `collectSectionReferencedCells`: the object spaces the data root's objects reference, in
    /// the order a walk from its roots meets them.
    fn section_cells(&mut self) -> Vec<CellId> {
        let package = self.package;
        let objects = Objects::of(&package.data_root.groups);
        let mut visited = HashSet::new();
        let mut cells = Vec::new();
        for &root in &package.data_root.roots {
            let object = objects.get(root).cloned();
            if object.is_none() {
                self.warn(format!(
                    "OneNote section root object {root} could not be resolved"
                ));
            }
            self.collect_cells(object.as_deref(), &objects, &mut visited, &mut cells, 0);
        }
        cells
    }

    fn collect_cells(
        &mut self,
        object: Option<&Object<'a>>,
        objects: &Objects<'a>,
        visited: &mut HashSet<GuidKey>,
        cells: &mut Vec<CellId>,
        depth: usize,
    ) {
        let Some(object) = object.filter(|object| object.property_set.is_some()) else {
            return;
        };
        if depth >= MAX_DEPTH {
            self.warn(format!(
                "OneNote section reference traversal exceeded depth limit {MAX_DEPTH}"
            ));
            return;
        }
        if let Some(id) = object.id {
            if !visited.insert(id.key()) {
                return;
            }
        }
        for action in self.object_actions(object).iter() {
            if let Some(space) = action.space {
                cells.push(space);
            } else if let Some(Some(reference)) = action.child {
                let child = objects.get(reference).cloned();
                if child.is_none() && !self.warnings.saturated() {
                    self.warn(format!(
                        "OneNote section object {reference} could not be resolved"
                    ));
                }
                self.collect_cells(child.as_deref(), objects, visited, cells, depth + 1);
            }
        }
    }

    /// `walkCell`: the objects of a cell from its roots, or all of them where it declares none
    /// or none of its roots that hold properties is found.
    fn walk_cell(&mut self, cell: &Cell<'a>) -> Result<(), Ended> {
        let objects = Objects::of(&cell.groups);
        let mut visited = HashSet::new();
        let (mut resolved_content_root, mut unresolved_root) = (false, false);
        for &root in &cell.roots {
            match objects.get(root).cloned() {
                None => {
                    self.warn(format!(
                        "OneNote cell root object {root} could not be resolved"
                    ));
                    unresolved_root = true;
                }
                Some(object) => {
                    resolved_content_root |= object.property_set.is_some();
                    self.walk_object(
                        Some(&object),
                        &objects,
                        &mut visited,
                        AuthorRole::None,
                        0,
                        None,
                    )?;
                }
            }
        }
        if cell.roots.is_empty() || (unresolved_root && !resolved_content_root) {
            self.warn(if cell.roots.is_empty() {
                "OneNote cell has no declared root objects; walking all objects".to_owned()
            } else {
                "OneNote cell root objects could not be resolved; walking all objects".to_owned()
            });
            for object in cell.groups.iter().flatten() {
                self.walk_object(
                    Some(object),
                    &objects,
                    &mut visited,
                    AuthorRole::None,
                    0,
                    None,
                )?;
            }
        }
        Ok(())
    }

    fn walk_object(
        &mut self,
        object: Option<&Object<'a>>,
        objects: &Objects<'a>,
        visited: &mut HashSet<GuidKey>,
        role: AuthorRole,
        depth: usize,
        inherited: Option<ResourceInfo>,
    ) -> Result<(), Ended> {
        let Some(object) = object else {
            return Ok(());
        };
        if depth >= MAX_DEPTH {
            self.warn(format!(
                "OneNote object traversal exceeded depth limit {MAX_DEPTH}"
            ));
            return Ok(());
        }
        if let Some(id) = object.id {
            if !visited.insert(id.key()) {
                self.record_authors(object, role);
                return Ok(());
            }
        }
        let actions = match object.property_set {
            Some(_) => self.object_actions(object),
            None => Rc::new(Vec::new()),
        };
        let mut info = resource_info(&actions).or(inherited);
        if info.is_none() && object.file_data.is_some() {
            if let Some(id) = object.id {
                info = self.info_from_referencing_object(id, objects, depth);
            }
        }
        if let Some(data) = &object.file_data {
            self.handle_embedded(data.as_deref(), info.clone(), object.id)?;
        }
        if object.property_set.is_none() {
            return Ok(());
        }
        let mut primary_picture = false;
        for action in actions.iter() {
            if let Some(Some(reference)) = action.child {
                if action.id.value == property::PICTURE_CONTAINER
                    && self.has_usable_picture(reference, objects, &mut HashSet::new(), depth)
                {
                    primary_picture = true;
                    break;
                }
            }
        }
        for action in actions.iter() {
            if action.id.value == property::STRUCTURE_ELEMENT_CHILD_NODES {
                self.process(action, objects, visited, role, depth, info.clone())?;
            }
        }
        for action in actions.iter() {
            let id = action.id.value;
            if (primary_picture && id == property::WEB_PICTURE_CONTAINER_14)
                || id == property::STRUCTURE_ELEMENT_CHILD_NODES
            {
                continue;
            }
            self.process(action, objects, visited, role, depth, info.clone())?;
        }
        Ok(())
    }

    fn has_usable_picture(
        &mut self,
        id: ExGuid,
        objects: &Objects<'a>,
        visited: &mut HashSet<GuidKey>,
        depth: usize,
    ) -> bool {
        if depth >= MAX_DEPTH || !visited.insert(id.key()) {
            return false;
        }
        let Some(object) = objects.get(id).cloned() else {
            return false;
        };
        if object
            .file_data
            .as_ref()
            .is_some_and(|data| data.as_ref().is_some_and(|data| !data.is_empty()))
        {
            return true;
        }
        if object.property_set.is_none() {
            return false;
        }
        for action in self.object_actions(&object).iter() {
            if let Some(Some(reference)) = action.child {
                if self.has_usable_picture(reference, objects, visited, depth + 1) {
                    return true;
                }
            }
        }
        false
    }

    /// `resourceInfoFromReferencingObject`: the name and type the object that holds `target`
    /// as a picture or file gives it, in the order Java iterates the objects, or the type of
    /// the first such reference.
    fn info_from_referencing_object(
        &mut self,
        target: ExGuid,
        objects: &Objects<'a>,
        depth: usize,
    ) -> Option<ResourceInfo> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let mut type_only = None;
        for key in &objects.order {
            let candidate = objects.by_id[key].clone();
            if candidate.property_set.is_none() {
                continue;
            }
            let actions = self.object_actions(&candidate);
            for action in actions.iter() {
                let Some(Some(reference)) = action.child else {
                    continue;
                };
                let container = matches!(
                    action.id.value,
                    property::PICTURE_CONTAINER
                        | property::EMBEDDED_FILE_CONTAINER
                        | property::WEB_PICTURE_CONTAINER_14
                );
                if !(reference.same(target) && container) {
                    continue;
                }
                if let Some(info) = resource_info(&actions) {
                    return Some(info);
                }
                if type_only.is_none() {
                    type_only = info_for_child(action, None);
                }
            }
        }
        type_only
    }

    /// `collectObjectActions`, once for each object.
    fn object_actions(&mut self, object: &Object<'a>) -> Rc<Vec<Action<'a>>> {
        let identity: *const Object<'a> = object;
        if let Some(actions) = self.actions.get(&identity) {
            return actions.clone();
        }
        let set = object
            .property_set
            .as_ref()
            .expect("the actions of an object with properties");
        let mut actions = Vec::new();
        let mut cursors = (0usize, 0usize);
        self.collect_actions(set, object, &mut cursors, &mut actions, 0);
        let actions = Rc::new(actions);
        self.actions.insert(identity, actions.clone());
        actions
    }

    fn collect_actions(
        &mut self,
        set: &PropertySet<'a>,
        object: &Object<'a>,
        cursors: &mut (usize, usize),
        actions: &mut Vec<Action<'a>>,
        depth: usize,
    ) {
        if depth >= MAX_DEPTH {
            self.warn(format!(
                "OneNote property traversal exceeded depth limit {MAX_DEPTH}"
            ));
            return;
        }
        for (id, value) in &set.properties {
            match id.kind {
                kind::OBJECT_ID => {
                    let reference = object.references.get(cursors.0).copied();
                    if reference.is_some() {
                        cursors.0 += 1;
                    } else {
                        self.warn("OneNote object reference slot was exhausted".to_owned());
                    }
                    actions.push(action(*id, value, Some(reference), None));
                }
                kind::ARRAY_OF_OBJECT_IDS | kind::ARRAY_OF_OBJECT_SPACE_IDS => {
                    let objects = id.kind == kind::ARRAY_OF_OBJECT_IDS;
                    let (cursor, total) = match objects {
                        true => (cursors.0, object.references.len()),
                        false => (cursors.1, object.space_references.len()),
                    };
                    let available = total as i64 - cursor as i64;
                    let requested = match value {
                        Property::ArrayNumber(number) => (*number).max(0),
                        _ => 0,
                    };
                    let declared = if requested <= 0 || available <= 0 {
                        0
                    } else {
                        i64::from(requested).min(available) as i32
                    };
                    let count = declared.min(MAX_REFERENCES);
                    let (what, unavailable, capped) = match objects {
                        true => (
                            "object",
                            WarningKind::ReferencesUnavailable,
                            WarningKind::ReferencesCapped,
                        ),
                        false => (
                            "object-space",
                            WarningKind::SpaceReferencesUnavailable,
                            WarningKind::SpaceReferencesCapped,
                        ),
                    };
                    if i64::from(requested) > available && !self.warnings.saturated() {
                        self.warn_kind(
                            unavailable,
                            format!(
                                "OneNote {what} reference array had unavailable entries (first \
                                 occurrence: declared {requested} entries but only {} were \
                                 available)",
                                available.max(0)
                            ),
                        );
                    }
                    if declared > count && !self.warnings.saturated() {
                        self.warn_kind(
                            capped,
                            format!(
                                "Capping OneNote {what} reference array at {MAX_REFERENCES} \
                                 entries"
                            ),
                        );
                    }
                    for _ in 0..count {
                        let action = match objects {
                            true => {
                                let reference = object.references.get(cursors.0).copied();
                                cursors.0 += usize::from(reference.is_some());
                                action(*id, value, Some(reference), None)
                            }
                            false => {
                                let space = object.space_references.get(cursors.1).copied();
                                cursors.1 += usize::from(space.is_some());
                                action(*id, value, None, space)
                            }
                        };
                        actions.push(action);
                    }
                    let skipped = (declared - count) as usize;
                    match objects {
                        true => cursors.0 += skipped,
                        false => cursors.1 += skipped,
                    }
                }
                kind::OBJECT_SPACE_ID => {
                    let space = object.space_references.get(cursors.1).copied();
                    if space.is_some() {
                        cursors.1 += 1;
                    } else {
                        self.warn("OneNote object-space reference slot was exhausted".to_owned());
                    }
                    actions.push(action(*id, value, None, space));
                }
                kind::PROPERTY_SET => {
                    if let Property::Set(nested) = value {
                        self.collect_actions(nested, object, cursors, actions, depth + 1);
                    }
                }
                kind::ARRAY_OF_PROPERTY_VALUES => {
                    if let Property::Values(sets) = value {
                        for nested in sets {
                            self.collect_actions(nested, object, cursors, actions, depth + 1);
                        }
                    }
                }
                _ => actions.push(action(*id, value, None, None)),
            }
        }
    }

    fn process(
        &mut self,
        action: &Action<'a>,
        objects: &Objects<'a>,
        visited: &mut HashSet<GuidKey>,
        role: AuthorRole,
        depth: usize,
        parent_info: Option<ResourceInfo>,
    ) -> Result<(), Ended> {
        if action.space.is_some() {
            return Ok(());
        }
        let Some(reference) = action.child else {
            return self.process_primitive(action, role);
        };
        let child_role = match action.id.value {
            property::AUTHOR_MOST_RECENT => AuthorRole::MostRecent,
            property::AUTHOR_ORIGINAL => AuthorRole::Original,
            _ => AuthorRole::None,
        };
        let child_info = info_for_child(action, parent_info);
        let child = reference.and_then(|reference| objects.get(reference).cloned());
        if let (None, Some(reference)) = (&child, reference) {
            if !self.warnings.saturated() {
                self.warn(format!("OneNote object {reference} could not be resolved"));
            }
        }
        self.walk_object(
            child.as_deref(),
            objects,
            visited,
            child_role,
            depth + 1,
            child_info,
        )
    }

    /// `processPrimitiveProperty`.
    fn process_primitive(&mut self, action: &Action<'a>, role: AuthorRole) -> Result<(), Ended> {
        let id = action.id.value;
        match id {
            property::LAST_MODIFIED_TIME_STAMP => {
                let seconds = scalar(action)? / 10_000_000 + DATETIME_EPOCH_DIFF_1601;
                if self
                    .last_modified_timestamp
                    .is_none_or(|last| seconds > last)
                {
                    self.last_modified_timestamp = Some(seconds);
                }
                let millis = self.last_modified_timestamp.unwrap_or(seconds) * 1000;
                self.xhtml
                    .metadata_mut()
                    .set("onenote:lastModifiedTimestamp", millis.to_string());
            }
            property::CREATION_TIME_STAMP => {
                let created = scalar(action)? + TIME32_EPOCH_DIFF_1980;
                self.creation_timestamp = self.creation_timestamp.min(created);
                let value = self.creation_timestamp.to_string();
                self.xhtml
                    .metadata_mut()
                    .set("onenote:creationTimestamp", value);
            }
            property::LAST_MODIFIED_TIME => {
                let modified = scalar(action)? + TIME32_EPOCH_DIFF_1980;
                self.last_modified = self.last_modified.max(modified);
                let value = self.last_modified.to_string();
                self.xhtml.metadata_mut().set("dcterms:modified", value);
            }
            property::AUTHOR => {
                if let Some(author) = action.author() {
                    self.record_author(author, role);
                }
            }
            _ => {
                let PropertyValue::Data(bytes) = action.property.as_ref() else {
                    return Ok(());
                };
                let binary = matches!(
                    id,
                    property::RG_OUTLINE_INDENT_DISTANCE
                        | property::NOTEBOOK_MANAGEMENT_ENTITY_GUID
                        | property::RICH_EDIT_TEXT_UNICODE
                );
                if id == property::TEXT_EXTENDED_ASCII {
                    self.paragraph(&crate::java::ascii(bytes))?;
                } else if !binary {
                    if id == property::IMAGE_FILENAME {
                        self.paragraph(&utf16le(bytes))?;
                    }
                } else if id == property::RICH_EDIT_TEXT_UNICODE {
                    self.rich_edit_text(bytes)?;
                }
            }
        }
        Ok(())
    }

    fn record_author(&mut self, author: &str, role: AuthorRole) {
        let sets = match role {
            AuthorRole::MostRecent => [Some(&mut self.most_recent_authors), None],
            AuthorRole::Original => [Some(&mut self.original_authors), Some(&mut self.authors)],
            AuthorRole::None => [Some(&mut self.authors), None],
        };
        for authors in sets.into_iter().flatten() {
            if !authors.contains(author) {
                authors.insert(author.to_owned());
            }
        }
    }

    /// `recordAuthors`: the authors an object visited again names.
    fn record_authors(&mut self, object: &Object<'a>, role: AuthorRole) {
        if object.property_set.is_none() {
            return;
        }
        for action in self.object_actions(object).iter() {
            if let Some(author) = action.author() {
                self.record_author(author, role);
            }
        }
    }

    /// `handleEmbedded`: a file an object holds, as an embedded document after a div of class
    /// `embedded`.
    fn handle_embedded(
        &mut self,
        data: Option<&[u8]>,
        info: Option<ResourceInfo>,
        id: Option<ExGuid>,
    ) -> Result<(), Ended> {
        let Some(data) = data.filter(|data| !data.is_empty()) else {
            return Ok(());
        };
        let mut metadata = TikaMetadata::new();
        match &info {
            Some(info) => info.kind.set(&mut metadata),
            None => ResourceType::Attachment.set(&mut metadata),
        }
        if let Some(name) = info.as_ref().and_then(|info| info.name.as_deref()) {
            metadata.set(RESOURCE_NAME, name);
        }
        let relationship = id.map(|id| format!("_{}_{}", id.value, id.guid));
        let mut attributes = vec![("class", "embedded")];
        if let Some(relationship) = &relationship {
            metadata.set(RELATIONSHIP_ID, relationship.clone());
            attributes.push(("id", relationship.as_str()));
        }
        self.xhtml.start_element_with("div", &attributes)?;
        self.xhtml.end_element("div")?;
        if embedded::is_parsed(&metadata) {
            self.content = true;
        }
        self.xhtml
            .parse_embedded_opened(data, &metadata, Wrapping::Bare, Opened::Alone)?;
        Ok(())
    }

    /// `handleRichEditTextUnicode`: the text up to the first NUL, or all of it where it has
    /// none, which Tika drops (#129), as a link where it is a hyperlink field.
    fn rich_edit_text(&mut self, bytes: &[u8]) -> Result<(), Ended> {
        let text = up_to_nul(bytes);
        if text.is_empty() {
            return Ok(());
        }
        let text = utf16le(text);
        let link = self
            .hyperlink
            .captures(&text)
            .map(|link| (link[1].to_owned(), link[2].to_owned()));
        match link {
            Some((href, label)) => {
                self.xhtml.start_element_with("a", &[("href", &href)])?;
                let written = self.xhtml.characters(&label);
                self.xhtml.end_element("a")?;
                written?;
                self.content = true;
                Ok(())
            }
            None => self.paragraph(&text),
        }
    }

    /// `emitParagraph`.
    fn paragraph(&mut self, text: &str) -> Result<(), Ended> {
        self.xhtml.start_element("p")?;
        let written = self.xhtml.characters(text);
        self.xhtml.end_element("p")?;
        written?;
        if !is_blank(text) {
            self.content = true;
        }
        Ok(())
    }
}

fn action<'a>(
    id: PropertyId,
    value: &Property<'a>,
    child: Option<Option<ExGuid>>,
    space: Option<CellId>,
) -> Action<'a> {
    let property = match value {
        Property::Data(bytes) => PropertyValue::Data(bytes),
        Property::Fixed(bytes) if id.kind == kind::FOUR_BYTES => PropertyValue::Scalar(i64::from(
            i32::from_le_bytes(bytes[..4].try_into().expect("four bytes")),
        )),
        Property::Fixed(bytes) if id.kind == kind::EIGHT_BYTES => PropertyValue::Scalar(
            i64::from_le_bytes(bytes[..8].try_into().expect("eight bytes")),
        ),
        _ => PropertyValue::Other(class_of(id.kind)),
    };
    Action {
        id,
        property: Rc::new(property),
        child,
        space,
        author: OnceCell::new(),
    }
}

/// `getScalar`.
fn scalar(action: &Action) -> Result<i64, Ended> {
    let class = match action.property.as_ref() {
        PropertyValue::Scalar(value) => return Ok(*value),
        PropertyValue::Data(_) => "PrtFourBytesOfLengthFollowedByData",
        PropertyValue::Other(class) => class,
    };
    Err(Ended::Thrown(thrown(
        TIKA_EXCEPTION,
        format_args!(
            "Could not parse scalar of type class \
             org.apache.tika.parser.microsoft.onenote.fsshttpb.property.{class}"
        ),
    )))
}

/// `decodeOneNoteText`: UTF-16LE without its NULs.
fn one_note_text(bytes: &[u8]) -> String {
    utf16le(bytes).replace('\0', "")
}

/// `embeddedResourceInfo`: the name an object's image or file name property gives its file, and
/// the type of that file.
fn resource_info(actions: &[Action]) -> Option<ResourceInfo> {
    for action in actions {
        let PropertyValue::Data(bytes) = action.property.as_ref() else {
            continue;
        };
        let kind = match action.id.value {
            property::IMAGE_FILENAME => ResourceType::Inline,
            property::EMBEDDED_FILE_NAME => ResourceType::Attachment,
            _ => continue,
        };
        let name = sanitized_name(&one_note_text(bytes));
        if !name.is_empty() {
            return Some(ResourceInfo {
                name: Some(name),
                kind,
            });
        }
    }
    None
}

/// `resourceInfoForChild`: the type of a picture or file a property refers to, unless the
/// object above gave one.
fn info_for_child(action: &Action, parent: Option<ResourceInfo>) -> Option<ResourceInfo> {
    let kind = match action.id.value {
        property::PICTURE_CONTAINER | property::WEB_PICTURE_CONTAINER_14 => ResourceType::Inline,
        property::EMBEDDED_FILE_CONTAINER => ResourceType::Attachment,
        _ => return parent,
    };
    parent.or(Some(ResourceInfo { name: None, kind }))
}

/// `sanitizeResourceName`: the last part of a path, without a drive.
fn sanitized_name(name: &str) -> String {
    let name = name.replace('\\', "/");
    let units: Vec<u16> = name.encode_utf16().collect();
    let name = if units.len() > 1 && units[1] == u16::from(b':') {
        String::from_utf16_lossy(&units[2..])
    } else {
        name
    };
    let last = name.rsplit('/').next().unwrap_or_default();
    match last {
        "." | ".." => String::new(),
        last => last.to_owned(),
    }
}

/// The UTF-16LE units of `bytes` before the first NUL, all of them where there is none.
fn up_to_nul(bytes: &[u8]) -> &[u8] {
    let units = bytes.as_chunks::<2>().0;
    let end = units
        .iter()
        .position(|pair| *pair == [0, 0])
        .unwrap_or(units.len());
    &bytes[..end * 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rich_text_up_to_its_first_nul_or_whole() {
        let utf16 =
            |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() };
        assert_eq!(up_to_nul(&utf16("Havn\0rest")), utf16("Havn"));
        assert_eq!(up_to_nul(&utf16("Bølge")), utf16("Bølge"));
        assert_eq!(up_to_nul(&utf16("\0")), b"");
    }

    #[test]
    fn looks_objects_up_in_the_order_they_are_declared() {
        let object = |value: i32| {
            Rc::new(Object {
                id: Some(ExGuid {
                    kind: 32,
                    value,
                    guid: super::super::Uuid([7; 16]),
                }),
                property_set: None,
                references: Vec::new(),
                space_references: Vec::new(),
                file_data: None,
            })
        };
        let groups = [vec![object(9), object(2)], vec![object(5), object(2)]];
        let values: Vec<i32> = Objects::of(&groups)
            .order
            .iter()
            .map(|(value, _)| *value)
            .collect();
        assert_eq!(values, [9, 2, 5]);
    }

    #[test]
    fn sanitizes_names_as_tika_does() {
        assert_eq!(
            sanitized_name("C:\\Users\\kari\\billede.png"),
            "billede.png"
        );
        assert_eq!(sanitized_name("a/b/.."), "");
        assert_eq!(sanitized_name("fil.txt"), "fil.txt");
    }

    /// An action's author is its data decoded as `decodeOneNoteText` decodes it, each time it is
    /// asked for, and none for another property or an author of another type.
    #[test]
    fn decodes_an_actions_author_once_as_one_note_text() {
        let author = PropertyId {
            kind: kind::DATA,
            value: property::AUTHOR,
        };
        let name = PropertyId {
            kind: kind::DATA,
            value: property::IMAGE_FILENAME,
        };
        for text in ["", "Kari", "K\0a\0ri", "\0", "Bølge 🌊"] {
            let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            for bytes in [&bytes[..], &bytes[..bytes.len().saturating_sub(1)]] {
                let decoded = action(author, &Property::Data(bytes), None, None);
                let expected = one_note_text(bytes);
                assert_eq!(decoded.author(), Some(expected.as_str()), "{bytes:?}");
                assert_eq!(decoded.author(), Some(expected.as_str()), "{bytes:?}");
                assert_eq!(
                    action(name, &Property::Data(bytes), None, None).author(),
                    None
                );
            }
        }
        let fixed = PropertyId {
            kind: kind::FOUR_BYTES,
            value: property::AUTHOR,
        };
        let scalar = action(fixed, &Property::Fixed(vec![1, 0, 0, 0]), None, None);
        assert_eq!(scalar.author(), None);
    }
}
