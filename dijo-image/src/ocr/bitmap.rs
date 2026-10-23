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

const CENTIMETRES_PER_INCH: f64 = 2.54;

/// How many pixels an image holds to the inch across and down, as its file records it or as it
/// was rendered. Tesseract's layout analysis measures by it, the gutter between columns among
/// all, and guesses it from the text where it is not told: on a page set in two columns it
/// guessed 413 dpi for 300, took the gutter for a word space and ran the columns together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Density {
    across: f64,
    down: f64,
}

impl Density {
    /// The density of `across` and `down` pixels to the inch, none unless both are positive.
    pub fn per_inch(across: f64, down: f64) -> Option<Self> {
        let positive = |value: f64| value.is_finite() && value > 0.0;
        (positive(across) && positive(down)).then_some(Self { across, down })
    }

    pub(crate) fn per_centimetre(across: f64, down: f64) -> Option<Self> {
        Self::per_inch(across * CENTIMETRES_PER_INCH, down * CENTIMETRES_PER_INCH)
    }

    /// The density of the image turned on its side when `sideways`, then scaled by `scale`.
    pub(crate) fn transformed(self, sideways: bool, scale: f64) -> Self {
        let (across, down) = match sideways {
            true => (self.down, self.across),
            false => (self.across, self.down),
        };
        Self {
            across: across * scale,
            down: down * scale,
        }
    }

    /// The density as tesseract takes it: the pixels to the inch down, whole.
    pub(crate) fn dpi(self) -> i32 {
        self.down.round() as i32
    }
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
            Self::Rgb(image) => Cow::Owned(mapped(image, |pixel| {
                Luma([luma(pixel[0], pixel[1], pixel[2])])
            })),
            Self::Rgba(image) => Cow::Owned(mapped(image, |pixel| {
                Luma([mul8(pixel[3], luma(pixel[0], pixel[1], pixel[2]))])
            })),
        }
    }

    /// [`Bitmap::gray`], a grey image taken as it is.
    pub(crate) fn into_gray(self) -> GrayImage {
        match self {
            Self::Gray(image) => image,
            other => other.gray().into_owned(),
        }
    }

    /// The image drawn through `transform` as [`java2d::draw`] draws it, each pixel read as Java2D
    /// reads it to interpolate: alpha, red, green and blue, the colours premultiplied by the
    /// alpha. The kind of image is matched once rather than for each of the pixels an
    /// interpolated one is drawn from.
    fn draw(
        &self,
        transform: (Affine, Affine),
        size: (u32, u32),
        interpolation: Interpolation,
        put: impl FnMut(u32, u32, [u8; 4]),
    ) {
        let dimensions = self.dimensions();
        let row = dimensions.0 as usize;
        let at = move |x: u32, y: u32| y as usize * row + x as usize;
        match self {
            Self::Gray(image) => {
                let levels = image.as_raw();
                let pixel = |x, y| {
                    let level = levels[at(x, y)];
                    [255, level, level, level]
                };
                java2d::draw(dimensions, pixel, transform, size, interpolation, put);
            }
            Self::Rgb(image) => {
                let colours = image.as_raw();
                let pixel = |x, y| {
                    let start = at(x, y) * 3;
                    [255, colours[start], colours[start + 1], colours[start + 2]]
                };
                java2d::draw(dimensions, pixel, transform, size, interpolation, put);
            }
            Self::Rgba(image) => {
                let colours = image.as_raw();
                let pixel = |x, y| {
                    let start = at(x, y) * 4;
                    let alpha = colours[start + 3];
                    [
                        alpha,
                        mul8(alpha, colours[start]),
                        mul8(alpha, colours[start + 1]),
                        mul8(alpha, colours[start + 2]),
                    ]
                };
                java2d::draw(dimensions, pixel, transform, size, interpolation, put);
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
        self.draw(
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
                Pixels::Encoded {
                    data: png.into(),
                    width: image.width(),
                    height: image.height(),
                }
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
        let size = (rotated_width, rotated_height);
        let over_white = |alpha: u8, colour: u8| colour + (255 - alpha);
        match self {
            Self::Gray(_) => {
                let mut rotated = GrayImage::from_pixel(rotated_width, rotated_height, Luma([255]));
                self.draw(
                    transform,
                    size,
                    Interpolation::Bicubic,
                    |x, y, [alpha, level, _, _]| {
                        rotated.put_pixel(x, y, Luma([over_white(alpha, level)]));
                    },
                );
                Self::Gray(rotated)
            }
            Self::Rgb(_) | Self::Rgba(_) => {
                let mut rotated =
                    RgbImage::from_pixel(rotated_width, rotated_height, Rgb([255, 255, 255]));
                self.draw(
                    transform,
                    size,
                    Interpolation::Bicubic,
                    |x, y, [alpha, red, green, blue]| {
                        rotated.put_pixel(
                            x,
                            y,
                            Rgb([
                                over_white(alpha, red),
                                over_white(alpha, green),
                                over_white(alpha, blue),
                            ]),
                        );
                    },
                );
                Self::Rgb(rotated)
            }
        }
    }
}

/// The samples of an image's pixels, without what its buffer may hold past them.
pub(crate) fn samples<P: Pixel>(image: &ImageBuffer<P, Vec<P::Subpixel>>) -> &[P::Subpixel] {
    let pixels = image.width() as usize * image.height() as usize;
    &image.as_raw()[..pixels * usize::from(P::CHANNEL_COUNT)]
}

/// An image of each pixel of `image` converted by `convert`, which is handed its samples.
fn mapped<P: Pixel<Subpixel = u8>, Q: Pixel<Subpixel = u8>>(
    image: &ImageBuffer<P, Vec<u8>>,
    convert: impl Fn(&[u8]) -> Q,
) -> ImageBuffer<Q, Vec<u8>> {
    let pixels = image.width() as usize * image.height() as usize;
    let mut converted = Vec::with_capacity(pixels * usize::from(Q::CHANNEL_COUNT));
    for pixel in samples(image).chunks_exact(usize::from(P::CHANNEL_COUNT)) {
        converted.extend_from_slice(convert(pixel).channels());
    }
    ImageBuffer::from_raw(image.width(), image.height(), converted)
        .expect("a converted pixel for each pixel")
}

/// The colours of an image laid over black by their alpha, as Java2D draws them into an
/// opaque image that starts black.
fn over_black(image: &RgbaImage) -> RgbImage {
    mapped(image, |pixel| {
        let alpha = pixel[3];
        Rgb([
            mul8(alpha, pixel[0]),
            mul8(alpha, pixel[1]),
            mul8(alpha, pixel[2]),
        ])
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
    if width == 0 || height == 0 {
        return ImageBuffer::new(out_width, out_height);
    }
    let channels = usize::from(P::CHANNEL_COUNT);
    let (width, height) = (width as usize, height as usize);
    let source = samples(image);
    let mut turned = vec![0; source.len()];
    let row_length = width * channels;
    let rows = source.chunks_exact(row_length).enumerate();
    match orientation.value() {
        5..=8 => {
            let mirrored_across = matches!(orientation.value(), 6 | 7);
            let mirrored_down = matches!(orientation.value(), 7 | 8);
            for (y, row) in rows {
                let to_x = if mirrored_across { height - 1 - y } else { y };
                for (x, pixel) in row.chunks_exact(channels).enumerate() {
                    let to_y = if mirrored_down { width - 1 - x } else { x };
                    let at = (to_y * height + to_x) * channels;
                    turned[at..at + channels].copy_from_slice(pixel);
                }
            }
        }
        value => {
            let mirrored_across = matches!(value, 2 | 3);
            let mirrored_down = matches!(value, 3 | 4);
            for (y, row) in rows {
                let to_y = if mirrored_down { height - 1 - y } else { y };
                let to = &mut turned[to_y * row_length..][..row_length];
                if mirrored_across {
                    let pixels = row.chunks_exact(channels).rev();
                    for (to, pixel) in to.chunks_exact_mut(channels).zip(pixels) {
                        to.copy_from_slice(pixel);
                    }
                } else {
                    to.copy_from_slice(row);
                }
            }
        }
    }
    ImageBuffer::from_raw(out_width, out_height, turned).expect("a pixel for each pixel")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(width: u32, height: u32) -> Bitmap {
        Bitmap::Gray(GrayImage::from_fn(width, height, |x, y| {
            Luma([(y * width + x) as u8])
        }))
    }

    /// A pixel as Java2D reads it to interpolate, read with `get_pixel`.
    fn read_premultiplied(image: &Bitmap, x: u32, y: u32) -> [u8; 4] {
        match image {
            Bitmap::Gray(image) => {
                let level = image.get_pixel(x, y).0[0];
                [255, level, level, level]
            }
            Bitmap::Rgb(image) => {
                let [red, green, blue] = image.get_pixel(x, y).0;
                [255, red, green, blue]
            }
            Bitmap::Rgba(image) => {
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

    #[test]
    fn draws_each_kind_of_image_with_the_pixels_get_pixel_reads() {
        let (width, height) = (37, 23);
        let images = [
            Bitmap::Gray(GrayImage::from_fn(width, height, |x, y| {
                Luma([byte(x, y, 0)])
            })),
            Bitmap::Rgb(RgbImage::from_fn(width, height, |x, y| {
                Rgb([byte(x, y, 0), byte(x, y, 1), byte(x, y, 2)])
            })),
            Bitmap::Rgba(RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([byte(x, y, 0), byte(x, y, 1), byte(x, y, 2), byte(x, y, 3)])
            })),
        ];
        let transforms = [
            (
                Affine::rotation(30.0, 25.0, 0.3, -18.5, -11.5),
                (61, 50),
                Interpolation::Bicubic,
            ),
            (Affine::scale(1.7, 0.6), (63, 14), Interpolation::Bilinear),
        ];
        for image in &images {
            for &(transform, size, interpolation) in &transforms {
                let mut drawn = Vec::new();
                image.draw(transform, size, interpolation, |x, y, pixel| {
                    drawn.push((x, y, pixel));
                });
                let mut expected = Vec::new();
                java2d::draw(
                    image.dimensions(),
                    |x, y| read_premultiplied(image, x, y),
                    transform,
                    size,
                    interpolation,
                    |x, y, pixel| expected.push((x, y, pixel)),
                );
                assert!(!expected.is_empty());
                assert_eq!(drawn, expected);
            }
        }
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

    fn byte(x: u32, y: u32, channel: u32) -> u8 {
        ((x * 7 + y * 13 + channel * 101) % 256) as u8
    }

    fn moved_pixel_by_pixel<P: Pixel<Subpixel = u8>>(
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
            let (to_x, to_y) = match orientation.value() {
                2 => (width - 1 - x, y),
                3 => (width - 1 - x, height - 1 - y),
                4 => (x, height - 1 - y),
                5 => (y, x),
                6 => (height - 1 - y, x),
                7 => (height - 1 - y, width - 1 - x),
                8 => (y, width - 1 - x),
                _ => (x, y),
            };
            turned.put_pixel(to_x, to_y, *pixel);
        }
        turned
    }

    #[test]
    fn converts_and_orients_as_pixel_by_pixel() {
        let sizes = [
            (0, 0),
            (0, 3),
            (4, 0),
            (1, 1),
            (1, 5),
            (6, 1),
            (2, 3),
            (37, 23),
        ];
        for (width, height) in sizes {
            let gray = GrayImage::from_fn(width, height, |x, y| Luma([byte(x, y, 0)]));
            let rgb = RgbImage::from_fn(width, height, |x, y| {
                Rgb([byte(x, y, 0), byte(x, y, 1), byte(x, y, 2)])
            });
            let rgba = RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([byte(x, y, 0), byte(x, y, 1), byte(x, y, 2), byte(x, y, 3)])
            });
            let rgb_gray = GrayImage::from_fn(width, height, |x, y| {
                let [red, green, blue] = rgb.get_pixel(x, y).0;
                Luma([luma(red, green, blue)])
            });
            let rgba_gray = GrayImage::from_fn(width, height, |x, y| {
                let [red, green, blue, alpha] = rgba.get_pixel(x, y).0;
                Luma([mul8(alpha, luma(red, green, blue))])
            });
            let black = RgbImage::from_fn(width, height, |x, y| {
                let [red, green, blue, alpha] = rgba.get_pixel(x, y).0;
                Rgb([mul8(alpha, red), mul8(alpha, green), mul8(alpha, blue)])
            });
            assert_eq!(*Bitmap::Rgb(rgb.clone()).gray(), rgb_gray);
            assert_eq!(*Bitmap::Rgba(rgba.clone()).gray(), rgba_gray);
            assert_eq!(over_black(&rgba), black);
            for value in 2..=8 {
                let orientation = Orientation::of(value).unwrap();
                let size = (width, height);
                assert_eq!(
                    oriented(&gray, orientation),
                    moved_pixel_by_pixel(&gray, orientation),
                    "{size:?} {value}"
                );
                assert_eq!(
                    oriented(&rgb, orientation),
                    moved_pixel_by_pixel(&rgb, orientation),
                    "{size:?} {value}"
                );
                assert_eq!(
                    oriented(&rgba, orientation),
                    moved_pixel_by_pixel(&rgba, orientation),
                    "{size:?} {value}"
                );
            }
        }
    }

    #[test]
    fn takes_the_samples_of_the_pixels_alone() {
        let mut buffer: Vec<u8> = (0..20).collect();
        let image = GrayImage::from_raw(3, 2, buffer.clone()).unwrap();
        buffer.truncate(6);
        assert_eq!(samples(&image), buffer);
        let rgb = RgbImage::from_raw(2, 2, (0..13).collect()).unwrap();
        assert_eq!(samples(&rgb), &(0..12).collect::<Vec<u8>>()[..]);
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

    #[test]
    fn a_density_turns_and_scales_with_its_image() {
        let fax = Density::per_inch(204.0, 98.0).unwrap();
        assert_eq!(fax.dpi(), 98);
        assert_eq!(fax.transformed(true, 1.0).dpi(), 204);
        assert_eq!(fax.transformed(false, 0.5).dpi(), 49);
        assert_eq!(Density::per_centimetre(118.11, 118.11).unwrap().dpi(), 300);
        assert_eq!(Density::per_inch(0.0, 300.0), None);
    }
}
