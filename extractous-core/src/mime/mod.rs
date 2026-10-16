//! A port of the media type detection extractous runs through Tika 4.1: `new DefaultDetector()`
//! on the native image's class path, as `TikaNativeMain.detect` calls it.
//!
//! `DefaultDetector` asks `MimeTypes` first (the magic, names and XML roots of
//! `tika-mimetypes.xml` and extractous's `custom-mimetypes.xml`), records that answer as
//! `tk:content-type-magic-detected`, then runs the detectors the class path provides, in the
//! order `ServiceLoaderUtils` sorts them: binary property lists, gzip web archives, raw camera
//! TIFFs, OLE2 documents by their streams, Matroska, Ogg, other OLE2 documents, and ZIP-based and
//! other archives. A later detector's answer replaces an earlier one only when it specializes
//! it. When neither the magic nor the detectors know the document, the first 512 bytes' statistics
//! decide whether it is plain text. The most specific of the magic's and the detectors' answers
//! wins.
//!
//! The answers match Tika's own, which `tests/fixtures/mime/mime-reference.tsv` holds for every
//! document of the parity corpora.

mod archive;
mod detectors;
mod java_regex;
mod magic;
mod media_type;
mod mime_types;
mod registry;
mod text;
mod uri;

#[cfg(test)]
pub(crate) use crate::ole2::tests::compound as compound_document;
pub(crate) use detectors::ole::container_type as ole_container_type;
pub(crate) use detectors::ole::directory_type as ole_directory_type;
pub(crate) use detectors::ole::is_ole10_native;
use detectors::{bplist, gzip, matroska, ogg, ole, raw_tiff, zip, Thrown};
pub(crate) use media_type::MediaType;
use mime_types::{mime_types, MIN_LENGTH};

/// What Tika knows of a document before it detects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin<'a> {
    /// A file, opened as `TikaInputStream.get(path, metadata)` opens it: its name is the resource
    /// name and its length is already in the metadata.
    File { name: &'a str },
    /// Bytes streamed without a name, whose length the metadata learns only if a detector reads
    /// them all.
    Bytes,
    /// An embedded document, streamed as bytes are, with the resource name and `Content-Type`
    /// its embedding parser gave it, which hint at its type. Its metadata learns its length when
    /// a detector reads it all only if the stream was opened with that metadata.
    Embedded {
        name: Option<&'a str>,
        content_type: Option<&'a str>,
        learns_length: bool,
    },
}

/// A metadata key the detection writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    /// `tk:content-type-magic-detected`: what `MimeTypes` answered.
    MagicDetected,
    /// `Content-Length`, written when a container detector reads a stream of bytes to its end.
    ContentLength,
    /// `zip:detector-zip-file-opened`.
    ZipFileOpened,
    /// `zip:detector-data-descriptor-required`.
    DataDescriptorRequired,
}

impl Key {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Key::MagicDetected => "tk:content-type-magic-detected",
            Key::ContentLength => "Content-Length",
            Key::ZipFileOpened => "zip:detector-zip-file-opened",
            Key::DataDescriptorRequired => "zip:detector-data-descriptor-required",
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
pub(crate) struct Detection {
    pub(crate) media_type: MediaType,
    pub(crate) metadata: Vec<Entry>,
}

/// Why Tika's detection throws instead of answering.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DetectionError {
    /// The `IOException` `TikaNativeMain` reports as an input it could not open.
    #[error("Tika's {0} detector fails on the input with an IOException")]
    Unreadable(&'static str),
    /// A `RuntimeException`, which escapes `TikaNativeMain` altogether.
    #[error("Tika's {0} detector fails on the input with a RuntimeException")]
    Crashes(&'static str),
}

fn thrown(detector: &'static str) -> impl Fn(Thrown) -> DetectionError {
    move |thrown| match thrown {
        Thrown::Io => DetectionError::Unreadable(detector),
        Thrown::Runtime => DetectionError::Crashes(detector),
    }
}

const OCTET_STREAM: &str = "application/octet-stream";

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
            Origin::Embedded { learns_length, .. } => learns_length,
        };
        if learns_length && !self.spooled {
            self.spooled = true;
            self.set(Key::ContentLength, self.length.to_string());
        }
    }
}

/// `MimeTypes.forName(type).getExtension()` of the default types: the first extension of the
/// type, with its dot, or nothing.
pub(crate) fn extension(media_type: &MediaType) -> &'static str {
    mime_types().extension(media_type)
}

/// The types `CompositeParser.getParser` looks a parser up for, in order, given a document's
/// type: the canonical type the default types' registry normalizes it to, then each supertype
/// `getSupertype` gives, to `application/octet-stream`.
pub(crate) fn lineage(media_type: &MediaType) -> impl Iterator<Item = MediaType> {
    let registry = mime_types().registry();
    std::iter::successors(Some(registry.normalize(media_type)), |kind| {
        registry.supertype(kind)
    })
}

/// Whether the default types declare `kind`, or its base type, as a type or an alias.
pub(crate) fn declares(kind: &MediaType) -> bool {
    mime_types().registry().declares(kind)
}

/// Every type the default types declare.
#[cfg(test)]
pub(crate) fn known_types() -> impl Iterator<Item = &'static MediaType> {
    mime_types().known_types()
}

/// `new DefaultZipContainerDetector().detect`: the type of a ZIP archive by its entries, of an
/// archive or compression format by its first bytes, or `application/octet-stream`.
pub(crate) fn zip_container_type(data: &[u8]) -> MediaType {
    match zip::detect(data) {
        zip::Detected::Zip(found) => found.media_type,
        zip::Detected::Other(found) => found.unwrap_or_else(|| MediaType::named(OCTET_STREAM)),
    }
}

/// `DefaultDetector.detect` on a document, as `TikaNativeMain.detect` runs it.
pub(crate) fn detect(data: &[u8], origin: Origin<'_>) -> Result<Detection, DetectionError> {
    let types = mime_types();
    let registry = types.registry();
    let (name, content_type) = match origin {
        Origin::File { name } => (Some(name), None),
        Origin::Bytes => (None, None),
        Origin::Embedded {
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
    answers.push(match zip::detect(data) {
        zip::Detected::Other(found) => found,
        zip::Detected::Zip(found) => {
            written.spool(origin);
            written.set(Key::ZipFileOpened, found.opened.to_string());
            if found.data_descriptor_required {
                written.set(Key::DataDescriptorRequired, "true".to_owned());
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
