//! Compound documents whose content Tika's `OfficeParser` does not read: Works, Project and
//! SolidWorks documents, StarOffice documents, Corel presentations, Quattro Pro 7 and 8
//! workbooks, OLE objects embedded whole and compound documents of no known kind. Tika writes
//! their summary information as metadata and, where POI names their kind, sets it as the content
//! type.

use crate::config::ParserConfigs;
use crate::formats::crypt;
use crate::formats::doc::DocParser;
use crate::formats::metadata::TikaMetadata;
use crate::formats::msg::MsgParser;
use crate::formats::ole2::office;
use crate::formats::ole2::vba::Macros;
use crate::formats::ole2::Summaries;
use crate::formats::poi::Unread;
use crate::formats::ppt::PptParser;
use crate::formats::publisher::PublisherParser;
use crate::formats::visio::VisioParser;
use crate::formats::xls::XlsParser;
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

    /// Fails a document POI does not open, a DRM encrypted document and an encrypted package as
    /// `OfficeParser` does; writes the macros when asked to.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let (summaries, document_type, macros) = read(input, configs)?;
        Macros::followed_by(
            Ok(Box::new(move |xhtml| {
                let metadata = xhtml.metadata_mut();
                summaries.write(metadata);
                if let Some(document_type) = document_type {
                    metadata.set("Content-Type", document_type);
                }
                Ok(())
            })),
            macros,
        )
    }
}

/// The native parsers of the kinds `POIFSDocumentType` detects whose content `OfficeParser`
/// reads, by those kinds.
const READERS: [(&[&str], &dyn Parser); 6] = [
    (&["application/msword"], &DocParser),
    (
        &[
            "application/vnd.ms-excel",
            "application/x-tika-msworks-spreadsheet",
        ],
        &XlsParser,
    ),
    (&["application/vnd.ms-powerpoint"], &PptParser),
    (&["application/vnd.ms-outlook"], &MsgParser),
    (&["application/vnd.visio"], &VisioParser),
    (&["application/x-mspublisher"], &PublisherParser),
];

/// The native parser `OfficeParser` reads the compound document `input` as, once
/// `POIFSDocumentType` has detected its kind, where that is not one of `own`, the kinds of the
/// parser it was given to: `OfficeParser` reads whatever kind it detects, whichever type Tika's
/// path gave it the document as. A kind it reads only the summary information of, as
/// [`CompoundDocumentParser`] does, is an MS Graph chart by its `CompObj` stream, another kind
/// `POIFSDocumentType` knows of no content, or no known kind: Tika's path hands a chart to
/// `OfficeParser` as a workbook, stored in another document as an open storage its detection
/// names a workbook by its entry names alone, or as an `Excel.Chart.8` object of a
/// presentation, typed so by its program id.
pub(crate) fn detected_otherwise(input: &[u8], own: &[&str]) -> Option<&'static dyn Parser> {
    let file = Ole2::open(input).ok()?;
    let detected = ole_directory_type(&file, file.root());
    let is = |name: &str| detected == MediaType::named(name);
    if own.iter().any(|&name| is(name)) {
        return None;
    }
    if is(UNKNOWN_COMPOUND) || DOCUMENT_TYPES.iter().any(|&known| is(known)) {
        return Some(&CompoundDocumentParser);
    }
    READERS
        .iter()
        .find(|(kinds, _)| kinds.iter().any(|&kind| is(kind)))
        .map(|&(_, parser)| parser)
}

/// The summary information of a compound document, the content type `OfficeParser` sets for
/// it, if any, and its macros where they are asked for; or why it does not read it as Tika's
/// parser does.
fn read(
    input: &[u8],
    configs: &ParserConfigs,
) -> Result<(Summaries, Option<&'static str>, Option<Macros>), Decline<'static>> {
    let file = office::open_file(input).map_err(Unread::declined)?;
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
    Ok((summaries, document_type, Macros::of(&file, configs)))
}

#[cfg(test)]
mod detection {
    use super::*;
    use crate::ole2::tests::compound;

    /// A compound document goes to the parser of the kind POI detects in it, as `OfficeParser`
    /// reads whatever kind it detects, where that is not the kind it was given as.
    #[test]
    fn reads_a_compound_document_as_the_kind_poi_detects() {
        let kinds = |parser: Option<&dyn Parser>| parser.map(Parser::media_types);
        let workbook = compound(&["Workbook"], b"cells");
        assert_eq!(
            kinds(detected_otherwise(&workbook, &["application/msword"])),
            Some(XlsParser.media_types())
        );
        assert!(detected_otherwise(&workbook, &["application/vnd.ms-excel"]).is_none());
        let unknown = compound(&["Mystery"], b"data");
        assert_eq!(
            kinds(detected_otherwise(&unknown, &["application/msword"])),
            Some(CompoundDocumentParser.media_types())
        );
        assert!(detected_otherwise(b"no compound document", &["application/msword"]).is_none());
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
