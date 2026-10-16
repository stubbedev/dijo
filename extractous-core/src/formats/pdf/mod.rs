//! PDF documents, as Tika's `PDFParser` reads them with PDFBox, under extractous's
//! `TextLayerCheckingPdfParser` when tesseract runs: the document's objects through `lopdf`,
//! then the metadata `PDFParser` collects, and the text of each page as PDFBox's
//! `PDFTextStripper` assembles it from the glyphs its content stream shows, written by
//! `PDF2XHTML` in our reading order, or for a tagged document by `PDFMarkedContent2XHTML`
//! through its structure tree, then the bookmarks, the files the document embeds, parsed as
//! embedded documents, and the form. Documents with a page Tika would OCR, and documents with
//! what the port does not do (encryption the empty password does not open, XFA forms and fonts
//! it cannot measure, among others), are left to Tika.

mod afm;
mod attachment;
mod cff;
mod cmap;
mod content;
mod cos;
mod document;
mod encoding;
mod engine;
mod font;
mod form;
mod jfloat;
mod matrix;
mod metadata;
mod ocr;
mod position;
mod quality;
mod reading_order;
mod recording;
mod stripper;
mod structure;
mod tagged;
mod truetype;
mod type1;
mod writer;
mod xfa;
mod xref;

use lopdf::Document;

use crate::config::{MarkedContentStrategy, ParserConfigs, PdfParserConfig};
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Stop;
use crate::formats::{Declined, Parser, Prepared};

use cos::Pdf;
use ocr::OcrCheck;
use recording::Recording;
use structure::StructureIndex;
use tagged::Tagged;
use writer::{DocumentWriter, WriteFailure};

const MEDIA_TYPE: &str = "application/pdf";
const TEXT_LAYER_CHECKING_PARSER: &str = "extractous::TextLayerCheckingPdfParser";
const PDF_PARSER: &str = "extractous::PDFParser";
#[cfg(test)]
const TIKA_PDF_PARSER: &str = "org.apache.tika.parser.pdf.PDFParser";
#[cfg(test)]
const TIKA_TEXT_LAYER_CHECKING_PARSER: &str = "ai.yobix.TextLayerCheckingPdfParser";
/// The parser our `TextLayerCheckingPdfParser` records delegating to, under both names.
#[cfg(test)]
const DELEGATES: [(&str, &str); 1] = [(PDF_PARSER, TIKA_PDF_PARSER)];
/// `AbstractPDF2XHTML.MAX_RECURSION_DEPTH`.
const MAX_RECURSION_DEPTH: usize = 100;

pub(super) use crate::formats::Unported;
use crate::ocr::tesseract_runs as ocr_runs;

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

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        if ocr_runs() {
            TIKA_TEXT_LAYER_CHECKING_PARSER
        } else {
            TIKA_PDF_PARSER
        }
    }

    #[cfg(test)]
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &DELEGATES
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let recording = record(input, metadata, configs)?;
        Ok(Box::new(move |xhtml| {
            let replayed = recording.replay(xhtml);
            xhtml.metadata_mut().set("pdf:ocr-page-count", "0");
            replayed
        }))
    }
}

/// The PDF settings the port reads, as Tika's `PDFParser` and our `TextLayerCheckingPdfParser`
/// take them.
#[derive(Debug, Clone, Copy)]
struct Settings {
    ocr: OcrCheck,
    /// Whether untagged pages are put into reading order.
    reading_order: bool,
    marked_content: MarkedContentStrategy,
    bookmarks: bool,
    annotation_text: bool,
}

impl Settings {
    /// The settings `config` sets, or why the port leaves documents to Tika under them.
    fn of(config: &PdfParserConfig) -> Result<Self, Unported> {
        if config.extracts_inline_images() {
            return Err(Unported(
                "the PDF setting setExtractInlineImages, which the port does not read".to_owned(),
            ));
        }
        Ok(Self {
            ocr: OcrCheck::of(config, ocr_runs())?,
            reading_order: config.reads_in_reading_order(),
            marked_content: config.marked_content(),
            bookmarks: config.extracts_bookmarks_text(),
            annotation_text: config.extracts_annotation_text(),
        })
    }
}

/// The document's extraction, recorded from `metadata`, unless the document is left to Tika:
/// for a setting the port does not read, something in the document it does not do, or a page
/// Tika would OCR. The document is read once, into the recording, which holds no more of it
/// than its output and the files it embeds.
fn record(
    input: &[u8],
    metadata: &TikaMetadata,
    configs: &ParserConfigs,
) -> Result<Recording, Declined> {
    let deferred = |Unported(reason)| Declined::Deferred(reason);
    let settings = Settings::of(&configs.pdf).map_err(deferred)?;
    let loaded = load(input).map_err(deferred)?;
    let mut recording = Recording::new(metadata.clone());
    if ocr_runs() {
        recording.delegate_to(PDF_PARSER);
    }
    let recorded =
        extract(&loaded, input, settings, &mut recording).map_err(|failure| match failure {
            WriteFailure::Unported(Unported(reason)) => Declined::Deferred(reason),
            WriteFailure::Stop(Stop::MemoryLimit(limit)) => {
                Declined::Fails(Error::MemoryLimit(limit))
            }
            WriteFailure::Stop(stop) => {
                unreachable!("a recording stops only at the memory budget, not at {stop:?}")
            }
        });
    if matches!(recorded, Err(Declined::Fails(_))) {
        return recorded.map(|()| recording);
    }
    if let Some(number) = loaded.pdf.lost_object_needed() {
        return Err(Declined::Deferred(format!(
            "object {number}, which lopdf cannot read and PDFBox may recover"
        )));
    }
    let unlisted = loaded.pdf.unlisted_objects_needed();
    if let Some((number, _)) = xref::first_found(input, &unlisted).map_err(deferred)? {
        return Err(Declined::Deferred(format!(
            "object {number}, which the cross-reference table does not list and PDFBox finds \
             by searching the document"
        )));
    }
    recorded.map(|()| recording)
}

/// A document's objects, as `lopdf` reads them, and what the empty password opens it with.
struct Loaded {
    pdf: Pdf,
    access: Access,
}

/// The document's objects where PDFBox finds them, decrypted with the empty password as Tika's
/// `PDFParser` opens an encrypted document without one. A document the empty password does
/// not open is left to Tika, which asks for a password.
fn load(input: &[u8]) -> Result<Loaded, Unported> {
    let resolution = xref::resolve(input)?;
    let mut doc = match Document::load_mem(input) {
        Ok(doc) if !resolution.needs_rewrite(&doc, input)? => doc,
        _ => Document::load_mem(&resolution.rewritten(input)?)
            .map_err(|error| Unported(format!("lopdf cannot read the document: {error}")))?,
    };
    resolution.set_trailer(&mut doc);
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
        pdf: Pdf::new(doc, input, resolution.listed()),
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
/// end of the document, written by the tagged writer for a document with marked content, into
/// `out`.
fn extract(
    loaded: &Loaded,
    input: &[u8],
    settings: Settings,
    out: &mut Recording,
) -> Result<(), WriteFailure> {
    let pdf = &loaded.pdf;
    let mut startxrefs = 0;
    for offset in metadata::scan_startxrefs(input) {
        if offset.startxref == 0 {
            continue;
        }
        startxrefs += 1;
        out.add("pdf:eof-offsets", offset.end_eof_offset.to_string());
    }
    let updates = (startxrefs - 1).max(0).to_string();
    out.set("pdf:incremental-update-count", updates.clone());
    out.set("tk:version-count", updates);
    let catalog = pdf.catalog().ok_or_else(|| {
        Unported("a document without a catalog, which PDFBox fails on".to_owned())
    })?;
    out.set("pdf:has-collection", catalog.has(b"Collection").to_string());
    let acro_form = pdf.dict(catalog, b"AcroForm");
    let has_xfa = acro_form.is_some_and(|form| form.has(b"XFA"));
    out.set("pdf:has-xfa", has_xfa.to_string());
    let has_marked_content = StructureIndex::<()>::has_marked_content(pdf);
    out.set("pdf:has-marked-content", has_marked_content.to_string());
    out.set("Content-Type", MEDIA_TYPE);
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
        out.set(name, loaded.access.grants(bit).to_string());
    }
    let encrypted = loaded.access != Access::Unencrypted;
    out.set("pdf:encrypted", encrypted.to_string());
    let has_fields = form::has_fields(pdf)?;
    let version = metadata::header_version(input)?;
    metadata::document(pdf, out, version, has_fields)?;
    form::signatures(pdf, out)?;
    if is_illustrator(pdf)? {
        out.set("Content-Type", "application/illustrator");
    }
    let tagged = match settings.marked_content {
        _ if !has_marked_content => None,
        MarkedContentStrategy::None => None,
        MarkedContentStrategy::Auto => Some(Tagged::new(pdf, settings.reading_order, true)),
        MarkedContentStrategy::Tags => Some(Tagged::new(pdf, settings.reading_order, false)),
    };
    DocumentWriter::new(pdf, out, tagged, settings).write()
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
    use crate::config::PdfOcrStrategy;
    use crate::formats::parity::{
        assert_same_as_tika_delegating, assert_same_as_tika_delegating_with, conformance_corpus,
        documents, Scratch,
    };
    use std::path::{Path, PathBuf};

    const CORPORA: [&str; 2] = ["../quality/corpus", "../test_files/documents"];

    /// Why a document of the corpora is left to Tika.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Why {
        /// Tika OCRs a page of next to no glyphs, whatever its text layer reads.
        Ocr,
        /// Tika OCRs a page of too many glyphs without characters: by their count, or for its
        /// doubtful text layer when tesseract runs.
        Unmapped,
        /// Our `TextLayerCheckingPdfParser` doubts a page's text layer, when tesseract runs.
        Doubtful,
        /// The port does not do what the document needs, as the reason starts.
        Unported(&'static str),
    }

    /// The documents of the corpora the port leaves to Tika, and why.
    const DEFERRED: [(&str, Why); 9] = [
        ("arabic.pdf", Why::Unmapped),
        ("deu-ocr.pdf", Why::Ocr),
        ("eng-ocr.pdf", Why::Ocr),
        ("hindi.pdf", Why::Unmapped),
        ("prose-broken-offset.pdf", Why::Doubtful),
        ("prose-broken-private-use.pdf", Why::Doubtful),
        ("prose-scan.pdf", Why::Ocr),
        ("thai.pdf", Why::Unmapped),
        (
            "user-password.pdf",
            Why::Unported("encryption the empty password does not open"),
        ),
    ];

    fn tika_name() -> &'static str {
        PdfParser.tika_name()
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

    /// A document of the given objects, numbered from 1 with the catalog first, and a
    /// cross-reference stream listing them, and each object of `dangling` at offset 0.
    fn stream_document(objects: &[&str], dangling: &[u32]) -> Vec<u8> {
        let mut out = b"%PDF-1.5\n".to_vec();
        let mut rows: Vec<(u32, u32)> = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            rows.push((index as u32 + 1, out.len() as u32));
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        rows.extend(dangling.iter().map(|&number| (number, 0)));
        let stream_number = rows.iter().map(|&(number, _)| number).max().unwrap_or(0) + 1;
        let start = out.len() as u32;
        rows.push((stream_number, start));
        rows.sort_unstable();
        let mut data = Vec::new();
        for (_, offset) in &rows {
            data.push(1);
            data.extend_from_slice(&offset.to_be_bytes());
            data.extend_from_slice(&0u16.to_be_bytes());
        }
        let index: Vec<String> = rows
            .iter()
            .map(|(number, _)| format!("{number} 1"))
            .collect();
        out.extend_from_slice(
            format!(
                "{stream_number} 0 obj\n<< /Type /XRef /Size {} /Root 1 0 R /W [1 4 2] \
                 /Index [{}] /Length {} >>\nstream\n",
                stream_number + 1,
                index.join(" "),
                data.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&data);
        out.extend_from_slice(
            format!("\nendstream\nendobj\nstartxref\n{start}\n%%EOF\n").as_bytes(),
        );
        out
    }

    /// Where `needle` first stands in `haystack`.
    fn find(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap_or_else(|| panic!("{} is missing", String::from_utf8_lossy(needle)))
    }

    /// `haystack` with the first `needle` replaced by `replacement`.
    fn replace(haystack: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
        let at = find(haystack, needle);
        [&haystack[..at], replacement, &haystack[at + needle.len()..]].concat()
    }

    fn deferral(path: &Path) -> Option<String> {
        let input = std::fs::read(path).unwrap();
        PdfParser.defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
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
    fn extracts_form_fields_and_signatures_as_tikas_path_does() {
        let mut objects = page_objects(TEXT);
        objects[0] = "<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>".to_owned();
        objects.extend(
            [
                "<< /Fields [7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 15 0 R 17 0 R 18 0 R] \
                 /DA (/Helv 0 Tf 0 g) >>",
                "<< /FT /Tx /T (name) /TU (Your name) /V (Kari Nordmann) >>",
                "<< /FT /Btn /T (agree) /V /1 /Opt [(no) (yes)] >>",
                "<< /FT /Btn /Ff 65536 /T (send) /TU (Send the form) >>",
                "<< /FT /Ch /T (colours) /V [(red) (bl\\345)] >>",
                "<< /T (address) /V /Home /Kids [13 0 R 14 0 R] >>",
                "<< /T (orphan) /V (no type) >>",
                "<< /FT /Tx /Parent 11 0 R /T (street) /V (Storgata 1) >>",
                "<< /Parent 11 0 R /T (city) /FT /Tx >>",
                "<< /FT /Sig /T (signature) /V 16 0 R >>",
                "<< /Type /Sig /Filter /Adobe.PPKLite /Name (Ola Nordmann) \
                 /M (D:20240102030405+01'30') /Reason (Approval) /Location () \
                 /ContactInfo ( ) >>",
                "<< /FT /Sig /T (unsigned) >>",
                "<< /FT /Btn /Ff 32768 /T (size) /V /2 /Kids [19 0 R] >>",
                "<< /Parent 18 0 R /Subtype /Widget /Rect [0 0 1 1] >>",
            ]
            .map(str::to_owned),
        );
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let scratch = Scratch::new("pdf-form");
        let form = scratch.write("form.pdf", &document(&objects, b"", &[]));
        assert_same_as_tika_delegating(&[&form], tika_name(), &DELEGATES);
    }

    #[test]
    fn parses_embedded_files_as_tikas_path_does() {
        let hello = "Hej verden, her er en fil.";
        let second = "Anden fil, ogs\u{e5} med.";
        let third = "Tredje fil.";
        let stream = |dict: &str, text: &str| {
            format!(
                "<< {dict} /Length {} >>\nstream\n{text}\nendstream",
                text.len()
            )
        };
        let mut objects = page_objects(TEXT);
        objects[0] =
            "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles 9 0 R >> >>".to_owned();
        objects[2] = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                      /Resources << /Font << /F1 5 0 R >> >> /Annots [8 0 R] >>"
            .to_owned();
        objects.extend([
            "<< /Type /Filespec /F (hello.txt) /UF (hello.txt) /Desc (A greeting) \
             /AFRelationship /Data /EF << /F 7 0 R >> >>"
                .to_owned(),
            stream(
                &format!("/Type /EmbeddedFile /Subtype /text#2Fplain /Params << /Size {} >>", hello.len()),
                hello,
            ),
            "<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 10 10] /FS (note.txt) \
             /Contents (A note) >>"
                .to_owned(),
            "<< /Names [(second.txt) << /F (  ) /UF (second.txt) /EF << /F 10 0 R /Mac 10 0 R >> >> \
             (hello) 6 0 R] >>"
                .to_owned(),
            stream("", second),
            "<< /Type /Filespec /F (third.txt) /EF << /F 12 0 R >> >>".to_owned(),
            stream("", third),
        ]);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let scratch = Scratch::new("pdf-embedded");
        let embedding = scratch.write("embedding.pdf", &document(&objects, b"", &[]));
        assert_same_as_tika_delegating(&[&embedding], tika_name(), &DELEGATES);
    }

    #[test]
    fn measures_type3_glyphs_from_their_procedures_as_tikas_path_does() {
        let content = format!(
            "{TEXT} BT /F2 12 Tf 72 600 Td (abba abab baba) Tj 0 -14 Td (ab ba ab ba) Tj ET"
        );
        let mut objects = page_objects(&content);
        objects[2] = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                      /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>"
            .to_owned();
        let procedure =
            |text: &str| format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len());
        objects.extend([
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 0 0] \
             /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /a 7 0 R /b 8 0 R /space 9 0 R >> \
             /Encoding << /Type /Encoding /Differences [32 /space 97 /a /b] >> >>"
                .to_owned(),
            procedure("520 0 -10 -200 480 700 d1 0 0 450 700 re f"),
            procedure("610 0 d0 0 0 500 650 re f"),
            procedure("250 0 0 0 0 0 d1"),
        ]);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let scratch = Scratch::new("pdf-type3");
        let type3 = scratch.write("type3.pdf", &document(&objects, b"", &[]));
        assert_same_as_tika_delegating(&[&type3], tika_name(), &DELEGATES);
    }

    #[test]
    fn extracts_xfa_forms_as_tikas_path_does() {
        let template = "<template xmlns=\"http://www.xfa.org/schema/xfa-template/3.0/\">\n\
             <subform><draw><value><text>Ans&#248;gning om tilskud</text></value></draw>\n\
             <field name=\"navn\"><assist><toolTip>Dit <p>fulde</p> navn</toolTip></assist>\
             </field>\n<field name=\"by\"/><field name=\"tom\"/></subform>\n</template>";
        let data = "<xfa:datasets xmlns:xfa=\"http://www.xfa.org/schema/xfa-data/1.0/\">\n\
             <xfa:data>\n<form1>\n<navn>Kari</navn><by>Oslo</by><by>Bergen</by></form1>\n\
             </xfa:data></xfa:datasets>";
        let stream =
            |text: &str| format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len());
        let mut objects = page_objects(TEXT);
        objects[0] = "<< /Type /Catalog /Pages 2 0 R /AcroForm 6 0 R >>".to_owned();
        objects.extend([
            "<< /Fields [] /XFA [(preamble) 7 0 R (template) 8 0 R (datasets) 9 0 R \
             (postamble) 10 0 R] >>"
                .to_owned(),
            stream("<xdp:xdp xmlns:xdp=\"http://ns.adobe.com/xdp/\">"),
            stream(template),
            stream(data),
            stream("</xdp:xdp>"),
        ]);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let scratch = Scratch::new("pdf-xfa");
        let xfa = scratch.write("xfa.pdf", &document(&objects, b"", &[]));
        assert_same_as_tika_delegating(&[&xfa], tika_name(), &DELEGATES);
    }

    #[test]
    fn defers_a_needed_object_lopdf_cannot_read() {
        let mut objects = page_objects(TEXT);
        objects[3] = format!("<< /Length 5 /X <zz> >>\nstream\n{TEXT}\nendstream");
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[]);
        assert_eq!(
            PdfParser
                .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            Some("object 4, which lopdf cannot read and PDFBox may recover")
        );
    }

    #[test]
    fn defers_a_needed_object_the_table_does_not_list_but_pdfbox_finds() {
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[]);
        let font = find(&input, b"5 0 obj");
        let entry = format!("{font:010} 00000 n \n");
        let input = replace(&input, entry.as_bytes(), b"0000000000 65535 f \n");
        assert_eq!(
            PdfParser
                .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            Some(
                "object 5, which the cross-reference table does not list and PDFBox finds by \
                 searching the document"
            )
        );
    }

    #[test]
    fn extracts_documents_pdfbox_repairs_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-repaired");
        let mut objects = page_objects(TEXT);
        objects.push("<< /Title (Eventyr) /Author (H. C. Andersen) >>".to_owned());
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let plain = document(&objects, b"", &[]);
        let start = find(&plain, b"xref\n");
        let startxref = format!("startxref\n{start}\n");
        let documents = [
            ("shifted.pdf", document(&objects, b"", &[(4, 3), (6, 1)])),
            (
                "startxref.pdf",
                replace(
                    &plain,
                    startxref.as_bytes(),
                    format!("startxref\n{}\n", start + 2).as_bytes(),
                ),
            ),
            ("no-table.pdf", plain[..start].to_vec()),
            ("stream.pdf", stream_document(&objects, &[9])),
        ];
        let paths: Vec<PathBuf> = documents
            .iter()
            .map(|(name, bytes)| scratch.write(name, bytes))
            .collect();
        for path in &paths {
            assert_eq!(deferral(path), None, "{}", path.display());
        }
        assert_same_as_tika_delegating(&as_paths(&paths), tika_name(), &DELEGATES);
    }

    #[test]
    fn fails_a_document_whose_recording_outgrows_the_memory_budget() {
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[]);
        let configs = ParserConfigs::default();
        let prepared = crate::memory::within(1, || {
            PdfParser
                .prepare(&input, &TikaMetadata::new(), &configs)
                .map(drop)
        });
        assert!(
            matches!(prepared, Err(Declined::Fails(Error::MemoryLimit(1)))),
            "{prepared:?}"
        );
    }

    fn with_pdf(pdf: PdfParserConfig) -> ParserConfigs {
        ParserConfigs {
            pdf,
            ..ParserConfigs::default()
        }
    }

    fn corpus_documents(names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| {
                CORPORA
                    .iter()
                    .map(|corpus| Path::new(corpus).join(name))
                    .find(|path| path.exists())
                    .unwrap_or_else(|| panic!("no corpus holds {name}"))
            })
            .collect()
    }

    #[test]
    fn extracts_under_the_settings_the_port_reads_as_tikas_path_does() {
        let scratch = Scratch::new("pdf-settings");
        let objects = page_objects("BT /F1 12 Tf 72 720 Td (Kort.) Tj ET");
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let short = scratch.write("short.pdf", &document(&objects, b"", &[]));
        let tagged = corpus_documents(&["danish.pdf", "report.pdf", "table.pdf"]);
        let settings: [(PdfParserConfig, Vec<PathBuf>); 8] = [
            (
                PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::NO_OCR),
                [corpus_documents(&["deu-ocr.pdf"]), vec![short.clone()]].concat(),
            ),
            (
                PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::AUTO),
                corpus_documents(&["notes.pdf"]),
            ),
            (
                PdfParserConfig::new().set_ocr_strategy_auto("faster, 4"),
                [corpus_documents(&["notes.pdf"]), vec![short]].concat(),
            ),
            (
                PdfParserConfig::new().set_extract_marked_content(false),
                tagged.clone(),
            ),
            (
                PdfParserConfig::new().set_extract_marked_content(true),
                [tagged.clone(), corpus_documents(&["prose-columns.pdf"])].concat(),
            ),
            (
                PdfParserConfig::new().set_extract_bookmarks_text(false),
                tagged,
            ),
            (
                PdfParserConfig::new().set_extract_annotation_text(false),
                corpus_documents(&["notes.pdf", "2022_Q3_AAPL.pdf"]),
            ),
            (
                PdfParserConfig::new()
                    .set_extract_inline_images(false)
                    .set_extract_unique_inline_images_only(false),
                corpus_documents(&["danish.pdf"]),
            ),
        ];
        for (pdf, documents) in settings {
            let configs = with_pdf(pdf);
            for document in &documents {
                let input = std::fs::read(document).unwrap();
                let deferral = PdfParser.defers(&input, &TikaMetadata::new(), &configs);
                assert_eq!(deferral, None, "{} under {configs:?}", document.display());
            }
            assert_same_as_tika_delegating_with(
                &as_paths(&documents),
                tika_name(),
                &DELEGATES,
                &configs,
            );
        }
    }

    #[test]
    fn defers_under_the_settings_the_port_does_not_read() {
        let objects = page_objects(TEXT);
        let objects: Vec<&str> = objects.iter().map(String::as_str).collect();
        let input = document(&objects, b"", &[]);
        for (pdf, start) in [
            (
                PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::OCR_ONLY),
                "the OCR strategy OCR_ONLY",
            ),
            (
                PdfParserConfig::new().set_ocr_strategy(PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION),
                "the OCR strategy OCR_AND_TEXT_EXTRACTION",
            ),
            (
                PdfParserConfig::new().set_extract_inline_images(true),
                "the PDF setting setExtractInlineImages",
            ),
            (
                PdfParserConfig::new().set_ocr_strategy_auto("faster , 4"),
                "the automatic OCR thresholds \"faster , 4\"",
            ),
        ] {
            let deferral = PdfParser
                .defers(&input, &TikaMetadata::new(), &with_pdf(pdf))
                .unwrap_or_default();
            assert!(deferral.starts_with(start), "{deferral}");
        }
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
                Why::Unmapped if ocr_runs() => reason.contains("is doubtful (unmapped-glyphs)"),
                Why::Ocr | Why::Unmapped => {
                    reason.starts_with("page ") && reason.ends_with("which Tika OCRs")
                }
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
