//! EPUB books (and iBooks), as Tika's `EpubParser` reads them under extractous's substitution of
//! which manifest items it parses as embedded documents: the package document's Dublin Core
//! metadata, layout and version, the `mimetype` and `metadata.xml`, then each XHTML content
//! document of the spine, read as XML and written body by body, a malformed one reported and
//! skipped, and then, in manifest order, the items outside the spine our `EpubEmbedded` admits,
//! such as pictures and fonts, each in an `embedded` div. Without a package document, or with an
//! empty spine, Tika writes every HTML and XML entry it can and fails. Encrypted books are left to
//! Tika.

mod content;
mod package;

use std::collections::HashSet;

use crate::config::ParserConfigs;
use crate::formats::embedded::{self, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml;
use crate::formats::zip_file::{ZipEntry, ZipFile};
use crate::formats::{Decline, Deferral, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages};
use crate::java;
use crate::java::is_blank;

use content::ContentDocument;
use package::{Contents, Item, MetadataParser};

const CONTAINER: &str = "META-INF/container.xml";
const ENCRYPTION: &str = "META-INF/encryption.xml";
const MIMETYPE: &str = "mimetype";
const METADATA: &str = "metadata.xml";

/// The media types of manifest items Tika's `EpubParser` lists the ids of contents with.
const CONTENTS_TYPES: [&str; 3] = [
    "application/x-dtbncx+xml",
    "application/xhtml+xml",
    "text/html",
];

/// Our `EpubEmbedded.shouldHandle`: whether Tika parses a manifest item outside the spine, of
/// `media_type`, as an embedded document. Not stylesheets, SVG, XML, Apple's display options and
/// the tables of contents.
fn parses_as_embedded(media_type: Option<&str>) -> bool {
    let Some(media_type) = media_type else {
        return true;
    };
    let media_type = media_type.to_lowercase();
    !(media_type.contains("css")
        || media_type.contains("svg")
        || media_type.ends_with("/xml")
        || media_type.contains("x-ibooks")
        || CONTENTS_TYPES.contains(&media_type.as_str()))
}

/// Whether Tika reads a spine item as a content document: by its media type when it has one,
/// else by the ending of its `href`.
fn is_content_document(item: &Item) -> bool {
    match &item.media_type {
        Some(media_type) => media_type.to_lowercase().contains("html"),
        None => {
            let href = item.href.to_lowercase();
            href.ends_with("htm") || href.ends_with("html") || href.ends_with(".xml")
        }
    }
}

/// Whether Tika's fallback reads an entry as a content document, by the ending of its name.
fn is_fallback_document(name: &str) -> bool {
    let name = name.to_lowercase();
    [".xhtml", ".html", ".htm", ".xml"]
        .iter()
        .any(|ending| name.ends_with(ending))
}

/// How Tika's `EpubParser` reads a book, decided by its package document.
enum Reading {
    /// Through the package document `root`, with the manifest and spine it lists.
    Package {
        root: ZipEntry,
        contents: Box<Contents>,
    },
    /// Every HTML and XML entry, without a package document to follow, which ends in a failure.
    Fallback(&'static str),
}

/// A failure as Tika's `EpubParser.parse` sees it: an `IOException`, after which it still ends
/// the document, or any other.
enum Thrown {
    Io(Failure),
    Other(Failure),
}

impl From<Failure> for Thrown {
    fn from(failure: Failure) -> Self {
        Self::Other(failure)
    }
}

impl From<crate::formats::xhtml::Stop> for Thrown {
    fn from(stop: crate::formats::xhtml::Stop) -> Self {
        Self::Other(stop.into())
    }
}

/// An opened book.
struct Book<'a> {
    zip: ZipFile<'a>,
}

impl<'a> Book<'a> {
    fn named(&self, name: &str) -> Option<ZipEntry> {
        self.zip.named(name).cloned()
    }

    fn read(&mut self, entry: &ZipEntry) -> Result<Input<'a>, Failure> {
        self.zip.read(entry)
    }

    /// `getRoot`: the package document `container.xml` names, or without one, the first entry
    /// whose name ends in `.opf`.
    fn root(&mut self) -> Result<Option<String>, Failure> {
        match self.named(CONTAINER) {
            Some(container) => package::root(&self.read(&container)?),
            None => Ok(self
                .zip
                .entries()
                .iter()
                .find(|entry| entry.name.to_lowercase().ends_with(".opf"))
                .map(|entry| entry.name.clone())),
        }
    }

    /// How Tika reads the book, reading the package document's metadata on the way, as it does.
    fn reading(&mut self, metadata: &mut TikaMetadata) -> Result<Reading, Failure> {
        let Some(root) = self.root()? else {
            return Ok(Reading::Fallback(
                "no OPF found in (possibly truncated) container",
            ));
        };
        let Some(root) = self.named(&root) else {
            return Ok(Reading::Fallback(
                "OPF entry missing or unreadable in (possibly truncated) container",
            ));
        };
        let document = self.read(&root)?;
        package::read_metadata(&document, &root.name, MetadataParser::Package, metadata)?;
        let contents = Contents::read(&document, &root.name)?;
        if contents.spine.is_empty() {
            return Ok(Reading::Fallback(
                "OPF declared no spine items in (possibly truncated) container",
            ));
        }
        Ok(Reading::Package {
            root,
            contents: Box::new(contents),
        })
    }

    /// `extractMetadata`: the media type the `mimetype` entry names, and `metadata.xml`.
    fn extract_metadata(&mut self, metadata: &mut TikaMetadata) -> Result<(), Failure> {
        if let Some(mimetype) = self.named(MIMETYPE) {
            let bytes = self.read(&mimetype)?;
            metadata.set("Content-Type", java::trim(&String::from_utf8_lossy(&bytes)));
        }
        if let Some(entry) = self.named(METADATA) {
            let bytes = self.read(&entry)?;
            package::read_metadata(&bytes, METADATA, MetadataParser::DublinCore, metadata)?;
        }
        Ok(())
    }
}

/// The directory of the package document, which manifest `href`s are relative to.
fn relative_path(root: &str) -> &str {
    root.rfind('/').map_or("", |slash| &root[..=slash])
}

/// What the content document `entry` of `book` leaves: written, failed as malformed XML, which
/// Tika remembers and moves on from, or stopped.
fn write_content_document(
    book: &mut Book,
    entry: &ZipEntry,
    xhtml: &mut Xhtml,
) -> Result<Option<Failure>, Thrown> {
    let bytes = book.read(entry).map_err(Thrown::Io)?;
    let mut document = ContentDocument::new(xhtml);
    match xml::parse_part(&bytes, &entry.name, &mut document) {
        Ok(()) => Ok(None),
        Err(failure @ Failure::Stopped(_)) => Err(failure.into()),
        Err(failure) => {
            document.drain_open_elements()?;
            Ok(Some(failure))
        }
    }
}

/// The metadata Tika's `handleEmbedded` gives a manifest item at `path`.
fn embedded_metadata(item: &Item, path: &str, cover: bool) -> TikaMetadata {
    let mut metadata = TikaMetadata::new();
    if let Some(media_type) = item.media_type.as_deref().filter(|media| !is_blank(media)) {
        metadata.set("Content-Type", media_type);
    }
    metadata.set(embedded::RESOURCE_NAME, path);
    if cover {
        embedded::ResourceType::Thumbnail.set(&mut metadata);
    }
    metadata
}

/// `handleEmbedded`: a manifest item outside the spine, parsed as an embedded document in an
/// `embedded` div, wrapped as a package entry unless it is a font.
fn write_embedded(
    book: &mut Book,
    relative: &str,
    item: &Item,
    cover: bool,
    xhtml: &mut Xhtml,
) -> Result<(), Thrown> {
    let path = format!("{relative}{}", item.href);
    let Some(entry) = book.named(&path) else {
        return Ok(());
    };
    let metadata = embedded_metadata(item, &path, cover);
    if !embedded::is_parsed(&metadata) {
        return Ok(());
    }
    let bytes = book.read(&entry).map_err(Thrown::Io)?;
    // Tika tests the `href` for `fonts` in its case only (#88).
    let font = item
        .media_type
        .as_deref()
        .is_some_and(|media| media.contains("font"))
        || item
            .href
            .get(..5)
            .is_some_and(|start| start.eq_ignore_ascii_case("fonts"));
    xhtml.start_element_with("div", &[("class", "embedded")])?;
    xhtml.parse_embedded(
        &bytes,
        &metadata,
        if font {
            Wrapping::Bare
        } else {
            Wrapping::PackageEntry
        },
    )?;
    xhtml.end_element("div")?;
    Ok(())
}

/// `bufferedParseZipFile` along the package document. A malformed content document is
/// reported as a warning and skipped: Tika fails the whole book on it, after writing everything
/// else (#89).
fn read_package(
    book: &mut Book,
    root: &ZipEntry,
    contents: &Contents,
    xhtml: &mut Xhtml,
) -> Result<(), Thrown> {
    let relative = relative_path(&root.name).to_owned();
    book.extract_metadata(xhtml.metadata_mut())?;
    let mut processed: HashSet<&str> = HashSet::new();
    let mut missing = 0;
    for id in &contents.spine {
        let Some(item) = contents.item(id) else {
            continue;
        };
        if !is_content_document(item) {
            continue;
        }
        let Some(entry) = book.named(&format!("{relative}{}", item.href)) else {
            missing += 1;
            continue;
        };
        let written = write_content_document(book, &entry, xhtml);
        processed.insert(id);
        if let Some(failure) = written? {
            let failure = match failure {
                Failure::Malformed(reason) => reason,
                other => format!("{other:?}"),
            };
            embedded::record_exception(
                xhtml.metadata_mut(),
                &format!(
                    "org.apache.tika.exception.TikaException: EPUB: the content document {} is \
                     malformed: {failure}",
                    entry.name
                ),
            );
        }
    }
    let cover = contents.cover();
    for (id, item) in &contents.items {
        if processed.contains(id.as_str()) || !parses_as_embedded(item.media_type.as_deref()) {
            continue;
        }
        write_embedded(book, &relative, item, cover == Some(id.as_str()), xhtml)?;
    }
    if missing > 0 {
        return Err(Thrown::Io(Failure::Malformed(format!(
            "EPUB: {missing} of {} spine items missing from (possibly truncated) container",
            contents.spine.len()
        ))));
    }
    Ok(())
}

/// `fallbackParseAllHtmlEntries`: the metadata as far as Tika reads it, failing or not, every HTML
/// and XML entry, a malformed one skipped, and then a failure.
fn read_fallback(book: &mut Book, reason: &str, xhtml: &mut Xhtml) -> Result<(), Thrown> {
    match book.extract_metadata(xhtml.metadata_mut()) {
        Ok(()) | Err(_) => {}
    }
    let entries: Vec<ZipEntry> = book
        .zip
        .entries()
        .iter()
        .filter(|entry| is_fallback_document(&entry.name))
        .cloned()
        .collect();
    for entry in &entries {
        match write_content_document(book, entry, xhtml) {
            Ok(_) | Err(Thrown::Io(_)) => {}
            Err(stopped @ Thrown::Other(_)) => return Err(stopped),
        }
    }
    Err(Thrown::Io(Failure::Malformed(format!(
        "EPUB: fallback recovery ({reason})"
    ))))
}

/// Reads the book in `input` as far as deciding whether this parser extracts it as Tika does:
/// the book and how it is read, along its package document, with the metadata it adds to
/// `metadata`, and its content documents, each read again as it is written, so that no more than
/// one is held at a time; or why Tika's path should read it.
fn prepare<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    metadata: &mut TikaMetadata,
) -> Result<(Book<'a>, Result<Reading, Failure>), Deferral> {
    let Ok(zip) = ZipFile::open(input, pages, "EPUB") else {
        return Err("its archive is not one the zip crate reads".to_owned());
    };
    if !zip.has_every_record(input) {
        return Err(
            "its archive has entries the zip crate does not list, such as duplicates".to_owned(),
        );
    }
    if !zip.reads_every_entry() {
        return Err("its archive has entries the zip crate cannot decompress".to_owned());
    }
    let mut book = Book { zip };
    let reading = book.reading(metadata);
    let documents: Vec<ZipEntry> = match &reading {
        Err(_) => Vec::new(),
        Ok(Reading::Fallback(_)) => book
            .zip
            .entries()
            .iter()
            .filter(|entry| is_fallback_document(&entry.name))
            .cloned()
            .collect(),
        Ok(Reading::Package { root, contents }) => {
            if let Some(encryption) = book.named(ENCRYPTION) {
                let names = book
                    .read(&encryption)
                    .and_then(|bytes| package::names_encrypted_items(&bytes));
                if !matches!(names, Ok(false)) {
                    return Err("its encryption.xml names encrypted items".to_owned());
                }
            }
            let relative = relative_path(&root.name).to_owned();
            contents
                .spine
                .iter()
                .filter_map(|id| contents.item(id))
                .filter(|item| is_content_document(item))
                .filter_map(|item| book.named(&format!("{relative}{}", item.href)))
                .collect()
        }
    };
    for document in &documents {
        if book.read(document).is_err() {
            return Err(format!("its entry {} cannot be read", document.name));
        }
    }
    Ok((book, reading))
}

pub struct EpubParser;

impl Parser for EpubParser {
    fn name(&self) -> &'static str {
        "extractous::EpubParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.epub.EpubParser"
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/epub+zip", "application/x-ibooks+zip"]
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
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let mut metadata = metadata.clone();
        let (mut book, reading) = prepare(input, pages, &mut metadata)?;
        Ok(Box::new(move |xhtml| {
            *xhtml.metadata_mut() = metadata;
            write(&mut book, reading, xhtml)
        }))
    }
}

/// Writes the book as `EpubParser.parse` does along the `reading` its preparation found, which
/// ends the document after an `IOException` too.
fn write(
    book: &mut Book,
    reading: Result<Reading, Failure>,
    xhtml: &mut Xhtml,
) -> Result<(), Failure> {
    let read = reading
        .map_err(Thrown::from)
        .and_then(|reading| match reading {
            Reading::Package { root, contents } => read_package(book, &root, &contents, xhtml),
            Reading::Fallback(reason) => read_fallback(book, reason, xhtml),
        });
    match read {
        Ok(()) => Ok(()),
        Err(Thrown::Other(failure)) => Err(failure),
        Err(Thrown::Io(failure)) => {
            xhtml.end_document()?;
            Err(failure)
        }
    }
}

/// For the parity tests: the books of Tika's tests with pictures outside the spine, which the
/// native parser writes in manifest order and Tika's path in the order of a `HashMap` of their
/// ids (#88).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const IN_HASH_ORDER: [&str; 2] = ["testEPUB_multi-metadata-vals.epub", "testiBooks.ibooks"];

/// For the parity tests: the books [`IN_HASH_ORDER`] compared with their lines, and the files a
/// spool holds, in any order (#88).
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const MANIFEST_ORDER: crate::formats::parity::Departures =
    &[crate::formats::parity::Departure::Paragraphs("#88", &[])];

/// A native result as Tika's path writes it, without the fixes of #89 that real books
/// meet everywhere: an element's `xml:lang` and `lang` collapse into the first, named
/// `lang`, and a publication or modification date beside a `dcterms:created` is dropped,
/// as Tika's last `dc:date` wins.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub fn as_tika_writes_it(
    text: &mut String,
    metadata: &mut crate::formats::parity::ComparedMetadata,
    _tika: &crate::formats::parity::ComparedMetadata,
) {
    let dates = ["dcterms:issued", "dcterms:modified"];
    if metadata.iter().any(|(name, _)| name == "dcterms:created") {
        metadata.retain(|(name, _)| !dates.contains(&name.as_str()));
        for date in dates {
            let meta = format!(r#"<meta name="{date}" content=""#);
            while let Some(start) = text.find(&meta) {
                let end = start + text[start..].find('\n').unwrap() + 1;
                text.replace_range(start..end, "");
            }
        }
    }
    let mut rewritten = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find('<') {
        let end = start
            + rest[start..]
                .find('>')
                .map_or(rest.len() - start, |end| end + 1);
        rewritten.push_str(&rest[..start]);
        let tag = &rest[start..end];
        if tag.contains(" xml:lang=\"") {
            rewritten.push_str(&with_one_lang(tag));
        } else {
            rewritten.push_str(tag);
        }
        rest = &rest[end..];
    }
    rewritten.push_str(rest);
    *text = rewritten;
}

/// The start tag `tag` with `xml:lang` named `lang`, and only the first `lang` kept.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
fn with_one_lang(tag: &str) -> String {
    let mut kept = String::with_capacity(tag.len());
    let mut seen = false;
    let mut rest = tag;
    while let Some(at) = rest.find(" xml:lang=\"").or_else(|| rest.find(" lang=\"")) {
        let at = [rest.find(" xml:lang=\""), rest.find(" lang=\"")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(at);
        let value_start = at + rest[at..].find('"').expect("the attribute's quote") + 1;
        // A text cut at the length limit may end inside the value.
        let Some(value_end) = rest[value_start..].find('"').map(|end| value_start + end) else {
            break;
        };
        kept.push_str(&rest[..at]);
        if !seen {
            kept.push_str(&format!(r#" lang="{}""#, &rest[value_start..value_end]));
            seen = true;
        }
        rest = &rest[value_end + 1..];
    }
    kept.push_str(rest);
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deferral(input: &[u8]) -> Option<String> {
        EpubParser.defers(input, &TikaMetadata::new(), &ParserConfigs::default())
    }
    #[cfg(feature = "tika")]
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, assert_same_as_tika_departing_rewriting,
        assert_same_as_tika_rewriting, conformance_corpus, documents, Scratch,
    };
    use std::io::{Cursor, Write};
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    #[cfg(feature = "tika")]
    const TIKA_NAME: &str = "org.apache.tika.parser.epub.EpubParser";
    #[cfg(feature = "tika")]
    const DOCUMENTS: [&str; 2] = [
        "../quality/corpus/prose.epub",
        "../test_files/documents/winter-sports.epub",
    ];
    const MIMETYPE_ENTRY: (&str, &str) = ("mimetype", "application/epub+zip\n");
    const CONTAINER_ENTRY: (&str, &str) = (
        CONTAINER,
        r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    );

    /// A book of `entries`, stored in that order.
    fn book(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, content) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    /// A package document of `metadata`, `manifest` items and `spine` itemrefs.
    fn package(version: &str, metadata: &str, manifest: &str, spine: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><package xmlns="http://www.idpf.org/2007/opf" version="{version}" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">{metadata}</metadata><manifest>{manifest}</manifest><spine>{spine}</spine></package>"#
        )
    }

    /// A content document of `body`, which may map prefixes of its own.
    fn chapter(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Not written</title></head><body>{body}</body></html>"#
        )
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts() {
        let documents: Vec<&Path> = DOCUMENTS.iter().map(Path::new).collect();
        assert_same_as_tika_rewriting(&documents, TIKA_NAME, as_tika_writes_it);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts_from_every_kind_of_book() {
        let scratch = Scratch::new("epub-books");
        let opf = package(
            "2.0",
            r#"<dc:title>Fyret</dc:title><dc:creator>Anna</dc:creator><dc:creator>Bo</dc:creator><dc:subject>Lys</dc:subject><dc:date>2026-01-02</dc:date><dc:language>da</dc:language><meta name="cover" content=" cover "/>"#,
            r#"<item id="ch1" href="chapter1.xhtml" media-type="application/xhtml+xml"/><item id="ch2" href="chapter%202%20b.html"/><item id="cover" href="cover.png" media-type="image/png"/><item id="notes" href="notes.txt" media-type="text/plain"/><item id="css" href="style.css" media-type="text/css"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="gone" href="missing.txt" media-type="text/plain"/><item id="font" href="fonts/a.otf" media-type="application/vnd.ms-opentype"/>"#,
            r#"<itemref idref="ch1"/><itemref idref="notes"/><itemref idref="ch2"/><itemref idref="unknown"/>"#,
        );
        let first = chapter(
            r#"Loose text<p epub:type="chapter" lang="en" class="c">Hver aften &amp; nat&#x21; <!-- skipped --><?pi skipped?></p><section xmlns:x="urn:x"><h1>Overskrift</h1><ul><li>en</li><li>to</li></ul><table><tr><td>a</td><td>b</td></tr></table></section><p>Efter</p>"#,
        );
        let second = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><div><p>Anden</p></div></body></html>"#;
        let documents = [
            scratch.write(
                "epub2.epub",
                &book(&[
                    MIMETYPE_ENTRY,
                    CONTAINER_ENTRY,
                    ("OEBPS/content.opf", &opf),
                    ("OEBPS/chapter1.xhtml", &first),
                    ("OEBPS/chapter 2 b.html", second),
                    ("OEBPS/cover.png", "not a picture"),
                    ("OEBPS/notes.txt", "Noter til fyret."),
                    ("OEBPS/style.css", "p {}"),
                    ("OEBPS/fonts/a.otf", "not a font"),
                    (
                        "metadata.xml",
                        r#"<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Andet</dc:title><dc:rights>Fri</dc:rights></metadata>"#,
                    ),
                ]),
            ),
            scratch.write(
                "epub3.epub",
                &book(&[
                    MIMETYPE_ENTRY,
                    CONTAINER_ENTRY,
                    (
                        "OEBPS/content.opf",
                        &package(
                            "3.0",
                            r#"<dc:title>Fast</dc:title><meta property="rendition:layout">pre-paginated</meta>"#,
                            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="b" href="b.html" media-type="text/html"/><item id="pic" href="pic.png" media-type="image/png" properties="cover-image"/>"#,
                            r#"<itemref idref="a" properties="rendition:layout-pre-paginated"/><itemref idref="b"/>"#,
                        ),
                    ),
                    ("OEBPS/a.xhtml", &chapter("<p>A</p>")),
                    ("OEBPS/b.html", second),
                    ("OEBPS/pic.png", "not a picture"),
                ]),
            ),
            scratch.write(
                "without-container.epub",
                &book(&[
                    MIMETYPE_ENTRY,
                    (
                        "book.OPF",
                        &package(
                            "2.0",
                            "<dc:title>Uden</dc:title>",
                            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>"#,
                            r#"<itemref idref="a"/>"#,
                        ),
                    ),
                    ("a.xhtml", &chapter("<p>Rod</p>")),
                ]),
            ),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    /// An element of a chapter under a prefix for XHTML is written well-formed, in XHTML, where
    /// Tika's path writes a prefixed start tag with an unprefixed end tag (#87).
    #[test]
    fn writes_xhtml_under_a_prefix_well_formed() {
        let opf = package(
            "2.0",
            "<dc:title>Prefiks</dc:title>",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>"#,
            r#"<itemref idref="a"/>"#,
        );
        let input = book(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            (
                "OEBPS/a.xhtml",
                &chapter(
                    r#"<p>Før</p><h:p xmlns:h="http://www.w3.org/1999/xhtml">Prefixed</h:p><p>Efter</p>"#,
                ),
            ),
        ]);
        let (xhtml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(&input)
            .unwrap();
        assert!(
            xhtml
                .contains("<p>Før</p>\n<p xmlns:h=\"http://www.w3.org/1999/xhtml\">Prefixed</p>\n<p>Efter</p>"),
            "{xhtml}"
        );
    }

    /// Elements outside XHTML in a chapter's body, such as an iBooks `svg`, are written by their
    /// local name with their text, neither indented nor followed by a line break.
    #[test]
    fn writes_elements_outside_xhtml() {
        let opf = package(
            "3.0",
            "",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>"#,
            r#"<itemref idref="a"/>"#,
        );
        let input = book(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            (
                "OEBPS/a.xhtml",
                &chapter(
                    r#"<object><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><title>Fyr</title><p>Ikke XHTML</p><li>Ej heller</li><image xlink:href="fyr.jpg"/></svg><p>Lys</p></object>"#,
                ),
            ),
        ]);
        let (xhtml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(&input)
            .unwrap();
        assert!(
            xhtml.contains(
                r#"<object><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><title>Fyr</title><p>Ikke XHTML</p><li>Ej heller</li><image href="fyr.jpg" /></svg><p>Lys</p>
</object>"#
            ),
            "{xhtml}"
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    fn closes_the_embedded_document_the_write_limit_stops_in() {
        let scratch = Scratch::new("epub-limit");
        let opf = package(
            "2.0",
            "<dc:title>Lang</dc:title>",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="notes" href="notes.txt" media-type="text/plain"/>"#,
            r#"<itemref idref="a"/>"#,
        );
        let notes = "Noter til fyret, som lyser hver nat. ".repeat(10);
        let document = scratch.write(
            "long-notes.epub",
            &book(&[
                MIMETYPE_ENTRY,
                CONTAINER_ENTRY,
                ("OEBPS/content.opf", &opf),
                ("OEBPS/a.xhtml", &chapter("<p>Kort</p>")),
                ("OEBPS/notes.txt", &notes),
            ]),
        );
        assert_same_as_tika(&[document.as_path()], TIKA_NAME);
    }

    #[cfg(feature = "tika")]
    #[test]
    fn fails_where_tikas_path_fails() {
        let scratch = Scratch::new("epub-failures");
        let manifest = r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/>"#;
        let spine = r#"<itemref idref="a"/><itemref idref="b"/>"#;
        let opf = package("2.0", "<dc:title>Fejl</dc:title>", manifest, spine);
        let documents = [
            scratch.write(
                "missing-chapter.epub",
                &book(&[
                    MIMETYPE_ENTRY,
                    CONTAINER_ENTRY,
                    ("OEBPS/content.opf", &opf),
                    ("OEBPS/a.xhtml", &chapter("<p>Only one</p>")),
                ]),
            ),
            scratch.write(
                "no-package.epub",
                &book(&[
                    MIMETYPE_ENTRY,
                    ("OEBPS/a.xhtml", &chapter("<p>Recovered</p>")),
                    ("OEBPS/b.xhtml", &chapter("<p>broken")),
                    ("OEBPS/c.html", &chapter("<p>Also recovered</p>")),
                ]),
            ),
            scratch.write(
                "empty-spine.epub",
                &book(&[
                    MIMETYPE_ENTRY,
                    CONTAINER_ENTRY,
                    (
                        "OEBPS/content.opf",
                        &package("2.0", "<dc:title>Tom</dc:title>", manifest, ""),
                    ),
                    ("OEBPS/a.xhtml", &chapter("<p>Recovered</p>")),
                ]),
            ),
            scratch.write(
                "malformed-package.epub",
                &book(&[
                    MIMETYPE_ENTRY,
                    CONTAINER_ENTRY,
                    ("OEBPS/content.opf", "<package><metadata>"),
                    ("OEBPS/a.xhtml", &chapter("<p>Never</p>")),
                ]),
            ),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&paths);
    }

    /// The XHTML and metadata the native path extracts from the book `entries`.
    fn extract(entries: &[(&str, &str)]) -> (String, crate::Metadata) {
        let input = book(entries);
        assert_eq!(deferral(&input), None);
        crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(&input)
            .unwrap()
    }

    #[test]
    fn writes_embedded_items_in_manifest_order() {
        // Tika writes them in the order of a `HashMap` of their ids (#88).
        let names = ["zeta", "alpha", "mid", "b", "a"];
        let manifest: String = names
            .iter()
            .map(|name| format!(r#"<item id="{name}" href="{name}.txt" media-type="text/plain"/>"#))
            .collect();
        let opf = package(
            "2.0",
            "",
            &format!(
                r#"<item id="c" href="c.xhtml" media-type="application/xhtml+xml"/>{manifest}"#
            ),
            r#"<itemref idref="c"/>"#,
        );
        let chapter = chapter("<p>C</p>");
        let texts: Vec<(String, String)> = names
            .iter()
            .map(|name| (format!("OEBPS/{name}.txt"), format!("Text of {name}.")))
            .collect();
        let mut entries = vec![
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", opf.as_str()),
            ("OEBPS/c.xhtml", chapter.as_str()),
        ];
        entries.extend(
            texts
                .iter()
                .map(|(name, text)| (name.as_str(), text.as_str())),
        );
        let (xhtml, _) = extract(&entries);
        let positions: Vec<usize> = names
            .iter()
            .map(|name| xhtml.find(&format!("Text of {name}.")).unwrap())
            .collect();
        assert!(positions.is_sorted(), "{xhtml}");
    }

    #[test]
    fn decodes_hrefs_as_uris_and_skips_only_a_broken_one() {
        // Tika decodes them as form data, `+` a space, and fails the book on a broken escape
        // (#88).
        let opf = package(
            "2.0",
            "",
            r#"<item id="a" href="a+b%20c.xhtml" media-type="application/xhtml+xml"/><item id="bad" href="a%2.xhtml" media-type="application/xhtml+xml"/><item id="signed" href="%+41.xhtml" media-type="application/xhtml+xml"/>"#,
            r#"<itemref idref="a"/><itemref idref="bad"/><itemref idref="signed"/>"#,
        );
        let (xhtml, _) = extract(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            ("OEBPS/a+b c.xhtml", &chapter("<p>Plus kept</p>")),
            ("OEBPS/a%2.xhtml", &chapter("<p>Broken</p>")),
            ("OEBPS/A.xhtml", &chapter("<p>Signed</p>")),
        ]);
        assert!(xhtml.contains("Plus kept"), "{xhtml}");
        assert!(!xhtml.contains("Broken"), "{xhtml}");
        assert!(!xhtml.contains("Signed"), "{xhtml}");
        assert_eq!(
            package::percent_decode("%C3%A6+x"),
            Some("\u{e6}+x".to_owned())
        );
        assert_eq!(package::percent_decode("%4"), None);
        assert_eq!(package::percent_decode("%+1"), None);
        assert_eq!(package::percent_decode("%C3"), None);
    }

    #[test]
    fn takes_the_first_usable_rootfile_matched_exactly() {
        // Tika takes the last `rootfile`, matched ignoring case, and one without a `full-path`
        // leaves the book without a package document (#88).
        let container = |rootfiles: &str| {
            format!(
                r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>{rootfiles}</rootfiles></container>"#
            )
        };
        let root = |rootfiles: &str| package::root(container(rootfiles).as_bytes()).unwrap();
        assert_eq!(
            root(r#"<rootfile/><rootfile full-path="a.opf"/><rootfile full-path="b.opf"/>"#),
            Some("a.opf".to_owned())
        );
        assert_eq!(root(r#"<ROOTFILE full-path="a.opf"/>"#), None);
        assert_eq!(root(r#"<rootfıle full-path="a.opf"/>"#), None);
    }

    #[test]
    fn matches_package_elements_exactly() {
        // Tika matches `itemRef`, `manifest` and the others ignoring case (#88): an `ITEM` is
        // not a manifest item.
        let opf = package(
            "2.0",
            "",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><ITEM id="b" href="b.xhtml" media-type="application/xhtml+xml"/>"#,
            r#"<itemref idref="a"/><itemref idref="b"/>"#,
        );
        let (xhtml, _) = extract(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            ("OEBPS/a.xhtml", &chapter("<p>Item</p>")),
            ("OEBPS/b.xhtml", &chapter("<p>Upper case</p>")),
        ]);
        assert!(xhtml.contains("Item"), "{xhtml}");
        assert!(!xhtml.contains("Upper case"), "{xhtml}");
    }

    #[test]
    fn reports_a_malformed_chapter_and_reads_the_rest() {
        // Tika fails the whole book on it, after writing everything else (#89).
        let opf = package(
            "2.0",
            "",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/>"#,
            r#"<itemref idref="a"/><itemref idref="b"/>"#,
        );
        let (xhtml, metadata) = extract(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            ("OEBPS/a.xhtml", &chapter("<p>open <b>bold</p>")),
            ("OEBPS/b.xhtml", &chapter("<p>After the failure</p>")),
        ]);
        assert!(xhtml.contains("open <b>bold</b>"), "{xhtml}");
        assert!(xhtml.contains("After the failure"), "{xhtml}");
        let warnings = &metadata[embedded::EXCEPTION_WARNING];
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("the content document OEBPS/a.xhtml is malformed"),
            "{warnings:?}"
        );
    }

    #[test]
    fn reads_content_documents_without_the_xhtml_namespace() {
        // Tika writes nothing of them (#89).
        let opf = package(
            "2.0",
            "",
            r#"<item id="a" href="a.html" media-type="text/html"/>"#,
            r#"<itemref idref="a"/>"#,
        );
        let (xhtml, _) = extract(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            ("OEBPS/a.html", "<html><body><p>Written</p></body></html>"),
        ]);
        assert!(xhtml.contains("<p>Written</p>"), "{xhtml}");
    }

    #[test]
    fn keeps_both_language_attributes() {
        // Tika keeps only the first of `xml:lang` and `lang` (#89).
        let opf = package(
            "2.0",
            "",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>"#,
            r#"<itemref idref="a"/>"#,
        );
        let (xhtml, _) = extract(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            (
                "OEBPS/a.xhtml",
                &chapter(r#"<p xml:lang="da" lang="en" epub:type="x">Sprog</p>"#),
            ),
        ]);
        assert!(
            xhtml.contains(r#"<p xml:lang="da" lang="en" type="x">Sprog</p>"#),
            "{xhtml}"
        );
    }

    #[test]
    fn names_dates_by_their_event() {
        // Tika makes every `dc:date` `dcterms:created` (#89).
        let metadata = |dates: &str| {
            let opf = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?><package xmlns="http://www.idpf.org/2007/opf" xmlns:opf="http://www.idpf.org/2007/opf" version="2.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">{dates}</metadata><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="a"/></spine></package>"#
            );
            extract(&[
                MIMETYPE_ENTRY,
                CONTAINER_ENTRY,
                ("OEBPS/content.opf", &opf),
                ("OEBPS/a.xhtml", &chapter("<p>a</p>")),
            ])
            .1
        };
        let dates = metadata(
            r#"<dc:date opf:event="creation">2001</dc:date><dc:date opf:event="publication">2002</dc:date><dc:date opf:event="modification">2003</dc:date>"#,
        );
        assert_eq!(dates["dcterms:created"], ["2001"]);
        assert_eq!(dates["dcterms:issued"], ["2002"]);
        assert_eq!(dates["dcterms:modified"], ["2003"]);
        let dates = metadata(r#"<dc:date> 2004 </dc:date>"#);
        assert_eq!(dates["dcterms:created"], ["2004"]);
        assert!(!dates.contains_key("dcterms:issued"));
    }

    #[test]
    fn leaves_encrypted_books_to_tika() {
        let opf = package(
            "3.0",
            "",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>"#,
            r#"<itemref idref="a"/>"#,
        );
        let with = |chapter_body: &str, extra: &[(&str, &str)]| {
            let chapter = chapter(chapter_body);
            let mut entries = vec![
                MIMETYPE_ENTRY,
                CONTAINER_ENTRY,
                ("OEBPS/content.opf", opf.as_str()),
                ("OEBPS/a.xhtml", chapter.as_str()),
            ];
            entries.extend_from_slice(extra);
            book(&entries)
        };
        let encrypted = with(
            "<p>x</p>",
            &[(
                ENCRYPTION,
                r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData><enc:CipherData><enc:CipherReference URI="OEBPS/a.xhtml"/></enc:CipherData></enc:EncryptedData></encryption>"#,
            )],
        );
        assert_eq!(
            deferral(&encrypted).as_deref(),
            Some("its encryption.xml names encrypted items")
        );
        assert_eq!(deferral(&with("<p>x</p>", &[])), None);
        assert_eq!(
            deferral(&with(
                r#"<h:p xmlns:h="http://www.w3.org/1999/xhtml">x</h:p>"#,
                &[]
            )),
            None
        );
        assert_eq!(
            deferral(&with(
                r#"<p>x</p>"#,
                &[(
                    ENCRYPTION,
                    r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"/>"#
                )]
            )),
            None
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore]
    fn extracts_what_tikas_path_extracts_from_tikas_documents() {
        let corpus = conformance_corpus();
        let (parsed, deferred) = documents(&EpubParser, &[&corpus]);
        assert!(deferred.is_empty(), "{deferred:?}");
        // Their pictures outside the spine come in manifest order, which Tika's `HashMap` of
        // their ids does not keep (#88). The iBooks book's are each read by OCR, so its lines
        // are compared in any order; the pictures of the other share lines.
        let (reordered, parsed): (Vec<&Path>, Vec<&Path>) = parsed
            .iter()
            .map(PathBuf::as_path)
            .partition(|path| IN_HASH_ORDER.iter().any(|name| path.ends_with(name)));
        assert_same_as_tika_rewriting(&parsed, TIKA_NAME, as_tika_writes_it);
        let [reordered, ibooks] = IN_HASH_ORDER.map(|name| {
            *reordered
                .iter()
                .find(|path| path.ends_with(name))
                .unwrap_or_else(|| panic!("{name} among {reordered:?}"))
        });
        assert_same_as_tika_departing_rewriting(
            &[ibooks],
            TIKA_NAME,
            MANIFEST_ORDER,
            Some((as_tika_writes_it, None)),
        );
        let (xml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_file_to_string(reordered.to_str().unwrap())
            .unwrap();
        let heading = |name: &str| xml.find(&format!("<h1>{name}</h1>")).unwrap();
        assert!(heading("epub/images/logo.png") < heading("epub/images/titlepage.png"));
    }
}
