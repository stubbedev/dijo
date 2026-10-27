//! A port of the media type detection of Tika 4.1 as dijo's former Java implementation ran
//! it: `new DefaultDetector()` on its class path, as `TikaNativeMain.detect` called it.
//!
//! `DefaultDetector` asks `MimeTypes` first (the magic, names and XML roots of
//! `tika-mimetypes.xml` and dijo's `custom-mimetypes.xml`), records that answer as
//! `tk:content-type-magic-detected`, then runs the detectors the class path provides, in the
//! order `ServiceLoaderUtils` sorts them: binary property lists, gzip web archives, raw camera
//! TIFFs, OLE2 documents by their streams, Matroska, Ogg, other OLE2 documents, and ZIP-based and
//! other archives. A later detector's answer replaces an earlier one only when it specializes
//! it. When neither the magic nor the detectors know the document, the first 512 bytes' statistics
//! decide whether it is plain text. The most specific of the magic's and the detectors' answers
//! wins.
//!
//! The answers match Tika's own, which `tests/fixtures/mime/mime-reference.tsv` holds for every
//! document of the reference corpora.

mod archive;
mod detected;
mod detectors;
mod java_regex;
mod magic;
mod media_type;
mod mime_types;
mod registry;
mod text;
pub mod uri;

#[cfg(any(test, feature = "testing"))]
pub use crate::ole2::tests::compound as compound_document;
pub use detected::{Detected, Headers, OpenedFrom};
pub use detectors::ole::container_type as ole_container_type;
pub use detectors::ole::directory_type as ole_directory_type;
pub use detectors::ole::is_ole10_native;
pub use detectors::zip::{Intent, IworkDocument, IWORK_CONTENT_ENTRIES};
use detectors::{bplist, gzip, matroska, ogg, ole, raw_tiff, zip, Thrown};
pub use media_type::MediaType;
use mime_types::{mime_types, MIN_LENGTH};

/// What Tika knows of a document before it detects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin<'a> {
    /// A file, opened as `TikaInputStream.get(path, metadata)` opens it: its name is the resource
    /// name and its length is already in the metadata.
    File { name: &'a str },
    /// Bytes streamed without a name, whose length the metadata learns only if a detector reads
    /// them all.
    Bytes,
    /// A stream of bytes with the resource name and `Content-Type` its opener gave it, which
    /// hint at its type: an embedded document's embedding parser, or the server a document was
    /// fetched from. Its metadata learns its length when a detector reads it all only if the
    /// stream was opened with that metadata and the length is not in it yet.
    Stream {
        name: Option<&'a str>,
        content_type: Option<&'a str>,
        learns_length: bool,
    },
}

/// A metadata key the detection writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// `tk:content-type-magic-detected`: what `MimeTypes` answered.
    MagicDetected,
    /// `Content-Length`, written when a container detector reads a stream of bytes to its end.
    ContentLength,
    /// `zip:detector-zip-file-opened`.
    ZipFileOpened,
    /// `zip:detector-data-descriptor-required`.
    DataDescriptorRequired,
    /// `zip:salvaged`.
    Salvaged,
}

impl Key {
    pub fn name(self) -> &'static str {
        match self {
            Key::MagicDetected => "tk:content-type-magic-detected",
            Key::ContentLength => "Content-Length",
            Key::ZipFileOpened => "zip:detector-zip-file-opened",
            Key::DataDescriptorRequired => "zip:detector-data-descriptor-required",
            Key::Salvaged => "zip:salvaged",
        }
    }
}

/// A metadata value the detection set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) key: Key,
    pub(crate) value: String,
}

/// A document's media type and the metadata the detection set, in the order it set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub media_type: MediaType,
    pub(crate) metadata: Vec<Entry>,
}

/// Why Tika's detection throws instead of answering.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DetectionError {
    /// An `IOException`, reported as an input that could not be opened.
    #[error("Tika's {0} detector fails on the input with an IOException")]
    Unreadable(&'static str),
    /// A `RuntimeException`, which escapes the detection altogether.
    #[error("Tika's {0} detector fails on the input with a RuntimeException")]
    Crashes(&'static str),
    /// An archive the ZIP detector salvages otherwise than the port.
    #[error("Tika's ZIP detector salvages an archive the port does not: {0}")]
    Unsalvageable(String),
}

fn thrown(detector: &'static str) -> impl Fn(Thrown) -> DetectionError {
    move |thrown| match thrown {
        Thrown::Io => DetectionError::Unreadable(detector),
        Thrown::Runtime => DetectionError::Crashes(detector),
    }
}

const OCTET_STREAM: &str = "application/octet-stream";

/// The value of `key` among `pairs` sorted by their unique keys.
fn sorted_get<V: Copy>(pairs: &[(&'static str, V)], key: &str) -> Option<V> {
    pairs
        .binary_search_by(|(known, _)| (*known).cmp(key))
        .ok()
        .map(|index| pairs[index].1)
}

/// The metadata a detection writes, with the one-time `Content-Length` of a spooled stream.
struct Written {
    entries: Vec<Entry>,
    spooled: bool,
    length: usize,
}

impl Written {
    fn set(&mut self, key: Key, value: String) {
        self.entries.push(Entry { key, value });
    }

    /// A detector read the stream to its end: a stream of bytes learns its length.
    fn spool(&mut self, origin: Origin<'_>) {
        let learns_length = match origin {
            Origin::File { .. } => false,
            Origin::Bytes => true,
            Origin::Stream { learns_length, .. } => learns_length,
        };
        if learns_length && !self.spooled {
            self.spooled = true;
            self.set(Key::ContentLength, self.length.to_string());
        }
    }
}

/// `MimeTypes.forName(type).getExtension()` of the default types: the first extension of the
/// type, with its dot, or nothing.
pub fn extension(media_type: &MediaType) -> &'static str {
    mime_types().extension(media_type)
}

/// The types `CompositeParser.getParser` looks a parser up for, in order, given a document's
/// type: the canonical type the default types' registry normalizes it to, then each supertype
/// `getSupertype` gives, to `application/octet-stream`.
pub fn lineage(media_type: &MediaType) -> impl Iterator<Item = MediaType> {
    let registry = mime_types().registry();
    std::iter::successors(Some(registry.normalize(media_type)), |kind| {
        registry.supertype(kind)
    })
}

/// A type the default types declare, as tika-server's `/mime-types` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub name: &'static str,
    pub aliases: Vec<&'static str>,
    /// What `getSupertype` gives, but never `application/octet-stream`, which every type
    /// specializes.
    pub supertype: Option<MediaType>,
    /// With their dots, the first the type's own.
    pub extensions: &'static [&'static str],
}

/// Every type the default types declare, in order of name.
pub fn declared_types() -> Vec<Declared> {
    let types = mime_types();
    let mut aliases: Vec<(&'static str, &'static str)> = types
        .registry()
        .aliases()
        .map(|(alias, kind)| (kind, alias))
        .collect();
    aliases.sort_unstable();
    let mut names: Vec<&'static str> = types.names().collect();
    names.sort_unstable();
    names
        .into_iter()
        .map(|name| declared(name, &aliases))
        .collect()
}

/// The type the default types declare as `name`, or one of its aliases names.
pub fn declared_type(name: &str) -> Option<Declared> {
    let kind = MediaType::parse(name)?;
    let canonical = mime_types().registry().normalize(&kind.base_type());
    declared_types()
        .into_iter()
        .find(|declared| declared.name == canonical.as_str())
}

fn declared(name: &'static str, aliases: &[(&'static str, &'static str)]) -> Declared {
    let kind = MediaType::named(name);
    let start = aliases.partition_point(|&(of, _)| of < name);
    Declared {
        name,
        aliases: aliases[start..]
            .iter()
            .take_while(|&&(of, _)| of == name)
            .map(|&(_, alias)| alias)
            .collect(),
        supertype: mime_types()
            .registry()
            .supertype(&kind)
            .filter(|supertype| supertype.as_str() != OCTET_STREAM),
        extensions: mime_types().declared_extensions(name).unwrap_or_default(),
    }
}

/// Whether the default types declare `kind`, or its base type, as a type or an alias.
pub fn declares(kind: &MediaType) -> bool {
    mime_types().registry().declares(kind)
}

/// `EmbeddedDocumentUtil.getExtensionForMediaType`: the first extension of the registered type,
/// with its dot, or nothing.
pub fn registered_extension(media_type: &MediaType) -> &'static str {
    mime_types().registered_extension(media_type)
}

/// `new DefaultZipContainerDetector().detect`: the type of a ZIP archive by its entries, of an
/// archive or compression format by its first bytes, or `application/octet-stream`.
pub fn zip_container_type(data: &[u8]) -> MediaType {
    match zip::detect(data, Intent::Detect) {
        Ok(zip::Detected::Zip(found)) => found.media_type,
        Ok(zip::Detected::Other(found)) => found.unwrap_or_else(|| MediaType::named(OCTET_STREAM)),
        Err(_) => unreachable!("only a detection a parse follows salvages"),
    }
}

/// `DefaultDetector.detect` on a document, run alone.
pub fn detect(data: &[u8], origin: Origin<'_>) -> Result<Detection, DetectionError> {
    detect_with(data, origin, Intent::Detect)
}

/// `DefaultDetector.detect` on a document, as `AutoDetectParser` runs it before it parses the
/// document.
pub(crate) fn detect_to_parse(
    data: &[u8],
    origin: Origin<'_>,
) -> Result<Detection, DetectionError> {
    detect_with(data, origin, Intent::Parse)
}

fn detect_with(
    data: &[u8],
    origin: Origin<'_>,
    intent: Intent,
) -> Result<Detection, DetectionError> {
    let types = mime_types();
    let registry = types.registry();
    let (name, content_type) = match origin {
        Origin::File { name } => (Some(name), None),
        Origin::Bytes => (None, None),
        Origin::Stream {
            name, content_type, ..
        } => (name, content_type),
    };
    let magic = types.detect(&data[..data.len().min(MIN_LENGTH)], name, content_type);
    let mut written = Written {
        entries: Vec::new(),
        spooled: false,
        length: data.len(),
    };
    written.set(Key::MagicDetected, magic.to_string());

    let ole_header = ole::is_ole(data);
    let ole_file = if ole_header {
        written.spool(origin);
        ole::OleFile::open(data).ok()
    } else {
        None
    };
    let mut answers: Vec<Option<MediaType>> = vec![
        bplist::detect(data).map_err(thrown("binary property list"))?,
        gzip::detect(data),
        raw_tiff::detect(data).map(MediaType::named),
        ole::poifs(data, ole_file.as_ref()),
        matroska::detect(data),
        ogg::detect(data).map_err(thrown("Ogg"))?,
        ole::misc(data, ole_file.as_ref()),
    ];
    let zip = zip::detect(data, intent)
        .map_err(|zip::Unsalvageable(reason)| DetectionError::Unsalvageable(reason))?;
    answers.push(match zip {
        zip::Detected::Other(found) => found,
        zip::Detected::Zip(found) => {
            written.spool(origin);
            written.set(Key::ZipFileOpened, found.opened.to_string());
            if found.data_descriptor_required {
                written.set(Key::DataDescriptorRequired, "true".to_owned());
            }
            if found.salvaged {
                written.set(Key::Salvaged, "true".to_owned());
            }
            Some(found.media_type)
        }
    });
    let octet = MediaType::named(OCTET_STREAM);
    let mut detected = octet.clone();
    for answer in answers.into_iter().flatten() {
        if registry.is_specialization_of(&answer, &detected) {
            detected = answer;
        }
    }

    let mut best = octet.clone();
    if magic != octet {
        best = magic.clone();
    }
    if detected != octet
        && (best == octet
            || registry.is_specialization_of(&detected, &best)
            || !registry.is_specialization_of(&best, &detected))
    {
        best = detected.clone();
    }
    if best == octet && detected == octet && magic == octet && text::is_text(data, 512) {
        best = MediaType::named("text/plain");
    }
    Ok(Detection {
        media_type: best,
        metadata: written.entries,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod declared_tests {
    use super::*;

    #[test]
    fn declared_types_carry_their_aliases_supertypes_and_extensions() {
        let types = declared_types();
        assert!(types.windows(2).all(|pair| pair[0].name < pair[1].name));
        let pdf = declared_type("application/x-pdf").unwrap();
        assert_eq!(pdf.name, "application/pdf");
        assert_eq!(pdf.aliases, ["application/x-pdf"]);
        assert_eq!(pdf.extensions.first(), Some(&".pdf"));
        assert_eq!(pdf.supertype, None);
        let atom = declared_type("application/atom+xml; charset=utf-8").unwrap();
        assert_eq!(atom.supertype, Some(MediaType::named("application/xml")));
        assert_eq!(declared_type("no/such"), None);
    }
}
