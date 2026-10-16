//! The JDK's `PNGImageReader` as far as its header and metadata, and `PNGMetadata`'s trees:
//! the chunks before `IEND` read into the native tree (`javax_imageio_png_1.0`) and the standard
//! one.

use std::io::Read as _;

use flate2::read::ZlibDecoder;

use super::{creation_time, standard_tree, Eof, Node, Read, Stream};
use crate::formats::Unported;
use crate::java::dtoa::java_float_to_string;
use crate::text::charset::Charset;

const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
const GRAY: u8 = 0;
const RGB: u8 = 2;
const PALETTE: u8 = 3;
const GRAY_ALPHA: u8 = 4;
const RGB_ALPHA: u8 = 6;
/// `PNGImageReader.MAX_INFLATED_TEXT_LENGTH`.
const MAX_INFLATED_TEXT_LENGTH: u64 = 262_144;
const CREATION_TIME_KEY: &str = "Creation Time";

const COLOR_TYPE_NAMES: [Option<&str>; 7] = [
    Some("Grayscale"),
    None,
    Some("RGB"),
    Some("Palette"),
    Some("GrayAlpha"),
    None,
    Some("RGBAlpha"),
];
const NUM_CHANNELS: [usize; 7] = [1, 0, 3, 3, 2, 0, 4];
const COLOR_SPACE_TYPE_NAMES: [Option<&str>; 7] = [
    Some("GRAY"),
    None,
    Some("RGB"),
    Some("RGB"),
    Some("GRAY"),
    None,
    Some("RGB"),
];
const UNIT_SPECIFIER_NAMES: [&str; 2] = ["unknown", "meter"];
const RENDERING_INTENT_NAMES: [&str; 4] = [
    "Perceptual",
    "Relative colorimetric",
    "Saturation",
    "Absolute colorimetric",
];

/// Why the PNG could not be read: `IIOException`, or something its trees throw on.
#[derive(Debug)]
pub(crate) enum PngError {
    /// An `IIOException`, which Tika reports as a parse error of the type.
    Image(String),
    /// A runtime exception ImageIO throws, which fails the parse too.
    Runtime(String),
    Unported(Unported),
}

impl From<Eof> for PngError {
    fn from(_: Eof) -> Self {
        Self::Image("Error reading PNG metadata".to_owned())
    }
}

#[derive(Debug, Default, Clone)]
struct Metadata {
    width: i32,
    height: i32,
    bit_depth: u8,
    color_type: u8,
    interlace: u8,
    plte: Option<(Vec<u8>, Vec<u8>, Vec<u8>)>,
    bkgd: Option<Background>,
    chrm: Option<[i32; 8]>,
    gama: Option<i32>,
    hist: Option<Vec<u16>>,
    iccp: Option<(String, u8)>,
    itxt: Vec<InternationalText>,
    phys: Option<(i32, i32, u8)>,
    sbit: Option<SignificantBits>,
    splt: Option<SuggestedPalette>,
    srgb: Option<u8>,
    text: Vec<(String, String)>,
    time: Option<[i32; 6]>,
    creation_time: Option<creation_time::Fields>,
    trns: Option<Transparency>,
    ztxt: Vec<(String, u8, String)>,
    unknown: Vec<String>,
}

#[derive(Debug, Clone)]
enum Background {
    Palette(u8),
    Gray(u16),
    Rgb(u16, u16, u16),
}

#[derive(Debug, Clone)]
struct InternationalText {
    keyword: String,
    compressed: bool,
    method: u8,
    language: String,
    translated: String,
    text: String,
}

#[derive(Debug, Clone)]
struct SignificantBits {
    color_type: u8,
    gray: u8,
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
}

#[derive(Debug, Clone)]
struct SuggestedPalette {
    name: String,
    depth: u8,
    entries: Vec<[u16; 5]>,
}

#[derive(Debug, Clone)]
enum Transparency {
    Palette(Vec<u8>),
    Gray(u16),
    Rgb(u16, u16, u16),
}

fn image(message: &str) -> PngError {
    PngError::Image(message.to_owned())
}

/// `readNullTerminatedString`: at most `max` bytes before a zero, which must end them.
fn null_terminated(stream: &mut Stream, charset: &str, max: i64) -> Result<String, PngError> {
    let mut bytes = Vec::new();
    let mut count = 0;
    let mut last = 0;
    while max > count {
        count += 1;
        let byte = stream.read().ok_or(Eof)?;
        last = byte;
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    if last != 0 {
        return Err(image("Found non null terminated string"));
    }
    Ok(decode(&bytes, charset))
}

fn decode(bytes: &[u8], charset: &str) -> String {
    Charset::named(charset)
        .decode(bytes)
        .expect("the JDK port decodes ISO-8859-1 and UTF-8")
}

/// `InflaterInputStream.readNBytes(MAX_INFLATED_TEXT_LENGTH)`.
fn inflate(bytes: &[u8]) -> Result<Vec<u8>, PngError> {
    let mut inflated = Vec::new();
    ZlibDecoder::new(bytes)
        .take(MAX_INFLATED_TEXT_LENGTH)
        .read_to_end(&mut inflated)
        .map_err(|e| PngError::Image(format!("Error reading PNG metadata: {e}")))?;
    Ok(inflated)
}

/// `readHeader`, `getNumImages`, `getWidth`, `getHeight` and `getImageMetadata` as Tika's
/// `ImageParser` calls them.
pub(crate) fn read(input: &[u8]) -> Result<Read, PngError> {
    let mut stream = Stream::new(input);
    let mut metadata = header(&mut stream)
        .map_err(|_| image("I/O error reading PNG header!"))
        .and_then(|header| header)?;
    chunks(&mut stream, &mut metadata)?;
    Ok(Read {
        images: 1,
        width: metadata.width,
        height: metadata.height,
        trees: vec![native_tree(&metadata)?, standard(&metadata)?],
    })
}

fn header(stream: &mut Stream) -> Result<Result<Metadata, PngError>, Eof> {
    if stream.bytes(8)? != SIGNATURE {
        return Ok(Err(image("Bad PNG signature!")));
    }
    if stream.i32_be()? != 13 {
        return Ok(Err(image("Bad length for IHDR chunk!")));
    }
    if stream.bytes(4)? != b"IHDR" {
        return Ok(Err(image("Bad type for IHDR chunk!")));
    }
    let width = stream.i32_be()?;
    let height = stream.i32_be()?;
    let fields = stream.bytes(5)?;
    let (bit_depth, color_type, compression, filter, interlace) =
        (fields[0], fields[1], fields[2], fields[3], fields[4]);
    stream.skip(4);
    let failure = if width <= 0 {
        Some("Image width <= 0!")
    } else if height <= 0 {
        Some("Image height <= 0!")
    } else if ![1, 2, 4, 8, 16].contains(&bit_depth) {
        Some("Bit depth must be 1, 2, 4, 8, or 16!")
    } else if ![0, 2, 3, 4, 6].contains(&color_type) {
        Some("Color type must be 0, 2, 3, 4, or 6!")
    } else if (color_type == PALETTE && bit_depth == 16)
        || ([RGB, RGB_ALPHA, GRAY_ALPHA].contains(&color_type) && bit_depth != 8 && bit_depth != 16)
    {
        Some("Bad color type/bit depth combination!")
    } else if compression != 0 {
        Some("Unknown compression method (not 0)!")
    } else if filter != 0 {
        Some("Unknown filter method (not 0)!")
    } else if interlace > 1 {
        Some("Unknown interlace method (not 0 or 1)!")
    } else {
        None
    };
    Ok(match failure {
        Some(message) => Err(image(message)),
        None => Ok(Metadata {
            width,
            height,
            bit_depth,
            color_type,
            interlace,
            ..Metadata::default()
        }),
    })
}

/// `readMetadata`: every chunk up to `IEND`, each read whole as its length says.
fn chunks(stream: &mut Stream, metadata: &mut Metadata) -> Result<(), PngError> {
    let mut seen_idat = false;
    loop {
        let length = stream.i32_be()?;
        let kind = stream.bytes(4)?;
        let kind: [u8; 4] = kind.try_into().expect("four bytes were read");
        if length < 0 {
            return Err(PngError::Image(format!("Invalid chunk length {length}")));
        }
        let start = stream.position;
        let crc = if &kind == b"IEND" {
            None
        } else {
            let mut peek = stream.clone();
            peek.position = start + length as u64;
            Some(
                peek.i32_be()
                    .map_err(|_| PngError::Image(format!("Invalid chunk length {length}")))?,
            )
        };
        match &kind {
            b"IDAT" => {
                if !seen_idat && metadata.color_type == PALETTE && metadata.plte.is_none() {
                    return Err(image("Required PLTE chunk missing"));
                }
                seen_idat = true;
                stream.skip(length as u64);
            }
            b"IEND" => return Ok(()),
            _ => chunk(stream, &kind, length, metadata)?,
        }
        if crc != Some(stream.i32_be()?) {
            return Err(PngError::Image(format!(
                "Failed to read a chunk of type {}",
                i32::from_be_bytes(kind)
            )));
        }
    }
}

fn chunk(
    stream: &mut Stream,
    kind: &[u8; 4],
    length: i32,
    metadata: &mut Metadata,
) -> Result<(), PngError> {
    let start = stream.position;
    let length_usize = length as usize;
    match kind {
        b"PLTE" => {
            if metadata.plte.is_some() || [GRAY, GRAY_ALPHA].contains(&metadata.color_type) {
                return Ok(());
            }
            let palette = stream.bytes(length_usize)?;
            let mut entries = length_usize / 3;
            if metadata.color_type == PALETTE {
                entries = entries.min(1 << metadata.bit_depth);
            }
            let size = match entries {
                17.. => 256,
                5..=16 => 16,
                3..=4 => 4,
                _ => 2,
            };
            let (mut red, mut green, mut blue) = (vec![0; size], vec![0; size], vec![0; size]);
            for index in 0..entries {
                red[index] = palette[index * 3];
                green[index] = palette[index * 3 + 1];
                blue[index] = palette[index * 3 + 2];
            }
            metadata.plte = Some((red, green, blue));
        }
        b"bKGD" => {
            metadata.bkgd = Some(match metadata.color_type {
                PALETTE => Background::Palette(stream.u8()?),
                GRAY | GRAY_ALPHA => Background::Gray(stream.u16_be()?),
                _ => Background::Rgb(stream.u16_be()?, stream.u16_be()?, stream.u16_be()?),
            });
        }
        b"cHRM" => {
            let mut values = [0; 8];
            for value in &mut values {
                *value = stream.i32_be()?;
            }
            metadata.chrm = Some(values);
        }
        b"gAMA" => metadata.gama = Some(stream.i32_be()?),
        b"hIST" => {
            if metadata.plte.is_none() {
                return Err(image("hIST chunk without prior PLTE chunk!"));
            }
            let mut histogram = Vec::with_capacity(length_usize / 2);
            for _ in 0..length_usize / 2 {
                histogram.push(stream.u16_be()?);
            }
            metadata.hist = Some(histogram);
        }
        b"iCCP" => {
            let keyword = null_terminated(stream, "ISO-8859-1", 80)?;
            let compressed = i64::from(length) - keyword.chars().count() as i64 - 2;
            if compressed <= 0 {
                return Err(image("iCCP chunk length is not proper"));
            }
            let method = stream.u8()?;
            stream.bytes(compressed as usize)?;
            metadata.iccp = Some((keyword, method));
        }
        b"iTXt" => itxt(stream, start, length, metadata)?,
        b"pHYs" => {
            metadata.phys = Some((stream.i32_be()?, stream.i32_be()?, stream.u8()?));
        }
        b"sBIT" => {
            let color_type = metadata.color_type;
            let mut bits = SignificantBits {
                color_type,
                gray: 0,
                red: 0,
                green: 0,
                blue: 0,
                alpha: 0,
            };
            if [GRAY, GRAY_ALPHA].contains(&color_type) {
                bits.gray = stream.u8()?;
            } else if [RGB, PALETTE, RGB_ALPHA].contains(&color_type) {
                bits.red = stream.u8()?;
                bits.green = stream.u8()?;
                bits.blue = stream.u8()?;
            }
            if [GRAY_ALPHA, RGB_ALPHA].contains(&color_type) {
                bits.alpha = stream.u8()?;
            }
            metadata.sbit = Some(bits);
        }
        b"sPLT" => {
            let name = null_terminated(stream, "ISO-8859-1", 80)?;
            let remaining = i64::from(length) - (name.chars().count() as i64 + 1);
            if remaining <= 0 {
                return Err(image("sPLT chunk length is not proper"));
            }
            let depth = stream.u8()?;
            let entry_size = 4 * i64::from(depth / 8) + 2;
            let count = (remaining / entry_size) as usize;
            let mut entries = Vec::with_capacity(count);
            match depth {
                8 => {
                    for _ in 0..count {
                        let bytes = stream.bytes(4)?;
                        let frequency = stream.u16_be()?;
                        entries.push([
                            u16::from(bytes[0]),
                            u16::from(bytes[1]),
                            u16::from(bytes[2]),
                            u16::from(bytes[3]),
                            frequency,
                        ]);
                    }
                }
                16 => {
                    for _ in 0..count {
                        entries.push([
                            stream.u16_be()?,
                            stream.u16_be()?,
                            stream.u16_be()?,
                            stream.u16_be()?,
                            stream.u16_be()?,
                        ]);
                    }
                }
                _ => return Err(image("sPLT sample depth not 8 or 16!")),
            }
            metadata.splt = Some(SuggestedPalette {
                name,
                depth,
                entries,
            });
        }
        b"sRGB" => metadata.srgb = Some(stream.u8()?),
        b"tEXt" => {
            let keyword = null_terminated(stream, "ISO-8859-1", 80)?;
            let text_length = i64::from(length) - keyword.chars().count() as i64 - 1;
            if text_length < 0 {
                return Err(image("tEXt chunk length is not proper"));
            }
            let text = decode(stream.bytes(text_length as usize)?, "ISO-8859-1");
            decode_creation_time(&keyword, &text, metadata)?;
            metadata.text.push((keyword, text));
        }
        b"tIME" => {
            metadata.time = Some([
                i32::from(stream.u16_be()?),
                i32::from(stream.u8()?),
                i32::from(stream.u8()?),
                i32::from(stream.u8()?),
                i32::from(stream.u8()?),
                i32::from(stream.u8()?),
            ]);
        }
        b"tRNS" => trns(stream, length_usize, metadata)?,
        b"zTXt" => {
            let keyword = null_terminated(stream, "ISO-8859-1", 80)?;
            let text_length = i64::from(length) - keyword.chars().count() as i64 - 2;
            if text_length < 0 {
                return Err(image("zTXt chunk length is not proper"));
            }
            let method = stream.u8()?;
            let text = decode(&inflate(stream.bytes(text_length as usize)?)?, "ISO-8859-1");
            decode_creation_time(&keyword, &text, metadata)?;
            metadata.ztxt.push((keyword, method, text));
        }
        _ => {
            stream.bytes(length_usize)?;
            metadata
                .unknown
                .push(kind.iter().map(|byte| char::from(*byte)).collect());
        }
    }
    Ok(())
}

/// `decodeImageCreationTimeFromTextChunk`: a text chunk of the image's creation time sets the
/// standard tree's when it parses, and leaves an earlier one when it does not.
fn decode_creation_time(
    keyword: &str,
    text: &str,
    metadata: &mut Metadata,
) -> Result<(), PngError> {
    if keyword == CREATION_TIME_KEY {
        if let Some(fields) = creation_time::parse(text).map_err(PngError::Unported)? {
            metadata.creation_time = Some(fields);
        }
    }
    Ok(())
}

fn itxt(
    stream: &mut Stream,
    start: u64,
    length: i32,
    metadata: &mut Metadata,
) -> Result<(), PngError> {
    let end = start as i64 + i64::from(length);
    let keyword = null_terminated(stream, "ISO-8859-1", 80)?;
    let compressed = stream.u8()? == 1;
    let method = stream.u8()?;
    let language = null_terminated(stream, "UTF-8", end - stream.position as i64)?;
    let remaining = end - stream.position as i64;
    if remaining < 0 {
        return Err(image("iTXt chunk length is not proper"));
    }
    let translated = null_terminated(stream, "UTF-8", remaining)?;
    let text_length = end - stream.position as i64;
    if text_length < 0 {
        return Err(image("iTXt chunk length is not proper"));
    }
    let bytes = stream.bytes(text_length as usize)?;
    let text = if compressed {
        decode(&inflate(bytes)?, "UTF-8")
    } else {
        decode(bytes, "UTF-8")
    };
    decode_creation_time(&keyword, &text, metadata)?;
    metadata.itxt.push(InternationalText {
        keyword,
        compressed,
        method,
        language,
        translated,
        text,
    });
    Ok(())
}

fn trns(stream: &mut Stream, length: usize, metadata: &mut Metadata) -> Result<(), PngError> {
    match metadata.color_type {
        PALETTE => {
            let Some((red, _, _)) = &metadata.plte else {
                return Ok(());
            };
            let max = red.len();
            let entries = if length > max && max > 0 { max } else { length };
            let alpha = stream.some_bytes(entries).to_vec();
            let mut alpha_table = vec![0u8; entries];
            alpha_table[..alpha.len()].copy_from_slice(&alpha);
            stream.skip((length - entries) as u64);
            metadata.trns = Some(Transparency::Palette(alpha_table));
        }
        GRAY => {
            if length != 2 {
                stream.skip(length as u64);
                return Ok(());
            }
            metadata.trns = Some(Transparency::Gray(stream.u16_be()?));
        }
        RGB => {
            if length != 6 {
                stream.skip(length as u64);
                return Ok(());
            }
            metadata.trns = Some(Transparency::Rgb(
                stream.u16_be()?,
                stream.u16_be()?,
                stream.u16_be()?,
            ));
        }
        _ => {}
    }
    Ok(())
}

fn runtime(what: &str) -> PngError {
    PngError::Runtime(format!(
        "ArrayIndexOutOfBoundsException naming the PNG's {what}"
    ))
}

/// `getNativeTree`.
fn native_tree(metadata: &Metadata) -> Result<Node, PngError> {
    let mut root = Node::new("javax_imageio_png_1.0");
    root.push(
        Node::new("IHDR")
            .attribute("width", metadata.width.to_string())
            .attribute("height", metadata.height.to_string())
            .attribute("bitDepth", metadata.bit_depth.to_string())
            .attribute(
                "colorType",
                COLOR_TYPE_NAMES[usize::from(metadata.color_type)]
                    .expect("the header checked the color type"),
            )
            .attribute("compressionMethod", "deflate")
            .attribute("filterMethod", "adaptive")
            .attribute(
                "interlaceMethod",
                if metadata.interlace == 0 {
                    "none"
                } else {
                    "adam7"
                },
            ),
    );
    if let Some((red, green, blue)) = &metadata.plte {
        let mut node = Node::new("PLTE");
        for index in 0..red.len() {
            node.push(
                Node::new("PLTEEntry")
                    .attribute("index", index.to_string())
                    .attribute("red", red[index].to_string())
                    .attribute("green", green[index].to_string())
                    .attribute("blue", blue[index].to_string()),
            );
        }
        root.push(node);
    }
    if let Some(background) = &metadata.bkgd {
        let child = match background {
            Background::Palette(index) => {
                Node::new("bKGD_Palette").attribute("index", index.to_string())
            }
            Background::Gray(gray) => {
                Node::new("bKGD_Grayscale").attribute("gray", gray.to_string())
            }
            Background::Rgb(red, green, blue) => Node::new("bKGD_RGB")
                .attribute("red", red.to_string())
                .attribute("green", green.to_string())
                .attribute("blue", blue.to_string()),
        };
        root.push(Node::new("bKGD").child(child));
    }
    if let Some(values) = metadata.chrm {
        let names = [
            "whitePointX",
            "whitePointY",
            "redX",
            "redY",
            "greenX",
            "greenY",
            "blueX",
            "blueY",
        ];
        let mut node = Node::new("cHRM");
        for (name, value) in names.iter().zip(values) {
            node = node.attribute(name, value.to_string());
        }
        root.push(node);
    }
    if let Some(gamma) = metadata.gama {
        root.push(Node::new("gAMA").attribute("value", gamma.to_string()));
    }
    if let Some(histogram) = &metadata.hist {
        let mut node = Node::new("hIST");
        for (index, value) in histogram.iter().enumerate() {
            node.push(
                Node::new("hISTEntry")
                    .attribute("index", index.to_string())
                    .attribute("value", value.to_string()),
            );
        }
        root.push(node);
    }
    if let Some((name, method)) = &metadata.iccp {
        if *method != 0 {
            return Err(runtime("ICC profile compression method"));
        }
        root.push(
            Node::new("iCCP")
                .attribute("profileName", name.clone())
                .attribute("compressionMethod", "deflate"),
        );
    }
    if !metadata.itxt.is_empty() {
        let mut node = Node::new("iTXt");
        for entry in &metadata.itxt {
            node.push(
                Node::new("iTXtEntry")
                    .attribute("keyword", entry.keyword.clone())
                    .attribute(
                        "compressionFlag",
                        if entry.compressed { "TRUE" } else { "FALSE" },
                    )
                    .attribute("compressionMethod", entry.method.to_string())
                    .attribute("languageTag", entry.language.clone())
                    .attribute("translatedKeyword", entry.translated.clone())
                    .attribute("text", entry.text.clone()),
            );
        }
        root.push(node);
    }
    if let Some((x, y, unit)) = metadata.phys {
        let unit = UNIT_SPECIFIER_NAMES
            .get(usize::from(unit))
            .ok_or_else(|| runtime("pHYs unit"))?;
        root.push(
            Node::new("pHYs")
                .attribute("pixelsPerUnitXAxis", x.to_string())
                .attribute("pixelsPerUnitYAxis", y.to_string())
                .attribute("unitSpecifier", *unit),
        );
    }
    if let Some(bits) = &metadata.sbit {
        let child = match bits.color_type {
            GRAY => Node::new("sBIT_Grayscale").attribute("gray", bits.gray.to_string()),
            GRAY_ALPHA => Node::new("sBIT_GrayAlpha")
                .attribute("gray", bits.gray.to_string())
                .attribute("alpha", bits.alpha.to_string()),
            RGB => Node::new("sBIT_RGB")
                .attribute("red", bits.red.to_string())
                .attribute("green", bits.green.to_string())
                .attribute("blue", bits.blue.to_string()),
            RGB_ALPHA => Node::new("sBIT_RGBAlpha")
                .attribute("red", bits.red.to_string())
                .attribute("green", bits.green.to_string())
                .attribute("blue", bits.blue.to_string())
                .attribute("alpha", bits.alpha.to_string()),
            _ => Node::new("sBIT_Palette")
                .attribute("red", bits.red.to_string())
                .attribute("green", bits.green.to_string())
                .attribute("blue", bits.blue.to_string()),
        };
        root.push(Node::new("sBIT").child(child));
    }
    if let Some(palette) = &metadata.splt {
        let mut node = Node::new("sPLT")
            .attribute("name", palette.name.clone())
            .attribute("sampleDepth", palette.depth.to_string());
        for (index, [red, green, blue, alpha, frequency]) in palette.entries.iter().enumerate() {
            node.push(
                Node::new("sPLTEntry")
                    .attribute("index", index.to_string())
                    .attribute("red", red.to_string())
                    .attribute("green", green.to_string())
                    .attribute("blue", blue.to_string())
                    .attribute("alpha", alpha.to_string())
                    .attribute("frequency", frequency.to_string()),
            );
        }
        root.push(node);
    }
    if let Some(intent) = metadata.srgb {
        let intent = RENDERING_INTENT_NAMES
            .get(usize::from(intent))
            .ok_or_else(|| runtime("sRGB rendering intent"))?;
        root.push(Node::new("sRGB").attribute("renderingIntent", *intent));
    }
    if !metadata.text.is_empty() {
        let mut node = Node::new("tEXt");
        for (keyword, value) in &metadata.text {
            node.push(
                Node::new("tEXtEntry")
                    .attribute("keyword", keyword.clone())
                    .attribute("value", value.clone()),
            );
        }
        root.push(node);
    }
    if let Some(time) = metadata.time {
        root.push(time_node("tIME", time));
    }
    if let Some(transparency) = &metadata.trns {
        let child = match transparency {
            Transparency::Palette(alpha) => {
                let mut node = Node::new("tRNS_Palette");
                for (index, value) in alpha.iter().enumerate() {
                    node.push(
                        Node::new("tRNS_PaletteEntry")
                            .attribute("index", index.to_string())
                            .attribute("alpha", value.to_string()),
                    );
                }
                node
            }
            Transparency::Gray(gray) => {
                Node::new("tRNS_Grayscale").attribute("gray", gray.to_string())
            }
            Transparency::Rgb(red, green, blue) => Node::new("tRNS_RGB")
                .attribute("red", red.to_string())
                .attribute("green", green.to_string())
                .attribute("blue", blue.to_string()),
        };
        root.push(Node::new("tRNS").child(child));
    }
    if !metadata.ztxt.is_empty() {
        let mut node = Node::new("zTXt");
        for (keyword, method, text) in &metadata.ztxt {
            if *method != 0 {
                return Err(runtime("zTXt compression method"));
            }
            node.push(
                Node::new("zTXtEntry")
                    .attribute("keyword", keyword.clone())
                    .attribute("compressionMethod", "deflate")
                    .attribute("text", text.clone()),
            );
        }
        root.push(node);
    }
    if !metadata.unknown.is_empty() {
        let mut node = Node::new("UnknownChunks");
        for kind in &metadata.unknown {
            node.push(Node::new("UnknownChunk").attribute("type", kind.clone()));
        }
        root.push(node);
    }
    Ok(root)
}

fn time_node(name: &'static str, [year, month, day, hour, minute, second]: [i32; 6]) -> Node {
    Node::new(name)
        .attribute("year", year.to_string())
        .attribute("month", month.to_string())
        .attribute("day", day.to_string())
        .attribute("hour", hour.to_string())
        .attribute("minute", minute.to_string())
        .attribute("second", second.to_string())
}

/// `getNumChannels`: a palette with transparency counts its alpha.
fn channels(metadata: &Metadata) -> usize {
    if metadata.color_type == PALETTE && matches!(metadata.trns, Some(Transparency::Palette(_))) {
        4
    } else {
        NUM_CHANNELS[usize::from(metadata.color_type)]
    }
}

/// `getStandardTree`: chroma, compression, data, dimension, document, text and transparency.
fn standard(metadata: &Metadata) -> Result<Node, PngError> {
    Ok(standard_tree([
        Some(chroma(metadata)),
        Some(
            Node::new("Compression")
                .child(Node::new("CompressionTypeName").attribute("value", "deflate"))
                .child(Node::new("Lossless").attribute("value", "TRUE"))
                .child(
                    Node::new("NumProgressiveScans")
                        .attribute("value", if metadata.interlace == 0 { "1" } else { "7" }),
                ),
        ),
        Some(data(metadata)),
        Some(dimension(metadata)),
        document(metadata),
        text(metadata),
        None,
        Some(transparency(metadata)),
    ]))
}

/// `getStandardDocumentNode`: the `tIME` chunk's time, then the creation time a text gave.
fn document(metadata: &Metadata) -> Option<Node> {
    let times = [
        metadata
            .time
            .map(|time| time_node("ImageModificationTime", time)),
        metadata
            .creation_time
            .map(|time| time_node("ImageCreationTime", time)),
    ];
    if times.iter().all(Option::is_none) {
        return None;
    }
    let mut node = Node::new("Document");
    for time in times.into_iter().flatten() {
        node.push(time);
    }
    Some(node)
}

fn chroma(metadata: &Metadata) -> Node {
    let mut node = Node::new("Chroma")
        .child(
            Node::new("ColorSpaceType").attribute(
                "name",
                COLOR_SPACE_TYPE_NAMES[usize::from(metadata.color_type)]
                    .expect("the header checked the color type"),
            ),
        )
        .child(Node::new("NumChannels").attribute("value", channels(metadata).to_string()));
    if let Some(gamma) = metadata.gama {
        node.push(
            Node::new("Gamma").attribute("value", java_float_to_string(gamma as f32 * 1.0e-5_f32)),
        );
    }
    node.push(Node::new("BlackIsZero").attribute("value", "TRUE"));
    if let Some((red, green, blue)) = &metadata.plte {
        let alpha = match &metadata.trns {
            Some(Transparency::Palette(alpha)) => Some(alpha),
            _ => None,
        };
        let mut palette = Node::new("Palette");
        for index in 0..red.len() {
            let mut entry = Node::new("PaletteEntry")
                .attribute("index", index.to_string())
                .attribute("red", red[index].to_string())
                .attribute("green", green[index].to_string())
                .attribute("blue", blue[index].to_string());
            if let Some(alpha) = alpha {
                let value = alpha.get(index).copied().unwrap_or(255);
                entry = entry.attribute("alpha", value.to_string());
            }
            palette.push(entry);
        }
        node.push(palette);
    }
    if let Some(background) = &metadata.bkgd {
        node.push(match background {
            Background::Palette(index) => {
                Node::new("BackgroundIndex").attribute("value", index.to_string())
            }
            Background::Gray(gray) => Node::new("BackgroundColor")
                .attribute("red", gray.to_string())
                .attribute("green", gray.to_string())
                .attribute("blue", gray.to_string()),
            Background::Rgb(red, green, blue) => Node::new("BackgroundColor")
                .attribute("red", red.to_string())
                .attribute("green", green.to_string())
                .attribute("blue", blue.to_string()),
        });
    }
    node
}

fn data(metadata: &Metadata) -> Node {
    let depth = metadata.bit_depth.to_string();
    let bits = vec![depth.as_str(); channels(metadata)].join(" ");
    let mut node = Node::new("Data")
        .child(Node::new("PlanarConfiguration").attribute("value", "PixelInterleaved"))
        .child(Node::new("SampleFormat").attribute(
            "value",
            if metadata.color_type == PALETTE {
                "Index"
            } else {
                "UnsignedIntegral"
            },
        ))
        .child(Node::new("BitsPerSample").attribute("value", bits));
    if let Some(sbit) = &metadata.sbit {
        let mut bits = if [GRAY, GRAY_ALPHA].contains(&sbit.color_type) {
            sbit.gray.to_string()
        } else {
            format!("{} {} {}", sbit.red, sbit.green, sbit.blue)
        };
        if [GRAY_ALPHA, RGB_ALPHA].contains(&sbit.color_type) {
            bits.push_str(&format!(" {}", sbit.alpha));
        }
        node.push(Node::new("SignificantBitsPerSample").attribute("value", bits));
    }
    node
}

fn dimension(metadata: &Metadata) -> Node {
    let ratio = match metadata.phys {
        Some((x, y, _)) => x as f32 / y as f32,
        None => 1.0,
    };
    let mut node = Node::new("Dimension")
        .child(Node::new("PixelAspectRatio").attribute("value", java_float_to_string(ratio)))
        .child(Node::new("ImageOrientation").attribute("value", "Normal"));
    if let Some((x, y, 1)) = metadata.phys {
        node.push(
            Node::new("HorizontalPixelSize")
                .attribute("value", java_float_to_string(1000.0_f32 / x as f32)),
        );
        node.push(
            Node::new("VerticalPixelSize")
                .attribute("value", java_float_to_string(1000.0_f32 / y as f32)),
        );
    }
    node
}

fn text(metadata: &Metadata) -> Option<Node> {
    if metadata.text.is_empty() && metadata.itxt.is_empty() && metadata.ztxt.is_empty() {
        return None;
    }
    let mut node = Node::new("Text");
    for (keyword, value) in &metadata.text {
        node.push(
            Node::new("TextEntry")
                .attribute("keyword", keyword.clone())
                .attribute("value", value.clone())
                .attribute("encoding", "ISO-8859-1")
                .attribute("compression", "none"),
        );
    }
    for entry in &metadata.itxt {
        node.push(
            Node::new("TextEntry")
                .attribute("keyword", entry.keyword.clone())
                .attribute("value", entry.text.clone())
                .attribute("language", entry.language.clone())
                .attribute("compression", if entry.compressed { "zip" } else { "none" }),
        );
    }
    for (keyword, _, text) in &metadata.ztxt {
        node.push(
            Node::new("TextEntry")
                .attribute("keyword", keyword.clone())
                .attribute("value", text.clone())
                .attribute("compression", "zip"),
        );
    }
    Some(node)
}

fn transparency(metadata: &Metadata) -> Node {
    let alpha = matches!(metadata.color_type, RGB_ALPHA | GRAY_ALPHA)
        || (metadata.color_type == PALETTE
            && matches!(metadata.trns, Some(Transparency::Palette(_))));
    let mut node = Node::new("Transparency").child(
        Node::new("Alpha").attribute("value", if alpha { "nonpremultipled" } else { "none" }),
    );
    if let Some(transparency) = &metadata.trns {
        let mut color = Node::new("TransparentColor");
        match transparency {
            Transparency::Rgb(red, green, blue) => {
                color = color.attribute("value", format!("{red} {green} {blue}"));
            }
            Transparency::Gray(gray) => color = color.attribute("value", gray.to_string()),
            Transparency::Palette(_) => {}
        }
        node.push(color);
    }
    node
}
