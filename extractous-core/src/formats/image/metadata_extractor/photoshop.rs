//! metadata-extractor's `PhotoshopReader`, `PhotoshopDirectory` and `PhotoshopDescriptor`: the
//! image resource blocks (`8BIM`) Photoshop writes, with the IPTC, ICC and Exif data some of them
//! hold read into directories of their own.

use super::descriptions::{base, decimal, Described};
use super::reader::{Bounds, Reader, Sequential};
use super::{exif, icc, iptc, java_string, Directory, Kind, Metadata, Unported, Value};
use crate::java::dtoa::{java_double_to_string, java_float_to_string};
use crate::text::charset::Charset;

const TAG_XML: i32 = 0x03ea;
const TAG_RESOLUTION_INFO: i32 = 0x03ed;
const TAG_IPTC: i32 = 0x0404;
const TAG_JPEG_QUALITY: i32 = 0x0406;
const TAG_THUMBNAIL_OLD: i32 = 0x0409;
const TAG_COPYRIGHT: i32 = 0x040a;
const TAG_URL: i32 = 0x040b;
const TAG_THUMBNAIL: i32 = 0x040c;
const TAG_GLOBAL_ANGLE: i32 = 0x040d;
const TAG_ICC_PROFILE_BYTES: i32 = 0x040f;
const TAG_SEED_NUMBER: i32 = 0x0414;
const TAG_GLOBAL_ALTITUDE: i32 = 0x0419;
const TAG_SLICES: i32 = 0x041a;
const TAG_URL_LIST: i32 = 0x041e;
const TAG_VERSION: i32 = 0x0421;
const TAG_EXIF_DATA_1: i32 = 0x0422;
const TAG_EXIF_DATA_3: i32 = 0x0423;
const TAG_XMP_DATA: i32 = 0x0424;
const TAG_PRINT_SCALE: i32 = 0x0426;
const TAG_PIXEL_ASPECT_RATIO: i32 = 0x0428;
const TAG_CLIPPING_PATH_NAME: i32 = 0x0bb7;
const FIRST_PATH: i32 = 0x07d0;
const LAST_PATH: i32 = 0x0bb6;
const FIRST_PLUG_IN: i32 = 0x0fa0;
const LAST_PLUG_IN: i32 = 0x1387;

/// The preamble of a JPEG APP13 segment of Photoshop resources.
const JPEG_PREAMBLE: &str = "Photoshop 3.0";

/// The names `PhotoshopReader` puts in the directory's name map as it reads clipping paths and
/// plug-in data, which every Photoshop directory of the process names its tags with from then
/// on: a tag of these ranges is only ever set where its name has been put.
pub(crate) fn dynamic_name(tag: i32) -> Option<String> {
    match tag {
        FIRST_PATH..=LAST_PATH => Some(format!("Path Info {}", tag - (FIRST_PATH - 1))),
        FIRST_PLUG_IN..=LAST_PLUG_IN => Some(format!("Plug-in {} Data", tag - FIRST_PLUG_IN + 1)),
        _ => None,
    }
}

/// `PhotoshopReader.readJpegSegments`.
pub(crate) fn read_jpeg_segments(
    segments: &[&[u8]],
    metadata: &mut Metadata,
) -> Result<(), Unported> {
    let preamble = JPEG_PREAMBLE.len();
    for segment in segments {
        if segment.len() < preamble + 1 || java_string(&segment[..preamble], None) != JPEG_PREAMBLE
        {
            continue;
        }
        read(&segment[preamble + 1..], metadata)?;
    }
    Ok(())
}

/// `PhotoshopReader.extract` of resources held in a TIFF tag.
pub(crate) fn extract(bytes: &[u8], metadata: &mut Metadata) -> Result<(), Unported> {
    read(bytes, metadata)
}

fn read(bytes: &[u8], metadata: &mut Metadata) -> Result<(), Unported> {
    metadata.add(Directory::new(Kind::Photoshop));
    let index = metadata.directories.len() - 1;
    let mut reader = Sequential::new(bytes);
    let length = bytes.len() as i64;
    let mut position: i64 = 0;
    let mut clipping_paths = 0;
    while position < length {
        match read_block(
            &mut reader,
            &mut position,
            length,
            &mut clipping_paths,
            index,
            metadata,
        ) {
            Ok(()) => {}
            Err(BlockError::Stopped(message)) => {
                metadata.directories[index].add_error(message);
                return Ok(());
            }
            Err(BlockError::Unported(unported)) => return Err(unported),
        }
    }
    Ok(())
}

enum BlockError {
    /// An exception the reader catches, ending the resources.
    Stopped(String),
    Unported(Unported),
}

impl From<Bounds> for BlockError {
    fn from(bounds: Bounds) -> Self {
        Self::Stopped(bounds.0)
    }
}

impl From<Unported> for BlockError {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

fn read_block(
    reader: &mut Sequential,
    position: &mut i64,
    length: i64,
    clipping_paths: &mut i32,
    index: usize,
    metadata: &mut Metadata,
) -> Result<(), BlockError> {
    let signature = java_string(reader.bytes(4)?, None);
    *position += 4;
    let tag = i32::from(reader.u16()?);
    *position += 2;
    let description_length = i64::from(reader.u8()?);
    *position += 1;
    if description_length + *position > length {
        return Err(BlockError::Stopped("Invalid string length".to_owned()));
    }
    let description: Vec<u8> = reader.bytes(description_length as usize)?.to_vec();
    *position += description_length;
    if *position % 2 != 0 {
        reader.skip(1)?;
        *position += 1;
    }
    let byte_count = reader.i32()?;
    *position += 4;
    let Ok(count) = usize::try_from(byte_count) else {
        return Err(BlockError::Stopped(byte_count.to_string()));
    };
    let mut data = reader.bytes(count)?.to_vec();
    *position += i64::from(byte_count);
    if *position % 2 != 0 {
        reader.skip(1)?;
        *position += 1;
    }
    if signature != "8BIM" {
        return Ok(());
    }
    match tag {
        TAG_IPTC => iptc::extract(&data, metadata)?,
        TAG_ICC_PROFILE_BYTES => icc::extract(&data, metadata),
        TAG_EXIF_DATA_1 | TAG_EXIF_DATA_3 => match exif::read_tiff(&data, 0, metadata) {
            Ok(()) | Err(exif::TiffError::Bounds(_) | exif::TiffError::Processing(_)) => {}
            Err(exif::TiffError::Unported(unported)) => return Err(unported.into()),
        },
        TAG_XMP_DATA => {
            return Err(
                Unported::new("XMP held in Photoshop resources, which XMPCore reads").into(),
            )
        }
        FIRST_PATH..=LAST_PATH => {
            *clipping_paths += 1;
            let old_length = data.len();
            let names = description.len();
            data.resize(old_length + names + 1, 0);
            let modulus = data.len() - 1;
            for i in old_length..data.len() {
                if modulus == 0 {
                    return Err(BlockError::Stopped("/ by zero".to_owned()));
                }
                data[i] = if i % modulus == 0 {
                    names as u8
                } else {
                    description[i - old_length]
                };
            }
            metadata.directories[index].set((FIRST_PATH - 1) + *clipping_paths, Value::Bytes(data));
        }
        _ => metadata.directories[index].set(tag, Value::Bytes(data)),
    }
    Ok(())
}

/// `PhotoshopDescriptor.getDescription`.
pub(crate) fn describe(directory: &Directory, tag: i32) -> Described {
    let bytes = || directory.bytes(tag);
    Ok(match tag {
        TAG_THUMBNAIL | TAG_THUMBNAIL_OLD => bytes().and_then(|bytes| thumbnail(&bytes)),
        TAG_URL | TAG_XML => bytes().map(|bytes| java_string(&bytes, None)),
        TAG_IPTC => bytes().map(|bytes| format!("{} bytes binary data", bytes.len())),
        TAG_SLICES => bytes().and_then(|bytes| slices(&bytes)),
        TAG_VERSION => bytes().and_then(|bytes| version(&bytes)),
        TAG_COPYRIGHT => bytes().and_then(|bytes| {
            bytes
                .first()
                .map(|first| if *first == 0 { "No" } else { "Yes" }.to_owned())
        }),
        TAG_RESOLUTION_INFO => bytes().and_then(|bytes| {
            let reader = Reader::new(&bytes);
            let x = reader.s15_fixed16(0).ok()?;
            let y = reader.s15_fixed16(8).ok()?;
            Some(format!(
                "{}x{} DPI",
                decimal("0.##", f64::from(x)),
                decimal("0.##", f64::from(y))
            ))
        }),
        TAG_GLOBAL_ANGLE | TAG_GLOBAL_ALTITUDE | TAG_URL_LIST | TAG_SEED_NUMBER => {
            bytes().and_then(|bytes| Reader::new(&bytes).i32(0).ok().map(|n| n.to_string()))
        }
        TAG_JPEG_QUALITY => match bytes() {
            Some(bytes) => jpeg_quality(&bytes),
            None => directory.string(tag),
        },
        TAG_PRINT_SCALE => bytes().and_then(|bytes| print_scale(&bytes)),
        TAG_PIXEL_ASPECT_RATIO => {
            bytes().and_then(|bytes| Reader::new(&bytes).f64(4).ok().map(java_double_to_string))
        }
        TAG_CLIPPING_PATH_NAME => bytes().and_then(|bytes| {
            let reader = Reader::new(&bytes);
            let length = reader.i8(0).ok()?;
            Some(java_string(
                reader.bytes(1, i64::from(length)).ok()?,
                Some(Charset::named("UTF-8")),
            ))
        }),
        FIRST_PATH..=LAST_PATH => bytes().and_then(|bytes| path(&bytes)),
        _ => base(directory, tag),
    })
}

fn jpeg_quality(bytes: &[u8]) -> Option<String> {
    let reader = Reader::new(bytes);
    let quality = i32::from(reader.u16(0).ok()?);
    let format = reader.u16(2).ok()?;
    let scans = reader.u16(4).ok()?;
    let shown = if (0xFFFD..=0xFFFF).contains(&quality) {
        quality - 0xFFFC
    } else if quality <= 8 {
        quality + 4
    } else {
        quality
    };
    let name = match quality {
        0xFFFD | 0xFFFE | 0xFFFF | 0 => "Low",
        1..=3 => "Medium",
        4 | 5 => "High",
        6..=8 => "Maximum",
        _ => "Unknown",
    };
    let format = match format {
        0x0000 => "Standard".to_owned(),
        0x0001 => "Optimised".to_owned(),
        0x0101 => "Progressive".to_owned(),
        other => format!("Unknown 0x{other:04X}"),
    };
    let scans = if (1..=3).contains(&scans) {
        (scans + 2).to_string()
    } else {
        format!("Unknown 0x{scans:04X}")
    };
    Some(format!("{shown} ({name}), {format} format, {scans} scans"))
}

fn print_scale(bytes: &[u8]) -> Option<String> {
    let reader = Reader::new(bytes);
    let style = reader.i32(0).ok()?;
    let x = java_float_to_string(reader.f32(2).ok()?);
    let y = java_float_to_string(reader.f32(6).ok()?);
    let scale = java_float_to_string(reader.f32(10).ok()?);
    Some(match style {
        0 => format!("Centered, Scale {scale}"),
        1 => "Size to fit".to_owned(),
        2 => format!("User defined, X:{x} Y:{y}, Scale:{scale}"),
        other => format!("Unknown {other:04X}, X:{x} Y:{y}, Scale:{scale}"),
    })
}

/// `reader.getString(index, length, "UTF-16")`.
fn utf16(reader: &Reader, index: i64, length: i64) -> Option<String> {
    Some(java_string(
        reader.bytes(index, length).ok()?,
        Some(Charset::named("UTF-16")),
    ))
}

fn version(bytes: &[u8]) -> Option<String> {
    let reader = Reader::new(bytes);
    let version = reader.i32(0).ok()?;
    let reader_length = i64::from(reader.i32(5).ok()?);
    let reader_name = utf16(&reader, 9, reader_length * 2)?;
    let mut position = 9 + reader_length * 2;
    let writer_length = i64::from(reader.i32(position).ok()?);
    position += 4;
    let writer_name = utf16(&reader, position, writer_length * 2)?;
    position += writer_length * 2;
    let file_version = reader.i32(position).ok()?;
    Some(format!(
        "{version} ({reader_name}, {writer_name}) {file_version}"
    ))
}

fn slices(bytes: &[u8]) -> Option<String> {
    let reader = Reader::new(bytes);
    let name_length = i64::from(reader.i32(20).ok()?);
    let name = utf16(&reader, 24, name_length * 2)?;
    let count = reader.i32(24 + name_length * 2).ok()?;
    Some(format!(
        "{name} ({},{},{},{}) {count} Slices",
        reader.i32(4).ok()?,
        reader.i32(8).ok()?,
        reader.i32(12).ok()?,
        reader.i32(16).ok()?
    ))
}

fn thumbnail(bytes: &[u8]) -> Option<String> {
    let reader = Reader::new(bytes);
    let format = reader.i32(0).ok()?;
    let width = reader.i32(4).ok()?;
    let height = reader.i32(8).ok()?;
    let total = reader.i32(16).ok()?;
    let compressed = reader.i32(20).ok()?;
    let bits = reader.i32(24).ok()?;
    Some(format!(
        "{}, {width}x{height}, Decomp {total} bytes, {bits} bpp, {compressed} bytes",
        if format == 1 { "JpegRGB" } else { "RawRGB" }
    ))
}

/// A Bézier knot of a path: whether it is linked, and its three points.
struct Knot {
    kind: &'static str,
    points: [f64; 6],
}

struct Subpath {
    kind: &'static str,
    knots: Vec<Knot>,
}

/// `getPathString`: a clipping path's name, fill rule and subpaths, or nothing where reading it
/// fails.
fn path(bytes: &[u8]) -> Option<String> {
    let reader = Reader::new(bytes);
    let length = reader.len();
    let name_length = i64::from(reader.i8(length - 1).ok()?);
    let records = (length - name_length - 1) as i32 / 26;
    let mut fill = None;
    let mut closed = Subpath {
        kind: "",
        knots: Vec::new(),
    };
    let mut open = Subpath {
        kind: "",
        knots: Vec::new(),
    };
    let mut paths: Vec<Subpath> = Vec::new();
    let knot = |kind: &'static str, spacer: i64| -> Option<Knot> {
        let mut points = [0.0; 6];
        for (j, point) in points.iter_mut().enumerate() {
            let at = j as i64 * 4 + spacer;
            *point = f64::from(reader.i8(at + 2).ok()?)
                + f64::from(reader_int24(&reader, at + 3)?) / 2f64.powi(24);
        }
        Some(Knot { kind, points })
    };
    for i in 0..i64::from(records.max(0)) {
        let spacer = 26 * i;
        match reader.i16(spacer).ok()? {
            0 => {
                let finished = std::mem::replace(
                    &mut closed,
                    Subpath {
                        kind: "Closed Subpath",
                        knots: Vec::new(),
                    },
                );
                if !finished.knots.is_empty() {
                    paths.push(finished);
                }
            }
            selector @ (1 | 2) => {
                closed.knots.push(knot(
                    if selector == 1 { "Linked" } else { "Unlinked" },
                    spacer,
                )?);
            }
            3 => {
                let finished = std::mem::replace(
                    &mut open,
                    Subpath {
                        kind: "Open Subpath",
                        knots: Vec::new(),
                    },
                );
                if !finished.knots.is_empty() {
                    paths.push(finished);
                }
            }
            selector @ (4 | 5) => {
                open.knots.push(knot(
                    if selector == 4 { "Linked" } else { "Unlinked" },
                    spacer,
                )?);
            }
            8 => {
                fill = Some(if reader.i16(2 + spacer).ok()? == 1 {
                    "with all pixels"
                } else {
                    "without all pixels"
                });
            }
            _ => {}
        }
    }
    if !closed.knots.is_empty() {
        paths.push(closed);
    }
    if !open.knots.is_empty() {
        paths.push(open);
    }
    let name_length = i64::from(reader.i8(length - 1).ok()?);
    let name = java_string(
        reader.bytes(length - name_length - 1, name_length).ok()?,
        Some(Charset::named("US-ASCII")),
    );
    let mut text = format!("\"{name}\" having ");
    if let Some(fill) = fill {
        text.push_str(&format!("initial fill rule \"{fill}\" and "));
    }
    let count = paths.len();
    text.push_str(&format!(
        "{count}{}",
        if count == 1 {
            " subpath:"
        } else {
            " subpaths:"
        }
    ));
    for path in &paths {
        text.push_str(&format!(
            "\n- {} with {count}{}",
            path.kind,
            if count == 1 { " knot:" } else { " knots:" }
        ));
        for knot in &path.knots {
            text.push_str(&format!("\n  - {}", knot.kind));
            for pair in knot.points.chunks(2) {
                text.push_str(&format!(
                    " ({},{})",
                    java_double_to_string(pair[0]),
                    java_double_to_string(pair[1])
                ));
            }
        }
    }
    Some(text)
}

/// `RandomAccessReader.getInt24`: three bytes, unsigned.
fn reader_int24(reader: &Reader, index: i64) -> Option<i32> {
    let bytes = reader.bytes(index, 3).ok()?;
    let value = (i32::from(bytes[0]) << 16) | (i32::from(bytes[1]) << 8) | i32::from(bytes[2]);
    Some(if reader.motorola {
        value
    } else {
        (i32::from(bytes[2]) << 16) | (i32::from(bytes[1]) << 8) | i32::from(bytes[0])
    })
}
