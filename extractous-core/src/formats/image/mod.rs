//! Images, as Tika's image parsers read them for their metadata, under the OCR enricher
//! (`PreparingOcrParser`) `TikaNativeMain` injects when tesseract runs: `AbstractImageParser`'s
//! flow, the XMP `ImageXmp` reads first, metadata-extractor's directories copied by
//! `ImageMetadataExtractor`, and the video of a motion photo as an embedded document.
//!
//! An image the enricher would read with tesseract is left to Tika's path until OCR is ported:
//! with OCR skipped, the enricher runs and records itself but reads nothing, which the port
//! does too.

mod extractor;
mod image_xmp;
mod imageio;
mod metadata_extractor;
mod motion_photo;
mod psd;
#[cfg(test)]
mod tests;

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::java_time::TimeZone;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Nested, Stop, Xhtml};
use crate::formats::{java_file_name, Declined, Failure, InputFile, Parser, Prepared, Unported};
use extractor::Target;
use metadata_extractor::{exif, heif, jpeg, webp, Directory, Kind, Metadata, Value};

/// The name the port records the OCR enricher under, as Tika records `ai.yobix.PreparingOcrParser`.
pub(crate) const PREPARING_OCR_PARSER: &str = "extractous::PreparingOcrParser";

/// The types `PreparingOcrParser` reads, for which Tika's image parsers run it as their enricher:
/// those of `TesseractParser`, of `RecognizedImageParser`, the ones decoded for tesseract and the
/// ones libheif decodes.
const OCR_TYPES: [&str; 21] = [
    "image/png",
    "image/jpeg",
    "image/tiff",
    "image/bmp",
    "image/gif",
    "image/jp2",
    "image/jpx",
    "image/x-portable-pixmap",
    "image/webp",
    "image/x-portable-bitmap",
    "image/x-portable-graymap",
    "image/x-portable-anymap",
    "image/vnd.adobe.photoshop",
    "image/vnd.zbrush.pcx",
    "image/x-tga",
    "image/x-jbig2",
    "image/heic",
    "image/heic-sequence",
    "image/heif",
    "image/heif-sequence",
    "image/avif",
];

/// `ContentEnrichers.MIN_PIXELS`: an image narrower or lower than this gets no enricher.
const MIN_PIXELS: i32 = 2;

/// Whether Tika hands an image of `content_type` to the OCR enricher: whether tesseract runs and
/// the enricher reads the type.
fn enriched(content_type: &str) -> bool {
    let base = content_type.split(';').next().unwrap_or_default().trim();
    crate::ocr::tesseract_runs() && OCR_TYPES.contains(&base)
}

/// Defers an image the enricher would read, until OCR is ported.
fn without_ocr(enricher: bool, configs: &ParserConfigs) -> Result<(), Unported> {
    if enricher && !configs.ocr.skips_ocr() {
        return Err(Unported::new(
            "an image Tika's path reads with tesseract, whose OCR the port does not do yet",
        ));
    }
    Ok(())
}

fn java_zone() -> Result<&'static TimeZone, Unported> {
    TimeZone::java_default().map_err(|e| {
        Unported::new(format!(
            "dates in Java's default time zone, which the port cannot find: {e}"
        ))
    })
}

/// The image formats metadata-extractor reads for Tika.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Jpeg,
    Tiff,
    /// An image `ImageParser` reads with ImageIO.
    ImageIo,
    Heif,
}

/// Tika's `JpegParser`.
pub(crate) struct JpegParser;

impl Parser for JpegParser {
    fn name(&self) -> &'static str {
        "extractous::JpegParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/jpeg"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare(Format::Jpeg, input, metadata, configs)
    }
}

/// Tika's `TiffParser`.
pub(crate) struct TiffParser;

impl Parser for TiffParser {
    fn name(&self) -> &'static str {
        "extractous::TiffParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/tiff"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare(Format::Tiff, input, metadata, configs)
    }
}

/// Tika's `HeifParser`.
pub(crate) struct HeifParser;

impl Parser for HeifParser {
    fn name(&self) -> &'static str {
        "extractous::HeifParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "image/heif",
            "image/heif-sequence",
            "image/heic",
            "image/heic-sequence",
            "image/avif",
        ]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare(Format::Heif, input, metadata, configs)
    }
}

/// Tika's `ImageParser`, of the types whose ImageIO readers the port reads as they do and of
/// those ImageIO has no reader for.
pub(crate) struct ImageParser;

impl Parser for ImageParser {
    fn name(&self) -> &'static str {
        "extractous::ImageParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "image/png",
            "image/gif",
            "image/bmp",
            "image/vnd.wap.wbmp",
            "image/vnd.microsoft.icon",
            "image/x-xcf",
            "image/jp2",
            "image/jpx",
        ]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare(Format::ImageIo, input, metadata, configs)
    }
}

/// `ImageParser.extractMetadata`: the reader ImageIO picks for the type reads the image's
/// count, size and metadata trees, and the comment and bits per sample are mapped to Tika's.
fn image_io(input: &[u8], mut metadata: TikaMetadata) -> Result<Read, Declined> {
    let content_type = metadata.get("Content-Type").unwrap_or_default().to_owned();
    let parse_error = |cause: &str| fails(format!("{content_type} parse error: {cause}"));
    let read = match content_type.as_str() {
        "image/png" => imageio::png::read(input).map_err(|error| match error {
            imageio::png::PngError::Image(cause) => parse_error(&cause),
            imageio::png::PngError::Runtime(cause) => fails(cause),
            imageio::png::PngError::Unported(unported) => unported.into(),
        })?,
        "image/gif" => match imageio::gif::read(input) {
            Ok(imageio::gif::Outcome::Read(read)) => read,
            Ok(imageio::gif::Outcome::UnexpectedBlockZero { images }) => {
                metadata.set("tk:num-images", images.to_string());
                return Ok(Read {
                    metadata,
                    prefix_route_names: 0,
                    reads_file: false,
                });
            }
            Err(imageio::gif::GifError::Image(cause)) => return Err(parse_error(&cause)),
            Err(imageio::gif::GifError::Runtime(cause)) => return Err(fails(cause)),
        },
        "image/bmp" | "image/vnd.wap.wbmp" => {
            return Err(Unported::new(format!(
                "an image of {content_type}, which Tika's ImageIO reads with the JDK's reader or \
                 jai-imageio's, picked at random in each process, so its metadata has no one form"
            ))
            .into())
        }
        _ => {
            return Ok(Read {
                metadata,
                prefix_route_names: 0,
                reads_file: false,
            })
        }
    };
    metadata.set("tk:num-images", read.images.to_string());
    metadata.set(extractor::IMAGE_WIDTH.name, read.width.to_string());
    metadata.set(extractor::IMAGE_LENGTH.name, read.height.to_string());
    let mut target = Target::new(&mut metadata);
    target.add_prefixed(extractor::UNKNOWN_IMG, "height", &read.height.to_string());
    target.add_prefixed(extractor::UNKNOWN_IMG, "width", &read.width.to_string());
    for tree in &read.trees {
        imageio::load_node(&mut target, tree, "", false);
    }
    let prefix_route_names = target.prefix_route_names();
    for (key, property) in [
        (
            "img:CommentExtensions CommentExtension",
            &extractor::COMMENTS,
        ),
        ("img:markerSequence com", &extractor::COMMENTS),
        ("img:Data BitsPerSample", &extractor::BITS_PER_SAMPLE),
    ] {
        if let Some(value) = metadata.get(key) {
            let value = match value.strip_suffix(' ') {
                Some(stripped) => stripped.to_owned(),
                None => value.to_owned(),
            };
            crate::formats::xmp::set(&mut metadata, property, &value);
        }
    }
    Ok(Read {
        metadata,
        prefix_route_names,
        reads_file: false,
    })
}

/// Our Java `RecognizedImageParser`: the parser Tika has for the image's type writes a document
/// of its own into the image's, and the image is then handed to the OCR enricher.
pub(crate) struct RecognizedImageParser;

/// The parser Tika has for a type `RecognizedImageParser` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recognized {
    WebP,
    Photoshop,
    /// `EmptyParser`, for the types Tika has no parser of.
    Empty,
}

impl Recognized {
    fn of(content_type: &str) -> Self {
        match content_type.split(';').next().unwrap_or_default().trim() {
            "image/webp" => Self::WebP,
            "image/vnd.adobe.photoshop" => Self::Photoshop,
            _ => Self::Empty,
        }
    }
}

impl Parser for RecognizedImageParser {
    fn name(&self) -> &'static str {
        "extractous::RecognizedImageParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "image/webp",
            "image/vnd.adobe.photoshop",
            "image/x-portable-bitmap",
            "image/x-portable-graymap",
            "image/x-portable-anymap",
            "image/x-portable-arbitrarymap",
            "image/vnd.zbrush.pcx",
            "image/x-tga",
        ]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let content_type = metadata.get("Content-Type").unwrap_or_default();
        let recognizer = enriched(content_type);
        without_ocr(recognizer, configs)?;
        let mut read = metadata.clone();
        let file_directory = match Recognized::of(content_type) {
            Recognized::WebP => Some(webp_metadata(input, &mut read)?),
            Recognized::Photoshop => {
                psd::read(input, &mut read).map_err(|error| match error {
                    psd::PsdError::Fails(message) => fails(message),
                    psd::PsdError::Unported(unported) => unported.into(),
                })?;
                None
            }
            Recognized::Empty => None,
        };
        Ok(Box::new(move |xhtml| {
            *xhtml.metadata_mut() = read;
            if let Some((zone, prefix_route_names)) = file_directory {
                add_file_directory(xhtml, input.len(), true, zone, prefix_route_names)?;
            }
            let context = xhtml.context();
            let metadata = xhtml.metadata_mut().clone();
            let (_, _, nested) =
                Xhtml::write(
                    &mut Nested(xhtml),
                    metadata,
                    context,
                    |_| Ok::<(), Stop>(()),
                );
            nested?;
            if recognizer {
                enrich(xhtml);
            }
            Ok(())
        }))
    }
}

/// `WebPParser`'s metadata: its XMP, then metadata-extractor's directories, which end with the
/// file's, in the zone and after the prefix route names this gives.
fn webp_metadata(
    input: &[u8],
    read: &mut TikaMetadata,
) -> Result<(&'static TimeZone, usize), Declined> {
    let zone = java_zone()?;
    image_xmp::extract_webp(input, read)?;
    let mut directories = Metadata::default();
    match webp::read(input, &mut directories) {
        Ok(()) => {}
        Err(webp::RiffError::Processing(message)) => {
            return Err(fails(format!("Can't process Riff data: {message}")))
        }
        Err(webp::RiffError::End(bounds)) => return Err(fails(bounds.0)),
        Err(webp::RiffError::Unported(unported)) => return Err(unported.into()),
    }
    let mut target = Target::new(read);
    extractor::handle(&directories, &mut target, zone)?;
    Ok((zone, target.prefix_route_names()))
}

/// The directory of the file metadata-extractor reads, when it reads one, copied as the last of
/// the image's directories.
fn add_file_directory(
    xhtml: &mut Xhtml,
    length: usize,
    spools: bool,
    zone: &'static TimeZone,
    prefix_route_names: usize,
) -> Result<(), Failure> {
    let Some(file) = input_file(xhtml, length, spools).map_err(Failure::Extraction)? else {
        return Ok(());
    };
    let mut directories = Metadata::default();
    directories.add(file.directory(zone));
    let mut target = Target::continuing(xhtml.metadata_mut(), prefix_route_names);
    extractor::handle(&directories, &mut target, zone)
        .expect("the file's directory describes every tag");
    Ok(())
}

/// A failure Tika throws reading an image's metadata: before the document starts without the
/// enricher, and once it has ended with it, either way failing the extraction.
fn fails(message: String) -> Declined {
    Declined::Fails(Error::ParseError(message))
}

/// The metadata `extractMetadata` gives an image, up to the directory of the file it reads,
/// which the document's file decides when it is written.
struct Read {
    metadata: TikaMetadata,
    prefix_route_names: usize,
    /// Whether metadata-extractor read it, which reads the file too.
    reads_file: bool,
}

/// `AbstractImageParser.parse` of a JPEG or TIFF: its metadata read now, the file's own when the
/// document is written, then the enricher and the motion photo's video.
fn prepare<'a>(
    format: Format,
    input: &'a [u8],
    metadata: &TikaMetadata,
    configs: &'a ParserConfigs,
) -> Result<Prepared<'a>, Declined> {
    let enricher = enriched(metadata.get("Content-Type").unwrap_or_default());
    without_ocr(enricher, configs)?;
    let zone = java_zone()?;
    let read = extract_metadata(format, input, metadata.clone(), zone)?;
    let trailer = motion_photo::trailer(&read.metadata, input)?;
    Ok(Box::new(move |xhtml| {
        *xhtml.metadata_mut() = read.metadata;
        if read.reads_file {
            add_file_directory(xhtml, input.len(), enricher, zone, read.prefix_route_names)?;
        }
        if enricher {
            enrich(xhtml);
        }
        if let Some(trailer) = trailer {
            xhtml.parse_embedded(
                &input[trailer.start..],
                &trailer.metadata,
                Wrapping::PackageEntry,
            )?;
        }
        Ok(())
    }))
}

/// `extractMetadata` of a JPEG, TIFF or HEIF image: its XMP, then metadata-extractor's
/// directories copied, which for the first two end with the directory of the file it reads.
fn extract_metadata(
    format: Format,
    input: &[u8],
    mut metadata: TikaMetadata,
    zone: &'static TimeZone,
) -> Result<Read, Declined> {
    let mut directories = Metadata::default();
    match format {
        Format::Jpeg => {
            image_xmp::extract_jpeg(input, &mut metadata)?;
            let wanted = |kind| jpeg::read_by_tika(kind);
            let segments =
                jpeg::read_segments(input, Some(&wanted)).map_err(|error| match error {
                    jpeg::SegmentError::Processing(message) => {
                        fails(format!("Can't read JPEG metadata: {message}"))
                    }
                    jpeg::SegmentError::End(bounds) => fails(bounds.0),
                })?;
            jpeg::read(&segments, &mut directories)?;
        }
        Format::ImageIo => return image_io(input, metadata),
        Format::Heif => {
            if !image_xmp::extract_heif(input, &mut metadata)? {
                image_xmp::scan_and_extract(input, &mut metadata)?;
            }
            heif::read(input, &mut directories)?;
        }
        Format::Tiff => {
            image_xmp::scan_and_extract(input, &mut metadata)?;
            match exif::read_tiff(input, 0, &mut directories) {
                Ok(()) => {}
                Err(exif::TiffError::Processing(message)) => {
                    return Err(fails(format!("Can't read TIFF metadata: {message}")))
                }
                Err(exif::TiffError::Bounds(bounds)) => return Err(fails(bounds.0)),
                Err(exif::TiffError::Unported(unported)) => return Err(unported.into()),
            }
        }
    }
    let mut target = Target::new(&mut metadata);
    extractor::handle(&directories, &mut target, zone)?;
    let prefix_route_names = target.prefix_route_names();
    Ok(Read {
        metadata,
        prefix_route_names,
        reads_file: format != Format::Heif,
    })
}

/// The file metadata-extractor reads an image from, its name, size and time of modification:
/// the document's own file, or with the enricher the temporary file Tika spools a document read
/// from memory to; none for a document read from memory without it.
struct FileFacts {
    name: String,
    size: i64,
    modified: i64,
}

impl FileFacts {
    /// `FileSystemMetadataReader.read`.
    fn directory(&self, zone: &'static TimeZone) -> Directory {
        let mut directory = Directory::new(Kind::FileSystem);
        directory.set(1, Value::Text(self.name.clone()));
        directory.set(2, Value::Long(self.size));
        directory.set(3, Value::Date(self.modified, zone));
        directory
    }
}

/// The file metadata-extractor reads: the document's own, or when Tika `spools` a document read
/// from memory before reading it, the temporary file it spools to.
fn input_file(xhtml: &Xhtml, length: usize, spools: bool) -> Result<Option<FileFacts>, Error> {
    match xhtml.context().input_file() {
        InputFile::Original(path) => {
            let unreadable = |e: std::io::Error| {
                Error::IoError(format!("Could not read the file facts of {path}: {e}"))
            };
            let facts = fs::metadata(path).map_err(unreadable)?;
            let modified = facts.modified().map_err(unreadable)?;
            Ok(Some(FileFacts {
                name: java_file_name(path).to_owned(),
                size: facts.len() as i64,
                modified: epoch_millis(modified),
            }))
        }
        InputFile::Spooled if spools => Ok(Some(FileFacts {
            name: temporary_name(),
            size: length as i64,
            modified: epoch_millis(SystemTime::now()),
        })),
        InputFile::Spooled => Ok(None),
    }
}

/// `File.lastModified`: whole milliseconds since the epoch.
fn epoch_millis(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_millis() as i64,
        Err(before) => -(before.duration().as_millis() as i64),
    }
}

/// The name of a temporary file Tika spools to, `TemporaryResources.createTempFile`'s
/// `apache-tika-<random>.tmp`.
fn temporary_name() -> String {
    use std::hash::{BuildHasher, RandomState};
    format!(
        "apache-tika-{}.tmp",
        RandomState::new().hash_one(SystemTime::now())
    )
}

/// `GuardedEnricher.parse` with OCR skipped: the enricher is recorded unless the image is too
/// small for it, and reads nothing.
fn enrich(xhtml: &mut Xhtml) {
    let metadata = xhtml.metadata_mut();
    let dimension = |name: &str| metadata.get(name).and_then(crate::formats::java::parse_int);
    let too_small = dimension(extractor::IMAGE_WIDTH.name).is_some_and(|width| width < MIN_PIXELS)
        || dimension(extractor::IMAGE_LENGTH.name).is_some_and(|height| height < MIN_PIXELS);
    if !too_small {
        xhtml.delegate_to(PREPARING_OCR_PARSER);
    }
}
