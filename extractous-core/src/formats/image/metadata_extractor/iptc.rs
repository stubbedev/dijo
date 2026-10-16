//! metadata-extractor's `IptcReader`, `IptcDirectory` and `IptcDescriptor`: IPTC-IIM datasets,
//! each tag a record number and a dataset number, kept as text in the coded character set the
//! envelope names or the one the bytes read well in.

use super::descriptions::{base, named, Described};
use super::reader::Sequential;
use super::{string_value, Directory, Kind, Metadata, StringValue, Unported, Value};
use crate::text::charset::Charset;

pub(crate) const TAG_CODED_CHARACTER_SET: i32 = 0x015a;
const TAG_ENVELOPE_RECORD_VERSION: i32 = 0x0100;
const TAG_FILE_FORMAT: i32 = 0x0114;
const TAG_FILE_VERSION: i32 = 0x0116;
const TAG_DATE_SENT: i32 = 0x0146;
const TAG_TIME_SENT: i32 = 0x0150;
const TAG_ARM_VERSION: i32 = 0x017a;
const TAG_APPLICATION_RECORD_VERSION: i32 = 0x0200;
pub(crate) const TAG_OBJECT_NAME: i32 = 0x0205;
const TAG_URGENCY: i32 = 0x020a;
pub(crate) const TAG_KEYWORDS: i32 = 0x0219;
const TAG_RELEASE_DATE: i32 = 0x021e;
const TAG_RELEASE_TIME: i32 = 0x0223;
const TAG_EXPIRATION_DATE: i32 = 0x0225;
const TAG_EXPIRATION_TIME: i32 = 0x0226;
const TAG_REFERENCE_DATE: i32 = 0x022f;
const TAG_DATE_CREATED: i32 = 0x0237;
const TAG_TIME_CREATED: i32 = 0x023c;
const TAG_DIGITAL_DATE_CREATED: i32 = 0x023e;
const TAG_DIGITAL_TIME_CREATED: i32 = 0x023f;
const TAG_PROGRAM_VERSION: i32 = 0x0246;
pub(crate) const TAG_BY_LINE: i32 = 0x0250;
pub(crate) const TAG_HEADLINE: i32 = 0x0269;
pub(crate) const TAG_CAPTION: i32 = 0x0278;

const MARKER: u8 = 0x1c;
const ESC: u8 = 0x1b;

/// `IptcReader.extract`: the datasets of `bytes`, up to the first that does not fit.
pub(crate) fn extract(bytes: &[u8], metadata: &mut Metadata) -> Result<(), Unported> {
    let mut directory = Directory::new(Kind::Iptc);
    let result = read(&mut Sequential::new(bytes), &mut directory, bytes.len());
    metadata.add(directory);
    result
}

fn read(reader: &mut Sequential, directory: &mut Directory, length: usize) -> Result<(), Unported> {
    let length = length as i64;
    let mut offset: i64 = 0;
    while offset < length {
        let Ok(start) = reader.u8() else {
            directory.add_error("Unable to read starting byte of IPTC tag");
            return Ok(());
        };
        offset += 1;
        if start != MARKER {
            if offset != length {
                directory.add_error(format!(
                    "Invalid IPTC tag marker at offset {}. Expected '0x1c' but got '0x{start:x}'.",
                    offset - 1
                ));
            }
            return Ok(());
        }
        if offset + 4 > length {
            directory.add_error("Too few bytes remain for a valid IPTC tag");
            return Ok(());
        }
        let descriptor = (|| {
            let record = i32::from(reader.u8()?);
            let dataset = i32::from(reader.u8()?);
            let mut count = i64::from(reader.u16()?);
            if count > 32767 {
                count = ((count & 0x7FFF) << 16) | i64::from(reader.u16()?);
                offset += 2;
            }
            Ok::<_, super::reader::Bounds>((record, dataset, count))
        })();
        let Ok((record, dataset, count)) = descriptor else {
            directory.add_error("IPTC data segment ended mid-way through tag descriptor");
            return Ok(());
        };
        offset += 4;
        let count = count as i32;
        if offset + i64::from(count) > length {
            directory.add_error("Data for tag extends beyond end of IPTC segment");
            return Ok(());
        }
        match process_tag(reader, directory, record, dataset, count) {
            Ok(()) => {}
            Err(TagError::Bounds) => {
                directory.add_error("Error processing IPTC tag");
                return Ok(());
            }
            Err(TagError::Unported(unported)) => return Err(unported),
        }
        offset += i64::from(count);
    }
    Ok(())
}

enum TagError {
    Bounds,
    Unported(Unported),
}

impl From<super::reader::Bounds> for TagError {
    fn from(_: super::reader::Bounds) -> Self {
        Self::Bounds
    }
}

impl From<Unported> for TagError {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

fn process_tag(
    reader: &mut Sequential,
    directory: &mut Directory,
    record: i32,
    dataset: i32,
    count: i32,
) -> Result<(), TagError> {
    let tag = dataset | (record << 8);
    if count == 0 {
        directory.set(tag, Value::Text(String::new()));
        return Ok(());
    }
    let count = usize::try_from(count).map_err(|_| {
        Unported::new("an IPTC dataset of a negative length, which metadata-extractor throws on")
    })?;
    match tag {
        TAG_CODED_CHARACTER_SET => {
            let bytes = reader.bytes(count)?;
            let name = match iso2022_charset(bytes)? {
                Some(name) => name.to_owned(),
                None => super::java_string(bytes, None),
            };
            directory.set(tag, Value::Text(name));
            return Ok(());
        }
        TAG_ENVELOPE_RECORD_VERSION
        | TAG_APPLICATION_RECORD_VERSION
        | TAG_FILE_VERSION
        | TAG_ARM_VERSION
        | TAG_PROGRAM_VERSION
            if count >= 2 =>
        {
            let value = reader.u16()?;
            reader.skip(count - 2)?;
            directory.set(tag, Value::Int(i32::from(value)));
            return Ok(());
        }
        TAG_URGENCY => {
            directory.set(tag, Value::Int(i32::from(reader.u8()?)));
            reader.skip(count - 1)?;
            return Ok(());
        }
        _ => {}
    }
    let string = match directory.string(TAG_CODED_CHARACTER_SET) {
        Some(name) => string_value(reader.bytes(count)?, Charset::for_name(&name))?,
        None => {
            let bytes = reader.bytes(count)?;
            let charset = if std::str::from_utf8(bytes).is_ok() {
                Charset::named("UTF-8")
            } else {
                Charset::named("ISO-8859-1")
            };
            string_value(bytes, Some(charset))?
        }
    };
    if directory.contains(tag) {
        let mut strings: Vec<StringValue> = match directory.object(tag) {
            Some(Value::StringValues(values)) => values.clone(),
            Some(Value::StringValue(value)) => vec![value.clone()],
            _ => Vec::new(),
        };
        strings.push(string);
        directory.set(tag, Value::StringValues(strings));
    } else {
        directory.set(tag, Value::StringValue(string));
    }
    Ok(())
}

/// `Iso2022Converter.convertISO2022CharsetToJavaCharset`.
fn iso2022_charset(bytes: &[u8]) -> Result<Option<&'static str>, Unported> {
    let starts = |second: u8, third: u8| {
        bytes.len() > 2 && bytes[0] == ESC && bytes[1] == second && bytes[2] == third
    };
    if starts(b'%', b'G') {
        return Ok(Some("UTF-8"));
    }
    if starts(b'.', b'A') {
        return Ok(Some("ISO-8859-1"));
    }
    if bytes.len() > 3 && bytes[0] == ESC && bytes[1..4] == [0xe2, 0x80, 0xa2] {
        let Some(fifth) = bytes.get(4) else {
            return Err(Unported::new(
                "an IPTC coded character set of four bytes, which metadata-extractor throws on",
            ));
        };
        if *fifth == b'A' {
            return Ok(Some("ISO-8859-1"));
        }
    }
    if starts(b'-', b'A') {
        return Ok(Some("ISO-8859-1"));
    }
    Ok(None)
}

/// `IptcDescriptor.getDescription`.
pub(crate) fn describe(directory: &Directory, tag: i32) -> Described {
    Ok(match tag {
        TAG_DATE_CREATED
        | TAG_DIGITAL_DATE_CREATED
        | TAG_DATE_SENT
        | TAG_EXPIRATION_DATE
        | TAG_REFERENCE_DATE
        | TAG_RELEASE_DATE => directory.string(tag).map(|text| {
            let units: Vec<u16> = text.encode_utf16().collect();
            if units.len() == 8 {
                format!(
                    "{}:{}:{}",
                    String::from_utf16_lossy(&units[..4]),
                    String::from_utf16_lossy(&units[4..6]),
                    String::from_utf16_lossy(&units[6..])
                )
            } else {
                text
            }
        }),
        TAG_EXPIRATION_TIME
        | TAG_RELEASE_TIME
        | TAG_TIME_CREATED
        | TAG_DIGITAL_TIME_CREATED
        | TAG_TIME_SENT => directory.string(tag).map(|text| {
            let units: Vec<u16> = text.encode_utf16().collect();
            if units.len() == 6 || units.len() == 11 {
                format!(
                    "{}:{}:{}",
                    String::from_utf16_lossy(&units[..2]),
                    String::from_utf16_lossy(&units[2..4]),
                    String::from_utf16_lossy(&units[4..])
                )
            } else {
                text
            }
        }),
        TAG_FILE_FORMAT => named(
            directory,
            tag,
            0,
            &[
                "No ObjectData",
                "IPTC-NAA Digital Newsphoto Parameter Record",
                "IPTC7901 Recommended Message Format",
                "Tagged Image File Format (Adobe/Aldus Image data)",
                "Illustrator (Adobe Graphics data)",
                "AppleSingle (Apple Computer Inc)",
                "NAA 89-3 (ANPA 1312)",
                "MacBinary II",
                "IPTC Unstructured Character Oriented File Format (UCOFF)",
                "United Press International ANPA 1312 variant",
                "United Press International Down-Load Message",
                "JPEG File Interchange (JFIF)",
                "Photo-CD Image-Pac (Eastman Kodak)",
                "Bit Mapped Graphics File [.BMP] (Microsoft)",
                "Digital Audio File [.WAV] (Microsoft & Creative Labs)",
                "Audio plus Moving Video [.AVI] (Microsoft)",
                "PC DOS/Windows Executable Files [.COM][.EXE]",
                "Compressed Binary File [.ZIP] (PKWare Inc)",
                "Audio Interchange File Format AIFF (Apple Computer Inc)",
                "RIFF Wave (Microsoft Corporation)",
                "Freehand (Macromedia/Aldus)",
                "Hypertext Markup Language [.HTML] (The Internet Society)",
                "MPEG 2 Audio Layer 2 (Musicom), ISO/IEC",
                "MPEG 2 Audio Layer 3, ISO/IEC",
                "Portable Document File [.PDF] Adobe",
                "News Industry Text Format (NITF)",
                "Tape Archive [.TAR]",
                "Tidningarnas Telegrambyra NITF version (TTNITF DTD)",
                "Ritzaus Bureau NITF version (RBNITF DTD)",
                "Corel Draw [.CDR]",
            ],
        ),
        TAG_KEYWORDS => directory.strings(tag).map(|keywords| keywords.join(";")),
        _ => base(directory, tag),
    })
}
