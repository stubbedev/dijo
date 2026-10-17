//! The JDK's `BMPImageReader` as far as its header, and `BMPMetadata`'s standard tree: its
//! native tree keeps values in its nodes rather than in attributes, so Tika takes nothing from
//! it. Every BMP is read so, where Tika's path holds jai-imageio's reader too and takes
//! whichever of the two its registry, in an order nothing defines, lists first (#114).

use super::{standard_tree, Eof, Node, Read, Stream};
use crate::formats::Unported;
use crate::java::dtoa::java_float_to_string;

const VERSION_2: &str = "BMP v. 2.x";
const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_JPEG: u32 = 4;
const BI_PNG: u32 = 5;
const PROFILE_LINKED: u32 = 3;
const PROFILE_EMBEDDED: u32 = 4;
const COMPRESSION_TYPE_NAMES: [&str; 6] = [
    "BI_RGB",
    "BI_RLE8",
    "BI_RLE4",
    "BI_BITFIELDS",
    "BI_JPEG",
    "BI_PNG",
];

/// Why the BMP could not be read.
#[derive(Debug)]
pub(crate) enum BmpError {
    /// An exception the reader throws: an `IIOException`, which Tika reports as a parse error of
    /// the type, or another `IOException` or runtime exception, which fails the parse as it is.
    Fails(String),
    Unported(Unported),
}

impl From<Eof> for BmpError {
    fn from(_: Eof) -> Self {
        Self::Fails("EOFException reading the BMP header".to_owned())
    }
}

fn fails(message: impl Into<String>) -> BmpError {
    BmpError::Fails(message.into())
}

fn unported(what: &str) -> BmpError {
    BmpError::Unported(Unported::new(format!(
        "a BMP {what}, whose color model the port does not build as the JDK does"
    )))
}

/// The fields of `BMPMetadata` its standard tree reads.
#[derive(Debug, Default)]
struct Metadata {
    version: &'static str,
    bits_per_pixel: u16,
    compression: u32,
    x_pixels_per_meter: i32,
    y_pixels_per_meter: i32,
    red_mask: i32,
    green_mask: i32,
    blue_mask: i32,
    alpha_mask: i32,
    palette: Vec<u8>,
    palette_size: i32,
}

/// The masks `readHeader` builds its color model from, which `BMPMetadata` does not always keep.
#[derive(Debug, Default, Clone, Copy)]
struct Masks {
    red: i32,
    green: i32,
    blue: i32,
    alpha: i32,
}

/// `getNumImages(true)`, which is one without reading, then `getWidth(0)`, `getHeight(0)` and
/// `getImageMetadata(0)`, which read the header once.
pub(crate) fn read(input: &[u8]) -> Result<Read, BmpError> {
    let mut stream = Stream::new(input);
    let (width, height, metadata) = header(&mut stream)?;
    let compression = usize::try_from(metadata.compression)
        .ok()
        .and_then(|index| COMPRESSION_TYPE_NAMES.get(index))
        .ok_or_else(|| {
            fails(format!(
                "ArrayIndexOutOfBoundsException: {}",
                metadata.compression as i32
            ))
        })?;
    Ok(Read {
        images: 1,
        width,
        height,
        trees: vec![standard(&metadata, compression)?],
    })
}

/// `readColorPalette`: `staggeredReadByteStream` of an `int` count of bytes.
fn palette(stream: &mut Stream, size: i32) -> Result<Vec<u8>, BmpError> {
    let size =
        usize::try_from(size).map_err(|_| fails(format!("NegativeArraySizeException: {size}")))?;
    Ok(stream.bytes(size)?.to_vec())
}

/// `(int)((bitmapOffset - 14 - size) / entry)` entries of `entry` bytes each.
fn palette_entries(bitmap_offset: i64, size: i64, entry: i64) -> i32 {
    ((bitmap_offset - 14 - size) / entry) as i32
}

/// `readHeader`, with the checks the sample and color models it makes throw on.
fn header(stream: &mut Stream) -> Result<(i32, i32, Metadata), BmpError> {
    if stream.some_bytes(2) != b"BM" {
        return Err(fails("Invalid magic value for BMP file."));
    }
    let bitmap_file_size = i64::from(stream.u32_le()?);
    stream.skip(4);
    let bitmap_offset = i64::from(stream.u32_le()?);
    let size = i64::from(stream.u32_le()?);
    let (width, mut height) = if size == 12 {
        (i32::from(stream.i16_le()?), i32::from(stream.i16_le()?))
    } else {
        (stream.i32_le()?, stream.i32_le()?)
    };
    stream.u16_le()?;
    let bits_per_pixel = stream.u16_le()?;
    let mut metadata = Metadata {
        bits_per_pixel,
        ..Metadata::default()
    };
    let mut masks = Masks::default();
    let mut compression = BI_RGB;
    let mut color_space = 0;
    let unsupported_bits = || fails("Invalid bits per pixel in image header.");
    let kept = |metadata: &mut Metadata, masks: Masks| {
        metadata.red_mask = masks.red;
        metadata.green_mask = masks.green;
        metadata.blue_mask = masks.blue;
    };
    if size == 12 {
        metadata.version = VERSION_2;
        if !matches!(bits_per_pixel, 1 | 4 | 8 | 24) {
            return Err(unsupported_bits());
        }
        let entries = palette_entries(bitmap_offset, size, 3);
        metadata.palette = palette(stream, entries.wrapping_mul(3))?;
        metadata.palette_size = entries;
    } else {
        compression = stream.u32_le()?;
        stream.u32_le()?;
        metadata.compression = compression;
        metadata.x_pixels_per_meter = stream.i32_le()?;
        metadata.y_pixels_per_meter = stream.i32_le()?;
        let colors_used = stream.u32_le()?;
        stream.u32_le()?;
        if size == 40 {
            match compression {
                BI_JPEG | BI_PNG => metadata.version = "BMP v. 3.x",
                BI_RGB | BI_RLE8 | BI_RLE4 => {
                    if bitmap_offset < size + 14 {
                        return Err(fails("Invalid bitmap offset."));
                    }
                    let entries = palette_entries(bitmap_offset, size, 4);
                    metadata.palette = palette(stream, entries.wrapping_mul(4))?;
                    metadata.palette_size = entries;
                    match bits_per_pixel {
                        1 | 4 | 8 | 24 => {}
                        16 => {
                            masks = Masks {
                                red: 0x7C00,
                                green: 0x3E0,
                                blue: 0x1F,
                                alpha: 0,
                            };
                            kept(&mut metadata, masks);
                        }
                        32 => {
                            masks = Masks {
                                red: 0x00FF_0000,
                                green: 0x0000_FF00,
                                blue: 0x0000_00FF,
                                alpha: 0,
                            };
                            kept(&mut metadata, masks);
                        }
                        _ => return Err(unsupported_bits()),
                    }
                    metadata.version = "BMP v. 3.x";
                }
                BI_BITFIELDS => {
                    if !matches!(bits_per_pixel, 16 | 32) {
                        return Err(unsupported_bits());
                    }
                    masks.red = stream.u32_le()? as i32;
                    masks.green = stream.u32_le()? as i32;
                    masks.blue = stream.u32_le()? as i32;
                    kept(&mut metadata, masks);
                    if colors_used != 0 {
                        metadata.palette = palette(stream, (colors_used as i32).wrapping_mul(4))?;
                        metadata.palette_size = colors_used as i32;
                    }
                    metadata.version = "BMP v. 3.x NT";
                }
                _ => return Err(fails("Invalid compression specified in BMP stream.")),
            }
        } else if size == 52 || size == 56 || size == 108 || size == 124 {
            masks.red = stream.u32_le()? as i32;
            masks.green = stream.u32_le()? as i32;
            masks.blue = stream.u32_le()? as i32;
            if size == 52 {
                metadata.version = "BMP V2/V3 INFO";
            } else {
                masks.alpha = stream.u32_le()? as i32;
                metadata.version = match size {
                    56 => "BMP V2/V3 INFO",
                    108 => "BMP v. 4.x",
                    _ => "BMP v. 5.x",
                };
            }
            if size >= 108 {
                color_space = stream.u32_le()?;
                stream.bytes(48)?;
                if size == 124 {
                    stream.bytes(12)?;
                    stream.skip(4);
                }
            }
            let entries = palette_entries(bitmap_offset, size, 4);
            metadata.palette = palette(stream, entries.wrapping_mul(4))?;
            metadata.palette_size = entries;
            if compression != BI_JPEG && compression != BI_PNG {
                match bits_per_pixel {
                    1 | 4 | 8 | 24 => {}
                    16 if compression == BI_RGB => {
                        (masks.red, masks.green, masks.blue) = (0x7C00, 0x3E0, 0x1F);
                    }
                    32 if compression == BI_RGB => {
                        (masks.red, masks.green, masks.blue) =
                            (0x00FF_0000, 0x0000_FF00, 0x0000_00FF);
                    }
                    16 | 32 => {}
                    _ => return Err(unsupported_bits()),
                }
                kept(&mut metadata, masks);
                metadata.alpha_mask = masks.alpha;
            }
        } else {
            return Err(fails("New BMP version not implemented yet."));
        }
    }
    if height <= 0 {
        height = height.wrapping_abs();
    }
    if metadata.compression == BI_RGB && metadata.palette_size == 0 && bits_per_pixel >= 16 {
        let data_size = i64::from(width) * i64::from(height) * i64::from(bits_per_pixel) / 8;
        if data_size > bitmap_file_size - bitmap_offset {
            return Err(fails("Invalid width/height for BI_RGB image data."));
        }
    }
    if color_space == PROFILE_LINKED || color_space == PROFILE_EMBEDDED {
        return Err(unported("with an ICC profile"));
    }
    models(width, height, bits_per_pixel, compression, &metadata, masks)?;
    Ok((width, height, metadata))
}

/// The sample and color models `readHeader` makes, as far as the `IllegalArgumentException`s
/// they throw, which `getWidth` wraps in an `IIOException`.
fn models(
    width: i32,
    height: i32,
    bits: u16,
    compression: u32,
    metadata: &Metadata,
    masks: Masks,
) -> Result<(), BmpError> {
    if bits == 0 || compression == BI_JPEG || compression == BI_PNG {
        return Ok(());
    }
    if width <= 0 || height <= 0 {
        return Err(fails(format!(
            "Width ({width}) and height ({height}) must be > 0"
        )));
    }
    let pixels = i64::from(width) * i64::from(height);
    let interleaved = |bands: i64| {
        if pixels * bands > i64::from(i32::MAX) {
            Err(fails("Invalid scanline stride"))
        } else {
            Ok(())
        }
    };
    match bits {
        1 | 4 | 8 => {
            if bits == 8 {
                interleaved(1)?;
            } else if pixels > i64::from(i32::MAX) {
                return Err(fails("Dimensions are too large"));
            }
            let entry = if metadata.version == VERSION_2 { 3 } else { 4 };
            if metadata.palette.len() / entry == 0 {
                return Err(fails("Map size (0) must be >= 1"));
            }
            Ok(())
        }
        16 => packed(pixels, 16, 0xFFFF, [masks.red, masks.green, masks.blue], 0),
        32 => packed(
            pixels,
            32,
            -1,
            [masks.red, masks.green, masks.blue],
            masks.alpha,
        ),
        _ => interleaved(3),
    }
}

/// `SinglePixelPackedSampleModel` and `DirectColorModel` of `bits` per pixel: the masks,
/// limited to the data type's bits, must each be contiguous, and the port reads only masks
/// that are set and fit the pixel.
fn packed(
    pixels: i64,
    bits: u32,
    data_mask: i32,
    colors: [i32; 3],
    alpha: i32,
) -> Result<(), BmpError> {
    if pixels > i64::from(i32::MAX) {
        return Err(fails("Dimensions are too large"));
    }
    let sample_masks = colors
        .into_iter()
        .chain((alpha != 0).then_some(alpha))
        .map(|mask| mask & data_mask);
    for mask in sample_masks {
        if mask != 0 && !contiguous(mask) {
            return Err(fails(format!("Mask {mask} must be contiguous")));
        }
    }
    let fits = |mask: i32| {
        let mask = mask as u32;
        mask != 0 && contiguous(mask as i32) && 32 - mask.leading_zeros() <= bits
    };
    if colors.into_iter().all(fits) && (alpha == 0 || fits(alpha)) {
        Ok(())
    } else {
        Err(unported(
            "with masks that are unset or wider than its pixels",
        ))
    }
}

fn contiguous(mask: i32) -> bool {
    let mask = mask as u32;
    let shifted = mask >> mask.trailing_zeros();
    shifted & shifted.wrapping_add(1) == 0
}

/// `BMPMetadata.countBits`: the bits set in a positive mask, or nothing.
fn count_bits(mask: i32) -> String {
    match mask {
        1.. => mask.count_ones().to_string(),
        _ => String::new(),
    }
}

/// `getStandardTree`, of the chroma, compression, data and dimension nodes `BMPMetadata` gives.
fn standard(metadata: &Metadata, compression: &str) -> Result<Node, BmpError> {
    let chroma = match metadata.palette_size {
        1.. => Some(chroma(metadata)?),
        _ => None,
    };
    let compression = Node::new("Compression")
        .child(Node::new("CompressionTypeName").attribute("value", compression));
    let bits = match metadata.bits_per_pixel {
        24 => "8 8 8 ".to_owned(),
        16 | 32 => format!(
            "{} {}{}{}",
            count_bits(metadata.red_mask),
            count_bits(metadata.green_mask),
            count_bits(metadata.blue_mask),
            count_bits(metadata.alpha_mask)
        ),
        _ => String::new(),
    };
    let data = Node::new("Data").child(Node::new("BitsPerSample").attribute("value", bits));
    let (x, y) = (metadata.x_pixels_per_meter, metadata.y_pixels_per_meter);
    let dimension = (y > 0 && x > 0).then(|| {
        Node::new("Dimension")
            .child(
                Node::new("PixelAspectRatio")
                    .attribute("value", java_float_to_string((y / x) as f32)),
            )
            .child(
                Node::new("HorizontalPhysicalPixelSpacing")
                    .attribute("value", java_float_to_string(1000.0 / x as f32)),
            )
            .child(
                Node::new("VerticalPhysicalPixelSpacing")
                    .attribute("value", java_float_to_string(1000.0 / y as f32)),
            )
    });
    Ok(standard_tree([
        chroma,
        Some(compression),
        Some(data),
        dimension,
        None,
        None,
        None,
        None,
    ]))
}

/// `getStandardChromaNode`: the palette's entries, read three bytes at a time as red, green and
/// blue, with a fourth as alpha only when it is not zero. A palette of fewer bytes than three
/// for each entry, which a count that overflows reads, runs off its end.
fn chroma(metadata: &Metadata) -> Result<Node, BmpError> {
    let components = metadata.palette.len() as i32 / metadata.palette_size;
    if components < 3 {
        return Err(fails(format!(
            "ArrayIndexOutOfBoundsException: a palette of {} bytes for {} entries",
            metadata.palette.len(),
            metadata.palette_size
        )));
    }
    let mut palette = Node::new("Palette").attribute("value", components.to_string());
    let byte = |at: usize| (metadata.palette[at] as i8).to_string();
    let mut at = 0;
    for index in 0..metadata.palette_size {
        let mut entry = Node::new("PaletteEntry")
            .attribute("index", index.to_string())
            .attribute("red", byte(at))
            .attribute("green", byte(at + 1))
            .attribute("blue", byte(at + 2));
        at += 3;
        if components == 4 && metadata.palette[at] != 0 {
            entry = entry.attribute("alpha", byte(at));
            at += 1;
        }
        palette.push(entry);
    }
    Ok(Node::new("Chroma").child(palette))
}
