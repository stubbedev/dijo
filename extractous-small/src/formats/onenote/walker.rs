//! The walk of a OneNote section's revision store, as MS-ONESTORE describes it: from the root
//! object space on, the current revision of each object space, its objects walked once each
//! from the roots of the revision along the references of their properties, in order, and each
//! object space they reference walked where they reference it, the title of a page before its
//! content. It writes the image file names, the plain ASCII texts and the first run of each rich
//! text's UTF-16LE text, each a paragraph, a hyperlink as a link in it, and hands the file data
//! an object refers to to the embedded document extractor where the object stands; and it
//! collects the authors and times.
//!
//! Tika's `OneNoteTreeWalker` walks every file node of every revision instead, the objects of
//! each property taken from the start of their streams, which writes the text of the earlier
//! revisions and the same text many times, misses other text, writes the cached titles of the
//! pages besides their titles, a link outside any paragraph, and the file data at the end
//! (#122).

use std::collections::{BTreeMap, HashMap, HashSet};

use regex::Regex;

use super::file::{memory_limit, tika, Dif, Thrown};
use super::guid::{ExtendedGuid, Guid};
use super::structure::{Chunk, Declares, Document, FileNode, FileNodeList, PropertyValue, Ptr};
use crate::formats::embedded::{self, ResourceType, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Failure, Opened};

const MAX_WALK_DEPTH: usize = 500;
/// Seconds from 1970 back to 1980, and to 1601.
const TIME32_EPOCH_DIFF_1980: i64 = 315_532_800;
const DATETIME_EPOCH_DIFF_1601: i64 = -11_644_473_600;
const EXCEPTION_WARNING: &str = embedded::EXCEPTION_WARNING;
/// `HYPERLINK_PATTERN`, with Java's `\s`.
pub(super) const HYPERLINK: &str = "\u{FDDF}HYPERLINK[ \t\n\x0B\x0C\r]+\"([^\"]+)\"([^\"]+)$";
/// The role of the revision that holds an object space's content, in the default context.
const DEFAULT_CONTENT_ROLE: i64 = 0x1;

/// The property IDs the walk tells apart.
const RICH_EDIT_TEXT_UNICODE: i64 = 0x1C00_1C22;
const CREATION_TIME_STAMP: i64 = 0x1400_1D09;
const STRUCTURE_ELEMENT_CHILD_NODES: i64 = 0x2400_1D5F;
const AUTHOR: i64 = 0x1C00_1D75;
const LAST_MODIFIED_TIME_STAMP: i64 = 0x1800_1D77;
const AUTHOR_ORIGINAL: i64 = 0x2000_1D78;
const AUTHOR_MOST_RECENT: i64 = 0x2000_1D79;
const LAST_MODIFIED_TIME: i64 = 0x1400_1D7A;
const IMAGE_FILENAME: i64 = 0x1C00_1DD7;
const TEXT_EXTENDED_ASCII: i64 = 0x1C00_3498;

/// The property types the walk tells apart.
const DATA: i64 = 0x7;
const OBJECT_ID: i64 = 0x8;
const ARRAY_OF_OBJECT_IDS: i64 = 0x9;
const OBJECT_SPACE_ID: i64 = 0xA;
const ARRAY_OF_OBJECT_SPACE_IDS: i64 = 0xB;

/// Why a walk ended early: an exception, after which Tika falls back to the legacy dump, or a
/// failure of the output or of an embedded document, which ends the document.
pub(super) enum Ended {
    Thrown(Thrown),
    Failed(Failure),
}

impl From<Thrown> for Ended {
    fn from(thrown: Thrown) -> Self {
        Ended::Thrown(thrown)
    }
}

impl From<Failure> for Ended {
    fn from(failure: Failure) -> Self {
        Ended::Failed(failure)
    }
}

impl From<Stop> for Ended {
    fn from(stop: Stop) -> Self {
        Ended::Failed(Failure::Stopped(stop))
    }
}

/// What a walk found of the document's authors and times.
#[derive(Debug, Default)]
pub(super) struct Found {
    /// Every author, whatever the role, where Tika's are those it finds in none.
    pub(super) authors: Vec<String>,
    pub(super) most_recent_authors: Vec<String>,
    pub(super) original_authors: Vec<String>,
    /// `lastModifiedTimestamp`, in seconds, if any.
    pub(super) last_modified_timestamp: Option<i64>,
    /// The earliest creation time, if any, where Tika gives `Long.MAX_VALUE` for none (#122).
    pub(super) creation_timestamp: Option<i64>,
    pub(super) last_modified: Option<i64>,
}

/// Whose author an object's `Author` property names: the property that referenced the object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthorRole {
    Unnamed,
    MostRecent,
    Original,
}

impl AuthorRole {
    fn of_reference(pid: i64) -> Self {
        match pid {
            AUTHOR_MOST_RECENT => AuthorRole::MostRecent,
            AUTHOR_ORIGINAL => AuthorRole::Original,
            _ => AuthorRole::Unnamed,
        }
    }
}

/// The revision store of a section: the object space it starts from, the manifest list of each
/// object space, and where the data of each file data store object is.
struct Store<'d> {
    root: Option<ExtendedGuid>,
    spaces: HashMap<ExtendedGuid, &'d FileNodeList>,
    files: HashMap<Guid, Chunk>,
}

impl<'d> Store<'d> {
    fn of(document: &'d Document) -> Self {
        let mut store = Store {
            root: None,
            spaces: HashMap::new(),
            files: HashMap::new(),
        };
        for node in &document.root.children {
            match node.declares {
                Declares::RootObjectSpace(id) => {
                    store.root.get_or_insert(id);
                }
                Declares::ObjectSpace(id) => {
                    store.spaces.entry(id).or_insert(&node.child_list);
                }
                Declares::FileDataStoreList => {
                    for object in &node.child_list.children {
                        if let Declares::FileDataStoreObject { id, data } = object.declares {
                            store.files.entry(id).or_insert(data);
                        }
                    }
                }
                _ => {}
            }
        }
        store
    }
}

/// An object of a revision.
#[derive(Debug, Clone, Copy)]
enum Object<'d> {
    Properties(&'d [PropertyValue]),
    /// File data, in the file data store object named, if the section holds it.
    FileData(Option<Guid>),
}

/// What a revision manifest declares: the objects and roots it adds to those of the revision it
/// depends on.
struct Manifest<'d> {
    dependent: ExtendedGuid,
    objects: Vec<(ExtendedGuid, Object<'d>)>,
    roots: Vec<(i64, ExtendedGuid)>,
}

impl<'d> Manifest<'d> {
    /// What `node` and the file node list it references declare.
    fn collect(&mut self, node: &'d FileNode) {
        match &node.declares {
            Declares::Object { id, properties } => {
                self.objects.push((*id, Object::Properties(properties)));
            }
            Declares::FileDataObject { id, store } => {
                self.objects.push((*id, Object::FileData(*store)));
            }
            Declares::Root { object, role } => self.roots.push((*role, *object)),
            _ => {}
        }
        for child in &node.child_list.children {
            self.collect(child);
        }
    }
}

/// The current revision of an object space: its objects, and its roots by role.
struct Space<'d> {
    objects: HashMap<ExtendedGuid, Object<'d>>,
    roots: BTreeMap<i64, ExtendedGuid>,
}

impl<'d> Space<'d> {
    /// The revision of the object space whose manifest list `list` is that has the default
    /// content role in the default context last, with the objects and roots of the revisions it
    /// depends on that it does not replace; none where no revision has that role.
    fn current(list: &'d FileNodeList) -> Result<Option<Self>, Thrown> {
        let Some(revisions) = list
            .children
            .iter()
            .rev()
            .find(|node| matches!(node.declares, Declares::RevisionManifestList))
        else {
            return Ok(None);
        };
        let mut manifests: HashMap<ExtendedGuid, Manifest> = HashMap::new();
        let mut roles = HashMap::new();
        let mut open = None;
        for node in &revisions.child_list.children {
            match &node.declares {
                Declares::Revision { role, dependent } => {
                    manifests.insert(
                        role.revision,
                        Manifest {
                            dependent: *dependent,
                            objects: Vec::new(),
                            roots: Vec::new(),
                        },
                    );
                    roles.insert((role.context, role.role), role.revision);
                    open = Some(role.revision);
                }
                Declares::RevisionRole(role) => {
                    roles.insert((role.context, role.role), role.revision);
                }
                _ if node.ends_revision_manifest() => open = None,
                _ => {
                    if let Some(manifest) = open.and_then(|id| manifests.get_mut(&id)) {
                        manifest.collect(node);
                    }
                }
            }
        }
        let Some(&current) = roles.get(&(ExtendedGuid::NIL, DEFAULT_CONTENT_ROLE)) else {
            return Ok(None);
        };
        let mut chain = Vec::new();
        let mut at = current;
        while at != ExtendedGuid::NIL {
            if chain.len() > manifests.len() {
                return Err(tika(format!(
                    "OneNote revision {current} depends on itself"
                )));
            }
            let manifest = manifests
                .get(&at)
                .ok_or_else(|| tika(format!("Missing OneNote revision {at}")))?;
            chain.push(manifest);
            at = manifest.dependent;
        }
        let mut space = Space {
            objects: HashMap::new(),
            roots: BTreeMap::new(),
        };
        for manifest in chain.into_iter().rev() {
            space.objects.extend(manifest.objects.iter().copied());
            space.roots.extend(manifest.roots.iter().copied());
        }
        Ok(Some(space))
    }
}

pub(super) struct Walker<'w, 'a, 'x> {
    store: Store<'w>,
    dif: &'w mut Dif<'a>,
    xhtml: &'w mut Xhtml<'x>,
    hyperlink: Regex,
    found: Found,
    walked_spaces: HashSet<ExtendedGuid>,
    depth: usize,
    depth_warning_recorded: bool,
}

impl<'w, 'a, 'x> Walker<'w, 'a, 'x> {
    pub(super) fn new(
        document: &'w Document,
        dif: &'w mut Dif<'a>,
        xhtml: &'w mut Xhtml<'x>,
    ) -> Self {
        Walker {
            store: Store::of(document),
            dif,
            xhtml,
            hyperlink: Regex::new(HYPERLINK).expect("the hyperlink pattern is valid"),
            found: Found::default(),
            walked_spaces: HashSet::new(),
            depth: 0,
            depth_warning_recorded: false,
        }
    }

    /// The walk from the root object space, and what it found.
    pub(super) fn walk(mut self) -> Result<Found, Ended> {
        let root = self
            .store
            .root
            .ok_or_else(|| tika("OneNote section declares no root object space"))?;
        self.walk_space(root)?;
        Ok(self.found)
    }

    /// The objects of the current revision of object space `id` from its roots, in the order of
    /// their roles, unless it was walked before.
    fn walk_space(&mut self, id: ExtendedGuid) -> Result<(), Ended> {
        if !self.walked_spaces.insert(id) {
            return Ok(());
        }
        let Some(list) = self.store.spaces.get(&id).copied() else {
            self.warn(format!("OneNote object space {id} could not be resolved"));
            return Ok(());
        };
        let Some(space) = Space::current(list)? else {
            self.warn(format!("OneNote object space {id} has no current revision"));
            return Ok(());
        };
        let mut visited = HashSet::new();
        for &root in space.roots.values() {
            self.walk_object(&space, &mut visited, root, AuthorRole::Unnamed)?;
        }
        Ok(())
    }

    /// Object `id` of `space`, unless it was walked before, when only the authors it names are
    /// recorded for `role`.
    fn walk_object(
        &mut self,
        space: &Space<'w>,
        visited: &mut HashSet<ExtendedGuid>,
        id: ExtendedGuid,
        role: AuthorRole,
    ) -> Result<(), Ended> {
        let Some(object) = space.objects.get(&id).copied() else {
            self.warn(format!("OneNote object {id} could not be resolved"));
            return Ok(());
        };
        if self.depth >= MAX_WALK_DEPTH {
            self.record_depth_warning();
            return Ok(());
        }
        if !visited.insert(id) {
            if let Object::Properties(properties) = object {
                self.record_authors(properties, role)?;
            }
            return Ok(());
        }
        self.depth += 1;
        let walked = match object {
            Object::Properties(properties) => self.process_object(space, visited, properties, role),
            Object::FileData(store) => self.walk_file_data(store),
        };
        self.depth -= 1;
        walked
    }

    fn record_depth_warning(&mut self) {
        if self.depth_warning_recorded {
            return;
        }
        self.depth_warning_recorded = true;
        self.warn(format!(
            "OneNote object traversal exceeded depth limit {MAX_WALK_DEPTH}"
        ));
    }

    fn warn(&mut self, warning: String) {
        self.xhtml.metadata_mut().add(EXCEPTION_WARNING, warning);
    }

    /// The file data a file data object names, handed to the embedded document extractor;
    /// nothing for a file the section does not hold.
    fn walk_file_data(&mut self, store: Option<Guid>) -> Result<(), Ended> {
        let Some(guid) = store else {
            return Ok(());
        };
        let Some(&file_data) = self.store.files.get(&guid) else {
            self.warn(format!(
                "OneNote file data store object {guid} could not be resolved"
            ));
            return Ok(());
        };
        let mut content = Ptr::at(self.dif);
        content.reposition(self.dif, file_data)?;
        if file_data.cb > self.dif.size() {
            return Err(memory_limit(format!(
                "File data store cb {} exceeds document size: {}",
                file_data.cb,
                self.dif.size()
            ))
            .into());
        }
        self.handle_embedded(file_data.cb as i32)
    }

    fn handle_embedded(&mut self, length: i32) -> Result<(), Ended> {
        let length = allocation(length)?;
        let data = match self.dif.read(length) {
            Ok(data) => data,
            Err(Thrown(exception)) => {
                embedded::record_stream_exception(self.xhtml.metadata_mut(), &exception);
                return Ok(());
            }
        };
        let mut metadata = TikaMetadata::new();
        ResourceType::Attachment.set(&mut metadata);
        self.xhtml
            .start_element_with("div", &[("class", "embedded")])?;
        self.xhtml.end_element("div")?;
        self.xhtml
            .parse_embedded_opened(data, &metadata, Wrapping::Bare, Opened::Alone)?;
        Ok(())
    }

    /// The properties of an object, the structure elements a page holds its title in first.
    fn process_object(
        &mut self,
        space: &Space<'w>,
        visited: &mut HashSet<ExtendedGuid>,
        properties: &'w [PropertyValue],
        role: AuthorRole,
    ) -> Result<(), Ended> {
        let (structure, rest): (Vec<_>, Vec<_>) = properties
            .iter()
            .partition(|value| value.id.pid == STRUCTURE_ELEMENT_CHILD_NODES);
        for value in structure.into_iter().chain(rest) {
            self.process_property_value(space, visited, value, role)?;
        }
        Ok(())
    }

    fn process_property_set(
        &mut self,
        space: &Space<'w>,
        visited: &mut HashSet<ExtendedGuid>,
        properties: &'w [PropertyValue],
        role: AuthorRole,
    ) -> Result<(), Ended> {
        for value in properties {
            self.process_property_value(space, visited, value, role)?;
        }
        Ok(())
    }

    /// A property: its time or author recorded, its text written, the objects or object spaces
    /// it references walked, and the property sets it holds processed.
    fn process_property_value(
        &mut self,
        space: &Space<'w>,
        visited: &mut HashSet<ExtendedGuid>,
        value: &'w PropertyValue,
        role: AuthorRole,
    ) -> Result<(), Ended> {
        let pid = value.id.pid;
        match pid {
            LAST_MODIFIED_TIME_STAMP => {
                let seconds = value.scalar / 10_000_000 + DATETIME_EPOCH_DIFF_1601;
                if self
                    .found
                    .last_modified_timestamp
                    .is_none_or(|last| seconds > last)
                {
                    self.found.last_modified_timestamp = Some(seconds);
                }
            }
            CREATION_TIME_STAMP => {
                let created = value.scalar + TIME32_EPOCH_DIFF_1980;
                if self
                    .found
                    .creation_timestamp
                    .is_none_or(|earliest| created < earliest)
                {
                    self.found.creation_timestamp = Some(created);
                }
            }
            LAST_MODIFIED_TIME => {
                let modified = value.scalar + TIME32_EPOCH_DIFF_1980;
                if self
                    .found
                    .last_modified
                    .is_none_or(|latest| modified > latest)
                {
                    self.found.last_modified = Some(modified);
                }
            }
            AUTHOR => {
                let author = self.read_utf16(value.raw_data)?;
                self.record_author(author, role);
            }
            _ if value.id.kind == DATA => self.process_data(value)?,
            _ => {}
        }
        match value.id.kind {
            OBJECT_ID | ARRAY_OF_OBJECT_IDS => {
                let child_role = AuthorRole::of_reference(pid);
                for &id in &value.compact_ids {
                    self.walk_object(space, visited, id, child_role)?;
                }
            }
            OBJECT_SPACE_ID | ARRAY_OF_OBJECT_SPACE_IDS => {
                for &id in &value.compact_ids {
                    self.walk_space(id)?;
                }
            }
            _ => {}
        }
        self.process_property_set(space, visited, &value.property_set, role)
    }

    /// An author among the authors, and among those of `role`.
    fn record_author(&mut self, author: String, role: AuthorRole) {
        let of_role = match role {
            AuthorRole::MostRecent => Some(&mut self.found.most_recent_authors),
            AuthorRole::Original => Some(&mut self.found.original_authors),
            AuthorRole::Unnamed => None,
        };
        for authors in of_role.into_iter().chain([&mut self.found.authors]) {
            if !authors.contains(&author) {
                authors.push(author.clone());
            }
        }
    }

    /// The authors the properties of an object walked before name, recorded for `role`.
    fn record_authors(
        &mut self,
        properties: &[PropertyValue],
        role: AuthorRole,
    ) -> Result<(), Thrown> {
        for value in properties.iter().filter(|value| value.id.pid == AUTHOR) {
            let author = self.read_utf16(value.raw_data)?;
            self.record_author(author, role);
        }
        Ok(())
    }

    /// The text of a property, written as a paragraph where it has one: a rich text that is a
    /// hyperlink field as a link in it, where Tika writes the link alone, which runs into the
    /// paragraph after it in the text (#122).
    fn process_data(&mut self, value: &PropertyValue) -> Result<(), Ended> {
        let pid = value.id.pid;
        let mut content = Ptr::at(self.dif);
        content.reposition(self.dif, value.raw_data)?;
        let size = content.size();
        self.check_size(size)?;
        let data = self.dif.read(allocation(size)?)?;
        let Some(text) = property_text(pid, data) else {
            return Ok(());
        };
        let link = (pid == RICH_EDIT_TEXT_UNICODE)
            .then(|| self.hyperlink.captures(&text))
            .flatten();
        self.xhtml.start_element("p")?;
        let Some(link) = link else {
            return self.element_text("p", &text);
        };
        let linked = self
            .xhtml
            .start_element_with("a", &[("href", &link[1])])
            .map_err(Ended::from)
            .and_then(|()| self.element_text("a", &link[2]));
        self.xhtml.end_element("p")?;
        linked
    }

    fn check_size(&self, size: i32) -> Result<(), Thrown> {
        if i64::from(size) > self.dif.size() {
            return Err(memory_limit(format!(
                "File data store cb {size} exceeds document size: {}",
                self.dif.size()
            )));
        }
        Ok(())
    }

    /// The text of the element `name` started, which ends even where the text stops the
    /// output, as Tika ends it in a `finally` block.
    fn element_text(&mut self, name: &str, text: &str) -> Result<(), Ended> {
        let written = self.xhtml.characters(text);
        self.xhtml.end_element(name)?;
        written?;
        Ok(())
    }

    /// `getAuthor`: the property's data as UTF-16LE.
    fn read_utf16(&mut self, raw_data: Chunk) -> Result<String, Thrown> {
        let mut content = Ptr::at(self.dif);
        content.reposition(self.dif, raw_data)?;
        let size = content.size();
        self.check_size(size)?;
        let data = self.dif.read(allocation(size)?)?;
        Ok(crate::java::utf16le(data))
    }
}

/// The text a property of `pid` with `data` holds, if it holds one the walk writes: a rich text
/// up to its first NUL, none where it has none or starts with one, a plain ASCII text, or an
/// image's file name.
fn property_text(pid: i64, data: &[u8]) -> Option<String> {
    match pid {
        RICH_EDIT_TEXT_UNICODE => {
            let first_null = data
                .as_chunks::<2>()
                .0
                .iter()
                .position(|pair| *pair == [0, 0])
                .filter(|&pair| pair > 0)?;
            Some(crate::java::utf16le(&data[..first_null * 2]))
        }
        TEXT_EXTENDED_ASCII => Some(crate::java::ascii(data)),
        IMAGE_FILENAME => Some(crate::java::utf16le(data)),
        _ => None,
    }
}

/// The texts the objects of a section declare: those of every revision, and those of the
/// current revisions of its object spaces.
#[cfg(all(test, feature = "tika"))]
pub(super) fn declared_texts(
    document: &Document,
    input: &[u8],
) -> (HashSet<String>, HashSet<String>) {
    fn texts(properties: &[PropertyValue], input: &[u8], out: &mut HashSet<String>) {
        for value in properties {
            if value.id.kind == DATA {
                let start = value.raw_data.stp as usize;
                let data = &input[start..start + value.raw_data.cb as usize];
                out.extend(property_text(value.id.pid, data));
            }
            texts(&value.property_set, input, out);
        }
    }
    fn declared(nodes: &FileNodeList, input: &[u8], out: &mut HashSet<String>) {
        for node in &nodes.children {
            if let Declares::Object { properties, .. } = &node.declares {
                texts(properties, input, out);
            }
            declared(&node.child_list, input, out);
        }
    }
    let store = Store::of(document);
    let (mut all, mut current) = (HashSet::new(), HashSet::new());
    for list in store.spaces.values() {
        declared(list, input, &mut all);
        let space = Space::current(list).unwrap().unwrap();
        for object in space.objects.values() {
            if let Object::Properties(properties) = object {
                texts(properties, input, &mut current);
            }
        }
    }
    (all, current)
}

/// `ByteBuffer.allocate(length)`, which fails on a negative length.
fn allocation(length: i32) -> Result<usize, Thrown> {
    usize::try_from(length).map_err(|_| {
        Thrown(format!(
            "java.lang.IllegalArgumentException: capacity < 0: ({length} < 0)"
        ))
    })
}
