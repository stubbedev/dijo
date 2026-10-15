//! EPUB books (and iBooks), as Tika's `EpubParser` reads them under extractous's substitution of
//! which manifest items it parses as embedded documents: the package document's Dublin Core
//! metadata, layout and version, the `mimetype` and `metadata.xml`, then each XHTML content
//! document of the spine, read as XML and written body by body, and then, in the order of Java's
//! `HashMap` of manifest ids, the items outside the spine our `EpubEmbedded` admits, such as
//! pictures and fonts, each in an `embedded` div. Without a package document, or with an empty
//! spine, Tika writes every HTML and XML entry it can and fails. Encrypted books, and books with
//! elements outside XHTML in a content document's body, are left to Tika.

mod content;
mod package;

use std::collections::{HashMap, HashSet};

use crate::config::ParserConfigs;
use crate::formats::embedded::{self, Wrapping};
use crate::formats::handlers::is_blank;
use crate::formats::java;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml;
use crate::formats::zip_file::{ZipEntry, ZipFile};
use crate::formats::{Deferral, Failure, Parser, Prepared};

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
    /// The entries read ahead to check them, by name, each to be read once more.
    read_ahead: HashMap<String, Vec<u8>>,
}

impl<'a> Book<'a> {
    fn named(&self, name: &str) -> Option<ZipEntry> {
        self.zip.named(name).cloned()
    }

    fn read(&mut self, entry: &ZipEntry) -> Result<Vec<u8>, Failure> {
        match self.read_ahead.remove(&entry.name) {
            Some(bytes) => Ok(bytes),
            None => self.zip.read(entry),
        }
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
    match xml::parse_sax(&bytes, &entry.name, &mut document) {
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
        metadata.set(embedded::RESOURCE_TYPE, embedded::THUMBNAIL);
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
    let font = item
        .media_type
        .as_deref()
        .is_some_and(|media| media.contains("font"))
        || item.href.starts_with("fonts");
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

/// `bufferedParseZipFile` along the package document.
fn read_package(
    book: &mut Book,
    root: &ZipEntry,
    contents: &Contents,
    xhtml: &mut Xhtml,
) -> Result<(), Thrown> {
    let relative = relative_path(&root.name).to_owned();
    book.extract_metadata(xhtml.metadata_mut())?;
    let mut processed: HashSet<&str> = HashSet::new();
    let mut malformed = None;
    let mut missing = 0;
    for id in &contents.spine {
        let Some(item) = contents.items.get(id) else {
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
            malformed.get_or_insert(failure);
        }
    }
    let cover = contents.cover();
    for (id, item) in contents.items.iter() {
        if processed.contains(id) || !parses_as_embedded(item.media_type.as_deref()) {
            continue;
        }
        write_embedded(book, &relative, item, cover == Some(id), xhtml)?;
    }
    if let Some(failure) = malformed {
        return Err(Thrown::Other(failure));
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
/// its package document, with the metadata it adds to `metadata`, and its content documents,
/// which it keeps for writing; or why Tika's path should read it.
fn prepare<'a>(
    input: &'a [u8],
    metadata: &mut TikaMetadata,
) -> Result<(Book<'a>, Result<Reading, Failure>), Deferral> {
    let Ok(zip) = ZipFile::open(input, "EPUB") else {
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
    let mut book = Book {
        zip,
        read_ahead: HashMap::new(),
    };
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
                .filter_map(|id| contents.items.get(id))
                .filter(|item| is_content_document(item))
                .filter_map(|item| book.named(&format!("{relative}{}", item.href)))
                .collect()
        }
    };
    for document in &documents {
        let Ok(bytes) = book.read(document) else {
            return Err(format!("its entry {} cannot be read", document.name));
        };
        if content::holds_foreign_elements(&bytes, &document.name) {
            return Err(format!(
                "its content document {} has elements outside XHTML in its body",
                document.name
            ));
        }
        book.read_ahead.insert(document.name.clone(), bytes);
    }
    Ok((book, reading))
}

pub(crate) struct EpubParser;

impl Parser for EpubParser {
    fn name(&self) -> &'static str {
        "extractous::EpubParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/epub+zip", "application/x-ibooks+zip"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Deferral> {
        let mut metadata = metadata.clone();
        let (mut book, reading) = prepare(input, &mut metadata)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn deferral(input: &[u8]) -> Option<String> {
        EpubParser.defers(input, &TikaMetadata::new(), &ParserConfigs::default())
    }
    use crate::formats::parity::{
        assert_fails_as_tika, assert_same_as_tika, conformance_corpus, documents, Scratch,
    };
    use std::io::{Cursor, Write};
    use std::path::{Path, PathBuf};

    const TIKA_NAME: &str = "org.apache.tika.parser.epub.EpubParser";
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

    #[test]
    fn extracts_what_tikas_path_extracts() {
        let documents: Vec<&Path> = DOCUMENTS.iter().map(Path::new).collect();
        assert_same_as_tika(&documents, TIKA_NAME);
    }

    #[test]
    fn extracts_what_tikas_path_extracts_from_every_kind_of_book() {
        let scratch = Scratch::new("epub-books");
        let opf = package(
            "2.0",
            r#"<dc:title>Fyret</dc:title><dc:creator>Anna</dc:creator><dc:creator>Bo</dc:creator><dc:subject>Lys</dc:subject><dc:date>2026-01-02</dc:date><dc:language>da</dc:language><meta name="cover" content=" cover "/>"#,
            r#"<item id="ch1" href="chapter1.xhtml" media-type="application/xhtml+xml"/><item id="ch2" href="chapter%202+b.html"/><item id="cover" href="cover.png" media-type="image/png"/><item id="notes" href="notes.txt" media-type="text/plain"/><item id="css" href="style.css" media-type="text/css"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="gone" href="missing.txt" media-type="text/plain"/><item id="font" href="fonts/a.otf" media-type="application/vnd.ms-opentype"/>"#,
            r#"<itemref idref="ch1"/><itemref idref="notes"/><itemref idref="ch2"/><itemref idref="unknown"/>"#,
        );
        let first = chapter(
            r#"Loose text<p epub:type="chapter" xml:lang="da" lang="en" class="c">Hver aften &amp; nat&#x21; <!-- skipped --><?pi skipped?></p><section xmlns:x="urn:x"><h1>Overskrift</h1><ul><li>en</li><li>to</li></ul><table><tr><td>a</td><td>b</td></tr></table></section><h:p xmlns:h="http://www.w3.org/1999/xhtml">Prefixed</h:p><p>Efter</p>"#,
        );
        let second = r#"<html xmlns="http://www.w3.org/1999/xhtml"><head/><body><div><p>Anden</p></div></body></html>"#;
        let unnamespaced = r#"<html><body><p>Never written</p></body></html>"#;
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
                    ("OEBPS/b.html", unnamespaced),
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

    #[test]
    fn fails_where_tikas_path_fails() {
        let scratch = Scratch::new("epub-failures");
        let manifest = r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/>"#;
        let spine = r#"<itemref idref="a"/><itemref idref="b"/>"#;
        let opf = package("2.0", "<dc:title>Fejl</dc:title>", manifest, spine);
        let documents = [
            scratch.write(
                "malformed-chapter.epub",
                &book(&[
                    MIMETYPE_ENTRY,
                    CONTAINER_ENTRY,
                    ("OEBPS/content.opf", &opf),
                    ("OEBPS/a.xhtml", &chapter("<p>open <b>bold</p>")),
                    ("OEBPS/b.xhtml", &chapter("<p>After the failure</p>")),
                ]),
            ),
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
            scratch.write(
                "bad-href.epub",
                &book(&[
                    MIMETYPE_ENTRY,
                    CONTAINER_ENTRY,
                    (
                        "OEBPS/content.opf",
                        &package(
                            "2.0",
                            "",
                            r#"<item id="a" href="a%2.xhtml" media-type="application/xhtml+xml"/>"#,
                            r#"<itemref idref="a"/>"#,
                        ),
                    ),
                ]),
            ),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_fails_as_tika(&paths);
    }

    #[test]
    fn leaves_encrypted_books_and_foreign_elements_to_tika() {
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
        let svg = with(
            r#"<p>x</p><svg xmlns="http://www.w3.org/2000/svg"><title>t</title></svg>"#,
            &[],
        );
        assert_eq!(
            deferral(&svg).as_deref(),
            Some("its content document OEBPS/a.xhtml has elements outside XHTML in its body")
        );
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

    #[test]
    #[ignore]
    fn extracts_what_tikas_path_extracts_from_tikas_documents() {
        let corpus = conformance_corpus();
        let (parsed, deferred) = documents(&EpubParser, &[&corpus]);
        let names: Vec<String> = deferred
            .iter()
            .map(|file| file.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["testiBooks.ibooks"]);
        let parsed: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&parsed, TIKA_NAME);
    }
}
