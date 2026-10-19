//! Better Portable Graphics images, their header read as the BPG specification lays it out:
//! the bit depth, colour space and size, and the Exif and XMP its extensions hold. Tika's
//! `BPGParser` reads the header's bit fields from the wrong end, so it takes the bit depth for
//! 10, never sees the extensions and loses their metadata (#132).

use super::extractor::{self, Target};
use super::metadata_extractor::{exif, Metadata};
use super::{dates_zone, fails, Decline, Read};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xmp;

const SIGNATURE: [u8; 4] = [b'B', b'P', b'G', 0xfb];
const EXTENSION_TAG_EXIF: u32 = 1;
const EXTENSION_TAG_XMP: u32 = 3;
/// The bytes a `ue7(32)` value takes at most.
const UE7_MAX: usize = 5;
const COLOR_MODE: &str = "photoshop:ColorMode";

/// Where the TIFF structure of an Exif extension starts: after the zero byte left of JPEG's
/// `Exif\0\0` header, which `bpgenc` writes before it, or at its start.
fn tiff_start(exif: &[u8]) -> i64 {
    match exif {
        [0, b'I', b'I', ..] | [0, b'M', b'M', ..] => 1,
        _ => 0,
    }
}

/// The header's `color_space`, as `photoshop:ColorMode` names it.
fn colour_mode(colour_space: u8) -> Option<&'static str> {
    match colour_space {
        0 => Some("YCbCr Colour"),
        1 => Some("RGB Colour"),
        2 => Some("YCgCo Colour"),
        3 => Some("YCbCr BT.709 Colour"),
        4 => Some("YCbCr BT.2020 Colour"),
        _ => None,
    }
}

/// The header, read front to back.
struct Stream<'a> {
    input: &'a [u8],
    at: usize,
}

impl<'a> Stream<'a> {
    /// The next `length` bytes, those of the image's `what`.
    fn bytes(&mut self, length: usize, what: &str) -> Result<&'a [u8], Decline<'static>> {
        let bytes = self
            .at
            .checked_add(length)
            .and_then(|end| self.input.get(self.at..end))
            .ok_or_else(|| {
                fails(format!(
                    "the BPG image ends within the {length} bytes of its {what} at {}",
                    self.at
                ))
            })?;
        self.at += length;
        Ok(bytes)
    }

    fn byte(&mut self, what: &str) -> Result<u8, Decline<'static>> {
        Ok(self.bytes(1, what)?[0])
    }

    /// A `ue7(32)`: seven bits a byte, big end first, up to the byte with its top bit clear, in
    /// at most five bytes and 32 bits.
    fn ue7(&mut self, what: &str) -> Result<u32, Decline<'static>> {
        let start = self.at;
        let mut value: u32 = 0;
        for _ in 0..UE7_MAX {
            let byte = self.byte(what)?;
            value = value
                .checked_mul(1 << 7)
                .map(|shifted| shifted | u32::from(byte & 0x7f))
                .ok_or_else(|| {
                    fails(format!("the BPG {what} at {start} does not fit in 32 bits"))
                })?;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(fails(format!(
            "the BPG {what} at {start} runs past {UE7_MAX} bytes"
        )))
    }

    /// A `ue7(32)` length.
    fn length(&mut self, what: &str) -> Result<usize, Decline<'static>> {
        let at = self.at;
        let length = self.ue7(what)?;
        usize::try_from(length)
            .map_err(|_| fails(format!("the BPG {what} at {at}, {length}, is out of range")))
    }
}

/// The metadata of the BPG image `input` into `metadata`, which is then the image's.
pub(super) fn extract_metadata(
    input: &[u8],
    mut metadata: TikaMetadata,
) -> Result<Read, Decline<'static>> {
    let mut stream = Stream { input, at: 0 };
    if stream.bytes(SIGNATURE.len(), "signature")? != SIGNATURE {
        return Err(fails("BPG magic signature invalid".to_owned()));
    }
    let bit_depth = (stream.byte("pixel format")? & 0x0f) + 8;
    let flags = stream.byte("colour space")?;
    let has_extensions = flags & 0x08 != 0;
    let width = stream.ue7("picture width")?;
    let height = stream.ue7("picture height")?;
    stream.ue7("picture data length")?;
    let mut prefix_route_names = 0;
    if has_extensions {
        let extensions_length = stream.length("extension data length")?;
        let mut extensions = Stream {
            input: stream.bytes(extensions_length, "extension data")?,
            at: 0,
        };
        while extensions.at < extensions.input.len() {
            let tag = extensions.ue7("extension tag")?;
            let length = extensions.length("extension length")?;
            let data = extensions.bytes(length, "extension")?;
            match tag {
                EXTENSION_TAG_EXIF => {
                    let mut directories = Metadata::default();
                    match exif::read_tiff(data, tiff_start(data), &mut directories) {
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
        }
    }
    xmp::set(
        &mut metadata,
        &extractor::BITS_PER_SAMPLE,
        &bit_depth.to_string(),
    );
    if let Some(colour_mode) = colour_mode(flags >> 4) {
        metadata.set(COLOR_MODE, colour_mode);
    }
    xmp::set(&mut metadata, &extractor::IMAGE_LENGTH, &height.to_string());
    xmp::set(&mut metadata, &extractor::IMAGE_WIDTH, &width.to_string());
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
    fn reads_ue7_as_the_specification_lays_it_out() {
        let mut stream = Stream {
            input: &[0x81, 0x00, 0x05, 0x8f, 0xff, 0xff, 0xff, 0x7f],
            at: 0,
        };
        assert_eq!(stream.ue7("first").unwrap(), 128);
        assert_eq!(stream.ue7("second").unwrap(), 5);
        assert_eq!(stream.ue7("third").unwrap(), u32::MAX);
        assert!(stream.ue7("fourth").is_err());
        for long in [&[0x90, 0x80, 0x80, 0x80, 0x00][..], &[0x81; 6]] {
            assert!(Stream { input: long, at: 0 }.ue7("long").is_err());
        }
    }

    #[test]
    fn reads_the_bit_depth_and_colour_space_from_the_high_bits() {
        let mut header = SIGNATURE.to_vec();
        header.extend_from_slice(&[0x22, 0x10, 0x81, 0x00, 0x40, 0x05]);
        let read = extract_metadata(&header, TikaMetadata::new()).unwrap();
        let value = |name: &str| read.metadata.values(name).join("|");
        assert_eq!(value("tiff:BitsPerSample"), "10");
        assert_eq!(value(COLOR_MODE), "RGB Colour");
        assert_eq!(value("tiff:ImageWidth"), "128");
        assert_eq!(value("tiff:ImageLength"), "64");
        let mut extended = SIGNATURE.to_vec();
        extended.extend_from_slice(&[0x20, 0x08, 0x02, 0x02, 0x05, 0x03, 0x09, 0x02, b'a', b'b']);
        assert!(extract_metadata(&extended, TikaMetadata::new()).is_err());
        extended[9] = 0x04;
        assert!(extract_metadata(&extended, TikaMetadata::new()).is_ok());
    }
}
