//! Tika's `AbstractOOXMLExtractor.handleEmbeddedOLE`: an OLE object an Office Open XML document
//! embeds, opened as a compound document from a stream. The document stored in its package
//! entry, or the file packaged in its `\1Ole10Native` stream, is parsed with the summary
//! information of the object; any other object is parsed whole as an embedded file.
//!
//! Where POI throws on the object, Tika records the exception with its stack trace in the
//! document's metadata, so this port leaves the document to Tika.

use super::ole10_native::{Ole10Native, Refusal};
use super::summary::parse_summaries;
use crate::formats::metadata::TikaMetadata;
use crate::formats::Unported;
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::{EntryKind, Ole2, Unreadable};

const OLE10_NATIVE: &str = "\u{1}Ole10Native";
const OLE_MARKER: &str = "\u{1}Ole";

/// What Tika parses of an OLE object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OleObject {
    /// A document stored in the object, parsed with this metadata.
    Document {
        data: Vec<u8>,
        metadata: TikaMetadata,
    },
    /// The object's part, parsed as an embedded file.
    Part,
    /// Nothing: an OLE 1.0 object POI finds invalid.
    Nothing,
}

/// `handleEmbeddedOLE` for the object of `part`, the bytes of the part named `part_name` a
/// relationship of id `relationship` (with its source's prefix) leads to.
pub(crate) fn open(
    part: &[u8],
    relationship: &str,
    part_name: &str,
) -> Result<OleObject, Unported> {
    let thrown = |reason: &str| {
        Unported(format!(
            "POI throws on the OLE object {part_name} ({reason}), which Tika records with its \
             stack trace"
        ))
    };
    let file = Ole2::open_stream(part).map_err(|Unreadable(reason)| thrown(reason))?;
    let root = file.root();
    let mut metadata = TikaMetadata::new();
    metadata.set("tk:embedded-resource-type", "ATTACHMENT");
    metadata.set("tk:embedded-relationship-id", relationship);
    metadata.set("tk:internal-path", part_name);
    let media_type = ole_directory_type(&file, root);
    parse_summaries(&file, root, &mut metadata)?;
    if let Some(name) = package_entry_name(&file) {
        let entry = file
            .child_ignoring_case(root, name)
            .expect("a package entry the root has");
        if file.kind(entry) != EntryKind::Document {
            return Err(thrown("its package entry is a directory"));
        }
        let data = file
            .read(entry)
            .map_err(|Unreadable(reason)| thrown(reason))?;
        return Ok(OleObject::Document { data, metadata });
    }
    let ole10_native = MediaType::named("application/x-tika-msoffice-embedded")
        .with_parameter("format", "ole10_native");
    if media_type != ole10_native {
        return Ok(OleObject::Part);
    }
    let Some(entry) = file.child_ignoring_case(root, OLE10_NATIVE) else {
        return Ok(OleObject::Nothing);
    };
    if file.kind(entry) != EntryKind::Document {
        return Err(thrown("its \\1Ole10Native entry is a directory"));
    }
    let stream = file
        .read(entry)
        .map_err(|Unreadable(reason)| thrown(reason))?;
    let native = match Ole10Native::read(&stream) {
        Ok(native) => native,
        Err(Refusal::Invalid) => return Ok(OleObject::Nothing),
        Err(Refusal::Thrown) => return Err(thrown("its \\1Ole10Native stream is malformed")),
    };
    if let Some(label) = native.label {
        metadata.set("tk:resource-name", label);
    }
    for name in [native.command, native.file_name].into_iter().flatten() {
        metadata.add("tk:orig-resource-name", name);
    }
    Ok(OleObject::Document {
        data: native.data,
        metadata,
    })
}

/// `getPackageEntryName`: the name of the entry holding a document, found by its exact name,
/// which `createDocumentInputStream` then looks up ignoring case.
fn package_entry_name(file: &Ole2<'_>) -> Option<&'static str> {
    let has = |name: &str| file.child(file.root(), name).is_some();
    if has(OLE_MARKER) {
        if let Some(name) = ["Package", "CONTENTS", "package"]
            .into_iter()
            .find(|&name| has(name))
        {
            return Some(name);
        }
    }
    has("package").then_some("package")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ole2::tests::compound;

    fn document(object: OleObject) -> (Vec<u8>, TikaMetadata) {
        match object {
            OleObject::Document { data, metadata } => (data, metadata),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn opens_the_package_entry_of_an_object() {
        let object = open(&compound(&["\u{1}Ole", "Package"], b"PK"), "rId3", "/a.bin").unwrap();
        let (data, metadata) = document(object);
        assert_eq!(data, b"PK");
        assert_eq!(metadata.get("tk:embedded-relationship-id"), Some("rId3"));
        assert_eq!(
            metadata.get("tk:embedded-resource-type"),
            Some("ATTACHMENT")
        );
        assert_eq!(metadata.get("tk:internal-path"), Some("/a.bin"));
    }

    #[test]
    fn opens_the_file_packaged_in_an_ole10_native_stream() {
        let mut body = 2i16.to_le_bytes().to_vec();
        body.extend(b"a.txt\0C:\\a.txt\0\0\0\x03\0");
        body.extend(8i32.to_le_bytes());
        body.extend(b"C:\\a.tx\0");
        body.extend(2i32.to_le_bytes());
        body.extend(b"hi\0\0");
        let mut native = (body.len() as i32).to_le_bytes().to_vec();
        native.extend(body);
        let object = open(&compound(&["\u{1}Ole10Native"], &native), "rId1", "/b.bin").unwrap();
        let (data, metadata) = document(object);
        assert_eq!(data, b"hi");
        assert_eq!(metadata.get("tk:resource-name"), Some("a.txt"));
        assert_eq!(
            metadata.values("tk:orig-resource-name"),
            ["C:\\a.tx", "C:\\a.txt"]
        );
    }

    #[test]
    fn parses_other_objects_whole() {
        let object = open(&compound(&["Equation Native"], b"x"), "rId1", "/c.bin").unwrap();
        assert_eq!(object, OleObject::Part);
    }

    #[test]
    fn leaves_objects_poi_throws_on_to_tika() {
        assert!(open(&[0xD0, 0xCF], "rId1", "/d.bin").is_err());
    }
}
