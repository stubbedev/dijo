//! Tika's `DefaultZipContainerDetector`: an archive or compression format by its first bytes,
//! and a ZIP archive by its entries, through the ZIP container detectors in the order the native
//! image's class path loads them: OOXML packages (`OPCPackageDetector`), iWork, iOS apps, Java
//! and Android archives, KMZ, OpenDocument, StarOffice XML, Frictionless data packages and
//! GeoGebra. An archive `ZipFile` cannot open is read entry by entry instead.

mod opc;

use std::collections::HashSet;

use crate::compress::zip::file::ZipFile;
use crate::compress::zip::stream::{Failure, StreamEntry, ZipStream};
use crate::compress::zip::Content;
use crate::mime::archive::{self, Archive};
use crate::mime::media_type::MediaType;
use crate::xml::{self, Event, Flow, XMLNS};

/// What the ZIP container detector found in a ZIP archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZipDetection {
    pub(crate) media_type: MediaType,
    /// Whether `ZipFile` opened the archive: Tika records it as `zip:detector-zip-file-opened`.
    pub(crate) opened: bool,
    /// Whether the entry-by-entry reading needed stored entries with data descriptors allowed:
    /// `zip:detector-data-descriptor-required`.
    pub(crate) data_descriptor_required: bool,
}

/// What `DefaultZipContainerDetector.detect` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Detected {
    /// A ZIP archive, which the detector spools to read its entries.
    Zip(ZipDetection),
    /// Another format by its first bytes, or `None` for none.
    Other(Option<MediaType>),
}

/// `DefaultZipContainerDetector.detect`.
pub(crate) fn detect(data: &[u8]) -> Detected {
    let prefix = &data[..data.len().min(1024)];
    match archive::archive(prefix) {
        Some(Archive::Tiff) => Detected::Other(Some(MediaType::named("image/tiff"))),
        Some(Archive::Other(kind)) => Detected::Other(Some(MediaType::named(kind))),
        Some(Archive::Zip) => Detected::Zip(detect_zip(data)),
        None => Detected::Other(archive::compressor(prefix).map(MediaType::named)),
    }
}

fn detect_zip(data: &[u8]) -> ZipDetection {
    if let Ok(zip) = ZipFile::open(data) {
        let media_type = by_file(&zip).unwrap_or_else(|| MediaType::named("application/zip"));
        return ZipDetection {
            media_type,
            opened: true,
            data_descriptor_required: false,
        };
    }
    match by_stream(data, false) {
        Streamed::Found(media_type) => ZipDetection {
            media_type,
            opened: false,
            data_descriptor_required: false,
        },
        Streamed::NeedsDataDescriptors => ZipDetection {
            media_type: match by_stream(data, true) {
                Streamed::Found(media_type) => media_type,
                Streamed::NeedsDataDescriptors => {
                    unreachable!("a stream that allows data descriptors is not asked to")
                }
            },
            opened: false,
            data_descriptor_required: true,
        },
    }
}

/// Why a file-mode detector stopped the loop: `ZipFile.getInputStream` threw.
struct Aborted;

/// A ZIP container detector over an opened archive.
type FileDetector = fn(&ZipFile<'_>) -> Result<Option<MediaType>, Aborted>;

/// The detectors over an opened archive, in order; `None` for a plain ZIP archive.
fn by_file(zip: &ZipFile<'_>) -> Option<MediaType> {
    let detectors: [FileDetector; 9] = [
        |zip| Ok(opc::detect(zip)),
        |zip| Ok(iwork_file(zip)),
        |zip| Ok(ipa_file(zip)),
        |zip| Ok(jar_file(zip)),
        |zip| Ok(kmz_file(zip)),
        |zip| Ok(open_document_file(zip)),
        star_office_file,
        |zip| Ok(frictionless_file(zip)),
        |zip| Ok(geogebra_file(zip)),
    ];
    for detector in detectors {
        match detector(zip) {
            Ok(Some(kind)) => return Some(kind),
            Ok(None) => {}
            Err(Aborted) => return None,
        }
    }
    None
}

/// `XmlRootExtractor.extractRootElement(stream)`: the namespace and name of the root element.
fn root_element(content: &[u8]) -> Option<(String, String)> {
    let mut root = None;
    let _ = xml::parse(content, &mut |event| {
        if let Event::Start(name, _) = event {
            root = Some((name.namespace.clone(), name.local_name.clone()));
            return Flow::Stop;
        }
        Flow::Continue
    });
    root
}

/// `IWorkPackageParser.IWORK_CONTENT_ENTRIES`, a `HashSet`.
const IWORK_CONTENT_ENTRIES: [&str; 3] = ["index.apxl", "index.xml", "presentation.apxl"];

/// `IWORKDocumentType.detectType`: the iWork 2008 document a content entry's root names.
fn iwork_type(content: &[u8]) -> Option<MediaType> {
    let (namespace, local) = root_element(content)?;
    let kind = match (namespace.as_str(), local.as_str()) {
        ("http://developer.apple.com/namespaces/keynote2", "presentation") => {
            "application/vnd.apple.keynote"
        }
        ("http://developer.apple.com/namespaces/ls", "document") => "application/vnd.apple.numbers",
        ("http://developer.apple.com/namespaces/sl", "document") => "application/vnd.apple.pages",
        _ => return None,
    };
    Some(MediaType::named(kind))
}

/// `IWork18DocumentType.detectIfPossible`.
fn iwork18(name: &str) -> Option<MediaType> {
    let kind = if name.ends_with(".numbers/Metadata/BuildVersionHistory.plist") {
        "application/vnd.apple.numbers.18"
    } else if name.ends_with(".pages/Metadata/BuildVersionHistory.plist") {
        "application/vnd.apple.pages.18"
    } else if name.ends_with(".key/Metadata/BuildVersionHistory.plist") {
        "application/vnd.apple.keynote.18"
    } else {
        return None;
    };
    Some(MediaType::named(kind))
}

/// `IWork13DocumentType.detectIfPossible`.
fn iwork13(name: &str) -> Option<MediaType> {
    let slide = name == "Index/MasterSlide.iwa"
        || name.starts_with("Index/MasterSlide-")
        || name == "Index/Slide.iwa"
        || name.starts_with("Index/Slide-");
    (name.ends_with(".iwa") && slide).then(|| MediaType::named("application/vnd.apple.keynote.13"))
}

const IWORK13_UNKNOWN: &str = "application/vnd.apple.unknown.13";

fn iwork_file(zip: &ZipFile<'_>) -> Option<MediaType> {
    if let Some(kind) = zip.entries().iter().find_map(|e| iwork18(&e.name)) {
        return Some(kind);
    }
    if zip.entry("Metadata/BuildVersionHistory.plist").is_some() {
        return Some(
            zip.entries()
                .iter()
                .find_map(|e| iwork13(&e.name))
                .unwrap_or_else(|| MediaType::named(IWORK13_UNKNOWN)),
        );
    }
    zip.entry("buildVersionHistory.plist")?;
    for name in crate::mime::java::hash_set_order(IWORK_CONTENT_ENTRIES) {
        let found = zip
            .entry(name)
            .and_then(|entry| zip.read(entry))
            .and_then(|content| iwork_type(content.bytes()));
        if found.is_some() {
            return found;
        }
    }
    Some(MediaType::named("application/vnd.apple.iwork"))
}

/// A pattern an entry name matches in full.
type NamePattern = fn(&str) -> bool;

/// The entry names `IPADetector` must all see, `^...$` matched in full.
fn ipa_patterns() -> Vec<NamePattern> {
    fn app_path(name: &str, suffix: &str) -> bool {
        name.strip_prefix("Payload/")
            .and_then(|rest| rest.strip_suffix(suffix))
            .and_then(|rest| rest.strip_suffix(".app/"))
            .is_some_and(|middle| !middle.contains(crate::mime::java::is_line_terminator))
    }
    vec![
        |name| name == "Payload/",
        |name| app_path(name, ""),
        |name| app_path(name, "_CodeSignature/"),
        |name| app_path(name, "_CodeSignature/CodeResources"),
        |name| app_path(name, "Info.plist"),
        |name| app_path(name, "PkgInfo"),
    ]
}

const IPA: &str = "application/x-itunes-ipa";

fn ipa_file(zip: &ZipFile<'_>) -> Option<MediaType> {
    let mut patterns = ipa_patterns();
    for entry in zip.entries() {
        patterns.retain(|matches| !matches(&entry.name));
        if patterns.is_empty() {
            return Some(MediaType::named(IPA));
        }
    }
    None
}

fn jar_file(zip: &ZipFile<'_>) -> Option<MediaType> {
    let has = |name: &str| zip.entry(name).is_some();
    let kind = if has("META-INF/MANIFEST.MF") {
        if has("AndroidManifest.xml") {
            "application/vnd.android.package-archive"
        } else if has("WEB-INF/") {
            "application/x-tika-java-web-archive"
        } else if has("META-INF/application.xml") {
            "application/x-tika-java-enterprise-archive"
        } else if has("base/manifest/AndroidManifest.xml") {
            "application/x-authorware-bin"
        } else {
            "application/java-archive"
        }
    } else if has("AndroidManifest.xml") {
        "application/vnd.android.package-archive"
    } else {
        return None;
    };
    Some(MediaType::named(kind))
}

const KMZ: &str = "application/vnd.google-earth.kmz";

fn is_top_level_kml(name: &str) -> bool {
    !name.contains(['/', '\\']) && crate::mime::java::to_lower_case(name).ends_with(".kml")
}

fn kmz_file(zip: &ZipFile<'_>) -> Option<MediaType> {
    let mut count = 0;
    for entry in zip.entries() {
        if !entry.is_directory() && !entry.name.contains(['/', '\\']) {
            if is_top_level_kml(&entry.name) {
                count += 1;
            }
            if count > 1 {
                return None;
            }
        }
    }
    (count == 1).then(|| MediaType::named(KMZ))
}

fn open_document_file(zip: &ZipFile<'_>) -> Option<MediaType> {
    let entry = zip.entry("mimetype").filter(|entry| entry.size > 0)?;
    match zip.read(entry)? {
        Content::Complete(bytes) => MediaType::parse(&String::from_utf8_lossy(&bytes)),
        Content::Truncated(_) => None,
    }
}

/// `StarOfficeDetector.detectStarOfficeX`: the media type a manifest's file entries name.
fn star_office_type(content: &[u8]) -> Option<MediaType> {
    let mut found = None;
    let _ = xml::parse(content, &mut |event| {
        let Event::Start(name, attributes) = event else {
            return Flow::Continue;
        };
        if name.local_name != "file-entry" {
            return Flow::Continue;
        }
        let mut media_type = None;
        let mut full_path = None;
        for attribute in attributes.iter().filter(|a| a.namespace != XMLNS) {
            if attribute.local_name == "media-type" {
                media_type = Some(attribute.value.as_str());
                let kind = match attribute.value.as_str() {
                    "application/vnd.sun.xml.writer" => "application/vnd.sun.xml.writer",
                    "application/vnd.sun.xml.calc" => "application/vnd.sun.xml.calc",
                    "application/vnd.sun.xml.draw" => "application/vnd.sun.xml.draw",
                    "application/vnd.sun.xml.impress" => "application/vnd.sun.xml.impress",
                    "application/vnd.sun.star.configuration-data" => {
                        "application/vnd.openofficeorg.extension"
                    }
                    _ => continue,
                };
                found = Some(MediaType::named(kind));
                return Flow::Stop;
            } else if attribute.local_name == "full-path" {
                full_path = Some(attribute.value.as_str());
            }
        }
        if media_type == Some("") && full_path == Some("/") {
            found = Some(MediaType::named("application/vnd.openofficeorg.autotext"));
            return Flow::Stop;
        }
        Flow::Continue
    });
    found
}

fn star_office_file(zip: &ZipFile<'_>) -> Result<Option<MediaType>, Aborted> {
    let Some(entry) = zip.entry("META-INF/manifest.xml") else {
        return Ok(None);
    };
    let content = zip.read(entry).ok_or(Aborted)?;
    Ok(star_office_type(content.bytes()))
}

/// The parts `FrictionlessPackageDetector` counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FrictionlessPart {
    Package,
    Archive,
    Indexes,
    Pages,
}

fn frictionless_part(name: &str) -> Option<FrictionlessPart> {
    if name.starts_with("archive/") {
        Some(FrictionlessPart::Archive)
    } else if name.starts_with("indexes/") {
        Some(FrictionlessPart::Indexes)
    } else if name.starts_with("pages/") {
        Some(FrictionlessPart::Pages)
    } else if name == "datapackage.json" {
        Some(FrictionlessPart::Package)
    } else {
        None
    }
}

const WACZ: &str = "application/x-wacz";

fn frictionless_type(parts: &HashSet<FrictionlessPart>) -> Option<&'static str> {
    if !parts.contains(&FrictionlessPart::Package) {
        return None;
    }
    if parts.len() == 1 {
        return Some("application/x-vnd.datapackage+zip");
    }
    let wacz = parts.contains(&FrictionlessPart::Archive)
        || (parts.contains(&FrictionlessPart::Indexes) && parts.contains(&FrictionlessPart::Pages));
    wacz.then_some(WACZ)
}

fn frictionless_file(zip: &ZipFile<'_>) -> Option<MediaType> {
    let mut parts = HashSet::new();
    for entry in zip.entries() {
        parts.extend(frictionless_part(&entry.name));
        if frictionless_type(&parts) == Some(WACZ) {
            return Some(MediaType::named(WACZ));
        }
    }
    frictionless_type(&parts).map(MediaType::named)
}

/// What `GeoGebraDetector` has seen.
#[derive(Debug, Default)]
struct GeoGebra {
    geogebra_xml: bool,
    macro_xml: bool,
    structure_json: bool,
    slide_xml: bool,
}

/// `^_slide\d+/geogebra\.xml$`.
fn is_slide_xml(name: &str) -> bool {
    name.strip_prefix("_slide")
        .and_then(|rest| rest.strip_suffix("/geogebra.xml"))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

impl GeoGebra {
    fn update(&mut self, name: &str) {
        match name {
            "geogebra.xml" => self.geogebra_xml = true,
            "geogebra_macro.xml" => self.macro_xml = true,
            "structure.json" => self.structure_json = true,
            name if is_slide_xml(name) => self.slide_xml = true,
            _ => {}
        }
    }

    fn decide(&self) -> Option<MediaType> {
        let kind = if self.structure_json && self.slide_xml {
            "application/vnd.geogebra.slides"
        } else if self.geogebra_xml {
            "application/vnd.geogebra.file"
        } else if self.macro_xml {
            "application/vnd.geogebra.tool"
        } else {
            return None;
        };
        Some(MediaType::named(kind))
    }
}

fn geogebra_file(zip: &ZipFile<'_>) -> Option<MediaType> {
    let names = GeoGebra {
        geogebra_xml: zip.entry("geogebra.xml").is_some(),
        macro_xml: zip.entry("geogebra_macro.xml").is_some(),
        structure_json: zip.entry("structure.json").is_some(),
        slide_xml: zip.entry("structure.json").is_some()
            && zip.entries().iter().any(|e| is_slide_xml(&e.name)),
    };
    names.decide()
}

/// The `StreamingDetectContext` the detectors keep while entries stream past.
#[derive(Default)]
struct Streaming {
    ooxml_hints: usize,
    iwork_names: Option<HashSet<String>>,
    ipa_patterns: Option<Vec<NamePattern>>,
    jar_manifest: bool,
    kml_count: Option<usize>,
    frictionless: Option<HashSet<FrictionlessPart>>,
    geogebra: Option<GeoGebra>,
}

impl Streaming {
    /// The detectors' `streamingDetectUpdate` for one entry, in order.
    fn update(&mut self, entry: &StreamEntry) -> Result<Option<MediaType>, Failure> {
        let name = entry.name.as_str();
        if name == "[Content_Types].xml" {
            let found = entry
                .data
                .as_ref()
                .ok()
                .and_then(|c| opc::content_types_type(c.bytes()));
            return Ok(Some(
                found.unwrap_or_else(|| MediaType::named("application/x-tika-ooxml")),
            ));
        }
        if opc::OOXML_HINTS.contains(&name) {
            self.ooxml_hints += 1;
        }
        self.iwork_names
            .get_or_insert_with(HashSet::new)
            .insert(name.to_owned());
        if IWORK_CONTENT_ENTRIES.contains(&name) {
            if let Some(kind) = entry.data.as_ref().ok().and_then(|c| iwork_type(c.bytes())) {
                return Ok(Some(kind));
            }
        }
        if let Some(kind) = iwork18(name).or_else(|| iwork13(name)) {
            return Ok(Some(kind));
        }
        let patterns = self.ipa_patterns.get_or_insert_with(ipa_patterns);
        patterns.retain(|matches| !matches(name));
        if patterns.is_empty() {
            return Ok(Some(MediaType::named(IPA)));
        }
        if name == "AndroidManifest.xml" {
            return Ok(Some(MediaType::named(
                "application/vnd.android.package-archive",
            )));
        }
        if name == "base/manifest/AndroidManifest.xml" {
            return Ok(Some(MediaType::named("application/x-authorware-bin")));
        }
        if name == "META-INF/MANIFEST.MF" {
            self.jar_manifest = true;
        }
        if self.jar_manifest && name == "WEB-INF/" {
            return Ok(Some(MediaType::named(
                "application/x-tika-java-web-archive",
            )));
        }
        if self.jar_manifest && name == "META-INF/application.xml" {
            return Ok(Some(MediaType::named(
                "application/x-tika-java-enterprise-archive",
            )));
        }
        if is_top_level_kml(name) {
            *self.kml_count.get_or_insert(0) += 1;
        }
        if name == "mimetype" {
            let content = bounded(&entry.data, 1024)?;
            if !content.is_empty() {
                if let Some(kind) = MediaType::parse(&String::from_utf8_lossy(content)) {
                    return Ok(Some(kind));
                }
            }
        }
        if name == "META-INF/manifest.xml" {
            let content = bounded(&entry.data, 20 * 1024 * 1024)?;
            if let Some(kind) = star_office_type(content) {
                return Ok(Some(kind));
            }
        }
        let parts = self.frictionless.get_or_insert_with(HashSet::new);
        parts.extend(frictionless_part(name));
        if frictionless_type(parts) == Some(WACZ) {
            return Ok(Some(MediaType::named(WACZ)));
        }
        self.geogebra
            .get_or_insert_with(GeoGebra::default)
            .update(name);
        Ok(None)
    }

    /// The detectors' `streamingDetectFinal`, in order, else a plain ZIP archive.
    fn finish(&self) -> MediaType {
        if self.ooxml_hints > 2 {
            return MediaType::named("application/x-tika-ooxml");
        }
        if let Some(names) = &self.iwork_names {
            if names.contains("Index/Document.iwa") {
                return MediaType::named(IWORK13_UNKNOWN);
            }
            if names.contains("buildVersionHistory.plist") {
                return MediaType::named("application/vnd.apple.iwork");
            }
        }
        if self.jar_manifest {
            return MediaType::named("application/java-archive");
        }
        if self.kml_count == Some(1) {
            return MediaType::named(KMZ);
        }
        if let Some(kind) = self.frictionless.as_ref().and_then(frictionless_type) {
            return MediaType::named(kind);
        }
        self.geogebra
            .as_ref()
            .and_then(GeoGebra::decide)
            .unwrap_or_else(|| MediaType::named("application/zip"))
    }
}

/// What copying an entry through a `BoundedInputStream` of `limit` bytes gives, or what it
/// throws.
fn bounded(data: &Result<Content, Failure>, limit: usize) -> Result<&[u8], Failure> {
    match data {
        Ok(Content::Complete(bytes)) => Ok(&bytes[..bytes.len().min(limit)]),
        Ok(Content::Truncated(bytes)) if bytes.len() >= limit => Ok(&bytes[..limit]),
        Ok(Content::Truncated(_)) => Err(Failure::Io),
        Err(failure) => Err(*failure),
    }
}

/// What reading an archive entry by entry gives.
enum Streamed {
    Found(MediaType),
    /// A stored entry with a data descriptor stopped a stream that does not allow them.
    NeedsDataDescriptors,
}

/// `detectStreamingFromChannel`: a data descriptor the stream does not allow is returned for
/// the caller to retry; any other failure ends in the final detection.
fn by_stream(data: &[u8], allow_stored_data_descriptor: bool) -> Streamed {
    let mut stream = ZipStream::new(data, allow_stored_data_descriptor);
    let mut context = Streaming::default();
    loop {
        let step = stream.next_entry().and_then(|entry| match entry {
            None => Ok(None),
            Some(entry) => context.update(&entry).map(Some),
        });
        match step {
            Ok(None) => return Streamed::Found(context.finish()),
            Ok(Some(Some(kind))) => return Streamed::Found(kind),
            Ok(Some(None)) => {}
            Err(Failure::DataDescriptor) if !allow_stored_data_descriptor => {
                return Streamed::NeedsDataDescriptors
            }
            Err(_) => return Streamed::Found(context.finish()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn detected(data: &[u8]) -> String {
        match detect(data) {
            Detected::Zip(found) => found.media_type.to_string(),
            Detected::Other(found) => found.map_or("none".into(), |t| t.to_string()),
        }
    }

    #[test]
    fn names_zip_based_formats() {
        assert_eq!(
            detected(&zip(&[(
                "mimetype",
                b"application/vnd.oasis.opendocument.text"
            )])),
            "application/vnd.oasis.opendocument.text"
        );
        assert_eq!(
            detected(&zip(&[("META-INF/MANIFEST.MF", b"x")])),
            "application/java-archive"
        );
        assert_eq!(detected(&zip(&[("doc.kml", b"<kml/>")])), KMZ);
        assert_eq!(detected(&zip(&[("a.txt", b"x")])), "application/zip");
        assert_eq!(
            detected(&zip(&[
                ("datapackage.json", b"{}"),
                ("archive/a.warc", b"")
            ])),
            WACZ
        );
    }

    #[test]
    fn reads_a_broken_archive_entry_by_entry() {
        let data = zip(&[("META-INF/MANIFEST.MF", b"x"), ("b", b"y")]);
        let cut = &data[..data.len() - 30];
        let Detected::Zip(found) = detect(cut) else {
            panic!("a zip");
        };
        assert!(!found.opened);
        assert_eq!(found.media_type.as_str(), "application/java-archive");
    }

    #[test]
    fn recognizes_other_archives_and_compressors() {
        assert_eq!(detected(b"BZh91AY&SY"), "application/x-bzip2");
        assert_eq!(detected(b"II\x2a\x00\x08\x00"), "image/tiff");
        assert_eq!(detected(b"plain"), "none");
    }

    #[test]
    fn matches_ipa_and_slide_names() {
        assert!(is_slide_xml("_slide12/geogebra.xml"));
        assert!(!is_slide_xml("_slide/geogebra.xml"));
        let patterns = ipa_patterns();
        assert!(patterns[1]("Payload/My.app/"));
        assert!(patterns[4]("Payload/My.app/Info.plist"));
        assert!(!patterns[4]("Payload/My.app/Other.plist"));
    }
}
