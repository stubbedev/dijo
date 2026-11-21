//! Windows icons and cursors, as Tika's `ICOParser` reads them: the directory and the header of
//! each image it lists, for the size and colour depth of the largest image, the list of all of
//! them and a cursor's hotspot, and an empty document. The images are not decoded.

use super::fails;
use crate::config::ParserConfigs;
use crate::formats::embedded;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Footprint, Parser, Prepared};

const ICO_TYPE: &str = "image/vnd.microsoft.icon";
const ICO_ALIAS: &str = "image/x-icon";
const CUR_TYPE: &str = "image/x-win-bitmap";
const TYPE_ICON: u16 = 1;
const TYPE_CURSOR: u16 = 2;
const COUNT_OFFSET: usize = 4;
const HEADER_SIZE: usize = 6;
const ENTRY_SIZE: usize = 16;
const BITMAP_INFO_HEADER_SIZE: u32 = 40;
/// The PNG signature, then the first chunk's length and type: an IHDR of 13 bytes.
const PNG_HEADER_START: [u8; 16] = [
    0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
];
const PNG_IHDR_SIZE: u64 = 8 + 8 + 13;
/// `MAX_FILE_SIZE`: how much of the file is read.
const MAX_FILE_SIZE: usize = 16 * 1024 * 1024;
/// `MAX_DIMENSION`: the largest width or height an image's header is believed in.
const MAX_DIMENSION: i64 = 65535;

const IMAGE_COUNT: &str = "icon:image-count";
const IMAGES: &str = "icon:images";
const HOTSPOT_X: &str = "icon:hotspot-x";
const HOTSPOT_Y: &str = "icon:hotspot-y";
const IMAGE_WIDTH: &str = "tiff:ImageWidth";
const IMAGE_LENGTH: &str = "tiff:ImageLength";
const BITS_PER_SAMPLE: &str = "tiff:BitsPerSample";
const SAMPLES_PER_PIXEL: &str = "tiff:SamplesPerPixel";

/// How an image of the directory is stored, as far as its header tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Png,
    Bmp,
    Unknown,
}

impl Encoding {
    fn name(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Bmp => "bmp",
            Self::Unknown => "unknown",
        }
    }
}

fn u16_le(file: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([file[at], file[at + 1]])
}

fn u32_le(file: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(file[at..at + 4].try_into().expect("four bytes"))
}

fn i32_le(file: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(file[at..at + 4].try_into().expect("four bytes"))
}

fn u32_be(file: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(file[at..at + 4].try_into().expect("four bytes"))
}

/// An image the directory lists: its size and depth, from its own header where that can be
/// read and from the directory otherwise, and a cursor's hotspot.
#[derive(Debug, Clone, Copy)]
struct Image {
    width: i64,
    height: i64,
    bits_per_pixel: u32,
    bits_per_sample: u32,
    samples_per_pixel: u32,
    hotspot_x: u16,
    hotspot_y: u16,
    encoding: Encoding,
}

impl Image {
    /// `Image.read`: the directory entry at `entry` and the header of the image it points to;
    /// none when its data lies outside the file.
    fn read(file: &[u8], entry: usize, cursor: bool) -> Option<Self> {
        let directory_size = |size: u8| if size == 0 { 256 } else { i64::from(size) };
        let mut image = Self {
            width: directory_size(file[entry]),
            height: directory_size(file[entry + 1]),
            bits_per_pixel: 0,
            bits_per_sample: 0,
            samples_per_pixel: 0,
            hotspot_x: 0,
            hotspot_y: 0,
            encoding: Encoding::Unknown,
        };
        if cursor {
            image.hotspot_x = u16_le(file, entry + 4);
            image.hotspot_y = u16_le(file, entry + 6);
        } else {
            image.set_depth(u32::from(u16_le(file, entry + 6)));
        }
        let size = u64::from(u32_le(file, entry + 8));
        let offset = u32_le(file, entry + 12) as usize;
        if offset < HEADER_SIZE || offset >= file.len() {
            return None;
        }
        let available = size.min((file.len() - offset) as u64);
        if available >= PNG_IHDR_SIZE && file[offset..].starts_with(&PNG_HEADER_START) {
            let width = i64::from(u32_be(file, offset + 16));
            let height = i64::from(u32_be(file, offset + 20));
            if image.try_set_size(width, height) {
                image.encoding = Encoding::Png;
                image.set_png_depth(u32::from(file[offset + 24]), file[offset + 25]);
            }
        } else if available >= u64::from(BITMAP_INFO_HEADER_SIZE)
            && u32_le(file, offset) == BITMAP_INFO_HEADER_SIZE
        {
            let height = i64::from(i32_le(file, offset + 8)).abs() / 2;
            if image.try_set_size(i64::from(i32_le(file, offset + 4)), height) {
                image.encoding = Encoding::Bmp;
                image.set_depth(u32::from(u16_le(file, offset + 14)));
            }
        }
        Some(image)
    }

    /// `trySetSize`: the size, if both lie between 1 and [`MAX_DIMENSION`].
    fn try_set_size(&mut self, width: i64, height: i64) -> bool {
        let believed = 1..=MAX_DIMENSION;
        if !believed.contains(&width) || !believed.contains(&height) {
            return false;
        }
        self.width = width;
        self.height = height;
        true
    }

    /// `setDepth`: a DIB's colour depth split into samples; one no DIB has leaves the depth as
    /// it was.
    fn set_depth(&mut self, bits_per_pixel: u32) {
        let (bits_per_sample, samples_per_pixel) = match bits_per_pixel {
            32 | 24 => (8, bits_per_pixel / 8),
            16 => (5, 3),
            8 | 4 | 1 => (bits_per_pixel, 1),
            _ => return,
        };
        self.bits_per_sample = bits_per_sample;
        self.samples_per_pixel = samples_per_pixel;
        self.bits_per_pixel = bits_per_pixel;
    }

    /// `setPngDepth`: a bit depth or colour type PNG does not define leaves the depth as it was.
    fn set_png_depth(&mut self, bit_depth: u32, color_type: u8) {
        let samples = match color_type {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            6 => 4,
            _ => return,
        };
        if bit_depth > 16 || !bit_depth.is_power_of_two() {
            return;
        }
        self.bits_per_sample = bit_depth;
        self.samples_per_pixel = samples;
        self.bits_per_pixel = bit_depth * samples;
    }

    /// `outranks`: the larger area wins, then the higher colour depth.
    fn outranks(&self, other: &Self) -> bool {
        let (area, other_area) = (self.width * self.height, other.width * other.height);
        area > other_area || area == other_area && self.bits_per_pixel > other.bits_per_pixel
    }

    /// `describe`: `WIDTHxHEIGHT@BITSbpp ENCODING`, without a depth not known.
    fn describe(&self) -> String {
        let depth = if self.bits_per_pixel > 0 {
            format!("@{}bpp", self.bits_per_pixel)
        } else {
            String::new()
        };
        format!(
            "{}x{}{depth} {}",
            self.width,
            self.height,
            self.encoding.name()
        )
    }
}

/// `EmbeddedDocumentUtil.recordException` of the `TikaException` the parser warns with.
fn warn(metadata: &mut TikaMetadata, message: &str) {
    embedded::record_exception(
        metadata,
        &format!("org.apache.tika.exception.TikaException: {message}"),
    );
}

/// `extractMetadata`: the metadata of the icon or cursor `file`.
fn read(file: &[u8], metadata: &mut TikaMetadata) -> Result<(), Decline<'static>> {
    if file.len() < COUNT_OFFSET || u16_le(file, 0) != 0 {
        return Err(fails("Not an ICO or CUR file".to_owned()));
    }
    let kind = u16_le(file, 2);
    if kind != TYPE_ICON && kind != TYPE_CURSOR {
        return Err(fails(format!("Not an ICO or CUR file: type {kind}")));
    }
    let cursor = kind == TYPE_CURSOR;
    metadata.set("Content-Type", if cursor { CUR_TYPE } else { ICO_TYPE });
    if file.len() < HEADER_SIZE {
        warn(metadata, "The header ends before the image count");
        return Ok(());
    }
    let count = usize::from(u16_le(file, COUNT_OFFSET));
    if count == 0 {
        metadata.set(IMAGE_COUNT, "0");
        warn(metadata, "The directory lists no images");
        return Ok(());
    }
    let mut largest: Option<Image> = None;
    let mut listed = 0;
    let mut unreadable = 0;
    for index in 0..count {
        let entry = HEADER_SIZE + index * ENTRY_SIZE;
        if entry + ENTRY_SIZE > file.len() {
            unreadable += count - index;
            break;
        }
        let Some(image) = Image::read(file, entry, cursor) else {
            unreadable += 1;
            continue;
        };
        if image.encoding == Encoding::Unknown {
            unreadable += 1;
        }
        metadata.add(IMAGES, image.describe());
        listed += 1;
        if largest.is_none_or(|largest| image.outranks(&largest)) {
            largest = Some(image);
        }
    }
    metadata.set(IMAGE_COUNT, listed.to_string());
    if let Some(largest) = largest {
        metadata.set(IMAGE_WIDTH, largest.width.to_string());
        metadata.set(IMAGE_LENGTH, largest.height.to_string());
        if largest.bits_per_sample > 0 {
            metadata.set(BITS_PER_SAMPLE, largest.bits_per_sample.to_string());
            metadata.set(SAMPLES_PER_PIXEL, largest.samples_per_pixel.to_string());
        }
        if cursor {
            metadata.set(HOTSPOT_X, largest.hotspot_x.to_string());
            metadata.set(HOTSPOT_Y, largest.hotspot_y.to_string());
        }
    }
    if unreadable > 0 {
        warn(
            metadata,
            &format!(
                "{unreadable} of {count} images lie outside the file or have no readable header"
            ),
        );
    }
    Ok(())
}

/// Tika's `ICOParser`.
pub struct IcoParser;

impl Parser for IcoParser {
    fn name(&self) -> &'static str {
        "dijo::ICOParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.image.ICOParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[ICO_TYPE, ICO_ALIAS, CUR_TYPE]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        _: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'static>> {
        let mut read_metadata = metadata.clone();
        read(&input[..input.len().min(MAX_FILE_SIZE)], &mut read_metadata)?;
        Ok(Box::new(move |xhtml| {
            *xhtml.metadata_mut() = read_metadata;
            Ok(())
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An icon or cursor file of `kind` whose directory lists `entries`, each its directory's
    /// eight leading bytes and the image data it points to.
    fn file(kind: u16, entries: &[([u8; 8], Vec<u8>)]) -> Vec<u8> {
        let mut file = vec![0, 0];
        file.extend_from_slice(&kind.to_le_bytes());
        file.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        let mut offset = HEADER_SIZE + entries.len() * ENTRY_SIZE;
        for (directory, data) in entries {
            file.extend_from_slice(directory);
            file.extend_from_slice(&(data.len() as u32).to_le_bytes());
            file.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += data.len();
        }
        for (_, data) in entries {
            file.extend_from_slice(data);
        }
        file
    }

    fn png(width: u32, height: u32, bit_depth: u8, color_type: u8) -> Vec<u8> {
        let mut png = PNG_HEADER_START.to_vec();
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
        png
    }

    fn bmp(width: i32, height: i32, bits: u16) -> Vec<u8> {
        let mut bmp = BITMAP_INFO_HEADER_SIZE.to_le_bytes().to_vec();
        bmp.extend_from_slice(&width.to_le_bytes());
        bmp.extend_from_slice(&height.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&bits.to_le_bytes());
        bmp.resize(BITMAP_INFO_HEADER_SIZE as usize, 0);
        bmp
    }

    fn value(metadata: &TikaMetadata, name: &str) -> String {
        metadata.values(name).join("|")
    }

    #[test]
    fn reports_the_largest_image_from_the_images_own_headers() {
        let icon = file(
            TYPE_ICON,
            &[
                ([32, 32, 0, 0, 1, 0, 32, 0], bmp(32, -64, 32)),
                ([0, 0, 0, 0, 1, 0, 0, 0], png(256, 256, 8, 6)),
                ([16, 16, 0, 0, 1, 0, 4, 0], vec![1, 2, 3]),
            ],
        );
        let mut metadata = TikaMetadata::new();
        read(&icon, &mut metadata).unwrap();
        assert_eq!(value(&metadata, "Content-Type"), ICO_TYPE);
        assert_eq!(value(&metadata, IMAGE_COUNT), "3");
        assert_eq!(
            value(&metadata, IMAGES),
            "32x32@32bpp bmp|256x256@32bpp png|16x16@4bpp unknown"
        );
        assert_eq!(value(&metadata, IMAGE_WIDTH), "256");
        assert_eq!(value(&metadata, IMAGE_LENGTH), "256");
        assert_eq!(value(&metadata, BITS_PER_SAMPLE), "8");
        assert_eq!(value(&metadata, SAMPLES_PER_PIXEL), "4");
        assert_eq!(value(&metadata, HOTSPOT_X), "");
        assert_eq!(
            value(&metadata, embedded::EXCEPTION_WARNING),
            "org.apache.tika.exception.TikaException: 1 of 3 images lie outside the file or \
             have no readable header\n"
        );
    }

    #[test]
    fn reports_a_cursors_hotspot() {
        let cursor = file(TYPE_CURSOR, &[([32, 32, 0, 0, 5, 0, 7, 0], bmp(32, 64, 1))]);
        let mut metadata = TikaMetadata::new();
        read(&cursor, &mut metadata).unwrap();
        assert_eq!(value(&metadata, "Content-Type"), CUR_TYPE);
        assert_eq!(value(&metadata, IMAGES), "32x32@1bpp bmp");
        assert_eq!(value(&metadata, HOTSPOT_X), "5");
        assert_eq!(value(&metadata, HOTSPOT_Y), "7");
        assert_eq!(value(&metadata, embedded::EXCEPTION_WARNING), "");
    }

    #[test]
    fn warns_of_an_empty_directory_and_refuses_what_is_no_icon() {
        let mut metadata = TikaMetadata::new();
        read(&file(TYPE_ICON, &[]), &mut metadata).unwrap();
        assert_eq!(value(&metadata, IMAGE_COUNT), "0");
        assert!(value(&metadata, embedded::EXCEPTION_WARNING).contains("lists no images"));
        assert!(read(b"\0\0\x03\0\0\0", &mut TikaMetadata::new()).is_err());
        assert!(read(b"\x01\0", &mut TikaMetadata::new()).is_err());
    }

    /// Tika's test icons and cursor read as `ICOParserTest` expects them.
    #[test]
    #[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
    fn reads_tikas_test_icons_as_tika_does() {
        let documents = crate::formats::testing::conformance_corpus().join(
            "tika-parsers-standard/tika-parsers-standard-modules/tika-parser-image-module/src/test/\
             resources/test-documents",
        );
        let read_document = |name: &str| {
            let mut metadata = TikaMetadata::new();
            read(&std::fs::read(documents.join(name)).unwrap(), &mut metadata).unwrap();
            metadata
        };
        let icon = read_document("testICO.ico");
        assert_eq!(value(&icon, "Content-Type"), ICO_TYPE);
        assert_eq!(value(&icon, IMAGE_WIDTH), "256");
        assert_eq!(value(&icon, IMAGE_LENGTH), "256");
        assert_eq!(value(&icon, BITS_PER_SAMPLE), "8");
        assert_eq!(value(&icon, SAMPLES_PER_PIXEL), "4");
        assert_eq!(value(&icon, IMAGE_COUNT), "3");
        assert_eq!(
            value(&icon, IMAGES),
            "16x16@32bpp bmp|32x32@32bpp bmp|256x256@32bpp png"
        );
        assert_eq!(value(&icon, HOTSPOT_X), "");
        assert_eq!(value(&icon, embedded::EXCEPTION_WARNING), "");
        let bmp_only = read_document("testICO_bmpOnly.ico");
        assert_eq!(value(&bmp_only, IMAGE_WIDTH), "48");
        assert_eq!(value(&bmp_only, IMAGE_COUNT), "2");
        assert_eq!(value(&bmp_only, IMAGES), "16x16@32bpp bmp|48x48@32bpp bmp");
        let cursor = read_document("testCUR.cur");
        assert_eq!(value(&cursor, "Content-Type"), CUR_TYPE);
        assert_eq!(value(&cursor, IMAGE_WIDTH), "32");
        assert_eq!(value(&cursor, BITS_PER_SAMPLE), "8");
        assert_eq!(value(&cursor, SAMPLES_PER_PIXEL), "4");
        assert_eq!(value(&cursor, HOTSPOT_X), "7");
        assert_eq!(value(&cursor, HOTSPOT_Y), "5");
        assert_eq!(value(&cursor, IMAGES), "16x16@32bpp bmp|32x32@32bpp bmp");
    }
}
