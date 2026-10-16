//! jbig2-imageio's `JBIG2ImageReader` as far as counting a file's pages and reading the first
//! one's size and metadata, and `JBIG2ImageMetadata`'s trees (`jbig2` and the standard one),
//! which hold its dimension node alone.

use crate::java::dtoa::java_float_to_string;
use crate::ocr::jbig2::{Jbig2File, PageInformation};

use super::{standard_tree, Node, Read};

/// The height a page information segment gives a striped page whose height is known only once
/// it is decoded.
const UNKNOWN_HEIGHT: i32 = -1;
/// Millimetres in an inch, and inches in a metre as `JBIG2ImageMetadata` rounds them.
const MILLIMETRES_PER_INCH: f32 = 25.4;
const INCHES_PER_METRE: f32 = 39.3701;

/// `getNumImages(true)`, `getWidth(0)`, `getHeight(0)` and `getImageMetadata(0)`, or why the port
/// cannot read them as the reader does: where it throws, or where it would decode the page to
/// learn its height.
pub(crate) fn read(input: &[u8]) -> Result<Read, String> {
    let file = Jbig2File::read(input)?;
    let images = file.page_count();
    let page = file.first_page(input)?;
    if page.height == UNKNOWN_HEIGHT {
        return Err("a striped page whose height only decoding it tells".to_owned());
    }
    let dimension = dimension(&page);
    Ok(Read {
        images,
        width: page.width,
        height: page.height,
        trees: vec![
            Node::new("jbig2").child(dimension.clone()),
            standard_tree([None, None, None, Some(dimension), None, None, None, None]),
        ],
    })
}

/// `getStandardDimensionNode`: the size of a pixel in millimetres where the page gives its
/// resolution.
fn dimension(page: &PageInformation) -> Node {
    let mut node = Node::new("Dimension")
        .child(Node::new("PixelAspectRatio").attribute("value", "1.0"))
        .child(Node::new("ImageOrientation").attribute("value", "Normal"));
    for (name, resolution) in [
        ("HorizontalPixelSize", page.resolution_x),
        ("VerticalPixelSize", page.resolution_y),
    ] {
        if resolution != 0 {
            let size = MILLIMETRES_PER_INCH / (resolution as f32 / INCHES_PER_METRE);
            node.push(Node::new(name).attribute("value", java_float_to_string(size)));
        }
    }
    node
}
