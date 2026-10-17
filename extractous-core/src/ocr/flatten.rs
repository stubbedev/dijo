//! Our Java `PageFlattening`: a page photographed at an angle, made flat for OCR. The page is
//! found as the largest bright region against a darker background, its corners where that region
//! reaches furthest along the diagonals, and the quadrilateral they span is mapped back to a
//! rectangle. An image the page fills, as a scan does, one without a bright region large enough
//! to be a page, one whose page is already an upright rectangle, such as a screenshot's white
//! panel, and one whose bright region fades into the dark rather than ending at a sharp edge,
//! such as the lit middle of a page in shadow, are left as they are.

use image::GrayImage;

use super::bitmap::Bitmap;
use super::prepare::{histogram, otsu_threshold};

const ANALYSIS_SIZE: u32 = 800;
const MAX_PAGE_FILL: f64 = 0.85;
const MIN_PAGE_AREA: f64 = 0.2;
const MIN_CORNER_OFFSET: f64 = 0.02;
const WHITE: u8 = 255;
const EDGE_SAMPLES: u32 = 24;
const EDGE_OFFSET: f64 = 0.015;
const MIN_EDGE_CONTRAST: i64 = 64;

/// A point in an image, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

/// The page made flat, in grey, or none when the image has no page to flatten.
pub(crate) fn flatten(image: &Bitmap) -> Option<GrayImage> {
    let (image_width, image_height) = image.dimensions();
    let scale = 1.0_f64.min(f64::from(ANALYSIS_SIZE) / f64::from(image_width.max(image_height)));
    let width = ((f64::from(image_width) * scale).round() as u32).max(1);
    let height = ((f64::from(image_height) * scale).round() as u32).max(1);
    let gray = image.gray_resized(width, height);
    let levels = gray.as_raw();
    let threshold = otsu_threshold(&histogram(levels));
    let bright: Vec<bool> = levels
        .iter()
        .map(|&level| u32::from(level) >= threshold)
        .collect();
    let bright_count = bright.iter().filter(|&&bright| bright).count();
    if bright_count as f64 > MAX_PAGE_FILL * levels.len() as f64 {
        return None;
    }
    let mut corners = largest_region_corners(&bright, width as usize, height as usize)?;
    if area(&corners) < MIN_PAGE_AREA * levels.len() as f64
        || !convex(&corners)
        || upright(&corners, width, height)
        || !sharp_edges(&corners, &gray)
    {
        return None;
    }
    for corner in &mut corners {
        corner.x /= scale;
        corner.y /= scale;
    }
    Some(warp(&image.gray(), &corners))
}

/// The corners of the largest 4-connected bright region, top left, top right, bottom right and
/// bottom left, or none when it is smaller than a page could be.
fn largest_region_corners(bright: &[bool], width: usize, height: usize) -> Option<[Point; 4]> {
    let mut region = vec![0u32; bright.len()];
    let mut queue = vec![0usize; bright.len()];
    let mut largest = 0;
    let mut largest_size = 0;
    let mut label = 0;
    for start in 0..bright.len() {
        if !bright[start] || region[start] != 0 {
            continue;
        }
        label += 1;
        let (mut head, mut tail) = (0, 0);
        queue[tail] = start;
        tail += 1;
        region[start] = label;
        while head < tail {
            let pixel = queue[head];
            head += 1;
            let (x, y) = (pixel % width, pixel / width);
            let mut visit = |neighbour: usize| {
                if bright[neighbour] && region[neighbour] == 0 {
                    region[neighbour] = label;
                    queue[tail] = neighbour;
                    tail += 1;
                }
            };
            if x > 0 {
                visit(pixel - 1);
            }
            if x < width - 1 {
                visit(pixel + 1);
            }
            if y > 0 {
                visit(pixel - width);
            }
            if y < height - 1 {
                visit(pixel + width);
            }
        }
        if tail > largest_size {
            largest_size = tail;
            largest = label;
        }
    }
    if (largest_size as f64) < MIN_PAGE_AREA * bright.len() as f64 {
        return None;
    }
    let mut corners = [None; 4];
    let mut best = [f64::MAX, f64::MIN, f64::MIN, f64::MAX];
    for (pixel, _) in region
        .iter()
        .enumerate()
        .filter(|(_, &found)| found == largest)
    {
        let x = (pixel % width) as f64;
        let y = (pixel / width) as f64;
        let scores = [x + y, x - y, x + y, x - y];
        for corner in 0..4 {
            let better = if corner == 0 || corner == 3 {
                scores[corner] < best[corner]
            } else {
                scores[corner] > best[corner]
            };
            if better {
                best[corner] = scores[corner];
                corners[corner] = Some(Point { x, y });
            }
        }
    }
    Some([corners[0]?, corners[1]?, corners[2]?, corners[3]?])
}

fn area(corners: &[Point; 4]) -> f64 {
    let twice: f64 = (0..4)
        .map(|i| {
            let (a, b) = (corners[i], corners[(i + 1) % 4]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    twice.abs() / 2.0
}

fn convex(corners: &[Point; 4]) -> bool {
    let mut sign = 0.0;
    for i in 0..4 {
        let (a, b, c) = (corners[i], corners[(i + 1) % 4], corners[(i + 2) % 4]);
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        let turn = if cross > 0.0 {
            1.0
        } else if cross < 0.0 {
            -1.0
        } else {
            0.0
        };
        if turn == 0.0 || (sign != 0.0 && turn != sign) {
            return false;
        }
        sign = turn;
    }
    true
}

/// Whether the corners already make an upright rectangle, so that there is nothing to do.
fn upright(corners: &[Point; 4], width: u32, height: u32) -> bool {
    let tolerance = MIN_CORNER_OFFSET * f64::from(width.max(height));
    (corners[0].y - corners[1].y).abs() <= tolerance
        && (corners[3].y - corners[2].y).abs() <= tolerance
        && (corners[0].x - corners[3].x).abs() <= tolerance
        && (corners[1].x - corners[2].x).abs() <= tolerance
}

/// Whether every side of the quadrilateral is a sharp edge: just inside it, the image is
/// markedly brighter than just outside it.
fn sharp_edges(corners: &[Point; 4], gray: &GrayImage) -> bool {
    let (width, height) = gray.dimensions();
    let centre_x: f64 = corners.iter().map(|corner| corner.x / 4.0).sum();
    let centre_y: f64 = corners.iter().map(|corner| corner.y / 4.0).sum();
    let offset = EDGE_OFFSET * f64::from(width.max(height));
    for side in 0..4 {
        let (from, to) = (corners[side], corners[(side + 1) % 4]);
        let length = distance(from, to);
        let mut normal_x = -(to.y - from.y) / length;
        let mut normal_y = (to.x - from.x) / length;
        let middle_x = (from.x + to.x) / 2.0;
        let middle_y = (from.y + to.y) / 2.0;
        if normal_x * (centre_x - middle_x) + normal_y * (centre_y - middle_y) < 0.0 {
            normal_x = -normal_x;
            normal_y = -normal_y;
        }
        let mut contrast: i64 = 0;
        for sample in 1..=EDGE_SAMPLES {
            let along = f64::from(sample) / f64::from(EDGE_SAMPLES + 1);
            let x = from.x + (to.x - from.x) * along;
            let y = from.y + (to.y - from.y) * along;
            let inside = level(gray, x + normal_x * offset, y + normal_y * offset);
            let outside = level(gray, x - normal_x * offset, y - normal_y * offset);
            let (Some(inside), Some(outside)) = (inside, outside) else {
                return false;
            };
            contrast += i64::from(inside) - i64::from(outside);
        }
        if contrast < MIN_EDGE_CONTRAST * i64::from(EDGE_SAMPLES) {
            return false;
        }
    }
    true
}

/// The grey level at a point, or none outside the image.
fn level(gray: &GrayImage, x: f64, y: f64) -> Option<u8> {
    let (column, row) = (x.round(), y.round());
    if column < 0.0 || row < 0.0 {
        return None;
    }
    gray.get_pixel_checked(column as u32, row as u32)
        .map(|pixel| pixel.0[0])
}

/// The quadrilateral of `image` within `corners`, mapped onto an upright rectangle.
fn warp(image: &GrayImage, corners: &[Point; 4]) -> GrayImage {
    let out_width = distance(corners[0], corners[1])
        .max(distance(corners[3], corners[2]))
        .round() as u32;
    let out_height = distance(corners[0], corners[3])
        .max(distance(corners[1], corners[2]))
        .round() as u32;
    let mapping = square_to_quad(corners);
    GrayImage::from_fn(out_width, out_height, |u, v| {
        let s = (f64::from(u) + 0.5) / f64::from(out_width);
        let t = (f64::from(v) + 0.5) / f64::from(out_height);
        let w = mapping[6] * s + mapping[7] * t + 1.0;
        let x = (mapping[0] * s + mapping[1] * t + mapping[2]) / w - 0.5;
        let y = (mapping[3] * s + mapping[4] * t + mapping[5]) / w - 0.5;
        image::Luma([sample(image, x, y)])
    })
}

fn distance(a: Point, b: Point) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// The projective mapping of the unit square onto the quadrilateral (Heckbert, "Fundamentals of
/// Texture Mapping and Image Warping", 1989): `[a, b, c, d, e, f, g, h]` with
/// `x = (a s + b t + c) / (g s + h t + 1)` and `y = (d s + e t + f) / (g s + h t + 1)`.
fn square_to_quad(corners: &[Point; 4]) -> [f64; 8] {
    let [p0, p1, p2, p3] = *corners;
    let (dx1, dx2, dx3) = (p1.x - p2.x, p3.x - p2.x, p0.x - p1.x + p2.x - p3.x);
    let (dy1, dy2, dy3) = (p1.y - p2.y, p3.y - p2.y, p0.y - p1.y + p2.y - p3.y);
    let determinant = dx1 * dy2 - dx2 * dy1;
    let g = (dx3 * dy2 - dx2 * dy3) / determinant;
    let h = (dx1 * dy3 - dx3 * dy1) / determinant;
    [
        p1.x - p0.x + g * p1.x,
        p3.x - p0.x + h * p3.x,
        p0.x,
        p1.y - p0.y + g * p1.y,
        p3.y - p0.y + h * p3.y,
        p0.y,
        g,
        h,
    ]
}

/// The grey level at a point between pixels, white outside the image.
fn sample(image: &GrayImage, x: f64, y: f64) -> u8 {
    let (left, top) = (x.floor(), y.floor());
    let (width, height) = image.dimensions();
    if left < 0.0 || top < 0.0 || left + 1.0 >= f64::from(width) || top + 1.0 >= f64::from(height) {
        return WHITE;
    }
    let (fx, fy) = (x - left, y - top);
    let (left, top) = (left as u32, top as u32);
    let level = |x, y| f64::from(image.get_pixel(x, y).0[0]);
    let upper = level(left, top) + (level(left + 1, top) - level(left, top)) * fx;
    let lower = level(left, top + 1) + (level(left + 1, top + 1) - level(left, top + 1)) * fx;
    (upper + (lower - upper) * fy).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    #[test]
    fn leaves_a_scan_as_it_is() {
        let scan = Bitmap::Gray(GrayImage::from_pixel(300, 400, Luma([WHITE])));
        assert!(flatten(&scan).is_none());
    }

    #[test]
    fn flattens_a_page_photographed_at_an_angle() {
        let corners = [
            Point { x: 120.0, y: 60.0 },
            Point { x: 650.0, y: 110.0 },
            Point { x: 620.0, y: 740.0 },
            Point { x: 90.0, y: 700.0 },
        ];
        let photo = GrayImage::from_fn(760, 800, |x, y| {
            let inside = (0..4).all(|i| {
                let (a, b) = (corners[i], corners[(i + 1) % 4]);
                (b.x - a.x) * (f64::from(y) - a.y) - (b.y - a.y) * (f64::from(x) - a.x) >= 0.0
            });
            Luma([if inside { 235 } else { 40 }])
        });
        let flat = flatten(&Bitmap::Gray(photo)).expect("a page to flatten");
        let (width, height) = flat.dimensions();
        assert!((525..=540).contains(&width), "{width}");
        assert!((635..=650).contains(&height), "{height}");
        assert_eq!(flat.get_pixel(width / 2, height / 2).0[0], 235);
    }
}
