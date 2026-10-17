//! Tika's `AbstractPOIFSExtractor.handleEmbeddedOfficeDoc`: a storage of a compound document
//! that holds an embedded object, read into the document Tika hands on to be parsed and the
//! metadata it gives it. An OOXML package is handed on as its ZIP archive, an OLE 1.0 object as
//! the file its record holds, an object with a `CONTENTS` stream as that stream, and any other
//! storage as a compound document of its own, written so that POI iterates it as it iterated the
//! storage.

use crate::config::ParserConfigs;
use crate::formats::embedded::Wrapping;
use crate::formats::embedded::{RELATIONSHIP_ID, RESOURCE_NAME};
use crate::formats::metadata::office::{
    EMBEDDED_STORAGE_CLASS_ID, OCX_NAME, ORIGINAL_RESOURCE_NAME, RESOURCE_NAME_EXTENSION_INFERRED,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::msg::{EmbeddedMessage, MsgParser};
use crate::formats::ole10_native::{self, Ole10Native, Unread};
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser, Unported};
use crate::java::equals_ignore_case;
use crate::java::is_blank;
use crate::java::to_lower_case;
use crate::mime::{self, ole_directory_type, MediaType, Origin};
use crate::ole2::{class_id_string, write, EntryId, EntryKind, Ole2, Unreadable};

const CONTENT_TYPE: &str = "Content-Type";
const CONTENT_LENGTH: &str = "Content-Length";
const OLE10_NATIVE: &str = "\u{1}Ole10Native";
const OCX_NAME_ENTRY: &str = "\u{3}OCXNAME";
const OUTLOOK: &str = "application/vnd.ms-outlook";
const GRAPH: &str = "application/vnd.ms-graph";
/// The streams `tryToGetMsgTitle` takes a message's title from, in order.
const MESSAGE_TITLES: [&str; 3] = [
    "__substg1.0_0037001F",
    "__substg1.0_0E1D001F",
    "__substg1.0_0070001F",
];

/// A document handed on to be parsed, with the metadata Tika gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Embedded {
    pub(crate) data: Vec<u8>,
    pub(crate) metadata: TikaMetadata,
}

/// What `handleEmbeddedOfficeDoc` hands on: a document, which Tika parses from its bytes; a
/// storage, which Tika parses from the storage itself, handed on as an open container, and the
/// native path from the storage written as a compound document of its own; or an Outlook
/// message stored in another document, which Tika's `OfficeParser` reads from its storage as a
/// message embedded in another, and the native one reads alike.
#[derive(Debug)]
pub(crate) enum Handed {
    Document(Embedded),
    Storage(Embedded),
    Message(Box<StoredMessage>),
}

/// An Outlook message read from its storage, with the metadata Tika hands it on with.
pub(crate) struct StoredMessage {
    message: EmbeddedMessage,
    metadata: TikaMetadata,
}

impl std::fmt::Debug for StoredMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredMessage")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

impl Handed {
    /// `parseEmbedded` of what is handed on, wrapped as `wrapping` says.
    pub(crate) fn write(self, xhtml: &mut Xhtml, wrapping: Wrapping) -> Result<(), Failure> {
        match self {
            Handed::Document(Embedded { data, metadata }) => {
                xhtml.parse_embedded(&data, &metadata, wrapping)
            }
            Handed::Storage(Embedded { data, metadata }) => {
                xhtml.parse_embedded_container(&data, &metadata, wrapping)
            }
            Handed::Message(stored) => {
                let StoredMessage { message, metadata } = *stored;
                xhtml.parse_container_natively(&metadata, wrapping, MsgParser.name(), |child| {
                    message.write(child)
                })
            }
        }
    }
}

/// POI's `POIFSDocumentType`: what Tika's container detector names a storage, and the extension
/// it names such a document by.
fn document_type(media_type: &MediaType) -> (MediaType, &'static str) {
    const TYPES: [(&str, &str); 16] = [
        ("application/vnd.ms-excel", "xls"),
        ("application/msword", "doc"),
        ("application/x-tika-ole-drm-encrypted", "ole"),
        ("application/x-tika-ooxml-protected", "ole"),
        ("application/vnd.ms-powerpoint", "ppt"),
        ("application/x-mspublisher", "pub"),
        ("application/vnd.ms-project", "mpp"),
        ("application/vnd.visio", "vsd"),
        ("application/vnd.ms-works", "wps"),
        ("application/x-tika-msworks-spreadsheet", "xlr"),
        ("application/vnd.ms-outlook", "msg"),
        ("application/sldworks", "slddrw"),
        ("application/vnd.ms-graph", ""),
        ("application/x-tika-msoffice", "unknown"),
        (
            "application/x-tika-msoffice-embedded; format=ole10_native",
            "ole",
        ),
        (
            "application/x-tika-msoffice-embedded; format=comp_obj",
            "ole",
        ),
    ];
    TYPES
        .iter()
        .map(|&(name, extension)| (MediaType::parse(name).expect("a media type"), extension))
        .find(|(known, _)| known == media_type)
        .unwrap_or_else(|| (MediaType::named("application/x-tika-msoffice"), "unknown"))
}

/// `handleEmbeddedOfficeDoc(dir, metadata, resourceName, xhtml, outputHtml)`: the document to
/// hand on, with `metadata` and what Tika adds to it, an embedded message read under `configs`;
/// `None` where Tika hands on nothing, or why Tika records a failure or fails instead.
pub(crate) fn office_document(
    file: &Ole2<'_>,
    directory: EntryId,
    mut metadata: TikaMetadata,
    resource_name: Option<&str>,
    configs: &ParserConfigs,
) -> Result<Option<Handed>, Unported> {
    let name = file.name(directory);
    let class_id = class_id_string(&file.class_id(directory));
    let package = file
        .child(directory, "Package")
        .or_else(|| file.child(directory, "package"));
    if let Some(package) = package {
        let data = read_document(file, package)?;
        metadata.set(CONTENT_LENGTH, data.len().to_string());
        metadata.set(RELATIONSHIP_ID, name);
        metadata.set(EMBEDDED_STORAGE_CLASS_ID, class_id);
        metadata.set(CONTENT_TYPE, mime::zip_container_type(&data).to_string());
        return Ok(Some(Handed::Document(Embedded { data, metadata })));
    }
    metadata.set(RELATIONSHIP_ID, name.clone());
    metadata.set(EMBEDDED_STORAGE_CLASS_ID, class_id);
    let detected = ole_directory_type(file, directory);
    let (kind, extension) = document_type(&detected);
    let resource = resource_name.map_or(name, str::to_owned);
    ocx_name(file, directory, &mut metadata);
    if kind
        == MediaType::parse("application/x-tika-msoffice-embedded; format=ole10_native")
            .expect("a media type")
    {
        return Ok(ole_native(file, directory, &resource, metadata)?.map(Handed::Document));
    }
    if kind
        == MediaType::parse("application/x-tika-msoffice-embedded; format=comp_obj")
            .expect("a media type")
    {
        return Ok(contents(file, directory, &resource, metadata)?.map(Handed::Document));
    }
    let outlook = kind == MediaType::named(OUTLOOK);
    let resource = if outlook {
        message_title(file, directory).unwrap_or(resource)
    } else {
        resource
    };
    metadata.set(CONTENT_TYPE, kind.to_string());
    if !is_blank(&resource) {
        if extension.is_empty() {
            metadata.set(RESOURCE_NAME, resource);
        } else {
            metadata.set(RESOURCE_NAME, with_extension(&resource, extension));
            metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
        }
    }
    let size: usize = file
        .children(directory)
        .filter(|&child| file.kind(child) == EntryKind::Document)
        .map(|child| file.read(child).map(|data| data.len()))
        .sum::<Result<usize, Unreadable>>()
        .map_err(unreadable)?;
    metadata.set(CONTENT_LENGTH, size.to_string());
    if outlook && directory != file.root() {
        let message = EmbeddedMessage::read(file, directory, configs)?;
        return Ok(Some(Handed::Message(Box::new(StoredMessage {
            message,
            metadata,
        }))));
    }
    if kind == MediaType::named(GRAPH) {
        return Err(Unported(
            "an MS Graph storage, which Tika's detection names by its class as the open container \
             Tika hands on, and by its streams as the bytes our Java parses"
                .to_owned(),
        ));
    }
    let storage = file.storage(directory).map_err(unreadable)?;
    let data = write(&storage).map_err(unreadable)?;
    Ok(Some(Handed::Storage(Embedded { data, metadata })))
}

/// `tryToGetMsgTitle`: the subject, conversation topic or display name of a message storage,
/// the first of its streams there is, decoded as UTF-16LE up to a NUL that ends it. POI takes the
/// first 1000 bytes of the stream, which may cut a long title and split a character (#119).
fn message_title(file: &Ole2<'_>, directory: EntryId) -> Option<String> {
    MESSAGE_TITLES.iter().find_map(|name| {
        let entry = file.child(directory, name)?;
        if file.kind(entry) != EntryKind::Document {
            return None;
        }
        let mut title = utf16_le(&file.read(entry).ok()?);
        if let Some(end) = title.find('\0') {
            title.truncate(end);
        }
        Some(title)
    })
}

/// Bytes decoded as Java's UTF-16LE decoder replaces what it cannot decode: each surrogate
/// without its pair, and a last odd byte, as U+FFFD.
fn utf16_le(bytes: &[u8]) -> String {
    let (pairs, rest) = bytes.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    let mut text = String::from_utf16_lossy(&units);
    if !rest.is_empty() {
        text.push(char::REPLACEMENT_CHARACTER);
    }
    text
}

fn unreadable(Unreadable(reason): Unreadable) -> Unported {
    Unported(format!(
        "an embedded storage POI cannot read, whose failure Tika records: {reason}"
    ))
}

/// A document entry read whole, failing as POI's cast and read do.
fn read_document(file: &Ole2<'_>, entry: EntryId) -> Result<Vec<u8>, Unported> {
    if file.kind(entry) != EntryKind::Document {
        return Err(Unported(
            "an embedded storage's stream is a directory, which Tika fails on".to_owned(),
        ));
    }
    file.read(entry).map_err(unreadable)
}

/// `handleOLENative`: the file an OLE 1.0 object's record holds.
fn ole_native(
    file: &Ole2<'_>,
    directory: EntryId,
    resource: &str,
    mut metadata: TikaMetadata,
) -> Result<Option<Embedded>, Unported> {
    let entry = file
        .child_ignoring_case(directory, OLE10_NATIVE)
        .ok_or_else(|| Unported("an OLE 1.0 object without its record".to_owned()))?;
    let record = read_document(file, entry)?;
    if record.len() > ole10_native::MAX_RECORD_LENGTH as usize {
        return Err(Unported(format!(
            "an OLE 1.0 object's record of {} bytes is longer than POI reads",
            record.len()
        )));
    }
    let native = match Ole10Native::parse(&record) {
        Ok(native) => native,
        Err(Unread::Invalid) => return Ok(None),
        Err(Unread::Failed(message)) => {
            return Err(Unported(format!(
                "POI fails on an OLE 1.0 object's record, which Tika records: {message}"
            )))
        }
    };
    match &native.label {
        Some(label) => metadata.set(RESOURCE_NAME, format!("{resource}/{label}")),
        None => metadata.add(RESOURCE_NAME, resource),
    }
    for name in [&native.command, &native.file_name].into_iter().flatten() {
        metadata.add(ORIGINAL_RESOURCE_NAME, name.clone());
    }
    metadata.set(CONTENT_LENGTH, native.data.len().to_string());
    Ok(Some(Embedded {
        data: native.data,
        metadata,
    }))
}

/// `handleCompObj`: an object's `CONTENTS` stream, named by the type Tika detects in it.
fn contents(
    file: &Ole2<'_>,
    directory: EntryId,
    resource: &str,
    mut metadata: TikaMetadata,
) -> Result<Option<Embedded>, Unported> {
    let entry = match file
        .child(directory, "CONTENTS")
        .or_else(|| file.child(directory, "Contents"))
    {
        Some(entry) => entry,
        None => {
            let mut named = file
                .children(directory)
                .filter(|&child| equals_ignore_case(&file.name(child), "contents"));
            let Some(entry) = named.next() else {
                return Ok(None);
            };
            if named.next().is_some() {
                return Err(Unported(
                    "an object has several contents streams, of which Tika takes the first \
                     of a hash map"
                        .to_owned(),
                ));
            }
            entry
        }
    };
    let data = read_document(file, entry)?;
    let detected = mime::detect(&data, Origin::Bytes)
        .map_err(|error| Unported(format!("Tika's detection fails on an object: {error:?}")))?
        .media_type;
    let extension = mime::extension(&detected);
    metadata.set(CONTENT_TYPE, detected.to_string());
    metadata.set(RESOURCE_NAME, with_extension(resource, extension));
    metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
    metadata.set(CONTENT_LENGTH, data.len().to_string());
    Ok(Some(Embedded { data, metadata }))
}

/// `extractOCXName`: the name an ActiveX control's storage gives it.
fn ocx_name(file: &Ole2<'_>, directory: EntryId, metadata: &mut TikaMetadata) {
    let Some(entry) = file.child(directory, OCX_NAME_ENTRY) else {
        return;
    };
    if file.kind(entry) != EntryKind::Document {
        return;
    }
    let Ok(bytes) = file.read(entry) else {
        return;
    };
    let Some(length) = bytes.len().checked_sub(4) else {
        return;
    };
    if length % 2 != 0 {
        return;
    }
    let units: Vec<u16> = bytes[..length]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|&unit| unit != 0)
        .collect();
    metadata.set(OCX_NAME, String::from_utf16_lossy(&units));
}

/// `appendExtensionIfMissing`.
fn with_extension(name: &str, extension: &str) -> String {
    if is_blank(extension) {
        return name.to_owned();
    }
    let dotted = if extension.starts_with('.') {
        extension.to_owned()
    } else {
        format!(".{extension}")
    };
    if to_lower_case(name).ends_with(&to_lower_case(&dotted)) {
        name.to_owned()
    } else {
        format!("{name}{dotted}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ole2::{Node, Storage};

    fn units(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
    }

    fn embedding(entries: Vec<(Vec<u16>, Node)>) -> Vec<u8> {
        let object = Storage {
            class_id: [1; 16],
            entries,
        };
        write(&Storage {
            class_id: [0; 16],
            entries: vec![(units("_123"), Node::Storage(object))],
        })
        .unwrap()
    }

    fn handed_on(data: &[u8]) -> Option<Embedded> {
        let file = Ole2::open(data).unwrap();
        let object = file.child(file.root(), "_123").unwrap();
        match office_document(
            &file,
            object,
            TikaMetadata::new(),
            None,
            &ParserConfigs::default(),
        )
        .unwrap()?
        {
            Handed::Document(embedded) | Handed::Storage(embedded) => Some(embedded),
            Handed::Message(message) => panic!("a document, not {message:?}"),
        }
    }

    #[test]
    fn leaves_an_ms_graph_storage_to_tika() {
        let data = embedding(vec![
            (units("Workbook"), Node::Document(vec![1; 10])),
            (
                units("\u{1}CompObj"),
                Node::Document(b"..MSGraph.Chart.8..".to_vec()),
            ),
        ]);
        let file = Ole2::open(&data).unwrap();
        let object = file.child(file.root(), "_123").unwrap();
        let handed = office_document(
            &file,
            object,
            TikaMetadata::new(),
            None,
            &ParserConfigs::default(),
        );
        assert!(
            matches!(&handed, Err(Unported(reason)) if reason.contains("MS Graph")),
            "{handed:?}"
        );
    }

    #[test]
    fn hands_on_a_storage_as_a_compound_document_named_by_its_type() {
        let data = embedding(vec![
            (units("WordDocument"), Node::Document(vec![1; 10])),
            (units("1Table"), Node::Document(vec![2; 5])),
        ]);
        let embedded = handed_on(&data).unwrap();
        let metadata = &embedded.metadata;
        assert_eq!(metadata.get(CONTENT_TYPE), Some("application/msword"));
        assert_eq!(metadata.get(RESOURCE_NAME), Some("_123.doc"));
        assert_eq!(metadata.get(CONTENT_LENGTH), Some("15"));
        assert_eq!(metadata.get(RELATIONSHIP_ID), Some("_123"));
        assert_eq!(
            metadata.get(EMBEDDED_STORAGE_CLASS_ID),
            Some("{01010101-0101-0101-0101-010101010101}")
        );
        let copy = Ole2::open(&embedded.data).unwrap();
        assert_eq!(copy.top_level_names(), ["WordDocument", "1Table"]);
        assert_eq!(copy.class_id(copy.root()), [1; 16]);
    }

    #[test]
    fn hands_on_the_contents_of_an_object_named_by_its_detected_type() {
        let data = embedding(vec![
            (units("\u{1}CompObj"), Node::Document(vec![0; 8])),
            (units("CONTENTS"), Node::Document(b"%PDF-1.4\n%".to_vec())),
        ]);
        let embedded = handed_on(&data).unwrap();
        assert_eq!(embedded.data, b"%PDF-1.4\n%");
        assert_eq!(embedded.metadata.get(CONTENT_TYPE), Some("application/pdf"));
        assert_eq!(embedded.metadata.get(RESOURCE_NAME), Some("_123.pdf"));
    }

    #[test]
    fn reads_a_stored_message_natively_named_by_its_subject() {
        let data = embedding(vec![
            (
                units("__substg1.0_0037001F"),
                Node::Document(vec![b'H', 0, b'i', 0, 7]),
            ),
            (
                units("__properties_version1.0"),
                Node::Document(vec![0; 24]),
            ),
        ]);
        let file = Ole2::open(&data).unwrap();
        let object = file.child(file.root(), "_123").unwrap();
        let handed = office_document(
            &file,
            object,
            TikaMetadata::new(),
            None,
            &ParserConfigs::default(),
        )
        .unwrap()
        .unwrap();
        let Handed::Message(stored) = handed else {
            panic!("a stored message, not {handed:?}");
        };
        assert_eq!(
            stored.metadata.get(CONTENT_TYPE),
            Some("application/vnd.ms-outlook")
        );
        assert_eq!(stored.metadata.get(RESOURCE_NAME), Some("Hi\u{fffd}.msg"));
    }

    #[test]
    fn decodes_a_title_as_java_decodes_utf_16() {
        assert_eq!(utf16_le(&[0x00, 0xD8, b'a', 0, 1]), "\u{fffd}a\u{fffd}");
    }

    #[test]
    fn names_a_stored_message_by_its_whole_subject() {
        let subject: Vec<u8> = "\u{e9}"
            .repeat(600)
            .encode_utf16()
            .chain([0, 0x41])
            .flat_map(u16::to_le_bytes)
            .collect();
        let data = embedding(vec![
            (units("__substg1.0_0037001F"), Node::Document(subject)),
            (
                units("__properties_version1.0"),
                Node::Document(vec![0; 24]),
            ),
        ]);
        let file = Ole2::open(&data).unwrap();
        let object = file.child(file.root(), "_123").unwrap();
        assert_eq!(message_title(&file, object), Some("\u{e9}".repeat(600)));
    }

    #[test]
    fn appends_an_extension_a_name_lacks() {
        assert_eq!(with_extension("a.DOC", "doc"), "a.DOC");
        assert_eq!(with_extension("a", ".pdf"), "a.pdf");
        assert_eq!(with_extension("a", ""), "a");
    }
}
