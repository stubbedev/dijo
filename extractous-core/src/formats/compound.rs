//! Compound documents whose content Tika's `OfficeParser` does not read: Works, Project and
//! SolidWorks documents, StarOffice documents, Corel presentations and compound documents of no
//! known kind. Tika writes their summary information as metadata and, where POI names their
//! kind, sets it as the content type.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::Summaries;
use crate::formats::{Declined, Parser, Prepared, Unported};
use crate::mime::{ole_directory_type, MediaType};
use crate::ole2::{Ole2, Unreadable};

/// The types `POIFSDocumentType` knows among those this parser reads, which `OfficeParser`
/// sets as the content type.
const DOCUMENT_TYPES: [&str; 3] = [
    "application/vnd.ms-works",
    "application/vnd.ms-project",
    "application/sldworks",
];

/// The entry of an encrypted package, on which `OfficeParser` throws for a document of a kind it
/// reads nothing of but its summary.
const ENCRYPTED_PACKAGE: &str = "EncryptedPackage";

/// Tika's `OfficeParser` of the compound documents it reads the summary information of only.
pub(crate) struct CompoundDocumentParser;

impl Parser for CompoundDocumentParser {
    fn name(&self) -> &'static str {
        "extractous::OfficeParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.microsoft.OfficeParser"
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
            "application/x-tika-msoffice",
        ]
    }

    /// Defers documents POI does not open as this port does, macros extracted when asked to and
    /// an encrypted package Tika throws on.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
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

/// The summary information of a compound document, and the content type `OfficeParser` sets for
/// it, if any.
fn read(
    input: &[u8],
    configs: &ParserConfigs,
) -> Result<(Summaries, Option<&'static str>), Unported> {
    if configs.office.extracts_macros() {
        return Err(Unported(
            "macros are extracted, which this port does not do".to_owned(),
        ));
    }
    let file = Ole2::open(input).map_err(|Unreadable(reason)| {
        Unported(format!("POI cannot open the compound document: {reason}"))
    })?;
    if let Err(Unreadable(reason)) = Ole2::open_stream(input) {
        return Err(Unported(format!(
            "POI reads the compound document from a stream differently: {reason}"
        )));
    }
    let root = file.root();
    let detected = ole_directory_type(&file, root);
    let document_type = DOCUMENT_TYPES
        .into_iter()
        .find(|&known| detected == MediaType::named(known));
    let silent_kind = matches!(
        document_type,
        Some("application/vnd.ms-project" | "application/sldworks")
    );
    if !silent_kind && file.child(root, ENCRYPTED_PACKAGE).is_some() {
        return Err(Unported(
            "an encrypted package, on which Tika throws an EncryptedDocumentException".to_owned(),
        ));
    }
    Ok((Summaries::read(&file, root)?, document_type))
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{assert_same_as_tika, conformance_corpus, documents, Scratch};
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
        ];
        let deferred_one = scratch.write(
            "encrypted.wps",
            &compound(&["MatOST", ENCRYPTED_PACKAGE], b"x"),
        );
        let (parsed, deferred) = documents(&CompoundDocumentParser, &[scratch.directory()]);
        assert_eq!(deferred, [deferred_one], "{deferred:?}");
        assert_eq!(parsed.len(), read.len(), "{parsed:?}");
        assert_same_as_tika(&paths(&read), CompoundDocumentParser.tika_name());
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_compound_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&CompoundDocumentParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 21, "{parsed:?}");
        assert_same_as_tika(&paths(&parsed), CompoundDocumentParser.tika_name());
    }
}
