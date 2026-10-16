//! OpenDocument files (ODT, ODS, ODP, ODG and the rest, their templates, and StarOffice Writer
//! files), as Tika's `OpenDocumentParser` reads them: the metadata of `meta.xml`, the media type
//! the `mimetype` entry names, and the text of every `content.xml` and `styles.xml` in the
//! archive, embedded objects' included, in the order of its central directory. Documents with
//! pictures or embedded binary data, which Tika parses as embedded documents, and encrypted
//! documents are left to Tika.

mod body;
mod flat;

pub(crate) use flat::FlatOpenDocumentParser;
mod meta;

use std::borrow::Cow;
use std::collections::HashMap;

use crate::config::ParserConfigs;
use crate::formats::embedded::{self, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::zip_file::{ZipEntry, ZipFile};
use crate::formats::{Declined, Failure, Parser, Prepared};
use crate::mime::{self, Origin};

pub(crate) const OFFICE_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:office:1.0";
pub(crate) const META_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:meta:1.0";
/// The element in which an OpenDocument part embeds a file, base64-encoded.
pub(crate) const BINARY_DATA_ELEMENT: &str = "binary-data";
const OLD_NS: &str = "http://openoffice.org/2000/";
const NEW_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:";
const META_NAME: &[u8] = b"meta.xml";
const MIMETYPE_NAME: &[u8] = b"mimetype";

/// The media types Tika parses with its `OpenDocumentParser`: those it names, and the StarOffice
/// Writer template, which Tika's registry makes a kind of StarOffice Writer file.
const MEDIA_TYPES: [&str; 34] = [
    "application/vnd.sun.xml.writer",
    "application/vnd.sun.xml.writer.template",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.oasis.opendocument.graphics",
    "application/vnd.oasis.opendocument.presentation",
    "application/vnd.oasis.opendocument.spreadsheet",
    "application/vnd.oasis.opendocument.chart",
    "application/vnd.oasis.opendocument.image",
    "application/vnd.oasis.opendocument.formula",
    "application/vnd.oasis.opendocument.text-master",
    "application/vnd.oasis.opendocument.text-web",
    "application/vnd.oasis.opendocument.text-template",
    "application/vnd.oasis.opendocument.graphics-template",
    "application/vnd.oasis.opendocument.presentation-template",
    "application/vnd.oasis.opendocument.spreadsheet-template",
    "application/vnd.oasis.opendocument.chart-template",
    "application/vnd.oasis.opendocument.image-template",
    "application/vnd.oasis.opendocument.formula-template",
    "application/x-vnd.oasis.opendocument.text",
    "application/x-vnd.oasis.opendocument.graphics",
    "application/x-vnd.oasis.opendocument.presentation",
    "application/x-vnd.oasis.opendocument.spreadsheet",
    "application/x-vnd.oasis.opendocument.chart",
    "application/x-vnd.oasis.opendocument.image",
    "application/x-vnd.oasis.opendocument.formula",
    "application/x-vnd.oasis.opendocument.text-master",
    "application/x-vnd.oasis.opendocument.text-web",
    "application/x-vnd.oasis.opendocument.text-template",
    "application/x-vnd.oasis.opendocument.graphics-template",
    "application/x-vnd.oasis.opendocument.presentation-template",
    "application/x-vnd.oasis.opendocument.spreadsheet-template",
    "application/x-vnd.oasis.opendocument.chart-template",
    "application/x-vnd.oasis.opendocument.image-template",
    "application/x-vnd.oasis.opendocument.formula-template",
];

/// Tika's `NSNormalizerContentHandler`: a namespace of OpenOffice.org 1 in its OpenDocument
/// form. Tika maps the namespaces of elements only, never those of attributes.
pub(crate) fn normalized_namespace(namespace: &str) -> Cow<'_, str> {
    match namespace.strip_prefix(OLD_NS) {
        Some(rest) => Cow::Owned(format!("{NEW_NS}{rest}:1.0")),
        None => Cow::Borrowed(namespace),
    }
}

/// What Tika's `OpenDocumentParser` does with an entry of the archive, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    /// A manifest, which Tika reads only to fail on an encrypted document.
    Manifest,
    Mimetype,
    Meta,
    /// A `content.xml` or `styles.xml`, whose text Tika writes.
    Body,
    /// The document's thumbnail, which extractous's embedded document extractor skips.
    Thumbnail,
    /// A picture or a thumbnail of an embedded object, which Tika parses as an embedded document.
    Embedded,
    /// Anything else, which Tika leaves alone, macros among it unless it is asked for them.
    Ignored,
}

impl EntryKind {
    /// The branches of Tika's `handleZipArchiveEntry`, in their order.
    fn of(name: &[u8]) -> Self {
        let contains = |part: &[u8]| name.windows(part.len()).any(|window| window == part);
        if name.ends_with(b"/") {
            Self::Directory
        } else if contains(b"manifest.xml") {
            Self::Manifest
        } else if name == MIMETYPE_NAME {
            Self::Mimetype
        } else if name == META_NAME {
            Self::Meta
        } else if name.ends_with(b"content.xml") || name.ends_with(b"styles.xml") {
            Self::Body
        } else if contains(b"Pictures/") {
            Self::Embedded
        } else if name.starts_with(b"Thumbnails/") {
            Self::Thumbnail
        } else if contains(b"Thumbnails/") {
            Self::Embedded
        } else {
            Self::Ignored
        }
    }
}

/// An entry of the archive, in the order of its central directory.
#[derive(Debug, Clone)]
struct Entry {
    entry: ZipEntry,
    kind: EntryKind,
}

impl Entry {
    fn name(&self) -> &str {
        &self.entry.name
    }
}

/// A document's ZIP archive and its entries.
struct Archive<'a> {
    zip: ZipFile<'a>,
    entries: Vec<Entry>,
    /// The entries read ahead to check them, by name, each to be read once more.
    read_ahead: HashMap<String, Vec<u8>>,
}

impl<'a> Archive<'a> {
    fn open(input: &'a [u8]) -> Result<Self, Failure> {
        let zip = ZipFile::open(input, "OpenDocument")?;
        let entries = zip
            .entries()
            .iter()
            .map(|entry| Entry {
                kind: EntryKind::of(&entry.raw_name),
                entry: entry.clone(),
            })
            .collect();
        Ok(Self {
            zip,
            entries,
            read_ahead: HashMap::new(),
        })
    }

    fn of_kind(&self, kind: EntryKind) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |entry| entry.kind == kind)
    }

    fn read(&mut self, entry: &Entry) -> Result<Vec<u8>, Failure> {
        match self.read_ahead.remove(entry.name()) {
            Some(bytes) => Ok(bytes),
            None => self.zip.read(&entry.entry),
        }
    }

    /// Reads `entry` now, for the read that writes it.
    fn read_now(&mut self, entry: &Entry) -> Result<&[u8], Failure> {
        let bytes = self.read(entry)?;
        Ok(self
            .read_ahead
            .entry(entry.name().to_owned())
            .or_insert(bytes))
    }

    fn has_every_record(&self, input: &[u8]) -> bool {
        self.zip.has_every_record(input)
    }
}

/// Finds whether an XML part has an element of a local name before anything Java's parser would
/// stop at.
struct ElementFinder {
    local_name: &'static str,
    found: bool,
}

impl ElementFinder {
    /// Whether `input` has an element named `local_name`, in any namespace, before the first
    /// point where Java's SAX parser would fail. The finder stops the parse where it finds one.
    fn finds(local_name: &'static str, input: &[u8], what: &str) -> bool {
        let mut finder = Self {
            local_name,
            found: false,
        };
        let stopped = xml::parse_sax(input, what, &mut finder).is_err();
        stopped && finder.found
    }
}

impl SaxHandler for ElementFinder {
    fn start_element(
        &mut self,
        _namespace: &str,
        local_name: &str,
        _attributes: &[Attribute],
    ) -> Result<(), Failure> {
        if local_name == self.local_name {
            self.found = true;
            return Err(Failure::Malformed(format!("<{local_name}> found")));
        }
        Ok(())
    }

    fn end_element(&mut self, _namespace: &str, _local_name: &str) -> Result<(), Failure> {
        Ok(())
    }

    fn characters(&mut self, _text: &str) -> Result<(), Failure> {
        Ok(())
    }
}

const UNREADABLE_ARCHIVE: &str = "its archive is not one the zip crate reads";
const DUPLICATES: &str = "its archive has entries the zip crate does not list, such as duplicates";
const UNREADABLE_MANIFEST: &str = "its manifest cannot be read";
const ENCRYPTED: &str = "it is encrypted";
const NO_BODY: &str = "it has no content.xml or styles.xml, so Tika leaves its XHTML unfinished";
const UNREADABLE_ENTRY: &str = "an entry it reads cannot be read by the zip crate";
const BINARY_DATA: &str = "it embeds binary data, which Tika parses as embedded documents";

/// The metadata Tika's `OpenDocumentParser` gives the picture or object thumbnail `name` of
/// `bytes` it hands to the embedded document extractor: a picture's type as its detector finds
/// it. It also sets the pages a picture is drawn on, which change nothing of what is extracted
/// or spooled.
fn embedded_metadata(name: &str, bytes: &[u8]) -> Result<TikaMetadata, Failure> {
    let mut metadata = TikaMetadata::new();
    metadata.set(embedded::INTERNAL_PATH, name);
    metadata.set(embedded::RESOURCE_NAME, name);
    if name.contains("Pictures/") {
        metadata.set(embedded::RESOURCE_TYPE, "INLINE");
        let detection = mime::detect(bytes, Origin::File { name }).map_err(|error| {
            Failure::Malformed(format!("Tika's detection throws on {name}: {error:?}"))
        })?;
        metadata.set("Content-Type", detection.media_type.to_string());
    }
    Ok(metadata)
}

/// The archive of the document in `input`, its metadata, type and bodies read ahead, or why
/// Tika's path should read it rather than this parser.
fn read_ahead(input: &[u8]) -> Result<Archive<'_>, &'static str> {
    let Ok(mut archive) = Archive::open(input) else {
        return Err(UNREADABLE_ARCHIVE);
    };
    if !archive.has_every_record(input) {
        return Err(DUPLICATES);
    }
    let manifests: Vec<Entry> = archive.of_kind(EntryKind::Manifest).cloned().collect();
    for manifest in &manifests {
        let Ok(bytes) = archive.read(manifest) else {
            return Err(UNREADABLE_MANIFEST);
        };
        if ElementFinder::finds("encryption-data", &bytes, manifest.name()) {
            return Err(ENCRYPTED);
        }
    }
    let bodies: Vec<Entry> = archive.of_kind(EntryKind::Body).cloned().collect();
    if bodies.is_empty() {
        return Err(NO_BODY);
    }
    let read: Vec<Entry> = archive
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::Meta | EntryKind::Mimetype))
        .cloned()
        .collect();
    for entry in &read {
        archive.read_now(entry).map_err(|_| UNREADABLE_ENTRY)?;
    }
    for body in &bodies {
        let Ok(bytes) = archive.read_now(body) else {
            return Err(UNREADABLE_ENTRY);
        };
        let named = bytes
            .windows(BINARY_DATA_ELEMENT.len())
            .any(|window| window == BINARY_DATA_ELEMENT.as_bytes());
        if named && ElementFinder::finds(BINARY_DATA_ELEMENT, bytes, body.name()) {
            return Err(BINARY_DATA);
        }
    }
    Ok(archive)
}

pub(crate) struct OpenDocumentParser;

impl Parser for OpenDocumentParser {
    fn name(&self) -> &'static str {
        "extractous::OpenDocumentParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.odf.OpenDocumentParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let archive = read_ahead(input).map_err(str::to_owned)?;
        Ok(Box::new(move |xhtml| write(archive, configs, xhtml)))
    }
}

/// Writes the document in `archive` as `OpenDocumentParser.parse` does.
fn write(mut archive: Archive, configs: &ParserConfigs, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let meta = archive.of_kind(EntryKind::Meta).next().cloned();
    if let Some(entry) = meta {
        let bytes = archive.read(&entry)?;
        meta::read_meta(&bytes, xhtml.metadata_mut())?;
    }
    let entries = archive.entries.clone();
    for entry in &entries {
        match entry.kind {
            EntryKind::Mimetype => {
                let bytes = archive.read(entry)?;
                xhtml
                    .metadata_mut()
                    .set("Content-Type", String::from_utf8_lossy(&bytes));
            }
            EntryKind::Body => {
                let bytes = archive.read(entry)?;
                let mut body = body::Body::new(
                    xhtml,
                    configs.office.includes_deleted_content(),
                    body::Namespaces::Normalized,
                );
                xml::parse_sax(&bytes, entry.name(), &mut body)?;
            }
            EntryKind::Embedded => {
                let bytes = archive.read(entry)?;
                let metadata = embedded_metadata(entry.name(), &bytes)?;
                xhtml.parse_embedded(&bytes, &metadata, Wrapping::Bare)?;
            }
            EntryKind::Directory
            | EntryKind::Manifest
            | EntryKind::Meta
            | EntryKind::Thumbnail
            | EntryKind::Ignored => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OfficeParserConfig;
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, assert_same_as_tika_with, conformance_corpus,
        Scratch,
    };
    use std::io::Cursor;
    use std::path::{Path, PathBuf};
    use zip::CompressionMethod;

    const TIKA_NAME: &str = "org.apache.tika.parser.odf.OpenDocumentParser";
    const DOCUMENTS: [&str; 4] = [
        "../test_files/documents/simple.odt",
        "../quality/corpus/notes.odt",
        "../quality/corpus/prose.odt",
        "../quality/corpus/revisions.odt",
    ];

    fn paths<'a>(documents: &[&'a str]) -> Vec<&'a Path> {
        documents
            .iter()
            .map(|document| Path::new(*document))
            .collect()
    }

    #[test]
    fn extracts_what_tikas_path_extracts() {
        assert_same_as_tika(&paths(&DOCUMENTS), TIKA_NAME);
    }

    const MISC_OFFICE: &str = "tika-parsers-standard/tika-parsers-standard-modules/\
                               tika-parser-miscoffice-module/src/test/resources/test-documents";
    const INTEGRATION: &str = "tika-parsers-standard/tika-parsers-standard-integration-tests/\
                               src/test/resources/test-documents";

    /// Tika's own test documents of this format, as `(directory, name)` in the directory
    /// `CONFORMANCE_CORPUS` names, which `just conformance` prepares.
    const CONFORMANCE: [(&str, &str); 27] = [
        (INTEGRATION, "test-columnar.ods"),
        (INTEGRATION, "testPhoneNumberExtractor.odt"),
        (INTEGRATION, "testStarOffice-6.0-writer.sxw"),
        (INTEGRATION, "testStarOffice-6.0-writer-template.stw"),
        (MISC_OFFICE, "testFooter.ods"),
        (MISC_OFFICE, "testFooter.odt"),
        (MISC_OFFICE, "testMasterFooter.odp"),
        (MISC_OFFICE, "testNPEOpenDocument.odt"),
        (MISC_OFFICE, "testODFwithOOo3.odt"),
        (MISC_OFFICE, "testODPMacro.odp"),
        (MISC_OFFICE, "testODTNoMeta.odt"),
        (MISC_OFFICE, "testODP_NPE.odp"),
        (MISC_OFFICE, "testODSMacro.ods"),
        (MISC_OFFICE, "testODTEmbeddedImageLink.odt"),
        (MISC_OFFICE, "testODTEmbedded.odt"),
        (MISC_OFFICE, "testODTMacro.odt"),
        (MISC_OFFICE, "testODT-TIKA-6000.odt"),
        (MISC_OFFICE, "testODTStyles2.odt"),
        (MISC_OFFICE, "testODTStyles3.odt"),
        (MISC_OFFICE, "testODT_svgTitleInStyledSpan.odt"),
        (MISC_OFFICE, "testOpenOffice2.odf"),
        (MISC_OFFICE, "testOpenOffice2.odt"),
        (MISC_OFFICE, "testStyles.odt"),
        (MISC_OFFICE, "versions/LibreOfficeCalc_ods_1.3.ods"),
        (MISC_OFFICE, "versions/LibreOfficeDraw_odg_1.3.odg"),
        (MISC_OFFICE, "versions/LibreOfficeImpress_odp_1.3.odp"),
        (MISC_OFFICE, "versions/LibreOfficeWriter_odt_1.3.odt"),
    ];

    /// Tika's own test documents of this format that Tika's path keeps, and why.
    const CONFORMANCE_DEFERRED: [(&str, &str, &str); 1] =
        [(MISC_OFFICE, "testODTEncrypted.odt", ENCRYPTED)];

    #[test]
    #[ignore]
    fn extracts_what_tikas_path_extracts_from_tikas_documents() {
        let corpus = conformance_corpus();
        let documents: Vec<PathBuf> = CONFORMANCE
            .iter()
            .map(|(directory, name)| corpus.join(directory).join(name))
            .collect();
        let documents: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&documents, TIKA_NAME);
    }

    #[test]
    #[ignore]
    fn leaves_tikas_documents_with_pictures_or_encryption_to_tika() {
        let corpus = conformance_corpus();
        for (directory, name, reason) in CONFORMANCE_DEFERRED {
            let input = std::fs::read(corpus.join(directory).join(name)).unwrap();
            assert_eq!(
                OpenDocumentParser
                    .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
                    .as_deref(),
                Some(reason),
                "{name}"
            );
        }
    }

    const NAMESPACES: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0""#;
    const TEXT: &str = "application/vnd.oasis.opendocument.text";
    const MANIFEST: (&str, &str) = (
        "META-INF/manifest.xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/></manifest:manifest>"#,
    );

    fn content(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {NAMESPACES}><office:automatic-styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style><style:style style:name="T2" style:family="text"><style:text-properties fo:font-style="italic" style:text-underline-style="solid"/></style:style><style:style style:name="T3" style:family="text"><style:text-properties fo:font-weight="600" fo:font-style="oblique"/></style:style><style:style style:name="P1" style:family="paragraph"><style:text-properties fo:font-weight="bold"/></style:style><style:style style:family="paragraph"><style:text-properties style:text-underline-style="solid"/></style:style><style:style style:name="C1" style:family="table-cell"/><text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style><text:list-style style:name="L2"><text:list-level-style-bullet text:level="1"><style:text-properties fo:font-weight="bold"/></text:list-level-style-bullet></text:list-style></office:automatic-styles><office:body>{body}</office:body></office:document-content>"#
        )
    }

    /// An OpenDocument file of `entries`, stored in that order after the `mimetype`.
    fn document(media_type: &str, entries: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            zip::write::SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("mimetype", options).unwrap();
        std::io::Write::write_all(&mut zip, media_type.as_bytes()).unwrap();
        for (entry, text) in entries {
            zip.start_file(*entry, options).unwrap();
            std::io::Write::write_all(&mut zip, text.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_what_tikas_path_extracts_from_every_kind_of_element() {
        let text = content(
            &r#"<office:text><text:sequence-decls><text:sequence-decl text:name="Table"/></text:sequence-decls><text:tracked-changes><text:changed-region text:id="c1"><text:deletion><text:p>gone words</text:p></text:deletion></text:changed-region></text:tracked-changes>
<text:h text:outline-level="2">Heading <text:span text:style-name="T1">bold</text:span> two</text:h><text:h>Plain heading</text:h><text:h text:outline-level="9">Deep</text:h><text:h text:outline-level="0">Zero</text:h>
<text:p text:style-name="P1">Bold paragraph <text:span text:style-name="T2">italic under</text:span><text:span text:style-name="T3">semi</text:span> end<text:tab/>tab<text:s text:c="3"/>space<text:line-break/>next</text:p>
<text:p>Before note<text:note text:id="n1" text:note-class="footnote"><text:note-citation>1</text:note-citation><text:note-body><text:p>Note text</text:p></text:note-body></text:note>@GAP@after note &amp; more &#x263A; 😀 a]b</text:p>
<text:p>Link <text:a xlink:href="http://example.com/" xlink:title="Ex"><text:span text:style-name="T1">styled link</text:span></text:a> after</text:p>
<text:list text:style-name="L1"><text:list-item><text:p>One</text:p><text:list><text:list-item><text:p>Nested</text:p></text:list-item></text:list></text:list-item><text:list-item><text:p>Two</text:p></text:list-item></text:list><text:list text:style-name="L2"><text:list-item><text:p>Bullet</text:p></text:list-item></text:list><text:list><text:list-item><text:p>Unstyled</text:p></text:list-item></text:list>
<table:table table:name="T"><table:table-column table:number-columns-repeated="3"/><table:table-row><table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="2"><text:p>Span</text:p></table:table-cell><table:covered-table-cell><text:p>hidden</text:p></table:covered-table-cell><table:table-cell table:number-columns-repeated="2"><text:p>Rep <text:span text:style-name="T1">b</text:span></text:p><text:p>second</text:p></table:table-cell></table:table-row></table:table>
<text:p>Page <text:page-number>3</text:page-number> of <text:page-count>9</text:page-count></text:p>
<text:p><draw:frame><svg:title>Frame title</svg:title><svg:desc>Frame desc</svg:desc><draw:text-box><text:p>In box</text:p></draw:text-box></draw:frame><draw:frame><draw:image xlink:href="http://example.com/x.png"/></draw:frame><draw:a xlink:href="http://example.com/2"><draw:frame><draw:image xlink:href=""/></draw:frame></draw:a></text:p>
<text:p>Annotated<office:annotation><dc:creator>Me</dc:creator><text:p>Remark</text:p></office:annotation> text</text:p>
<text:p><text:span text:style-name="T1">bold <text:span text:style-name="T2">nested</text:span> back</text:span><text:span text:style-name="T1"><svg:title>t</svg:title></text:span><text:span text:style-name="T1">x<text:a xlink:href="y">in</text:a>out</text:span></text:p>
<draw:object xlink:href="./Object 1"/><text:p>emoji 😀 and <![CDATA[cdata <b>]]></text:p><text:section><text:p>in section</text:p></text:section></office:text>"#
                .replace("@GAP@", "  \n \n  "),
        );
        let styles = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-styles {NAMESPACES}><office:styles><style:style style:name="Standard" style:family="paragraph"/></office:styles><office:master-styles><style:master-page style:name="Standard"><style:header><text:p>Header text</text:p></style:header><style:footer><text:p>Footer <text:page-number>1</text:page-number></text:p></style:footer></style:master-page></office:master-styles></office:document-styles>"#
        );
        let chart = content(
            r#"<office:chart><table:table><table:table-row><table:table-cell><text:p>Chart cell</text:p></table:table-cell></table:table-row></table:table></office:chart>"#,
        );
        let meta = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta {NAMESPACES} office:version="1.3"><office:meta><meta:generator>Test</meta:generator><dc:title>Elements</dc:title><meta:initial-creator>Anna</meta:initial-creator><dc:creator>Bo</dc:creator><meta:keyword>hav</meta:keyword><meta:keyword>sten</meta:keyword><dc:subject>Lys</dc:subject><dc:language>da-DK</dc:language><dc:date>2026-01-02T03:04:05</dc:date><meta:creation-date>2026-01-01T00:00:00</meta:creation-date><meta:editing-duration>PT1M</meta:editing-duration><meta:editing-cycles>3</meta:editing-cycles><meta:user-defined meta:name="Info 1">Tekst</meta:user-defined><meta:user-defined meta:name="Info 1">Mere</meta:user-defined><meta:document-statistic meta:page-count="2" meta:paragraph-count="9" meta:word-count="40" meta:character-count="200" meta:table-count="1" meta:image-count="2" meta:object-count="1"/></office:meta></office:document-meta>"#
        );
        let scratch = Scratch::new("odf-elements");
        let documents = [
            scratch.write(
                "elements.odt",
                &document(
                TEXT,
                &[
                    ("styles.xml", &styles),
                    ("content.xml", &text),
                    ("meta.xml", &meta),
                    ("Object 1/content.xml", &chart),
                    ("Object 1/meta.xml", &meta),
                    ("Thumbnails/thumbnail.png", "not a picture"),
                    MANIFEST,
                ],
            ),
            ),
            scratch.write(
                "presentation.odp",
                &document(
                "application/vnd.oasis.opendocument.presentation",
                &[
                    MANIFEST,
                    (
                        "content.xml",
                        &content(
                            r#"<office:presentation><draw:page draw:name="p1"><draw:frame><draw:text-box><text:p>Slide</text:p></draw:text-box></draw:frame><presentation:notes><draw:frame><draw:text-box><text:p>Speaker</text:p></draw:text-box></draw:frame></presentation:notes></draw:page></office:presentation>"#,
                        ),
                    ),
                ],
            ),
            ),
            scratch.write(
                "staroffice.sxw",
                &document(
                "application/vnd.sun.xml.writer",
                &[
                    (
                        "content.xml",
                        r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE office:document-content PUBLIC "-//OpenOffice.org//DTD OfficeDocument 1.0//EN" "office.dtd"><office:document-content xmlns:office="http://openoffice.org/2000/office" xmlns:style="http://openoffice.org/2000/style" xmlns:text="http://openoffice.org/2000/text" xmlns:fo="http://www.w3.org/1999/XSL/Format"><office:automatic-styles><style:style style:name="T1" style:family="text"><style:properties fo:font-weight="bold"/></style:style></office:automatic-styles><office:body><text:h text:level="1">Old heading</text:h><text:p>Old <text:span text:style-name="T1">text</text:span><text:tab-stop/>tab</text:p></office:body></office:document-content>"#,
                    ),
                    (
                        "meta.xml",
                        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta xmlns:office="http://openoffice.org/2000/office" xmlns:meta="http://openoffice.org/2000/meta" xmlns:dc="http://purl.org/dc/elements/1.1/" office:version="1.0"><office:meta><meta:generator>StarOffice</meta:generator><dc:title>Old</dc:title><meta:document-statistic meta:page-count="1"/></office:meta></office:document-meta>"#,
                    ),
                    MANIFEST,
                ],
            ),
            ),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[test]
    fn fails_where_tikas_path_fails() {
        let scratch = Scratch::new("odf-failures");
        let documents = [
            scratch.write(
                "malformed.odt",
                &document(
                    TEXT,
                    &[
                        (
                            "content.xml",
                            &content("<office:text><text:p>open</office:text>"),
                        ),
                        MANIFEST,
                    ],
                ),
            ),
            scratch.write(
                "font-weight.odt",
                &document(
                    TEXT,
                    &[
                        (
                            "content.xml",
                            &content(r#"<office:text><text:p>x</text:p></office:text>"#)
                                .replace(r#"fo:font-weight="600""#, r#"fo:font-weight="6x""#),
                        ),
                        MANIFEST,
                    ],
                ),
            ),
            scratch.write(
                "empty-font-weight.odt",
                &document(
                    TEXT,
                    &[
                        (
                            "content.xml",
                            &content(r#"<office:text><text:p>x</text:p></office:text>"#)
                                .replace(r#"fo:font-weight="600""#, r#"fo:font-weight="""#),
                        ),
                        MANIFEST,
                    ],
                ),
            ),
            scratch.write(
                "meta.odt",
                &document(
                    TEXT,
                    &[
                        ("meta.xml", "<office:document-meta>"),
                        ("content.xml", &content("<office:text/>")),
                        MANIFEST,
                    ],
                ),
            ),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&paths);
    }

    #[test]
    fn leaves_documents_with_binary_data_to_tika_and_reads_those_with_pictures() {
        let text = content("<office:text><text:p>x</text:p></office:text>");
        let picture = document(TEXT, &[("content.xml", &text), ("Pictures/a.png", "png")]);
        assert_eq!(
            OpenDocumentParser
                .defers(&picture, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            None
        );
        let object_thumbnail = document(
            TEXT,
            &[("content.xml", &text), ("Object 1/Thumbnails/a.png", "png")],
        );
        assert_eq!(
            OpenDocumentParser
                .defers(
                    &object_thumbnail,
                    &TikaMetadata::new(),
                    &ParserConfigs::default()
                )
                .as_deref(),
            None
        );
        let binary = document(
            TEXT,
            &[(
                "content.xml",
                &content(
                    r#"<office:text><text:p><draw:frame><draw:image><office:binary-data>AAAA</office:binary-data></draw:image></draw:frame></text:p></office:text>"#,
                ),
            )],
        );
        assert_eq!(
            OpenDocumentParser
                .defers(&binary, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            Some(BINARY_DATA)
        );
        let mentioned = document(
            TEXT,
            &[(
                "content.xml",
                &content("<office:text><text:p>binary-data</text:p></office:text>"),
            )],
        );
        assert_eq!(
            OpenDocumentParser
                .defers(&mentioned, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            None
        );
        let thumbnail = document(
            TEXT,
            &[("content.xml", &text), ("Thumbnails/thumbnail.png", "png")],
        );
        assert_eq!(
            OpenDocumentParser
                .defers(&thumbnail, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            None
        );
    }

    #[test]
    fn leaves_odd_archives_to_tika() {
        let text = content("<office:text><text:p>x</text:p></office:text>");
        assert_eq!(
            OpenDocumentParser
                .defers(
                    b"not a zip",
                    &TikaMetadata::new(),
                    &ParserConfigs::default()
                )
                .as_deref(),
            Some(UNREADABLE_ARCHIVE)
        );
        let without_content = document(TEXT, &[MANIFEST]);
        assert_eq!(
            OpenDocumentParser
                .defers(
                    &without_content,
                    &TikaMetadata::new(),
                    &ParserConfigs::default()
                )
                .as_deref(),
            Some(NO_BODY)
        );
        let duplicated = document(TEXT, &[("content.xml", &text), ("content.xmm", &text)]);
        let duplicated = replace_all(&duplicated, b"content.xmm", b"content.xml");
        assert_eq!(
            OpenDocumentParser
                .defers(&duplicated, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            Some(DUPLICATES)
        );
    }

    fn replace_all(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        let mut replaced = bytes.to_vec();
        let mut index = 0;
        while let Some(found) = replaced[index..]
            .windows(from.len())
            .position(|w| w == from)
        {
            replaced[index + found..index + found + from.len()].copy_from_slice(to);
            index += found + from.len();
        }
        replaced
    }

    #[test]
    fn leaves_encrypted_documents_to_tika() {
        let input = std::fs::read("../test_files/documents/encrypted/undecryptable.odt").unwrap();
        assert_eq!(
            OpenDocumentParser
                .defers(&input, &TikaMetadata::new(), &ParserConfigs::default())
                .as_deref(),
            Some(ENCRYPTED)
        );
    }

    #[test]
    fn extracts_deleted_content_as_tikas_path_does_when_asked() {
        let configs = ParserConfigs {
            office: OfficeParserConfig::new().set_include_deleted_content(true),
            ..ParserConfigs::default()
        };
        assert_same_as_tika_with(
            &paths(&["../quality/corpus/revisions.odt"]),
            TIKA_NAME,
            &configs,
        );
    }
}
