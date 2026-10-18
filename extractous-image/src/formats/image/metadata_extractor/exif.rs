//! metadata-extractor's `TiffReader` with its `ExifTiffHandler`: the IFDs of a TIFF structure, in
//! a TIFF file or an Exif segment, read into the directories of Exif: IFD0, its Exif and GPS
//! sub-IFDs, the interoperability IFD, the thumbnail or the further pages that follow IFD0, and
//! the IPTC, ICC, Photoshop and PrintIM data some tags hold, and Apple's maker note. Other maker
//! notes and Panasonic's raw IFD0 are left to Tika's path; XMP held in a tag is read by [`xmp`].

use std::collections::HashSet;

use super::reader::{Bounds, Reader};
use super::{
    apple, icc, iptc, java_string, photoshop, xmp, Directory, Kind, Metadata, Rational, Unported,
    Value,
};
use crate::text::charset::Charset;
use tags::*;

/// The tags the port reads or describes.
pub(crate) mod tags {
    pub(crate) const TAG_INTEROP_INDEX: i32 = 0x0001;
    pub(crate) const TAG_INTEROP_VERSION: i32 = 0x0002;
    pub(crate) const TAG_NEW_SUBFILE_TYPE: i32 = 0x00fe;
    pub(crate) const TAG_SUBFILE_TYPE: i32 = 0x00ff;
    pub(crate) const TAG_IMAGE_WIDTH: i32 = 0x0100;
    pub(crate) const TAG_IMAGE_HEIGHT: i32 = 0x0101;
    pub(crate) const TAG_BITS_PER_SAMPLE: i32 = 0x0102;
    pub(crate) const TAG_COMPRESSION: i32 = 0x0103;
    pub(crate) const TAG_PHOTOMETRIC_INTERPRETATION: i32 = 0x0106;
    pub(crate) const TAG_THRESHOLDING: i32 = 0x0107;
    pub(crate) const TAG_FILL_ORDER: i32 = 0x010a;
    pub(crate) const TAG_IMAGE_DESCRIPTION: i32 = 0x010e;
    pub(crate) const TAG_MAKE: i32 = 0x010f;
    pub(crate) const TAG_MODEL: i32 = 0x0110;
    pub(crate) const TAG_ORIENTATION: i32 = 0x0112;
    pub(crate) const TAG_SAMPLES_PER_PIXEL: i32 = 0x0115;
    pub(crate) const TAG_ROWS_PER_STRIP: i32 = 0x0116;
    pub(crate) const TAG_STRIP_BYTE_COUNTS: i32 = 0x0117;
    pub(crate) const TAG_X_RESOLUTION: i32 = 0x011a;
    pub(crate) const TAG_Y_RESOLUTION: i32 = 0x011b;
    pub(crate) const TAG_PLANAR_CONFIGURATION: i32 = 0x011c;
    pub(crate) const TAG_RESOLUTION_UNIT: i32 = 0x0128;
    pub(crate) const TAG_PAGE_NUMBER: i32 = 0x0129;
    pub(crate) const TAG_SOFTWARE: i32 = 0x0131;
    pub(crate) const TAG_DATETIME: i32 = 0x0132;
    pub(crate) const TAG_SUB_IFD_OFFSET: i32 = 0x014a;
    pub(crate) const TAG_EXTRA_SAMPLES: i32 = 0x0152;
    pub(crate) const TAG_SAMPLE_FORMAT: i32 = 0x0153;
    pub(crate) const TAG_JPEG_PROC: i32 = 0x0200;
    pub(crate) const TAG_THUMBNAIL_OFFSET: i32 = 0x0201;
    pub(crate) const TAG_THUMBNAIL_LENGTH: i32 = 0x0202;
    pub(crate) const TAG_YCBCR_SUBSAMPLING: i32 = 0x0212;
    pub(crate) const TAG_YCBCR_POSITIONING: i32 = 0x0213;
    pub(crate) const TAG_REFERENCE_BLACK_WHITE: i32 = 0x0214;
    pub(crate) const TAG_APPLICATION_NOTES: i32 = 0x02bc;
    pub(crate) const TAG_CFA_REPEAT_PATTERN_DIM: i32 = 0x828d;
    pub(crate) const TAG_CFA_PATTERN_2: i32 = 0x828e;
    pub(crate) const TAG_EXPOSURE_TIME: i32 = 0x829a;
    pub(crate) const TAG_FNUMBER: i32 = 0x829d;
    pub(crate) const TAG_IPTC_NAA: i32 = 0x83bb;
    pub(crate) const TAG_PHOTOSHOP_SETTINGS: i32 = 0x8649;
    pub(crate) const TAG_EXIF_SUB_IFD_OFFSET: i32 = 0x8769;
    pub(crate) const TAG_INTER_COLOR_PROFILE: i32 = 0x8773;
    pub(crate) const TAG_EXPOSURE_PROGRAM: i32 = 0x8822;
    pub(crate) const TAG_GPS_INFO_OFFSET: i32 = 0x8825;
    pub(crate) const TAG_ISO_EQUIVALENT: i32 = 0x8827;
    pub(crate) const TAG_SENSITIVITY_TYPE: i32 = 0x8830;
    pub(crate) const TAG_EXIF_VERSION: i32 = 0x9000;
    pub(crate) const TAG_DATETIME_ORIGINAL: i32 = 0x9003;
    pub(crate) const TAG_COMPONENTS_CONFIGURATION: i32 = 0x9101;
    pub(crate) const TAG_COMPRESSED_AVERAGE_BITS_PER_PIXEL: i32 = 0x9102;
    pub(crate) const TAG_SHUTTER_SPEED: i32 = 0x9201;
    pub(crate) const TAG_APERTURE: i32 = 0x9202;
    pub(crate) const TAG_BRIGHTNESS_VALUE: i32 = 0x9203;
    pub(crate) const TAG_EXPOSURE_BIAS: i32 = 0x9204;
    pub(crate) const TAG_MAX_APERTURE: i32 = 0x9205;
    pub(crate) const TAG_SUBJECT_DISTANCE: i32 = 0x9206;
    pub(crate) const TAG_METERING_MODE: i32 = 0x9207;
    pub(crate) const TAG_WHITE_BALANCE: i32 = 0x9208;
    pub(crate) const TAG_FLASH: i32 = 0x9209;
    pub(crate) const TAG_FOCAL_LENGTH: i32 = 0x920a;
    pub(crate) const TAG_MAKERNOTE: i32 = 0x927c;
    pub(crate) const TAG_USER_COMMENT: i32 = 0x9286;
    pub(crate) const TAG_TEMPERATURE: i32 = 0x9400;
    pub(crate) const TAG_HUMIDITY: i32 = 0x9401;
    pub(crate) const TAG_PRESSURE: i32 = 0x9402;
    pub(crate) const TAG_WATER_DEPTH: i32 = 0x9403;
    pub(crate) const TAG_ACCELERATION: i32 = 0x9404;
    pub(crate) const TAG_CAMERA_ELEVATION_ANGLE: i32 = 0x9405;
    pub(crate) const TAG_WIN_TITLE: i32 = 0x9c9b;
    pub(crate) const TAG_WIN_COMMENT: i32 = 0x9c9c;
    pub(crate) const TAG_WIN_AUTHOR: i32 = 0x9c9d;
    pub(crate) const TAG_WIN_KEYWORDS: i32 = 0x9c9e;
    pub(crate) const TAG_WIN_SUBJECT: i32 = 0x9c9f;
    pub(crate) const TAG_FLASHPIX_VERSION: i32 = 0xa000;
    pub(crate) const TAG_COLOR_SPACE: i32 = 0xa001;
    pub(crate) const TAG_EXIF_IMAGE_WIDTH: i32 = 0xa002;
    pub(crate) const TAG_EXIF_IMAGE_HEIGHT: i32 = 0xa003;
    pub(crate) const TAG_INTEROP_OFFSET: i32 = 0xa005;
    pub(crate) const TAG_FOCAL_PLANE_X_RESOLUTION: i32 = 0xa20e;
    pub(crate) const TAG_FOCAL_PLANE_Y_RESOLUTION: i32 = 0xa20f;
    pub(crate) const TAG_FOCAL_PLANE_RESOLUTION_UNIT: i32 = 0xa210;
    pub(crate) const TAG_SENSING_METHOD: i32 = 0xa217;
    pub(crate) const TAG_FILE_SOURCE: i32 = 0xa300;
    pub(crate) const TAG_SCENE_TYPE: i32 = 0xa301;
    pub(crate) const TAG_CFA_PATTERN: i32 = 0xa302;
    pub(crate) const TAG_CUSTOM_RENDERED: i32 = 0xa401;
    pub(crate) const TAG_EXPOSURE_MODE: i32 = 0xa402;
    pub(crate) const TAG_WHITE_BALANCE_MODE: i32 = 0xa403;
    pub(crate) const TAG_DIGITAL_ZOOM_RATIO: i32 = 0xa404;
    pub(crate) const TAG_35MM_FILM_EQUIV_FOCAL_LENGTH: i32 = 0xa405;
    pub(crate) const TAG_SCENE_CAPTURE_TYPE: i32 = 0xa406;
    pub(crate) const TAG_GAIN_CONTROL: i32 = 0xa407;
    pub(crate) const TAG_CONTRAST: i32 = 0xa408;
    pub(crate) const TAG_SATURATION: i32 = 0xa409;
    pub(crate) const TAG_SHARPNESS: i32 = 0xa40a;
    pub(crate) const TAG_SUBJECT_DISTANCE_RANGE: i32 = 0xa40c;
    pub(crate) const TAG_LENS_SPECIFICATION: i32 = 0xa432;
    pub(crate) const TAG_PRINT_IMAGE_MATCHING_INFO: i32 = 0xc4a5;

    pub(crate) const GPS_VERSION_ID: i32 = 0x0000;
    pub(crate) const GPS_LATITUDE_REF: i32 = 0x0001;
    pub(crate) const GPS_LATITUDE: i32 = 0x0002;
    pub(crate) const GPS_LONGITUDE_REF: i32 = 0x0003;
    pub(crate) const GPS_LONGITUDE: i32 = 0x0004;
    pub(crate) const GPS_ALTITUDE_REF: i32 = 0x0005;
    pub(crate) const GPS_ALTITUDE: i32 = 0x0006;
    pub(crate) const GPS_TIME_STAMP: i32 = 0x0007;
    pub(crate) const GPS_STATUS: i32 = 0x0009;
    pub(crate) const GPS_MEASURE_MODE: i32 = 0x000a;
    pub(crate) const GPS_DOP: i32 = 0x000b;
    pub(crate) const GPS_SPEED_REF: i32 = 0x000c;
    pub(crate) const GPS_SPEED: i32 = 0x000d;
    pub(crate) const GPS_TRACK_REF: i32 = 0x000e;
    pub(crate) const GPS_TRACK: i32 = 0x000f;
    pub(crate) const GPS_IMG_DIRECTION_REF: i32 = 0x0010;
    pub(crate) const GPS_IMG_DIRECTION: i32 = 0x0011;
    pub(crate) const GPS_DEST_LATITUDE_REF: i32 = 0x0013;
    pub(crate) const GPS_DEST_LATITUDE: i32 = 0x0014;
    pub(crate) const GPS_DEST_LONGITUDE_REF: i32 = 0x0015;
    pub(crate) const GPS_DEST_LONGITUDE: i32 = 0x0016;
    pub(crate) const GPS_DEST_BEARING_REF: i32 = 0x0017;
    pub(crate) const GPS_DEST_BEARING: i32 = 0x0018;
    pub(crate) const GPS_DEST_DISTANCE_REF: i32 = 0x0019;
    pub(crate) const GPS_DEST_DISTANCE: i32 = 0x001a;
    pub(crate) const GPS_PROCESSING_METHOD: i32 = 0x001b;
    pub(crate) const GPS_AREA_INFORMATION: i32 = 0x001c;
    pub(crate) const GPS_DATE_STAMP: i32 = 0x001d;
    pub(crate) const GPS_DIFFERENTIAL: i32 = 0x001e;
    pub(crate) const GPS_H_POSITIONING_ERROR: i32 = 0x001f;
}

/// Why reading the TIFF structure ended early.
#[derive(Debug)]
pub(crate) enum TiffError {
    /// A read beyond the data, metadata-extractor's `IOException`.
    Bounds(Bounds),
    /// `TiffProcessingException`: the structure is not TIFF.
    Processing(String),
    /// A part of it this port leaves to Tika's path.
    Unported(Unported),
}

impl From<Bounds> for TiffError {
    fn from(bounds: Bounds) -> Self {
        Self::Bounds(bounds)
    }
}

impl From<Unported> for TiffError {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

const CODE_INT8_U: u16 = 1;
const CODE_STRING: u16 = 2;
const CODE_INT16_U: u16 = 3;
const CODE_INT32_U: u16 = 4;
const CODE_RATIONAL_U: u16 = 5;
const CODE_INT8_S: u16 = 6;
const CODE_UNDEFINED: u16 = 7;
const CODE_INT16_S: u16 = 8;
const CODE_INT32_S: u16 = 9;
const CODE_RATIONAL_S: u16 = 10;
const CODE_SINGLE: u16 = 11;
const CODE_DOUBLE: u16 = 12;
const CODE_INT64_U: u16 = 16;
const CODE_INT64_S: u16 = 17;

/// `TiffDataFormat.fromTiffFormatCode(code).getComponentSizeBytes()`.
fn component_size(code: u16) -> Option<i64> {
    Some(match code {
        CODE_INT8_U | CODE_STRING | CODE_INT8_S | CODE_UNDEFINED => 1,
        CODE_INT16_U | CODE_INT16_S => 2,
        CODE_INT32_U | CODE_INT32_S | CODE_SINGLE => 4,
        CODE_RATIONAL_U | CODE_RATIONAL_S | CODE_DOUBLE | CODE_INT64_U | CODE_INT64_S => 8,
        _ => return None,
    })
}

/// `ExifTiffHandler` over `DirectoryTiffHandler`: the directory being filled, the ones it
/// interrupted, and where the Exif data starts, from which thumbnail offsets count.
struct Handler<'m> {
    metadata: &'m mut Metadata,
    current: Option<usize>,
    stack: Vec<usize>,
    exif_start: i32,
}

impl Handler<'_> {
    fn push(&mut self, kind: Kind) {
        if let Some(current) = self.current {
            self.stack.push(current);
        }
        self.metadata.add(Directory::new(kind));
        self.current = Some(self.metadata.directories.len() - 1);
    }

    fn ending_ifd(&mut self) {
        self.current = self.stack.pop();
    }

    fn current_kind(&self) -> Option<Kind> {
        self.current
            .map(|index| self.metadata.directories[index].kind)
    }

    fn directory(&mut self) -> &mut Directory {
        let index = self
            .current
            .expect("a TIFF tag is read into the IFD the handler entered");
        &mut self.metadata.directories[index]
    }

    /// `warn` and `error`: on the current directory, or on the extraction's error directory.
    fn error(&mut self, message: impl Into<String>) {
        if self.current.is_none() {
            match self.metadata.first_index(|kind| kind == Kind::Error) {
                Some(index) => {
                    self.metadata.directories[index].add_error(message);
                    return;
                }
                None => self.push(Kind::Error),
            }
        }
        self.directory().add_error(message);
    }

    fn set_tiff_marker(&mut self, marker: u16) -> Result<(), TiffError> {
        match marker {
            0x002A | 0x4F52 | 0x5352 => {
                self.push(Kind::ExifIfd0);
                Ok(())
            }
            0x0055 => Err(Unported::new("a Panasonic raw IFD0").into()),
            other => Err(TiffError::Processing(format!(
                "Unexpected TIFF marker: 0x{other:X}"
            ))),
        }
    }

    fn try_enter_sub_ifd(&mut self, tag: i32) -> bool {
        if tag == TAG_SUB_IFD_OFFSET {
            self.push(Kind::ExifSubIfd);
            return true;
        }
        match self.current_kind() {
            Some(Kind::ExifIfd0) if tag == TAG_EXIF_SUB_IFD_OFFSET => {
                self.push(Kind::ExifSubIfd);
                true
            }
            Some(Kind::ExifIfd0) if tag == TAG_GPS_INFO_OFFSET => {
                self.push(Kind::Gps);
                true
            }
            Some(Kind::ExifSubIfd) if tag == TAG_INTEROP_OFFSET => {
                self.push(Kind::ExifInterop);
                true
            }
            _ => false,
        }
    }

    fn has_follower_ifd(&mut self) -> bool {
        match self.current_kind() {
            Some(Kind::ExifIfd0 | Kind::ExifImage) => {
                if self.directory().contains(TAG_PAGE_NUMBER) {
                    self.push(Kind::ExifImage);
                } else {
                    self.push(Kind::ExifThumbnail {
                        exif_start: self.exif_start,
                    });
                }
                true
            }
            Some(Kind::ExifThumbnail { .. }) => true,
            _ => false,
        }
    }

    /// `customProcessTag`: whether the tag was read here rather than as a plain value.
    fn custom_process_tag(
        &mut self,
        offset: i64,
        reader: &mut Reader,
        tag: i32,
        byte_count: i64,
        processed: &mut HashSet<i64>,
    ) -> Result<bool, TiffError> {
        let kind = self.current_kind();
        if tag == 0 {
            if self.directory().contains(tag) {
                return Ok(false);
            }
            if byte_count == 0 {
                return Ok(true);
            }
        }
        if tag == TAG_MAKERNOTE && kind == Some(Kind::ExifSubIfd) {
            return self.process_makernote(offset, reader, processed);
        }
        if tag == TAG_IPTC_NAA && kind == Some(Kind::ExifIfd0) {
            if reader.i8(offset)? == 0x1c {
                let bytes = reader.bytes(offset, byte_count)?;
                iptc::extract(bytes, self.metadata)?;
                return Ok(true);
            }
            return Ok(false);
        }
        if tag == TAG_INTER_COLOR_PROFILE {
            let bytes = reader.bytes(offset, byte_count)?;
            icc::extract(bytes, self.metadata);
            return Ok(true);
        }
        if tag == TAG_PHOTOSHOP_SETTINGS && kind == Some(Kind::ExifIfd0) {
            let bytes = reader.bytes(offset, byte_count)?;
            photoshop::extract(bytes, self.metadata)?;
            return Ok(true);
        }
        if tag == TAG_APPLICATION_NOTES && matches!(kind, Some(Kind::ExifIfd0 | Kind::ExifSubIfd)) {
            let bytes = reader.null_terminated_bytes(offset, byte_count)?;
            xmp::extract(bytes, self.metadata)?;
            return Ok(true);
        }
        if tag == apple::TAG_RUN_TIME && kind == Some(Kind::AppleMakernote) {
            let bytes = reader.bytes(offset, byte_count)?;
            let maker_note = self
                .current
                .expect("the maker note is the current directory");
            apple::extract_run_time(bytes, self.metadata, maker_note)?;
            return Ok(true);
        }
        if tag == TAG_PRINT_IMAGE_MATCHING_INFO {
            self.metadata.add(Directory::new(Kind::PrintIm));
            let index = self.metadata.directories.len() - 1;
            process_print_im(
                &mut self.metadata.directories[index],
                offset,
                reader,
                byte_count,
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    /// `processMakernote`: Apple's maker note read as its IFD, and as far as telling whether
    /// metadata-extractor reads another maker note as a directory of its own, which the port does
    /// not: such a note is left to Tika's path.
    fn process_makernote(
        &mut self,
        offset: i64,
        reader: &mut Reader,
        processed: &mut HashSet<i64>,
    ) -> Result<bool, TiffError> {
        let make = self
            .metadata
            .first(Kind::ExifIfd0)
            .and_then(|ifd0| ifd0.string(TAG_MAKE));
        let chars = |count: i64| -> String {
            reader
                .bytes(offset, count)
                .map(|bytes| java_string(bytes, None))
                .unwrap_or_default()
        };
        let upper = make.as_deref().map(str::to_uppercase);
        let starts = |prefix: &str| {
            upper
                .as_deref()
                .is_some_and(|make| make.starts_with(prefix))
        };
        let make_is = |name: &str| {
            make.as_deref()
                .is_some_and(|make| crate::java::equals_ignore_case(make, name))
        };
        let maker = if chars(6) == "OLYMP\0"
            || chars(5) == "EPSON"
            || chars(4) == "AGFA"
            || chars(10) == "OLYMPUS\0II"
            || chars(14) == "OM SYSTEM\0\0\0II"
        {
            "Olympus"
        } else if starts("MINOLTA") {
            "Minolta"
        } else if make
            .as_deref()
            .is_some_and(|make| crate::java::trim(make).to_uppercase().starts_with("NIKON"))
        {
            "Nikon"
        } else if chars(8) == "SONY CAM"
            || chars(8) == "SONY DSC"
            || (make.as_deref().is_some_and(|make| make.starts_with("SONY"))
                && reader.bytes(offset, 2)? != [0x01, 0x00])
        {
            "Sony"
        } else if chars(12) == "SEMC MS\0\0\0\0\0" {
            "Sony Ericsson"
        } else if chars(8) == "SIGMA\0\0\0" || chars(8) == "FOVEON\0\0" {
            "Sigma"
        } else if chars(3) == "KDK" {
            "Kodak"
        } else if make_is("Canon") {
            "Canon"
        } else if starts("CASIO") {
            "Casio"
        } else if chars(8) == "FUJIFILM" || make_is("Fujifilm") {
            "Fujifilm"
        } else if chars(7) == "KYOCERA" {
            "Kyocera"
        } else if chars(5) == "LEICA" {
            let eight = chars(8);
            let leica_type5 = [
                "LEICA\0\u{1}\0",
                "LEICA\0\u{4}\0",
                "LEICA\0\u{5}\0",
                "LEICA\0\u{6}\0",
                "LEICA\0\u{7}\0",
            ];
            if leica_type5.contains(&eight.as_str())
                || make.as_deref() == Some("Leica Camera AG")
                || make.as_deref() == Some("LEICA")
            {
                "Leica"
            } else {
                return Ok(false);
            }
        } else if chars(12) == "Panasonic\0\0\0" {
            "Panasonic"
        } else if chars(4) == "AOC\0" {
            "Casio"
        } else if starts("PENTAX") || starts("ASAHI") {
            "Pentax"
        } else if chars(8) == "SANYO\0\u{1}\0" {
            "Sanyo"
        } else if make
            .as_deref()
            .is_some_and(|make| make.to_lowercase().starts_with("ricoh"))
        {
            if chars(2) == "Rv" || chars(3) == "Rev" {
                return Ok(false);
            }
            if crate::java::equals_ignore_case(&chars(5), "Ricoh") {
                "Ricoh"
            } else {
                return Ok(true);
            }
        } else if chars(10) == "Apple iOS\0" {
            let order = reader.motorola;
            reader.motorola = true;
            self.push(Kind::AppleMakernote);
            let read = process_ifd(self, reader, processed, offset + 14, offset);
            reader.motorola = order;
            read?;
            return Ok(true);
        } else if reader.u16(offset)? == 0x0001 {
            "Reconyx HyperFire"
        } else if crate::java::equals_ignore_case(&chars(9), "RECONYXUF")
            || crate::java::equals_ignore_case(&chars(9), "RECONYXH2")
        {
            "Reconyx"
        } else if make_is("SAMSUNG") {
            "Samsung"
        } else if make_is("DJI") {
            "DJI"
        } else {
            return Ok(false);
        };
        Err(Unported::new(format!("an Exif maker note of {maker}")).into())
    }
}

/// `processPrintIM`. A size that reads well only in the other byte order leaves the reader in
/// that order when the size is still wrong, as metadata-extractor leaves it.
fn process_print_im(
    directory: &mut Directory,
    offset: i64,
    reader: &mut Reader,
    byte_count: i64,
) -> Result<(), TiffError> {
    if byte_count == 0 {
        directory.add_error("Empty PrintIM data");
        return Ok(());
    }
    if byte_count <= 15 {
        directory.add_error("Bad PrintIM data");
        return Ok(());
    }
    let header = java_string(reader.bytes(offset, 12)?, Some(Charset::named("UTF-8")));
    if !header.starts_with("PrintIM") {
        directory.add_error("Invalid PrintIM header");
        return Ok(());
    }
    let mut reset = None;
    let mut count = i64::from(reader.u16(offset + 14)?);
    if byte_count < 16 + count * 6 {
        reset = Some(reader.motorola);
        reader.motorola = !reader.motorola;
        count = i64::from(reader.u16(offset + 14)?);
        if byte_count < 16 + count * 6 {
            directory.add_error("Bad PrintIM size");
            return Ok(());
        }
    }
    let units: Vec<u16> = header.encode_utf16().collect();
    if units.len() >= 12 {
        directory.set(0, Value::Text(String::from_utf16_lossy(&units[8..12])));
    }
    for entry in 0..count {
        let position = offset + 16 + entry * 6;
        let tag = i32::from(reader.u16(position)?);
        let value = i64::from(reader.u32(position + 2)?);
        directory.set(tag, Value::Long(value));
    }
    if let Some(order) = reset {
        reader.motorola = order;
    }
    Ok(())
}

/// `TiffReader.processTiff` into `metadata` with an `ExifTiffHandler`, the TIFF header at
/// `header` in `data`.
pub(crate) fn read_tiff(
    data: &[u8],
    header: i64,
    metadata: &mut Metadata,
) -> Result<(), TiffError> {
    let mut reader = Reader::new(data);
    let mut handler = Handler {
        metadata,
        current: None,
        stack: Vec::new(),
        exif_start: header as i32,
    };
    process_tiff(&mut reader, &mut handler, header)
}

fn process_tiff(reader: &mut Reader, handler: &mut Handler, header: i64) -> Result<(), TiffError> {
    match reader.i16(header)? {
        0x4d4d => reader.motorola = true,
        0x4949 => reader.motorola = false,
        other => {
            return Err(TiffError::Processing(format!(
                "Unclear distinction between Motorola/Intel byte ordering: {other}"
            )))
        }
    }
    let marker = reader.u16(2 + header)?;
    handler.set_tiff_marker(marker)?;
    let mut first = i64::from(reader.i32(4 + header)?) + header;
    if first >= reader.len() - 1 {
        handler.error(
            "First IFD offset is beyond the end of the TIFF data segment -- trying default offset",
        );
        first = header + 2 + 2 + 4;
    }
    let mut processed = HashSet::new();
    process_ifd(handler, reader, &mut processed, first, header)
}

/// `TiffReader.processIfd`: the IFD at `offset`, then the sub-IFDs its tags point at and the
/// IFD that follows it; each IFD ends, whatever happens, with `endingIFD` and the byte order it
/// started with. Java follows the chain of IFDs, a page of a TIFF each, by recursion; it is
/// followed here in a loop, the IFDs ending together at its end, so that a TIFF of any number of
/// pages is read in a bounded stack.
fn process_ifd(
    handler: &mut Handler,
    reader: &mut Reader,
    processed: &mut HashSet<i64>,
    offset: i64,
    header: i64,
) -> Result<(), TiffError> {
    let mut offset = offset;
    let mut chained = 0usize;
    let mut first_order = None;
    let result = loop {
        let mut reset = None;
        let followed = process_ifd_body(handler, reader, processed, offset, header, &mut reset);
        chained += 1;
        first_order = first_order.or(reset);
        match followed {
            Ok(Some(follower)) => offset = follower,
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        }
    };
    for _ in 0..chained {
        handler.ending_ifd();
    }
    if let Some(order) = first_order {
        reader.motorola = order;
    }
    result
}

/// The IFD at `offset` and the sub-IFDs its tags point at: the offset of the IFD that follows
/// it, if the handler follows it.
fn process_ifd_body(
    handler: &mut Handler,
    reader: &mut Reader,
    processed: &mut HashSet<i64>,
    offset: i64,
    header: i64,
    reset: &mut Option<bool>,
) -> Result<Option<i64>, TiffError> {
    let offset_key = i64::from(offset as i32);
    if !processed.insert(offset_key) {
        return Ok(None);
    }
    if offset >= reader.len() || offset < 0 {
        handler.error("Ignored IFD marked to start outside data segment");
        return Ok(None);
    }
    let mut count = i64::from(reader.u16(offset)?);
    if count > 0xFF && count & 0xFF == 0 {
        *reset = Some(reader.motorola);
        count >>= 8;
        reader.motorola = !reader.motorola;
    }
    let length = 2 + 12 * count + 4;
    if length + offset > reader.len() {
        handler.error("Illegally sized IFD");
        return Ok(None);
    }
    let mut invalid_formats = 0;
    for number in 0..count {
        let entry = offset + 2 + 12 * number;
        let tag = i32::from(reader.u16(entry)?);
        let format = reader.u16(entry + 2)?;
        let components = i64::from(reader.u32(entry + 4)?);
        let byte_count = match component_size(format) {
            Some(size) => components * size,
            None => match format {
                13 => components * 4,
                0 => 0,
                _ => {
                    handler.error(format!(
                        "Invalid TIFF tag format code {format} for tag 0x{tag:04X}"
                    ));
                    invalid_formats += 1;
                    if invalid_formats > 5 {
                        handler.error("Stopping processing as too many errors seen in TIFF IFD");
                        return Ok(None);
                    }
                    continue;
                }
            },
        };
        let value_offset = if byte_count > 4 {
            let pointer = i64::from(reader.u32(entry + 8)?);
            if pointer + byte_count > reader.len() {
                handler.error("Illegal TIFF tag pointer offset");
                continue;
            }
            header + pointer
        } else {
            entry + 8
        };
        if value_offset < 0 || value_offset > reader.len() {
            handler.error("Illegal TIFF tag pointer offset");
            continue;
        }
        if byte_count < 0 || value_offset + byte_count > reader.len() {
            handler.error(format!(
                "Illegal number of bytes for TIFF tag data: {byte_count}"
            ));
            continue;
        }
        let mut is_ifd_pointer = false;
        if byte_count == 4 * components {
            for index in 0..components {
                if handler.try_enter_sub_ifd(tag) {
                    is_ifd_pointer = true;
                    let sub = header + i64::from(reader.i32(value_offset + index * 4)?);
                    process_ifd(handler, reader, processed, sub, header)?;
                }
            }
        }
        if !is_ifd_pointer
            && !handler.custom_process_tag(value_offset, reader, tag, byte_count, processed)?
        {
            process_tag(
                handler,
                tag,
                value_offset,
                components as i32,
                format,
                reader,
            )?;
        }
    }
    let next = reader.i32(offset + 2 + 12 * count)?;
    if next != 0 {
        let next = i64::from(next) + header;
        if next >= reader.len() || next < offset {
            return Ok(None);
        }
        if handler.has_follower_ifd() {
            return Ok(Some(next));
        }
    }
    Ok(None)
}

/// `TiffReader.processTag`: the tag's value, kept as the Java type metadata-extractor keeps it.
fn process_tag(
    handler: &mut Handler,
    tag: i32,
    offset: i64,
    count: i32,
    format: u16,
    reader: &Reader,
) -> Result<(), TiffError> {
    let count = i64::from(count);
    let many = |size: i64| (0..count).map(move |index| offset + index * size);
    let value = match format {
        CODE_UNDEFINED => Value::Bytes(reader.bytes(offset, count)?.to_vec()),
        CODE_STRING => {
            Value::StringValue(reader.null_terminated_string_value(offset, count, None)?)
        }
        CODE_RATIONAL_S | CODE_RATIONAL_U => {
            let read = |at: i64| -> Result<Rational, Bounds> {
                Ok(if format == CODE_RATIONAL_S {
                    Rational::new(i64::from(reader.i32(at)?), i64::from(reader.i32(at + 4)?))
                } else {
                    Rational::new(i64::from(reader.u32(at)?), i64::from(reader.u32(at + 4)?))
                })
            };
            match count {
                1 => Value::Rational(read(offset)?),
                2.. => Value::Rationals(many(8).map(read).collect::<Result<_, _>>()?),
                _ => return Ok(()),
            }
        }
        CODE_SINGLE => match count {
            1 => Value::Float(reader.f32(offset)?),
            _ => Value::Floats(many(4).map(|at| reader.f32(at)).collect::<Result<_, _>>()?),
        },
        CODE_DOUBLE => match count {
            1 => Value::Double(reader.f64(offset)?),
            _ => Value::Doubles(many(8).map(|at| reader.f64(at)).collect::<Result<_, _>>()?),
        },
        CODE_INT8_S => match count {
            1 => Value::Int(i32::from(reader.i8(offset)?)),
            _ => Value::Bytes(
                many(1)
                    .map(|at| reader.i8(at).map(|byte| byte as u8))
                    .collect::<Result<_, _>>()?,
            ),
        },
        CODE_INT8_U => match count {
            1 => Value::Int(i32::from(reader.u8(offset)?)),
            _ => Value::Shorts(
                many(1)
                    .map(|at| reader.u8(at).map(i16::from))
                    .collect::<Result<_, _>>()?,
            ),
        },
        CODE_INT16_S => match count {
            1 => Value::Int(i32::from(reader.i16(offset)?)),
            _ => Value::Shorts(many(2).map(|at| reader.i16(at)).collect::<Result<_, _>>()?),
        },
        CODE_INT16_U => match count {
            1 => Value::Int(i32::from(reader.u16(offset)?)),
            _ => Value::Ints(
                many(2)
                    .map(|at| reader.u16(at).map(i32::from))
                    .collect::<Result<_, _>>()?,
            ),
        },
        CODE_INT32_S => match count {
            1 => Value::Int(reader.i32(offset)?),
            _ => Value::Ints(many(4).map(|at| reader.i32(at)).collect::<Result<_, _>>()?),
        },
        CODE_INT32_U => match count {
            1 => Value::Long(i64::from(reader.u32(offset)?)),
            _ => Value::Longs(
                many(4)
                    .map(|at| reader.u32(at).map(i64::from))
                    .collect::<Result<_, _>>()?,
            ),
        },
        CODE_INT64_S | CODE_INT64_U => match count {
            1 => Value::Long(reader.i64(offset)?),
            _ => Value::Longs(many(8).map(|at| reader.i64(at)).collect::<Result<_, _>>()?),
        },
        _ => {
            handler.error(format!(
                "Invalid TIFF tag format code {format} for tag 0x{tag:04X}"
            ));
            return Ok(());
        }
    };
    handler.directory().set(tag, value);
    Ok(())
}
