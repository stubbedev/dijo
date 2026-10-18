//! metadata-extractor's `TagDescriptor`s: the text a tag's value is described with, by the
//! descriptor of its directory, `TagDescriptor.getDescription`'s default where the descriptor
//! has nothing of its own for the tag. The Exif and GPS descriptors are here; those of the other
//! directories are beside their readers.

use crate::java::decimal::Rounding;
use crate::java::decimal_format::DecimalFormat;
use crate::java::trim as java_trim;
use crate::text::charset::Charset;

use super::exif::tags::*;
use super::{
    apple, dates, geo, heif, icc, iptc, jpeg, photoshop, webp, Directory, Kind, Unported, Value,
};

/// A description, or the exception metadata-extractor throws describing the tag, which the port
/// does not reproduce: the document is left to Tika's path.
pub(crate) type Described = Result<Option<String>, Unported>;

/// `getDescription`, dispatched to the directory's descriptor.
pub(super) fn describe(directory: &Directory, tag: i32) -> Described {
    match directory.kind {
        Kind::ExifIfd0 | Kind::ExifSubIfd | Kind::ExifInterop | Kind::ExifImage => {
            exif(directory, tag)
        }
        Kind::ExifThumbnail { exif_start } => thumbnail(directory, tag, exif_start),
        Kind::Gps => gps(directory, tag),
        Kind::Iptc => iptc::describe(directory, tag),
        Kind::Icc => icc::describe(directory, tag),
        Kind::Photoshop => photoshop::describe(directory, tag),
        Kind::Jpeg
        | Kind::JpegComment
        | Kind::Jfif
        | Kind::Jfxx
        | Kind::Adobe
        | Kind::Ducky
        | Kind::Huffman => jpeg::describe(directory, tag),
        Kind::Webp => webp::describe(directory, tag),
        Kind::FileSystem => Ok(file_system(directory, tag)),
        Kind::AppleMakernote => apple::describe(directory, tag),
        Kind::AppleRunTime => apple::describe_run_time(directory, tag),
        Kind::Heif => heif::describe(directory, tag),
        Kind::PrintIm | Kind::Xmp | Kind::Error => Ok(base(directory, tag)),
    }
}

/// `TagDescriptor.getDescription`: arrays of more than 16 values by their count, dates as
/// `TagDescriptor` writes them, anything else as `getString`.
pub(super) fn base(directory: &Directory, tag: i32) -> Option<String> {
    let value = directory.object(tag)?;
    if let Some(length) = value.array_length() {
        if length > 16 {
            return Some(format!("[{length} values]"));
        }
    }
    if let Value::Date(millis, zone) = value {
        return Some(dates::describe_date(*millis, zone));
    }
    directory.string(tag)
}

/// `DecimalFormat` with a pattern, rounding half even as Java's does unless told otherwise.
pub(super) fn decimal(pattern: &str, value: f64) -> String {
    rounded(pattern, Rounding::HalfEven, value)
}

pub(super) fn rounded(pattern: &str, rounding: Rounding, value: f64) -> String {
    DecimalFormat::new(pattern, rounding)
        .unwrap_or_else(|e| panic!("the pattern {pattern} compiles: {e}"))
        .format_f64(value)
}

/// `getIndexedDescription`: the description at the value less `base_index`, or `Unknown (n)`.
pub(super) fn indexed(
    directory: &Directory,
    tag: i32,
    base_index: i64,
    descriptions: &[Option<&str>],
) -> Option<String> {
    let index = directory.long(tag)?;
    let at = index - base_index;
    if at >= 0 && (at as u64) < descriptions.len() as u64 {
        if let Some(description) = descriptions[at as usize] {
            return Some(description.to_owned());
        }
    }
    Some(format!("Unknown ({index})"))
}

/// [`indexed`] where every value has a description.
pub(super) fn named(
    directory: &Directory,
    tag: i32,
    base_index: i64,
    descriptions: &[&str],
) -> Option<String> {
    let descriptions: Vec<Option<&str>> = descriptions.iter().copied().map(Some).collect();
    indexed(directory, tag, base_index, &descriptions)
}

/// `convertBytesToVersionString`.
pub(super) fn version_string(components: &[i32], major_digits: usize) -> String {
    let mut version = String::new();
    for (index, component) in components.iter().take(4).enumerate() {
        if index == major_digits {
            version.push('.');
        }
        let mut unit = *component as u16;
        if unit < u16::from(b'0') {
            unit = unit.wrapping_add(u16::from(b'0'));
        }
        if index == 0 && unit == u16::from(b'0') {
            continue;
        }
        version.extend(char::decode_utf16([unit]).map(|c| c.unwrap_or('\u{fffd}')));
    }
    version
}

pub(super) fn version_bytes(
    directory: &Directory,
    tag: i32,
    major_digits: usize,
) -> Option<String> {
    Some(version_string(&directory.ints(tag)?, major_digits))
}

/// `getFStopDescription`.
fn f_stop(f_stop: f64) -> String {
    format!("f/{}", rounded("0.0", Rounding::HalfUp, f_stop))
}

/// `getFocalLengthDescription`.
fn focal_length(millimetres: f64) -> String {
    format!("{} mm", rounded("0.#", Rounding::HalfUp, millimetres))
}

fn lens_specification(directory: &Directory, tag: i32) -> Option<String> {
    let values = directory.rationals(tag)?;
    if values.len() != 4 || (values[0].is_zero() && values[2].is_zero()) {
        return None;
    }
    let mut text = if values[0].same_value(values[1]) {
        format!("{}mm", values[0].to_simple_string(true))
    } else {
        format!(
            "{}-{}mm",
            values[0].to_simple_string(true),
            values[1].to_simple_string(true)
        )
    };
    if !values[2].is_zero() {
        text.push(' ');
        if values[2].same_value(values[3]) {
            text.push_str(&f_stop(values[2].double_value()));
        } else {
            text.push_str(&format!(
                "f/{}-{}",
                rounded("0.0", Rounding::HalfUp, values[2].double_value()),
                rounded("0.0", Rounding::HalfUp, values[3].double_value())
            ));
        }
    }
    Some(text)
}

pub(super) fn orientation(directory: &Directory, tag: i32) -> Option<String> {
    named(
        directory,
        tag,
        1,
        &[
            "Top, left side (Horizontal / normal)",
            "Top, right side (Mirror horizontal)",
            "Bottom, right side (Rotate 180)",
            "Bottom, left side (Mirror vertical)",
            "Left side, top (Mirror horizontal and rotate 270 CW)",
            "Right side, top (Rotate 90 CW)",
            "Right side, bottom (Mirror horizontal and rotate 90 CW)",
            "Left side, bottom (Rotate 270 CW)",
        ],
    )
}

fn shutter_speed(directory: &Directory, tag: i32) -> Option<String> {
    let apex = directory.float(tag)?;
    if apex <= 1.0 {
        let power = (1.0 / (f64::from(apex) * 2f64.ln()).exp()) as f32;
        let power10 = crate::java::round(f64::from(power) * 10.0);
        let rounded_power = power10 as f32 / 10.0;
        return Some(format!(
            "{} sec",
            rounded("0.##", Rounding::HalfUp, f64::from(rounded_power))
        ));
    }
    let power = crate::java::round((f64::from(apex) * 2f64.ln()).exp()) as i32;
    Some(format!("1/{power} sec"))
}

/// `getEncodedTextDescription`: an Exif comment, its character code named in its first bytes.
pub(super) fn encoded_text(directory: &Directory, tag: i32) -> Option<String> {
    let bytes = directory.bytes(tag)?;
    if bytes.is_empty() {
        return Some(String::new());
    }
    let utf8 = Charset::named("UTF-8");
    let decode = |bytes: &[u8], charset: Charset| -> String {
        java_trim(&super::java_string(bytes, Some(charset))).to_owned()
    };
    if bytes.len() >= 10 {
        let first_ten = super::java_string(&bytes[..10], Some(utf8));
        for (name, charset) in [
            ("ASCII", utf8),
            ("UNICODE", Charset::named("UTF-16LE")),
            ("JIS", Charset::named("Shift_JIS")),
        ] {
            if first_ten.starts_with(name) {
                for start in name.len()..10 {
                    let byte = bytes[start];
                    if byte != 0 && byte != b' ' {
                        return Some(decode(&bytes[start..], charset));
                    }
                }
                return Some(decode(&bytes[10..], charset));
            }
        }
    }
    Some(decode(&bytes, utf8))
}

/// `ExifThumbnailDescriptor`.
fn thumbnail(directory: &Directory, tag: i32, exif_start: i32) -> Described {
    match tag {
        TAG_THUMBNAIL_OFFSET => Ok(directory
            .integer(TAG_THUMBNAIL_OFFSET)
            .map(|offset| format!("{} bytes", offset.wrapping_add(exif_start)))),
        TAG_THUMBNAIL_LENGTH => Ok(suffixed(directory, tag, " bytes")),
        _ => exif(directory, tag),
    }
}

fn suffixed(directory: &Directory, tag: i32, suffix: &str) -> Option<String> {
    Some(format!("{}{suffix}", directory.string(tag)?))
}

fn resolution_unit(directory: &Directory) -> Option<String> {
    named(
        directory,
        TAG_RESOLUTION_UNIT,
        1,
        &["(No unit)", "Inch", "cm"],
    )
}

fn resolution(directory: &Directory, tag: i32) -> Option<String> {
    let value = directory.rational(tag)?;
    let unit = resolution_unit(directory);
    Some(format!(
        "{} dots per {}",
        value.to_simple_string(true),
        unit.map_or_else(|| "unit".to_owned(), |unit| unit.to_lowercase())
    ))
}

fn focal_plane_unit(directory: &Directory) -> Option<String> {
    named(
        directory,
        TAG_FOCAL_PLANE_RESOLUTION_UNIT,
        1,
        &["(No unit)", "Inches", "cm"],
    )
}

fn focal_plane_resolution(directory: &Directory, tag: i32) -> Option<String> {
    let value = directory.rational(tag)?;
    let unit = focal_plane_unit(directory);
    Some(format!(
        "{}{}",
        value.reciprocal().to_simple_string(true),
        unit.map_or_else(String::new, |unit| format!(" {}", unit.to_lowercase()))
    ))
}

/// A measure that is unknown when its denominator is 0xFFFFFFFF.
fn measure(directory: &Directory, tag: i32, pattern: &str, unit: &str) -> Option<String> {
    let value = directory.rational(tag)?;
    if value.denominator == 0xFFFF_FFFF {
        return Some("Unknown".to_owned());
    }
    Some(format!("{}{unit}", decimal(pattern, value.double_value())))
}

fn unicode(directory: &Directory, tag: i32) -> Option<String> {
    let bytes = directory.bytes(tag)?;
    Some(
        java_trim(&super::java_string(
            &bytes,
            Some(Charset::named("UTF-16LE")),
        ))
        .to_owned(),
    )
}

fn cfa_colors(pattern: &[i32]) -> Described {
    if pattern.len() < 2 {
        return Ok(Some("<truncated data>".to_owned()));
    }
    if pattern[0] == 0 && pattern[1] == 0 {
        return Ok(Some("<zero pattern size>".to_owned()));
    }
    let end = pattern[0].wrapping_mul(pattern[1]).wrapping_add(2);
    if i64::from(end) > pattern.len() as i64 {
        return Ok(Some("<invalid pattern size>".to_owned()));
    }
    const COLORS: [&str; 7] = ["Red", "Green", "Blue", "Cyan", "Magenta", "Yellow", "White"];
    let thrown = || {
        Err(Unported::new(
            "a CFA pattern metadata-extractor throws on: a negative colour or no columns",
        ))
    };
    let mut text = String::from("[");
    for position in 2..end.max(2) {
        let color = pattern[position as usize];
        match usize::try_from(color) {
            Ok(index) if index < COLORS.len() => text.push_str(COLORS[index]),
            Ok(_) => text.push_str("Unknown"),
            Err(_) => return thrown(),
        }
        if pattern[1] == 0 {
            return thrown();
        }
        if (position - 2) % pattern[1] == 0 {
            text.push(',');
        } else if position != end - 1 {
            text.push_str("][");
        }
    }
    text.push(']');
    Ok(Some(text))
}

fn cfa_pattern(directory: &Directory) -> Described {
    let Some(values) = directory.bytes(TAG_CFA_PATTERN) else {
        return Ok(None);
    };
    if values.len() < 4 {
        let pattern: Vec<i32> = values.iter().map(|byte| i32::from(*byte as i8)).collect();
        return cfa_colors(&pattern);
    }
    let mut pattern = vec![0i32; values.len() - 2];
    let mut reader = super::reader::Reader::new(&values);
    let item = |reader: &super::reader::Reader, at| reader.i16(at).expect("four bytes are there");
    let mut item0 = item(&reader, 0);
    let mut item1 = item(&reader, 2);
    let mut copy = true;
    if 2 + i32::from(item0) * i32::from(item1) > values.len() as i32 {
        reader.motorola = !reader.motorola;
        item0 = item(&reader, 0);
        item1 = item(&reader, 2);
        copy = values.len() as i32 >= 2 + i32::from(item0) * i32::from(item1);
    }
    if copy {
        pattern[0] = i32::from(item0);
        pattern[1] = i32::from(item1);
        for index in 4..values.len() {
            pattern[index - 2] = i32::from(values[index] as i8);
        }
    }
    cfa_colors(&pattern)
}

fn cfa_pattern_2(directory: &Directory) -> Described {
    let Some(values) = directory.bytes(TAG_CFA_PATTERN_2) else {
        return Ok(None);
    };
    let Some(repeat) = directory.ints(TAG_CFA_REPEAT_PATTERN_DIM) else {
        return Ok(Some(format!(
            "Repeat Pattern not found for CFAPattern ({})",
            base(directory, TAG_CFA_PATTERN_2).unwrap_or_else(|| "null".to_owned())
        )));
    };
    if repeat.len() == 2 && values.len() as i32 == repeat[0].wrapping_mul(repeat[1]) {
        let mut pattern = vec![repeat[0], repeat[1]];
        pattern.extend(values.iter().map(|byte| i32::from(*byte)));
        return cfa_colors(&pattern);
    }
    Ok(Some(format!(
        "Unknown Pattern ({})",
        base(directory, TAG_CFA_PATTERN_2).unwrap_or_else(|| "null".to_owned())
    )))
}

fn reference_black_white(directory: &Directory) -> Option<String> {
    let mut ints = directory.ints(TAG_REFERENCE_BLACK_WHITE);
    if ints.as_ref().is_none_or(|ints| ints.len() < 6) {
        match directory.object(TAG_REFERENCE_BLACK_WHITE) {
            Some(Value::Longs(longs)) if longs.len() >= 6 => {
                ints = Some(longs.iter().map(|long| *long as i32).collect());
            }
            _ => return None,
        }
    }
    let ints = ints?;
    Some(format!(
        "[{},{},{}] [{},{},{}]",
        ints[0], ints[2], ints[4], ints[1], ints[3], ints[5]
    ))
}

/// `ExifDescriptorBase.getWhiteBalanceDescription(int)`.
fn white_balance(value: i32) -> String {
    match value {
        0 => "Unknown",
        1 => "Daylight",
        2 => "Florescent",
        3 => "Tungsten (Incandescent)",
        4 => "Flash",
        9 => "Fine Weather",
        10 => "Cloudy",
        11 => "Shade",
        12 => "Daylight Fluorescent",
        13 => "Day White Fluorescent",
        14 => "Cool White Fluorescent",
        15 => "White Fluorescent",
        16 => "Warm White Fluorescent",
        17 => "Standard light A",
        18 => "Standard light B",
        19 => "Standard light C",
        20 => "D55",
        21 => "D65",
        22 => "D75",
        23 => "D50",
        24 => "ISO Studio Tungsten",
        255 => "Other",
        other => return format!("Unknown ({other})"),
    }
    .to_owned()
}

fn compression(value: i32) -> String {
    match value {
        1 => "Uncompressed",
        2 => "CCITT 1D",
        3 => "T4/Group 3 Fax",
        4 => "T6/Group 4 Fax",
        5 => "LZW",
        6 => "JPEG (old-style)",
        7 => "JPEG",
        8 => "Adobe Deflate",
        9 => "JBIG B&W",
        10 => "JBIG Color",
        99 => "JPEG",
        262 => "Kodak 262",
        32766 => "Next",
        32767 => "Sony ARW Compressed",
        32769 => "Packed RAW",
        32770 => "Samsung SRW Compressed",
        32771 => "CCIRLEW",
        32772 => "Samsung SRW Compressed 2",
        32773 => "PackBits",
        32809 => "Thunderscan",
        32867 => "Kodak KDC Compressed",
        32895 => "IT8CTPAD",
        32896 => "IT8LW",
        32897 => "IT8MP",
        32898 => "IT8BL",
        32908 => "PixarFilm",
        32909 => "PixarLog",
        32946 => "Deflate",
        32947 => "DCS",
        34661 => "JBIG",
        34676 => "SGILog",
        34677 => "SGILog24",
        34712 => "JPEG 2000",
        34713 => "Nikon NEF Compressed",
        34715 => "JBIG2 TIFF FX",
        34718 => "Microsoft Document Imaging (MDI) Binary Level Codec",
        34719 => "Microsoft Document Imaging (MDI) Progressive Transform Codec",
        34720 => "Microsoft Document Imaging (MDI) Vector",
        34892 => "Lossy JPEG",
        65000 => "Kodak DCR Compressed",
        65535 => "Pentax PEF Compressed",
        other => return format!("Unknown ({other})"),
    }
    .to_owned()
}

fn photometric_interpretation(value: i32) -> &'static str {
    match value {
        0 => "WhiteIsZero",
        1 => "BlackIsZero",
        2 => "RGB",
        3 => "RGB Palette",
        4 => "Transparency Mask",
        5 => "CMYK",
        6 => "YCbCr",
        8 => "CIELab",
        9 => "ICCLab",
        10 => "ITULab",
        32803 => "Color Filter Array",
        32844 => "Pixar LogL",
        32845 => "Pixar LogLuv",
        32892 => "Linear Raw",
        _ => "Unknown colour space",
    }
}

fn metering_mode(value: i32) -> String {
    match value {
        0 => "Unknown",
        1 => "Average",
        2 => "Center weighted average",
        3 => "Spot",
        4 => "Multi-spot",
        5 => "Multi-segment",
        6 => "Partial",
        255 => "(Other)",
        other => return format!("Unknown ({other})"),
    }
    .to_owned()
}

fn flash(value: i32) -> String {
    let mut text = String::from(if value & 0x1 != 0 {
        "Flash fired"
    } else {
        "Flash did not fire"
    });
    if value & 0x4 != 0 {
        text.push_str(if value & 0x2 != 0 {
            ", return detected"
        } else {
            ", return not detected"
        });
    }
    if value & 0x10 != 0 && value & 0x0F != 0 {
        text.push_str(", auto");
    }
    if value & 0x40 != 0 {
        text.push_str(", red-eye reduction");
    }
    text
}

fn sample_format(values: &[i32]) -> String {
    values
        .iter()
        .map(|value| match value {
            1 => "Unsigned".to_owned(),
            2 => "Signed".to_owned(),
            3 => "Float".to_owned(),
            4 => "Undefined".to_owned(),
            5 => "Complex int".to_owned(),
            6 => "Complex float".to_owned(),
            other => format!("Unknown ({other})"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `ExifDescriptorBase.getDescription`.
fn exif(directory: &Directory, tag: i32) -> Described {
    match tag {
        TAG_CFA_PATTERN => cfa_pattern(directory),
        TAG_CFA_PATTERN_2 => cfa_pattern_2(directory),
        _ => Ok(exif_value(directory, tag)),
    }
}

fn exif_value(directory: &Directory, tag: i32) -> Option<String> {
    let d = directory;
    match tag {
        TAG_INTEROP_INDEX => {
            let value = d.string(tag)?;
            Some(
                if crate::java::equals_ignore_case(java_trim(&value), "R98") {
                    "Recommended Exif Interoperability Rules (ExifR98)".to_owned()
                } else {
                    format!("Unknown ({value})")
                },
            )
        }
        TAG_INTEROP_VERSION | TAG_EXIF_VERSION | TAG_FLASHPIX_VERSION => version_bytes(d, tag, 2),
        TAG_NEW_SUBFILE_TYPE => named(
            d,
            tag,
            0,
            &[
                "Full-resolution image",
                "Reduced-resolution image",
                "Single page of multi-page image",
                "Single page of multi-page reduced-resolution image",
                "Transparency mask",
                "Transparency mask of reduced-resolution image",
                "Transparency mask of multi-page image",
                "Transparency mask of reduced-resolution multi-page image",
            ],
        ),
        TAG_SUBFILE_TYPE => named(
            d,
            tag,
            1,
            &[
                "Full-resolution image",
                "Reduced-resolution image",
                "Single page of multi-page image",
            ],
        ),
        TAG_IMAGE_WIDTH | TAG_IMAGE_HEIGHT => suffixed(d, tag, " pixels"),
        TAG_BITS_PER_SAMPLE => suffixed(d, tag, " bits/component/pixel"),
        TAG_COMPRESSION => Some(compression(d.integer(tag)?)),
        TAG_PHOTOMETRIC_INTERPRETATION => {
            Some(photometric_interpretation(d.integer(tag)?).to_owned())
        }
        TAG_THRESHOLDING => named(
            d,
            tag,
            1,
            &[
                "No dithering or halftoning",
                "Ordered dither or halftone",
                "Randomized dither",
            ],
        ),
        TAG_FILL_ORDER => named(d, tag, 1, &["Normal", "Reversed"]),
        TAG_ORIENTATION => orientation(d, tag),
        TAG_SAMPLES_PER_PIXEL => suffixed(d, tag, " samples/pixel"),
        TAG_ROWS_PER_STRIP => suffixed(d, tag, " rows/strip"),
        TAG_STRIP_BYTE_COUNTS => suffixed(d, tag, " bytes"),
        TAG_X_RESOLUTION | TAG_Y_RESOLUTION => resolution(d, tag),
        TAG_PLANAR_CONFIGURATION => named(
            d,
            tag,
            1,
            &[
                "Chunky (contiguous for each subsampling pixel)",
                "Separate (Y-plane/Cb-plane/Cr-plane format)",
            ],
        ),
        TAG_RESOLUTION_UNIT => resolution_unit(d),
        TAG_JPEG_PROC => Some(match d.integer(tag)? {
            1 => "Baseline".to_owned(),
            14 => "Lossless".to_owned(),
            other => format!("Unknown ({other})"),
        }),
        TAG_YCBCR_SUBSAMPLING => {
            let positions = d.ints(tag)?;
            if positions.len() < 2 {
                return None;
            }
            Some(
                match (positions[0], positions[1]) {
                    (2, 1) => "YCbCr4:2:2",
                    (2, 2) => "YCbCr4:2:0",
                    _ => "(Unknown)",
                }
                .to_owned(),
            )
        }
        TAG_YCBCR_POSITIONING => named(d, tag, 1, &["Center of pixel array", "Datum point"]),
        TAG_REFERENCE_BLACK_WHITE => reference_black_white(d),
        TAG_EXPOSURE_TIME => suffixed(d, tag, " sec"),
        TAG_FNUMBER => Some(f_stop(d.rational(tag)?.double_value())),
        TAG_EXPOSURE_PROGRAM => named(
            d,
            tag,
            1,
            &[
                "Manual control",
                "Program normal",
                "Aperture priority",
                "Shutter priority",
                "Program creative (slow program)",
                "Program action (high-speed program)",
                "Portrait mode",
                "Landscape mode",
            ],
        ),
        TAG_ISO_EQUIVALENT => d.integer(tag).map(|iso| iso.to_string()),
        TAG_SENSITIVITY_TYPE => named(
            d,
            tag,
            0,
            &[
                "Unknown",
                "Standard Output Sensitivity",
                "Recommended Exposure Index",
                "ISO Speed",
                "Standard Output Sensitivity and Recommended Exposure Index",
                "Standard Output Sensitivity and ISO Speed",
                "Recommended Exposure Index and ISO Speed",
                "Standard Output Sensitivity, Recommended Exposure Index and ISO Speed",
            ],
        ),
        TAG_COMPONENTS_CONFIGURATION => {
            let components = d.ints(tag)?;
            const NAMES: [&str; 7] = ["", "Y", "Cb", "Cr", "R", "G", "B"];
            Some(
                components
                    .iter()
                    .take(4)
                    .filter(|component| **component > 0 && **component < NAMES.len() as i32)
                    .map(|component| NAMES[*component as usize])
                    .collect(),
            )
        }
        TAG_COMPRESSED_AVERAGE_BITS_PER_PIXEL => {
            let value = d.rational(tag)?;
            let ratio = value.to_simple_string(true);
            Some(if value.is_integer() && value.int_value() == 1 {
                format!("{ratio} bit/pixel")
            } else {
                format!("{ratio} bits/pixel")
            })
        }
        TAG_SHUTTER_SPEED => shutter_speed(d, tag),
        TAG_APERTURE | TAG_MAX_APERTURE => {
            Some(f_stop(std::f64::consts::SQRT_2.powf(d.double(tag)?)))
        }
        TAG_BRIGHTNESS_VALUE => {
            let value = d.rational(tag)?;
            if value.numerator == 0xFFFF_FFFF {
                return Some("Unknown".to_owned());
            }
            Some(decimal("0.0##", value.double_value()))
        }
        TAG_EXPOSURE_BIAS => Some(format!("{} EV", d.rational(tag)?.to_simple_string(true))),
        TAG_SUBJECT_DISTANCE => {
            let value = d.rational(tag)?;
            if value.numerator == 0xFFFF_FFFF {
                return Some("Infinity".to_owned());
            }
            if value.numerator == 0 {
                return Some("Unknown".to_owned());
            }
            Some(format!("{} metres", decimal("0.0##", value.double_value())))
        }
        TAG_METERING_MODE => Some(metering_mode(d.integer(tag)?)),
        TAG_WHITE_BALANCE => Some(white_balance(d.integer(tag)?)),
        TAG_FLASH => Some(flash(d.integer(tag)?)),
        TAG_FOCAL_LENGTH => Some(focal_length(d.rational(tag)?.double_value())),
        TAG_USER_COMMENT => encoded_text(d, tag),
        TAG_TEMPERATURE => measure(d, tag, "0.0", " °C"),
        TAG_HUMIDITY => measure(d, tag, "0.0", " %"),
        TAG_PRESSURE => measure(d, tag, "0.0", " hPa"),
        TAG_WATER_DEPTH => measure(d, tag, "0.0##", " metres"),
        TAG_ACCELERATION => measure(d, tag, "0.0##", " mGal"),
        TAG_CAMERA_ELEVATION_ANGLE => measure(d, tag, "0.##", " degrees"),
        TAG_WIN_TITLE | TAG_WIN_COMMENT | TAG_WIN_AUTHOR | TAG_WIN_KEYWORDS | TAG_WIN_SUBJECT => {
            unicode(d, tag)
        }
        TAG_COLOR_SPACE => Some(match d.integer(tag)? {
            1 => "sRGB".to_owned(),
            65535 => "Undefined".to_owned(),
            other => format!("Unknown ({other})"),
        }),
        TAG_EXIF_IMAGE_WIDTH | TAG_EXIF_IMAGE_HEIGHT => Some(format!("{} pixels", d.integer(tag)?)),
        TAG_FOCAL_PLANE_X_RESOLUTION | TAG_FOCAL_PLANE_Y_RESOLUTION => {
            focal_plane_resolution(d, tag)
        }
        TAG_FOCAL_PLANE_RESOLUTION_UNIT => focal_plane_unit(d),
        TAG_SENSING_METHOD => indexed(
            d,
            tag,
            1,
            &[
                Some("(Not defined)"),
                Some("One-chip color area sensor"),
                Some("Two-chip color area sensor"),
                Some("Three-chip color area sensor"),
                Some("Color sequential area sensor"),
                None,
                Some("Trilinear sensor"),
                Some("Color sequential linear sensor"),
            ],
        ),
        TAG_FILE_SOURCE => named(
            d,
            tag,
            1,
            &[
                "Film Scanner",
                "Reflection Print Scanner",
                "Digital Still Camera (DSC)",
            ],
        ),
        TAG_SCENE_TYPE => named(d, tag, 1, &["Directly photographed image"]),
        TAG_CUSTOM_RENDERED => named(d, tag, 0, &["Normal process", "Custom process"]),
        TAG_EXPOSURE_MODE => named(
            d,
            tag,
            0,
            &["Auto exposure", "Manual exposure", "Auto bracket"],
        ),
        TAG_WHITE_BALANCE_MODE => named(d, tag, 0, &["Auto white balance", "Manual white balance"]),
        TAG_DIGITAL_ZOOM_RATIO => {
            let value = d.rational(tag)?;
            Some(if value.numerator == 0 {
                "Digital zoom not used".to_owned()
            } else {
                decimal("0.#", value.double_value())
            })
        }
        TAG_35MM_FILM_EQUIV_FOCAL_LENGTH => {
            let value = d.integer(tag)?;
            Some(if value == 0 {
                "Unknown".to_owned()
            } else {
                focal_length(f64::from(value))
            })
        }
        TAG_SCENE_CAPTURE_TYPE => named(
            d,
            tag,
            0,
            &["Standard", "Landscape", "Portrait", "Night scene"],
        ),
        TAG_GAIN_CONTROL => named(
            d,
            tag,
            0,
            &[
                "None",
                "Low gain up",
                "Low gain down",
                "High gain up",
                "High gain down",
            ],
        ),
        TAG_CONTRAST => named(d, tag, 0, &["None", "Soft", "Hard"]),
        TAG_SATURATION => named(d, tag, 0, &["None", "Low saturation", "High saturation"]),
        TAG_SHARPNESS => named(d, tag, 0, &["None", "Low", "Hard"]),
        TAG_SUBJECT_DISTANCE_RANGE => named(
            d,
            tag,
            0,
            &["Unknown", "Macro", "Close view", "Distant view"],
        ),
        TAG_LENS_SPECIFICATION => lens_specification(d, tag),
        TAG_EXTRA_SAMPLES => named(
            d,
            tag,
            0,
            &["Unspecified", "Associated alpha", "Unassociated alpha"],
        ),
        TAG_SAMPLE_FORMAT => Some(sample_format(&d.ints(tag)?)),
        _ => base(d, tag),
    }
}

/// `GpsDescriptor.getDescription`.
fn gps(directory: &Directory, tag: i32) -> Described {
    if tag == GPS_TIME_STAMP {
        if let Some(components) = directory.rationals(tag) {
            if components.len() < 3 {
                return Err(Unported::new(
                    "a GPS time stamp of fewer than three values, which metadata-extractor throws on",
                ));
            }
        }
    }
    Ok(gps_value(directory, tag))
}

fn gps_value(directory: &Directory, tag: i32) -> Option<String> {
    let d = directory;
    let trimmed = |tag: i32| d.string(tag).map(|value| java_trim(&value).to_owned());
    let case = |value: &str, name: &str| crate::java::equals_ignore_case(value, name);
    match tag {
        GPS_VERSION_ID => version_bytes(d, tag, 1),
        GPS_ALTITUDE | GPS_H_POSITIONING_ERROR => Some(format!(
            "{} metres",
            decimal("0.##", d.rational(tag)?.double_value())
        )),
        GPS_ALTITUDE_REF => named(d, tag, 0, &["Sea level", "Below sea level"]),
        GPS_STATUS => {
            let value = trimmed(tag)?;
            Some(if case(&value, "A") {
                "Active (Measurement in progress)".to_owned()
            } else if case(&value, "V") {
                "Void (Measurement Interoperability)".to_owned()
            } else {
                format!("Unknown ({value})")
            })
        }
        GPS_MEASURE_MODE => {
            let value = trimmed(tag)?;
            Some(if case(&value, "2") {
                "2-dimensional measurement".to_owned()
            } else if case(&value, "3") {
                "3-dimensional measurement".to_owned()
            } else {
                format!("Unknown ({value})")
            })
        }
        GPS_DOP => Some(decimal("0.##", d.rational(tag)?.double_value())),
        GPS_SPEED_REF => speed_reference(d),
        GPS_SPEED => {
            let value = d.rational(tag)?;
            let unit = speed_reference(d);
            Some(format!(
                "{} {}",
                decimal("0.##", value.double_value()),
                unit.map_or_else(|| "unit".to_owned(), |unit| unit.to_lowercase())
            ))
        }
        GPS_TRACK_REF | GPS_IMG_DIRECTION_REF | GPS_DEST_BEARING_REF => {
            let value = trimmed(tag)?;
            Some(if case(&value, "T") {
                "True direction".to_owned()
            } else if case(&value, "M") {
                "Magnetic direction".to_owned()
            } else {
                format!("Unknown ({value})")
            })
        }
        GPS_TRACK | GPS_IMG_DIRECTION | GPS_DEST_BEARING => {
            let value = match d.rational(tag) {
                Some(angle) => Some(decimal("0.##", angle.double_value())),
                None => d.string(tag),
            }?;
            let value = java_trim(&value);
            (!value.is_empty()).then(|| format!("{value} degrees"))
        }
        GPS_DEST_LATITUDE => destination(d, GPS_DEST_LATITUDE, GPS_DEST_LATITUDE_REF, "S"),
        GPS_DEST_LONGITUDE => destination(d, GPS_DEST_LONGITUDE, GPS_DEST_LONGITUDE_REF, "W"),
        GPS_DEST_DISTANCE_REF => distance_reference(d),
        GPS_DEST_DISTANCE => {
            let value = d.rational(tag)?;
            let unit = distance_reference(d);
            Some(format!(
                "{} {}",
                decimal("0.##", value.double_value()),
                unit.map_or_else(|| "unit".to_owned(), |unit| unit.to_lowercase())
            ))
        }
        GPS_TIME_STAMP => {
            let components = d.rationals(tag)?;
            let [hours, minutes, seconds, ..] = components else {
                return None;
            };
            Some(format!(
                "{:02}:{:02}:{} UTC",
                hours.int_value(),
                minutes.int_value(),
                decimal("00.000", seconds.double_value())
            ))
        }
        GPS_LATITUDE => {
            geo::location(d).map(|location| geo::degrees_minutes_seconds(location.latitude))
        }
        GPS_LONGITUDE => {
            geo::location(d).map(|location| geo::degrees_minutes_seconds(location.longitude))
        }
        GPS_PROCESSING_METHOD | GPS_AREA_INFORMATION => encoded_text(d, tag),
        GPS_DIFFERENTIAL => named(d, tag, 0, &["No Correction", "Differential Corrected"]),
        _ => base(d, tag),
    }
}

fn speed_reference(directory: &Directory) -> Option<String> {
    let value = java_trim(&directory.string(GPS_SPEED_REF)?).to_owned();
    let case = |name: &str| crate::java::equals_ignore_case(&value, name);
    Some(if case("K") {
        "km/h".to_owned()
    } else if case("M") {
        "mph".to_owned()
    } else if case("N") {
        "knots".to_owned()
    } else {
        format!("Unknown ({value})")
    })
}

fn distance_reference(directory: &Directory) -> Option<String> {
    let value = java_trim(&directory.string(GPS_DEST_DISTANCE_REF)?).to_owned();
    let case = |name: &str| crate::java::equals_ignore_case(&value, name);
    Some(if case("K") {
        "kilometers".to_owned()
    } else if case("M") {
        "miles".to_owned()
    } else if case("N") {
        "knots".to_owned()
    } else {
        format!("Unknown ({value})")
    })
}

fn destination(directory: &Directory, tag: i32, reference: i32, positive: &str) -> Option<String> {
    let values = directory.rationals(tag)?;
    let reference = directory.string(reference)?;
    let [degrees, minutes, seconds] = values else {
        return None;
    };
    let decimal = geo::to_decimal(
        *degrees,
        *minutes,
        *seconds,
        crate::java::equals_ignore_case(&reference, positive),
    )?;
    Some(geo::degrees_minutes_seconds(decimal))
}

/// `FileSystemDescriptor`: the size in bytes.
fn file_system(directory: &Directory, tag: i32) -> Option<String> {
    const TAG_FILE_SIZE: i32 = 2;
    if tag == TAG_FILE_SIZE {
        return Some(format!("{} bytes", directory.long(tag)?));
    }
    base(directory, tag)
}
