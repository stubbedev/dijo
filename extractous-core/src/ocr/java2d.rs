//! Java2D's drawing of an image through an affine transform with interpolation, as
//! `DrawImage.renderImageXform` and the native `TransformHelper` draw it: the destination pixels
//! the transformed source covers, each mapped back to the source in 32.32 fixed point and
//! interpolated from its neighbours, edges repeated, bilinearly or bicubically, in premultiplied
//! ARGB with Java2D's integer arithmetic.

/// An affine transform as `java.awt.geom.AffineTransform` holds it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Affine {
    m00: f64,
    m10: f64,
    m01: f64,
    m11: f64,
    m02: f64,
    m12: f64,
}

impl Affine {
    /// `new AffineTransform()` scaled by `scale(sx, sy)`, and its `createInverse`.
    pub(crate) fn scale(sx: f64, sy: f64) -> (Self, Self) {
        let forward = Self {
            m00: sx,
            m10: 0.0,
            m01: 0.0,
            m11: sy,
            m02: 0.0,
            m12: 0.0,
        };
        let inverse = Self {
            m00: 1.0 / sx,
            m11: 1.0 / sy,
            ..forward
        };
        (forward, inverse)
    }

    /// `translate(tx, ty)`, `rotate(theta)` and `translate(ux, uy)` of a new transform, as
    /// `OcrImages.rotate` builds it, and its `createInverse`.
    pub(crate) fn rotation(tx: f64, ty: f64, theta: f64, ux: f64, uy: f64) -> (Self, Self) {
        let (sin, cos) = (theta.sin(), theta.cos());
        let (m0, m1) = (1.0, 0.0);
        let m00 = cos * m0 + sin * m1;
        let m01 = -sin * m0 + cos * m1;
        let (m0, m1) = (0.0, 1.0);
        let m10 = cos * m0 + sin * m1;
        let m11 = -sin * m0 + cos * m1;
        let m02 = ux * m00 + uy * m01 + tx;
        let m12 = ux * m10 + uy * m11 + ty;
        let forward = Self {
            m00,
            m10,
            m01,
            m11,
            m02,
            m12,
        };
        let det = m00 * m11 - m01 * m10;
        let inverse = Self {
            m00: m11 / det,
            m10: -m10 / det,
            m01: -m01 / det,
            m11: m00 / det,
            m02: (m01 * m12 - m11 * m02) / det,
            m12: (m10 * m02 - m00 * m12) / det,
        };
        (forward, inverse)
    }

    fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.m00 * x + self.m01 * y + self.m02,
            self.m10 * x + self.m11 * y + self.m12,
        )
    }
}

/// The interpolations of `AffineTransformOp` Java2D draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interpolation {
    Bilinear,
    Bicubic,
}

const HALF: i64 = 1 << 31;

/// `DblToLong`: a coordinate in 32.32 fixed point, truncated.
fn fixed(value: f64) -> i64 {
    (value * 4_294_967_296.0) as i64
}

/// `WholeOfLong`.
fn whole(value: i64) -> i32 {
    (value >> 32) as i32
}

/// The eight bits of a fixed-point coordinate's fraction the interpolation weighs by.
fn fraction(value: i64) -> usize {
    ((value as u32) >> 24) as usize
}

/// Draws a source of `width` by `height` pixels, which `pixel` gives as premultiplied ARGB,
/// through `forward`, whose inverse is `inverse`, onto a destination of `out_width` by
/// `out_height` pixels: hands `put` each destination pixel the source covers and its
/// interpolated premultiplied ARGB.
pub(crate) fn draw(
    (width, height): (u32, u32),
    pixel: impl Fn(u32, u32) -> [u8; 4],
    (forward, inverse): (Affine, Affine),
    (out_width, out_height): (u32, u32),
    interpolation: Interpolation,
    mut put: impl FnMut(u32, u32, [u8; 4]),
) {
    let (w, h) = (f64::from(width), f64::from(height));
    let corners = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].map(|(x, y)| forward.apply(x, y));
    let (mut min_x, mut min_y) = corners[0];
    let (mut max_x, mut max_y) = corners[0];
    for &(x, y) in &corners[1..] {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let x1 = (min_x.floor() as i32).max(0);
    let y1 = (min_y.floor() as i32).max(0);
    let x2 = (max_x.ceil() as i32).min(out_width as i32);
    let y2 = (max_y.ceil() as i32).min(out_height as i32);
    if x2 <= x1 || y2 <= y1 {
        return;
    }
    let (origin_x, origin_y) = inverse.apply(f64::from(x1) + 0.5, f64::from(y1) + 0.5);
    let (dxdx, dydx) = (fixed(inverse.m00), fixed(inverse.m10));
    let (dxdy, dydy) = (fixed(inverse.m01), fixed(inverse.m11));
    let (x_base, y_base) = (fixed(origin_x), fixed(origin_y));
    let outside = |x: i64, y: i64| whole(y) as u32 >= height || whole(x) as u32 >= width;
    let coefficients = bicubic_coefficients();
    let last_column = i64::from(width) - 1;
    let last_row = i64::from(height) - 1;
    for row in 0..(y2 - y1) {
        let row_x = x_base.wrapping_add(i64::from(row).wrapping_mul(dxdy));
        let row_y = y_base.wrapping_add(i64::from(row).wrapping_mul(dydy));
        let at = |column: i32| {
            let step = i64::from(column - x1);
            (
                row_x.wrapping_add(step.wrapping_mul(dxdx)),
                row_y.wrapping_add(step.wrapping_mul(dydx)),
            )
        };
        let mut left = x1;
        while left < x2 && {
            let (x, y) = at(left);
            outside(x, y)
        } {
            left += 1;
        }
        let mut right = x2;
        while right > left && {
            let (x, y) = at(right - 1);
            outside(x, y)
        } {
            right -= 1;
        }
        for column in left..right {
            let (x, y) = at(column);
            let (x, y) = (x.wrapping_sub(HALF), y.wrapping_sub(HALF));
            let (x_whole, y_whole) = (i64::from(whole(x)), i64::from(whole(y)));
            let sample = |dx: i64, dy: i64| {
                pixel(
                    (x_whole + dx).clamp(0, last_column) as u32,
                    (y_whole + dy).clamp(0, last_row) as u32,
                )
            };
            let (fx, fy) = (fraction(x), fraction(y));
            let value = match interpolation {
                Interpolation::Bilinear => bilinear(
                    [sample(0, 0), sample(1, 0), sample(0, 1), sample(1, 1)],
                    fx as i32,
                    fy as i32,
                ),
                Interpolation::Bicubic => {
                    let x_weights = [fx + 256, fx, 256 - fx, 512 - fx].map(|i| coefficients[i]);
                    let y_weights = [fy + 256, fy, 256 - fy, 512 - fy].map(|i| coefficients[i]);
                    let mut accumulated = [1i32 << 15; 4];
                    for (dy, y_weight) in (-1..=2).zip(y_weights) {
                        for (dx, x_weight) in (-1..=2).zip(x_weights) {
                            let factor = x_weight * y_weight;
                            for (sum, channel) in accumulated.iter_mut().zip(sample(dx, dy)) {
                                *sum += i32::from(channel) * factor;
                            }
                        }
                    }
                    let [a, r, g, b] = accumulated.map(|sum| sum >> 16);
                    let a = a.clamp(0, 255);
                    [a, r.clamp(0, a), g.clamp(0, a), b.clamp(0, a)].map(|value| value as u8)
                }
            };
            put(column as u32, (y1 + row) as u32, value);
        }
    }
}

/// `BilinearInterp` of the four premultiplied pixels around a point, by its fractions.
fn bilinear(pixels: [[u8; 4]; 4], fx: i32, fy: i32) -> [u8; 4] {
    let lerp = |v1: i32, v2: i32, f: i32| (v1 << 8) + (v2 - v1) * f;
    std::array::from_fn(|channel| {
        let [c1, c2, c3, c4] = pixels.map(|pixel| i32::from(pixel[channel]));
        let top = lerp(c1, c2, fx);
        let bottom = lerp(c3, c4, fx);
        ((lerp(top, bottom, fy) + (1 << 15)) >> 16) as u8
    })
}

/// `init_bicubic_table(-0.5)`: Keys' kernel in steps of 1/256, scaled by 256, each run of four
/// weights adding up to 256.
fn bicubic_coefficients() -> [i32; 513] {
    const A: f64 = -0.5;
    let mut coefficients = [0i32; 513];
    for (i, coefficient) in coefficients.iter_mut().enumerate().take(256) {
        let x = i as f64 / 256.0;
        *coefficient = ((((A + 2.0) * x - (A + 3.0)) * x * x + 1.0) * 256.0) as i32;
    }
    for (i, coefficient) in coefficients.iter_mut().enumerate().take(384).skip(256) {
        let x = i as f64 / 256.0;
        *coefficient = ((((A * x - 5.0 * A) * x + 8.0 * A) * x - 4.0 * A) * 256.0) as i32;
    }
    coefficients[384] = (256 - coefficients[128] * 2) / 2;
    for i in 385..=512 {
        coefficients[i] =
            256 - (coefficients[512 - i] + coefficients[i - 256] + coefficients[768 - i]);
    }
    coefficients
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighs_each_run_of_four_to_one() {
        let coefficients = bicubic_coefficients();
        for f in 0..256 {
            let sum: i32 = [f + 256, f, 256 - f, 512 - f]
                .iter()
                .map(|&i| coefficients[i])
                .sum();
            assert_eq!(sum, 256, "{f}");
        }
    }

    #[test]
    fn scales_a_flat_image_flat() {
        let mut drawn = 0;
        draw(
            (4, 4),
            |_, _| [255, 100, 100, 100],
            Affine::scale(0.5, 0.5),
            (2, 2),
            Interpolation::Bilinear,
            |_, _, pixel| {
                assert_eq!(pixel, [255, 100, 100, 100]);
                drawn += 1;
            },
        );
        assert_eq!(drawn, 4);
    }
}
