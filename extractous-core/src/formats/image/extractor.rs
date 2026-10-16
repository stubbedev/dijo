//! Tika's `ImageMetadataExtractor`: the directories metadata-extractor read, copied into Tika's
//! metadata by its handlers, in their order for each directory: every tag under `img:`, `icc:`
//! or its Exif directory's name, then the page count, the JPEG comment, the camera's Exif
//! fields, the dimensions, the GPS position and the IPTC fields Tika maps.

use crate::formats::data_format::decimal::Rounding;
use crate::formats::data_format::decimal_format::DecimalFormat;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xmp::{self, bag, text, Property};
use crate::formats::Unported;
use crate::java::simple_date_format::SimpleDateFormat;
use crate::java::time::{self, TimeZone};

use super::metadata_extractor::dates::parse_gps_date;
use super::metadata_extractor::exif::tags::*;
use super::metadata_extractor::{geo, icc, iptc, jpeg, Directory, Kind, Metadata, Value};

pub(crate) const UNKNOWN_IMG: &str = "img:";
const ICC: &str = "icc:";
/// `Metadata.MAX_PREFIX_ROUTE_NAME_LENGTH`.
const MAX_PREFIX_ROUTE_NAME_LENGTH: usize = 1024;
/// `Metadata.MAX_PREFIX_ROUTE_NAMES`.
const MAX_PREFIX_ROUTE_NAMES: usize = 10_000;
const GEO_DECIMAL_FORMAT: &str = "#.######";

const ICC_CURV: i32 = 0x6375_7276;
const ICC_PARA: i32 = 0x7061_7261;
const ICC_MFT1: i32 = 0x6D66_7431;
const ICC_MFT2: i32 = 0x6D66_7432;
const ICC_MAB: i32 = 0x6D41_4220;
const ICC_MBA: i32 = 0x6D42_4120;

pub(crate) const COMMENTS: Property = bag("w:Comments");
pub(crate) const DESCRIPTION: Property = bag("dc:description");
const TITLE: Property = xmp::DC_TITLE;
const CREATOR: Property = xmp::DC_CREATOR;
const SUBJECT: Property = xmp::DC_SUBJECT;
const CREATED: Property = text("dcterms:created");
const MODIFIED: Property = text("dcterms:modified");
const LATITUDE: Property = text("geo:lat");
const LONGITUDE: Property = text("geo:long");
const ALTITUDE: Property = text("geo:alt");
const GEO_TIMESTAMP: Property = text("geo:timestamp");
pub(crate) const IMAGE_WIDTH: Property = text("tiff:ImageWidth");
pub(crate) const IMAGE_LENGTH: Property = text("tiff:ImageLength");
pub(crate) const BITS_PER_SAMPLE: Property = bag("tiff:BitsPerSample");
const SAMPLES_PER_PIXEL: Property = text("tiff:SamplesPerPixel");
const FLASH_FIRED: Property = text("exif:Flash");
const EXPOSURE_TIME: Property = text("exif:ExposureTime");
const F_NUMBER: Property = text("exif:FNumber");
const FOCAL_LENGTH: Property = text("exif:FocalLength");
const ISO_SPEED_RATINGS: Property = bag("exif:IsoSpeedRatings");
const EQUIPMENT_MAKE: Property = text("tiff:Make");
const EQUIPMENT_MODEL: Property = text("tiff:Model");
const SOFTWARE: Property = text("tiff:Software");
pub(crate) const ORIENTATION: Property = text("tiff:Orientation");
const RESOLUTION_HORIZONTAL: Property = text("tiff:XResolution");
const RESOLUTION_VERTICAL: Property = text("tiff:YResolution");
const RESOLUTION_UNIT: Property = text("tiff:ResolutionUnit");
const ORIGINAL_DATE: Property = text("exif:DateTimeOriginal");
const EXIF_PAGE_COUNT: Property = text("exif:PageCount");

/// The names of the properties of `TikaCoreProperties` and `Metadata`, which
/// `MetadataFields.isMetadataField` knows: a tag of one of these names is not copied.
const METADATA_FIELDS: [&str; 86] = [
    ":",
    "X-TIKA:",
    "dc:contributor",
    "dc:coverage",
    "dc:creator",
    "dc:description",
    "dc:format",
    "dc:identifier",
    "dc:language",
    "dc:publisher",
    "dc:relation",
    "dc:rights",
    "dc:source",
    "dc:subject",
    "dc:title",
    "dc:type",
    "dcterms:created",
    "dcterms:modified",
    "geo:alt",
    "geo:lat",
    "geo:long",
    "meta:last-author",
    "meta:print-date",
    "tk:",
    "tk:chunks",
    "tk:content",
    "tk:content-handler-type",
    "tk:content-type-hint",
    "tk:content-type-magic-detected",
    "tk:content-type-override",
    "tk:content-type-parser-override",
    "tk:decoded-charset",
    "tk:detected-encoding",
    "tk:detected-language",
    "tk:detected-language-confidence",
    "tk:detected-language-confidence-raw",
    "tk:detection-content-length",
    "tk:embedded-depth",
    "tk:embedded-id",
    "tk:embedded-id-path",
    "tk:embedded-relationship-id",
    "tk:embedded-resource-path",
    "tk:embedded-resource-type",
    "tk:encoding-detection-trace",
    "tk:encoding-detector",
    "tk:encrypted",
    "tk:exception:",
    "tk:exception:container-exception",
    "tk:exception:embedded-bytes-exception",
    "tk:exception:embedded-depth-limit-reached",
    "tk:exception:embedded-exception",
    "tk:exception:embedded-parser",
    "tk:exception:embedded-resource-limit-reached",
    "tk:exception:embedded-stream-exception",
    "tk:exception:embedded-warning",
    "tk:exception:task-deadline-reached",
    "tk:exception:warn",
    "tk:exception:write-limit-reached",
    "tk:final-embedded-resource-path",
    "tk:has-signature",
    "tk:inference-released",
    "tk:internal-path",
    "tk:num-images",
    "tk:orig-resource-name",
    "tk:parse-time-millis",
    "tk:parsed-by",
    "tk:parsed-by-full-set",
    "tk:pipes-result",
    "tk:resource-name",
    "tk:resource-name-extension-inferred",
    "tk:signature:contact-info",
    "tk:signature:date",
    "tk:signature:filter",
    "tk:signature:location",
    "tk:signature:name",
    "tk:signature:reason",
    "tk:source-path",
    "tk:truncated-content-for-detection",
    "tk:version-count",
    "tk:version-number",
    "tk:warn:",
    "tk:warn:truncated-metadata",
    "w:Comments",
    "xmp:CreatorTool",
    "xmp:MetadataDate",
    "xmp:Rating",
];

/// The Tika metadata a document's directories are copied into, with the count of distinct
/// names written through `add(KeyPrefix, name, value)`, which `Metadata` caps.
pub(crate) struct Target<'m> {
    pub(crate) metadata: &'m mut TikaMetadata,
    prefix_route_names: usize,
}

impl<'m> Target<'m> {
    pub(crate) fn new(metadata: &'m mut TikaMetadata) -> Self {
        Self {
            metadata,
            prefix_route_names: 0,
        }
    }

    /// Copies into metadata that already has `prefix_route_names` names written through
    /// [`Target::add_prefixed`].
    pub(crate) fn continuing(metadata: &'m mut TikaMetadata, prefix_route_names: usize) -> Self {
        Self {
            metadata,
            prefix_route_names,
        }
    }

    pub(crate) fn prefix_route_names(&self) -> usize {
        self.prefix_route_names
    }

    /// `Metadata.add(KeyPrefix, name, value)`: a blank or overlong name is skipped, and so is a
    /// new name past the cap.
    pub(crate) fn add_prefixed(&mut self, prefix: &str, name: &str, value: &str) {
        if name.chars().all(char::is_whitespace)
            || name.encode_utf16().count() > MAX_PREFIX_ROUTE_NAME_LENGTH
        {
            return;
        }
        let key = format!("{prefix}{name}");
        if self.metadata.values(&key).is_empty() {
            if self.prefix_route_names >= MAX_PREFIX_ROUTE_NAMES {
                return;
            }
            self.prefix_route_names += 1;
        }
        self.metadata.add(&key, value);
    }
}

/// `ImageMetadataExtractor.handle`: each directory through the handlers that support it.
pub(crate) fn handle(
    directories: &Metadata,
    target: &mut Target,
    zone: &TimeZone,
) -> Result<(), Unported> {
    for directory in &directories.directories {
        copy_unknown_fields(directory, target)?;
        page_number(directory, target.metadata);
        if directory.kind == Kind::JpegComment {
            if let Some(comment) = directory.string(jpeg::TAG_COMMENT) {
                xmp::add(target.metadata, &COMMENTS, &comment)?;
            }
        }
        if matches!(directory.kind, Kind::ExifIfd0 | Kind::ExifSubIfd) {
            exif(directory, target.metadata, zone)?;
        }
        if matches!(
            directory.kind,
            Kind::Jpeg | Kind::ExifSubIfd | Kind::ExifThumbnail { .. } | Kind::ExifIfd0
        ) {
            dimensions(directory, target.metadata);
        }
        if directory.kind == Kind::Gps {
            gps(directory, target.metadata)?;
        }
        if directory.kind == Kind::Iptc {
            self::iptc(directory, target.metadata)?;
        }
    }
    Ok(())
}

/// `CopyUnknownFieldsHandler`, curves and lookup tables of ICC profiles left out as Tika's
/// default configuration leaves them.
fn copy_unknown_fields(directory: &Directory, target: &mut Target) -> Result<(), Unported> {
    for tag in directory.tags() {
        let name = directory.tag_name(tag);
        if METADATA_FIELDS.contains(&name.as_str()) {
            continue;
        }
        let icc_type = if directory.kind == Kind::Icc {
            icc::tag_data_type(directory, tag)
        } else {
            0
        };
        if matches!(
            icc_type,
            ICC_CURV | ICC_PARA | ICC_MFT1 | ICC_MFT2 | ICC_MAB | ICC_MBA
        ) {
            continue;
        }
        let Some(description) = directory.description(tag)? else {
            continue;
        };
        let mut value = crate::java::trim(&description).to_owned();
        if crate::java::equals_ignore_case(&value, "true") {
            value = "true".to_owned();
        } else if crate::java::equals_ignore_case(&value, "false") {
            value = "false".to_owned();
        }
        if directory.kind.is_exif() {
            target.add_prefixed(
                UNKNOWN_IMG,
                &format!("{}:{name}", directory.kind.name()),
                &value,
            );
        } else if directory.kind == Kind::Icc {
            target.add_prefixed(ICC, &name, &value);
        } else {
            target.add_prefixed(UNKNOWN_IMG, &name, &value);
        }
    }
    Ok(())
}

/// `TiffPageNumberHandler`: the largest page count a page number gives.
fn page_number(directory: &Directory, metadata: &mut TikaMetadata) {
    if !directory.contains(TAG_PAGE_NUMBER) {
        return;
    }
    let Some(pages) = directory.ints(TAG_PAGE_NUMBER) else {
        return;
    };
    if pages.len() > 1 {
        let current = metadata
            .get(EXIF_PAGE_COUNT.name)
            .and_then(crate::java::parse_int);
        if current.is_none_or(|current| current < pages[1]) {
            xmp::set(metadata, &EXIF_PAGE_COUNT, &pages[1].to_string());
        }
    }
}

/// `DimensionsHandler`.
fn dimensions(directory: &Directory, metadata: &mut TikaMetadata) {
    let set = |metadata: &mut TikaMetadata, tag: i32, property: &Property| {
        if !directory.contains(tag) {
            return;
        }
        if let Some(number) = directory.string(tag).as_deref().and_then(leading_number) {
            xmp::set(metadata, property, number);
        }
    };
    set(metadata, jpeg::TAG_IMAGE_WIDTH, &IMAGE_WIDTH);
    set(metadata, jpeg::TAG_IMAGE_HEIGHT, &IMAGE_LENGTH);
    set(metadata, jpeg::TAG_DATA_PRECISION, &BITS_PER_SAMPLE);
    set(metadata, TAG_BITS_PER_SAMPLE, &BITS_PER_SAMPLE);
    set(metadata, TAG_SAMPLES_PER_PIXEL, &SAMPLES_PER_PIXEL);
}

/// The leading number `(\d+)\s*.*` captures when it matches the whole text: ASCII digits, then
/// whitespace, then anything but a line terminator.
fn leading_number(text: &str) -> Option<&str> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = text[digits..].trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r']);
    if rest.contains(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']) {
        return None;
    }
    Some(&text[..digits])
}

/// `trimPixels`: the text before its last ` pixels`.
fn trim_pixels(text: &str) -> Result<String, Unported> {
    match text.rfind(" pixels") {
        Some(end) => Ok(text[..end].to_owned()),
        None => Err(Unported::new(
            "a size described without pixels, which Tika's trimPixels throws on",
        )),
    }
}

/// `ExifHandler`: dates, camera fields and the image description.
fn exif(
    directory: &Directory,
    metadata: &mut TikaMetadata,
    zone: &TimeZone,
) -> Result<(), Unported> {
    exif_dates(directory, metadata, zone);
    photo_tags(directory, metadata)?;
    if metadata.get(DESCRIPTION.name).is_none() && directory.contains(TAG_IMAGE_DESCRIPTION) {
        if let Some(description) = directory.string(TAG_IMAGE_DESCRIPTION) {
            xmp::set(metadata, &DESCRIPTION, &description);
        }
    }
    Ok(())
}

/// A date as Tika writes an Exif date: `yyyy-MM-dd'T'HH:mm:ss` in GMT.
fn unspecified_zone(millis: i64) -> String {
    SimpleDateFormat::new("yyyy-MM-dd'T'HH:mm:ss")
        .expect("Tika's Exif date pattern compiles")
        .format(millis, &TimeZone::fixed(0))
        .expect("Tika's Exif date pattern names no zone")
}

fn exif_dates(directory: &Directory, metadata: &mut TikaMetadata, zone: &TimeZone) {
    if directory.contains(TAG_DATETIME_ORIGINAL) {
        if let Some(original) = directory.date(TAG_DATETIME_ORIGINAL, zone) {
            let text = unspecified_zone(original);
            xmp::set(metadata, &CREATED, &text);
            xmp::set(metadata, &ORIGINAL_DATE, &text);
        }
    }
    if directory.contains(TAG_DATETIME) {
        if let Some(modified) = directory.date(TAG_DATETIME, zone) {
            let text = unspecified_zone(modified);
            xmp::set(metadata, &MODIFIED, &text);
            if metadata.get(CREATED.name).is_none() {
                xmp::set(metadata, &CREATED, &text);
            }
        }
    }
}

/// A rational as `Double.toString` of its value, or the tag's text when it is not a rational.
fn rational_or_text(directory: &Directory, tag: i32) -> Option<String> {
    match directory.object(tag)? {
        Value::Rational(rational) => Some(crate::java::dtoa::java_double_to_string(
            rational.double_value(),
        )),
        _ => directory.string(tag),
    }
}

fn photo_tags(directory: &Directory, metadata: &mut TikaMetadata) -> Result<(), Unported> {
    let set = |metadata: &mut TikaMetadata, property: &Property, value: Option<String>| {
        if let Some(value) = value {
            xmp::set(metadata, property, &value);
        }
    };
    if directory.contains(TAG_EXPOSURE_TIME) {
        set(
            metadata,
            &EXPOSURE_TIME,
            rational_or_text(directory, TAG_EXPOSURE_TIME),
        );
    }
    if directory.contains(TAG_FLASH) {
        if let Some(flash) = directory.description(TAG_FLASH)? {
            let value = if flash.contains("Flash fired") {
                "true".to_owned()
            } else if flash.contains("Flash did not fire") {
                "false".to_owned()
            } else {
                flash
            };
            set(metadata, &FLASH_FIRED, Some(value));
        }
    }
    if directory.contains(TAG_FNUMBER) {
        set(
            metadata,
            &F_NUMBER,
            rational_or_text(directory, TAG_FNUMBER),
        );
    }
    if directory.contains(TAG_FOCAL_LENGTH) {
        set(
            metadata,
            &FOCAL_LENGTH,
            rational_or_text(directory, TAG_FOCAL_LENGTH),
        );
    }
    if directory.contains(TAG_ISO_EQUIVALENT) {
        set(
            metadata,
            &ISO_SPEED_RATINGS,
            directory.string(TAG_ISO_EQUIVALENT),
        );
    }
    if directory.contains(TAG_MAKE) {
        set(metadata, &EQUIPMENT_MAKE, directory.string(TAG_MAKE));
    }
    if directory.contains(TAG_MODEL) {
        set(metadata, &EQUIPMENT_MODEL, directory.string(TAG_MODEL));
    }
    if directory.contains(TAG_ORIENTATION) {
        set(metadata, &ORIENTATION, directory.string(TAG_ORIENTATION));
    }
    if directory.contains(TAG_SOFTWARE) {
        set(metadata, &SOFTWARE, directory.string(TAG_SOFTWARE));
    }
    if directory.contains(TAG_X_RESOLUTION) {
        set(
            metadata,
            &RESOLUTION_HORIZONTAL,
            rational_or_text(directory, TAG_X_RESOLUTION),
        );
    }
    if directory.contains(TAG_Y_RESOLUTION) {
        set(
            metadata,
            &RESOLUTION_VERTICAL,
            rational_or_text(directory, TAG_Y_RESOLUTION),
        );
    }
    if directory.contains(TAG_RESOLUTION_UNIT) {
        set(
            metadata,
            &RESOLUTION_UNIT,
            directory.description(TAG_RESOLUTION_UNIT)?,
        );
    }
    if directory.contains(TAG_IMAGE_WIDTH) {
        if let Some(width) = directory.description(TAG_IMAGE_WIDTH)? {
            set(metadata, &IMAGE_WIDTH, Some(trim_pixels(&width)?));
        }
    }
    if directory.contains(TAG_IMAGE_HEIGHT) {
        if let Some(height) = directory.description(TAG_IMAGE_HEIGHT)? {
            set(metadata, &IMAGE_LENGTH, Some(trim_pixels(&height)?));
        }
    }
    if directory.contains(TAG_EXIF_IMAGE_WIDTH) {
        if let Some(width) = directory.description(TAG_EXIF_IMAGE_WIDTH)? {
            set(metadata, &IMAGE_WIDTH, Some(trim_pixels(&width)?));
        }
    }
    if directory.contains(TAG_EXIF_IMAGE_HEIGHT) {
        if let Some(height) = directory.description(TAG_EXIF_IMAGE_HEIGHT)? {
            set(metadata, &IMAGE_LENGTH, Some(trim_pixels(&height)?));
        }
    }
    Ok(())
}

/// `IptcHandler`: keywords, and the title, creator and description XMP left unset.
fn iptc(directory: &Directory, metadata: &mut TikaMetadata) -> Result<(), Unported> {
    if directory.contains(iptc::TAG_KEYWORDS) {
        for keyword in directory.strings(iptc::TAG_KEYWORDS).unwrap_or_default() {
            xmp::add(metadata, &SUBJECT, &keyword)?;
        }
    }
    if metadata.get(TITLE.name).is_none() {
        if directory.contains(iptc::TAG_HEADLINE) {
            if let Some(headline) = directory.string(iptc::TAG_HEADLINE) {
                xmp::set(metadata, &TITLE, &headline);
            }
        } else if directory.contains(iptc::TAG_OBJECT_NAME) {
            if let Some(name) = directory.string(iptc::TAG_OBJECT_NAME) {
                xmp::set(metadata, &TITLE, &name);
            }
        }
    }
    if directory.contains(iptc::TAG_BY_LINE) {
        if let Some(by_line) = directory.string(iptc::TAG_BY_LINE) {
            if metadata.get(CREATOR.name).is_none() {
                xmp::set(metadata, &CREATOR, &by_line);
            }
            xmp::set(metadata, &CREATOR, &by_line);
        }
    }
    if directory.contains(iptc::TAG_CAPTION) && metadata.get(DESCRIPTION.name).is_none() {
        if let Some(caption) = directory.string(iptc::TAG_CAPTION) {
            xmp::set(
                metadata,
                &DESCRIPTION,
                &caption.replace("\r\n", "\n").replace('\r', "\n"),
            );
        }
    }
    Ok(())
}

/// `GeotagHandler`: the position in decimal degrees, the GPS time and the altitude.
fn gps(directory: &Directory, metadata: &mut TikaMetadata) -> Result<(), Unported> {
    let format = |value: f64| {
        DecimalFormat::new(GEO_DECIMAL_FORMAT, Rounding::HalfEven)
            .expect("Tika's geo pattern compiles")
            .format_f64(value)
    };
    if let Some(location) = geo::location(directory) {
        xmp::set(metadata, &LATITUDE, &format(location.latitude));
        xmp::set(metadata, &LONGITUDE, &format(location.longitude));
    }
    if let Some(millis) = gps_date(directory) {
        xmp::set(metadata, &GEO_TIMESTAMP, &time::format_instant(millis));
    }
    if let Some(altitude) = directory.rational(GPS_ALTITUDE) {
        let mut metres = altitude.double_value();
        if directory.integer(GPS_ALTITUDE_REF) == Some(1) {
            metres = -metres;
        }
        xmp::set(metadata, &ALTITUDE, &format(metres));
    }
    Ok(())
}

/// `GpsDirectory.getGpsDate`.
fn gps_date(directory: &Directory) -> Option<i64> {
    let date = directory.string(GPS_DATE_STAMP)?;
    let [hours, minutes, seconds] = directory.rationals(GPS_TIME_STAMP)? else {
        return None;
    };
    parse_gps_date(
        &date,
        hours.int_value(),
        minutes.int_value(),
        seconds.double_value(),
    )
}
