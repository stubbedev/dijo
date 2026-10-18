//! Images as our Java `PreparingOcrParser` holds them while it prepares them: in the kinds of
//! `BufferedImage` ImageIO and Java2D give it, grey, colour, or colour with alpha, and drawn as
//! Java2D draws them: into grey, at another size, turned and rotated. Colour is kept as long as
//! Java keeps it, since tesseract reads the colours of the image it is handed in its own grey.

use std::borrow::Cow;

use image::{GrayImage, ImageBuffer, ImageEncoder, Luma, Pixel, Rgb, RgbImage, RgbaImage};

use super::java2d::{self, Affine, Interpolation};
use super::prepare::Orientation;
use super::tesseract::Pixels;

/// An image as `PreparingOcrParser` prepares it.
#[derive(Debug, Clone)]
pub enum Bitmap {
    /// Grey levels, as a `TYPE_BYTE_GRAY` image holds them.
    Gray(GrayImage),
    /// Opaque colours.
    Rgb(RgbImage),
    /// Colours with an alpha, not premultiplied.
    Rgba(RgbaImage),
}

/// The grey level Java2D draws a colour as: ITU-R BT.601 luma, rounded.
pub fn luma(red: u8, green: u8, blue: u8) -> u8 {
    ((77 * u32::from(red) + 150 * u32::from(green) + 29 * u32::from(blue) + 128) >> 8) as u8
}

/// The colour of cyan, magenta, yellow and black ink, each from 0, none, to 255, printed
/// without a profile: the light each colour's ink and the black leave.
pub(crate) fn cmyk_rgb([cyan, magenta, yellow, black]: [u8; 4]) -> [u8; 3] {
    let left = |ink: u8| ((255 - u32::from(ink)) * (255 - u32::from(black)) / 255) as u8;
    [left(cyan), left(magenta), left(yellow)]
}

/// `mul8table` of Java2D's AlphaMath: `a × b ÷ 255`, rounded as its table rounds it.
pub(crate) fn mul8(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    ((u32::from(a) * 0x10101 * u32::from(b) + (1 << 23)) >> 24) as u8
}

impl Bitmap {
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Gray(image) => image.dimensions(),
            Self::Rgb(image) => image.dimensions(),
            Self::Rgba(image) => image.dimensions(),
        }
    }

    /// `OcrImages.gray` at the image's own size: the image drawn into a new grey image, which
    /// starts black, each colour's luma laid over black by its alpha.
    pub(crate) fn gray(&self) -> Cow<'_, GrayImage> {
        match self {
            Self::Gray(image) => Cow::Borrowed(image),
            Self::Rgb(image) => {
                Cow::Owned(GrayImage::from_fn(image.width(), image.height(), |x, y| {
                    let [red, green, blue] = image.get_pixel(x, y).0;
                    Luma([luma(red, green, blue)])
                }))
            }
            Self::Rgba(image) => {
                Cow::Owned(GrayImage::from_fn(image.width(), image.height(), |x, y| {
                    let [red, green, blue, alpha] = image.get_pixel(x, y).0;
                    Luma([mul8(alpha, luma(red, green, blue))])
                }))
            }
        }
    }

    /// A pixel as Java2D reads it to draw it with interpolation: alpha, red, green and blue,
    /// the colours premultiplied by the alpha.
    fn premultiplied(&self, x: u32, y: u32) -> [u8; 4] {
        match self {
            Self::Gray(image) => {
                let level = image.get_pixel(x, y).0[0];
                [255, level, level, level]
            }
            Self::Rgb(image) => {
                let [red, green, blue] = image.get_pixel(x, y).0;
                [255, red, green, blue]
            }
            Self::Rgba(image) => {
                let [red, green, blue, alpha] = image.get_pixel(x, y).0;
                [
                    alpha,
                    mul8(alpha, red),
                    mul8(alpha, green),
                    mul8(alpha, blue),
                ]
            }
        }
    }

    /// `OcrImages.gray` at another size: the image drawn with bilinear interpolation into a new
    /// grey image, as Java2D draws it.
    pub(crate) fn gray_resized(&self, width: u32, height: u32) -> GrayImage {
        if self.dimensions() == (width, height) {
            return self.gray().into_owned();
        }
        let (source_width, source_height) = self.dimensions();
        let mut gray = GrayImage::new(width, height);
        java2d::draw(
            (source_width, source_height),
            |x, y| self.premultiplied(x, y),
            Affine::scale(
                f64::from(width) / f64::from(source_width),
                f64::from(height) / f64::from(source_height),
            ),
            (width, height),
            Interpolation::Bilinear,
            |x, y, [_, red, green, blue]| gray.put_pixel(x, y, Luma([luma(red, green, blue)])),
        );
        gray
    }

    /// `OcrImages.applyOrientation`: the image drawn turned into a new colour image, which
    /// starts black; a grey one stays grey, its levels unchanged.
    pub(crate) fn oriented(&self, orientation: Orientation) -> Self {
        match self {
            Self::Gray(image) => Self::Gray(oriented(image, orientation)),
            Self::Rgb(image) => Self::Rgb(oriented(image, orientation)),
            Self::Rgba(image) => Self::Rgb(oriented(&over_black(image), orientation)),
        }
    }

    /// The image as tesseract reads the PNG Java writes of it: its grey levels or colours, and an
    /// image with alpha as a PNG, whose transparency tesseract lays over white.
    pub(crate) fn pixels(&self) -> Pixels<'_> {
        match self {
            Self::Gray(image) => Pixels::Gray {
                data: image.as_raw(),
                width: image.width(),
                height: image.height(),
            },
            Self::Rgb(image) => Pixels::Rgb {
                data: image.as_raw(),
                width: image.width(),
                height: image.height(),
            },
            Self::Rgba(image) => {
                let mut png = Vec::new();
                image::codecs::png::PngEncoder::new(&mut png)
                    .write_image(
                        image.as_raw(),
                        image.width(),
                        image.height(),
                        image::ExtendedColorType::Rgba8,
                    )
                    .expect("an image in memory encodes as PNG");
                Pixels::Encoded(png.into())
            }
        }
    }

    /// The image turned clockwise by a number of quarter turns, or none when it needs no turn.
    pub(crate) fn turned_clockwise(&self, quarter_turns: u8) -> Option<Self> {
        Orientation::clockwise(quarter_turns).map(|orientation| self.oriented(orientation))
    }

    /// `OcrImages.rotate`: the image rotated by -degrees about its centre, drawn with bicubic
    /// interpolation onto a white canvas that holds all of it, as Java2D draws it; a grey one
    /// stays grey.
    pub(crate) fn rotated(&self, degrees: f64) -> Self {
        let radians = (-degrees).to_radians();
        let (sin, cos) = (radians.sin().abs(), radians.cos().abs());
        let (width, height) = self.dimensions();
        let (w, h) = (f64::from(width), f64::from(height));
        let rotated_width = (w * cos + h * sin).ceil() as u32;
        let rotated_height = (h * cos + w * sin).ceil() as u32;
        let transform = Affine::rotation(
            f64::from(rotated_width) / 2.0,
            f64::from(rotated_height) / 2.0,
            radians,
            -w / 2.0,
            -h / 2.0,
        );
        let draw = |put: &mut dyn FnMut(u32, u32, [u8; 4])| {
            java2d::draw(
                (width, height),
                |x, y| self.premultiplied(x, y),
                transform,
                (rotated_width, rotated_height),
                Interpolation::Bicubic,
                put,
            );
        };
        let over_white = |alpha: u8, colour: u8| colour + (255 - alpha);
        match self {
            Self::Gray(_) => {
                let mut rotated = GrayImage::from_pixel(rotated_width, rotated_height, Luma([255]));
                draw(&mut |x, y, [alpha, level, _, _]| {
                    rotated.put_pixel(x, y, Luma([over_white(alpha, level)]));
                });
                Self::Gray(rotated)
            }
            Self::Rgb(_) | Self::Rgba(_) => {
                let mut rotated =
                    RgbImage::from_pixel(rotated_width, rotated_height, Rgb([255, 255, 255]));
                draw(&mut |x, y, [alpha, red, green, blue]| {
                    rotated.put_pixel(
                        x,
                        y,
                        Rgb([
                            over_white(alpha, red),
                            over_white(alpha, green),
                            over_white(alpha, blue),
                        ]),
                    );
                });
                Self::Rgb(rotated)
            }
        }
    }
}

/// The colours of an image laid over black by their alpha, as Java2D draws them into an
/// opaque image that starts black.
fn over_black(image: &RgbaImage) -> RgbImage {
    RgbImage::from_fn(image.width(), image.height(), |x, y| {
        let [red, green, blue, alpha] = image.get_pixel(x, y).0;
        Rgb([mul8(alpha, red), mul8(alpha, green), mul8(alpha, blue)])
    })
}

/// An image turned or mirrored by an EXIF orientation, its pixels moved as they are.
fn oriented<P: Pixel<Subpixel = u8>>(
    image: &ImageBuffer<P, Vec<u8>>,
    orientation: Orientation,
) -> ImageBuffer<P, Vec<u8>> {
    let (width, height) = image.dimensions();
    let (out_width, out_height) = if orientation.swaps_sides() {
        (height, width)
    } else {
        (width, height)
    };
    let mut turned = ImageBuffer::new(out_width, out_height);
    for (x, y, pixel) in image.enumerate_pixels() {
        let (to_x, to_y) = orientation.moved(x, y, width, height);
        turned.put_pixel(to_x, to_y, *pixel);
    }
    turned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(width: u32, height: u32) -> Bitmap {
        Bitmap::Gray(GrayImage::from_fn(width, height, |x, y| {
            Luma([(y * width + x) as u8])
        }))
    }

    fn rows(image: &Bitmap) -> Vec<Vec<u8>> {
        image
            .gray()
            .rows()
            .map(|row| row.map(|pixel| pixel.0[0]).collect())
            .collect()
    }

    #[test]
    fn turns_and_mirrors_as_exif_orientations_do() {
        let image = numbered(3, 2);
        let orientation = |value| Orientation::parse(value).unwrap();
        assert_eq!(rows(&image), [[0, 1, 2], [3, 4, 5]]);
        let expected: [(&str, &[&[u8]]); 7] = [
            ("2", &[&[2, 1, 0], &[5, 4, 3]]),
            ("3", &[&[5, 4, 3], &[2, 1, 0]]),
            ("4", &[&[3, 4, 5], &[0, 1, 2]]),
            ("5", &[&[0, 3], &[1, 4], &[2, 5]]),
            ("6", &[&[3, 0], &[4, 1], &[5, 2]]),
            ("7", &[&[5, 2], &[4, 1], &[3, 0]]),
            ("8", &[&[2, 5], &[1, 4], &[0, 3]]),
        ];
        for (value, turned) in expected {
            assert_eq!(rows(&image.oriented(orientation(value))), turned, "{value}");
        }
        assert_eq!(Orientation::parse(" 1 "), None);
        assert_eq!(Orientation::parse("9"), None);
        assert_eq!(Orientation::parse("x"), None);
        assert!(image.turned_clockwise(4).is_none());
    }

    #[test]
    fn takes_every_orientation_back() {
        let image = numbered(3, 2);
        for value in 2..=8 {
            let orientation = Orientation::of(value).unwrap();
            let back = image.oriented(orientation).oriented(orientation.undone());
            assert_eq!(rows(&back), rows(&image), "{value}");
        }
        assert_eq!(Orientation::of(1), None);
        assert_eq!(Orientation::of(9), None);
    }

    #[test]
    fn draws_colours_in_grey_as_java2d_does() {
        assert_eq!(luma(255, 255, 255), 255);
        assert_eq!(luma(0, 0, 0), 0);
        assert_eq!(luma(255, 0, 0), 77);
        assert_eq!(mul8(255, 200), 200);
        assert_eq!(mul8(128, 255), 128);
        assert_eq!(mul8(0, 255), 0);
    }

    #[test]
    fn turns_colours_over_black() {
        let image = Bitmap::Rgba(RgbaImage::from_pixel(
            2,
            1,
            image::Rgba([200, 100, 50, 128]),
        ));
        let Bitmap::Rgb(turned) = image.oriented(Orientation::parse("6").unwrap()) else {
            panic!("a colour image turns into colours");
        };
        assert_eq!(turned.dimensions(), (1, 2));
        assert_eq!(turned.get_pixel(0, 0).0, [100, 50, 25]);
    }
}
