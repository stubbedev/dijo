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
use super::{record_unreadable, too_large, write_entry_div};
use crate::compress::zip::extra::Instant;
use crate::compress::zip::file::{self as zip_file, NameSource, ZipEntry, ZipFile};
use crate::compress::zip::salvage::salvage;
use crate::compress::zip::stream::{self, ZipStream};
use crate::compress::zip::Content;
use crate::compress::MAX_DECODED;
use crate::config::ParserConfigs;
use crate::formats::embedded::{
    record_exception, record_stream_exception, Wrapping, ENCRYPTED_DOCUMENT, INTERNAL_PATH,
    RESOURCE_NAME,
};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Pages, Paging};
use crate::java::time::instant_string;
use crate::mime::{Key, MediaType};
use crate::text::charset::{self, Charset};

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
    ) -> Result<Prepared<'a>, Decline<'static>> {
        if let Ok(zip) = ZipFile::open(input) {
            let paging = Paging::new(pages);
            check_entries(&zip, input.len(), &paging)?;
            return Ok(Box::new(move |xhtml| {
                write_archive(input, &zip, xhtml, &paging)
            }));
        }
        if metadata.get(Key::Salvaged.name()) != Some("true") {
            let streamed = Streamed::scan(input)?;
            return Ok(Box::new(move |xhtml| streamed.write(input, xhtml)));
        }
        let salvaged = salvage(input).map_err(|unsalvaged| {
            Decline::Deferred(format!(
                "the detection salvaged the archive, which salvages no more: {unsalvaged:?}"
            ))
        })?;
        let unopened = || "the salvaged archive does not open".to_owned();
        let in_memory = Paging::new(Pages::IN_MEMORY);
        check_entries(
            &ZipFile::open(&salvaged).map_err(|_| unopened())?,
            input.len(),
            &in_memory,
        )?;
        Ok(Box::new(move |xhtml| {
            let zip = ZipFile::open(&salvaged).map_err(|_| Failure::Malformed(unopened()))?;
            write_archive(input, &zip, xhtml, &in_memory)
        }))
    }
}

/// Reads each entry of the archive `zip`, `length` bytes in all, to rule out what Tika's path
/// reads otherwise.
fn check_entries(zip: &ZipFile, length: usize, paging: &Paging) -> Result<(), Decline<'static>> {
    let mut scan = Scan::new();
    for entry in zip.entries() {
        if entry.is_directory() || !entry.can_read() {
            continue;
        }
        if entry_name(entry).is_none() {
            return Err(Decline::Deferred(format!(
                "the name of the entry {} is detected in a charset the native decoders do \
                 not decode",
                entry.name
            )));
        }
        match zip.read(entry) {
            Some(Content::Complete(content)) => {
                if let Some(reason) = scan.entry(&content) {
                    return Err(Decline::Deferred(reason));
                }
                paging.read(content.len());
            }
            Some(Content::Truncated(_)) => {
                return Err(Decline::Deferred(format!(
                    "the data of the entry {} is cut short or corrupt, which Tika's embedded \
                     parser reads as far as it goes",
                    entry.name
                )))
            }
            Some(Content::Oversized(_)) => {
                return Err(too_large(format!(
                    "the entry {} inflates into more than {MAX_DECODED} bytes",
                    entry.name
                )))
            }
            None => {
                return Err(Decline::Deferred(format!(
                    "the entry {} is compressed with method {}, which the port does not \
                     decompress",
                    entry.name, entry.method
                )))
            }
        }
    }
    if let Some(reason) = scan.document(length) {
        return Err(Decline::Deferred(reason));
    }
    paging.release();
    Ok(())
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
        let name = entry_name(entry).ok_or_else(|| {
            Failure::Malformed(format!("the name of {} cannot be decoded", entry.name))
        })?;
        let name = zip_file::entry_name(name, entry.version_made_by);
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
        let content = match zip.read(entry) {
            Some(Content::Complete(content)) => content,
            _ => {
                return Err(Failure::Malformed(format!(
                    "the entry {name} was not deferred, but its data cannot be read"
                )))
            }
        };
        let metadata = entry_metadata(entry, &name);
        write_entry_div(xhtml, &name)?;
        xhtml.parse_embedded(&content, &metadata, Wrapping::PackageEntry)?;
        paging.read(content.len());
    }
    Ok(())
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
/// entries that occur more than once, at most a hundred, in order.
struct Streamed {
    duplicates: Vec<String>,
}

impl Streamed {
    /// Reads each entry of the archive `input`, to rule out what the native path does not read
    /// as Tika's does, and finds the duplicate names.
    fn scan(input: &[u8]) -> Result<Self, Decline<'static>> {
        let mut stream = local_headers(input);
        let mut scan = Scan::new();
        let mut seen = HashSet::new();
        let mut duplicates = Vec::new();
        while let Ok(Some(entry)) = stream.next_entry() {
            if entry.is_directory() {
                continue;
            }
            if entry_name(&entry).is_none() {
                return Err(Decline::Deferred(format!(
                    "the name of the entry {} is detected in a charset the native decoders do \
                     not decode",
                    entry.name
                )));
            }
            match stream.content() {
                Ok(Content::Complete(content)) => {
                    if let Some(reason) = scan.entry(&content) {
                        return Err(Decline::Deferred(reason));
                    }
                }
                Ok(Content::Oversized(_)) => {
                    return Err(too_large(format!(
                        "the entry {} inflates into more than {MAX_DECODED} bytes",
                        entry.name
                    )))
                }
                Err(stream::Failure::Method(method)) => {
                    return Err(Decline::Deferred(format!(
                        "the entry {} is compressed with method {method}, which the port does \
                         not decompress",
                        entry.name
                    )))
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
        Ok(Streamed { duplicates })
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
            let name = entry_name(&entry).ok_or_else(|| {
                Failure::Malformed(format!("the name of {} cannot be decoded", entry.name))
            })?;
            let name = zip_file::entry_name(name, entry.version_made_by);
            if entry.uses_encryption() {
                record_unread(
                    xhtml,
                    &format!("{ENCRYPTED_DOCUMENT}: stream ({name}) is encrypted"),
                );
                continue;
            }
            match stream.content() {
                Ok(Content::Complete(content)) => {
                    let metadata = entry_metadata(&entry, &name);
                    write_entry_div(xhtml, &name)?;
                    xhtml.parse_embedded(&content, &metadata, Wrapping::PackageEntry)?;
                }
                Ok(Content::Truncated(_)) => {
                    record_unreadable(
                        xhtml.metadata_mut(),
                        &name,
                        "Truncated ZIP file: its data ends with the archive, as the last entry \
                         of a split archive's segment does",
                    );
                    return Ok(());
                }
                Err(_) => record_unread(
                    xhtml,
                    &format!(
                        "org.apache.tika.exception.TikaException: Can't read archive stream \
                         ({name})"
                    ),
                ),
                Ok(Content::Oversized(_)) => {
                    return Err(Failure::Malformed(format!(
                        "the entry {name} was not deferred, but its data cannot be read"
                    )))
                }
            }
        }
    }
}

/// `detectEntryName`: a name from the Unicode extra field as commons-compress read it, a name
/// flagged UTF-8 (EFS) decoded so when it is valid UTF-8, else the name's bytes decoded in the
/// charset Tika's encoding detector finds, told a name flagged UTF-8 claims so; `None` for a
/// charset the native decoders do not decode. Tika takes the flag only as a hint to its
/// detector, which may find another charset (#99).
fn entry_name(entry: &ZipEntry) -> Option<String> {
    if entry.name_source == NameSource::UnicodeExtraField {
        return Some(entry.name.clone());
    }
    if entry.name_source == NameSource::NameWithEfsFlag {
        if let Ok(name) = std::str::from_utf8(&entry.raw_name) {
            return Some(name.to_owned());
        }
    }
    let detection = match entry.name_source {
        NameSource::NameWithEfsFlag => {
            charset::detect_hinted(&entry.raw_name, "text/plain; charset=UTF-8")
        }
        _ => charset::detect(&entry.raw_name, None),
    };
    decode_name(&entry.raw_name, detection.decode_as)
}

fn decode_name(raw: &[u8], charset: Charset) -> Option<String> {
    charset.decode_exactly(raw)
}

/// An entry Tika cannot read: its failure recorded. Tika also writes its name in a paragraph,
/// as though it were text of the archive (#99).
fn record_unread(xhtml: &mut Xhtml, exception: &str) {
    record_stream_exception(xhtml.metadata_mut(), exception);
}

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
    fn defers_methods_it_does_not_decompress() {
        let mut data = zip(&[("a", b"hej")]);
        let cd = central_record(&data);
        data[8..10].copy_from_slice(&12u16.to_le_bytes());
        data[cd + 10..cd + 12].copy_from_slice(&12u16.to_le_bytes());
        let reason = defers(&data).unwrap();
        assert!(reason.contains("method 12"), "{reason}");
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
