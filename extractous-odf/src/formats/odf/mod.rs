//! OpenDocument files (ODT, ODS, ODP, ODG and the rest, their templates, and StarOffice Writer
//! files), as Tika's `OpenDocumentParser` reads them: the metadata of `meta.xml`, the media type
//! the `mimetype` entry names, and the text of every `content.xml` and `styles.xml` in the
//! archive, embedded objects' included, in the order of its central directory, with the
//! pictures, object thumbnails and base64 binary data it embeds parsed as embedded documents.
//! An encrypted document fails, as Tika fails it, before anything of it is written.

mod body;
mod flat;

pub use flat::FlatOpenDocumentParser;
mod meta;

use std::borrow::Cow;
use std::io::{self, Write};

use crate::compress::zip::file::ZipEntry;
use crate::config::ParserConfigs;
use crate::formats::crypt;
use crate::formats::embedded::{self, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml::{self, Attribute, SaxHandler};
use crate::formats::zip_package::{self, Opened, Undecoded, Unread, ZipPackage};
use crate::formats::{Decline, Difference, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages, Spool};
use crate::java::CommonsBase64;
use crate::mime::{self, Origin};

const TIKA_NAME: &str = "org.apache.tika.parser.odf.OpenDocumentParser";
pub(crate) const OFFICE_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:office:1.0";
pub(crate) const META_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:meta:1.0";
/// The element in which an OpenDocument part embeds a file, base64-encoded.
pub(crate) const BINARY_DATA_ELEMENT: &str = "binary-data";

/// The bytes [`BinaryData`] decodes before it hands them to its spool.
const BINARY_DATA_BUFFER: usize = 64 * 1024;

/// The text of a `binary-data` element decoded as `OpenDocumentBodyHandler` decodes it, with
/// commons-codec's `Base64.decodeBase64`, but piece by piece as the text comes, into a spool.
pub(crate) struct BinaryData {
    decoder: CommonsBase64,
    decoded: Vec<u8>,
    spool: Spool,
}

impl BinaryData {
    pub(crate) fn new() -> Self {
        Self {
            decoder: CommonsBase64::default(),
            decoded: Vec::with_capacity(BINARY_DATA_BUFFER),
            spool: Spool::new(None),
        }
    }

    /// Decodes `text`, the next piece of the element's text.
    pub(crate) fn push(&mut self, text: &str) -> io::Result<()> {
        self.decoder.push(text, &mut self.decoded);
        if self.decoded.len() < BINARY_DATA_BUFFER {
            return Ok(());
        }
        self.spool.write_all(&self.decoded)?;
        self.decoded.clear();
        Ok(())
    }

    /// The bytes decoded, once the element's text has ended.
    pub(crate) fn finish(mut self) -> io::Result<Input<'static>> {
        self.decoder.finish(&mut self.decoded);
        self.spool.write_all(&self.decoded)?;
        self.spool.finish()
    }
}

const OLD_NS: &str = "http://openoffice.org/2000/";
const NEW_NS: &str = "urn:oasis:names:tc:opendocument:xmlns:";
const META_NAME: &str = "meta.xml";
const MIMETYPE_NAME: &str = "mimetype";
const MANIFEST_NAME: &str = "META-INF/manifest.xml";

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
/// form.
pub(crate) fn normalized_namespace(namespace: &str) -> Cow<'_, str> {
    match namespace.strip_prefix(OLD_NS) {
        Some(rest) => Cow::Owned(format!("{NEW_NS}{rest}:1.0")),
        None => Cow::Borrowed(namespace),
    }
}

/// `attributes` with their OpenOffice.org 1 namespaces in OpenDocument form, as their
/// elements'. Tika's `NSNormalizerContentHandler` passes the attributes on as written, so the
/// handlers never find those of OpenOffice.org 1 documents (#90).
pub(crate) fn normalized_attributes(attributes: &[Attribute]) -> Cow<'_, [Attribute]> {
    if !attributes
        .iter()
        .any(|attribute| attribute.namespace.starts_with(OLD_NS))
    {
        return Cow::Borrowed(attributes);
    }
    Cow::Owned(
        attributes
            .iter()
            .map(|attribute| Attribute {
                namespace: normalized_namespace(&attribute.namespace).into_owned(),
                ..attribute.clone()
            })
            .collect(),
    )
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
    fn of(name: &str) -> Self {
        if name.ends_with('/') {
            Self::Directory
        } else if name.contains("manifest.xml") {
            Self::Manifest
        } else if name == MIMETYPE_NAME {
            Self::Mimetype
        } else if name == META_NAME {
            Self::Meta
        } else if name.ends_with("content.xml") || name.ends_with("styles.xml") {
            Self::Body
        } else if name.contains("Pictures/") {
            Self::Embedded
        } else if name.starts_with("Thumbnails/") {
            Self::Thumbnail
        } else if name.contains("Thumbnails/") {
            Self::Embedded
        } else {
            Self::Ignored
        }
    }

    /// Whether Tika's `OpenDocumentParser` reads the data of an entry of this kind.
    fn is_read(self) -> bool {
        match self {
            Self::Manifest | Self::Mimetype | Self::Meta | Self::Body | Self::Embedded => true,
            Self::Directory | Self::Thumbnail | Self::Ignored => false,
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
    package: ZipPackage<'a>,
    entries: Vec<Entry>,
}

impl<'a> Archive<'a> {
    fn new(package: ZipPackage<'a>) -> Self {
        let entries = package
            .zip()
            .entries()
            .iter()
            .map(|entry| Entry {
                kind: EntryKind::of(&entry.name),
                entry: entry.clone(),
            })
            .collect();
        Self { package, entries }
    }

    /// The indexes of the entries whose data `handleZipFile` reads, in its order: the first
    /// manifest and the first `meta.xml` by their names, then every entry but a `meta.xml`.
    fn handled(&self) -> Vec<usize> {
        let first = |name: &str| self.entries.iter().position(|entry| entry.name() == name);
        first(MANIFEST_NAME)
            .into_iter()
            .chain(first(META_NAME))
            .chain(
                self.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| entry.name() != META_NAME)
                    .map(|(index, _)| index),
            )
            .filter(|&index| self.entries[index].kind.is_read())
            .collect()
    }

    /// Reads the data of each entry `handleZipFile` reads, in its order, one at a time, for
    /// `each` to handle, failing where that reading throws.
    fn read_each(
        &self,
        mut each: impl FnMut(&Entry, &Input) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let zip = self.package.zip();
        for index in self.handled() {
            let entry = &self.entries[index];
            let input = self
                .package
                .spool(&zip, &entry.entry)
                .map_err(|unread| unread.failure(entry.name(), Opened::Tagged, TIKA_NAME))?;
            each(entry, &input)?;
        }
        Ok(())
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
    fn finds(local_name: &'static str, input: &Input, what: &str) -> bool {
        let mut finder = Self {
            local_name,
            found: false,
        };
        let stopped = xml::parse_part(input, what, &mut finder).is_err();
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

const UNENDED: &str = "it has pictures but no content.xml or styles.xml, so Tika leaves its \
                       document unended, where the native parser ends it";

/// The metadata Tika's `OpenDocumentParser` gives the picture or object thumbnail `name` of
/// `bytes` it hands to the embedded document extractor: a picture's type as its detector finds
/// it. It also sets the pages a picture is drawn on, which change nothing of what is extracted
/// or spooled.
fn embedded_metadata(name: &str, bytes: &[u8]) -> Result<TikaMetadata, Failure> {
    let mut metadata = TikaMetadata::new();
    metadata.set(embedded::INTERNAL_PATH, name);
    metadata.set(embedded::RESOURCE_NAME, name);
    if name.contains("Pictures/") {
        embedded::ResourceType::Inline.set(&mut metadata);
        let detection = mime::detect(bytes, Origin::File { name }).map_err(|error| {
            Failure::Malformed(format!("Tika's detection throws on {name}: {error:?}"))
        })?;
        metadata.set("Content-Type", detection.media_type.to_string());
    }
    Ok(metadata)
}

/// Reads the document in `input`, whose detection left `metadata`, as far as deciding how it is
/// written: each entry Tika reads is read in its order up to the first one that fails Tika's
/// parser, those that spool to files held on to for the writing. A manifest's
/// `encryption-data` fails the document at once with the `EncryptedDocumentException` that
/// Tika's `OpenDocumentManifestHandler` throws.
///
/// Without a `content.xml` or `styles.xml`, nothing ends the document Tika starts: without
/// pictures either, Tika writes nothing of it, and leaves the metadata it read.
fn prepare<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    metadata: &TikaMetadata,
    configs: &'a ParserConfigs,
) -> Result<Prepared<'a>, Decline<'a>> {
    let archive = Archive::new(ZipPackage::open(input, pages, metadata, TIKA_NAME)?);
    let checked = check(&archive);
    archive.package.release();
    let has = |kind| archive.entries.iter().any(|entry| entry.kind == kind);
    let (bodies, pictures) = (has(EntryKind::Body), has(EntryKind::Embedded));
    if let (Ok(()), false, false) = (&checked, bodies, pictures) {
        let mut found = metadata.clone();
        if archive
            .read_each(|entry, bytes| read_metadata(entry.kind, bytes, &mut found))
            .is_ok()
        {
            return Err(Decline::Unwritten(found));
        }
    }
    let reading: Prepared<'a> = Box::new(move |xhtml| write(archive, configs, xhtml));
    match checked {
        Ok(()) if bodies => Ok(reading),
        Ok(()) => Err(Decline::Differs {
            reading,
            difference: Difference::new(UNENDED),
        }),
        Err(Checked::Encrypted) => Err(Decline::Fails(crypt::encrypted(None))),
        Err(Checked::Undecoded(name, undecoded)) => Err(undecoded.decline(&name, reading)),
        Err(Checked::Unspooled(error)) => Err(Decline::Fails(error)),
    }
}

/// What ends the check of an archive before its last entry.
enum Checked {
    Encrypted,
    /// The entry of the name is not decoded as Tika decodes it.
    Undecoded(String, Undecoded),
    /// Spooling an entry fails, with this error.
    Unspooled(crate::errors::Error),
}

/// Reads each entry of `archive` that Tika reads, in its order, checking each manifest for
/// `encryption-data`, up to the first entry that cannot be read. An unsupported entry fails the
/// writing where it fails Tika's parser, so the check ends there without a reason.
fn check(archive: &Archive) -> Result<(), Checked> {
    let zip = archive.package.zip();
    for index in archive.handled() {
        let entry = &archive.entries[index];
        let read = match entry.kind {
            EntryKind::Manifest => archive
                .package
                .spool(&zip, &entry.entry)
                .map(|manifest| ElementFinder::finds("encryption-data", &manifest, entry.name())),
            _ => archive
                .package
                .read_ahead(&zip, &entry.entry)
                .map(|()| false),
        };
        match read {
            Ok(false) => {}
            Ok(true) => return Err(Checked::Encrypted),
            Err(Unread::Unsupported(_)) => return Ok(()),
            Err(Unread::Undecoded(undecoded)) => {
                return Err(Checked::Undecoded(entry.name().to_owned(), undecoded))
            }
            Err(Unread::Unspooled(reason)) => {
                return Err(Checked::Unspooled(zip_package::unspooled(
                    entry.name(),
                    &reason,
                )))
            }
        }
    }
    Ok(())
}

/// Sets what the data `input` of an entry of `kind` says of the document in `metadata`: a
/// `meta.xml` its metadata, a `mimetype` its type.
fn read_metadata(
    kind: EntryKind,
    input: &Input,
    metadata: &mut TikaMetadata,
) -> Result<(), Failure> {
    match kind {
        EntryKind::Meta => meta::read_meta(input, metadata),
        EntryKind::Mimetype => {
            metadata.set("Content-Type", String::from_utf8_lossy(input));
            Ok(())
        }
        EntryKind::Manifest
        | EntryKind::Body
        | EntryKind::Embedded
        | EntryKind::Directory
        | EntryKind::Thumbnail
        | EntryKind::Ignored => Ok(()),
    }
}

pub struct OpenDocumentParser;

impl Parser for OpenDocumentParser {
    fn name(&self) -> &'static str {
        "extractous::OpenDocumentParser"
    }
    fn tika_name(&self) -> &'static str {
        TIKA_NAME
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn media_types(&self) -> &'static [&'static str] {
        &MEDIA_TYPES
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        prepare(input, pages, metadata, configs)
    }
}

/// Writes the document in `archive` as `OpenDocumentParser.handleZipFile` does.
fn write(archive: Archive, configs: &ParserConfigs, xhtml: &mut Xhtml) -> Result<(), Failure> {
    archive.read_each(|entry, input| {
        read_metadata(entry.kind, input, xhtml.metadata_mut())?;
        match entry.kind {
            EntryKind::Body => {
                let mut body = body::Body::new(xhtml, configs.office.includes_deleted_content());
                xml::parse_part(input, entry.name(), &mut body)
            }
            EntryKind::Embedded => {
                let metadata = embedded_metadata(entry.name(), input)?;
                Ok(xhtml.parse_embedded_input(input, &metadata, Wrapping::Bare)?)
            }
            EntryKind::Meta
            | EntryKind::Mimetype
            | EntryKind::Manifest
            | EntryKind::Directory
            | EntryKind::Thumbnail
            | EntryKind::Ignored => Ok(()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "tika")]
    use crate::config::OfficeParserConfig;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika_adjusting, conformance_corpus, Adjustments,
        Scratch,
    };
    use crate::formats::zip_package::tests::{replace_all, with_method};
    use std::io::Cursor;
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};
    use zip::CompressionMethod;

    #[cfg(feature = "tika")]
    const DOCUMENTS: [&str; 4] = [
        "../test_files/documents/simple.odt",
        "../quality/corpus/notes.odt",
        "../quality/corpus/prose.odt",
        "../quality/corpus/revisions.odt",
    ];

    #[cfg(feature = "tika")]
    fn paths<'a>(documents: &[&'a str]) -> Vec<&'a Path> {
        documents
            .iter()
            .map(|document| Path::new(*document))
            .collect()
    }

    /// How many headings and notes Tika's XHTML `xhtml` has: the native parser ends each with a
    /// line break, where Tika ends them in the ODF namespace, which breaks no line (#90).
    pub(crate) fn heading_and_note_ends(xhtml: &str, _native: &str) -> usize {
        const NOTES: [&str; 3] = ["class=\"note\"", "class=\"annotation\"", "class=\"notes\""];
        let mut ends = 0;
        let mut paragraphs = Vec::new();
        let mut rest = xhtml;
        while let Some(at) = rest.find('<') {
            let tag_end = rest[at..].find('>').map_or(rest.len(), |end| at + end + 1);
            let tag = &rest[at..tag_end];
            rest = &rest[tag_end..];
            let name: String = tag
                .trim_start_matches(['<', '/'])
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            let heading = matches!(name.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6");
            let note = name == "p" && NOTES.iter().any(|class| tag.contains(class));
            if tag.starts_with("</") {
                if heading || (name == "p" && paragraphs.pop() == Some(true)) {
                    ends += 1;
                }
            } else if tag.ends_with("/>") {
                if heading || note {
                    ends += 1;
                }
            } else if name == "p" {
                paragraphs.push(note);
            }
        }
        ends
    }

    /// How the native parser departs from Tika's path: it ends headings and notes as blocks.
    #[cfg(feature = "tika")]
    pub(crate) const DEPARTURES: Adjustments = Adjustments {
        line_breaks: Some(heading_and_note_ends),
        metadata: &[],
        wrappers: &[],
        repeated: &[],
        final_in_head: &[],
    };

    #[cfg(feature = "tika")]
    fn assert_same_as_tika(documents: &[&Path], tika_name: &str) {
        assert_same_as_tika_adjusting(documents, tika_name, &ParserConfigs::default(), DEPARTURES);
    }

    fn decoded(pieces: &[&str]) -> Vec<u8> {
        let mut data = BinaryData::new();
        for piece in pieces {
            data.push(piece).unwrap();
        }
        data.finish().unwrap().to_vec()
    }

    #[test]
    fn decodes_binary_data_in_pieces_as_whole_past_its_buffer() {
        let text = "SGVq IGRp\n".repeat(BINARY_DATA_BUFFER / 4);
        let pieces: Vec<&str> = text.split_inclusive('\n').collect();
        let whole = crate::java::commons_base64_decode(&text);
        assert!(whole.len() > BINARY_DATA_BUFFER);
        assert_eq!(decoded(&pieces), whole);
        assert_eq!(decoded(&["aGVsbA"]), b"hell");
        assert_eq!(decoded(&[]), b"");
    }

    #[test]
    fn counts_headings_and_notes_in_xhtml() {
        assert_eq!(
            heading_and_note_ends(
                "<h1>a</h1><p>b<p class=\"note\"><p>c</p></p></p><h2/><p class=\"notes\"/><p/>",
                ""
            ),
            4
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts() {
        assert_same_as_tika(&paths(&DOCUMENTS), TIKA_NAME);
    }

    #[cfg(feature = "tika")]
    const MISC_OFFICE: &str = "tika-parsers-standard/tika-parsers-standard-modules/\
                               tika-parser-miscoffice-module/src/test/resources/test-documents";
    #[cfg(feature = "tika")]
    const INTEGRATION: &str = "tika-parsers-standard/tika-parsers-standard-integration-tests/\
                               src/test/resources/test-documents";

    /// Tika's own test documents of this format, as `(directory, name)` in the directory
    /// `CONFORMANCE_CORPUS` names, which `just conformance` prepares.
    #[cfg(feature = "tika")]
    const CONFORMANCE: [(&str, &str); 21] = [
        (INTEGRATION, "testPhoneNumberExtractor.odt"),
        (MISC_OFFICE, "testFooter.ods"),
        (MISC_OFFICE, "testFooter.odt"),
        (MISC_OFFICE, "testMasterFooter.odp"),
        (MISC_OFFICE, "testNPEOpenDocument.odt"),
        (MISC_OFFICE, "testODPMacro.odp"),
        (MISC_OFFICE, "testODTNoMeta.odt"),
        (MISC_OFFICE, "testODSMacro.ods"),
        (MISC_OFFICE, "testODTEmbeddedImageLink.odt"),
        (MISC_OFFICE, "testODTEmbedded.odt"),
        (MISC_OFFICE, "testODTMacro.odt"),
        (MISC_OFFICE, "testODT-TIKA-6000.odt"),
        (MISC_OFFICE, "testODTStyles2.odt"),
        (MISC_OFFICE, "testODTStyles3.odt"),
        (MISC_OFFICE, "testODT_svgTitleInStyledSpan.odt"),
        (MISC_OFFICE, "testOpenOffice2.odt"),
        (MISC_OFFICE, "testStyles.odt"),
        (MISC_OFFICE, "versions/LibreOfficeCalc_ods_1.3.ods"),
        (MISC_OFFICE, "versions/LibreOfficeDraw_odg_1.3.odg"),
        (MISC_OFFICE, "versions/LibreOfficeImpress_odp_1.3.odp"),
        (MISC_OFFICE, "versions/LibreOfficeWriter_odt_1.3.odt"),
    ];

    /// Tika's own test documents the native parser reads differently from Tika's path on
    /// purpose (#90), as `(directory, name, XHTML it writes)`: spaces as `text:c` counts them,
    /// keywords each a value of their own, and the attributes of OpenOffice.org 1 documents.
    #[cfg(feature = "tika")]
    const CONFORMANCE_DEPARTING: [(&str, &str, &[&str]); 6] = [
        (
            INTEGRATION,
            "test-columnar.ods",
            &["<p>This is row            0 of           10</p>"],
        ),
        (
            INTEGRATION,
            "testStarOffice-6.0-writer.sxw",
            &[
                "<meta name=\"odf:version\" content=\"1.0\" />",
                "<h1 />\n<p>Example Text",
            ],
        ),
        (
            INTEGRATION,
            "testStarOffice-6.0-writer-template.stw",
            &["<meta name=\"meta:page-count\" content=\"1\" />"],
        ),
        (
            MISC_OFFICE,
            "testODFwithOOo3.odt",
            &[
                "<meta name=\"meta:keyword\" content=\"Lucene\" />",
                "<meta name=\"dc:subject\" content=\"Tika\" />",
            ],
        ),
        (MISC_OFFICE, "testODP_NPE.odp", &["  "]),
        (
            MISC_OFFICE,
            "testOpenOffice2.odf",
            &["<meta name=\"meta:keyword\" content=\"fox\" />"],
        ),
    ];

    /// Tika's own test documents of this format that Tika's path fails on.
    #[cfg(feature = "tika")]
    const CONFORMANCE_FAILING: [(&str, &str); 1] = [(MISC_OFFICE, "testODTEncrypted.odt")];

    #[cfg(feature = "tika")]
    #[test]
    #[ignore]
    fn fails_where_tikas_path_fails_on_tikas_documents() {
        let corpus = conformance_corpus();
        let documents: Vec<PathBuf> = CONFORMANCE_FAILING
            .iter()
            .map(|(directory, name)| corpus.join(directory).join(name))
            .collect();
        let documents: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&documents);
    }

    #[cfg(feature = "tika")]
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
        for (directory, name, texts) in CONFORMANCE_DEPARTING {
            let (xml, _) = extract(&std::fs::read(corpus.join(directory).join(name)).unwrap());
            for text in texts {
                assert!(xml.contains(text), "{name}: {text:?} in {xml}");
            }
        }
    }

    pub(super) const NAMESPACES: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0""#;
    pub(super) const TEXT: &str = "application/vnd.oasis.opendocument.text";
    pub(super) const MANIFEST: (&str, &str) = (
        "META-INF/manifest.xml",
        r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/></manifest:manifest>"#,
    );

    fn content(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {NAMESPACES}><office:automatic-styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style><style:style style:name="T2" style:family="text"><style:text-properties fo:font-style="italic" style:text-underline-style="solid"/></style:style><style:style style:name="T3" style:family="text"><style:text-properties fo:font-weight="600" fo:font-style="oblique"/></style:style><style:style style:name="P1" style:family="paragraph"><style:text-properties fo:font-weight="bold"/></style:style><style:style style:family="paragraph"><style:text-properties style:text-underline-style="solid"/></style:style><style:style style:name="C1" style:family="table-cell"/><text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style><text:list-style style:name="L2"><text:list-level-style-bullet text:level="1"><style:text-properties fo:font-weight="bold"/></text:list-level-style-bullet></text:list-style></office:automatic-styles><office:body>{body}</office:body></office:document-content>"#
        )
    }

    /// An OpenDocument file of `entries`, stored in that order after the `mimetype`.
    pub(super) fn document(media_type: &str, entries: &[(&str, &str)]) -> Vec<u8> {
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

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts_from_every_kind_of_element() {
        let text = content(
            &r#"<office:text><text:sequence-decls><text:sequence-decl text:name="Table"/></text:sequence-decls><text:tracked-changes><text:changed-region text:id="c1"><text:deletion><text:p>gone words</text:p></text:deletion></text:changed-region></text:tracked-changes>
<text:h text:outline-level="2">Heading <text:span text:style-name="T1">bold</text:span> two</text:h><text:h>Plain heading</text:h><text:h text:outline-level="9">Deep</text:h><text:h text:outline-level="0">Zero</text:h>
<text:p text:style-name="P1">Bold paragraph <text:span text:style-name="T2">italic under</text:span><text:span text:style-name="T3">semi</text:span> end<text:tab/>tab<text:s/>space<text:line-break/>next</text:p>
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
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta {NAMESPACES} office:version="1.3"><office:meta><meta:generator>Test</meta:generator><dc:title>Elements</dc:title><meta:initial-creator>Anna</meta:initial-creator><dc:creator>Bo</dc:creator><meta:keyword>hav</meta:keyword><dc:subject>Lys</dc:subject><dc:language>da-DK</dc:language><dc:date>2026-01-02T03:04:05</dc:date><meta:creation-date>2026-01-01T00:00:00</meta:creation-date><meta:editing-duration>PT1M</meta:editing-duration><meta:editing-cycles>3</meta:editing-cycles><meta:user-defined meta:name="Info 1">Tekst</meta:user-defined><meta:user-defined meta:name="Info 1">Mere</meta:user-defined><meta:document-statistic meta:page-count="2" meta:paragraph-count="9" meta:word-count="40" meta:character-count="200" meta:table-count="1" meta:image-count="2" meta:object-count="1"/></office:meta></office:document-meta>"#
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
                        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta xmlns:office="http://openoffice.org/2000/office" xmlns:meta="http://openoffice.org/2000/meta" xmlns:dc="http://purl.org/dc/elements/1.1/"><office:meta><meta:generator>StarOffice</meta:generator><dc:title>Old</dc:title></office:meta></office:document-meta>"#,
                    ),
                    MANIFEST,
                ],
            ),
            ),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    #[cfg(feature = "tika")]
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

    /// The XHTML and metadata the native parser extracts from `input`.
    fn extract(input: &[u8]) -> (String, crate::Metadata) {
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(input)
            .unwrap()
    }

    /// What Tika's path gets wrong and the native parser does not (#90): headings and notes end
    /// as blocks, `text:c` counts spaces, malformed style values are ignored rather than failing
    /// the document, keywords and creators are values of their own, and the attributes of
    /// OpenOffice.org 1 documents are read.
    #[test]
    fn reads_what_tikas_path_misreads() {
        let text = content(
            r#"<office:text><text:h text:outline-level="x">Heading</text:h><text:p>a<text:s text:c="3"/>b<text:note><text:note-body><text:p>Note</text:p></text:note-body></text:note>after</text:p><text:p><text:span text:style-name="T3">semi</text:span><text:span text:style-name="T1">bold</text:span></text:p></office:text>"#,
        )
        .replace(r#"fo:font-weight="600""#, r#"fo:font-weight="6x""#);
        let meta = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta {NAMESPACES}><office:meta><dc:creator>Bo</dc:creator><meta:initial-creator>Anna</meta:initial-creator><meta:keyword>hav</meta:keyword><meta:keyword>sten</meta:keyword></office:meta></office:document-meta>"#
        );
        let input = document(
            TEXT,
            &[("content.xml", &text), ("meta.xml", &meta), MANIFEST],
        );
        let (xml, metadata) = extract(&input);
        assert!(
            xml.contains(
                "<h1><b><u>Heading</u></b></h1>\n<p>a   bafter</p><p class=\"note\"> Note </p>\n\n\
                 <p><i>semi</i><b>bold</b></p>"
            ),
            "{xml}"
        );
        assert_eq!(metadata["dc:creator"], ["Bo", "Anna"]);
        assert_eq!(metadata["meta:keyword"], ["hav", "sten"]);
        assert_eq!(metadata["dc:subject"], ["hav", "sten"]);

        let old = document(
            "application/vnd.sun.xml.writer",
            &[
                (
                    "content.xml",
                    r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="http://openoffice.org/2000/office" xmlns:style="http://openoffice.org/2000/style" xmlns:text="http://openoffice.org/2000/text" xmlns:fo="http://www.w3.org/1999/XSL/Format"><office:automatic-styles><text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style></office:automatic-styles><office:body><text:list text:style-name="L1"><text:list-item><text:p>One</text:p></text:list-item></text:list></office:body></office:document-content>"#,
                ),
                (
                    "meta.xml",
                    r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta xmlns:office="http://openoffice.org/2000/office" xmlns:meta="http://openoffice.org/2000/meta" office:version="1.0"><office:meta><meta:document-statistic meta:page-count="1"/></office:meta></office:document-meta>"#,
                ),
                MANIFEST,
            ],
        );
        let (xml, metadata) = extract(&old);
        assert!(xml.contains("<ol>\t<li><p>One</p>\n</li>\n</ol>"), "{xml}");
        assert_eq!(metadata["odf:version"], ["1.0"]);
        assert_eq!(metadata["meta:page-count"], ["1"]);
    }

    #[test]
    fn reads_documents_with_pictures_and_binary_data() {
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
            None
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

    fn meta_titled(title: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-meta {NAMESPACES}><office:meta><dc:title>{title}</dc:title></office:meta></office:document-meta>"#
        )
    }

    fn paragraph(text: &str) -> String {
        content(&format!(
            "<office:text><text:p>{text}</text:p></office:text>"
        ))
    }

    /// A document whose central directory is cut off, which Tika reads as `ZipSalvager` rebuilds
    /// it.
    fn cut() -> Vec<u8> {
        let whole = document(
            TEXT,
            &[
                ("content.xml", &paragraph("Kept text")),
                ("meta.xml", &meta_titled("Cut")),
                MANIFEST,
            ],
        );
        whole[..whole.len() - 10].to_vec()
    }

    /// A document cut short in the middle of its `content.xml`.
    fn cut_in_content() -> Vec<u8> {
        let whole = document(
            TEXT,
            &[
                ("meta.xml", &meta_titled("Cut")),
                ("content.xml", &paragraph("Kept text")),
            ],
        );
        let content = whole
            .windows(b"Kept text".len())
            .position(|window| window == b"Kept text")
            .unwrap();
        whole[..content].to_vec()
    }

    /// A document with two entries of each name of its parts.
    fn duplicated() -> Vec<u8> {
        let duplicated = document(
            TEXT,
            &[
                ("meta.xml", &meta_titled("First")),
                ("content.xml", &paragraph("One")),
                ("meta.xmm", &meta_titled("Second")),
                ("content.xmm", &paragraph("Two")),
                MANIFEST,
            ],
        );
        replace_all(&duplicated, b".xmm", b".xml")
    }

    /// A document whose deflated `content.xml` is corrupt.
    fn corrupt() -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored =
            zip::write::SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        zip.start_file("mimetype", stored).unwrap();
        std::io::Write::write_all(&mut zip, TEXT.as_bytes()).unwrap();
        zip.start_file(
            "content.xml",
            stored.compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
        std::io::Write::write_all(&mut zip, paragraph("Lost").as_bytes()).unwrap();
        let mut bytes = zip.finish().unwrap().into_inner();
        let data = 30 + "mimetype".len() + TEXT.len() + 30 + "content.xml".len();
        bytes[data..data + 8].copy_from_slice(&[0xFF; 8]);
        bytes
    }

    /// A document whose `content.xml` says it is compressed with LZMA, which commons-compress
    /// does not decompress.
    fn unsupported() -> Vec<u8> {
        with_method(
            document(TEXT, &[("content.xml", &paragraph("Locked")), MANIFEST]),
            "content.xml",
            14,
        )
    }

    fn salvaged() -> TikaMetadata {
        let mut metadata = TikaMetadata::new();
        metadata.set(crate::mime::Key::Salvaged.name(), "true");
        metadata
    }

    #[test]
    fn reads_packages_zip_file_cannot_open_as_tika_salvages_them() {
        let configs = ParserConfigs::default();
        assert!(OpenDocumentParser
            .prepare(&cut(), &salvaged(), &configs)
            .is_ok());
        let (xml, metadata) = extract(&cut());
        assert!(xml.contains("<p><b><u>Kept text</u></b></p>"), "{xml}");
        assert_eq!(metadata["dc:title"], ["Cut"]);
        let error = crate::Extractor::new()
            .extract_bytes_to_string(&cut_in_content())
            .unwrap_err();
        assert!(error.to_string().contains("content.xml"), "{error}");
    }

    #[test]
    fn reads_every_entry_of_a_name() {
        let (xml, metadata) = extract(&duplicated());
        assert!(
            xml.contains("<p><b><u>One</u></b></p>\n<p><b><u>Two</u></b></p>"),
            "{xml}"
        );
        assert_eq!(metadata["dc:title"], ["First"]);
    }

    #[test]
    fn fails_as_tika_fails_what_is_not_a_zip_archive() {
        let configs = ParserConfigs::default();
        let declined = OpenDocumentParser
            .prepare(b"not a zip", &TikaMetadata::new(), &configs)
            .err()
            .map(|declined| declined.to_string());
        assert_eq!(
            declined.as_deref(),
            Some(
                "Parse error occurred : org.apache.tika.exception.TikaException: TIKA-198: \
                 Illegal IOException from org.apache.tika.parser.odf.OpenDocumentParser <- \
                 caused by java.util.zip.ZipException: Archive is not a ZIP archive"
            )
        );
    }

    #[test]
    fn fails_as_tika_fails_an_entry_commons_compress_does_not_decompress() {
        let error = crate::Extractor::new()
            .extract_bytes_to_string(&unsupported())
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Parse error occurred : org.apache.tika.exception.TikaException: TIKA-198: Illegal \
             IOException from org.apache.tika.parser.odf.OpenDocumentParser <- caused by \
             org.apache.commons.io.TaggedIOException: Unsupported compression method 14 (LZMA) \
             used in entry content.xml <- caused by \
             org.apache.commons.compress.archivers.zip.UnsupportedZipFeatureException: \
             Unsupported compression method 14 (LZMA) used in entry content.xml"
        );
    }

    #[test]
    fn fails_corrupt_entries_as_malformed() {
        let (input, configs) = (corrupt(), ParserConfigs::default());
        let declined = OpenDocumentParser
            .prepare(&input, &TikaMetadata::new(), &configs)
            .err();
        assert!(
            matches!(
                &declined,
                Some(Decline::Differs { difference, .. })
                    if difference.0.starts_with("the data of the entry content.xml is cut short")
            ),
            "{declined:?}"
        );
        if !cfg!(feature = "tika") {
            let error = crate::Extractor::new()
                .extract_bytes_to_string(&corrupt())
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("content.xml is cut short or corrupt"),
                "{error}"
            );
        }
    }

    fn bodiless() -> Vec<u8> {
        document(TEXT, &[("meta.xml", &meta_titled("Bodiless")), MANIFEST])
    }

    fn pictured() -> Vec<u8> {
        document(
            TEXT,
            &[
                ("meta.xml", &meta_titled("Pictured")),
                ("Pictures/a.txt", "picture text"),
                MANIFEST,
            ],
        )
    }

    #[test]
    fn writes_nothing_of_a_document_without_a_body_or_pictures() {
        let (input, configs) = (bodiless(), ParserConfigs::default());
        let declined = OpenDocumentParser
            .prepare(&input, &TikaMetadata::new(), &configs)
            .err();
        let Some(Decline::Unwritten(metadata)) = declined else {
            panic!("{declined:?}");
        };
        assert_eq!(metadata.get("dc:title"), Some("Bodiless"));
        assert_eq!(metadata.get("Content-Type"), Some(TEXT));
        let input = pictured();
        let declined = OpenDocumentParser
            .prepare(&input, &TikaMetadata::new(), &configs)
            .err();
        assert!(
            matches!(&declined, Some(Decline::Differs { difference, .. }) if difference.0 == UNENDED),
            "{declined:?}"
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts_from_damaged_packages() {
        let scratch = Scratch::new("odf-damaged");
        let documents = [
            scratch.write("cut.odt", &cut()),
            scratch.write("duplicated.odt", &duplicated()),
            scratch.write("bodiless.odt", &bodiless()),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
        let pictured = scratch.write("pictured.odt", &pictured());
        let (_, xhtml) = crate::formats::parity::differing_reading(&OpenDocumentParser, &pictured);
        assert!(
            xhtml.contains("<p>picture text</p>") && xhtml.trim_end().ends_with("</body></html>"),
            "{xhtml}"
        );
    }

    /// Tika's `CompositeParser` names the parser it wraps an exception for by the decorator
    /// around `OpenDocumentParser`, `MimeFilteringDecorator@<hash>`, which differs on each run,
    /// where the native path names `OpenDocumentParser`: what each wraps is compared.
    #[cfg(feature = "tika")]
    #[test]
    fn fails_where_tikas_path_fails_on_damaged_packages() {
        let scratch = Scratch::new("odf-damaged-failing");
        let documents = [
            scratch.write("cut-in-content.odt", &cut_in_content()),
            scratch.write("unsupported.odt", &unsupported()),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&paths);
        let cause = |error: &str| {
            error
                .split_once(" <- caused by ")
                .map(|(_, cause)| cause.to_owned())
        };
        let (native, tika) =
            crate::formats::parity::errors(&documents[1], &ParserConfigs::default());
        assert!(cause(&native).is_some(), "{native}");
        assert_eq!(cause(&native), cause(&tika));
    }

    const UNDECRYPTABLE: &str = "../test_files/documents/encrypted/undecryptable.odt";

    #[test]
    fn fails_encrypted_documents_as_encrypted() {
        let input = std::fs::read(UNDECRYPTABLE).unwrap();
        let configs = ParserConfigs::default();
        let declined = OpenDocumentParser
            .prepare(&input, &TikaMetadata::new(), &configs)
            .err();
        assert!(
            matches!(
                &declined,
                Some(Decline::Fails(crate::Error::Encrypted(reason)))
                    if *reason == crypt::encrypted(None).to_string()
            ),
            "{declined:?}"
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    fn fails_encrypted_documents_where_tikas_path_fails() {
        assert_fails_as_tika(&[Path::new(UNDECRYPTABLE)]);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_deleted_content_as_tikas_path_does_when_asked() {
        let configs = ParserConfigs {
            office: OfficeParserConfig::new().set_include_deleted_content(true),
            ..ParserConfigs::default()
        };
        assert_same_as_tika_adjusting(
            &paths(&["../quality/corpus/revisions.odt"]),
            TIKA_NAME,
            &configs,
            DEPARTURES,
        );
    }
}
