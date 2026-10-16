//! DjVu pages rendered in grey for OCR (DjVu v3 specification, section 4), as our Java
//! `DjvuRenderer` renders them: the JB2 mask's pixels in the foreground's colour, from the FG44
//! image or the FGbz palette, black without either, and every other pixel in the background's,
//! from the BG44 image or a BGjp JPEG, white without either; then turned as the INFO chunk's
//! orientation says.

use std::rc::Rc;

use image::GrayImage;

use super::document::{Chunk, DjvuDocument, Form};
use super::jb2::{self, Bitmap, Blit};
use super::{bzz, iw44, DjvuError, MAX_PIXELS};

const MAX_INCLUDES: usize = 16;
const MAX_REDUCTION: usize = 12;
/// The flag of an FGbz chunk's version byte that says colour indices follow its palette.
const FGBZ_INDICES: u8 = 0x80;
const MAX_PALETTE_BYTES: usize = 16 << 20;

fn malformed(message: impl Into<String>) -> DjvuError {
    DjvuError::Malformed(message.into())
}

/// A colour's grey, weighted as DjVuLibre's renderer weighs it.
fn grey(red: u8, green: u8, blue: u8) -> u8 {
    ((5 * u32::from(red) + 9 * u32::from(green) + 2 * u32::from(blue)) >> 4) as u8
}

/// The page a FORM:DJVU chunk codes, in grey.
pub(super) fn page(document: &DjvuDocument, page: &Form) -> Result<GrayImage, DjvuError> {
    let chunks = document.chunks(page)?;
    let mut info = None;
    let mut mask = None;
    let mut palette = None;
    let mut foreground_jpeg = None;
    let mut background_jpeg = None;
    let mut foreground = Vec::new();
    let mut background = Vec::new();
    for chunk in &chunks {
        match &chunk.id {
            b"INFO" => info = Some(*chunk),
            b"Sjbz" => mask = Some(*chunk),
            b"Smmr" => {
                return Err(DjvuError::Unsupported(
                    "DjVu masks coded with MMR (Smmr) are not supported".to_owned(),
                ))
            }
            b"FGbz" => palette = Some(*chunk),
            b"FG44" => foreground.push(document.data(chunk)?),
            b"BG44" => background.push(document.data(chunk)?),
            b"FGjp" => foreground_jpeg = Some(*chunk),
            b"BGjp" => background_jpeg = Some(*chunk),
            _ => {}
        }
    }
    let info = info
        .filter(|info| info.size >= 4)
        .ok_or_else(|| malformed("A DjVu page has no INFO chunk"))?;
    let info = document.data(&info)?;
    let width = usize::from(u16::from_be_bytes([info[0], info[1]]));
    let height = usize::from(u16::from_be_bytes([info[2], info[3]]));
    let orientation = info.get(9).map_or(1, |flags| flags & 0x07);
    if (width * height) as u64 > MAX_PIXELS {
        return Err(malformed(format!(
            "A DjVu page of {width}×{height} pixels exceeds {MAX_PIXELS}"
        )));
    }
    let back = layer(document, &background, background_jpeg)?;
    let mut pixels = vec![0u8; width * height];
    paint(&mut pixels, width, height, back.as_ref(), 0xff)?;
    if let Some(mask) = mask {
        let decoded = jb2::decode_image(
            document.data(&mask)?,
            dictionary(document, &chunks, 0)?.as_deref(),
        )?;
        if decoded.image.width != width || decoded.image.height != height {
            return Err(malformed(format!(
                "A DjVu page of {width}×{height} has a mask of {}×{}",
                decoded.image.width, decoded.image.height
            )));
        }
        let front = layer(document, &foreground, foreground_jpeg)?;
        match palette {
            Some(palette) => paint_blits(
                &mut pixels,
                width,
                height,
                &decoded.blits,
                document.data(&palette)?,
            )?,
            None => {
                let mut colour = vec![0u8; width * height];
                paint(&mut colour, width, height, front.as_ref(), 0)?;
                for ((pixel, &black), &colour) in
                    pixels.iter_mut().zip(&decoded.image.pixels).zip(&colour)
                {
                    if black != 0 {
                        *pixel = colour;
                    }
                }
            }
        }
    }
    let page =
        GrayImage::from_raw(width as u32, height as u32, pixels).expect("the pixels fill the page");
    Ok(oriented(page, orientation))
}

/// A layer's image: its IW44 chunks decoded, or else its JPEG, or none.
fn layer(
    document: &DjvuDocument,
    iw44_chunks: &[&[u8]],
    jpeg: Option<Chunk>,
) -> Result<Option<GrayImage>, DjvuError> {
    if !iw44_chunks.is_empty() {
        return iw44::decode(iw44_chunks).map(Some);
    }
    jpeg.map(|jpeg| decode_jpeg(document.data(&jpeg)?))
        .transpose()
}

/// The shapes of the shared dictionary a page's mask may require: its own Djbz chunk, or the
/// one a component file it includes holds, itself completed by the files that one includes.
fn dictionary(
    document: &DjvuDocument,
    chunks: &[Chunk],
    depth: usize,
) -> Result<Option<Vec<Rc<Bitmap>>>, DjvuError> {
    if depth > MAX_INCLUDES {
        return Err(malformed(format!(
            "DjVu component files include each other more than {MAX_INCLUDES} deep"
        )));
    }
    let mut included = None;
    let mut own = None;
    for chunk in chunks {
        match &chunk.id {
            b"Djbz" => own = Some(*chunk),
            b"INCL" if included.is_none() => {
                let id = crate::java::utf8(document.data(chunk)?);
                let component = document.component(crate::java::strip(&id))?;
                included = dictionary(document, &document.chunks(&component)?, depth + 1)?;
            }
            _ => {}
        }
    }
    match own {
        Some(own) => jb2::decode_dictionary(document.data(&own)?, included.as_deref()).map(Some),
        None => Ok(included),
    }
}

/// Fills a page-sized image from one of the same aspect at a whole fraction of its size.
fn paint(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    layer: Option<&GrayImage>,
    missing: u8,
) -> Result<(), DjvuError> {
    let Some(layer) = layer else {
        pixels.fill(missing);
        return Ok(());
    };
    let (layer_width, layer_height) = (layer.width() as usize, layer.height() as usize);
    let reduction = (width + layer_width - 1) / layer_width.max(1);
    if !(1..=MAX_REDUCTION).contains(&reduction) || height.div_ceil(reduction) != layer_height {
        return Err(malformed(format!(
            "A DjVu layer of {layer_width}×{layer_height} does not fit a page of {width}×{height}"
        )));
    }
    let source = layer.as_raw();
    for y in 0..height {
        let row = (y / reduction) * layer_width;
        for x in 0..width {
            pixels[y * width + x] = source[row + x / reduction];
        }
    }
    Ok(())
}

/// Paints each symbol the mask places in its colour from an FGbz chunk (8.3.10): a palette of
/// BGR colours, then, when the version's high bit says so, a BZZ-coded colour index for each
/// placed symbol in order; without indices every symbol takes the first colour.
fn paint_blits(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    blits: &[Blit],
    chunk: &[u8],
) -> Result<(), DjvuError> {
    if chunk.len() < 3 {
        return Err(malformed("A DjVu FGbz chunk ends inside its header"));
    }
    let colours = usize::from(u16::from_be_bytes([chunk[1], chunk[2]]));
    let index_start = 3 + 3 * colours;
    if colours == 0 || index_start > chunk.len() {
        return Err(malformed(format!(
            "A DjVu FGbz chunk ends inside its palette of {colours}"
        )));
    }
    let greys: Vec<u8> = chunk[3..index_start]
        .as_chunks::<3>()
        .0
        .iter()
        .map(|&[blue, green, red]| grey(red, green, blue))
        .collect();
    let indices = if chunk[0] & FGBZ_INDICES != 0 {
        if index_start + 3 > chunk.len() {
            return Err(malformed("A DjVu FGbz chunk ends inside its index count"));
        }
        let count = usize::from(chunk[index_start]) << 16
            | usize::from(chunk[index_start + 1]) << 8
            | usize::from(chunk[index_start + 2]);
        let coded = bzz::decode(&chunk[index_start + 3..], MAX_PALETTE_BYTES)?;
        if coded.len() < 2 * count {
            return Err(malformed(format!(
                "A DjVu FGbz chunk has fewer colour indices than {count}"
            )));
        }
        Some(
            coded[..2 * count]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&index| usize::from(u16::from_be_bytes(index)))
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    for (b, blit) in blits.iter().enumerate() {
        let index = match &indices {
            None => Some(0),
            Some(indices) => indices.get(b).copied(),
        };
        let level = index
            .and_then(|index| greys.get(index))
            .copied()
            .ok_or_else(|| malformed("A DjVu symbol has no colour in its FGbz palette"))?;
        let mark = &blit.mark;
        let (width, height) = (width as isize, height as isize);
        for y in (-blit.y).max(0)..mark.height as isize {
            if blit.y + y >= height {
                break;
            }
            for x in (-blit.x).max(0)..mark.width as isize {
                if blit.x + x >= width {
                    break;
                }
                if mark.pixels[y as usize * mark.width + x as usize] != 0 {
                    pixels[((blit.y + y) * width + blit.x + x) as usize] = level;
                }
            }
        }
    }
    Ok(())
}

/// A JPEG layer in grey, as Java's ImageIO decodes it to RGB.
fn decode_jpeg(data: &[u8]) -> Result<GrayImage, DjvuError> {
    let image = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)
        .map_err(|e| malformed(format!("A DjVu JPEG layer cannot be decoded: {e}")))?
        .to_rgb8();
    let (width, height) = image.dimensions();
    let pixels = image
        .pixels()
        .map(|pixel| grey(pixel.0[0], pixel.0[1], pixel.0[2]))
        .collect();
    Ok(GrayImage::from_raw(width, height, pixels).expect("a grey pixel for each pixel"))
}

/// The page turned as its INFO chunk's orientation says (8.3.11): 6 a quarter turn
/// counter-clockwise, 2 half a turn, 5 a quarter turn clockwise, and any other value upright.
fn oriented(page: GrayImage, orientation: u8) -> GrayImage {
    match orientation {
        2 => image::imageops::rotate180(&page),
        5 => image::imageops::rotate90(&page),
        6 => image::imageops::rotate270(&page),
        _ => page,
    }
}
