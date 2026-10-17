//! jbig2-imageio's `JBIG2ImageReader` as far as counting a file's pages and reading the first
//! one's size and metadata, and `JBIG2ImageMetadata`'s trees (`jbig2` and the standard one).
//! jbig2-imageio holds the dimension node in both, so that Tika records it twice (#120); the
//! port keeps it in the standard tree alone.

use crate::java::dtoa::java_float_to_string;
use crate::ocr::jbig2::{Jbig2File, PageInformation};

use super::{standard_tree, Node, Read};

/// The height a page information segment gives a striped page whose height is known only once
/// it is decoded.
const UNKNOWN_HEIGHT: i32 = -1;
/// Millimetres in a metre.
const MILLIMETRES_PER_METRE: f64 = 1000.0;

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
            Node::new("jbig2"),
            standard_tree([None, None, None, Some(dimension), None, None, None, None]),
        ],
    })
}

/// `getStandardDimensionNode`: the size of a pixel in millimetres where the page gives its
/// resolution, in pixels a metre, written as the standard tree's floats are. jbig2-imageio
/// converts through 39.3701 inches a metre in `float` arithmetic, a little off (#120).
fn dimension(page: &PageInformation) -> Node {
    let mut node = Node::new("Dimension")
        .child(Node::new("PixelAspectRatio").attribute("value", "1.0"))
        .child(Node::new("ImageOrientation").attribute("value", "Normal"));
    for (name, resolution) in [
        ("HorizontalPixelSize", page.resolution_x),
        ("VerticalPixelSize", page.resolution_y),
    ] {
        if resolution != 0 {
            let size = (MILLIMETRES_PER_METRE / f64::from(resolution)) as f32;
            node.push(Node::new(name).attribute("value", java_float_to_string(size)));
        }
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_pixels_from_their_resolution_in_a_metre() {
        let page = PageInformation {
            width: 1,
            height: 1,
            resolution_x: 11811,
            resolution_y: 4000,
        };
        assert_eq!(
            dimension(&page),
            Node::new("Dimension")
                .child(Node::new("PixelAspectRatio").attribute("value", "1.0"))
                .child(Node::new("ImageOrientation").attribute("value", "Normal"))
                .child(Node::new("HorizontalPixelSize").attribute("value", "0.08466683"))
                .child(Node::new("VerticalPixelSize").attribute("value", "0.25"))
        );
    }
}
