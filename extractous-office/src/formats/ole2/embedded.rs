//! Tika's `AbstractPOIFSExtractor.handleEmbeddedOfficeDoc`: a storage of a compound document
//! that holds an embedded object, read into the document Tika hands on to be parsed and the
//! metadata it gives it. An OOXML package is handed on as its ZIP archive, an OLE 1.0 object as
//! the file its record holds, an object with a `CONTENTS` stream as that stream, and any other
//! storage as a compound document of its own, written so that POI iterates it as it iterated the
//! storage, and parsed by the parser of the type Tika's detection names the open storage.

use std::borrow::Cow;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::{record_stream_exception, Wrapping};
use crate::formats::embedded::{PARSER_OVERRIDE, RELATIONSHIP_ID, RESOURCE_NAME};
use crate::formats::metadata::office::{
    EMBEDDED_STORAGE_CLASS_ID, OCX_NAME, ORIGINAL_RESOURCE_NAME, RESOURCE_NAME_EXTENSION_INFERRED,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::msg::{EmbeddedMessage, MsgParser};
use crate::formats::ole10_native::{self, Ole10Native, Unread};
use crate::formats::poi::{safely_allocate_check, Thrown};
use crate::formats::xhtml::Xhtml;
use crate::formats::{Difference, Failure, Parser, Unported};
use crate::input::Input;
use crate::java::equals_ignore_case;
use crate::java::is_blank;
use crate::java::to_lower_case;
use crate::mime::{self, ole_container_type, ole_directory_type, MediaType, Origin};
use crate::ole2::{class_id_string, Content, EntryId, EntryKind, Node, Ole2, Unreadable};

const CONTENT_TYPE: &str = "Content-Type";
const CONTENT_LENGTH: &str = "Content-Length";
const OLE10_NATIVE: &str = "\u{1}Ole10Native";
const OCX_NAME_ENTRY: &str = "\u{3}OCXNAME";
const OUTLOOK: &str = "application/vnd.ms-outlook";
/// The streams `tryToGetMsgTitle` takes a message's title from, in order.
const MESSAGE_TITLES: [&str; 3] = [
    "__substg1.0_0037001F",
    "__substg1.0_0E1D001F",
    "__substg1.0_0070001F",
];

/// A document handed on to be parsed, with the metadata Tika gives it: a stream in place in the
/// compound document it is stored in, bytes made of one, or a storage, written as a compound
/// document of its own only as it is handed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Embedded<'a> {
    pub(crate) data: Node<'a>,
    pub(crate) metadata: TikaMetadata,
}

impl<'a> Embedded<'a> {
    /// A document of bytes of its own.
    pub(crate) fn owned(data: Vec<u8>, metadata: TikaMetadata) -> Self {
        Self {
            data: Node::Owned(data),
            metadata,
        }
    }

    /// `parseEmbedded` of the document, wrapped as `wrapping` says, its bytes in place where
    /// they lie in one run, and copied together, to a file of their own past what is read
    /// whole, where they do not; a storage handed on as Tika hands on an open container.
    pub(crate) fn write(self, xhtml: &mut Xhtml, wrapping: Wrapping) -> Result<(), Failure> {
        let storage = matches!(self.data, Node::Storage(_));
        let data = handed_on(self.data)?;
        if storage {
            xhtml.parse_embedded_storage(&data, &self.metadata, wrapping)
        } else {
            xhtml.parse_embedded_input(&data, &self.metadata, wrapping)
        }
    }

    pub(crate) fn into_owned(self) -> Embedded<'static> {
        Embedded {
            data: self.data.into_owned(),
            metadata: self.metadata,
        }
    }
}

/// The bytes of a document as it is handed on, failing the extraction where they cannot be
/// spooled.
pub(crate) fn handed_on(data: Node<'_>) -> Result<Input<'_>, Failure> {
    data.into_input().map_err(|error| {
        Failure::Extraction(Error::IoError(format!(
            "Could not spool an embedded document of a compound document: {error}"
        )))
    })
}

/// What `handleEmbeddedOfficeDoc` hands on: a document, which Tika parses from its bytes, as
/// the native path parses a storage written as a compound document of its own, where Tika
/// parses the storage itself, handed on as an open container that spools empty (#116); an
/// Outlook message stored in another document, which Tika's `OfficeParser` reads from its
/// storage as a message embedded in another, and the native one reads alike; or nothing, where
/// POI throws reading the object, which Tika records in the embedding document's metadata
/// before it goes on.
#[derive(Debug)]
pub(crate) enum Handed<'a> {
    Document(Embedded<'a>),
    Message(Box<StoredMessage<'a>>),
    Recorded(Thrown),
}

/// An Outlook message read from its storage, with the metadata Tika hands it on with.
pub(crate) struct StoredMessage<'a> {
    message: EmbeddedMessage<'a>,
    metadata: TikaMetadata,
}

impl std::fmt::Debug for StoredMessage<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredMessage")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

impl Handed<'_> {
    /// `parseEmbedded` of what is handed on, wrapped as `wrapping` says.
    pub(crate) fn write(self, xhtml: &mut Xhtml, wrapping: Wrapping) -> Result<(), Failure> {
        match self {
            Handed::Document(embedded) => embedded.write(xhtml, wrapping),
            Handed::Message(stored) => {
                let StoredMessage { message, metadata } = *stored;
                let data = handed_on(message.standalone())?;
                xhtml.parse_container_natively(
                    &data,
                    &metadata,
                    wrapping,
                    MsgParser.name(),
                    |child| message.write(child),
                )
            }
            Handed::Recorded(thrown) => {
                record_stream_exception(xhtml.metadata_mut(), &thrown.to_string());
                Ok(())
            }
        }
    }

    /// Where the native reading of what is handed on differs from Tika's, if it does: the
    /// exception POI throws on an object, recorded as the port tells it, where Tika records
    /// POI's.
    pub(crate) fn difference(&self) -> Option<Difference> {
        match self {
            Handed::Recorded(Thrown::Told(reason)) => Some(Difference(format!(
                "an embedded object POI throws on, which Tika records with POI's exception: \
                 {reason}"
            ))),
            Handed::Recorded(Thrown::Java(_)) | Handed::Document(_) | Handed::Message(_) => None,
        }
    }

    /// The same, owning what it read in place.
    pub(crate) fn into_owned(self) -> Handed<'static> {
        match self {
            Handed::Document(embedded) => Handed::Document(embedded.into_owned()),
            Handed::Message(stored) => {
                let StoredMessage { message, metadata } = *stored;
                Handed::Message(Box::new(StoredMessage {
                    message: message.into_owned(),
                    metadata,
                }))
            }
            Handed::Recorded(thrown) => Handed::Recorded(thrown),
        }
    }
}

/// POI's `POIFSDocumentType`: what Tika's container detector names a storage, and the extension
/// it names such a document by.
pub(crate) fn document_type(media_type: &MediaType) -> (MediaType, &'static str) {
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
pub(crate) fn office_document<'a>(
    file: &Ole2<'a>,
    directory: EntryId,
    mut metadata: TikaMetadata,
    resource_name: Option<&str>,
    configs: &ParserConfigs,
) -> Result<Option<Handed<'a>>, Unported> {
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
        metadata.set(
            CONTENT_TYPE,
            mime::zip_container_type(&data.bytes()).to_string(),
        );
        return Ok(Some(Handed::Document(Embedded {
            data: Node::Document(data),
            metadata,
        })));
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
        return ole_native(file, directory, &resource, metadata);
    }
    if kind
        == MediaType::parse("application/x-tika-msoffice-embedded; format=comp_obj")
            .expect("a media type")
    {
        return contents(file, directory, &resource, metadata);
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
        .map(|child| file.content(child).map(|content| content.len()))
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
    let opened = ole_container_type(file, directory);
    if opened != detected {
        metadata.set(PARSER_OVERRIDE, opened.to_string());
    }
    let storage = file.storage(directory).map_err(unreadable)?;
    storage.check_writable().map_err(unreadable)?;
    Ok(Some(Handed::Document(Embedded {
        data: Node::Storage(storage),
        metadata,
    })))
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

/// An OOXML package's entry, in place, failing as POI's cast and read do.
fn read_document<'a>(file: &Ole2<'a>, entry: EntryId) -> Result<Content<'a>, Unported> {
    if file.kind(entry) != EntryKind::Document {
        return Err(Unported(
            "an embedded storage's stream is a directory, which Tika fails on".to_owned(),
        ));
    }
    file.content(entry).map_err(unreadable)
}

/// `handleOLENative`: the file an OLE 1.0 object's record holds, or what POI throws reading
/// the record, which Tika records.
fn ole_native<'a>(
    file: &Ole2<'a>,
    directory: EntryId,
    resource: &str,
    mut metadata: TikaMetadata,
) -> Result<Option<Handed<'a>>, Unported> {
    let entry = file
        .child_ignoring_case(directory, OLE10_NATIVE)
        .ok_or_else(|| Unported("an OLE 1.0 object without its record".to_owned()))?;
    if file.kind(entry) != EntryKind::Document {
        return Ok(Some(Handed::Recorded(Thrown::new(
            "an OLE 1.0 object's record that is a directory, which POI casts to a stream",
        ))));
    }
    let record = match file.content(entry) {
        Ok(record) => record,
        Err(Unreadable(reason)) => return Ok(Some(Handed::Recorded(Thrown::new(reason)))),
    };
    if let Err(thrown) = safely_allocate_check(
        record.len() as i64,
        i64::from(ole10_native::MAX_RECORD_LENGTH),
    ) {
        return Ok(Some(Handed::Recorded(thrown)));
    }
    let parsed = match record.bytes() {
        Cow::Borrowed(bytes) => Ole10Native::parse(bytes),
        Cow::Owned(bytes) => Ole10Native::parse(&bytes).map(Ole10Native::into_owned),
    };
    let native = match parsed {
        Ok(native) => native,
        Err(Unread::Invalid) => return Ok(None),
        Err(Unread::Failed(message)) => return Ok(Some(Handed::Recorded(Thrown::new(message)))),
    };
    match &native.label {
        Some(label) => metadata.set(RESOURCE_NAME, format!("{resource}/{label}")),
        None => metadata.add(RESOURCE_NAME, resource),
    }
    for name in [&native.command, &native.file_name].into_iter().flatten() {
        metadata.add(ORIGINAL_RESOURCE_NAME, name.clone());
    }
    metadata.set(CONTENT_LENGTH, native.data.len().to_string());
    Ok(Some(Handed::Document(Embedded {
        data: native.data.into(),
        metadata,
    })))
}

/// `handleCompObj`: an object's `CONTENTS` stream, named by the type Tika detects in it, or
/// the failure to read it, which Tika records.
fn contents<'a>(
    file: &Ole2<'a>,
    directory: EntryId,
    resource: &str,
    mut metadata: TikaMetadata,
) -> Result<Option<Handed<'a>>, Unported> {
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
    if file.kind(entry) != EntryKind::Document {
        return Err(Unported(
            "an object's contents stream is a directory, which Tika fails on".to_owned(),
        ));
    }
    let data = match file.content(entry) {
        Ok(data) => data,
        Err(Unreadable(reason)) => return Ok(Some(Handed::Recorded(Thrown::new(reason)))),
    };
    let detected = mime::detect(&data.bytes(), Origin::Bytes)
        .map_err(|error| Unported(format!("Tika's detection fails on an object: {error:?}")))?
        .media_type;
    let extension = mime::extension(&detected);
    metadata.set(CONTENT_TYPE, detected.to_string());
    metadata.set(RESOURCE_NAME, with_extension(resource, extension));
    metadata.set(RESOURCE_NAME_EXTENSION_INFERRED, "true");
    metadata.set(CONTENT_LENGTH, data.len().to_string());
    Ok(Some(Handed::Document(Embedded {
        data: Node::Document(data),
        metadata,
    })))
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
    use crate::ole2::{write, Node, Storage};

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

    fn object_of(data: &[u8]) -> Option<Embedded<'_>> {
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
            Handed::Document(embedded) => Some(embedded),
            Handed::Message(message) => panic!("a document, not {message:?}"),
            Handed::Recorded(thrown) => panic!("a document, not {thrown:?}"),
        }
    }

    #[test]
    fn parses_an_ms_graph_storage_as_tika_detects_it_open() {
        let data = embedding(vec![
            (units("Workbook"), Node::Owned(vec![1; 10])),
            (
                units("\u{1}CompObj"),
                Node::Owned(b"..MSGraph.Chart.8..".to_vec()),
            ),
        ]);
        let embedded = object_of(&data).unwrap();
        let metadata = &embedded.metadata;
        assert_eq!(metadata.get(CONTENT_TYPE), Some("application/vnd.ms-graph"));
        assert_eq!(metadata.get(RESOURCE_NAME), Some("_123"));
        assert_eq!(
            metadata.get(PARSER_OVERRIDE),
            Some("application/vnd.ms-excel")
        );
    }

    #[test]
    fn hands_on_a_storage_as_a_compound_document_named_by_its_type() {
        let data = embedding(vec![
            (units("WordDocument"), Node::Owned(vec![1; 10])),
            (units("1Table"), Node::Owned(vec![2; 5])),
        ]);
        let embedded = object_of(&data).unwrap();
        let metadata = &embedded.metadata;
        assert_eq!(metadata.get(CONTENT_TYPE), Some("application/msword"));
        assert_eq!(metadata.get(PARSER_OVERRIDE), None);
        assert_eq!(metadata.get(RESOURCE_NAME), Some("_123.doc"));
        assert_eq!(metadata.get(CONTENT_LENGTH), Some("15"));
        assert_eq!(metadata.get(RELATIONSHIP_ID), Some("_123"));
        assert_eq!(
            metadata.get(EMBEDDED_STORAGE_CLASS_ID),
            Some("{01010101-0101-0101-0101-010101010101}")
        );
        let written = embedded.data.into_input().unwrap();
        let copy = Ole2::open(&written).unwrap();
        assert_eq!(copy.top_level_names(), ["WordDocument", "1Table"]);
        assert_eq!(copy.class_id(copy.root()), [1; 16]);
    }

    #[test]
    fn hands_on_the_contents_of_an_object_named_by_its_detected_type() {
        let data = embedding(vec![
            (units("\u{1}CompObj"), Node::Owned(vec![0; 8])),
            (units("CONTENTS"), Node::Owned(b"%PDF-1.4\n%".to_vec())),
        ]);
        let embedded = object_of(&data).unwrap();
        assert_eq!(embedded.data, Node::Owned(b"%PDF-1.4\n%".to_vec()));
        assert_eq!(embedded.metadata.get(CONTENT_TYPE), Some("application/pdf"));
        assert_eq!(embedded.metadata.get(RESOURCE_NAME), Some("_123.pdf"));
    }

    #[test]
    fn reads_a_stored_message_natively_named_by_its_subject() {
        let data = embedding(vec![
            (
                units("__substg1.0_0037001F"),
                Node::Owned(vec![b'H', 0, b'i', 0, 7]),
            ),
            (units("__properties_version1.0"), Node::Owned(vec![0; 24])),
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

    /// An OLE 1.0 object whose record POI throws on is handed on as nothing but its exception,
    /// which Tika records in the embedding document's metadata before it goes on.
    #[test]
    fn records_an_ole_1_object_poi_throws_on() {
        let mut body = vec![2, 0];
        body.extend(b"label.txt\0C:\\dir\\file.txt\0");
        body.extend([0, 0, 3, 0]);
        body.extend(5i32.to_le_bytes());
        body.extend(b"C:\\x\0");
        body.extend(5i32.to_le_bytes());
        body.extend(b"hello");
        let mut record = i32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
        record.extend(body);
        let data = embedding(vec![(units("\u{1}Ole10Native"), Node::Owned(record))]);
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
        let Handed::Recorded(thrown) = &handed else {
            panic!("a recorded exception, not {handed:?}");
        };
        assert!(thrown.to_string().starts_with("Buffer overrun"), "{thrown}");
        assert!(handed.difference().is_some());
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
            (units("__substg1.0_0037001F"), Node::Owned(subject)),
            (units("__properties_version1.0"), Node::Owned(vec![0; 24])),
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

    /// An embedded document left to Tika is opened with the extraction's password, as Tika's
    /// path opens every document embedded in one it was given the password for (#119). The
    /// protected package goes to Tika because macros are extracted, which its port leaves to
    /// Tika.
    #[cfg(feature = "tika")]
    #[test]
    fn hands_the_password_on_with_a_document_left_to_tika() {
        use crate::config::{OfficeParserConfig, Password};
        use crate::formats::embedded::ParseContext;
        use crate::formats::handlers::{Body, ToText};
        use crate::formats::StringOut;

        crate::formats::parity::install_routes();
        let package = std::fs::read("../test_files/documents/encrypted/password.docx")
            .expect("the protected package");
        let mut entry = TikaMetadata::new();
        entry.set(RESOURCE_NAME, "password.docx");
        let text = |password: Option<&Password>| {
            let configs = ParserConfigs {
                office: OfficeParserConfig::new().set_extract_macros(true),
                ..ParserConfigs::default()
            }
            .with_password(password);
            let context = ParseContext::new(&configs);
            let mut out = StringOut::default();
            let mut handler = Body::new(ToText::new(&mut out));
            let (_, _, result) =
                Xhtml::write(&mut handler, TikaMetadata::new(), &context, |xhtml| {
                    xhtml.parse_embedded(&package, &entry, Wrapping::PackageEntry)
                });
            result.expect("the embedding document");
            out.0
        };
        let locked = text(None);
        let opened = text(Some(&Password::new("extractous")));
        let words = |text: &str| text.split_whitespace().count();
        assert!(words(&opened) > words(&locked), "{locked}\n{opened}");
    }
}
