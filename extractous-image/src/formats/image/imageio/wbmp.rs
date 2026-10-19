//! The JDK's `WBMPImageReader` as far as its header, and `WBMPMetadata`'s standard tree: its
//! native tree keeps values in its nodes rather than in attributes, so Tika takes nothing from
//! it. Every WBMP is read so, where Tika may take jai-imageio's reader instead (#114).

use super::{standard_tree, Eof, Node, Read, Stream};

/// Why the WBMP could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WbmpError {
    /// The `IIOException` of a header that is not of a type 0 WBMP.
    Image(&'static str),
    /// The `EOFException` of a header that ends early.
    Eof,
}

impl From<Eof> for WbmpError {
    fn from(_: Eof) -> Self {
        Self::Eof
    }
}

/// `getNumImages(true)`, which is one without reading, then `getWidth(0)`, `getHeight(0)` and
/// `getImageMetadata(0)`, which read the header once.
pub(crate) fn read(input: &[u8]) -> Result<Read, WbmpError> {
    let mut stream = Stream::new(input);
    let kind = stream.u8()?;
    let fixed_header = stream.u8()?;
    if fixed_header != 0 || kind != 0 {
        return Err(WbmpError::Image("Bad WBMP header."));
    }
    let width = multi_byte_integer(&mut stream)?;
    let height = multi_byte_integer(&mut stream)?;
    Ok(Read {
        images: 1,
        width,
        height,
        trees: vec![standard_tree([
            Some(Node::new("Chroma").child(Node::new("BlackIsZero").attribute("value", "TRUE"))),
            None,
            None,
            Some(
                Node::new("Dimension")
                    .child(Node::new("ImageOrientation").attribute("value", "Normal")),
            ),
            None,
            None,
            None,
            None,
        ])],
    })
}

/// `ReaderUtil.readMultiByteInteger`: seven bits a byte, most significant first, while a byte's
/// top bit is set, in an `int` that wraps.
fn multi_byte_integer(stream: &mut Stream) -> Result<i32, Eof> {
    let mut byte = stream.u8()?;
    let mut value = i32::from(byte & 0x7F);
    while byte & 0x80 != 0 {
        byte = stream.u8()?;
        value = value.wrapping_shl(7) | i32::from(byte & 0x7F);
    }
    Ok(value)
}
