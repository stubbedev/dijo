//! PDF documents, as Tika's `PDFParser` reads them with PDFBox, under extractous's
//! `TextLayerCheckingPdfParser` when tesseract runs: the document's objects through `lopdf`,
//! then the metadata `PDFParser` collects, and the text of each page as PDFBox's
//! `PDFTextStripper` assembles it from the glyphs its content stream shows, written by
//! `PDF2XHTML` in our reading order, or for a tagged document by `PDFMarkedContent2XHTML`
//! through its structure tree. Documents with a page Tika would OCR, and documents with what the
//! port does not do yet (encryption, form fields, embedded files and fonts it cannot measure,
//! among others), are left to Tika.

mod afm;
mod cmap;
mod content;
mod cos;
mod date;
mod document;
mod encoding;
mod engine;
mod font;
mod jfloat;
mod matrix;
mod metadata;
mod position;
mod quality;
mod reading_order;
mod stripper;
mod structure;
mod tagged;
mod truetype;
mod writer;
mod xmp;

use std::sync::OnceLock;

use lopdf::Document;

use crate::config::ParserConfigs;
use crate::formats::handlers::Discard;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Failure, Parser};

use cos::{as_dict, Pdf};
use structure::StructureIndex;
use tagged::Tagged;
use writer::{DocumentWriter, PageLayer, WriteFailure};

const MEDIA_TYPE: &str = "application/pdf";
const TEXT_LAYER_CHECKING_PARSER: &str = "extractous::TextLayerCheckingPdfParser";
const PDF_PARSER: &str = "extractous::PDFParser";
/// `PagesConfig.Auto`'s defaults: a page of at most this many glyphs is OCRed...
const OCR_MAX_GLYPHS: i32 = 10;
/// ...and so is one with more glyphs than this that map to no character.
const OCR_UNMAPPED_LIMIT: f32 = 10.0;

/// Why the port leaves a document to Tika: something in it the port does not do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unported(pub(super) String);

/// Whether tesseract runs, which makes Tika read PDFs through our `TextLayerCheckingPdfParser`.
fn ocr_runs() -> bool {
    static RUNS: OnceLock<bool> = OnceLock::new();
    *RUNS.get_or_init(|| crate::ocr::tesseract_version().is_ok())
}

/// Tika's `PDFParser`, wrapped in our `TextLayerCheckingPdfParser` when tesseract runs.
pub(crate) struct PdfParser;

impl Parser for PdfParser {
    fn name(&self) -> &'static str {
        if ocr_runs() {
            TEXT_LAYER_CHECKING_PARSER
        } else {
            PDF_PARSER
        }
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn defers(
        &self,
        input: &[u8],
        _metadata: &TikaMetadata,
        configs: &ParserConfigs,
    ) -> Option<String> {
        deferral(input, configs)
            .err()
            .map(|Unported(reason)| reason)
    }

    fn parse(
        &self,
        input: &[u8],
        configs: &ParserConfigs,
        xhtml: &mut Xhtml,
    ) -> Result<(), Failure> {
        if ocr_runs() {
            xhtml.delegate_to(PDF_PARSER);
        }
        let loaded = load(input).map_err(|Unported(reason)| {
            Failure::Malformed(format!("the PDF was not deferred, though {reason}"))
        })?;
        extract(&loaded, input, configs, xhtml, false)?;
        Ok(())
    }
}

/// Why the document is left to Tika, if it is: a setting the port does not read, something in
/// the document it does not do, or a page Tika would OCR.
fn deferral(input: &[u8], configs: &ParserConfigs) -> Result<(), Unported> {
    if let Some(setting) = configs.pdf.unread_setting() {
        return Err(Unported(format!(
            "the PDF setting {setting}, which the port does not read"
        )));
    }
    let loaded = load(input)?;
    let mut layers = Vec::new();
    let (_, _, result) = Xhtml::write(&mut Discard, TikaMetadata::new(), configs, |xhtml| {
        layers = extract(&loaded, input, configs, xhtml, true)?;
        Ok(())
    });
    if let Some(number) = loaded.pdf.lost_object_needed() {
        return Err(Unported(format!(
            "object {number}, which lopdf cannot read and PDFBox may recover"
        )));
    }
    match result {
        Ok(()) => {}
        Err(WriteFailure::Unported(unported)) => return Err(unported),
        Err(WriteFailure::Stop(stop)) => {
            unreachable!("a discarded document stopped being written: {stop:?}")
        }
    }
    for (index, layer) in layers.iter().enumerate() {
        ocr_deferral(index + 1, layer)?;
    }
    Ok(())
}

/// Why Tika would OCR a page, if it would: `AbstractPDF2XHTML.ocrWanted` under its automatic
/// strategy, and our `TextLayerCheckingPdfParser`'s doubt about the page's text when tesseract
/// runs. A page of more glyphs than the limit without characters is both.
fn ocr_deferral(page: usize, layer: &PageLayer) -> Result<(), Unported> {
    if layer.glyphs <= OCR_MAX_GLYPHS || layer.unmapped as f32 > OCR_UNMAPPED_LIMIT {
        return Err(Unported(format!(
            "page {page} shows {} glyphs, {} of them without characters, which Tika OCRs",
            layer.glyphs, layer.unmapped
        )));
    }
    if !ocr_runs() {
        return Ok(());
    }
    let assessment = quality::assess(
        &layer.text,
        layer.glyphs,
        layer.unmapped,
        OCR_UNMAPPED_LIMIT,
    );
    if assessment.badness >= quality::DOUBTFUL {
        return Err(Unported(format!(
            "the text layer of page {page} is doubtful ({}), which Tika OCRs",
            assessment.fault
        )));
    }
    Ok(())
}

/// A document's objects, as `lopdf` reads them, and what the empty password opens it with.
struct Loaded {
    pdf: Pdf,
    access: Access,
}

/// The document's objects, decrypted with the empty password as Tika's `PDFParser` opens an
/// encrypted document without one. A document the empty password does not open is left to
/// Tika, which asks for a password.
fn load(input: &[u8]) -> Result<Loaded, Unported> {
    let mut doc = Document::load_mem(input)
        .map_err(|error| Unported(format!("lopdf cannot read the document: {error}")))?;
    if doc.trailer.has(b"Encrypt") {
        return Err(Unported(
            "encryption the empty password does not open, which Tika asks a password for"
                .to_owned(),
        ));
    }
    let access = match doc.encryption_state.clone() {
        None => Access::Unencrypted,
        Some(state) => Access::of(&mut doc, &state)?,
    };
    Ok(Loaded {
        pdf: Pdf::new(doc),
        access,
    })
}

/// What a document lets the one who opened it do, as PDFBox's `AccessPermission` holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Unencrypted,
    /// Opened with the owner password: everything.
    Owner,
    /// Opened with the user password: what the permission bits `P` grant.
    User(u64),
}

impl Access {
    /// `StandardSecurityHandler.prepareForDecryption` with the empty password, which opens the
    /// document as its owner when it is the owner password too, else as a user.
    fn of(doc: &mut Document, state: &lopdf::EncryptionState) -> Result<Self, Unported> {
        let dictionary = state.encode().map_err(|error| {
            Unported(format!(
                "an encryption dictionary lopdf cannot restate: {error}"
            ))
        })?;
        let id = (
            doc.objects
                .keys()
                .next_back()
                .map_or(1, |&(number, _)| number + 1),
            0,
        );
        doc.objects
            .insert(id, lopdf::Object::Dictionary(dictionary));
        doc.trailer.set("Encrypt", lopdf::Object::Reference(id));
        let owner = doc.authenticate_raw_owner_password(b"").is_ok();
        doc.trailer.remove(b"Encrypt");
        doc.objects.remove(&id);
        Ok(if owner {
            Self::Owner
        } else {
            Self::User(state.permissions().bits())
        })
    }

    /// `AccessPermission.isPermissionBitOn`.
    fn grants(self, bit: u32) -> bool {
        match self {
            Self::Unencrypted | Self::Owner => true,
            Self::User(permissions) => permissions & (1 << (bit - 1)) != 0,
        }
    }
}

/// `PDFParser.parse` from the cross-reference scan on: the metadata, then the pages and the
/// end of the document, written by the tagged writer for a document with marked content. Each
/// page's text layer is returned when `record_layers` asks for it.
fn extract(
    loaded: &Loaded,
    input: &[u8],
    configs: &ParserConfigs,
    xhtml: &mut Xhtml,
    record_layers: bool,
) -> Result<Vec<PageLayer>, WriteFailure> {
    let pdf = &loaded.pdf;
    let metadata = xhtml.metadata_mut();
    let mut startxrefs = 0;
    for offset in metadata::scan_startxrefs(input) {
        if offset.startxref == 0 {
            continue;
        }
        startxrefs += 1;
        metadata.add("pdf:eof-offsets", offset.end_eof_offset.to_string());
    }
    let updates = (startxrefs - 1).max(0).to_string();
    metadata.set("pdf:incremental-update-count", updates.clone());
    metadata.set("tk:version-count", updates);
    let catalog = pdf.catalog().ok_or_else(|| {
        Unported("a document without a catalog, which PDFBox fails on".to_owned())
    })?;
    metadata.set("pdf:has-collection", catalog.has(b"Collection").to_string());
    let acro_form = pdf.dict(catalog, b"AcroForm");
    let has_xfa = acro_form.is_some_and(|form| form.has(b"XFA"));
    metadata.set("pdf:has-xfa", has_xfa.to_string());
    let has_marked_content = StructureIndex::<()>::has_marked_content(pdf);
    metadata.set("pdf:has-marked-content", has_marked_content.to_string());
    metadata.set("Content-Type", MEDIA_TYPE);
    for (name, bit) in [
        ("access-permission:extract-for-accessibility", 10),
        ("access-permission:extract-content", 5),
        ("access-permission:assemble-document", 11),
        ("access-permission:fill-in-form", 9),
        ("access-permission:can-modify", 4),
        ("access-permission:modify-annotations", 6),
        ("access-permission:can-print", 3),
        ("access-permission:can-print-faithful", 12),
    ] {
        metadata.set(name, loaded.access.grants(bit).to_string());
    }
    let encrypted = loaded.access != Access::Unencrypted;
    metadata.set("pdf:encrypted", encrypted.to_string());
    let has_fields = acro_form
        .and_then(|form| pdf.array(form, b"Fields"))
        .is_some_and(|fields| {
            fields
                .iter()
                .any(|field| pdf.resolve(field).and_then(as_dict).is_some())
        });
    if has_fields {
        return Err(Unported(
            "form fields, whose values and signatures the port does not read".to_owned(),
        )
        .into());
    }
    let version = metadata::header_version(input)?;
    metadata::document(pdf, metadata, version, has_fields)?;
    if is_illustrator(pdf)? {
        metadata.set("Content-Type", "application/illustrator");
    }
    let reading_order = configs.pdf.reads_in_reading_order();
    let tagged = has_marked_content.then(|| Tagged::new(pdf, reading_order));
    let mut writer =
        DocumentWriter::new(pdf, xhtml, tagged, reading_order, ocr_runs(), record_layers);
    let written = writer.write();
    let layers = writer.layers.take().unwrap_or_default();
    xhtml.metadata_mut().set("pdf:ocr-page-count", "0");
    written.map(|()| layers)
}

/// `PDFParser.checkIllustrator`: whether the first page holds Illustrator's private data.
fn is_illustrator(pdf: &Pdf) -> Result<bool, Unported> {
    let pages = pdf.pages()?;
    let Some(page) = pages.first() else {
        return Ok(false);
    };
    Ok(pdf
        .dict(page.dict, b"PieceInfo")
        .and_then(|piece_info| pdf.dict(piece_info, b"Illustrator"))
        .and_then(|illustrator| pdf.dict(illustrator, b"Private"))
        .is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::parity::{
        assert_same_as_tika_delegating, conformance_corpus, documents, Scratch,
    };
    use std::path::{Path, PathBuf};

    const TIKA_PDF_PARSER: &str = "org.apache.tika.parser.pdf.PDFParser";
    const TIKA_TEXT_LAYER_CHECKING_PARSER: &str = "ai.yobix.TextLayerCheckingPdfParser";
    const DELEGATES: [(&str, &str); 1] = [(PDF_PARSER, TIKA_PDF_PARSER)];
    const CORPORA: [&str; 2] = ["../quality/corpus", "../test_files/documents"];

    /// Why a document of the corpora is left to Tika.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Why {
        /// Tika OCRs a page whatever its text layer reads.
        Ocr,
        /// Our `TextLayerCheckingPdfParser` doubts a page's text layer, when tesseract runs.
        Doubtful,
        /// The port does not do what the document needs, as the reason starts.
        Unported(&'static str),
    }

    /// The documents of the corpora the port leaves to Tika, and why.
    const DEFERRED: [(&str, Why); 13] = [
        ("arabic.pdf", Why::Ocr),
        (
            "chinese.pdf",
            Why::Unported("an embedded Type 1 font program"),
        ),
        ("deu-ocr.pdf", Why::Ocr),
        ("eng-ocr.pdf", Why::Ocr),
        ("hindi.pdf", Why::Ocr),
        (
            "hyphenated.pdf",
            Why::Unported("an embedded Type 1 font program"),
        ),
        (
            "japanese-vertical.pdf",
            Why::Unported("an embedded Type 1 font program"),
        ),
        (
            "japanese.pdf",
            Why::Unported("an embedded Type 1 font program"),
        ),
        ("prose-broken-offset.pdf", Why::Doubtful),
        (
            "prose-broken-private-use.pdf",
            Why::Unported("a font PDFBox measures with its fallback"),
        ),
        ("prose-scan.pdf", Why::Ocr),
        ("thai.pdf", Why::Ocr),
        (
            "user-password.pdf",
            Why::Unported("encryption the empty password does not open"),
        ),
    ];

    fn tika_name() -> &'static str {
        if ocr_runs() {
            TIKA_TEXT_LAYER_CHECKING_PARSER
        } else {
            TIKA_PDF_PARSER
        }
    }

    fn as_paths(paths: &[PathBuf]) -> Vec<&Path> {
        paths.iter().map(PathBuf::as_path).collect()
    }

    fn corpora() -> Vec<&'static Path> {
        CORPORA.iter().map(Path::new).collect()
    }

    fn deferral_of(path: &Path) -> String {
        let input = std::fs::read(path).unwrap();
        PdfParser
            .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
            .unwrap_or_else(|| panic!("{} is not deferred", path.display()))
    }

    /// A document of the given objects, numbered from 1 with the catalog first, and a
    /// cross-reference table whose entries `shift` moves off their objects.
    fn document(objects: &[&str], prefix: &[u8], shift: &[(usize, usize)]) -> Vec<u8> {
        let mut out = prefix.to_vec();
        let start = out.len();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len() - start);
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len() - start;
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for (index, offset) in offsets.iter().enumerate() {
            let moved = shift
                .iter()
                .find(|(object, _)| *object == index + 1)
                .map_or(0, |(_, by)| *by);
            out.extend_from_slice(format!("{:010} 00000 n \n", offset + moved).as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// A page of Helvetica text drawn by `content`.
    fn page_objects(content: &str) -> Vec<String> {
        vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len() + 1
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_owned(),
        ]
    }

    const TEXT: &str =
        "BT /F1 12 Tf 72 720 Td (Det var en gang en konge, som havde tre d\\370tre.) Tj \
                        0 -14 Td (Den yngste var den smukkeste af dem alle sammen.) Tj ET";

    #[test]
    fn extracts_documents_lopdf_recovers_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-recovered");
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let plain = scratch.write("plain.pdf", &document(&objects, b"", &[]));
        let marked = scratch.write("marked.pdf", &document(&objects, b"\xef\xbb\xbf", &[]));
        assert_same_as_tika_delegating(&[&plain, &marked], tika_name(), &DELEGATES);
    }

    #[test]
    fn defers_a_needed_object_lopdf_cannot_read() {
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[(4, 3)]);
        assert_eq!(
            PdfParser
                .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            Some("object 4, which lopdf cannot read and PDFBox may recover")
        );
    }

    #[test]
    fn grants_what_the_permission_bits_grant_to_a_user() {
        let user = Access::User(0xFFFF_FFEC);
        assert!(user.grants(3) && user.grants(4) && !user.grants(2));
        let no_extraction = Access::User(-532_i64 as u64);
        assert!(!no_extraction.grants(5) && !no_extraction.grants(10) && no_extraction.grants(3));
        assert!(Access::Owner.grants(5) && Access::Unencrypted.grants(10));
    }

    #[test]
    fn extracts_what_tikas_path_extracts() {
        let (parsed, _) = documents(&PdfParser, &corpora());
        assert_same_as_tika_delegating(&as_paths(&parsed), tika_name(), &DELEGATES);
    }

    #[test]
    fn defers_what_tika_ocrs_and_what_the_port_does_not_do() {
        let (_, deferred) = documents(&PdfParser, &corpora());
        let mut found: Vec<(String, String)> = deferred
            .iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_str().unwrap().to_owned();
                (name, deferral_of(path))
            })
            .collect();
        found.sort();
        let expected: Vec<(&str, Why)> = DEFERRED
            .iter()
            .copied()
            .filter(|(_, why)| ocr_runs() || *why != Why::Doubtful)
            .collect();
        assert_eq!(
            found
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            expected.iter().map(|(name, _)| *name).collect::<Vec<_>>()
        );
        for ((name, reason), (_, why)) in found.iter().zip(&expected) {
            let as_expected = match why {
                Why::Ocr => reason.starts_with("page ") && reason.ends_with("which Tika OCRs"),
                Why::Doubtful => reason.contains("is doubtful"),
                Why::Unported(start) => reason.starts_with(start),
            };
            assert!(as_expected, "{name}: {reason}");
        }
    }

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_the_conformance_documents_as_tikas_path_does() {
        let (parsed, deferred) = documents(&PdfParser, &[&conformance_corpus()]);
        for path in &deferred {
            println!("deferred {}: {}", path.display(), deferral_of(path));
        }
        println!("{} parsed, {} deferred", parsed.len(), deferred.len());
        assert_same_as_tika_delegating(&as_paths(&parsed), tika_name(), &DELEGATES);
    }
}
