//! AVIF images decoded for OCR as libheif decodes them for our Java `HeifDecoder`: the primary
//! item of the HEIF container (ISO/IEC 23008-12), an AV1 image or a grid of them, its data in
//! the file, in the `idat` box or constructed from other items, decoded by rav1d, its colours
//! converted with its matrix coefficients, and cropped, rotated and mirrored as its
//! transformative properties say. A file whose primary image is coded otherwise is left to
//! libheif.

use std::collections::BTreeMap;

use image::{DynamicImage, GrayImage, ImageBuffer, Pixel, RgbImage};

use super::av1::{self, Chroma, Picture};
use super::bitmap::Bitmap;
use super::decode::decodable;

/// A box's or an item's type.
type Kind = [u8; 4];

/// A box: its type and its payload.
#[derive(Debug, Clone, Copy)]
struct Bmff<'a> {
    kind: Kind,
    payload: &'a [u8],
}

/// Big-endian reads that fail with what was being read at the end of the data.
#[derive(Debug, Clone)]
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn is_empty(&self) -> bool {
        self.at >= self.data.len()
    }

    fn bytes(&mut self, count: usize, what: &str) -> Result<&'a [u8], String> {
        let bytes = self
            .at
            .checked_add(count)
            .and_then(|end| self.data.get(self.at..end))
            .ok_or_else(|| format!("the AVIF ends inside {what}"))?;
        self.at += count;
        Ok(bytes)
    }

    fn uint(&mut self, size: usize, what: &str) -> Result<u64, String> {
        Ok(self
            .bytes(size, what)?
            .iter()
            .fold(0, |value, &byte| value << 8 | u64::from(byte)))
    }

    fn u8(&mut self, what: &str) -> Result<u8, String> {
        Ok(self.bytes(1, what)?[0])
    }

    fn u16(&mut self, what: &str) -> Result<u16, String> {
        Ok(self.uint(2, what)? as u16)
    }

    fn u32(&mut self, what: &str) -> Result<u32, String> {
        Ok(self.uint(4, what)? as u32)
    }

    /// A full box's version and flags.
    fn full(&mut self, what: &str) -> Result<(u8, u32), String> {
        let version = self.u8(what)?;
        let flags = self.uint(3, what)? as u32;
        Ok((version, flags))
    }

    /// The next box.
    fn next_box(&mut self) -> Result<Bmff<'a>, String> {
        let size = u64::from(self.u32("a box header")?);
        let kind: Kind = self
            .bytes(4, "a box header")?
            .try_into()
            .expect("four bytes");
        let header = match size {
            1 => {
                let size = self.uint(8, "a box header")?;
                size.checked_sub(16)
            }
            0 => Some((self.data.len() - self.at) as u64),
            _ => size.checked_sub(8),
        };
        let length = header
            .and_then(|length| usize::try_from(length).ok())
            .ok_or_else(|| format!("a {} box of {size} bytes", fourcc(kind)))?;
        let payload = self.bytes(length, "a box")?;
        Ok(Bmff { kind, payload })
    }

    /// The boxes up to the end.
    fn boxes(mut self) -> Result<Vec<Bmff<'a>>, String> {
        let mut boxes = Vec::new();
        while !self.is_empty() {
            boxes.push(self.next_box()?);
        }
        Ok(boxes)
    }
}

fn fourcc(kind: Kind) -> String {
    String::from_utf8_lossy(&kind).into_owned()
}

/// Where an item's data comes from, as `iloc` constructs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Construction {
    /// Extents of the file (method 0).
    File,
    /// Extents of the `idat` box (method 1).
    Idat,
    /// Extents of the data of the items its `iloc` references name (method 2).
    Items,
}

/// An extent of an item's data: which of the items it refers to it is of, counted from 1, for
/// the construction from items, its offset there, and its length, 0 for all that follows.
#[derive(Debug, Clone, Copy)]
struct Extent {
    index: u64,
    offset: u64,
    length: u64,
}

/// Where an item's data is.
#[derive(Debug, Clone)]
struct Location {
    construction: Construction,
    extents: Vec<Extent>,
}

/// A transformative property, in the order the item lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transform {
    /// `clap`: the clean aperture, as a width, a height and the offsets of its centre from the
    /// image's, each a fraction.
    Crop {
        width: (u32, u32),
        height: (u32, u32),
        horizontal: (i32, u32),
        vertical: (i32, u32),
    },
    /// `irot`: quarter turns anticlockwise.
    Rotate(u8),
    /// `imir`, whose axis libheif reads as Tika's path decodes with it: 0 swaps top and bottom,
    /// 1 left and right.
    Mirror { top_to_bottom: bool },
}

/// A colour description, of an `nclx` `colr` property or of an AV1 sequence header: the colour
/// primaries and the matrix coefficients as ISO/IEC 23091-2 numbers them, and whether samples
/// span the full range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Colour {
    primaries: u16,
    matrix: u16,
    full_range: bool,
}

/// The properties of an item the decoding reads.
#[derive(Debug, Clone, Default)]
struct Properties {
    colour: Option<Colour>,
    transforms: Vec<Transform>,
}

/// The meta box's items.
#[derive(Debug, Default)]
struct Items<'a> {
    primary: Option<u32>,
    types: BTreeMap<u32, Kind>,
    locations: BTreeMap<u32, Location>,
    /// Each item's `dimg` references, the images it is derived from, in order.
    derived_from: BTreeMap<u32, Vec<u32>>,
    /// Each item's `iloc` references, the items its data is constructed from, in order.
    constructed_from: BTreeMap<u32, Vec<u32>>,
    properties: BTreeMap<u32, Properties>,
    idat: &'a [u8],
}

/// An AVIF file's primary image, or that it is coded with codecs other than AV1.
pub(crate) enum Avif {
    Decoded(Bitmap),
    /// Its primary image, or a tile of it, is no AV1 image, such as one coded with HEVC or
    /// JPEG, which libheif decodes for Tika's path.
    OfOtherCodecs,
}

/// Decodes the primary image of an AVIF file.
pub(crate) fn decode(input: &[u8]) -> Result<Avif, String> {
    let meta = Reader::new(input)
        .boxes()?
        .into_iter()
        .find(|found| &found.kind == b"meta")
        .ok_or("the AVIF has no meta box")?;
    let items = items(meta.payload)?;
    let primary = items.primary.ok_or("the AVIF names no primary item")?;
    let properties = items.properties.get(&primary).cloned().unwrap_or_default();
    let picture = match items.types.get(&primary) {
        Some(b"av01") => picture(
            &av1::decode(&item_data(input, &items, primary)?)?,
            properties.colour,
        )?,
        Some(b"grid") => match grid(input, &items, primary, properties.colour)? {
            Some(canvas) => canvas,
            None => return Ok(Avif::OfOtherCodecs),
        },
        Some(_) => return Ok(Avif::OfOtherCodecs),
        None => return Err(format!("no item {primary}, which pitm names")),
    };
    properties
        .transforms
        .iter()
        .try_fold(picture, |image, transform| transformed(image, *transform))
        .map(Avif::Decoded)
}

/// Reads the boxes of the meta box the decoding needs.
fn items(meta: &[u8]) -> Result<Items<'_>, String> {
    let mut reader = Reader::new(meta);
    reader.full("the meta box")?;
    let mut items = Items::default();
    let mut ipco = Vec::new();
    let mut ipma = None;
    for found in Reader::new(&meta[reader.at..]).boxes()? {
        let mut body = Reader::new(found.payload);
        match &found.kind {
            b"pitm" => {
                let (version, _) = body.full("pitm")?;
                items.primary = Some(match version {
                    0 => u32::from(body.u16("pitm")?),
                    _ => body.u32("pitm")?,
                });
            }
            b"iinf" => {
                let (version, _) = body.full("iinf")?;
                match version {
                    0 => body.u16("iinf")?.into(),
                    _ => body.u32("iinf")?,
                };
                for entry in Reader::new(&found.payload[body.at..]).boxes()? {
                    if &entry.kind != b"infe" {
                        continue;
                    }
                    let mut infe = Reader::new(entry.payload);
                    let (version, _) = infe.full("infe")?;
                    let id = match version {
                        2 => u32::from(infe.u16("infe")?),
                        3 => infe.u32("infe")?,
                        _ => continue,
                    };
                    infe.u16("infe")?;
                    let kind: Kind = infe.bytes(4, "infe")?.try_into().expect("four bytes");
                    items.types.insert(id, kind);
                }
            }
            b"iloc" => items.locations = locations(&mut body)?,
            b"iref" => {
                let (version, _) = body.full("iref")?;
                for reference in Reader::new(&found.payload[body.at..]).boxes()? {
                    let mut ids = Reader::new(reference.payload);
                    let id = |ids: &mut Reader| match version {
                        0 => ids.u16("iref").map(u32::from),
                        _ => ids.u32("iref"),
                    };
                    let from = id(&mut ids)?;
                    let count = ids.u16("iref")?;
                    let to = (0..count)
                        .map(|_| id(&mut ids))
                        .collect::<Result<Vec<_>, _>>()?;
                    match &reference.kind {
                        b"dimg" => {
                            items.derived_from.insert(from, to);
                        }
                        b"iloc" => {
                            items.constructed_from.insert(from, to);
                        }
                        _ => {}
                    }
                }
            }
            b"iprp" => {
                for property in Reader::new(found.payload).boxes()? {
                    match &property.kind {
                        b"ipco" => ipco = Reader::new(property.payload).boxes()?,
                        b"ipma" => ipma = Some(property.payload),
                        _ => {}
                    }
                }
            }
            b"idat" => items.idat = found.payload,
            _ => {}
        }
    }
    if let Some(ipma) = ipma {
        items.properties = associations(ipma, &ipco)?;
    }
    Ok(items)
}

/// `iloc`: each item's construction and extents.
fn locations(body: &mut Reader) -> Result<BTreeMap<u32, Location>, String> {
    let (version, _) = body.full("iloc")?;
    let sizes = body.u16("iloc")?;
    let offset_size = usize::from(sizes >> 12);
    let length_size = usize::from(sizes >> 8 & 0xF);
    let base_size = usize::from(sizes >> 4 & 0xF);
    let index_size = if version == 0 {
        0
    } else {
        usize::from(sizes & 0xF)
    };
    let count = match version {
        0 | 1 => u32::from(body.u16("iloc")?),
        _ => body.u32("iloc")?,
    };
    let mut locations = BTreeMap::new();
    for _ in 0..count {
        let id = match version {
            0 | 1 => u32::from(body.u16("iloc")?),
            _ => body.u32("iloc")?,
        };
        let method = match version {
            0 => 0,
            _ => body.u16("iloc")? & 0xF,
        };
        let construction = match method {
            0 => Construction::File,
            1 => Construction::Idat,
            2 => Construction::Items,
            other => return Err(format!("item {id} is built by construction method {other}")),
        };
        body.u16("iloc")?;
        let base = body.uint(base_size, "iloc")?;
        let extent_count = body.u16("iloc")?;
        let mut extents = Vec::with_capacity(usize::from(extent_count));
        for _ in 0..extent_count {
            let index = match index_size {
                0 => 1,
                _ => body.uint(index_size, "iloc")?,
            };
            let offset = body.uint(offset_size, "iloc")?;
            let length = body.uint(length_size, "iloc")?;
            extents.push(Extent {
                index,
                offset: base.saturating_add(offset),
                length,
            });
        }
        locations.insert(
            id,
            Location {
                construction,
                extents,
            },
        );
    }
    Ok(locations)
}

/// `ipma`: the properties of each item, of those the decoding reads.
fn associations(ipma: &[u8], ipco: &[Bmff]) -> Result<BTreeMap<u32, Properties>, String> {
    let mut body = Reader::new(ipma);
    let (version, flags) = body.full("ipma")?;
    let count = body.u32("ipma")?;
    let mut properties = BTreeMap::new();
    for _ in 0..count {
        let id = match version {
            0 => u32::from(body.u16("ipma")?),
            _ => body.u32("ipma")?,
        };
        let associated = body.u8("ipma")?;
        let mut item = Properties::default();
        for _ in 0..associated {
            let index = match flags & 1 {
                1 => usize::from(body.u16("ipma")? & 0x7FFF),
                _ => usize::from(body.u8("ipma")? & 0x7F),
            };
            let Some(property) = index.checked_sub(1).and_then(|index| ipco.get(index)) else {
                continue;
            };
            property_of(property, &mut item)?;
        }
        properties.insert(id, item);
    }
    Ok(properties)
}

/// Keeps a property the decoding reads.
fn property_of(property: &Bmff, item: &mut Properties) -> Result<(), String> {
    let mut body = Reader::new(property.payload);
    match &property.kind {
        b"colr" => {
            if body.bytes(4, "colr")? == b"nclx" {
                let primaries = body.u16("colr")?;
                body.u16("colr")?;
                let matrix = body.u16("colr")?;
                let full_range = body.u8("colr")? & 0x80 != 0;
                item.colour = Some(Colour {
                    primaries,
                    matrix,
                    full_range,
                });
            }
        }
        b"irot" => item
            .transforms
            .push(Transform::Rotate(body.u8("irot")? & 3)),
        b"imir" => item.transforms.push(Transform::Mirror {
            top_to_bottom: body.u8("imir")? & 1 == 0,
        }),
        b"clap" => {
            let mut fraction =
                || -> Result<(u32, u32), String> { Ok((body.u32("clap")?, body.u32("clap")?)) };
            let width = fraction()?;
            let height = fraction()?;
            let (horizontal, horizontal_d) = fraction()?;
            let (vertical, vertical_d) = fraction()?;
            item.transforms.push(Transform::Crop {
                width,
                height,
                horizontal: (horizontal as i32, horizontal_d),
                vertical: (vertical as i32, vertical_d),
            });
        }
        _ => {}
    }
    Ok(())
}

/// An item's data, its extents joined: of the file, of the `idat` box, or of the items it is
/// constructed from.
fn item_data(input: &[u8], items: &Items, id: u32) -> Result<Vec<u8>, String> {
    constructed(input, items, id, &mut Vec::new())
}

/// An item's data, constructed while the items in `constructing` are, which it cannot be
/// constructed from.
fn constructed(
    input: &[u8],
    items: &Items,
    id: u32,
    constructing: &mut Vec<u32>,
) -> Result<Vec<u8>, String> {
    if constructing.contains(&id) {
        return Err(format!("item {id} is constructed from itself"));
    }
    let location = items
        .locations
        .get(&id)
        .ok_or_else(|| format!("item {id} has no location"))?;
    constructing.push(id);
    let mut sources: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    if location.construction == Construction::Items {
        for extent in &location.extents {
            if let std::collections::btree_map::Entry::Vacant(source) = sources.entry(extent.index)
            {
                let referenced = usize::try_from(extent.index)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| items.constructed_from.get(&id)?.get(index).copied())
                    .ok_or_else(|| {
                        format!(
                            "item {id} is constructed from its item reference {}, which it lacks",
                            extent.index
                        )
                    })?;
                source.insert(constructed(input, items, referenced, constructing)?);
            }
        }
    }
    let mut data = Vec::new();
    for extent in &location.extents {
        let source: &[u8] = match location.construction {
            Construction::File => input,
            Construction::Idat => items.idat,
            Construction::Items => &sources[&extent.index],
        };
        let extent_bytes = usize::try_from(extent.offset)
            .ok()
            .and_then(|offset| match extent.length {
                0 => source.get(offset..),
                length => source.get(offset..offset.checked_add(usize::try_from(length).ok()?)?),
            })
            .ok_or_else(|| format!("item {id} lies past the end of its data"))?;
        data.extend_from_slice(extent_bytes);
    }
    constructing.pop();
    Ok(data)
}

/// A grid item (ISO/IEC 23008-12, 6.6.2.3): its tiles, the images it is derived from, laid
/// out in rows on a canvas of its output size, each in the colours of its own properties; or
/// none when a tile is no AV1 image.
fn grid(
    input: &[u8],
    items: &Items,
    id: u32,
    colour: Option<Colour>,
) -> Result<Option<Bitmap>, String> {
    let data = item_data(input, items, id)?;
    let mut body = Reader::new(&data);
    body.u8("the grid")?;
    let flags = body.u8("the grid")?;
    let rows = u32::from(body.u8("the grid")?) + 1;
    let columns = u32::from(body.u8("the grid")?) + 1;
    let size = if flags & 1 == 1 { 4 } else { 2 };
    let width = body.uint(size, "the grid")? as u32;
    let height = body.uint(size, "the grid")? as u32;
    decodable(width, height)?;
    let tiles = items
        .derived_from
        .get(&id)
        .ok_or_else(|| format!("the grid {id} has no tiles"))?;
    if tiles.len() != (rows * columns) as usize {
        return Err(format!(
            "a grid of {rows}x{columns} with {} tiles",
            tiles.len()
        ));
    }
    if tiles
        .iter()
        .any(|tile| items.types.get(tile) != Some(b"av01"))
    {
        return Ok(None);
    }
    let decoded = tiles
        .iter()
        .map(|&tile| {
            let tile_colour = items
                .properties
                .get(&tile)
                .and_then(|properties| properties.colour)
                .or(colour);
            picture(&av1::decode(&item_data(input, items, tile)?)?, tile_colour)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let tile_size = decoded[0].dimensions();
    let place = |index: usize| {
        (
            index as u32 % columns * tile_size.0,
            index as u32 / columns * tile_size.1,
        )
    };
    if decoded.iter().all(|tile| matches!(tile, Bitmap::Gray(_))) {
        let mut canvas = GrayImage::new(width, height);
        for (index, tile) in decoded.iter().enumerate() {
            paste(&mut canvas, &tile.gray(), place(index));
        }
        return Ok(Some(Bitmap::Gray(canvas)));
    }
    let mut canvas = RgbImage::new(width, height);
    for (index, tile) in decoded.into_iter().enumerate() {
        let tile = match tile {
            Bitmap::Gray(gray) => DynamicImage::ImageLuma8(gray).to_rgb8(),
            Bitmap::Rgb(rgb) => rgb,
            Bitmap::Rgba(rgba) => DynamicImage::ImageRgba8(rgba).to_rgb8(),
        };
        paste(&mut canvas, &tile, place(index));
    }
    Ok(Some(Bitmap::Rgb(canvas)))
}

/// Lays a tile on a canvas at `(x, y)`, cut at the canvas's edges.
fn paste<P: Pixel>(
    canvas: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    tile: &ImageBuffer<P, Vec<P::Subpixel>>,
    (x, y): (u32, u32),
) {
    for (tile_x, tile_y, pixel) in tile.enumerate_pixels() {
        if x + tile_x < canvas.width() && y + tile_y < canvas.height() {
            canvas.put_pixel(x + tile_x, y + tile_y, *pixel);
        }
    }
}

/// The colour matrix samples are coded with, as libheif converts it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Matrix {
    /// Green, blue and red in the three planes.
    Identity,
    /// Luma and colour differences, with the weights of red and blue in luma.
    Weighted {
        red: f64,
        blue: f64,
    },
    YCgCo,
    /// A matrix with no linear conversion to red, green and blue, SMPTE ST 2085's Y'D'zD'x
    /// (11) and ICtCp (14), whose first plane holds the brightness, read alone in grey. libheif
    /// refuses them.
    Brightness,
}

/// The weights of red and blue in luma of BT.601, which libheif converts with when the matrix
/// coefficients name no weights of their own.
const BT_601: (f64, f64) = (0.299, 0.114);

impl Matrix {
    /// The matrix of the matrix coefficients `code`, its weights derived from the colour
    /// primaries `primaries` for codes 12 and 13, as libheif's `get_Kr_Kb` takes them.
    fn of(code: u16, primaries: u16) -> Result<Self, String> {
        let weighted = |(red, blue)| Self::Weighted { red, blue };
        Ok(match code {
            0 => Self::Identity,
            1 => weighted((0.2126, 0.0722)),
            4 => weighted((0.30, 0.11)),
            7 => weighted((0.212, 0.087)),
            8 => Self::YCgCo,
            9 | 10 => weighted((0.2627, 0.0593)),
            11 | 14 => Self::Brightness,
            12 | 13 => weighted(chromaticity_weights(primaries).unwrap_or(BT_601)),
            16 | 17 => {
                return Err(format!(
                    "the matrix coefficients {code} (YCgCo-R), which AV1 does not code"
                ))
            }
            _ => weighted(BT_601),
        })
    }
}

/// The chromaticities of the red, green, blue and white of colour primaries, as x and y, as
/// libheif's `get_colour_primaries` lists them, or none for primaries it does not know.
fn chromaticities(primaries: u16) -> Option<[(f64, f64); 4]> {
    const D65: (f64, f64) = (0.3127, 0.3290);
    Some(match primaries {
        1 => [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060), D65],
        4 => [(0.67, 0.33), (0.21, 0.71), (0.14, 0.08), (0.310, 0.316)],
        5 => [(0.64, 0.33), (0.29, 0.60), (0.15, 0.06), D65],
        6 | 7 => [(0.630, 0.340), (0.310, 0.595), (0.155, 0.070), D65],
        8 => [
            (0.681, 0.319),
            (0.243, 0.692),
            (0.145, 0.049),
            (0.310, 0.316),
        ],
        9 => [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046), D65],
        10 => [(1.0, 0.0), (0.0, 1.0), (0.0, 0.0), (0.333333, 0.33333)],
        11 => [
            (0.680, 0.320),
            (0.265, 0.690),
            (0.150, 0.060),
            (0.314, 0.351),
        ],
        12 => [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060), D65],
        22 => [(0.630, 0.340), (0.295, 0.605), (0.155, 0.077), D65],
        _ => return None,
    })
}

/// The weights of red and blue in luma derived from colour primaries (ITU-T H.273, equations
/// 39 and 40), or none for primaries without them.
fn chromaticity_weights(primaries: u16) -> Option<(f64, f64)> {
    let [(xr, yr), (xg, yg), (xb, yb), (xw, yw)] = chromaticities(primaries)?;
    let (zr, zg, zb, zw) = (
        1.0 - (xr + yr),
        1.0 - (xg + yg),
        1.0 - (xb + yb),
        1.0 - (xw + yw),
    );
    let denominator =
        yw * (xr * (yg * zb - yb * zg) + xg * (yb * zr - yr * zb) + xb * (yr * zg - yg * zr));
    if denominator == 0.0 {
        return None;
    }
    let red = yr * (xw * (yg * zb - yb * zg) + yw * (xb * zg - xg * zb) + zw * (xg * yb - xb * yg))
        / denominator;
    let blue = yb
        * (xw * (yr * zg - yg * zr) + yw * (xg * zr - xr * zg) + zw * (xr * yg - xg * yr))
        / denominator;
    Some((red, blue))
}

/// How samples of a bit depth map to the unit range: luma to 0 to 1, colour differences to
/// -0.5 to 0.5.
#[derive(Debug, Clone, Copy)]
struct Range {
    full: bool,
    /// The largest sample.
    max: f64,
    /// A sample's size against an 8-bit one.
    scale: f64,
}

impl Range {
    fn new(depth: u8, full: bool) -> Self {
        Self {
            full,
            max: f64::from((1u32 << depth) - 1),
            scale: f64::from(1u32 << (depth - 8)),
        }
    }

    fn luma(self, sample: u16) -> f64 {
        match self.full {
            true => f64::from(sample) / self.max,
            false => (f64::from(sample) - 16.0 * self.scale) / (219.0 * self.scale),
        }
    }

    fn chroma(self, sample: u16) -> f64 {
        match self.full {
            true => (f64::from(sample) - (self.max + 1.0) / 2.0) / self.max,
            false => (f64::from(sample) - 128.0 * self.scale) / (224.0 * self.scale),
        }
    }
}

/// A unit value as an 8-bit level.
fn level(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A picture's samples converted to red, green and blue with the colour description of the
/// item's property, or else of the picture's sequence header: in grey for a picture of luma
/// alone or a matrix of brightness, and else in colour.
fn picture(picture: &Picture, colour: Option<Colour>) -> Result<Bitmap, String> {
    let colour = colour.unwrap_or(Colour {
        primaries: picture.primaries,
        matrix: picture.matrix,
        full_range: picture.full_range,
    });
    let matrix = Matrix::of(colour.matrix, colour.primaries)?;
    let range = Range::new(picture.depth, colour.full_range);
    let luma_plane = &picture.planes[0];
    let (width, height) = (luma_plane.width, luma_plane.height);
    let short = || "the picture holds fewer samples than its size".to_owned();
    let shifts = match picture.chroma {
        Chroma::Subsampled { x, y } if matrix != Matrix::Brightness => (x, y),
        _ => {
            let levels = luma_plane
                .samples
                .iter()
                .map(|&sample| level(range.luma(sample)))
                .collect();
            return GrayImage::from_raw(width as u32, height as u32, levels)
                .map(Bitmap::Gray)
                .ok_or_else(short);
        }
    };
    let mut samples = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let brightness = range.luma(luma_plane.at(x, y));
            let (chroma_x, chroma_y) = (x >> shifts.0, y >> shifts.1);
            let blue = picture.planes[1].at(chroma_x, chroma_y);
            let red = picture.planes[2].at(chroma_x, chroma_y);
            let (r, g, b) = match matrix {
                Matrix::Identity => (range.luma(red), brightness, range.luma(blue)),
                Matrix::YCgCo => {
                    let green = range.chroma(blue);
                    let orange = range.chroma(red);
                    let base = brightness - green;
                    (base + orange, brightness + green, base - orange)
                }
                Matrix::Weighted {
                    red: red_weight,
                    blue: blue_weight,
                } => {
                    let r = brightness + 2.0 * (1.0 - red_weight) * range.chroma(red);
                    let b = brightness + 2.0 * (1.0 - blue_weight) * range.chroma(blue);
                    let g = (brightness - red_weight * r - blue_weight * b)
                        / (1.0 - red_weight - blue_weight);
                    (r, g, b)
                }
                Matrix::Brightness => unreachable!("brightness is read in grey"),
            };
            samples.extend([level(r), level(g), level(b)]);
        }
    }
    RgbImage::from_raw(width as u32, height as u32, samples)
        .map(Bitmap::Rgb)
        .ok_or_else(short)
}

/// An image after a transformative property.
fn transformed(image: Bitmap, transform: Transform) -> Result<Bitmap, String> {
    Ok(match image {
        Bitmap::Gray(gray) => Bitmap::Gray(transformed_buffer(gray, transform)?),
        Bitmap::Rgb(rgb) => Bitmap::Rgb(transformed_buffer(rgb, transform)?),
        Bitmap::Rgba(rgba) => Bitmap::Rgba(transformed_buffer(rgba, transform)?),
    })
}

fn transformed_buffer<P: Pixel + 'static>(
    image: ImageBuffer<P, Vec<P::Subpixel>>,
    transform: Transform,
) -> Result<ImageBuffer<P, Vec<P::Subpixel>>, String> {
    Ok(match transform {
        Transform::Rotate(0) => image,
        Transform::Rotate(1) => image::imageops::rotate270(&image),
        Transform::Rotate(2) => image::imageops::rotate180(&image),
        Transform::Rotate(_) => image::imageops::rotate90(&image),
        Transform::Mirror {
            top_to_bottom: true,
        } => image::imageops::flip_vertical(&image),
        Transform::Mirror {
            top_to_bottom: false,
        } => image::imageops::flip_horizontal(&image),
        Transform::Crop {
            width,
            height,
            horizontal,
            vertical,
        } => crop(image, width, height, horizontal, vertical)?,
    })
}

/// The clean aperture of an image, as libheif rounds its edges: the centre's offset added to
/// the image's centre, half the aperture each side, the left and top rounded down.
fn crop<P: Pixel + 'static>(
    image: ImageBuffer<P, Vec<P::Subpixel>>,
    width: (u32, u32),
    height: (u32, u32),
    horizontal: (i32, u32),
    vertical: (i32, u32),
) -> Result<ImageBuffer<P, Vec<P::Subpixel>>, String> {
    let fraction = |(numerator, denominator): (f64, u32)| -> Result<f64, String> {
        match denominator {
            0 => Err("a clean aperture with a zero denominator".to_owned()),
            _ => Ok(numerator / f64::from(denominator)),
        }
    };
    let aperture_width = fraction((f64::from(width.0), width.1))?;
    let aperture_height = fraction((f64::from(height.0), height.1))?;
    let centre_x =
        fraction((f64::from(horizontal.0), horizontal.1))? + f64::from(image.width() - 1) / 2.0;
    let centre_y =
        fraction((f64::from(vertical.0), vertical.1))? + f64::from(image.height() - 1) / 2.0;
    let left = (centre_x - (aperture_width - 1.0) / 2.0).round();
    let top = (centre_y - (aperture_height - 1.0) / 2.0).round();
    let right = (left + aperture_width - 1.0).round();
    let bottom = (top + aperture_height - 1.0).round();
    if left < 0.0
        || top < 0.0
        || right >= f64::from(image.width())
        || bottom >= f64::from(image.height())
        || right < left
        || bottom < top
    {
        return Err(format!(
            "a clean aperture from {left},{top} to {right},{bottom} of an image of {}x{}",
            image.width(),
            image.height()
        ));
    }
    Ok(image::imageops::crop_imm(
        &image,
        left as u32,
        top as u32,
        (right - left) as u32 + 1,
        (bottom - top) as u32 + 1,
    )
    .to_image())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded(input: &[u8]) -> Result<Bitmap, String> {
        match decode(input)? {
            Avif::Decoded(bitmap) => Ok(bitmap),
            Avif::OfOtherCodecs => Err("of other codecs".to_owned()),
        }
    }

    /// The test cards of `tests/fixtures/avif`, coded with each layout, range, depth, matrix and
    /// transformative property the decoder reads, and the corpus's scan decode as libheif
    /// decodes them for Tika's path, `heif-dec` in the development shell, but for the rounding
    /// of a colour's grey; colour cards in colour.
    #[test]
    fn decodes_as_libheif_does() {
        let fixtures = std::fs::read_dir("tests/fixtures/avif")
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "avif")
            });
        let mut paths: Vec<_> = fixtures.collect();
        paths.sort();
        paths.push("../quality/corpus/prose-scan.avif".into());
        for path in paths {
            let input = std::fs::read(&path).unwrap();
            let ours = decoded(&input).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let theirs = super::super::heif::libheif(&input).unwrap();
            let (ours, theirs) = (ours.gray(), theirs.gray());
            assert_eq!(ours.dimensions(), theirs.dimensions(), "{}", path.display());
            let largest = ours
                .pixels()
                .zip(theirs.pixels())
                .map(|(a, b)| a.0[0].abs_diff(b.0[0]))
                .max()
                .unwrap_or_default();
            assert!(largest <= 1, "{}: {largest} levels off", path.display());
        }
        let card = decoded(&std::fs::read("tests/fixtures/avif/limited-709-420.avif").unwrap());
        assert!(matches!(card, Ok(Bitmap::Rgb(_))));
    }

    /// Weights derived from the primaries match those the standards name: BT.709's and
    /// BT.2020's.
    #[test]
    fn derives_luma_weights_from_primaries() {
        for (primaries, (red, blue)) in [(1, (0.2126, 0.0722)), (9, (0.2627, 0.0593))] {
            let (derived_red, derived_blue) = chromaticity_weights(primaries).unwrap();
            assert!(
                (derived_red - red).abs() < 0.0005,
                "{primaries}: {derived_red}"
            );
            assert!(
                (derived_blue - blue).abs() < 0.0005,
                "{primaries}: {derived_blue}"
            );
        }
        assert_eq!(
            Matrix::of(12, 2).unwrap(),
            Matrix::Weighted {
                red: 0.299,
                blue: 0.114
            }
        );
        assert_eq!(Matrix::of(14, 9).unwrap(), Matrix::Brightness);
        assert!(Matrix::of(16, 1).is_err());
    }

    /// An item of construction method 2 is the extents of the items its `iloc` references name.
    #[test]
    fn constructs_items_from_items() {
        let input = b"..abcdef..ghij";
        let extent = |index, offset, length| Extent {
            index,
            offset,
            length,
        };
        let items = Items {
            locations: BTreeMap::from([
                (
                    1,
                    Location {
                        construction: Construction::File,
                        extents: vec![extent(1, 2, 6)],
                    },
                ),
                (
                    2,
                    Location {
                        construction: Construction::File,
                        extents: vec![extent(1, 10, 0)],
                    },
                ),
                (
                    3,
                    Location {
                        construction: Construction::Items,
                        extents: vec![extent(2, 1, 2), extent(1, 4, 0)],
                    },
                ),
                (
                    4,
                    Location {
                        construction: Construction::Items,
                        extents: vec![extent(1, 0, 0)],
                    },
                ),
            ]),
            constructed_from: BTreeMap::from([(3, vec![1, 2]), (4, vec![4])]),
            ..Items::default()
        };
        assert_eq!(item_data(input, &items, 3).unwrap(), b"hief");
        assert!(item_data(input, &items, 4)
            .unwrap_err()
            .contains("from itself"));
    }

    /// A file whose primary image is coded with HEVC is left to libheif.
    #[test]
    fn leaves_other_codecs_to_libheif() {
        let heic = std::fs::read("../quality/corpus/prose-scan.heic").unwrap();
        assert!(matches!(decode(&heic), Ok(Avif::OfOtherCodecs)));
    }
}
