//! metadata-extractor's `IccReader` and `IccDescriptor`: the header of an ICC profile and its
//! tag table, each tag's data kept as bytes and described by its type.

use super::descriptions::{base, decimal, named, Described};
use super::reader::Reader;
use super::{java_string, Directory, Kind, Metadata, Unported, Value};
use crate::formats::data_format::printf;
use crate::text::charset::Charset;

const TAG_PROFILE_BYTE_COUNT: i32 = 0x0000;
const TAG_CMM_TYPE: i32 = 0x0004;
const TAG_PROFILE_VERSION: i32 = 0x0008;
const TAG_PROFILE_CLASS: i32 = 0x000c;
const TAG_COLOR_SPACE: i32 = 0x0010;
const TAG_PROFILE_CONNECTION_SPACE: i32 = 0x0014;
const TAG_PROFILE_DATETIME: i32 = 0x0018;
const TAG_SIGNATURE: i32 = 0x0024;
const TAG_PLATFORM: i32 = 0x0028;
const TAG_CMM_FLAGS: i32 = 0x002c;
const TAG_DEVICE_MAKE: i32 = 0x0030;
const TAG_DEVICE_MODEL: i32 = 0x0034;
const TAG_DEVICE_ATTR: i32 = 0x0038;
const TAG_RENDERING_INTENT: i32 = 0x0040;
const TAG_XYZ_VALUES: i32 = 0x0044;
const TAG_TAG_COUNT: i32 = 0x0080;

const TYPE_TEXT: i32 = 0x7465_7874;
const TYPE_DESC: i32 = 0x6465_7363;
const TYPE_SIG: i32 = 0x7369_6720;
const TYPE_MEAS: i32 = 0x6D65_6173;
const TYPE_XYZ_ARRAY: i32 = 0x5859_5A20;
const TYPE_MLUC: i32 = 0x6d6c_7563;
const TYPE_CURV: i32 = 0x6375_7276;

/// The preamble of the JPEG APP2 segments an ICC profile is split across.
const JPEG_PREAMBLE: &str = "ICC_PROFILE";

/// `IccReader.readJpegSegments`: the profile spread over APP2 segments, put back together.
pub(crate) fn read_jpeg_segments(
    segments: &[&[u8]],
    metadata: &mut Metadata,
) -> Result<(), Unported> {
    let mut buffer: Option<Vec<u8>> = None;
    for segment in segments {
        if segment.len() < JPEG_PREAMBLE.len()
            || !crate::java::equals_ignore_case(
                &java_string(&segment[..JPEG_PREAMBLE.len()], None),
                JPEG_PREAMBLE,
            )
        {
            continue;
        }
        let Some(data) = segment.get(14..) else {
            return Err(Unported::new(
                "an ICC_PROFILE segment shorter than its header, which metadata-extractor throws on",
            ));
        };
        buffer.get_or_insert_with(Vec::new).extend_from_slice(data);
    }
    if let Some(buffer) = buffer {
        extract(&buffer, metadata);
    }
    Ok(())
}

/// `getStringFromInt32`: the four bytes of a big-endian int read as text.
fn int32_text(value: i32) -> String {
    java_string(&value.to_be_bytes(), None)
}

/// `IccReader.extract`.
pub(crate) fn extract(bytes: &[u8], metadata: &mut Metadata) {
    let mut directory = Directory::new(Kind::Icc);
    if let Err(bounds) = read(&Reader::new(bytes), &mut directory) {
        directory.add_error(format!("Exception reading ICC profile: {}", bounds.0));
    }
    metadata.add(directory);
}

fn read(reader: &Reader, directory: &mut Directory) -> Result<(), super::reader::Bounds> {
    let at = |tag: i32| i64::from(tag);
    directory.set(
        TAG_PROFILE_BYTE_COUNT,
        Value::Int(reader.i32(at(TAG_PROFILE_BYTE_COUNT))?),
    );
    let text = |directory: &mut Directory, tag: i32| -> Result<(), super::reader::Bounds> {
        let value = reader.i32(at(tag))?;
        if value != 0 {
            directory.set(tag, Value::Text(int32_text(value)));
        }
        Ok(())
    };
    let int = |directory: &mut Directory, tag: i32| -> Result<(), super::reader::Bounds> {
        let value = reader.i32(at(tag))?;
        if value != 0 {
            directory.set(tag, Value::Int(value));
        }
        Ok(())
    };
    text(directory, TAG_CMM_TYPE)?;
    int(directory, TAG_PROFILE_VERSION)?;
    text(directory, TAG_PROFILE_CLASS)?;
    text(directory, TAG_COLOR_SPACE)?;
    text(directory, TAG_PROFILE_CONNECTION_SPACE)?;
    read_date(reader, directory)?;
    text(directory, TAG_SIGNATURE)?;
    text(directory, TAG_PLATFORM)?;
    int(directory, TAG_CMM_FLAGS)?;
    text(directory, TAG_DEVICE_MAKE)?;
    let model = reader.i32(at(TAG_DEVICE_MODEL))?;
    if model != 0 {
        if model <= 0x2020_2020 {
            directory.set(TAG_DEVICE_MODEL, Value::Int(model));
        } else {
            directory.set(TAG_DEVICE_MODEL, Value::Text(int32_text(model)));
        }
    }
    int(directory, TAG_RENDERING_INTENT)?;
    let attributes = reader.i64(at(TAG_DEVICE_ATTR))?;
    if attributes != 0 {
        directory.set(TAG_DEVICE_ATTR, Value::Long(attributes));
    }
    let xyz = vec![
        reader.s15_fixed16(at(TAG_XYZ_VALUES))?,
        reader.s15_fixed16(at(TAG_XYZ_VALUES) + 4)?,
        reader.s15_fixed16(at(TAG_XYZ_VALUES) + 8)?,
    ];
    directory.set(TAG_XYZ_VALUES, Value::Floats(xyz));
    let count = reader.i32(at(TAG_TAG_COUNT))?;
    directory.set(TAG_TAG_COUNT, Value::Int(count));
    for index in 0..count {
        let position = at(TAG_TAG_COUNT) + 4 + i64::from(index) * 12;
        let tag = reader.i32(position)?;
        let pointer = reader.i32(position + 4)?;
        let length = reader.i32(position + 8)?;
        let data = reader.bytes(i64::from(pointer), i64::from(length))?;
        directory.set(tag, Value::Bytes(data.to_vec()));
    }
    Ok(())
}

/// `IccReader.setDate`: the profile's date as `yyyy:MM:dd HH:mm:ss` text, when it is valid.
fn read_date(reader: &Reader, directory: &mut Directory) -> Result<(), super::reader::Bounds> {
    let at = i64::from(TAG_PROFILE_DATETIME);
    let field = |index: i64| reader.u16(at + index * 2).map(i32::from);
    let (year, month, day) = (field(0)?, field(1)?, field(2)?);
    let (hour, minute, second) = (field(3)?, field(4)?, field(5)?);
    if valid_date(year, month - 1, day)
        && (0..24).contains(&hour)
        && (0..60).contains(&minute)
        && (0..60).contains(&second)
    {
        directory.set(
            TAG_PROFILE_DATETIME,
            Value::Text(format!(
                "{year:04}:{month:02}:{day:02} {hour:02}:{minute:02}:{second:02}"
            )),
        );
    } else {
        directory.add_error(format!("ICC data describes an invalid date/time: year={year} month={month} day={day} hour={hour} minute={minute} second={second}"));
    }
    Ok(())
}

/// `DateUtil.isValidDate`, the month counted from 0.
fn valid_date(year: i32, month: i32, day: i32) -> bool {
    const DAYS: [i32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=9999).contains(&year) || !(0..=11).contains(&month) {
        return false;
    }
    let mut days = DAYS[month as usize];
    if month == 1 && year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
        days += 1;
    }
    day >= 1 && day <= days
}

/// The data type of an ICC tag whose data is held, as Tika's handler reads it to skip curves
/// and lookup tables: 0 for a header field or data shorter than a type.
pub(crate) fn tag_data_type(directory: &Directory, tag: i32) -> i32 {
    if tag <= 0x2020_2020 || tag >= 0x7a7a_7a7a {
        return 0;
    }
    match directory.bytes(tag) {
        Some(data) if data.len() >= 4 => i32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        _ => 0,
    }
}

/// `IccDescriptor.getDescription`.
pub(crate) fn describe(directory: &Directory, tag: i32) -> Described {
    Ok(match tag {
        TAG_PROFILE_VERSION => directory.integer(tag).map(|value| {
            let major = (value & 0xFF00_0000u32 as i32) >> 24;
            let minor = (value & 0x00F0_0000) >> 20;
            let fix = (value & 0x000F_0000) >> 16;
            format!("{major}.{minor}.{fix}")
        }),
        TAG_PROFILE_CLASS => directory.string(tag).map(|text| match signature(&text) {
            Some(0x7363_6E72) => "Input Device".to_owned(),
            Some(0x6D6E_7472) => "Display Device".to_owned(),
            Some(0x7072_7472) => "Output Device".to_owned(),
            Some(0x6C69_6E6B) => "DeviceLink".to_owned(),
            Some(0x7370_6163) => "ColorSpace Conversion".to_owned(),
            Some(0x6162_7374) => "Abstract".to_owned(),
            Some(0x6E6D_636C) => "Named Color".to_owned(),
            Some(_) => format!("Unknown ({text})"),
            None => text,
        }),
        TAG_PLATFORM => directory.string(tag).map(|text| match signature(&text) {
            Some(0x4150_504C) => "Apple Computer, Inc.".to_owned(),
            Some(0x4D53_4654) => "Microsoft Corporation".to_owned(),
            Some(0x5347_4920) => "Silicon Graphics, Inc.".to_owned(),
            Some(0x5355_4E57) => "Sun Microsystems, Inc.".to_owned(),
            Some(0x5447_4E54) => "Taligent, Inc.".to_owned(),
            Some(_) => format!("Unknown ({text})"),
            None => text,
        }),
        TAG_RENDERING_INTENT => named(
            directory,
            tag,
            0,
            &[
                "Perceptual",
                "Media-Relative Colorimetric",
                "Saturation",
                "ICC-Absolute Colorimetric",
            ],
        ),
        _ if tag > 0x2020_2020 && tag < 0x7a7a_7a7a => return tag_data(directory, tag),
        _ => base(directory, tag),
    })
}

/// `getInt32FromString`: the first four bytes of the text's UTF-8, read as a big-endian int.
fn signature(text: &str) -> Option<i32> {
    let bytes = text.as_bytes();
    bytes
        .get(..4)
        .map(|four| i32::from_be_bytes([four[0], four[1], four[2], four[3]]))
}

fn thrown(what: &str) -> Described {
    Err(Unported::new(format!(
        "an ICC {what} tag metadata-extractor throws on describing"
    )))
}

/// `getTagDataString`.
fn tag_data(directory: &Directory, tag: i32) -> Described {
    let Some(bytes) = directory.bytes(tag) else {
        return Ok(directory.string(tag));
    };
    let reader = Reader::new(&bytes);
    let Ok(kind) = reader.i32(0) else {
        return Ok(None);
    };
    match kind {
        TYPE_TEXT => {
            let Some(text) = bytes
                .len()
                .checked_sub(9)
                .map(|length| &bytes[8..8 + length])
            else {
                return thrown("text");
            };
            Ok(Some(java_string(text, Some(Charset::named("US-ASCII")))))
        }
        TYPE_DESC => {
            let Ok(length) = reader.i32(8) else {
                return Ok(None);
            };
            if length < 0 || i64::from(length) > bytes.len() as i64 - 12 {
                return Ok(None);
            }
            if length == 0 {
                return thrown("description");
            }
            Ok(Some(java_string(
                &bytes[12..12 + length as usize - 1],
                None,
            )))
        }
        TYPE_SIG => Ok(reader.i32(8).ok().map(int32_text)),
        TYPE_MEAS => Ok(measurement(&reader)),
        TYPE_XYZ_ARRAY => {
            let count = (bytes.len() as i64 - 8) / 12;
            let mut text = String::new();
            for index in 0..count {
                let at = 8 + index * 12;
                let (Ok(x), Ok(y), Ok(z)) = (
                    reader.s15_fixed16(at),
                    reader.s15_fixed16(at + 4),
                    reader.s15_fixed16(at + 8),
                ) else {
                    return Ok(None);
                };
                if index > 0 {
                    text.push_str(", ");
                }
                text.push_str(&format!(
                    "({}, {}, {})",
                    decimal("0.####", f64::from(x)),
                    decimal("0.####", f64::from(y)),
                    decimal("0.####", f64::from(z))
                ));
            }
            Ok(Some(text))
        }
        TYPE_MLUC => {
            let Ok(count) = reader.i32(8) else {
                return Ok(None);
            };
            let mut text = count.to_string();
            for index in 0..i64::from(count) {
                let at = 16 + index * 12;
                let (Ok(language), Ok(length), Ok(offset)) =
                    (reader.i32(at), reader.i32(at + 4), reader.i32(at + 8))
                else {
                    return Ok(None);
                };
                let (Ok(offset), Ok(length)) = (usize::try_from(offset), usize::try_from(length))
                else {
                    return thrown("localized text");
                };
                let Some(name) = bytes.get(offset..offset.saturating_add(length)) else {
                    return thrown("localized text");
                };
                text.push_str(&format!(
                    " {}({})",
                    int32_text(language),
                    java_string(name, Some(Charset::named("UTF-16BE")))
                ));
            }
            Ok(Some(text))
        }
        TYPE_CURV => {
            let Ok(count) = reader.i32(8) else {
                return Ok(None);
            };
            let mut values = Vec::new();
            for index in 0..i64::from(count) {
                let Ok(value) = reader.u16(12 + index * 2) else {
                    return Ok(None);
                };
                values.push(trimmed_fixed(f64::from(f32::from(value)) / 65535.0, 7));
            }
            Ok(Some(values.join(", ")))
        }
        other => Ok(Some(format!(
            "{} (0x{:08X}): {} bytes",
            int32_text(other),
            other,
            bytes.len()
        ))),
    }
}

/// `formatDoubleAsString(value, precision, false)`: `%.Nf` without its trailing zeros but one.
fn trimmed_fixed(value: f64, precision: usize) -> String {
    let formatted = printf::fixed(value, 0, precision, false);
    let dot = formatted.find('.').map_or(0, |dot| dot as i64);
    let mut end = formatted.len() as i64;
    while end > dot + 2 && formatted.as_bytes()[end as usize - 1] == b'0' {
        end -= 1;
    }
    formatted[..end as usize].to_owned()
}

fn measurement(reader: &Reader) -> Option<String> {
    let observer = reader.i32(8).ok()?;
    let x = reader.s15_fixed16(12).ok()?;
    let y = reader.s15_fixed16(16).ok()?;
    let z = reader.s15_fixed16(20).ok()?;
    let geometry = reader.i32(24).ok()?;
    let flare = reader.s15_fixed16(28).ok()?;
    let illuminant = reader.i32(32).ok()?;
    let observer_text = match observer {
        0 => "Unknown".to_owned(),
        1 => "1931 2°".to_owned(),
        2 => "1964 10°".to_owned(),
        other => format!("Unknown {other}"),
    };
    let geometry_text = match geometry {
        0 => "Unknown".to_owned(),
        1 => "0/45 or 45/0".to_owned(),
        2 => "0/d or d/0".to_owned(),
        _ => format!("Unknown {observer}"),
    };
    let illuminant_text = match illuminant {
        0 => "unknown".to_owned(),
        1 => "D50".to_owned(),
        2 => "D65".to_owned(),
        3 => "D93".to_owned(),
        4 => "F2".to_owned(),
        5 => "D55".to_owned(),
        6 => "A".to_owned(),
        7 => "Equi-Power (E)".to_owned(),
        8 => "F8".to_owned(),
        other => format!("Unknown {other}"),
    };
    let flare_percent = (f64::from(flare * 100.0) + 0.5).floor() as i32;
    Some(format!(
        "{observer_text} Observer, Backing ({}, {}, {}), Geometry {geometry_text}, Flare {flare_percent}%, Illuminant {illuminant_text}",
        decimal("0.###", f64::from(x)),
        decimal("0.###", f64::from(y)),
        decimal("0.###", f64::from(z)),
    ))
}
