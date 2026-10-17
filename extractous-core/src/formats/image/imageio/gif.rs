//! The JDK's `GIFImageReader` as far as counting its images and reading the first one's
//! metadata, and `GIFImageMetadata`'s trees (`javax_imageio_gif_image_1.0` and the standard one).
//!
//! A stray zero byte between blocks, which some encoders leave, is passed over. The JDK's reader
//! stops counting images at one and throws `Unexpected block type 0!` reading the metadata after
//! one, which Tika swallows, keeping the image count alone (#114).

use super::{standard_tree, Eof, Node, Read, Stream};

const IMAGE_DESCRIPTOR: u8 = 0x2c;
const EXTENSION: u8 = 0x21;
const TRAILER: u8 = 0x3b;
const DISPOSAL_METHOD_NAMES: [&str; 8] = [
    "none",
    "doNotDispose",
    "restoreToBackgroundColor",
    "restoreToPrevious",
    "undefinedDisposalMethod4",
    "undefinedDisposalMethod5",
    "undefinedDisposalMethod6",
    "undefinedDisposalMethod7",
];

/// Why the GIF could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GifError {
    /// An `IIOException`, with its message, which Tika reports as a parse error of the type.
    Image(String),
    /// A runtime exception the reader throws, which fails the parse too.
    Runtime(String),
}

#[derive(Debug, Default, Clone)]
struct ImageMetadata {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    interlace: bool,
    sort: bool,
    local_color_table: Option<Vec<u8>>,
    disposal: u8,
    user_input: bool,
    transparent: bool,
    delay: u16,
    transparent_index: u8,
    plain_text: Option<PlainText>,
    applications: Vec<([u8; 8], [u8; 3])>,
    comments: Vec<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct PlainText {
    fields: [u16; 4],
    cells: [u8; 4],
    text: Vec<u8>,
}

fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

/// `getNumImages(true)`, `getWidth(0)`, `getHeight(0)` and `getImageMetadata(0)`.
pub(crate) fn read(input: &[u8]) -> Result<Read, GifError> {
    let mut stream = Stream::new(input);
    let first =
        header(&mut stream).map_err(|_| GifError::Image("I/O error reading header!".to_owned()))?;
    let images = count(&mut stream.clone(), first)?;
    stream.position = first;
    let metadata = image_metadata(&mut stream)?;
    Ok(Read {
        images,
        width: i32::from(metadata.width),
        height: i32::from(metadata.height),
        trees: vec![native_tree(&metadata), standard(&metadata)],
    })
}

/// `readHeader`: the position the first image's blocks start at, after the global color table.
fn header(stream: &mut Stream) -> Result<u64, Eof> {
    stream.bytes(6)?;
    stream.bytes(4)?;
    let packed = stream.u8()?;
    stream.bytes(2)?;
    if packed & 0x80 != 0 {
        let entries = 1usize << ((packed & 0x7) + 1);
        stream.bytes(3 * entries)?;
    }
    Ok(stream.position)
}

/// `locateImage(Integer.MAX_VALUE) + 1`: the images found skipping one after another.
fn count(stream: &mut Stream, first: u64) -> Result<i32, GifError> {
    stream.position = first;
    let mut index: i32 = 0;
    loop {
        if !skip_image(stream) {
            return Ok(index);
        }
        index += 1;
    }
}

/// `skipImage`: past the next image, or false at the trailer or the end.
fn skip_image(stream: &mut Stream) -> bool {
    let skip_blocks = |stream: &mut Stream| -> Result<(), Eof> {
        loop {
            let length = stream.u8()?;
            stream.skip(u64::from(length));
            if length == 0 {
                return Ok(());
            }
        }
    };
    let result = (|| -> Result<bool, Eof> {
        loop {
            match stream.u8()? {
                IMAGE_DESCRIPTOR => {
                    stream.skip(8);
                    let packed = stream.u8()?;
                    if packed & 0x80 != 0 {
                        stream.skip(3 * (1u64 << ((packed & 0x7) + 1)));
                    }
                    stream.skip(1);
                    skip_blocks(stream)?;
                    return Ok(true);
                }
                TRAILER => return Ok(false),
                0 => {}
                EXTENSION => {
                    stream.u8()?;
                    skip_blocks(stream)?;
                }
                _ => skip_blocks(stream)?,
            }
        }
    })();
    result.unwrap_or(false)
}

/// `concatenateBlocks`: the sub-blocks up to the empty one.
fn blocks(stream: &mut Stream) -> Result<Vec<u8>, Eof> {
    let mut data = Vec::new();
    loop {
        let length = stream.u8()?;
        if length == 0 {
            return Ok(data);
        }
        data.extend_from_slice(stream.bytes(usize::from(length))?);
    }
}

/// `readMetadata` of the image the stream is at.
fn image_metadata(stream: &mut Stream) -> Result<ImageMetadata, GifError> {
    let io = |_: Eof| GifError::Image("I/O error reading image metadata!".to_owned());
    let mut metadata = ImageMetadata::default();
    loop {
        let kind = stream.u8().map_err(io)?;
        match kind {
            IMAGE_DESCRIPTOR => {
                metadata.left = stream.u16_le().map_err(io)?;
                metadata.top = stream.u16_le().map_err(io)?;
                metadata.width = stream.u16_le().map_err(io)?;
                metadata.height = stream.u16_le().map_err(io)?;
                let packed = stream.u8().map_err(io)?;
                metadata.interlace = packed & 0x40 != 0;
                metadata.sort = packed & 0x20 != 0;
                if packed & 0x80 != 0 {
                    let entries = 1usize << ((packed & 0x7) + 1);
                    metadata.local_color_table =
                        Some(stream.bytes(3 * entries).map_err(io)?.to_vec());
                }
                return Ok(metadata);
            }
            EXTENSION => extension(stream, &mut metadata).map_err(io)?,
            0 => {}
            TRAILER => {
                return Err(GifError::Runtime(
                    "Attempt to read past end of image sequence!".to_owned(),
                ))
            }
            other => return Err(GifError::Image(format!("Unexpected block type {other}!"))),
        }
    }
}

fn extension(stream: &mut Stream, metadata: &mut ImageMetadata) -> Result<(), Eof> {
    match stream.u8()? {
        0xf9 => {
            stream.u8()?;
            let packed = stream.u8()?;
            metadata.disposal = (packed >> 2) & 0x3;
            metadata.user_input = packed & 0x2 != 0;
            metadata.transparent = packed & 0x1 != 0;
            metadata.delay = stream.u16_le()?;
            metadata.transparent_index = stream.u8()?;
            stream.u8()?;
        }
        0x01 => {
            stream.u8()?;
            let fields = [
                stream.u16_le()?,
                stream.u16_le()?,
                stream.u16_le()?,
                stream.u16_le()?,
            ];
            let cells = [stream.u8()?, stream.u8()?, stream.u8()?, stream.u8()?];
            let text = blocks(stream)?;
            metadata.plain_text = Some(PlainText {
                fields,
                cells,
                text,
            });
        }
        0xfe => {
            let comment = blocks(stream)?;
            metadata.comments.push(comment);
        }
        0xff => {
            let size = usize::from(stream.u8()?);
            let data = stream.bytes(size)?;
            let mut id = [0u8; 8];
            let mut code = [0u8; 3];
            let id_length = data.len().min(8);
            id[..id_length].copy_from_slice(&data[..id_length]);
            let code_length = (data.len() - id_length).min(3);
            code[..code_length].copy_from_slice(&data[id_length..id_length + code_length]);
            blocks(stream)?;
            metadata.applications.push((id, code));
        }
        _ => loop {
            let length = stream.u8()?;
            stream.skip(u64::from(length));
            if length == 0 {
                break;
            }
        },
    }
    Ok(())
}

fn flag(value: bool) -> &'static str {
    if value {
        "TRUE"
    } else {
        "FALSE"
    }
}

/// `getNativeTree`.
fn native_tree(metadata: &ImageMetadata) -> Node {
    let mut root = Node::new("javax_imageio_gif_image_1.0");
    root.push(
        Node::new("ImageDescriptor")
            .attribute("imageLeftPosition", metadata.left.to_string())
            .attribute("imageTopPosition", metadata.top.to_string())
            .attribute("imageWidth", metadata.width.to_string())
            .attribute("imageHeight", metadata.height.to_string())
            .attribute("interlaceFlag", flag(metadata.interlace)),
    );
    if let Some(table) = &metadata.local_color_table {
        let entries = table.len() / 3;
        let mut node = Node::new("LocalColorTable")
            .attribute("sizeOfLocalColorTable", entries.to_string())
            .attribute("sortFlag", flag(metadata.sort));
        for index in 0..entries {
            node.push(color_entry("ColorTableEntry", index, table));
        }
        root.push(node);
    }
    root.push(
        Node::new("GraphicControlExtension")
            .attribute(
                "disposalMethod",
                DISPOSAL_METHOD_NAMES[usize::from(metadata.disposal)],
            )
            .attribute("userInputFlag", flag(metadata.user_input))
            .attribute("transparentColorFlag", flag(metadata.transparent))
            .attribute("delayTime", metadata.delay.to_string())
            .attribute(
                "transparentColorIndex",
                metadata.transparent_index.to_string(),
            ),
    );
    if let Some(text) = &metadata.plain_text {
        let [left, top, width, height] = text.fields;
        let [cell_width, cell_height, foreground, background] = text.cells;
        root.push(
            Node::new("PlainTextExtension")
                .attribute("textGridLeft", left.to_string())
                .attribute("textGridTop", top.to_string())
                .attribute("textGridWidth", width.to_string())
                .attribute("textGridHeight", height.to_string())
                .attribute("characterCellWidth", cell_width.to_string())
                .attribute("characterCellHeight", cell_height.to_string())
                .attribute("textForegroundColor", foreground.to_string())
                .attribute("textBackgroundColor", background.to_string())
                .attribute("text", latin1(&text.text)),
        );
    }
    if !metadata.applications.is_empty() {
        let mut node = Node::new("ApplicationExtensions");
        for (id, code) in &metadata.applications {
            node.push(
                Node::new("ApplicationExtension")
                    .attribute("applicationID", latin1(id))
                    .attribute("authenticationCode", latin1(code)),
            );
        }
        root.push(node);
    }
    if !metadata.comments.is_empty() {
        let mut node = Node::new("CommentExtensions");
        for comment in &metadata.comments {
            node.push(Node::new("CommentExtension").attribute("value", latin1(comment)));
        }
        root.push(node);
    }
    root
}

fn color_entry(name: &'static str, index: usize, table: &[u8]) -> Node {
    Node::new(name)
        .attribute("index", index.to_string())
        .attribute("red", table[3 * index].to_string())
        .attribute("green", table[3 * index + 1].to_string())
        .attribute("blue", table[3 * index + 2].to_string())
}

fn standard(metadata: &ImageMetadata) -> Node {
    let mut chroma = Node::new("Chroma")
        .child(Node::new("ColorSpaceType").attribute("name", "RGB"))
        .child(
            Node::new("NumChannels")
                .attribute("value", if metadata.transparent { "4" } else { "3" }),
        )
        .child(Node::new("BlackIsZero").attribute("value", "TRUE"));
    if let Some(table) = &metadata.local_color_table {
        let mut palette = Node::new("Palette");
        for index in 0..table.len() / 3 {
            palette.push(color_entry("PaletteEntry", index, table));
        }
        chroma.push(palette);
    }
    let text = (!metadata.comments.is_empty()).then(|| {
        let mut node = Node::new("Text");
        for comment in &metadata.comments {
            node.push(
                Node::new("TextEntry")
                    .attribute("value", latin1(comment))
                    .attribute("encoding", "ISO-8859-1")
                    .attribute("compression", "none"),
            );
        }
        node
    });
    let transparency = metadata.transparent.then(|| {
        Node::new("Transparency").child(
            Node::new("TransparentIndex")
                .attribute("value", metadata.transparent_index.to_string()),
        )
    });
    standard_tree([
        Some(chroma),
        Some(
            Node::new("Compression")
                .child(Node::new("CompressionTypeName").attribute("value", "lzw"))
                .child(Node::new("Lossless").attribute("value", "TRUE"))
                .child(
                    Node::new("NumProgressiveScans")
                        .attribute("value", if metadata.interlace { "4" } else { "1" }),
                ),
        ),
        Some(Node::new("Data").child(Node::new("SampleFormat").attribute("value", "Index"))),
        Some(
            Node::new("Dimension")
                .child(Node::new("ImageOrientation").attribute("value", "Normal"))
                .child(
                    Node::new("HorizontalPixelOffset")
                        .attribute("value", metadata.left.to_string()),
                )
                .child(
                    Node::new("VerticalPixelOffset").attribute("value", metadata.top.to_string()),
                ),
        ),
        None,
        text,
        None,
        transparency,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::charset::Charset;

    /// A GIF of `images` images of 3 by 2 pixels, a stray zero byte before each.
    fn gif(images: usize) -> Vec<u8> {
        let mut gif = b"GIF89a\x03\x00\x02\x00\x00\x00\x00".to_vec();
        for _ in 0..images {
            gif.push(0);
            gif.extend_from_slice(&[IMAGE_DESCRIPTOR, 0, 0, 0, 0, 3, 0, 2, 0, 0]);
            gif.extend_from_slice(&[2, 2, 0x44, 0x01, 0]);
        }
        gif.push(TRAILER);
        gif
    }

    #[test]
    fn passes_over_stray_zero_bytes() {
        let read = read(&gif(2)).unwrap();
        assert_eq!((read.images, read.width, read.height), (2, 3, 2));
    }

    #[test]
    fn reads_latin1_as_the_jdk_does() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(
            Charset::named("ISO-8859-1").decode(&bytes).as_deref(),
            Some(latin1(&bytes).as_str())
        );
    }
}
