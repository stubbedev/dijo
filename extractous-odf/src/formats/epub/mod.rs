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

use crate::compress::zip::file::{ZipEntry, ZipFile};
use crate::config::ParserConfigs;
use crate::formats::crypt;
use crate::formats::embedded::{self, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::xml;
use crate::formats::zip_package::{Opened, Undecoded, Unread, ZipPackage};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages};
use crate::java;
use crate::java::is_blank;

use content::ContentDocument;
use package::{Contents, Item, MetadataParser};

const TIKA_NAME: &str = "org.apache.tika.parser.epub.EpubParser";
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

/// A failure as Tika's `EpubParser.parse` sees it: one after which it still ends the document,
/// an `IOException` or the `EncryptedDocumentException` it throws once it has, or any other.
enum Thrown {
    Ending(Failure),
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

/// An opened book: the archive Tika reads of it, and the first entry read that the port does not
/// decode as Tika does.
struct Book<'p> {
    package: &'p ZipPackage<'p>,
    zip: ZipFile<'p>,
    undecoded: Option<(String, Undecoded)>,
}

impl<'p> Book<'p> {
    fn new(package: &'p ZipPackage<'p>) -> Self {
        Self {
            package,
            zip: package.zip(),
            undecoded: None,
        }
    }

    /// `getEntry`: the first entry of `name`.
    fn named(&self, name: &str) -> Option<ZipEntry> {
        self.zip.entry(name).cloned()
    }

    /// The first entry of `name` that `canReadEntryData` lets Tika read.
    fn readable(&self, name: &str) -> Option<ZipEntry> {
        self.named(name).filter(ZipEntry::can_read)
    }

    /// The data of `entry`, which Tika's parser reads `opened` so.
    fn read(&mut self, entry: &ZipEntry, opened: Opened) -> Result<Input<'p>, Failure> {
        let read = self.package.spool(&self.zip, entry);
        self.noted(read, entry, opened)
    }

    /// Reads `entry`, which Tika's parser reads `opened` so, ahead of its writing.
    fn read_ahead(&mut self, entry: &ZipEntry, opened: Opened) -> Result<(), Failure> {
        let read = self.package.read_ahead(&self.zip, entry);
        self.noted(read, entry, opened)
    }

    /// What reading `entry`, `opened` so, gave, noting the first entry the port does not
    /// decode as Tika does.
    fn noted<T>(
        &mut self,
        read: Result<T, Unread>,
        entry: &ZipEntry,
        opened: Opened,
    ) -> Result<T, Failure> {
        read.map_err(|unread| {
            if let (None, Unread::Undecoded(undecoded)) = (&self.undecoded, &unread) {
                self.undecoded = Some((entry.name.clone(), *undecoded));
            }
            unread.failure(&entry.name, opened, TIKA_NAME)
        })
    }

    /// `getRoot`: the package document `container.xml` names, or without one, the first entry
    /// Tika can read whose name ends in `.opf`.
    fn root(&mut self) -> Result<Option<String>, Failure> {
        match self.named(CONTAINER) {
            Some(container) => package::root(&self.read(&container, Opened::Directly)?),
            None => Ok(self
                .zip
                .entries()
                .iter()
                .find(|entry| entry.name.to_lowercase().ends_with(".opf") && entry.can_read())
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
        let Some(root) = self.readable(&root) else {
            return Ok(Reading::Fallback(
                "OPF entry missing or unreadable in (possibly truncated) container",
            ));
        };
        let document = self.read(&root, Opened::Tagged)?;
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

    /// `extractMetadata`: the media type the `mimetype` entry names, and `metadata.xml`, each
    /// where Tika can read it.
    fn extract_metadata(&mut self, metadata: &mut TikaMetadata) -> Result<(), Failure> {
        if let Some(mimetype) = self.readable(MIMETYPE) {
            let input = self.read(&mimetype, Opened::Directly)?;
            metadata.set("Content-Type", java::trim(&String::from_utf8_lossy(&input)));
        }
        if let Some(entry) = self.readable(METADATA) {
            let input = self.read(&entry, Opened::Tagged)?;
            package::read_metadata(&input, METADATA, MetadataParser::DublinCore, metadata)?;
        }
        Ok(())
    }

    /// `checkForDRM`: the items `encryption.xml` names, if there is one.
    fn encrypted_items(&mut self) -> Result<Vec<String>, Failure> {
        match self.named(ENCRYPTION) {
            Some(encryption) => {
                package::encrypted_items(&self.read(&encryption, Opened::Directly)?)
            }
            None => Ok(Vec::new()),
        }
    }

    /// The entries `fallbackParseAllHtmlEntries` parses, in the order of the central directory.
    fn fallback_documents(&self) -> Vec<ZipEntry> {
        self.zip
            .entries()
            .iter()
            .filter(|entry| is_fallback_document(&entry.name) && entry.can_read())
            .cloned()
            .collect()
    }
}

/// `maybeThrowEncryptedException`: the `EncryptedDocumentException` naming `items`, in the order
/// of the `HashSet` Tika collects them in, as many as fit in about 500 characters.
fn encrypted(items: &[String]) -> Failure {
    let mut message = String::from("EPUB contains encrypted items: ");
    for (added, item) in java::hash_set_order(items.iter().map(String::as_str))
        .into_iter()
        .enumerate()
    {
        if message.encode_utf16().count() > 500 {
            message.push_str(" and others...");
            break;
        }
        if added > 0 {
            message.push_str(", ");
        }
        message.push_str(item);
    }
    Failure::Extraction(crypt::encrypted_document(&message))
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
    let input = book.read(entry, Opened::Tagged).map_err(Thrown::Ending)?;
    let mut document = ContentDocument::new(xhtml);
    match xml::parse_part(&input, &entry.name, &mut document) {
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

/// The manifest items outside the spine, but those `processed`, that Tika hands to its
/// `handleEmbedded`, in manifest order, with whether each is the cover: those our `EpubEmbedded`
/// admits, unless `encryption.xml` names them.
fn embedded_items<'c>(
    contents: &'c Contents,
    relative: &str,
    processed: &HashSet<&str>,
    encrypted: &[String],
) -> Vec<(&'c Item, bool)> {
    let cover = contents.cover();
    contents
        .items
        .iter()
        .filter(|(id, item)| {
            !processed.contains(id.as_str())
                && !encrypted.contains(&format!("{relative}{}", item.href))
                && parses_as_embedded(item.media_type.as_deref())
        })
        .map(|(id, item)| (item, cover == Some(id.as_str())))
        .collect()
}

/// The entry of the manifest item `item` that `handleEmbedded` parses, with the metadata it
/// gives it: one Tika can read, which the embedded document extractor parses.
fn embedded_entry(
    book: &Book,
    relative: &str,
    item: &Item,
    cover: bool,
) -> Option<(ZipEntry, TikaMetadata)> {
    let path = format!("{relative}{}", item.href);
    let entry = book.readable(&path)?;
    let metadata = embedded_metadata(item, &path, cover);
    embedded::is_parsed(&metadata).then_some((entry, metadata))
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
    let Some((entry, metadata)) = embedded_entry(book, relative, item, cover) else {
        return Ok(());
    };
    let input = book.read(&entry, Opened::Tagged).map_err(Thrown::Ending)?;
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
    xhtml.parse_embedded_input(
        &input,
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

/// The content documents of the spine Tika parses, in spine order, with their ids: those with
/// an entry, or `None` for one without.
fn spine_documents<'c>(
    book: &Book,
    relative: &str,
    contents: &'c Contents,
) -> Vec<(&'c str, String, Option<ZipEntry>)> {
    contents
        .spine
        .iter()
        .filter_map(|id| Some((id.as_str(), contents.item(id)?)))
        .filter(|(_, item)| is_content_document(item))
        .map(|(id, item)| {
            let path = format!("{relative}{}", item.href);
            let entry = book.named(&path);
            (id, path, entry)
        })
        .collect()
}

/// `bufferedParseZipFile` along the package document. A malformed content document is
/// reported as a warning and skipped: Tika fails the whole book on it, after writing everything
/// else (#89). Items `encryption.xml` names fail the book, as soon as one is in the spine,
/// else once the document has ended.
fn read_package(
    book: &mut Book,
    root: &ZipEntry,
    contents: &Contents,
    xhtml: &mut Xhtml,
) -> Result<(), Thrown> {
    let relative = relative_path(&root.name).to_owned();
    book.extract_metadata(xhtml.metadata_mut())?;
    let encrypted_items = book.encrypted_items()?;
    let mut processed: HashSet<&str> = HashSet::new();
    let mut missing = 0;
    for (id, path, entry) in spine_documents(book, &relative, contents) {
        if encrypted_items.contains(&path) {
            return Err(Thrown::Other(encrypted(&encrypted_items)));
        }
        let Some(entry) = entry else {
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
    for (item, cover) in embedded_items(contents, &relative, &processed, &encrypted_items) {
        write_embedded(book, &relative, item, cover, xhtml)?;
    }
    if missing > 0 {
        return Err(Thrown::Ending(Failure::Malformed(format!(
            "EPUB: {missing} of {} spine items missing from (possibly truncated) container",
            contents.spine.len()
        ))));
    }
    if !encrypted_items.is_empty() {
        return Err(Thrown::Ending(encrypted(&encrypted_items)));
    }
    Ok(())
}

/// `fallbackParseAllHtmlEntries`: the metadata as far as Tika reads it, failing or not, every HTML
/// and XML entry, a malformed or unreadable one skipped, and then a failure.
fn read_fallback(book: &mut Book, reason: &str, xhtml: &mut Xhtml) -> Result<(), Thrown> {
    match book.extract_metadata(xhtml.metadata_mut()) {
        Ok(()) | Err(_) => {}
    }
    for entry in &book.fallback_documents() {
        match write_content_document(book, entry, xhtml) {
            Ok(_) | Err(Thrown::Ending(_)) => {}
            Err(stopped @ Thrown::Other(_)) => return Err(stopped),
        }
    }
    Err(Thrown::Ending(Failure::Malformed(format!(
        "EPUB: fallback recovery ({reason})"
    ))))
}

/// Reads each entry the writing of `book` along `reading` reads, whose failures fail the writing
/// too, for the first one the port does not decode as Tika does. Tika moves on from an entry
/// of the fallback it cannot read, as the writing does, unless it is past what the port decodes.
fn check(book: &mut Book, reading: &Reading) {
    match reading {
        Reading::Fallback(_) => {
            for document in book.fallback_documents() {
                if let Err(Unread::Undecoded(oversized @ Undecoded::Oversized { .. })) =
                    book.package.read_ahead(&book.zip, &document)
                {
                    book.undecoded = Some((document.name.clone(), oversized));
                    return;
                }
            }
        }
        Reading::Package { root, contents } => {
            let relative = relative_path(&root.name);
            let read = book.extract_metadata(&mut TikaMetadata::new());
            let encrypted_items = book.encrypted_items();
            let (Ok(()), Ok(encrypted_items)) = (read, encrypted_items) else {
                return;
            };
            let mut processed = HashSet::new();
            for (id, _, entry) in spine_documents(book, relative, contents) {
                if let Some(entry) = entry {
                    processed.insert(id);
                    if book.read_ahead(&entry, Opened::Tagged).is_err() {
                        return;
                    }
                }
            }
            for (item, cover) in embedded_items(contents, relative, &processed, &encrypted_items) {
                if let Some((entry, _)) = embedded_entry(book, relative, item, cover) {
                    if book.read_ahead(&entry, Opened::Tagged).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// Reads the book in `input`, whose detection left `metadata`, as far as deciding how it is
/// written: the archive Tika reads of it, how it is read, along its package document, with the
/// metadata that adds, and the first entry its writing reads that the port does not decode as
/// Tika does, which makes the writing a reading that differs from Tika's. Each entry is read
/// again as it is written, so that no more than one is held in memory at a time, but for those
/// the check spooled to files, which the writing takes as they are.
fn prepare<'a>(
    input: &'a [u8],
    pages: Pages<'a>,
    mut metadata: TikaMetadata,
) -> Result<Prepared<'a>, Decline<'a>> {
    let package = ZipPackage::open(input, pages, &metadata, TIKA_NAME)?;
    let mut book = Book::new(&package);
    let reading = book.reading(&mut metadata);
    if let Ok(reading) = &reading {
        check(&mut book, reading);
    }
    let undecoded = book.undecoded.take();
    drop(book);
    package.release();
    let prepared: Prepared<'a> = Box::new(move |xhtml| {
        *xhtml.metadata_mut() = metadata;
        write(&mut Book::new(&package), reading, xhtml)
    });
    match undecoded {
        None => Ok(prepared),
        Some((name, undecoded)) => Err(undecoded.decline(&name, prepared)),
    }
}

pub struct EpubParser;

impl Parser for EpubParser {
    fn name(&self) -> &'static str {
        "extractous::EpubParser"
    }
    fn tika_name(&self) -> &'static str {
        TIKA_NAME
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
        prepare(input, pages, metadata.clone())
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
        Err(Thrown::Ending(failure)) => {
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
    use crate::formats::zip_package::tests::{replace_all, with_method};

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
        let root = |rootfiles: &str| {
            package::root(&Input::Borrowed(container(rootfiles).as_bytes())).unwrap()
        };
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

    /// A book of two chapters and a font, whose `encryption.xml` names `uris`.
    fn encrypted_book(uris: &[&str]) -> Vec<u8> {
        let opf = package(
            "3.0",
            "<dc:title>Låst</dc:title>",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/><item id="f" href="fonts/f.otf" media-type="application/vnd.ms-opentype"/>"#,
            r#"<itemref idref="a"/><itemref idref="b"/>"#,
        );
        let references: String = uris
            .iter()
            .map(|uri| {
                format!(
                    r#"<enc:EncryptedData><enc:CipherData><enc:CipherReference URI="{uri}"/></enc:CipherData></enc:EncryptedData>"#
                )
            })
            .collect();
        let encryption = format!(
            r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#">{references}</encryption>"#
        );
        book(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            ("OEBPS/a.xhtml", &chapter("<p>Synlig</p>")),
            ("OEBPS/b.xhtml", &chapter("<p>Også synlig</p>")),
            ("OEBPS/fonts/f.otf", "not a font"),
            (ENCRYPTION, &encryption),
        ])
    }

    #[test]
    fn fails_books_with_encrypted_items_as_tika_does() {
        let error = |uris: &[&str]| {
            crate::Extractor::new()
                .extract_bytes_to_string(&encrypted_book(uris))
                .unwrap_err()
                .to_string()
        };
        let thrown =
            "Parse error occurred : org.apache.tika.exception.EncryptedDocumentException: \
                      EPUB contains encrypted items: ";
        assert_eq!(
            error(&["OEBPS/fonts/f.otf"]),
            format!("{thrown}OEBPS/fonts/f.otf")
        );
        assert_eq!(error(&["OEBPS/b.xhtml"]), format!("{thrown}OEBPS/b.xhtml"));
        let many: Vec<String> = (0..40).map(|n| format!("OEBPS/fonts/{n:02}.otf")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(error(&many).ends_with(" and others..."));
        let (text, _) = crate::Extractor::new()
            .extract_bytes_to_string(&encrypted_book(&[]))
            .unwrap();
        assert!(text.contains("Også synlig"), "{text}");
    }

    /// A book with two entries of the name of its first chapter, and of its `mimetype`.
    fn duplicated_book() -> Vec<u8> {
        let opf = package(
            "2.0",
            "<dc:title>Dobbelt</dc:title>",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>"#,
            r#"<itemref idref="a"/>"#,
        );
        let duplicated = book(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            ("OEBPS/a.xhtml", &chapter("<p>Første</p>")),
            ("OEBPS/a.xhtmm", &chapter("<p>Anden</p>")),
            ("mimetypf", "application/x-other\n"),
        ]);
        replace_all(
            &replace_all(&duplicated, b"a.xhtmm", b"a.xhtml"),
            b"mimetypf",
            b"mimetype",
        )
    }

    /// A book whose central directory is cut off, which Tika reads as `ZipSalvager` rebuilds it.
    fn cut_book() -> Vec<u8> {
        let opf = package(
            "2.0",
            "<dc:title>Afkortet</dc:title>",
            r#"<item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>"#,
            r#"<itemref idref="a"/>"#,
        );
        let whole = book(&[
            MIMETYPE_ENTRY,
            CONTAINER_ENTRY,
            ("OEBPS/content.opf", &opf),
            ("OEBPS/a.xhtml", &chapter("<p>Bevaret</p>")),
        ]);
        whole[..whole.len() - 10].to_vec()
    }

    /// A book whose chapter says it is compressed with LZMA, which commons-compress does not
    /// decompress.
    fn unsupported_book() -> Vec<u8> {
        with_method(encrypted_book(&[]), "OEBPS/b.xhtml", 14)
    }

    #[test]
    fn reads_duplicated_and_cut_books_as_tika_does() {
        let read = |input: &[u8]| {
            crate::Extractor::new()
                .extract_bytes_to_string(input)
                .unwrap()
        };
        let (text, metadata) = read(&duplicated_book());
        assert!(text.contains("Første") && !text.contains("Anden"), "{text}");
        assert_eq!(metadata["Content-Type"], ["application/epub+zip"]);
        let (text, metadata) = read(&cut_book());
        assert!(text.contains("Bevaret"), "{text}");
        assert_eq!(metadata["zip:salvaged"], ["true"]);
    }

    #[test]
    fn fails_a_chapter_commons_compress_does_not_decompress_as_tika_does() {
        let error = crate::Extractor::new()
            .extract_bytes_to_string(&unsupported_book())
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Parse error occurred : org.apache.tika.exception.TikaException: TIKA-198: Illegal \
             IOException from org.apache.tika.parser.epub.EpubParser <- caused by \
             org.apache.commons.io.TaggedIOException: Unsupported compression method 14 (LZMA) \
             used in entry OEBPS/b.xhtml <- caused by \
             org.apache.commons.compress.archivers.zip.UnsupportedZipFeatureException: \
             Unsupported compression method 14 (LZMA) used in entry OEBPS/b.xhtml"
        );
    }

    #[cfg(feature = "tika")]
    #[test]
    fn extracts_what_tikas_path_extracts_from_damaged_books() {
        let scratch = Scratch::new("epub-damaged");
        let documents = [
            scratch.write("duplicated.epub", &duplicated_book()),
            scratch.write("cut.epub", &cut_book()),
        ];
        let paths: Vec<&Path> = documents.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika(&paths, TIKA_NAME);
    }

    /// Tika's `CompositeParser` names the parser it wraps an `IOException` for by the decorator
    /// around `EpubParser`, `MimeFilteringDecorator@<hash>`, which differs on each run, where the
    /// native path names `EpubParser`: what each wraps is compared.
    #[cfg(feature = "tika")]
    #[test]
    fn fails_where_tikas_path_fails_on_damaged_and_encrypted_books() {
        let scratch = Scratch::new("epub-damaged-failing");
        let many: Vec<String> = (0..40).map(|n| format!("OEBPS/fonts/{n:02}.otf")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let encrypted = [
            scratch.write("font.epub", &encrypted_book(&["OEBPS/fonts/f.otf"])),
            scratch.write(
                "chapter.epub",
                &encrypted_book(&["OEBPS/fonts/f.otf", "OEBPS/b.xhtml", "OEBPS/a.xhtml"]),
            ),
            scratch.write("many.epub", &encrypted_book(&many)),
        ];
        let unsupported = scratch.write("unsupported.epub", &unsupported_book());
        let mut documents: Vec<&Path> = encrypted.iter().map(PathBuf::as_path).collect();
        documents.push(&unsupported);
        assert_fails_as_tika(&documents);
        let configs = ParserConfigs::default();
        for path in &encrypted {
            let (native, tika) = crate::formats::parity::errors(path, &configs);
            assert_eq!(native, tika, "{}", path.display());
        }
        let cause = |error: &str| {
            error
                .split_once(" <- caused by ")
                .map(|(_, cause)| cause.to_owned())
        };
        let (native, tika) = crate::formats::parity::errors(&unsupported, &configs);
        assert!(cause(&native).is_some(), "{native}");
        assert_eq!(cause(&native), cause(&tika));
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
