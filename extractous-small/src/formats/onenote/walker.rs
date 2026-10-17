//! Tika's `OneNoteTreeWalker` with its default options: every file node from the root on, in
//! order, each object a property refers to walked from that property, a cycle or a walk too
//! deep cut off with a warning. It writes the image file names, the plain ASCII texts and the
//! first run of each rich text's UTF-16LE text, a hyperlink as a link, and hands each file data
//! store object to the embedded document extractor; and it collects the authors and times.

use std::collections::HashSet;

use regex::Regex;

use super::file::{memory_limit, Dif, Thrown};
use super::guid::ExtendedGuid;
use super::structure::{Chunk, Document, FileNode, FileNodeList, PropertyValue, Ptr};
use crate::formats::embedded::{self, ResourceType, Wrapping, RELATIONSHIP_ID};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Failure, Opened};

const MAX_CYCLE_WARNINGS: usize = 100;
const MAX_WALK_DEPTH: usize = 500;
/// Seconds from 1970 back to 1980, and to 1601.
const TIME32_EPOCH_DIFF_1980: i64 = 315_532_800;
const DATETIME_EPOCH_DIFF_1601: i64 = -11_644_473_600;
const EXCEPTION_WARNING: &str = embedded::EXCEPTION_WARNING;
/// `HYPERLINK_PATTERN`, with Java's `\s`.
pub(super) const HYPERLINK: &str = "\u{FDDF}HYPERLINK[ \t\n\x0B\x0C\r]+\"([^\"]+)\"([^\"]+)$";

/// The property IDs `OneNotePropertyEnum.of` names that the walk tells apart. Every
/// `ElementChildNodesOf...` shares one ID, which the enum's map gives the last of them,
/// `ElementChildNodesOfVersionHistory`.
const RG_OUTLINE_INDENT_DISTANCE: i64 = 0x1C00_1C12;
const ELEMENT_CHILD_NODES_OF_VERSION_HISTORY: i64 = 0x2400_1C20;
const RICH_EDIT_TEXT_UNICODE: i64 = 0x1C00_1C22;
const NOTEBOOK_MANAGEMENT_ENTITY_GUID: i64 = 0x1C00_1C30;
const CACHED_TITLE_STRING: i64 = 0x1C00_1CF3;
const CREATION_TIME_STAMP: i64 = 0x1400_1D09;
const AUTHOR: i64 = 0x1C00_1D75;
const LAST_MODIFIED_TIME_STAMP: i64 = 0x1800_1D77;
const AUTHOR_ORIGINAL: i64 = 0x2000_1D78;
const AUTHOR_MOST_RECENT: i64 = 0x2000_1D79;
const LAST_MODIFIED_TIME: i64 = 0x1400_1D7A;
const IMAGE_FILENAME: i64 = 0x1C00_1DD7;
const TEXT_EXTENDED_ASCII: i64 = 0x1C00_3498;

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
    pub(super) authors: Vec<String>,
    pub(super) most_recent_authors: Vec<String>,
    pub(super) original_authors: Vec<String>,
    /// `lastModifiedTimestamp`, in seconds, if any.
    pub(super) last_modified_timestamp: Option<i64>,
    pub(super) creation_timestamp: i64,
    pub(super) last_modified: i64,
}

pub(super) struct Walker<'w, 'a, 'x> {
    document: &'w Document,
    dif: &'w mut Dif<'a>,
    xhtml: &'w mut Xhtml<'x>,
    hyperlink: Regex,
    found: Found,
    most_recent_author_prop: bool,
    original_author_prop: bool,
    text_already_fetched: HashSet<(i64, i32)>,
    active: HashSet<*const FileNode>,
    reported_cycles: HashSet<*const FileNode>,
    cycle_warnings: usize,
    cycle_warnings_suppressed: bool,
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
            document,
            dif,
            xhtml,
            hyperlink: Regex::new(HYPERLINK).expect("the hyperlink pattern is valid"),
            found: Found {
                creation_timestamp: i64::MAX,
                last_modified: i64::MIN,
                ..Found::default()
            },
            most_recent_author_prop: false,
            original_author_prop: false,
            text_already_fetched: HashSet::new(),
            active: HashSet::new(),
            reported_cycles: HashSet::new(),
            cycle_warnings: 0,
            cycle_warnings_suppressed: false,
            depth: 0,
            depth_warning_recorded: false,
        }
    }

    /// `walkTree`: the root file node list walked, and what it found.
    pub(super) fn walk(mut self) -> Result<Found, Ended> {
        let document = self.document;
        self.walk_list(&document.root, None)?;
        Ok(self.found)
    }

    fn walk_list(&mut self, list: &'w FileNodeList, parent: Option<i64>) -> Result<(), Ended> {
        for child in &list.children {
            self.walk_node(child, parent)?;
        }
        Ok(())
    }

    fn walk_node(&mut self, node: &'w FileNode, parent: Option<i64>) -> Result<(), Ended> {
        if self.depth >= MAX_WALK_DEPTH {
            self.record_depth_warning();
            return Ok(());
        }
        let identity: *const FileNode = node;
        if !self.active.insert(identity) {
            self.record_cycle_warning(node);
            return Ok(());
        }
        self.depth += 1;
        let walked = self.walk_node_internal(node, parent);
        self.depth -= 1;
        self.active.remove(&identity);
        walked
    }

    fn record_depth_warning(&mut self) {
        if self.depth_warning_recorded {
            return;
        }
        self.depth_warning_recorded = true;
        self.warn(format!(
            "OneNote file-node traversal exceeded depth limit {MAX_WALK_DEPTH}"
        ));
    }

    fn record_cycle_warning(&mut self, node: &FileNode) {
        let identity: *const FileNode = node;
        if self.reported_cycles.contains(&identity) {
            return;
        }
        if self.cycle_warnings >= MAX_CYCLE_WARNINGS {
            if !self.cycle_warnings_suppressed {
                self.cycle_warnings_suppressed = true;
                self.warn(format!(
                    "Additional OneNote file-node cycle warnings were suppressed after \
                     {MAX_CYCLE_WARNINGS} distinct cycles"
                ));
            }
            return;
        }
        self.reported_cycles.insert(identity);
        self.cycle_warnings += 1;
        self.warn(format!(
            "OneNote file-node cycle detected at {}",
            node.gosid
        ));
    }

    fn warn(&mut self, warning: String) {
        self.xhtml.metadata_mut().add(EXCEPTION_WARNING, warning);
    }

    fn walk_node_internal(&mut self, node: &'w FileNode, parent: Option<i64>) -> Result<(), Ended> {
        if node.child_list.has_header {
            self.walk_list(&node.child_list, parent)?;
        }
        if let Some(properties) = &node.property_set {
            self.process_property_set(properties, parent)?;
        }
        if let Some(file_data) = node.file_data.filter(|&data| data != Chunk::NIL) {
            self.walk_file_data(file_data, node.gosid)?;
        }
        Ok(())
    }

    /// `walkFileDataStoreObjectReference`: the file data handed to the embedded document
    /// extractor.
    fn walk_file_data(&mut self, file_data: Chunk, gosid: ExtendedGuid) -> Result<(), Ended> {
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
        self.handle_embedded(file_data.cb as i32, gosid)
    }

    fn handle_embedded(&mut self, length: i32, gosid: ExtendedGuid) -> Result<(), Ended> {
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
        if gosid != ExtendedGuid::NIL {
            metadata.set(RELATIONSHIP_ID, gosid.to_string());
        }
        self.xhtml
            .start_element_with("div", &[("class", "embedded")])?;
        self.xhtml.end_element("div")?;
        self.xhtml
            .parse_embedded_opened(data, &metadata, Wrapping::Bare, Opened::Alone)?;
        Ok(())
    }

    fn process_property_set(
        &mut self,
        properties: &'w [PropertyValue],
        parent: Option<i64>,
    ) -> Result<(), Ended> {
        for value in properties {
            self.process_property_value(value, parent)?;
        }
        Ok(())
    }

    fn process_property_value(
        &mut self,
        value: &'w PropertyValue,
        parent: Option<i64>,
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
                if created < self.found.creation_timestamp {
                    self.found.creation_timestamp = created;
                }
            }
            LAST_MODIFIED_TIME => {
                let modified = value.scalar + TIME32_EPOCH_DIFF_1980;
                if modified > self.found.last_modified {
                    self.found.last_modified = modified;
                }
            }
            AUTHOR => {
                let author = self.read_utf16(value.raw_data)?;
                let authors = if self.most_recent_author_prop {
                    &mut self.found.most_recent_authors
                } else if self.original_author_prop {
                    &mut self.found.original_authors
                } else {
                    &mut self.found.authors
                };
                if !authors.contains(&author) {
                    authors.push(author);
                }
                self.most_recent_author_prop = false;
                self.original_author_prop = false;
            }
            AUTHOR_MOST_RECENT => self.most_recent_author_prop = true,
            AUTHOR_ORIGINAL => self.original_author_prop = true,
            _ if (1..=6).contains(&value.id.kind) => {}
            _ => self.process_data(value, parent)?,
        }
        for guid in &value.compact_ids {
            if let Some(path) = self.document.guid_to_object.get(guid) {
                if let Some(node) = self.document.dereference(path)? {
                    self.walk_node(node, Some(pid))?;
                }
            }
        }
        self.process_property_set(&value.property_set, parent)
    }

    /// The data of a property that is neither a number nor a known author or time: read,
    /// and written where it is an image's file name or an ASCII text, unless it is binary,
    /// where only a rich text or a title is read, once, if no version history holds it.
    fn process_data(&mut self, value: &PropertyValue, parent: Option<i64>) -> Result<(), Ended> {
        let pid = value.id.pid;
        let mut content = Ptr::at(self.dif);
        content.reposition(self.dif, value.raw_data)?;
        let size = content.size();
        let binary = matches!(
            pid,
            RG_OUTLINE_INDENT_DISTANCE
                | NOTEBOOK_MANAGEMENT_ENTITY_GUID
                | RICH_EDIT_TEXT_UNICODE
                | CACHED_TITLE_STRING
        );
        self.check_size(size)?;
        if binary {
            let shown =
                parent.is_some_and(|parent| parent != ELEMENT_CHILD_NODES_OF_VERSION_HISTORY);
            if (pid == RICH_EDIT_TEXT_UNICODE || pid == CACHED_TITLE_STRING) && shown {
                self.handle_rich_edit_text_unicode(size)?;
            }
            return Ok(());
        }
        let data = self.dif.read(allocation(size)?)?;
        if pid == TEXT_EXTENDED_ASCII {
            self.paragraph(&crate::java::ascii(data))?;
        } else if pid == IMAGE_FILENAME {
            self.paragraph(&crate::java::utf16le(data))?;
        }
        Ok(())
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

    fn paragraph(&mut self, text: &str) -> Result<(), Ended> {
        self.xhtml.start_element("p")?;
        self.element_text("p", text)
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

    /// `handleRichEditTextUnicode`: the text up to the first NUL, once for each place, as a
    /// link where it is a hyperlink field.
    fn handle_rich_edit_text_unicode(&mut self, length: i32) -> Result<(), Ended> {
        if !self
            .text_already_fetched
            .insert((self.dif.position(), length))
        {
            return Ok(());
        }
        let data = self.dif.read(allocation(length)?)?;
        let first_null = data
            .as_chunks::<2>()
            .0
            .iter()
            .position(|pair| *pair == [0, 0])
            .map_or(0, |pair| pair * 2);
        if first_null == 0 {
            return Ok(());
        }
        let text = crate::java::utf16le(&data[..first_null]);
        match self.hyperlink.captures(&text) {
            Some(link) => {
                self.xhtml.start_element_with("a", &[("href", &link[1])])?;
                self.element_text("a", &link[2])?;
            }
            None => self.paragraph(&text)?,
        }
        Ok(())
    }
}

/// `ByteBuffer.allocate(length)`, which fails on a negative length.
fn allocation(length: i32) -> Result<usize, Thrown> {
    usize::try_from(length).map_err(|_| {
        Thrown(format!(
            "java.lang.IllegalArgumentException: capacity < 0: ({length} < 0)"
        ))
    })
}
