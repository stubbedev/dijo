//! Better Portable Graphics images, as Tika's `BPGParser` reads their header: the bit depth,
//! colour space and size, and the Exif and XMP its extensions hold.

use super::extractor::{self, Target};
use super::metadata_extractor::{exif, Metadata};
use super::{dates_zone, fails, Declined, Read};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xmp;

const SIGNATURE: [u8; 4] = [b'B', b'P', b'G', 0xfb];
const EXTENSION_TAG_EXIF: i32 = 1;
const EXTENSION_TAG_XMP: i32 = 3;
/// `DEFAULT_MAX_RECORD_LENGTH`, past which an extension is taken for a memory bomb.
const MAX_RECORD_LENGTH: i32 = 50 * 1024 * 1024;
/// The bytes `EndianUtils.readUE7` adds to a value at most.
const UE7_MAX: usize = 6;
const COLOR_MODE: &str = "photoshop:ColorMode";
const EXIF_HEADER: &[u8; 6] = b"Exif\0\0";

/// The stream `extractMetadata` reads, front to back.
struct Stream<'a> {
    input: &'a [u8],
    at: usize,
}

impl Stream<'_> {
    /// `InputStream.read`: the next byte, or -1 at the end.
    fn read(&mut self) -> i32 {
        match self.input.get(self.at) {
            Some(&byte) => {
                self.at += 1;
                i32::from(byte)
            }
            None => -1,
        }
    }

    /// `IOUtils.readFully` of `length` bytes.
    fn read_fully(&mut self, length: usize) -> Result<&[u8], Declined> {
        let bytes = self
            .at
            .checked_add(length)
            .and_then(|end| self.input.get(self.at..end))
            .ok_or_else(|| {
                fails(format!(
                    "the BPG image ends within {length} bytes it declares at {}",
                    self.at
                ))
            })?;
        self.at += length;
        Ok(bytes)
    }

    /// `EndianUtils.readUE7`: seven bits a byte, big end first, up to the byte with its top bit
    /// clear; a seventh byte is read, and dropped.
    fn read_ue7(&mut self) -> Result<i64, Declined> {
        let mut value: i64 = 0;
        let mut read = 0;
        loop {
            let byte = self.read();
            if byte < 0 {
                return Err(fails(
                    "Buffer underun; expected one more byte in the BPG header".to_owned(),
                ));
            }
            if read >= UE7_MAX {
                return Ok(value);
            }
            read += 1;
            value <<= 7;
            if byte & 0x80 != 0 {
                value += i64::from(byte & 0x7f);
            } else {
                return Ok(value + i64::from(byte));
            }
        }
    }
}

/// `BPGParser.extractMetadata` into `metadata`, which is then the image's.
pub(super) fn extract_metadata(input: &[u8], mut metadata: TikaMetadata) -> Result<Read, Declined> {
    let mut stream = Stream { input, at: 0 };
    let signature = stream.read_fully(SIGNATURE.len())?;
    if signature != SIGNATURE {
        return Err(fails("BPG magic signature invalid".to_owned()));
    }
    let format = stream.read();
    let alpha_plane = format & 0x8 == 0x8;
    let bit_depth = (format >> 4) + 8;
    xmp::set(
        &mut metadata,
        &extractor::BITS_PER_SAMPLE,
        &bit_depth.to_string(),
    );
    let extension_flags = stream.read();
    let colour_mode = match extension_flags & 0x15 {
        0 => Some("YCbCr Colour"),
        1 => Some("RGB Colour"),
        2 => Some("YCgCo Colour"),
        3 => Some("YCbCrK Colour"),
        4 => Some("CMYK Colour"),
        _ => None,
    };
    if let Some(colour_mode) = colour_mode {
        metadata.set(COLOR_MODE, colour_mode);
    }
    let has_extensions = extension_flags & 16 == 16;
    let alpha_plane_2 = extension_flags & 32 == 32;
    let width = stream.read_ue7()? as i32;
    let height = stream.read_ue7()? as i32;
    xmp::set(&mut metadata, &extractor::IMAGE_LENGTH, &height.to_string());
    xmp::set(&mut metadata, &extractor::IMAGE_WIDTH, &width.to_string());
    stream.read_ue7()?;
    let extensions_length = if has_extensions {
        stream.read_ue7()?
    } else {
        0
    };
    if alpha_plane || alpha_plane_2 {
        stream.read_ue7()?;
    }
    let mut prefix_route_names = 0;
    if has_extensions {
        let mut seen: i64 = 0;
        while seen < extensions_length {
            let kind = stream.read_ue7()? as i32;
            let length = stream.read_ue7()? as i32;
            if length > MAX_RECORD_LENGTH {
                return Err(fails(format!(
                    "extension length ({length} bytes) is greater than 'maxRecordLength' \
                     ({MAX_RECORD_LENGTH} bytes)."
                )));
            }
            let size = usize::try_from(length)
                .map_err(|_| fails(format!("a BPG extension of a negative length, {length}")))?;
            let data = stream.read_fully(size)?;
            match kind {
                EXTENSION_TAG_EXIF => {
                    let mut exif_data = EXIF_HEADER.to_vec();
                    exif_data.extend_from_slice(data);
                    let mut directories = Metadata::default();
                    match exif::read_tiff(&exif_data, EXIF_HEADER.len() as i64, &mut directories) {
                        Ok(())
                        | Err(exif::TiffError::Bounds(_) | exif::TiffError::Processing(_)) => {}
                        Err(exif::TiffError::Unported(unported)) => return Err(unported.into()),
                    }
                    let mut target = Target::continuing(&mut metadata, prefix_route_names);
                    extractor::handle(&directories, &mut target, dates_zone())?;
                    prefix_route_names = target.prefix_route_names();
                }
                EXTENSION_TAG_XMP => xmp::extract(data, &mut metadata, xmp::as_is)?,
                _ => {}
            }
            seen += i64::from(length);
        }
    }
    Ok(Read {
        metadata,
        prefix_route_names,
        reads_file: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ue7_as_tika_does() {
        let mut stream = Stream {
            input: &[0x81, 0x00, 0x05],
            at: 0,
        };
        assert_eq!(stream.read_ue7().unwrap(), 128);
        assert_eq!(stream.read_ue7().unwrap(), 5);
        assert!(stream.read_ue7().is_err());
        let mut long = Stream {
            input: &[0x81; 8],
            at: 0,
        };
        assert_eq!(long.read_ue7().unwrap(), 0x8_1020_4081);
        assert_eq!(long.at, 7);
    }
}
