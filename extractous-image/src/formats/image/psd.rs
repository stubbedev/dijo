//! Tika's `PSDParser`: the size, depth and color mode in a Photoshop file's header, and the
//! caption and XMP among its image resources.

use super::extractor::{BITS_PER_SAMPLE, DESCRIPTION, IMAGE_LENGTH, IMAGE_WIDTH};
use super::image_xmp;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xmp::{self, text, Property};
use crate::formats::Unported;
use crate::input::{Pages, Paging};
use crate::text::charset::Charset;

const COLOR_MODE: Property = text("photoshop:ColorMode");
/// `Photoshop._COLOR_MODE_CHOICES_INDEXED`.
const COLOR_MODES: [&str; 13] = [
    "Bitmap",
    "Greyscale",
    "Indexed Colour",
    "RGB Color",
    "CMYK Colour",
    "Multi-Channel",
    "Duotone",
    "LAB Colour",
    "reserved",
    "reserved",
    "YCbCr Colour",
    "YCgCo Colour",
    "YCbCrK Colour",
];
const RESOURCE_SIGNATURE: i32 = 0x3842_494D;
const CAPTION: u16 = 0x03F0;
const EXIF_1: u16 = 0x0422;
const EXIF_3: u16 = 0x0423;
const XMP: u16 = 0x0424;
/// `PSDParserConfig`'s default `maxDataLengthBytes`.
const MAX_DATA_LENGTH_BYTES: i32 = 10_000_000;
const MAX_BLOCKS: usize = 10_000;

/// Why the file could not be read.
#[derive(Debug)]
pub(crate) enum PsdError {
    /// An exception `PSDParser` throws, which fails the parse.
    Fails(String),
    Unported(Unported),
}

impl From<Unported> for PsdError {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

fn fails(message: impl Into<String>) -> PsdError {
    PsdError::Fails(message.into())
}

/// The bytes `PSDParser` reads in turn, each read failing with `EOFException` at the end.
struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, count: usize) -> Result<&'a [u8], PsdError> {
        let end = self
            .position
            .checked_add(count)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| fails("EOFException"))?;
        let bytes = &self.data[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn byte(&mut self) -> Option<u8> {
        let byte = self.data.get(self.position).copied()?;
        self.position += 1;
        Some(byte)
    }

    fn u16(&mut self) -> Result<u16, PsdError> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn i32(&mut self) -> Result<i32, PsdError> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// `IOUtils.skipFully`, which rejects a negative count and fails short of it.
    fn skip(&mut self, count: i64) -> Result<(), PsdError> {
        let count = usize::try_from(count)
            .map_err(|_| fails(format!("Bytes to skip must not be negative: {count}")))?;
        self.bytes(count).map(|_| ())
    }
}

/// `PSDParser.parse` as far as the document, which it then writes with nothing in it.
pub(crate) fn read(input: &[u8], metadata: &mut TikaMetadata) -> Result<(), PsdError> {
    let mut reader = Reader {
        data: input,
        position: 0,
    };
    if reader.bytes(4)? != b"8BPS" {
        return Err(fails("PSD/PSB magic signature invalid"));
    }
    let version = reader.u16()?;
    if version != 1 && version != 2 {
        return Err(fails(format!("Invalid PSD/PSB version {version}")));
    }
    reader.bytes(6)?;
    reader.u16()?;
    let height = reader.i32()?;
    let width = reader.i32()?;
    xmp::set(metadata, &IMAGE_LENGTH, &height.to_string());
    xmp::set(metadata, &IMAGE_WIDTH, &width.to_string());
    let depth = reader.u16()?;
    xmp::set(metadata, &BITS_PER_SAMPLE, &depth.to_string());
    if let Some(mode) = COLOR_MODES.get(usize::from(reader.u16()?)) {
        xmp::set(metadata, &COLOR_MODE, mode);
    }
    let color_mode_section = reader.i32()?;
    reader.skip(i64::from(color_mode_section))?;
    let resources_section = i64::from(reader.i32()?);
    let mut read = 0i64;
    let mut blocks = 0;
    while read < resources_section && blocks < MAX_BLOCKS {
        let (id, total_length, data) = resource_block(&mut reader)?;
        read += i64::from(total_length);
        match id {
            CAPTION => {
                let caption = data
                    .len()
                    .checked_sub(1)
                    .ok_or_else(|| fails("StringIndexOutOfBoundsException: an empty caption"))?;
                let caption = Charset::named("US-ASCII")
                    .decode(&data[..caption])
                    .expect("the JDK port decodes US-ASCII");
                xmp::add(metadata, &DESCRIPTION, &caption)?;
            }
            XMP => image_xmp::scan_and_extract(data, &Paging::new(Pages::IN_MEMORY), metadata)?,
            _ => {}
        }
        blocks += 1;
    }
    Ok(())
}

/// `ResourceBlock`: its id, its length with its header, and its data when it is one `PSDParser`
/// keeps.
fn resource_block<'a>(reader: &mut Reader<'a>) -> Result<(u16, i32, &'a [u8]), PsdError> {
    let signature = reader.i32()?;
    if signature != RESOURCE_SIGNATURE {
        return Err(fails(format!(
            "Invalid Image Resource Block Signature Found, got {signature}"
        )));
    }
    let id = reader.u16()?;
    let mut name_length = 0i32;
    loop {
        let byte = reader.byte().ok_or_else(|| fails("EOFException"))?;
        name_length += 1;
        if byte == 0 {
            if name_length % 2 == 1 {
                reader.byte();
                name_length += 1;
            }
            break;
        }
    }
    let mut data_length = reader.i32()?;
    if data_length < 0 {
        return Err(fails(format!("data length must be >= 0: {data_length}")));
    }
    if data_length % 2 == 1 {
        data_length = data_length.wrapping_add(1);
    }
    if i32::MAX.wrapping_sub(data_length) < name_length.wrapping_add(10) {
        return Err(fails(format!("data length is too long:{data_length}")));
    }
    let total_length = (4 + 2 + name_length + 4).wrapping_add(data_length);
    let length = usize::try_from(data_length).expect("the length was checked to be positive");
    let data = if matches!(id, CAPTION | EXIF_1 | EXIF_3 | XMP) {
        if data_length > MAX_DATA_LENGTH_BYTES {
            return Err(fails(format!(
                "data length must be < {MAX_DATA_LENGTH_BYTES}: {data_length}"
            )));
        }
        reader.bytes(length)?
    } else {
        reader.skip(i64::from(data_length))?;
        &[]
    };
    Ok((id, total_length, data))
}
