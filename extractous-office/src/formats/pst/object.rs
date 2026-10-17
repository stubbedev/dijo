//! java-libpst's objects: `PSTObject`'s property accessors, and the folders, messages,
//! recipients and attachments Tika reads.

use std::cell::OnceCell;
use std::collections::HashMap;

use super::file::{
    code_page_charset, le_checked, DescriptorNode, PstFile, Read, SubNodes, Thrown, PSETID_TASK,
};
use super::lzfu;
use super::node::NodeStream;
use super::table::{Item, PropertyContext, Row, TableContext, ROW_ID};
use crate::java;
use crate::text::charset::Charset;

const UNICODE: i32 = 0x1F;
const MESSAGE_CODE_PAGE: i32 = 0x3FFD;
const INTERNET_CODE_PAGE: i32 = 0x3FDE;
const RECIPIENT_TABLE: i32 = 0x0692;
const ATTACHMENT_TABLE: i32 = 0x0671;
const ATTACH_DATA: i32 = 0x3701;
const ATTACH_METHOD: i32 = 0x3705;
const ATTACH_EMBEDDED_MESSAGE: i32 = 5;
const TASK_DUE_DATE: i32 = 0x8105;
const NORMAL_FOLDER: i32 = 0x02;
const SEARCH_FOLDER: i32 = 0x03;
const NORMAL_MESSAGE: i32 = 0x04;
const HIERARCHY_TABLE: i64 = 11;
const CONTENTS_TABLE: i64 = 12;
/// `EPOCH_DIFF`: milliseconds from 1601 to 1970.
const EPOCH_DIFF: i64 = 11_644_473_600_000;

/// Text in a charset the port does not decode as the JDK does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Undecodable(pub(super) String);

/// `PSTObject`: a node's properties and its sub-nodes.
pub(super) struct PstObject<'p, 'f> {
    file: &'p PstFile<'f>,
    items: HashMap<i32, Item>,
    local: Option<SubNodes>,
    descriptor: Option<DescriptorNode>,
}

impl<'p, 'f> PstObject<'p, 'f> {
    /// `getIntItem(identifier, defaultValue)`.
    fn int_or(&self, id: i32, default: i32) -> i32 {
        self.items.get(&id).map_or(default, |item| item.reference)
    }

    fn int(&self, id: i32) -> i32 {
        self.int_or(id, 0)
    }

    /// `getStringCodepage`, or the code page of the first of `ids` present.
    fn code_page(&self, ids: [i32; 2]) -> Option<&'static str> {
        let item = ids.iter().find_map(|id| self.items.get(id))?;
        code_page_charset(item.reference)
    }

    /// `getStringItem(identifier, 0, codepage)`, the code page the object's own where `None`.
    fn string_with(&self, id: i32, code_page: Option<&'static str>) -> Result<String, Undecodable> {
        let Some(item) = self.items.get(&id) else {
            return Ok(String::new());
        };
        let code_page =
            code_page.or_else(|| self.code_page([MESSAGE_CODE_PAGE, INTERNET_CODE_PAGE]));
        if !item.external {
            return java_string(&item.data, item.value_type, code_page);
        }
        let Some(sub_node) = self
            .local
            .as_ref()
            .and_then(|local| local.get(item.reference))
        else {
            return Ok(String::new());
        };
        match sub_node.data(self.file) {
            Ok(data) => java_string(&data, item.value_type, code_page),
            Err(_) => Ok(String::new()),
        }
    }

    fn string(&self, id: i32) -> Result<String, Undecodable> {
        self.string_with(id, None)
    }

    /// `getDateItem`: milliseconds since 1970.
    fn date(&self, id: i32) -> Read<Option<i64>> {
        let Some(item) = self.items.get(&id) else {
            return Ok(None);
        };
        if item.data.is_empty() {
            return Ok(Some(0));
        }
        let high = le_checked(&item.data, 4, 8, "a date")? as i32;
        let low = le_checked(&item.data, 0, 4, "a date")? as i32;
        let filetime = (i64::from(high) << 32) | (i64::from(low) & 0xFFFF_FFFF);
        Ok(Some(filetime / 10_000 - EPOCH_DIFF))
    }

    /// `getDisplayName`.
    pub(super) fn display_name(&self) -> Result<String, Undecodable> {
        self.string(0x3001)
    }

    /// `getDescriptorNodeId`.
    pub(super) fn descriptor_node_id(&self) -> i64 {
        self.descriptor.map_or(0, |node| i64::from(node.id))
    }
}

/// `createJavaString`.
fn java_string(
    data: &[u8],
    value_type: i32,
    code_page: Option<&str>,
) -> Result<String, Undecodable> {
    if value_type == UNICODE {
        return Ok(java::utf16le(data));
    }
    let Some(code_page) = code_page else {
        return Ok(java::utf8(data));
    };
    let Some(charset) = Charset::for_name(&code_page.to_ascii_uppercase()) else {
        return Ok(String::new());
    };
    charset.decode(data).ok_or_else(|| {
        Undecodable(format!(
            "a string in {}, which the port does not decode",
            charset.name()
        ))
    })
}

/// A node `detectAndLoadPSTObject` loads.
pub(super) enum Loaded<'p, 'f> {
    Folder(Box<Folder<'p, 'f>>),
    Message(Box<Message<'p, 'f>>),
}

/// `detectAndLoadPSTObject`.
pub(super) fn load<'p, 'f>(file: &'p PstFile<'f>, node: DescriptorNode) -> Read<Loaded<'p, 'f>> {
    if !matches!(
        node.node_type(),
        NORMAL_FOLDER | SEARCH_FOLDER | NORMAL_MESSAGE
    ) {
        return Err(Thrown::pst(format!(
            "Unknown child type with offset id: {}",
            node.sub_nodes
        )));
    }
    let object = read_object(file, node)?;
    Ok(if node.node_type() == NORMAL_MESSAGE {
        Loaded::Message(Box::new(Message::new(object)))
    } else {
        Loaded::Folder(Box::new(Folder::new(object)))
    })
}

/// The properties and sub-nodes of a node, as `PSTObject`'s constructors read them.
fn read_object<'p, 'f>(file: &'p PstFile<'f>, node: DescriptorNode) -> Read<PstObject<'p, 'f>> {
    let stream = NodeStream::from_block(file, file.block(node.data)?)?;
    let items = PropertyContext::read(file, stream)?.items;
    let local = if node.sub_nodes != 0 {
        Some(file.sub_nodes(node.sub_nodes)?)
    } else {
        None
    };
    Ok(PstObject {
        file,
        items,
        local,
        descriptor: Some(node),
    })
}

/// A table context of a node, as `PSTTable7C` reads it from the node's data and sub-nodes.
fn read_table<'p, 'f>(
    file: &'p PstFile<'f>,
    node: DescriptorNode,
    entity: Option<i32>,
) -> Read<TableContext<'p, 'f>> {
    let sub_nodes = if node.sub_nodes > 0 {
        file.sub_nodes(node.sub_nodes)?
    } else {
        SubNodes::default()
    };
    let stream = NodeStream::from_block(file, file.block(node.data)?)?;
    TableContext::read(file, stream, sub_nodes, entity)
}

/// `PSTFolder`.
pub(super) struct Folder<'p, 'f> {
    object: PstObject<'p, 'f>,
    emails: OnceCell<Read<Emails<'p, 'f>>>,
}

/// Where a folder's messages are listed: `emailsTable`, `fallbackEmailsTable`, or nowhere.
enum Emails<'p, 'f> {
    None,
    Table(Box<TableContext<'p, 'f>>),
    Fallback(Vec<DescriptorNode>),
}

impl<'p, 'f> Folder<'p, 'f> {
    fn new(object: PstObject<'p, 'f>) -> Self {
        Self {
            object,
            emails: OnceCell::new(),
        }
    }

    /// `getRootFolder`.
    pub(super) fn root(file: &'p PstFile<'f>) -> Read<Self> {
        Ok(Self::new(read_object(file, file.descriptor(290)?)?))
    }

    pub(super) fn display_name(&self) -> Result<String, Undecodable> {
        self.object.display_name()
    }

    /// `getContentCount`.
    pub(super) fn content_count(&self) -> i32 {
        self.object.int(0x3602)
    }

    /// `hasSubfolders`.
    pub(super) fn has_subfolders(&self) -> bool {
        self.object.int(0x360A) != 0
    }

    fn descriptor(&self) -> DescriptorNode {
        self.object
            .descriptor
            .expect("a folder is read from a node of the node B-tree")
    }

    /// The `PSTException` java-libpst wraps a failure to read the subfolders in.
    fn subfolders_failed(&self, thrown: &Thrown) -> Thrown {
        let name = self
            .display_name()
            .unwrap_or_else(|Undecodable(reason)| reason);
        Thrown::pst(format!(
            "Can't get child folders for folder {name}({}) child count: {} - {thrown}",
            self.object.descriptor_node_id(),
            self.content_count()
        ))
    }

    /// `getSubFolders`.
    pub(super) fn subfolders(&self) -> Read<Vec<Folder<'p, 'f>>> {
        let file = self.object.file;
        let wrap = |thrown: Thrown| match thrown.kind {
            super::file::Kind::Pst => self.subfolders_failed(&thrown),
            _ => thrown,
        };
        let node = file
            .descriptor(i64::from(self.descriptor().id) + HIERARCHY_TABLE)
            .map_err(wrap)?;
        let table = read_table(file, node, None).map_err(wrap)?;
        let rows = table.all_rows().map_err(wrap)?;
        rows.iter()
            .map(|row| {
                let id = row
                    .get(&ROW_ID)
                    .ok_or_else(|| Thrown::null("a subfolder's row id"))?
                    .reference;
                match load(file, file.descriptor(i64::from(id)).map_err(wrap)?).map_err(wrap)? {
                    Loaded::Folder(folder) => Ok(*folder),
                    Loaded::Message(_) => Err(Thrown::runtime(
                        "java.lang.ClassCastException",
                        "class com.pff.PSTMessage cannot be cast to class com.pff.PSTFolder",
                    )),
                }
            })
            .collect()
    }

    /// `initEmailsTable`.
    fn emails(&self) -> Read<&Emails<'p, 'f>> {
        let emails = self.emails.get_or_init(|| {
            let descriptor = self.descriptor();
            if descriptor.node_type() == SEARCH_FOLDER {
                return Ok(Emails::None);
            }
            let file = self.object.file;
            let table = file
                .descriptor(i64::from(descriptor.id) + CONTENTS_TABLE)
                .and_then(|node| read_table(file, node, Some(ROW_ID)));
            match table {
                Ok(table) => Ok(Emails::Table(Box::new(table))),
                Err(_) => Ok(Emails::Fallback(
                    file.children(descriptor.id)?
                        .into_iter()
                        .filter(|node| node.node_type() == NORMAL_MESSAGE)
                        .collect(),
                )),
            }
        });
        emails.as_ref().map_err(Thrown::clone)
    }

    /// `getNextChild` for the child at `index`, cast to a message as Tika casts it.
    pub(super) fn child(&self, index: usize) -> Read<Option<Message<'p, 'f>>> {
        let file = self.object.file;
        let count = i64::from(self.content_count());
        let node = match self.emails()? {
            Emails::None => return Ok(None),
            Emails::Table(table) => {
                let rows = table.rows(index, 1)?;
                if index as i64 == count {
                    return Ok(None);
                }
                let row = rows.first().ok_or_else(|| {
                    Thrown::runtime(
                        "java.lang.IndexOutOfBoundsException",
                        "Index 0 out of bounds for length 0",
                    )
                })?;
                let id = row
                    .get(&ROW_ID)
                    .ok_or_else(|| Thrown::null("a message's row id"))?
                    .reference;
                file.descriptor(i64::from(id))?
            }
            Emails::Fallback(nodes) => {
                if index as i64 >= count || index >= nodes.len() {
                    return Ok(None);
                }
                nodes[index]
            }
        };
        match load(file, node)? {
            Loaded::Message(message) => Ok(Some(*message)),
            Loaded::Folder(_) => Err(Thrown::runtime(
                "java.lang.ClassCastException",
                "class com.pff.PSTFolder cannot be cast to class com.pff.PSTMessage",
            )),
        }
    }
}

/// `PSTMessage`.
pub(super) struct Message<'p, 'f> {
    object: PstObject<'p, 'f>,
    recipients: OnceCell<Option<Recipients<'p, 'f>>>,
    attachments: OnceCell<Read<Option<TableContext<'p, 'f>>>>,
}

/// A message's recipient table and its rows.
struct Recipients<'p, 'f> {
    table: TableContext<'p, 'f>,
    rows: OnceCell<Read<Vec<Row>>>,
}

impl<'p, 'f> Message<'p, 'f> {
    fn new(object: PstObject<'p, 'f>) -> Self {
        Self {
            object,
            recipients: OnceCell::new(),
            attachments: OnceCell::new(),
        }
    }

    /// `getSubject`: without the two characters of a prefix marker.
    pub(super) fn subject(&self) -> Result<String, Undecodable> {
        let subject = self.object.string(0x0037)?;
        let units: Vec<u16> = subject.encode_utf16().collect();
        if units.len() >= 2 && units[0] == 0x01 {
            return Ok(String::from_utf16_lossy(&units[2..]));
        }
        Ok(subject)
    }

    /// `getBody`.
    pub(super) fn body(&self) -> Result<String, Undecodable> {
        let code_page = self
            .object
            .code_page([MESSAGE_CODE_PAGE, INTERNET_CODE_PAGE]);
        self.object.string_with(0x1000, code_page)
    }

    /// `getBodyHTML`.
    pub(super) fn body_html(&self) -> Result<String, Undecodable> {
        let code_page = self
            .object
            .code_page([INTERNET_CODE_PAGE, MESSAGE_CODE_PAGE]);
        self.object.string_with(0x1013, code_page)
    }

    /// `getRTFBody`.
    pub(super) fn rtf_body(&self) -> Read<String> {
        let Some(item) = self.object.items.get(&0x1009) else {
            return Ok(String::new());
        };
        if !item.data.is_empty() {
            return lzfu::decode(&item.data);
        }
        let local = self
            .object
            .local
            .as_ref()
            .ok_or_else(|| Thrown::null("localDescriptorItems"))?;
        match local.get(item.reference) {
            Some(sub_node) => lzfu::decode(&sub_node.data(self.object.file)?),
            None => Ok(String::new()),
        }
    }

    pub(super) fn string(&self, id: i32) -> Result<String, Undecodable> {
        self.object.string(id)
    }

    pub(super) fn int_or(&self, id: i32, default: i32) -> i32 {
        self.object.int_or(id, default)
    }

    pub(super) fn date(&self, id: i32) -> Read<Option<i64>> {
        self.object.date(id)
    }

    pub(super) fn descriptor_node_id(&self) -> i64 {
        self.object.descriptor_node_id()
    }

    /// `isFlagged`: whether the task's due date, a named property, is set.
    pub(super) fn is_flagged(&self) -> Read<bool> {
        let id = self.object.file.named_property(TASK_DUE_DATE, PSETID_TASK);
        Ok(self.object.date(id)?.is_some())
    }

    /// `processRecipients`: the recipient table, none where it cannot be read.
    fn recipients(&self) -> Option<&Recipients<'p, 'f>> {
        self.recipients
            .get_or_init(|| {
                let sub_node = self.object.local.as_ref()?.get(RECIPIENT_TABLE)?;
                let file = self.object.file;
                let sub_nodes = if sub_node.sub_nodes > 0 {
                    file.sub_nodes(sub_node.sub_nodes).ok()?
                } else {
                    SubNodes::default()
                };
                let stream = NodeStream::from_sub_node(file, sub_node).ok()?;
                let table = TableContext::read(file, stream, sub_nodes, None).ok()?;
                Some(Recipients {
                    table,
                    rows: OnceCell::new(),
                })
            })
            .as_ref()
    }

    /// `getNumberOfRecipients`.
    pub(super) fn recipient_count(&self) -> usize {
        self.recipients()
            .map_or(0, |recipients| recipients.table.row_count())
    }

    /// `getRecipient`.
    pub(super) fn recipient(&self, index: usize) -> Read<Recipient> {
        let unable = || Thrown::pst(format!("unable to fetch recipient number {index}"));
        if index >= self.recipient_count() {
            return Err(unable());
        }
        let recipients = self.recipients().ok_or_else(unable)?;
        let rows = recipients
            .rows
            .get_or_init(|| recipients.table.all_rows())
            .as_ref()
            .map_err(Thrown::clone)?;
        let row = rows.get(index).ok_or_else(unable)?;
        Ok(Recipient(row.clone()))
    }

    /// `processAttachments`.
    fn attachment_table(&self) -> Read<Option<&TableContext<'p, 'f>>> {
        self.attachments
            .get_or_init(|| {
                let Some(sub_node) = self
                    .object
                    .local
                    .as_ref()
                    .and_then(|local| local.get(ATTACHMENT_TABLE))
                else {
                    return Ok(None);
                };
                let file = self.object.file;
                let sub_nodes = if sub_node.sub_nodes > 0 {
                    file.sub_nodes(sub_node.sub_nodes)?
                } else {
                    SubNodes::default()
                };
                let stream = NodeStream::from_sub_node(file, sub_node)?;
                TableContext::read(file, stream, sub_nodes, None).map(Some)
            })
            .as_ref()
            .map(Option::as_ref)
            .map_err(Thrown::clone)
    }

    /// `getNumberOfAttachments`.
    pub(super) fn attachment_count(&self) -> usize {
        match self.attachment_table() {
            Ok(Some(table)) => table.row_count(),
            Ok(None) | Err(_) => 0,
        }
    }

    /// `getAttachment`.
    pub(super) fn attachment(&self, index: usize) -> Read<Attachment<'p, 'f>> {
        let table = self.attachment_table()?;
        let count = table.map_or(0, TableContext::row_count);
        if index >= count {
            return Err(Thrown::pst(format!(
                "unable to fetch attachment number {index}, only {count} in this email"
            )));
        }
        let table = table.expect("a table holds the attachments counted");
        let rows = table.all_rows()?;
        let row = rows.get(index).ok_or_else(|| {
            Thrown::runtime(
                "java.lang.IndexOutOfBoundsException",
                format!("Index {index} out of bounds for length {}", rows.len()),
            )
        })?;
        let id = row
            .get(&ROW_ID)
            .ok_or_else(|| Thrown::null("an attachment's row id"))?
            .reference;
        let file = self.object.file;
        let sub_node = self
            .object
            .local
            .as_ref()
            .and_then(|local| local.get(id))
            .ok_or_else(|| Thrown::null("the attachment's descriptor item"))?;
        if sub_node.data(file)?.is_empty() {
            return Err(Thrown::pst(format!(
                "unable to fetch attachment number {index}, unable to read attachment details table"
            )));
        }
        let items = PropertyContext::read(file, NodeStream::from_sub_node(file, sub_node)?)?.items;
        let local = if sub_node.sub_nodes > 0 {
            file.sub_nodes(sub_node.sub_nodes)?
        } else {
            SubNodes::default()
        };
        Ok(Attachment {
            object: PstObject {
                file,
                items,
                local: Some(local),
                descriptor: None,
            },
        })
    }
}

/// `PSTRecipient`: a row of the recipient table.
pub(super) struct Recipient(Row);

impl Recipient {
    fn string(&self, id: i32) -> String {
        self.0.get(&id).map_or_else(String::new, Item::string_value)
    }

    /// `getDisplayName`.
    pub(super) fn display_name(&self) -> String {
        self.string(0x3001)
    }

    /// `getEmailAddress`.
    pub(super) fn email_address(&self) -> String {
        self.string(0x3003)
    }

    /// `getRecipientType`.
    pub(super) fn recipient_type(&self) -> i32 {
        match self.0.get(&0x0C15) {
            Some(item) if item.value_type == 0x0003 => item.reference,
            Some(item) if item.value_type == 0x0002 => i32::from(item.reference as i16),
            _ => 0,
        }
    }
}

/// `PSTAttachment`.
pub(super) struct Attachment<'p, 'f> {
    object: PstObject<'p, 'f>,
}

impl<'p, 'f> Attachment<'p, 'f> {
    /// `getSize`.
    pub(super) fn size(&self) -> i32 {
        self.object.int(0x0E20)
    }

    /// `getLongFilename`.
    pub(super) fn long_filename(&self) -> Result<String, Undecodable> {
        self.object.string(0x3707)
    }

    /// `getFilename`.
    pub(super) fn filename(&self) -> Result<String, Undecodable> {
        self.object.string(0x3704)
    }

    fn data_item(&self) -> Read<&Item> {
        self.object
            .items
            .get(&ATTACH_DATA)
            .ok_or_else(|| Thrown::null("the attachment's data"))
    }

    /// `getEmbeddedPSTMessage`: the message an attachment of an embedded message holds.
    pub(super) fn embedded_message(&mut self) -> Read<Option<Message<'p, 'f>>> {
        if self.object.int(ATTACH_METHOD) != ATTACH_EMBEDDED_MESSAGE {
            return Ok(None);
        }
        let file = self.object.file;
        let item = self
            .object
            .items
            .get(&ATTACH_DATA)
            .ok_or_else(|| Thrown::null("the attachment's data"))?;
        let stream = match item.value_type {
            0x0102 if !item.external => {
                NodeStream::from_bytes(item.data.clone(), file.is_encrypted())?
            }
            0x0102 => {
                return Err(Thrown::pst(
                    "External reference in getEmbeddedPSTMessage()!\n",
                ))
            }
            0x000D => {
                let id = le_checked(&item.data, 0, 4, "an embedded message's node")? as i32;
                let local = self
                    .object
                    .local
                    .as_mut()
                    .expect("an attachment always has its sub-nodes");
                let nested = local
                    .get(id)
                    .cloned()
                    .ok_or_else(|| Thrown::null("the embedded message's descriptor item"))?;
                let stream = NodeStream::from_sub_node(file, &nested)?;
                if nested.sub_nodes > 0 {
                    local.extend(file.sub_nodes(nested.sub_nodes)?);
                }
                stream
            }
            _ => return Ok(None),
        };
        match PropertyContext::read(file, stream) {
            Ok(context) => Ok(Some(Message::new(PstObject {
                file,
                items: context.items,
                local: self.object.local.clone(),
                descriptor: None,
            }))),
            Err(thrown) if thrown.kind == super::file::Kind::Pst => Ok(None),
            Err(thrown) => Err(thrown),
        }
    }

    /// `getFileInputStream`: the attachment's data.
    pub(super) fn data(&self) -> Read<Vec<u8>> {
        let item = self.data_item()?;
        let file = self.object.file;
        let stream = if item.external {
            let sub_node = self
                .object
                .local
                .as_ref()
                .and_then(|local| local.get(item.reference))
                .ok_or_else(|| Thrown::null("the attachment's descriptor item"))?;
            NodeStream::from_sub_node(file, sub_node)?
        } else {
            NodeStream::from_bytes(item.data.clone(), false)?
        };
        stream.read(0, stream.len())
    }
}
