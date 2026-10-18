//! Compound documents whose content Tika's `OfficeParser` does not read: Works, Project and
//! SolidWorks documents, StarOffice documents, Corel presentations, Quattro Pro 7 and 8
//! workbooks, OLE objects embedded whole and compound documents of no known kind. Tika writes
//! their summary information as metadata and, where POI names their kind, sets it as the content
//! type.

use crate::config::ParserConfigs;
use crate::formats::crypt;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::office;
use crate::formats::ole2::Summaries;
use crate::formats::poi::Unread;
use crate::formats::{Decline, Footprint, Parser, Prepared};
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::Ole2;

/// `POIFSContainerDetector.MS_GRAPH_CHART`.
const MS_GRAPH_CHART: &str = "application/vnd.ms-graph";
/// A Quattro Pro 7 or 8 workbook, a compound document no parser of Tika's reads by its type, so
/// that Tika's composite parser hands it to `OfficeParser` by its supertype, which reads its
/// summary information only.
const QUATTRO_PRO_7_8: &str = "application/x-quattro-pro; version=7-8";

/// The types `POIFSDocumentType` knows among those this parser reads, which `OfficeParser`
/// sets as the content type.
const DOCUMENT_TYPES: [&str; 7] = [
    "application/vnd.ms-works",
    "application/vnd.ms-project",
    "application/sldworks",
    MS_GRAPH_CHART,
    DRM_ENCRYPTED,
    "application/x-tika-msoffice-embedded; format=ole10_native",
    "application/x-tika-msoffice-embedded; format=comp_obj",
];

/// The type of the compound documents of no known kind, whose subtypes no parser of their own
/// reads, such as an OLE object embedded whole, Tika's composite parser hands `OfficeParser`.
const UNKNOWN_COMPOUND: &str = "application/x-tika-msoffice";

/// The entry of an encrypted package, on which `OfficeParser` throws for a document of a kind it
/// reads nothing of but its summary.
const ENCRYPTED_PACKAGE: &str = "EncryptedPackage";

/// `POIFSDocumentType.DRMENCRYPTED`, which `OfficeParser` throws on once it has read the summary
/// information.
const DRM_ENCRYPTED: &str = "application/x-tika-ole-drm-encrypted";

/// The message of the `EncryptedDocumentException` `OfficeParser` throws on a DRM encrypted
/// document.
const DRM_UNSUPPORTED: &str = "DRM encrypted document is not yet supported by Apache POI";

/// The message of the `EncryptedDocumentException` `OfficeParser` throws on a document of a
/// kind it reads nothing of but its summary, where it has an encrypted package.
const UNRECOGNIZED_PACKAGE: &str = "OLE2 file with an unrecognized EncryptedPackage entry";

/// Tika's `OfficeParser` of the compound documents it reads the summary information of only.
pub struct CompoundDocumentParser;

impl Parser for CompoundDocumentParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }
    fn tika_name(&self) -> &'static str {
        crate::formats::poi::OFFICE_PARSER
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/vnd.ms-works",
            "application/vnd.ms-project",
            "application/sldworks",
            "application/vnd.stardivision.calc",
            "application/vnd.stardivision.draw",
            "application/vnd.stardivision.impress",
            "application/vnd.stardivision.writer",
            "application/x-corelpresentations",
            QUATTRO_PRO_7_8,
            UNKNOWN_COMPOUND,
            DRM_ENCRYPTED,
        ]
    }

    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &[UNKNOWN_COMPOUND]
    }

    /// It reads only the directory of the compound document and its summary information, in
    /// place.
    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    /// Defers macros extracted when asked to, and fails a document POI does not open, a DRM
    /// encrypted document and an encrypted package as `OfficeParser` does.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let (summaries, document_type) = read(input, configs)?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            summaries.write(metadata);
            if let Some(document_type) = document_type {
                metadata.set("Content-Type", document_type);
            }
            Ok(())
        }))
    }
}

/// Whether POI's container detection names the compound document `input` a kind `OfficeParser`
/// reads only the summary information of, as this parser does: an MS Graph chart by its
/// `CompObj` stream, another kind `POIFSDocumentType` knows of no content, or no known kind.
/// Tika's path hands such a document to `OfficeParser` as a workbook where the type it is
/// given says so: a chart stored in another document, as an open storage its detection names a
/// workbook by its entry names alone, or an `Excel.Chart.8` object of a presentation, typed so
/// by its program id; `OfficeParser` detects it again from the document itself.
pub(crate) fn reads_only_the_summary(input: &[u8]) -> bool {
    Ole2::open(input).is_ok_and(|file| {
        let detected = ole_directory_type(&file, file.root());
        detected == MediaType::named(UNKNOWN_COMPOUND)
            || DOCUMENT_TYPES
                .iter()
                .any(|&known| detected == MediaType::named(known))
    })
}

/// The summary information of a compound document, the content type `OfficeParser` sets for
/// it, if any, or why it does not read it as Tika's parser does.
fn read(
    input: &[u8],
    configs: &ParserConfigs,
) -> Result<(Summaries, Option<&'static str>), Decline<'static>> {
    let file = office::open_file(input, configs).map_err(Unread::declined)?;
    let root = file.root();
    let summaries = Summaries::read(&file, root)?;
    let detected = ole_directory_type(&file, root);
    let document_type = DOCUMENT_TYPES
        .into_iter()
        .find(|&known| detected == MediaType::named(known));
    let silent_kind = matches!(
        document_type,
        Some("application/vnd.ms-project" | "application/sldworks" | DRM_ENCRYPTED)
    );
    if document_type == Some(DRM_ENCRYPTED) {
        return Err(Decline::Fails(crypt::encrypted_document(DRM_UNSUPPORTED)));
    }
    if !silent_kind && file.child(root, ENCRYPTED_PACKAGE).is_some() {
        return Err(Decline::Fails(crypt::encrypted_document(
            UNRECOGNIZED_PACKAGE,
        )));
    }
    Ok((summaries, document_type))
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, errors, Scratch,
    };
    use crate::ole2::tests::compound;

    fn paths(files: &[PathBuf]) -> Vec<&Path> {
        files.iter().map(PathBuf::as_path).collect()
    }

    #[test]
    fn extracts_compound_documents_as_tikas_path_does() {
        let scratch = Scratch::new("compound");
        let read = [
            scratch.write("works.wps", &compound(&["MatOST"], b"works")),
            scratch.write(
                "solid.sldprt",
                &compound(&["SwDocContentMgr/", "SwDocMgrTempStorage/"], b""),
            ),
            scratch.write("writer.sdw", &compound(&["StarWriterDocument"], b"star")),
            scratch.write("calc.sdc", &compound(&["StarCalcDocument"], b"star")),
            scratch.write("unknown.ole", &compound(&["Mystery"], b"data")),
            scratch.write(
                "object.bin",
                &compound(&["CONTENTS", "\u{1}CompObj"], b"object"),
            ),
            scratch.write("native.bin", &compound(&["\u{1}Ole10Native"], b"native")),
        ];
        let encrypted = scratch.write(
            "encrypted.wps",
            &compound(&["MatOST", ENCRYPTED_PACKAGE], b"x"),
        );
        let (parsed, deferred) = documents(&CompoundDocumentParser, &[scratch.directory()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), read.len() + 1, "{parsed:?}");
        assert_same_as_tika(&paths(&read), CompoundDocumentParser.tika_name());
        let (native, tika) = errors(&encrypted, &ParserConfigs::default());
        assert_eq!(native, tika);
        assert_eq!(
            native,
            format!("{:?}", crypt::encrypted_document(UNRECOGNIZED_PACKAGE))
        );
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_compound_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&CompoundDocumentParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        let (failing, read): (Vec<PathBuf>, Vec<PathBuf>) = parsed
            .into_iter()
            .partition(|path| path.ends_with("testWORD_protected_drm.doc"));
        assert_eq!(read.len(), 22, "{read:?}");
        assert_eq!(failing.len(), 1);
        assert_same_as_tika(&paths(&read), CompoundDocumentParser.tika_name());
        assert_fails_as_tika(&paths(&failing));
    }

    /// A DRM encrypted document fails as `OfficeParser` fails it, natively, where it went to
    /// Tika's path (#119).
    #[test]
    fn fails_a_drm_encrypted_document_as_tikas_path_does() {
        let scratch = Scratch::new("compound-drm");
        let drm = scratch.write(
            "drm.doc",
            &compound(
                &["\u{6}DataSpaces/", "\tDRMDataSpace", "WordDocument"],
                b"x",
            ),
        );
        let input = std::fs::read(&drm).unwrap();
        assert!(matches!(
            CompoundDocumentParser.prepare(&input, &TikaMetadata::new(), &ParserConfigs::default()),
            Err(Decline::Fails(error))
                if error.to_string() == crypt::encrypted_document(DRM_UNSUPPORTED).to_string()
        ));
        assert_fails_as_tika(&[&drm]);
    }
}

#[cfg(test)]
mod large {
    use extractous_core::large::LargeFile;

    use crate::input::MAX_READ_WHOLE;

    /// A compound document too large to read whole is read as far as its directory and summary
    /// information, in place, in memory that does not grow with it.
    #[test]
    fn reads_compound_documents_too_large_to_read_whole_in_bounded_memory() {
        let content = vec![0x5A; MAX_READ_WHOLE as usize + 1];
        let document = crate::ole2::tests::compound(&["Contents"], &content);
        drop(content);
        let file = LargeFile::new("large.ole", &document, 0, |_| Vec::new(), b"");
        drop(document);
        let (text, metadata) = file.extract("extractous::OfficeParser");
        assert_eq!(text.trim(), "");
        assert_eq!(metadata["Content-Type"], ["application/x-tika-msoffice"]);
    }
}
