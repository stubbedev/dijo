//! The two detectors that name an OLE2 compound document by its entries, as [`crate::ole2`]
//! reads them: Tika's `POIFSContainerDetector` and `MiscOLEDetector`.

use std::collections::HashSet;

use crate::java;
use crate::mime::media_type::MediaType;
pub(crate) use crate::ole2::is_ole;
use crate::ole2::{EntryId, EntryKind, Ole2};

/// The compound document Tika's detectors look into: POI opened it, or it could not.
pub(crate) type OleFile<'a> = Ole2<'a>;

const OLE: &str = "application/x-tika-msoffice";
const WPS: &str = "application/vnd.ms-works";
const MS_GRAPH_CHART: &str = "application/vnd.ms-graph";
const COMP_OBJ_NAME: &str = "\u{1}COMPOBJ";

/// A directory of a compound document POI opened.
#[derive(Clone, Copy)]
struct Directory<'f, 'a> {
    file: &'f OleFile<'a>,
    id: EntryId,
}

/// `POIFSContainerDetector.detect(names, directory)` for the directory's entry names,
/// upper-cased.
fn by_names(names: &HashSet<String>, directory: Option<Directory<'_, '_>>) -> MediaType {
    let has = |name: &str| names.contains(name);
    let comp_obj = || directory.map_or(OLE, comp_obj_type);
    let found = if names.is_empty() {
        OLE
    } else if let Some(encrypted) = encrypted(names, directory) {
        encrypted
    } else if has("WORKBOOK") || has("BOOK") {
        if comp_obj() == MS_GRAPH_CHART {
            MS_GRAPH_CHART
        } else {
            "application/vnd.ms-excel"
        }
    } else if has("SWDOCCONTENTMGR") && has("SWDOCMGRTEMPSTORAGE") {
        "application/sldworks"
    } else if has("STARCALCDOCUMENT") {
        "application/vnd.stardivision.calc"
    } else if has("STARWRITERDOCUMENT") {
        "application/vnd.stardivision.writer"
    } else if has("STARDRAWDOCUMENT3") {
        comp_obj()
    } else if has("WKSSSWORKBOOK") {
        "application/x-tika-msworks-spreadsheet"
    } else if has("WORDDOCUMENT") {
        "application/msword"
    } else if has("QUILL") {
        "application/x-mspublisher"
    } else if has("POWERPOINT DOCUMENT") {
        "application/vnd.ms-powerpoint"
    } else if has("VISIODOCUMENT") {
        "application/vnd.visio"
    } else if has("\u{1}OLE10NATIVE") {
        return MediaType::named("application/x-tika-msoffice-embedded")
            .with_parameter("format", "ole10_native");
    } else if has("MATOST") || (has("CONTENTS") && has("SPELLING")) {
        WPS
    } else if has("EQUATION NATIVE") {
        "application/vnd.ms-equation"
    } else if has("\u{3}OCXNAME") {
        OLE
    } else if has("CONTENTS") && has("\u{3}OBJINFO") {
        return comp_obj_embedded();
    } else if has("CONTENTS") && has(COMP_OBJ_NAME) {
        if directory.is_some() && comp_obj() == WPS {
            WPS
        } else {
            return comp_obj_embedded();
        }
    } else if has("CONTENTS") {
        OLE
    } else if has(COMP_OBJ_NAME) && (has("PROPS") || has("PROPS9") || has("PROPS12")) {
        if names.iter().any(|name| is_project_data(name)) {
            "application/vnd.ms-project"
        } else {
            OLE
        }
    } else if has("LAYER") {
        "application/x-esri-layer"
    } else if has("DGN~MF") && has("DGN~S") && has("DGN~H") {
        "image/vnd.dgn; version=8"
    } else if names.iter().any(|name| name.starts_with("__SUBSTG1.0_")) {
        "application/vnd.ms-outlook"
    } else {
        OLE
    };
    MediaType::named(found)
}

fn comp_obj_embedded() -> MediaType {
    MediaType::named("application/x-tika-msoffice-embedded").with_parameter("format", "comp_obj")
}

/// Java's `\s\s\s\d+`, matched in full.
fn is_project_data(name: &str) -> bool {
    let mut chars = name.chars();
    (0..3).all(|_| chars.next().is_some_and(java::is_regex_space)) && {
        let rest = chars.as_str();
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
    }
}

/// `POIFSContainerDetector.checkEncrypted`.
fn encrypted(
    names: &HashSet<String>,
    directory: Option<Directory<'_, '_>>,
) -> Option<&'static str> {
    const DRM: &str = "application/x-tika-ole-drm-encrypted";
    let find = |target: &str| {
        directory.is_some_and(|d| {
            d.file
                .find_recursively(d.id, target, 0, 10, &java::to_upper_case)
        })
    };
    if names.contains("\u{6}DATASPACES") && find("\tDRMDATASPACE") {
        return Some(DRM);
    }
    if names.contains("ENCRYPTEDPACKAGE") {
        if names.contains("ENCRYPTIONINFO") {
            return Some("application/x-tika-ooxml-protected");
        }
        if names.contains("\u{6}DATASPACES") && find("DRMENCRYPTEDDATASPACE") {
            return Some(DRM);
        }
    }
    None
}

/// `processCompObjFormatType`: the application the `\1CompObj` stream names, else
/// `x-tika-msoffice`.
fn comp_obj_type(directory: Directory<'_, '_>) -> &'static str {
    let file = directory.file;
    let Some(entry) = file
        .children(directory.id)
        .find(|&i| java::to_upper_case(&file.name(i)) == COMP_OBJ_NAME)
    else {
        return OLE;
    };
    if file.kind(entry) != EntryKind::Document {
        return OLE;
    }
    let Ok(content) = file.read(entry) else {
        return OLE;
    };
    let contains = |needle: &[u8]| content.windows(needle.len()).any(|w| w == needle);
    if contains(b"MSGraph.Chart") {
        MS_GRAPH_CHART
    } else if contains(b"StarDraw") {
        "application/vnd.stardivision.draw"
    } else if contains(b"StarImpress") {
        "application/vnd.stardivision.impress"
    } else if contains(b"Quill96") {
        WPS
    } else {
        OLE
    }
}

fn upper_names(directory: Option<Directory<'_, '_>>) -> HashSet<String> {
    directory.map_or_else(HashSet::new, |d| {
        d.file
            .children(d.id)
            .map(|child| java::to_upper_case(&d.file.name(child)))
            .collect()
    })
}

/// `POIFSDocumentType.detectType(root) == OLE10_NATIVE`: whether `POIFSContainerDetector` names
/// the document an OLE 1.0 object.
pub fn is_ole10_native(file: &OleFile<'_>) -> bool {
    directory_type(file, file.root())
        == MediaType::named("application/x-tika-msoffice-embedded")
            .with_parameter("format", "ole10_native")
}

/// `POIFSDocumentType.detectType(directory)`'s detection: the type
/// `POIFSContainerDetector.detect` names a directory of an open compound document by.
pub fn directory_type(file: &OleFile<'_>, directory: EntryId) -> MediaType {
    let directory = Some(Directory {
        file,
        id: directory,
    });
    by_names(&upper_names(directory), directory)
}

/// `POIFSContainerDetector`: `None` for a document without the OLE2 signature.
pub(crate) fn poifs(data: &[u8], file: Option<&OleFile<'_>>) -> Option<MediaType> {
    if !is_ole(data) {
        return None;
    }
    let root = file.map(|file| Directory {
        file,
        id: file.root(),
    });
    Some(by_names(&upper_names(root), root))
}

/// `MiscOLEDetector`: Hancom, Corel and Quattro Pro documents.
pub(crate) fn misc(data: &[u8], file: Option<&OleFile<'_>>) -> Option<MediaType> {
    if !is_ole(data) {
        return None;
    }
    let names: HashSet<String> = file
        .map(|f| f.top_level_names().into_iter().collect())
        .unwrap_or_default();
    let quattro = || MediaType::named("application/x-quattro-pro");
    Some(if names.contains("\u{5}HwpSummaryInformation") {
        MediaType::named("application/x-hwp-v5")
    } else if names.contains("PerfectOffice_MAIN") && names.contains("SlideShow") {
        MediaType::named("application/x-corelpresentations")
    } else if names.contains("PerfectOffice_MAIN") && names.contains("PerfectOffice_OBJECTS") {
        quattro().with_parameter("version", "7-8")
    } else if !names.contains("PerfectOffice_MAIN") && names.contains("NativeContent_MAIN") {
        quattro().with_parameter("version", "9")
    } else {
        MediaType::named(OLE)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ole2::tests::compound;

    fn detect(data: &[u8]) -> (String, String) {
        let file = OleFile::open(data).ok();
        (
            poifs(data, file.as_ref()).unwrap().to_string(),
            misc(data, file.as_ref()).unwrap().to_string(),
        )
    }

    #[test]
    fn names_documents_by_their_streams() {
        assert_eq!(
            detect(&compound(&["WordDocument"], b"")).0,
            "application/msword"
        );
        assert_eq!(
            detect(&compound(&["Workbook"], b"")).0,
            "application/vnd.ms-excel"
        );
        assert_eq!(
            detect(&compound(&["EncryptionInfo", "EncryptedPackage"], b"")).0,
            "application/x-tika-ooxml-protected"
        );
        assert_eq!(
            detect(&compound(&["\u{5}HwpSummaryInformation"], b"")).1,
            "application/x-hwp-v5"
        );
        assert_eq!(
            detect(&compound(&["\u{1}Ole10Native"], b"")).0,
            "application/x-tika-msoffice-embedded; format=ole10_native"
        );
    }

    #[test]
    fn reads_the_comp_obj_stream() {
        let data = compound(&["StarDrawDocument3", "\u{1}CompObj"], b"..StarImpress..");
        assert_eq!(detect(&data).0, "application/vnd.stardivision.impress");
    }

    #[test]
    fn falls_back_to_ole_for_unreadable_files() {
        let mut data = compound(&["WordDocument"], b"");
        data.truncate(600);
        assert_eq!(detect(&data), (OLE.to_owned(), OLE.to_owned()));
        assert!(poifs(b"PK\x03\x04", None).is_none());
    }

    #[test]
    fn matches_project_names() {
        assert!(is_project_data("   112"));
        assert!(!is_project_data("   "));
        assert!(!is_project_data("  1"));
    }
}
