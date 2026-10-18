//! Tika's `ZipParser` over an archive commons-compress's `ZipFile` opens: the integrity check of
//! the local headers against the central directory, then each entry but a directory, in central
//! directory order, handed to the embedded document extractor with the metadata Tika gives it,
//! an encrypted or unreadable one recorded as a failure instead, its name decoded as UTF-8 when
//! flagged so, else in the charset Tika detects for it.
//!
//! An archive `ZipFile` cannot open, which has no end of central directory record, is read front
//! to back by its local headers, as Tika's `parseWithStream` reads it, the first segment of a
//! split archive included. Where an entry's data is cut short, as the last entry of such a
//! segment or of a truncated archive is, Tika's stream fails the whole document; the native
//! parser records the entry as unreadable and keeps the entries before it.

use std::collections::HashSet;

use super::nesting::Scan;
use super::{record_unreadable, too_large, write_entry_div, Cut, DEFERS};
use crate::compress::zip::extra::Instant;
use crate::compress::zip::file::{self as zip_file, NameSource, ZipEntry, ZipFile};
use crate::compress::zip::salvage::{salvage, Unsalvaged};
use crate::compress::zip::stream::{self, ZipStream};
use crate::compress::zip::Content;
use crate::config::ParserConfigs;
use crate::formats::embedded::{
    record_exception, record_stream_exception, Wrapping, ENCRYPTED_DOCUMENT, INTERNAL_PATH,
    RESOURCE_NAME,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Difference, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
use crate::java::time::instant_string;
use crate::mime::{Key, MediaType};
use crate::text::charset;

const ZIP: &str = "application/zip";
const JAR: &str = "application/java-archive";
const MAX_INTEGRITY_CHECK_ENTRIES: usize = 100;

/// `ZipParser.ZIP_SPECIALIZATIONS`: the types of ZIP archive the parser keeps as detected.
const ZIP_SPECIALIZATIONS: [&str; 80] = [
    "application/bizagi-modeler",
    "application/epub+zip",
    "application/hwp+zip",
    "application/java-archive",
    "application/vnd.adobe.air-application-installer-package+zip",
    "application/vnd.android.package-archive",
    "application/vnd.apple.iwork",
    "application/vnd.apple.keynote",
    "application/vnd.apple.numbers",
    "application/vnd.apple.pages",
    "application/vnd.apple.unknown.13",
    "application/vnd.etsi.asic-e+zip",
    "application/vnd.etsi.asic-s+zip",
    "application/vnd.geogebra.file",
    "application/vnd.geogebra.slides",
    "application/vnd.geogebra.tool",
    "application/vnd.google-earth.kmz",
    "application/vnd.mindjet.mindmanager",
    "application/vnd.ms-excel.addin.macroenabled.12",
    "application/vnd.ms-excel.sheet.binary.macroenabled.12",
    "application/vnd.ms-excel.sheet.macroenabled.12",
    "application/vnd.ms-excel.template.macroenabled.12",
    "application/vnd.ms-powerpoint.addin.macroenabled.12",
    "application/vnd.ms-powerpoint.presentation.macroenabled.12",
    "application/vnd.ms-powerpoint.slide.macroenabled.12",
    "application/vnd.ms-powerpoint.slideshow.macroenabled.12",
    "application/vnd.ms-powerpoint.template.macroenabled.12",
    "application/vnd.ms-visio.drawing",
    "application/vnd.ms-visio.drawing.macroenabled.12",
    "application/vnd.ms-visio.stencil",
    "application/vnd.ms-visio.stencil.macroenabled.12",
    "application/vnd.ms-visio.template",
    "application/vnd.ms-visio.template.macroenabled.12",
    "application/vnd.ms-word.document.macroenabled.12",
    "application/vnd.ms-word.template.macroenabled.12",
    "application/vnd.ms-xpsdocument",
    "application/vnd.oasis.opendocument.formula",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.presentationml.slide",
    "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
    "application/vnd.openxmlformats-officedocument.presentationml.template",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.template",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
    "application/x-ibooks+zip",
    "application/x-itunes-ipa",
    "application/x-tika-iworks-protected",
    "application/x-tika-java-enterprise-archive",
    "application/x-tika-java-web-archive",
    "application/x-tika-ooxml",
    "application/x-tika-visio-ooxml",
    "application/x-xliff+zip",
    "application/x-xmind",
    "model/vnd.dwfx+xps",
    "application/vnd.sun.xml.calc",
    "application/vnd.sun.xml.writer",
    "application/vnd.sun.xml.writer.template",
    "application/vnd.sun.xml.draw",
    "application/vnd.sun.xml.impress",
    "application/vnd.openofficeorg.autotext",
    "application/vnd.oasis.opendocument.graphics-template",
    "application/vnd.oasis.opendocument.text-web",
    "application/vnd.oasis.opendocument.spreadsheet-template",
    "application/vnd.oasis.opendocument.graphics",
    "application/vnd.oasis.opendocument.image-template",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.oasis.opendocument.text-template",
    "application/vnd.oasis.opendocument.presentation",
    "application/vnd.oasis.opendocument.chart",
    "application/vnd.openofficeorg.extension",
    "application/vnd.oasis.opendocument.spreadsheet",
    "application/vnd.oasis.opendocument.image",
    "application/vnd.oasis.opendocument.formula-template",
    "application/vnd.oasis.opendocument.presentation-template",
    "application/vnd.oasis.opendocument.chart-template",
    "application/vnd.oasis.opendocument.text-master",
    "application/vnd.adobe.indesign-idml-package",
    "application/x-wacz",
    "application/x-vnd.datapackage+zip",
];

/// Tika's `ZipParser`.
pub struct ZipParser;

impl Parser for ZipParser {
    fn name(&self) -> &'static str {
        "extractous::ZipParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.pkg.ZipParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            ZIP,
            JAR,
            "application/vnd.android.package-archive",
            "application/vnd.etsi.asic-e+zip",
            "application/vnd.etsi.asic-s+zip",
            "application/vnd.google-earth.kmz",
        ]
    }

    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &[ZIP]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    /// Opens the archive, or where `ZipFile` cannot open it, the archive Tika's detector
    /// salvaged from it for the parser, as its metadata says, and reads each entry to rule out
    /// what Tika's path reads otherwise. The entries are read again as they are written:
    /// holding them all would hold every entry's data at once, where writing holds one at a
    /// time, for decompression that costs little beside the parse of each entry. A stored
    /// entry's data is the archive's own.
    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        if let Ok(zip) = ZipFile::open(input) {
            let paging = Paging::new(pages);
            let difference = check_entries(&zip, input.len(), &paging)?;
            return reading(
                Box::new(move |xhtml| write_archive(input, &zip, xhtml, &paging)),
                difference,
            );
        }
        if metadata.get(Key::Salvaged.name()) != Some("true") {
            return streamed(input, None);
        }
        let salvaged = match salvage(input) {
            Ok(salvaged) => salvaged,
            Err(Unsalvaged::Thrown) => return streamed(input, None),
            Err(Unsalvaged::Unported(reason)) => {
                return streamed(
                    input,
                    Some(Difference::new(format!(
                        "the detection salvaged the archive, which the native parser does not \
                         salvage, for {reason}, and reads by its local headers instead"
                    ))),
                )
            }
        };
        let in_memory = Paging::new(Pages::IN_MEMORY);
        let difference = check_entries(
            &ZipFile::open(&salvaged).expect("a salvaged archive opens"),
            input.len(),
            &in_memory,
        )?;
        reading(
            Box::new(move |xhtml| {
                let zip = ZipFile::open(&salvaged).expect("a salvaged archive opens");
                write_archive(input, &zip, xhtml, &in_memory)
            }),
            difference,
        )
    }
}

/// `prepared`, the native reading of an archive, as it differs from Tika's in `difference`, if
/// it does.
fn reading(
    prepared: Prepared<'_>,
    difference: Option<Difference>,
) -> Result<Prepared<'_>, Decline<'_>> {
    match difference {
        None => Ok(prepared),
        Some(difference) => Err(Decline::Differs {
            reading: prepared,
            difference,
        }),
    }
}

/// The archive `input` read by its local headers, as its native reading differs from Tika's in
/// `difference`, if it does.
fn streamed(input: &[u8], difference: Option<Difference>) -> Result<Prepared<'_>, Decline<'_>> {
    let streamed = Streamed::scan(input)?;
    let difference = difference.or_else(|| streamed.difference.clone());
    reading(
        Box::new(move |xhtml| streamed.write(input, xhtml)),
        difference,
    )
}

/// Reads each entry of the archive `zip`, `length` bytes in all, to rule out what Tika's path
/// reads otherwise, and finds the first detail in which the native reading differs from it.
fn check_entries(
    zip: &ZipFile,
    length: usize,
    paging: &Paging,
) -> Result<Option<Difference>, Decline<'static>> {
    let mut scan = Scan::new();
    let mut difference = None;
    for entry in zip.entries() {
        if entry.is_directory() || !entry.can_read() {
            continue;
        }
        difference = difference.or(entry_name(entry).1);
        match zip.read(entry) {
            Some(Content::Complete(content)) => {
                if let Some(reason) = scan.entry(&content) {
                    return Err(Decline::Deferred(reason));
                }
                paging.read(content.len());
            }
            Some(Content::Oversized(_)) if DEFERS => {
                return Err(too_large(&the_entry(&entry.name)))
            }
            Some(Content::Oversized(_)) => break,
            Some(Content::Truncated(_)) | None => {
                difference = difference.or_else(|| Some(cut_short(&entry.name)));
            }
        }
    }
    if let Some(reason) = scan.document(length) {
        return Err(Decline::Deferred(reason));
    }
    paging.release();
    Ok(difference)
}

/// How the native reading of an entry of `name` whose data is cut short or corrupt differs
/// from Tika's.
fn cut_short(name: &str) -> Difference {
    Difference::new(format!(
        "the data of the entry {name} is cut short or corrupt: the native parser writes what it \
         decodes of it and records the failure, where the stream Tika's embedded parser reads \
         throws, which fails the archive unless that parser catches it"
    ))
}

/// `parseWithZipFile` of the archive `zip`, read from `input` or salvaged from it. Tika checks
/// the integrity of `input` after the document has ended, so its result never reaches the
/// document's head, and not at all when an entry fails it (#99).
fn write_archive(
    input: &[u8],
    zip: &ZipFile,
    xhtml: &mut Xhtml,
    paging: &Paging,
) -> Result<(), Failure> {
    check_integrity(input, &CentralNames::of(zip), xhtml.metadata_mut());
    paging.release();
    update_media_type(zip, xhtml.metadata_mut());
    let written = write_entries(zip, xhtml, paging);
    xhtml.end_document()?;
    written
}

/// `parseWithZipFile`'s loop over the entries, which ends the document whatever happens.
fn write_entries(zip: &ZipFile, xhtml: &mut Xhtml, paging: &Paging) -> Result<(), Failure> {
    for entry in zip.entries() {
        if entry.is_directory() {
            continue;
        }
        let name = zip_file::entry_name(entry_name(entry).0, entry.version_made_by);
        if entry.uses_encryption() {
            record_unread(
                xhtml,
                &format!("{ENCRYPTED_DOCUMENT}: stream ({name}) is encrypted"),
            );
            continue;
        }
        if !entry.can_read() {
            record_unread(
                xhtml,
                &format!(
                    "org.apache.tika.exception.TikaException: Can't read archive stream ({name})"
                ),
            );
            continue;
        }
        let metadata = entry_metadata(entry, &name);
        let (content, cut) = match zip.read(entry) {
            Some(Content::Complete(content)) => (content, None),
            Some(Content::Oversized(content)) if !DEFERS => (content, Some(Cut(the_entry(&name)))),
            Some(Content::Oversized(_)) => return Err(undeferred(&name)),
            Some(Content::Truncated(content)) => {
                write_entry(xhtml, &metadata, &name, &content, None)?;
                record_unreadable(xhtml.metadata_mut(), &name, TRUNCATED);
                continue;
            }
            None => {
                record_unreadable(xhtml.metadata_mut(), &name, TRUNCATED);
                continue;
            }
        };
        write_entry(xhtml, &metadata, &name, &content, cut)?;
        paging.read(content.len());
    }
    Ok(())
}

/// Writes the entry of `name` and `metadata` holding `content`: all of its data, or, as `cut`
/// says, what it inflates into up to the most the native path decodes, where the document stops.
fn write_entry(
    xhtml: &mut Xhtml,
    metadata: &TikaMetadata,
    name: &str,
    content: &[u8],
    cut: Option<Cut>,
) -> Result<(), Failure> {
    write_entry_div(xhtml, name)?;
    xhtml.parse_embedded(content, metadata, Wrapping::PackageEntry)?;
    Cut::end(cut)
}

/// The entry of `name`, as a failure names it.
fn the_entry(name: &str) -> String {
    format!("the entry {name}")
}

/// The failure of a document whose entry of `name` was read in full when it was prepared, but
/// not when it is written.
fn undeferred(name: &str) -> Failure {
    Failure::Malformed(format!(
        "the entry {name} was not deferred, but its data cannot be read"
    ))
}

/// The central directory's entry names, each once, in order: the `LinkedHashSet` the
/// integrity check compares the local headers with.
#[derive(Default)]
struct CentralNames<'z> {
    ordered: Vec<&'z str>,
    members: HashSet<&'z str>,
}

impl<'z> CentralNames<'z> {
    fn of(zip: &'z ZipFile) -> Self {
        let mut central = Self::default();
        for entry in zip.entries() {
            if central.members.insert(&entry.name) {
                central.ordered.push(&entry.name);
            }
        }
        central
    }
}

/// `updateMediaType(ZipFile, Metadata)`: a JAR when it holds a manifest, a ZIP otherwise,
/// unless the archive was detected as a specialization of ZIP. Tika takes it for a JAR only
/// when the manifest is the first entry (#99).
fn update_media_type(zip: &ZipFile, metadata: &mut TikaMetadata) {
    let kind = if zip
        .entries()
        .iter()
        .any(|entry| entry.name == "META-INF/MANIFEST.MF")
    {
        JAR
    } else {
        ZIP
    };
    set_media_type(kind, metadata);
}

/// `setMediaTypeIfNotSpecialization`: `kind`, unless the archive was detected as a
/// specialization of ZIP.
fn set_media_type(kind: &str, metadata: &mut TikaMetadata) {
    let specialization = metadata
        .get("Content-Type")
        .and_then(MediaType::parse)
        .is_some_and(|incoming| ZIP_SPECIALIZATIONS.contains(&incoming.as_str()));
    if !specialization {
        metadata.set("Content-Type", kind);
    }
}

/// The stream an archive `ZipFile` cannot open is read with: one that allows stored entries
/// with a data descriptor, as Tika's retry does, and skips a split archive's signature.
fn local_headers(input: &[u8]) -> ZipStream<'_> {
    ZipStream::new(input, true).skipping_split_signature()
}

/// What reading an archive by its local headers finds before it is written: the names of the
/// entries that occur more than once, at most a hundred, in order, and the first detail in
/// which the native reading differs from Tika's.
struct Streamed {
    duplicates: Vec<String>,
    difference: Option<Difference>,
}

impl Streamed {
    /// Reads each entry of the archive `input`, to rule out what the native path does not read
    /// as Tika's does, and finds the duplicate names.
    fn scan(input: &[u8]) -> Result<Self, Decline<'static>> {
        let mut stream = local_headers(input);
        let mut scan = Scan::new();
        let mut seen = HashSet::new();
        let mut duplicates = Vec::new();
        let mut difference = None;
        while let Ok(Some(entry)) = stream.next_entry() {
            if entry.is_directory() {
                continue;
            }
            difference = difference.or(entry_name(&entry).1);
            match stream.content() {
                Ok(Content::Complete(content)) => {
                    if let Some(reason) = scan.entry(&content) {
                        return Err(Decline::Deferred(reason));
                    }
                }
                Ok(Content::Oversized(_)) if DEFERS => {
                    return Err(too_large(&the_entry(&entry.name)))
                }
                Ok(Content::Oversized(_)) => break,
                Err(stream::Failure::Method(method)) => {
                    difference = difference.or_else(|| {
                        Some(Difference::new(format!(
                            "the entry {} has a data descriptor after data compressed with \
                             method {method}, whose end the native parser does not find: it \
                             stops at the entry, where commons-compress reads on",
                            entry.name
                        )))
                    });
                    break;
                }
                Ok(Content::Truncated(_)) => break,
                Err(_) => continue,
            }
            if !seen.insert(entry.name.clone()) && duplicates.len() < MAX_INTEGRITY_CHECK_ENTRIES {
                duplicates.push(entry.name);
            }
        }
        if let Some(reason) = scan.document(input.len()) {
            return Err(Decline::Deferred(reason));
        }
        Ok(Streamed {
            duplicates,
            difference,
        })
    }

    /// `parseWithStream`: the duplicate names as the integrity check's result, which cannot
    /// compare the entries with a central directory, then each entry but a directory up to
    /// the first whose data cannot be read whole or where the stream fails.
    fn write(&self, input: &[u8], xhtml: &mut Xhtml) -> Result<(), Failure> {
        let metadata = xhtml.metadata_mut();
        set_media_type(ZIP, metadata);
        if self.duplicates.is_empty() {
            metadata.set("zip:integrity-check-result", "PARTIAL");
        } else {
            metadata.set("zip:integrity-check-result", "FAIL");
            for name in &self.duplicates {
                metadata.add("zip:duplicate-entry-names", name.clone());
            }
        }
        let mut stream = local_headers(input);
        loop {
            let entry = match stream.next_entry() {
                Ok(Some(entry)) => entry,
                Ok(None) => return Ok(()),
                Err(failure) => {
                    record_exception(
                        xhtml.metadata_mut(),
                        &format!(
                            "java.util.zip.ZipException: the local header after the last entry \
                             cannot be read: {failure:?}"
                        ),
                    );
                    return Ok(());
                }
            };
            if entry.is_directory() {
                continue;
            }
            let name = zip_file::entry_name(entry_name(&entry).0, entry.version_made_by);
            if entry.uses_encryption() {
                record_unread(
                    xhtml,
                    &format!("{ENCRYPTED_DOCUMENT}: stream ({name}) is encrypted"),
                );
                continue;
            }
            let metadata = entry_metadata(&entry, &name);
            match stream.content() {
                Ok(Content::Complete(content)) => {
                    write_entry(xhtml, &metadata, &name, &content, None)?;
                }
                Ok(Content::Oversized(content)) if !DEFERS => {
                    write_entry(
                        xhtml,
                        &metadata,
                        &name,
                        &content,
                        Some(Cut(the_entry(&name))),
                    )?;
                }
                Ok(Content::Truncated(_)) => {
                    record_unreadable(xhtml.metadata_mut(), &name, TRUNCATED_SEGMENT);
                    return Ok(());
                }
                Err(_) => record_unread(
                    xhtml,
                    &format!(
                        "org.apache.tika.exception.TikaException: Can't read archive stream \
                         ({name})"
                    ),
                ),
                Ok(Content::Oversized(_)) => return Err(undeferred(&name)),
            }
        }
    }
}

/// `detectEntryName`: a name from the Unicode extra field as commons-compress read it, a name
/// flagged UTF-8 (EFS) decoded so when it is valid UTF-8, else the name's bytes decoded in the
/// charset Tika's encoding detector finds, told a name flagged UTF-8 claims so. Tika takes the
/// flag only as a hint to its detector, which may find another charset (#99). With the name,
/// how it may differ from Tika's: where its bytes are malformed in a charset Java decodes
/// otherwise than the WHATWG decoder, which replaces them its own way, and where the native
/// decoders do not decode the charset, for which the name is as commons-compress decodes it.
fn entry_name(entry: &ZipEntry) -> (String, Option<Difference>) {
    if entry.name_source == NameSource::UnicodeExtraField {
        return (entry.name.clone(), None);
    }
    if entry.name_source == NameSource::NameWithEfsFlag {
        if let Ok(name) = std::str::from_utf8(&entry.raw_name) {
            return (name.to_owned(), None);
        }
    }
    let detection = match entry.name_source {
        NameSource::NameWithEfsFlag => {
            charset::detect_hinted(&entry.raw_name, "text/plain; charset=UTF-8")
        }
        _ => charset::detect(&entry.raw_name, None),
    };
    let charset = detection.decode_as;
    if let Some(name) = charset.decode_exactly(&entry.raw_name) {
        return (name, None);
    }
    let differs = |how: &str| {
        Some(Difference::new(format!(
            "the name of the entry {} is detected in {}, {how}",
            entry.name,
            charset.name()
        )))
    };
    match charset.decode(&entry.raw_name) {
        Some(name) => (
            name,
            differs("in which its malformed bytes are replaced as the WHATWG decoder replaces them"),
        ),
        None => (
            entry.name.clone(),
            differs("which the native decoders do not decode, so it is read as commons-compress reads it"),
        ),
    }
}

/// An entry Tika cannot read: its failure recorded. Tika also writes its name in a paragraph,
/// as though it were text of the archive (#99).
fn record_unread(xhtml: &mut Xhtml, exception: &str) {
    record_stream_exception(xhtml.metadata_mut(), exception);
}

/// What the stream of an entry whose data is cut short or corrupt throws.
const TRUNCATED: &str = "Truncated ZIP file: its data is cut short or corrupt";
/// What the stream of an entry whose data ends with the archive throws.
const TRUNCATED_SEGMENT: &str = "Truncated ZIP file: its data ends with the archive, as the \
                                 last entry of a split archive's segment does";

fn instant(time: Instant) -> String {
    instant_string(time.seconds, time.nanos)
}

/// `buildEntryMetadata`: the entry's name and path, times, sizes, method, CRC, Unix mode,
/// platform, version and comment.
fn entry_metadata(entry: &ZipEntry, name: &str) -> TikaMetadata {
    let mut metadata = TikaMetadata::new();
    if !name.is_empty() {
        metadata.set(RESOURCE_NAME, name);
        metadata.set(INTERNAL_PATH, name);
    }
    if let Some(created) = entry.created() {
        metadata.set("dcterms:created", instant(created));
    }
    if let Some(modified) = entry.modified() {
        metadata.set("dcterms:modified", instant(modified));
    }
    if entry.size >= 0 {
        metadata.set("Content-Length", entry.size.to_string());
        metadata.set("zip:uncompressed-size", entry.size.to_string());
    }
    if entry.compressed_size >= 0 {
        metadata.set("zip:compressed-size", entry.compressed_size.to_string());
    }
    metadata.set("zip:compression-method", entry.method.to_string());
    if entry.crc >= 0 {
        metadata.set("zip:crc32", entry.crc.to_string());
    }
    let unix_mode = entry.unix_mode();
    if unix_mode != 0 {
        metadata.set("zip:unix-mode", unix_mode.to_string());
    }
    metadata.set("zip:platform", entry.platform().to_string());
    metadata.set("zip:version-made-by", entry.version_made_by.to_string());
    if !entry.comment.is_empty() {
        metadata.set("zip:comment", entry.comment.clone());
    }
    metadata
}

/// `performIntegrityCheck`: the local headers read front to back against the central
/// directory's names, up to where the stream fails, at most a hundred of each kind recorded.
fn check_integrity(input: &[u8], central: &CentralNames, metadata: &mut TikaMetadata) {
    let mut stream = ZipStream::new(input, true);
    let mut seen: HashSet<String> = HashSet::new();
    let mut duplicates = Vec::new();
    let mut local_only = Vec::new();
    while let Ok(Some(entry)) = stream.next_entry() {
        let name = entry.name;
        if seen.contains(&name) {
            if duplicates.len() < MAX_INTEGRITY_CHECK_ENTRIES {
                duplicates.push(name.clone());
            }
        } else {
            seen.insert(name.clone());
        }
        if !central.members.contains(name.as_str())
            && local_only.len() < MAX_INTEGRITY_CHECK_ENTRIES
        {
            local_only.push(name);
        }
    }
    let central_only: Vec<&str> = central
        .ordered
        .iter()
        .copied()
        .filter(|name| !seen.contains(*name))
        .take(MAX_INTEGRITY_CHECK_ENTRIES)
        .collect();
    let passed = duplicates.is_empty() && local_only.is_empty() && central_only.is_empty();
    metadata.set(
        "zip:integrity-check-result",
        if passed { "PASS" } else { "FAIL" },
    );
    for name in duplicates {
        metadata.add("zip:duplicate-entry-names", name);
    }
    for name in local_only {
        metadata.add("zip:local-header-only-entries", name);
    }
    for name in central_only {
        metadata.add("zip:central-directory-only-entries", name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::zip::file::tests::zip;

    /// Where the central directory record of a one-entry archive named `a` starts.
    fn central_record(data: &[u8]) -> usize {
        data.len() - 22 - 47
    }

    fn defers(input: &[u8]) -> Option<String> {
        ZipParser.defers(input, &TikaMetadata::new(), &ParserConfigs::default())
    }

    #[test]
    fn parses_archives_zip_file_reads() {
        assert_eq!(defers(&zip(&[("a", b"hej")])), None);
    }

    #[test]
    fn reads_archives_zip_file_cannot_open_by_their_local_headers_or_as_salvaged() {
        let data = zip(&[("a", b"hej")]);
        let cut = &data[..data.len() - 10];
        assert_eq!(defers(cut), None);
        let (xml, _) = crate::Extractor::new()
            .set_xml_output(true)
            .extract_bytes_to_string(cut)
            .unwrap();
        assert!(xml.contains("<p>hej</p>"), "{xml}");
        let mut salvaged = TikaMetadata::new();
        salvaged.set(Key::Salvaged.name(), "true");
        assert_eq!(
            ZipParser.defers(cut, &salvaged, &ParserConfigs::default()),
            None
        );
    }

    #[test]
    fn reads_every_method_commons_compress_decompresses() {
        use crate::compress::zip::file::tests::zip_compressed;
        use std::io::Write;
        let text = b"Hej fra K\xC3\xB8benhavn, hej igen.";
        let mut deflater =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        deflater.write_all(text).unwrap();
        let methods = [
            (9, deflater.finish().unwrap()),
            (12, crate::compress::bzip2::tests::bzip2(text)),
            (93, crate::compress::zstd::tests::zstd(text)),
            (95, crate::compress::xz::tests::xz(text)),
        ];
        for (method, data) in methods {
            let archive = zip_compressed("a.txt", method, &data, text.len() as u32);
            assert_eq!(defers(&archive), None, "method {method}");
            let zip = ZipFile::open(&archive).unwrap();
            let entry = &zip.entries()[0];
            assert_eq!(entry.method, method);
            assert_eq!(
                zip.read(entry).map(|content| content.bytes().to_vec()),
                Some(text.to_vec()),
                "method {method}"
            );
            let mut stream = ZipStream::new(&archive, false);
            stream.next_entry().unwrap().unwrap();
            assert_eq!(
                stream.content().map(|content| content.bytes().to_vec()),
                Ok(text.to_vec()),
                "method {method}"
            );
        }
    }

    #[cfg(feature = "tika")]
    #[test]
    fn defers_archives_nested_ten_deep() {
        let mut nested = zip(&[("level.txt", b"text")]);
        for _ in 1..9 {
            nested = zip(&[("level.txt", b"text"), ("inner.zip", &nested)]);
        }
        assert_eq!(defers(&nested), None);
        let reason = defers(&zip(&[("inner.zip", &nested)])).unwrap();
        assert!(reason.contains("nine deep"), "{reason}");
    }

    #[test]
    fn defers_entries_cut_short() {
        let mut data = zip(&[("a", &[7u8; 5000])]);
        let cd = central_record(&data);
        data[18..22].copy_from_slice(&2u32.to_le_bytes());
        data[cd + 20..cd + 24].copy_from_slice(&2u32.to_le_bytes());
        let reason = defers(&data).unwrap();
        assert!(reason.contains("cut short"), "{reason}");
    }
}
