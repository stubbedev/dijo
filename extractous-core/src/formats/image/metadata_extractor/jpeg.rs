//! metadata-extractor's `JpegSegmentReader` and the JPEG segment readers Tika's `JpegParser`
//! runs (every one but `XmpReader`), in their order: the frame header, comments, JFIF and JFXX,
//! Exif, ICC, Photoshop, Ducky, IPTC, Adobe and the Huffman tables, with the descriptors of the
//! directories they make.

use super::descriptions::{base, indexed, named, Described};
use super::reader::{Bounds, Reader, Sequential};
use super::{
    exif, icc, iptc, java_string, photoshop, Directory, Kind, Metadata, StringValue, Unported,
    Value,
};
use crate::text::charset::Charset;

const SOF0: u8 = 0xC0;
/// The frame types `JpegReader` reads, in its order.
const SOF_TYPES: [u8; 13] = [
    0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF,
];
const DHT: u8 = 0xC4;
const DNL: u8 = 0xDC;
const COM: u8 = 0xFE;
const APP0: u8 = 0xE0;
const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;
const APPC: u8 = 0xEC;
const APPD: u8 = 0xED;
const APPE: u8 = 0xEE;
const SOS: u8 = 0xDA;
const EOI: u8 = 0xD9;

pub(crate) const TAG_COMPRESSION_TYPE: i32 = -3;
pub(crate) const TAG_DATA_PRECISION: i32 = 0;
pub(crate) const TAG_IMAGE_HEIGHT: i32 = 1;
pub(crate) const TAG_IMAGE_WIDTH: i32 = 3;
const TAG_NUMBER_OF_COMPONENTS: i32 = 5;
const TAG_COMPONENT_DATA_1: i32 = 6;
pub(crate) const TAG_COMMENT: i32 = 0;
const JFIF_VERSION: i32 = 5;
const JFIF_UNITS: i32 = 7;
const JFIF_RES_X: i32 = 8;
const JFIF_RES_Y: i32 = 10;
const JFIF_THUMB_WIDTH: i32 = 12;
const JFIF_THUMB_HEIGHT: i32 = 13;
const JFXX_EXTENSION_CODE: i32 = 5;
const ADOBE_DCT_ENCODE_VERSION: i32 = 0;
const ADOBE_FLAGS0: i32 = 1;
const ADOBE_FLAGS1: i32 = 2;
const ADOBE_COLOR_TRANSFORM: i32 = 3;
const DUCKY_QUALITY: i32 = 1;
const DUCKY_COMMENT: i32 = 2;
const DUCKY_COPYRIGHT: i32 = 3;
const HUFFMAN_NUMBER_OF_TABLES: i32 = 1;

/// Why the segments could not be read: `JpegProcessingException`, or the end of the data
/// (`EOFException`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SegmentError {
    Processing(String),
    End(Bounds),
}

impl From<Bounds> for SegmentError {
    fn from(bounds: Bounds) -> Self {
        Self::End(bounds)
    }
}

/// The segments of a JPEG up to its first scan, as `JpegSegmentData` keeps them.
#[derive(Debug, Default)]
pub(crate) struct Segments<'a> {
    segments: Vec<(u8, &'a [u8])>,
}

impl<'a> Segments<'a> {
    /// `getSegments(type)`: the segments of a type, in file order.
    pub(crate) fn of(&self, kind: u8) -> Vec<&'a [u8]> {
        self.segments
            .iter()
            .filter(|(segment_kind, _)| *segment_kind == kind)
            .map(|(_, bytes)| *bytes)
            .collect()
    }
}

/// `JpegSegmentReader.readSegments`: the segments of the types `wanted` keeps (all when none),
/// up to the start of the scan or the end of the image.
pub(crate) fn read_segments<'a>(
    data: &'a [u8],
    wanted: Option<&dyn Fn(u8) -> bool>,
) -> Result<Segments<'a>, SegmentError> {
    let mut reader = Sequential::new(data);
    let magic = reader.u16()?;
    if magic != 0xFFD8 {
        return Err(SegmentError::Processing(format!(
            "JPEG data is expected to begin with 0xFFD8 (ÿØ) not 0x{magic:x}"
        )));
    }
    let mut segments = Segments::default();
    loop {
        let mut identifier = reader.u8()?;
        let mut kind = reader.u8()?;
        while identifier != 0xFF || kind == 0xFF || kind == 0 {
            identifier = kind;
            kind = reader.u8()?;
        }
        if kind == SOS || kind == EOI {
            return Ok(segments);
        }
        let length = i32::from(reader.u16()?) - 2;
        if length < 0 {
            return Err(SegmentError::Processing(
                "JPEG segment size would be less than zero".to_owned(),
            ));
        }
        let length = length as usize;
        if wanted.is_none_or(|wanted| wanted(kind)) {
            segments.segments.push((kind, reader.bytes(length)?));
        } else if !reader.try_skip(length) {
            return Ok(segments);
        }
    }
}

/// Whether a segment of this type is one Tika's JPEG readers read.
pub(crate) fn read_by_tika(kind: u8) -> bool {
    SOF_TYPES.contains(&kind) || [COM, APP0, APP1, APP2, APPC, APPD, APPE, DHT, DNL].contains(&kind)
}

/// Runs Tika's JPEG readers, in their order, on the segments.
pub(crate) fn read(segments: &Segments, metadata: &mut Metadata) -> Result<(), Unported> {
    for kind in SOF_TYPES {
        for segment in segments.of(kind) {
            read_frame(segment, kind, metadata);
        }
    }
    for segment in segments.of(COM) {
        let mut directory = Directory::new(Kind::JpegComment);
        directory.set(
            TAG_COMMENT,
            Value::StringValue(StringValue {
                bytes: segment.to_vec(),
                charset: None,
            }),
        );
        metadata.add(directory);
    }
    let app0 = segments.of(APP0);
    for segment in &app0 {
        if starts_with(segment, "JFIF") {
            read_jfif(segment, metadata);
        }
    }
    for segment in &app0 {
        if starts_with(segment, "JFXX") {
            let mut directory = Directory::new(Kind::Jfxx);
            match Reader::new(segment).u8(i64::from(JFXX_EXTENSION_CODE)) {
                Ok(code) => directory.set(JFXX_EXTENSION_CODE, Value::Int(i32::from(code))),
                Err(bounds) => directory.add_error(bounds.0),
            }
            metadata.add(directory);
        }
    }
    for segment in segments.of(APP1) {
        if segment.len() >= 6 && java_string(&segment[..6], None) == "Exif\0\0" {
            match exif::read_tiff(segment, 6, metadata) {
                Ok(()) | Err(exif::TiffError::Bounds(_) | exif::TiffError::Processing(_)) => {}
                Err(exif::TiffError::Unported(unported)) => return Err(unported),
            }
        }
    }
    icc::read_jpeg_segments(&segments.of(APP2), metadata)?;
    photoshop::read_jpeg_segments(&segments.of(APPD), metadata)?;
    for segment in segments.of(APPC) {
        if segment.len() >= 5 && java_string(&segment[..5], None) == "Ducky" {
            read_ducky(&segment[5..], metadata)?;
        }
    }
    for segment in segments.of(APPD) {
        if segment.first() == Some(&0x1c) {
            iptc::extract(segment, metadata)?;
        }
    }
    for segment in segments.of(APPE) {
        if segment.len() == 12
            && crate::formats::java::equals_ignore_case(&java_string(&segment[..5], None), "Adobe")
        {
            read_adobe(segment, metadata);
        }
    }
    let mut tables = 0;
    for segment in segments.of(DHT) {
        read_huffman(segment, metadata, &mut tables);
    }
    for segment in segments.of(DNL) {
        read_dnl(segment, metadata);
    }
    Ok(())
}

fn starts_with(segment: &[u8], preamble: &str) -> bool {
    segment.len() >= preamble.len() && java_string(&segment[..preamble.len()], None) == preamble
}

/// `JpegReader.extract`: the frame header's precision, size and components.
fn read_frame(segment: &[u8], kind: u8, metadata: &mut Metadata) {
    let mut directory = Directory::new(Kind::Jpeg);
    directory.set(TAG_COMPRESSION_TYPE, Value::Int(i32::from(kind - SOF0)));
    if segment.len() < 6 {
        directory.add_error("Insufficient bytes for JPEG segment header.");
        metadata.add(directory);
        return;
    }
    let mut reader = Sequential::new(segment);
    let result = (|| -> Result<(), Bounds> {
        directory.set(TAG_DATA_PRECISION, Value::Int(i32::from(reader.u8()?)));
        directory.set(TAG_IMAGE_HEIGHT, Value::Int(i32::from(reader.u16()?)));
        directory.set(TAG_IMAGE_WIDTH, Value::Int(i32::from(reader.u16()?)));
        let components = reader.u8()?;
        directory.set(TAG_NUMBER_OF_COMPONENTS, Value::Int(i32::from(components)));
        if reader.available() < usize::from(components) * 3 {
            directory
                .add_error("Insufficient bytes for JPEG the requested number of JPEG components.");
            return Ok(());
        }
        for index in 0..i32::from(components) {
            let id = i32::from(reader.u8()?);
            let sampling = i32::from(reader.u8()?);
            let quantization = i32::from(reader.u8()?);
            directory.set(
                TAG_COMPONENT_DATA_1 + index,
                Value::Component {
                    id,
                    sampling,
                    quantization,
                },
            );
        }
        Ok(())
    })();
    if let Err(bounds) = result {
        directory.add_error(bounds.0);
    }
    metadata.add(directory);
}

fn read_jfif(segment: &[u8], metadata: &mut Metadata) {
    let mut directory = Directory::new(Kind::Jfif);
    let reader = Reader::new(segment);
    let result = (|| -> Result<(), Bounds> {
        let at = i64::from;
        directory.set(
            JFIF_VERSION,
            Value::Int(i32::from(reader.u16(at(JFIF_VERSION))?)),
        );
        directory.set(
            JFIF_UNITS,
            Value::Int(i32::from(reader.u8(at(JFIF_UNITS))?)),
        );
        directory.set(
            JFIF_RES_X,
            Value::Int(i32::from(reader.u16(at(JFIF_RES_X))?)),
        );
        directory.set(
            JFIF_RES_Y,
            Value::Int(i32::from(reader.u16(at(JFIF_RES_Y))?)),
        );
        directory.set(
            JFIF_THUMB_WIDTH,
            Value::Int(i32::from(reader.u8(at(JFIF_THUMB_WIDTH))?)),
        );
        directory.set(
            JFIF_THUMB_HEIGHT,
            Value::Int(i32::from(reader.u8(at(JFIF_THUMB_HEIGHT))?)),
        );
        Ok(())
    })();
    if let Err(bounds) = result {
        directory.add_error(bounds.0);
    }
    metadata.add(directory);
}

/// `DuckyReader.extract` of the data after the preamble.
fn read_ducky(data: &[u8], metadata: &mut Metadata) -> Result<(), Unported> {
    let mut directory = Directory::new(Kind::Ducky);
    let mut reader = Sequential::new(data);
    let result = (|| -> Result<(), DuckyError> {
        loop {
            let tag = i32::from(reader.u16()?);
            if tag == 0 {
                return Ok(());
            }
            let length = usize::from(reader.u16()?);
            match tag {
                DUCKY_QUALITY => {
                    if length != 4 {
                        directory.add_error("Unexpected length for the quality tag");
                        return Ok(());
                    }
                    directory.set(tag, Value::Int(reader.i32()?));
                }
                DUCKY_COMMENT | DUCKY_COPYRIGHT => {
                    reader.skip(4)?;
                    let Some(text) = length.checked_sub(4) else {
                        return Err(DuckyError::Unported(Unported::new(
                            "a Ducky text shorter than its header, which metadata-extractor throws on",
                        )));
                    };
                    directory.set(
                        tag,
                        Value::StringValue(StringValue {
                            bytes: reader.bytes(text)?.to_vec(),
                            charset: Some(Charset::named("UTF-16BE")),
                        }),
                    );
                }
                _ => directory.set(tag, Value::Bytes(reader.bytes(length)?.to_vec())),
            }
        }
    })();
    metadata.add(directory);
    match result {
        Ok(()) => Ok(()),
        Err(DuckyError::Bounds(bounds)) => {
            let index = metadata.directories.len() - 1;
            metadata.directories[index].add_error(bounds.0);
            Ok(())
        }
        Err(DuckyError::Unported(unported)) => Err(unported),
    }
}

enum DuckyError {
    Bounds(Bounds),
    Unported(Unported),
}

impl From<Bounds> for DuckyError {
    fn from(bounds: Bounds) -> Self {
        Self::Bounds(bounds)
    }
}

/// `AdobeJpegReader.extract`: an APP14 segment, read in Intel byte order.
fn read_adobe(segment: &[u8], metadata: &mut Metadata) {
    let mut directory = Directory::new(Kind::Adobe);
    let mut reader = Sequential::new(segment);
    reader.motorola = false;
    let result = (|| -> Result<(), Bounds> {
        if java_string(reader.bytes(5)?, None) != "Adobe" {
            directory.add_error("Invalid Adobe JPEG data header.");
            return Ok(());
        }
        directory.set(
            ADOBE_DCT_ENCODE_VERSION,
            Value::Int(i32::from(reader.u16()?)),
        );
        directory.set(ADOBE_FLAGS0, Value::Int(i32::from(reader.u16()?)));
        directory.set(ADOBE_FLAGS1, Value::Int(i32::from(reader.u16()?)));
        directory.set(ADOBE_COLOR_TRANSFORM, Value::Int(i32::from(reader.i8()?)));
        Ok(())
    })();
    if let Err(bounds) = result {
        directory.add_error(format!("IO exception processing data: {}", bounds.0));
    }
    metadata.add(directory);
}

/// `JpegDhtReader.extract`: the tables of a DHT segment, counted in the one Huffman directory.
fn read_huffman(segment: &[u8], metadata: &mut Metadata, tables: &mut i32) {
    let index = match metadata.first_index(|kind| kind == Kind::Huffman) {
        Some(index) => index,
        None => {
            metadata.add(Directory::new(Kind::Huffman));
            metadata.directories.len() - 1
        }
    };
    let mut reader = Sequential::new(segment);
    let unstuffed = |reader: &mut Sequential, count: usize| -> Result<usize, Bounds> {
        let mut total = 0;
        for _ in 0..count {
            let byte = reader.u8()?;
            if byte == 0xFF {
                let stuffing = reader.u8()?;
                if stuffing != 0 {
                    return Err(Bounds(format!(
                        "Marker {stuffing} found inside DHT segment"
                    )));
                }
            }
            total += usize::from(byte);
        }
        Ok(total)
    };
    let result = (|| -> Result<(), Bounds> {
        while reader.available() > 0 {
            reader.u8()?;
            let values = unstuffed(&mut reader, 16)?;
            unstuffed(&mut reader, values)?;
            *tables += 1;
        }
        Ok(())
    })();
    let directory = &mut metadata.directories[index];
    if let Err(bounds) = result {
        directory.add_error(bounds.0);
    }
    directory.set(HUFFMAN_NUMBER_OF_TABLES, Value::Int(*tables));
}

/// `JpegDnlReader.extract`: the image height a DNL segment gives a frame header without one.
fn read_dnl(segment: &[u8], metadata: &mut Metadata) {
    let Some(index) = metadata.first_index(|kind| kind == Kind::Jpeg) else {
        let mut error = Directory::new(Kind::Error);
        error.add_error("DNL segment found without SOFx - illegal JPEG format");
        metadata.add(error);
        return;
    };
    let directory = &mut metadata.directories[index];
    let height = directory.integer(TAG_IMAGE_HEIGHT);
    if height.is_none() || height == Some(0) {
        match Sequential::new(segment).u16() {
            Ok(value) => directory.set(TAG_IMAGE_HEIGHT, Value::Int(i32::from(value))),
            Err(bounds) => directory.add_error(bounds.0),
        }
    }
}

/// `JpegComponent.getComponentName`.
fn component_name(id: i32) -> String {
    match id {
        1 => "Y".to_owned(),
        2 => "Cb".to_owned(),
        3 => "Cr".to_owned(),
        4 => "I".to_owned(),
        5 => "Q".to_owned(),
        other => format!("Unknown ({other})"),
    }
}

/// `JpegComponent.toString`.
pub(crate) fn component_text(sampling: i32, quantization: i32) -> String {
    format!(
        "Quantization table {quantization}, Sampling factors {} horiz/{} vert",
        (sampling >> 4) & 0x0F,
        sampling & 0x0F
    )
}

/// The descriptors of the directories the JPEG readers make.
pub(crate) fn describe(directory: &Directory, tag: i32) -> Described {
    let d = directory;
    Ok(match (d.kind, tag) {
        (Kind::Jpeg, TAG_COMPRESSION_TYPE) => indexed(
            d,
            tag,
            0,
            &[
                Some("Baseline"),
                Some("Extended sequential, Huffman"),
                Some("Progressive, Huffman"),
                Some("Lossless, Huffman"),
                None,
                Some("Differential sequential, Huffman"),
                Some("Differential progressive, Huffman"),
                Some("Differential lossless, Huffman"),
                Some("Reserved for JPEG extensions"),
                Some("Extended sequential, arithmetic"),
                Some("Progressive, arithmetic"),
                Some("Lossless, arithmetic"),
                None,
                Some("Differential sequential, arithmetic"),
                Some("Differential progressive, arithmetic"),
                Some("Differential lossless, arithmetic"),
            ],
        ),
        (Kind::Jpeg, TAG_COMPONENT_DATA_1..=9) => match d.object(tag) {
            Some(Value::Component {
                id,
                sampling,
                quantization,
            }) => Some(format!(
                "{} component: {}",
                component_name(*id),
                component_text(*sampling, *quantization)
            )),
            _ => None,
        },
        (Kind::Jpeg, TAG_DATA_PRECISION) => d.string(tag).map(|value| format!("{value} bits")),
        (Kind::Jpeg, TAG_IMAGE_HEIGHT | TAG_IMAGE_WIDTH) => {
            d.string(tag).map(|value| format!("{value} pixels"))
        }
        (Kind::Jfif, JFIF_VERSION) => d
            .integer(tag)
            .map(|value| format!("{}.{}", (value & 0xFF00) >> 8, value & 0xFF)),
        (Kind::Jfif, JFIF_RES_X | JFIF_RES_Y) => d
            .integer(tag)
            .map(|value| format!("{value} dot{}", if value == 1 { "" } else { "s" })),
        (Kind::Jfif, JFIF_UNITS) => d.integer(tag).map(|value| {
            match value {
                0 => "none",
                1 => "inch",
                2 => "centimetre",
                _ => "unit",
            }
            .to_owned()
        }),
        (Kind::Jfxx, JFXX_EXTENSION_CODE) => d.integer(tag).map(|value| match value {
            0x10 => "Thumbnail coded using JPEG".to_owned(),
            0x11 => "Thumbnail stored using 1 byte/pixel".to_owned(),
            0x13 => "Thumbnail stored using 3 bytes/pixel".to_owned(),
            other => format!("Unknown extension code {other}"),
        }),
        (Kind::Adobe, ADOBE_COLOR_TRANSFORM) => {
            named(d, tag, 0, &["Unknown (RGB or CMYK)", "YCbCr", "YCCK"])
        }
        (Kind::Adobe, ADOBE_DCT_ENCODE_VERSION) => d.integer(tag).map(|value| {
            if value == 0x64 {
                "100".to_owned()
            } else {
                value.to_string()
            }
        }),
        (Kind::Huffman, HUFFMAN_NUMBER_OF_TABLES) => d.integer(tag).map(|value| {
            format!(
                "{value}{}",
                if value == 1 {
                    " Huffman table"
                } else {
                    " Huffman tables"
                }
            )
        }),
        _ => base(d, tag),
    })
}
