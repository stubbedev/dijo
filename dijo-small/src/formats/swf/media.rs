//! The images and sounds of a movie, handed to the embedded document extractor.

use std::borrow::Cow;

use super::{Movie, Tag, Warning, DEFINE_BITS, DEFINE_BITS_JPEG_2, DEFINE_BITS_JPEG_4, NAME};
use crate::formats::embedded::{ResourceType, Wrapping, RESOURCE_NAME};
use crate::formats::metadata::TikaMetadata;
use crate::formats::Failure;

const SOI: [u8; 2] = [0xFF, 0xD8];
const EOI: [u8; 2] = [0xFF, 0xD9];
const SOS: u8 = 0xDA;
/// What the JPEG data of many movies starts with: the end of an image and the start of one,
/// before the image itself.
const ERRONEOUS_HEADER: [u8; 4] = [0xFF, 0xD9, 0xFF, 0xD8];
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const GIF_SIGNATURE: &[u8] = b"GIF8";

/// The `DefineSound` format of MP3.
const MP3: u8 = 2;
/// The bytes of `SeekSamples` before the frames of an MP3 sound.
const MP3_SEEK_SAMPLES: usize = 2;

impl<'d> Movie<'d, '_, '_> {
    /// `DefineBits`, completed with the movie's `JPEGTables`, and `DefineBitsJPEG2`, `3` and
    /// `4`: the JPEG, PNG or GIF they hold, without the alpha channel of a JPEG.
    pub(super) fn define_bits(&mut self, tag: Tag<'d>) -> Result<(), Failure> {
        let short = |short| tag.short(short);
        let mut bytes = tag.bytes();
        let id = bytes.u16("character id").map_err(short)?;
        let image = match tag.code {
            DEFINE_BITS | DEFINE_BITS_JPEG_2 => bytes.rest(),
            _ => {
                let length = bytes.u32("alpha data offset").map_err(short)?;
                if tag.code == DEFINE_BITS_JPEG_4 {
                    bytes.skip(2, "deblocking parameter").map_err(short)?;
                }
                let length = usize::try_from(length).expect("a u32 fits a usize");
                bytes.take(length, "image data").map_err(short)?
            }
        };
        if image.starts_with(PNG_SIGNATURE) {
            return self.embed(image, format!("image-{id}.png"));
        }
        if image.starts_with(GIF_SIGNATURE) {
            return self.embed(image, format!("image-{id}.gif"));
        }
        let joined;
        let image = match (tag.code, self.jpeg_tables) {
            (DEFINE_BITS, Some(tables)) => {
                joined = [tables, image].concat();
                &joined[..]
            }
            _ => image,
        };
        self.embed(&jpeg(image), format!("image-{id}.jpg"))
    }

    /// `DefineSound`: an MP3 sound; a sound in another format is not handed on, and a warning
    /// names the format.
    pub(super) fn define_sound(&mut self, tag: Tag<'d>) -> Result<(), Failure> {
        let short = |short| tag.short(short);
        let mut bytes = tag.bytes();
        let id = bytes.u16("sound id").map_err(short)?;
        let format = bytes.u8("sound format").map_err(short)? >> 4;
        bytes.skip(4, "sample count").map_err(short)?;
        if format != MP3 {
            self.warn(Warning::SoundFormat(format), || {
                format!("{NAME} hands on MP3 sounds alone, not those in the sound format {format}")
            });
            return Ok(());
        }
        bytes
            .skip(MP3_SEEK_SAMPLES, "seek samples")
            .map_err(short)?;
        self.embed(bytes.rest(), format!("sound-{id}.mp3"))
    }

    /// Hands `data`, named `name`, to the embedded document extractor.
    fn embed(&mut self, data: &[u8], name: String) -> Result<(), Failure> {
        let mut metadata = TikaMetadata::new();
        metadata.set(RESOURCE_NAME, name);
        ResourceType::Inline.set(&mut metadata);
        self.xhtml
            .parse_embedded(data, &metadata, Wrapping::PackageEntry)
    }
}

/// The JPEG `data` as a JPEG decoder reads it: without the erroneous header Flash writes before
/// it, and without the end and start of an image Flash writes between its tables and its image,
/// before the start of its scan. Data that is not JPEG is as it is, for its parser to judge.
fn jpeg(data: &[u8]) -> Cow<'_, [u8]> {
    let data = data.strip_prefix(&ERRONEOUS_HEADER[..]).unwrap_or(data);
    if !data.starts_with(&SOI) {
        return Cow::Borrowed(data);
    }
    let mut cuts = Vec::new();
    let mut at = SOI.len();
    loop {
        match data.get(at..at + 2) {
            Some(marker) if marker == EOI => {
                if data.get(at + 2..at + 4) != Some(&SOI[..]) {
                    break;
                }
                cuts.push(at);
                at += ERRONEOUS_HEADER.len();
            }
            Some(&[0xFF, marker]) if marker != SOS => {
                let Some(length) = data.get(at + 2..at + 4) else {
                    break;
                };
                at += 2 + usize::from(u16::from_be_bytes([length[0], length[1]]));
            }
            Some(_) | None => break,
        }
    }
    if cuts.is_empty() {
        return Cow::Borrowed(data);
    }
    let mut joined = Vec::with_capacity(data.len() - cuts.len() * ERRONEOUS_HEADER.len());
    let mut from = 0;
    for cut in cuts {
        joined.extend_from_slice(&data[from..cut]);
        from = cut + ERRONEOUS_HEADER.len();
    }
    joined.extend_from_slice(&data[from..]);
    Cow::Owned(joined)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A JPEG of an 8 by 8 grey square: its one block's DC difference and end of block coded
    /// with one-symbol tables.
    pub(in crate::formats::swf) fn grey_jpeg() -> Vec<u8> {
        let segment = |marker: u8, body: &[u8]| {
            let length = u16::try_from(body.len() + 2).unwrap().to_be_bytes();
            [&[0xFF, marker][..], &length, body].concat()
        };
        let huffman = |class: u8| {
            let mut counts = [0u8; 16];
            counts[0] = 1;
            [&[class][..], &counts, &[0]].concat()
        };
        [
            SOI.to_vec(),
            segment(0xDB, &[[0u8].as_slice(), &[1; 64]].concat()),
            segment(0xC0, &[8, 0, 8, 0, 8, 1, 1, 0x11, 0]),
            segment(0xC4, &huffman(0x00)),
            segment(0xC4, &huffman(0x10)),
            segment(SOS, &[1, 1, 0, 0, 63, 0]),
            vec![0x3F],
            EOI.to_vec(),
        ]
        .concat()
    }

    #[test]
    fn removes_the_end_and_start_flash_writes_before_and_within_jpegs() {
        let image = grey_jpeg();
        assert!(matches!(jpeg(&image), Cow::Borrowed(_)));
        let erroneous = [&ERRONEOUS_HEADER[..], &image[..]].concat();
        assert_eq!(jpeg(&erroneous), image);
        let (tables, scan) = image.split_at(image.len() - 13);
        let split = [tables, &EOI, &SOI, scan].concat();
        assert_eq!(&*jpeg(&split), &image[..]);
        let after_scan = [&image[..image.len() - 2], &EOI, &SOI, &EOI].concat();
        assert!(matches!(jpeg(&after_scan), Cow::Borrowed(_)));
        assert_eq!(&*jpeg(b"GIF89a"), b"GIF89a");
        let cut = &image[..5];
        assert_eq!(&*jpeg(cut), cut);
    }
}
