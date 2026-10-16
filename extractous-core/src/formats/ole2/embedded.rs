//! Tika's `AbstractPOIFSExtractor.handleEmbeddedOfficeDoc`: a storage of a compound document
//! that holds an embedded object, read into the document Tika hands on to be parsed and the
//! metadata it gives it. An OOXML package is handed on as its ZIP archive, an OLE 1.0 object as
//! the file its record holds, an object with a `CONTENTS` stream as that stream, and any other
//! storage as a compound document of its own, written so that POI iterates it as it iterated the
//! storage.

use crate::formats::embedded::{RELATIONSHIP_ID, RESOURCE_NAME};
use crate::formats::handlers::is_blank;
use crate::formats::java::equals_ignore_case;
use crate::formats::metadata::office::{
    EMBEDDED_STORAGE_CLASS_ID, OCX_NAME, ORIGINAL_RESOURCE_NAME, RESOURCE_NAME_EXTENSION_INFERRED,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole10_native::{self, Ole10Native, Unread};
use crate::formats::Unported;
use crate::mime::java::to_lower_case;
use crate::mime::{self, ole_directory_type, MediaType, Origin};
use crate::ole2::{class_id_string, write, EntryId, EntryKind, Ole2, Unreadable};

const CONTENT_TYPE: &str = "Content-Type";
const CONTENT_LENGTH: &str = "Content-Length";
const OLE10_NATIVE: &str = "\u{1}Ole10Native";
const OCX_NAME_ENTRY: &str = "\u{3}OCXNAME";

/// A document handed on to be parsed, with the metadata Tika gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Embedded {
    pub(crate) data: Vec<u8>,
    pub(crate) metadata: TikaMetadata,
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

/// `handleEmbeddedOfficeDoc(dir, resourceName, xhtml, outputHtml)`: the document to hand on,
/// `None` where Tika hands on nothing, or why Tika records a failure or fails instead.
pub(crate) fn office_document(
    file: &Ole2<'_>,
    directory: EntryId,
    resource_name: Option<&str>,
) -> Result<Option<Embedded>, Unported> {
    let name = file.name(directory);
    let class_id = class_id_string(&file.class_id(directory));
    let mut metadata = TikaMetadata::new();
    let package = file
        .child(directory, "Package")
        .or_else(|| file.child(directory, "package"));
    if let Some(package) = package {
        let data = read_document(file, package)?;
        metadata.set(CONTENT_LENGTH, data.len().to_string());
        metadata.set(RELATIONSHIP_ID, name);
        metadata.set(EMBEDDED_STORAGE_CLASS_ID, class_id);
        metadata.set(CONTENT_TYPE, mime::zip_container_type(&data).to_string());
        return Ok(Some(Embedded { data, metadata }));
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
    if kind == MediaType::named("application/vnd.ms-outlook") {
        return Err(Unported(
            "an embedded Outlook message, whose properties POI reads as those of a message \
             stored in another, and as a message of its own once handed on"
                .to_owned(),
        ));
    }
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
    let storage = file.storage(directory).map_err(unreadable)?;
    let data = write(&storage).map_err(unreadable)?;
    Ok(Some(Embedded { data, metadata }))
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
        office_document(&file, object, None).unwrap()
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
    fn leaves_an_embedded_message_to_tika() {
        let data = embedding(vec![(
            units("__substg1.0_0037001F"),
            Node::Document(vec![b'a', 0]),
        )]);
        let file = Ole2::open(&data).unwrap();
        let object = file.child(file.root(), "_123").unwrap();
        assert!(matches!(
            office_document(&file, object, None),
            Err(Unported(reason)) if reason.contains("Outlook message")
        ));
    }

    #[test]
    fn appends_an_extension_a_name_lacks() {
        assert_eq!(with_extension("a.DOC", "doc"), "a.DOC");
        assert_eq!(with_extension("a", ".pdf"), "a.pdf");
        assert_eq!(with_extension("a", ""), "a");
    }
}
