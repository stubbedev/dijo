//! Images, as Tika's image parsers read them for their metadata, under the OCR enricher
//! (`PreparingOcrParser`) `TikaNativeMain` injects when tesseract runs: `AbstractImageParser`'s
//! flow, the XMP `ImageXmp` reads first, metadata-extractor's directories copied by
//! `ImageMetadataExtractor`, and the video of a motion photo as an embedded document.
//!
//! The enricher reads the image with tesseract, natively too; with OCR skipped it records itself
//! but reads nothing.

mod bpg;
pub mod enricher;
mod extractor;
mod icns;
mod image_xmp;
mod imageio;
mod jxl;
mod metadata_extractor;
mod motion_photo;
mod psd;
mod raw;
#[cfg(test)]
mod tests;

use std::fs;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{java_file_name, Declined, Failure, InputFile, Parser, Prepared, Unported};
use crate::java::time::TimeZone;
use enricher::Enricher;
use extractor::Target;
pub use icns::IcnsParser;
use metadata_extractor::{exif, heif, jpeg, webp, Directory, Kind, Metadata, Value};
pub use raw::RawTiffParser;

/// The name the port records the OCR enricher under, as Tika records `ai.yobix.PreparingOcrParser`.
pub const PREPARING_OCR_PARSER: &str = "extractous::PreparingOcrParser";
/// The OCR enricher's names, the port's paired with Tika's.
const TIKA_DELEGATES: [(&str, &str); 1] = [(PREPARING_OCR_PARSER, "ai.yobix.PreparingOcrParser")];

/// The types the OCR enricher reads: those `PreparingOcrParser` reads, for which Tika's image
/// parsers run it as their enricher, of `TesseractParser`, of `RecognizedImageParser`, the ones
/// decoded for tesseract and the ones libheif decodes; and JPEG XL, which Tika does not read
/// (#132).
const OCR_TYPES: [&str; 22] = [
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
    PORTABLE_ANYMAP,
    "image/vnd.adobe.photoshop",
    "image/vnd.zbrush.pcx",
    "image/x-tga",
    "image/x-jbig2",
    "image/heic",
    "image/heic-sequence",
    "image/heif",
    "image/heif-sequence",
    "image/avif",
    "image/jxl",
];

/// `ContentEnrichers.MIN_PIXELS`: an image narrower or lower than this gets no enricher.
const MIN_PIXELS: i32 = 2;

/// Whether Tika hands an image of `content_type` to the OCR enricher: whether tesseract runs and
/// the enricher reads the type.
fn enriched(content_type: &str) -> bool {
    let base = content_type.split(';').next().unwrap_or_default().trim();
    crate::ocr::tesseract_runs() && OCR_TYPES.contains(&base)
}

/// The zone an image's dates are read and written in: UTC, so that a date naming no zone, as
/// Exif's do, keeps the time it gives. Tika reads them in Java's default zone and writes them
/// in UTC, shifting them by the machine's offset (#114).
fn dates_zone() -> &'static TimeZone {
    static UTC: LazyLock<TimeZone> = LazyLock::new(|| TimeZone::fixed(0));
    &UTC
}

/// The image formats metadata-extractor reads for Tika.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Jpeg,
    Tiff,
    /// An image `ImageParser` reads with ImageIO.
    ImageIo,
    Heif,
    Bpg,
    /// JPEG XL, which Tika does not read (#132).
    Jxl,
}

/// Tika's `JpegParser`.
pub struct JpegParser;

impl Parser for JpegParser {
    fn name(&self) -> &'static str {
        "extractous::JpegParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.image.JpegParser"
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &TIKA_DELEGATES
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
pub struct TiffParser;

impl Parser for TiffParser {
    fn name(&self) -> &'static str {
        "extractous::TiffParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.image.TiffParser"
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &TIKA_DELEGATES
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
pub struct HeifParser;

impl Parser for HeifParser {
    fn name(&self) -> &'static str {
        "extractous::HeifParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.image.HeifParser"
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &TIKA_DELEGATES
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

/// JPEG XL images, in place of Tika's `JXLParser`, which scans one for an XMP packet and writes
/// no document (#132): the image's document, with its header's and boxes' metadata, which the
/// OCR enricher reads.
pub struct JxlParser;

impl Parser for JxlParser {
    fn name(&self) -> &'static str {
        "extractous::JXLParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.image.JXLParser"
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &TIKA_DELEGATES
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/jxl"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare(Format::Jxl, input, metadata, configs)
    }
}

/// Tika's `BPGParser`.
pub struct BpgParser;

impl Parser for BpgParser {
    fn name(&self) -> &'static str {
        "extractous::BPGParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.image.BPGParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/x-bpg", "image/bpg"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        prepare(Format::Bpg, input, metadata, configs)
    }
}

/// Tika's `ImageParser`, of the types whose ImageIO readers the port reads as they do and of
/// those ImageIO has no reader for.
pub struct ImageParser;

impl Parser for ImageParser {
    fn name(&self) -> &'static str {
        "extractous::ImageParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.image.ImageParser"
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &TIKA_DELEGATES
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "image/png",
            "image/gif",
            "image/bmp",
            "image/vnd.wap.wbmp",
            "image/vnd.microsoft.icon",
            "image/x-xcf",
            "image/x-jbig2",
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
    // The reader is picked by the type without its parameters, where Tika finds none for a
    // type with them (#114).
    let base = content_type.split(';').next().unwrap_or_default().trim();
    let read = match base {
        "image/png" => imageio::png::read(input).map_err(|error| match error {
            imageio::png::PngError::Image(cause) => parse_error(&cause),
            imageio::png::PngError::Runtime(cause) => fails(cause),
            imageio::png::PngError::Unported(unported) => unported.into(),
        })?,
        "image/gif" => imageio::gif::read(input).map_err(|error| match error {
            imageio::gif::GifError::Image(cause) => parse_error(&cause),
            imageio::gif::GifError::Runtime(cause) => fails(cause),
        })?,
        "image/x-jbig2" => imageio::jbig2::read(input).map_err(|reason| {
            Unported::new(format!(
                "a JBIG2 image jbig2-imageio reads otherwise than the port: {reason}"
            ))
        })?,
        "image/bmp" => imageio::bmp::read(input).map_err(|error| match error {
            imageio::bmp::BmpError::Fails(cause) => fails(cause),
            imageio::bmp::BmpError::Unported(unported) => unported.into(),
        })?,
        "image/vnd.wap.wbmp" => imageio::wbmp::read(input).map_err(|error| match error {
            imageio::wbmp::WbmpError::Image(cause) => parse_error(cause),
            imageio::wbmp::WbmpError::Eof => {
                fails("EOFException reading the WBMP header".to_owned())
            }
        })?,
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

/// Our Java `RecognizedImageParser`: the parser Tika has for the image's type reads its
/// metadata, and the image is then handed to the OCR enricher. Our Java lets that parser write
/// a whole document of its own, `html`, `meta` and `body` elements, into the image's body
/// (#114); the image's own head, which that document followed, shows the metadata as it stood
/// before the enricher.
pub struct RecognizedImageParser;

/// The supertype of the portable bitmap formats.
const PORTABLE_ANYMAP: &str = "image/x-portable-anymap";

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
    fn tika_name(&self) -> &'static str {
        "ai.yobix.RecognizedImageParser"
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &TIKA_DELEGATES
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "image/webp",
            "image/vnd.adobe.photoshop",
            PORTABLE_ANYMAP,
            "image/vnd.zbrush.pcx",
            "image/x-tga",
        ]
    }

    /// The portable bitmaps, graymaps, pixmaps and arbitrary maps, which Tika's composite
    /// parser reads with the parser of their supertype.
    fn media_types_with_subtypes(&self) -> &'static [&'static str] {
        &[PORTABLE_ANYMAP]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        let content_type = metadata.get("Content-Type").unwrap_or_default();
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
        let enricher = Enricher::of(enriched(content_type), content_type, input, configs)?;
        Ok(Box::new(move |xhtml| {
            *xhtml.metadata_mut() = read;
            if let Some((zone, prefix_route_names)) = file_directory {
                add_file_directory(xhtml, input.len(), true, zone, prefix_route_names)?;
            }
            xhtml.start_body()?;
            enricher.enrich(xhtml, input.len())
        }))
    }
}

/// `WebPParser`'s metadata: its XMP, then metadata-extractor's directories, which end with the
/// file's, in the zone and after the prefix route names this gives.
fn webp_metadata(
    input: &[u8],
    read: &mut TikaMetadata,
) -> Result<(&'static TimeZone, usize), Declined> {
    let zone = dates_zone();
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
    let content_type = metadata.get("Content-Type").unwrap_or_default();
    let enriched = enriched(content_type);
    let zone = dates_zone();
    let read = extract_metadata(format, input, metadata.clone(), zone)?;
    let trailer = motion_photo::trailer(&read.metadata, input)?;
    let enricher = Enricher::of(enriched, content_type, input, configs)?;
    Ok(Box::new(move |xhtml| {
        *xhtml.metadata_mut() = read.metadata;
        if read.reads_file {
            add_file_directory(xhtml, input.len(), enriched, zone, read.prefix_route_names)?;
        }
        enricher.enrich(xhtml, input.len())?;
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
        Format::Bpg => return bpg::extract_metadata(input, metadata),
        Format::Jxl => return jxl::extract_metadata(input, metadata),
        Format::Heif => {
            // Tika scans the whole file for a packet where the image has no XMP item (#114).
            image_xmp::extract_heif(input, &mut metadata)?;
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

/// The file metadata-extractor reads an image from, its name, size and time of modification,
/// or of a document read from memory its size alone.
struct FileFacts {
    name: Option<String>,
    size: i64,
    modified: Option<i64>,
}

impl FileFacts {
    /// `FileSystemMetadataReader.read`.
    fn directory(&self, zone: &'static TimeZone) -> Directory {
        let mut directory = Directory::new(Kind::FileSystem);
        if let Some(name) = &self.name {
            directory.set(1, Value::Text(name.clone()));
        }
        directory.set(2, Value::Long(self.size));
        if let Some(modified) = self.modified {
            directory.set(3, Value::Date(modified, zone));
        }
        directory
    }
}

/// The file metadata-extractor reads: the document's own, or when Tika `spools` a document read
/// from memory before reading it, the document's size. Tika describes the temporary file it
/// spools to, `apache-tika-<random>.tmp` modified as it is written (#114).
fn input_file(xhtml: &Xhtml, length: usize, spools: bool) -> Result<Option<FileFacts>, Error> {
    match xhtml.context().input_file() {
        InputFile::Original(path) => {
            let unreadable = |e: std::io::Error| {
                Error::IoError(format!("Could not read the file facts of {path}: {e}"))
            };
            let facts = fs::metadata(path).map_err(unreadable)?;
            let modified = facts.modified().map_err(unreadable)?;
            Ok(Some(FileFacts {
                name: Some(java_file_name(path).to_owned()),
                size: facts.len() as i64,
                modified: Some(epoch_millis(modified)),
            }))
        }
        InputFile::Spooled if spools => Ok(Some(FileFacts {
            name: None,
            size: length as i64,
            modified: None,
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
