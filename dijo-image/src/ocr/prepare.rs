//! Our Java `OcrImages`: corrections that make an image readable for OCR, judged on its grey
//! levels: the orientation recorded in its EXIF data, the small rotation of a page scanned or
//! photographed slightly askew, which breaks tesseract's layout analysis already at a few
//! degrees, text too small for tesseract, as in screenshots and low-resolution scans, light text
//! on a dark ground, specks, faint marks, and lighting too uneven for a single binarisation
//! threshold. Turning and rotating keep the colours of an image ([`Bitmap`]); the other
//! corrections make it grey.

use std::ops::Range;

use image::GrayImage;

use super::bitmap::Bitmap;

const ANALYSIS_SIZE: u32 = 1200;
const MAX_SKEW_DEGREES: f64 = 10.0;
const COARSE_STEP_DEGREES: f64 = 0.25;
const FINE_STEP_DEGREES: f64 = 0.05;
const MIN_CORRECTION_DEGREES: f64 = 0.3;
const MIN_SHARPNESS_GAIN: f64 = 1.05;
const MIN_LINE_PIXELS: usize = 20;
const MAX_LINE_PIXELS: usize = 40;
const TARGET_LINE_PIXELS: f64 = 32.0;
const MIN_MEASURED_LINES: usize = 3;
const MAX_LINE_SHARE_OF_HEIGHT: usize = 10;
const MAX_UPSCALE: f64 = 4.0;
const MIN_INK_RUN_PIXELS: usize = 3;
const LIGHTING_TILES: u32 = 8;
const BACKGROUND_PERCENTILE: f64 = 0.9;
const UNEVEN_BACKGROUND_LEVELS: u32 = 48;
const BACKGROUND_BLOCK_PIXELS: u32 = 16;
const BACKGROUND_CLOSING_BLOCKS: usize = 2;
const MIN_INVERTED_DARK_PERCENT: u64 = 85;
const MIN_SPECKLE_PERCENT: u64 = 2;
const INK_PERCENTILE: f64 = 0.005;
const FAINT_PERCENT: u32 = 72;
const WHITE: u8 = 255;

/// An EXIF orientation, 2 to 8, which turns or mirrors the image it is recorded in; 1 needs
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Orientation(u8);

impl Orientation {
    /// The orientation a TIFF or EXIF orientation value records, when it needs anything done.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::of(value.trim().parse().ok()?)
    }

    /// The orientation of the value 1 to 8, when it needs anything done.
    pub(crate) fn of(value: u32) -> Option<Self> {
        match value {
            2..=8 => Some(Self(value as u8)),
            _ => None,
        }
    }

    /// The orientation that takes an image turned by this one back: a quarter turn the other
    /// way for 6 and 8; the others undo themselves.
    pub(crate) fn undone(self) -> Self {
        match self.0 {
            6 => Self(8),
            8 => Self(6),
            value => Self(value),
        }
    }

    pub(crate) fn value(self) -> u8 {
        self.0
    }

    /// Whether the orientation swaps the image's width and height.
    pub(crate) fn swaps_sides(self) -> bool {
        matches!(self.0, 5..=8)
    }

    /// Where the pixel at `x`, `y` of an image of `width` by `height` pixels moves to.
    pub(crate) fn moved(self, x: u32, y: u32, width: u32, height: u32) -> (u32, u32) {
        match self.0 {
            2 => (width - 1 - x, y),
            3 => (width - 1 - x, height - 1 - y),
            4 => (x, height - 1 - y),
            5 => (y, x),
            6 => (height - 1 - y, x),
            7 => (height - 1 - y, width - 1 - x),
            8 => (y, width - 1 - x),
            _ => (x, y),
        }
    }

    /// The orientation that turns an image clockwise by quarter turns, when it turns it at all.
    pub(crate) fn clockwise(quarter_turns: u8) -> Option<Self> {
        match quarter_turns % 4 {
            1 => Some(Self(6)),
            2 => Some(Self(3)),
            3 => Some(Self(8)),
            _ => None,
        }
    }
}

/// The angle, in degrees, by which the lines of text in the image are rotated, or 0 when no
/// rotation clearly lines them up better. Found by projecting the dark pixels of a reduced copy
/// onto the vertical axis at candidate angles: text lines give the sharpest profile when level.
pub(crate) fn skew_degrees(image: &Bitmap) -> f64 {
    let analysis = analysis(image, 1);
    let width = analysis.width() as i64;
    let height = analysis.height() as i64;
    let levels = analysis.as_raw();
    let threshold = otsu_threshold(&histogram(levels));
    let points: Vec<(f64, f64)> = levels
        .iter()
        .enumerate()
        .filter(|(_, &level)| u32::from(level) < threshold)
        .map(|(index, _)| ((index as i64 % width) as f64, (index as i64 / width) as f64))
        .collect();
    if points.is_empty() || points.len() > levels.len() / 2 {
        return 0.0;
    }
    let diagonal = (width as f64).hypot(height as f64).ceil() as i64;
    let mut rows = vec![0u64; (2 * diagonal + 1) as usize];
    let level = sharpness(&points, 0.0, diagonal, &mut rows);
    let mut best = 0.0;
    let mut best_sharpness = level;
    let mut angle = -MAX_SKEW_DEGREES;
    while angle <= MAX_SKEW_DEGREES {
        let sharpness = sharpness(&points, angle, diagonal, &mut rows);
        if sharpness > best_sharpness {
            best = angle;
            best_sharpness = sharpness;
        }
        angle += COARSE_STEP_DEGREES;
    }
    let coarse = best;
    let mut angle = coarse - COARSE_STEP_DEGREES;
    while angle <= coarse + COARSE_STEP_DEGREES {
        let sharpness = sharpness(&points, angle, diagonal, &mut rows);
        if sharpness > best_sharpness {
            best = angle;
            best_sharpness = sharpness;
        }
        angle += FINE_STEP_DEGREES;
    }
    if best.abs() < MIN_CORRECTION_DEGREES || best_sharpness < level * MIN_SHARPNESS_GAIN {
        return 0.0;
    }
    best
}

/// How sharp the profile of the points projected onto the vertical axis at an angle is: the sum
/// of the squared counts of its rows.
fn sharpness(points: &[(f64, f64)], degrees: f64, diagonal: i64, rows: &mut [u64]) -> f64 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    rows.fill(0);
    for &(x, y) in points {
        let row = (y * cos - x * sin).round() as i64 + diagonal;
        rows[row as usize] += 1;
    }
    rows.iter()
        .map(|&count| (count as f64) * (count as f64))
        .sum()
}

/// How much to resize the image so its text lines are about [`TARGET_LINE_PIXELS`] pixels high:
/// enlarged when they are below [`MIN_LINE_PIXELS`], since tesseract reads small text poorly,
/// but no further than `max_pixels` allows, and, when `reduce` allows it, reduced when they are
/// above [`MAX_LINE_PIXELS`], since tesseract reads such text as well and faster once it is
/// smaller. The line height is only trusted for reducing when at least [`MIN_MEASURED_LINES`]
/// lines were measured, each under a tenth of the image's height: on an inverted page or one
/// with a dark border, every row holds ink. It is only trusted for enlarging when most rows with
/// ink lie in runs that short: on a page set solid, whose lines touch, the text reads as a few
/// runs of hundreds of rows, and the specks and marks beside them as the shortest. 1 when the
/// image has no lines of text.
pub(crate) fn scale_factor(image: &Bitmap, max_pixels: u64, reduce: bool) -> f64 {
    let image = image.gray();
    let (width, height) = image.dimensions();
    let mut runs: Vec<usize> = ink_runs(&image).into_iter().map(|run| run.len()).collect();
    if runs.is_empty() {
        return 1.0;
    }
    runs.sort_unstable();
    let line_height = runs[runs.len() / 2];
    if line_height > MAX_LINE_PIXELS {
        let measured = runs.len() >= MIN_MEASURED_LINES
            && runs[runs.len() - 1] * MAX_LINE_SHARE_OF_HEIGHT < height as usize;
        return if reduce && measured {
            TARGET_LINE_PIXELS / line_height as f64
        } else {
            1.0
        };
    }
    let (short_rows, rows) = runs.iter().fold((0, 0), |(short, all), &run| {
        (
            short + if run < MIN_LINE_PIXELS { run } else { 0 },
            all + run,
        )
    });
    if line_height >= MIN_LINE_PIXELS || short_rows * 2 <= rows {
        return 1.0;
    }
    let wanted = MAX_UPSCALE.min(TARGET_LINE_PIXELS / line_height as f64);
    let allowed = (max_pixels as f64 / (u64::from(width) * u64::from(height)) as f64).sqrt();
    wanted.min(allowed).max(1.0)
}

pub(crate) fn ink_runs(image: &GrayImage) -> Vec<Range<usize>> {
    let levels = image.as_raw();
    let threshold = otsu_threshold(&histogram(levels));
    let inked: Vec<bool> = levels
        .chunks_exact(image.width() as usize)
        .map(|row| row.iter().any(|&level| u32::from(level) < threshold))
        .collect();
    let mut runs = Vec::new();
    let mut start = 0;
    for y in 0..=inked.len() {
        if y < inked.len() && inked[y] {
            continue;
        }
        if y - start >= MIN_INK_RUN_PIXELS {
            runs.push(start..y);
        }
        start = y + 1;
    }
    runs
}

/// Whether the image is lit unevenly, as a photographed page in shadow is: whether the
/// background brightness of its regions differs by [`UNEVEN_BACKGROUND_LEVELS`] grey levels or
/// more. The background of a region is the brightness its lightest tenth of pixels reaches.
pub(crate) fn uneven_lighting(image: &Bitmap) -> bool {
    let analysis = analysis(image, LIGHTING_TILES);
    let (width, height) = analysis.dimensions();
    let mut lightest = 0;
    let mut darkest = u32::from(WHITE);
    for tile_y in 0..LIGHTING_TILES {
        for tile_x in 0..LIGHTING_TILES {
            let mut histogram = [0u64; 256];
            let mut count = 0;
            for y in tile_y * height / LIGHTING_TILES..(tile_y + 1) * height / LIGHTING_TILES {
                for x in tile_x * width / LIGHTING_TILES..(tile_x + 1) * width / LIGHTING_TILES {
                    histogram[usize::from(analysis.get_pixel(x, y).0[0])] += 1;
                    count += 1;
                }
            }
            let background = percentile(&histogram, count, BACKGROUND_PERCENTILE);
            lightest = lightest.max(background);
            darkest = darkest.min(background);
        }
    }
    lightest - darkest >= UNEVEN_BACKGROUND_LEVELS
}

pub(crate) fn normalise_background(image: &Bitmap) -> GrayImage {
    let mut normalised = image.gray().into_owned();
    let (width, height) = normalised.dimensions();
    let background = resized(&block_background(&normalised), width, height);
    for (level, &paper) in normalised.iter_mut().zip(background.as_raw()) {
        let paper = u32::from(paper.max(1));
        *level = ((u32::from(*level) * u32::from(WHITE) + paper / 2) / paper).min(u32::from(WHITE))
            as u8;
    }
    normalised
}

fn block_background(image: &GrayImage) -> GrayImage {
    let (width, height) = image.dimensions();
    let blocks_wide = width.div_ceil(BACKGROUND_BLOCK_PIXELS);
    let blocks_high = height.div_ceil(BACKGROUND_BLOCK_PIXELS);
    let mut lightest = GrayImage::new(blocks_wide, blocks_high);
    let block_row_pixels = (width * BACKGROUND_BLOCK_PIXELS) as usize;
    for (blocks, rows) in lightest
        .chunks_exact_mut(blocks_wide as usize)
        .zip(image.chunks(block_row_pixels))
    {
        for row in rows.chunks_exact(width as usize) {
            for (block, pixels) in blocks
                .iter_mut()
                .zip(row.chunks(BACKGROUND_BLOCK_PIXELS as usize))
            {
                *block = pixels.iter().copied().fold(*block, u8::max);
            }
        }
    }
    let lightened = extremes(&lightest, BACKGROUND_CLOSING_BLOCKS, u8::max);
    extremes(&lightened, BACKGROUND_CLOSING_BLOCKS, u8::min)
}

fn extremes(image: &GrayImage, reach: usize, pick: fn(u8, u8) -> u8) -> GrayImage {
    let (width, height) = image.dimensions();
    let columns = width as usize;
    let rows = height as usize;
    let mut across = GrayImage::new(width, height);
    for (row, out) in image
        .chunks_exact(columns)
        .zip(across.chunks_exact_mut(columns))
    {
        for (x, level) in out.iter_mut().enumerate() {
            let window = &row[x.saturating_sub(reach)..(x + reach + 1).min(columns)];
            *level = window.iter().copied().fold(window[0], pick);
        }
    }
    let across = across.as_raw();
    let mut down = GrayImage::new(width, height);
    for (y, out) in down.chunks_exact_mut(columns).enumerate() {
        let window = y.saturating_sub(reach)..(y + reach + 1).min(rows);
        for (x, level) in out.iter_mut().enumerate() {
            *level = window
                .clone()
                .map(|row| across[row * columns + x])
                .fold(across[window.start * columns + x], pick);
        }
    }
    down
}

/// The level at or below which a share of the counted levels lies.
fn percentile(histogram: &[u64; 256], count: u64, share: f64) -> u32 {
    let wanted = (count as f64 * share).ceil() as u64;
    let mut seen = 0;
    for (level, &found) in histogram.iter().enumerate() {
        seen += found;
        if seen >= wanted {
            return level as u32;
        }
    }
    u32::from(WHITE)
}

/// The image, in grey, resized by a factor with bicubic interpolation (Keys' kernel with
/// a = -0.5, as Java2D interpolates), which keeps the strokes of letters sharp: each row is
/// resampled first, then each column of the result, the pixels beyond the edges taken as the
/// edge's.
pub(crate) fn scale(image: &Bitmap, factor: f64) -> GrayImage {
    let image = image.gray();
    let (width, height) = image.dimensions();
    let scaled_width = ((f64::from(width) * factor).round() as u32).max(1);
    let scaled_height = ((f64::from(height) * factor).round() as u32).max(1);
    resized(&image, scaled_width, scaled_height)
}

fn resized(image: &GrayImage, scaled_width: u32, scaled_height: u32) -> GrayImage {
    let (width, height) = image.dimensions();
    let columns = Taps::bicubic(width, scaled_width);
    let rows = Taps::bicubic(height, scaled_height);
    let levels = image.as_raw();
    let mut widened = vec![0f32; scaled_width as usize * height as usize];
    for (row, out) in levels
        .chunks_exact(width as usize)
        .zip(widened.chunks_exact_mut(scaled_width as usize))
    {
        for (taps, out) in columns.iter().zip(out.iter_mut()) {
            *out = taps
                .sources
                .iter()
                .zip(taps.weights)
                .map(|(&source, weight)| weight * f32::from(row[source]))
                .sum();
        }
    }
    let mut scaled = GrayImage::new(scaled_width, scaled_height);
    let mut sums = vec![0f32; scaled_width as usize];
    for (taps, out) in rows
        .iter()
        .zip(scaled.chunks_exact_mut(scaled_width as usize))
    {
        sums.fill(0.0);
        for (&source, weight) in taps.sources.iter().zip(taps.weights) {
            let row = &widened[source * scaled_width as usize..][..scaled_width as usize];
            for (sum, &value) in sums.iter_mut().zip(row) {
                *sum += weight * value;
            }
        }
        for (pixel, sum) in out.iter_mut().zip(&sums) {
            *pixel = sum.round().clamp(0.0, 255.0) as u8;
        }
    }
    scaled
}

/// The four source pixels a resampled pixel is interpolated from, and their weights.
struct Taps {
    sources: [usize; 4],
    weights: [f32; 4],
}

impl Taps {
    const A: f64 = -0.5;

    /// The taps of each of `resized` pixels over `original` pixels.
    fn bicubic(original: u32, resized: u32) -> Vec<Taps> {
        let step = f64::from(original) / f64::from(resized);
        (0..resized)
            .map(|index| {
                let centre = (f64::from(index) + 0.5) * step - 0.5;
                let first = centre.floor() as i64 - 1;
                let mut sources = [0; 4];
                let mut weights = [0.0; 4];
                for tap in 0..4 {
                    let source = first + tap as i64;
                    sources[tap] = source.clamp(0, i64::from(original) - 1) as usize;
                    weights[tap] = Self::keys(centre - source as f64) as f32;
                }
                Taps { sources, weights }
            })
            .collect()
    }

    fn keys(distance: f64) -> f64 {
        let x = distance.abs();
        if x <= 1.0 {
            return ((Self::A + 2.0) * x - (Self::A + 3.0)) * x * x + 1.0;
        }
        if x < 2.0 {
            return ((Self::A * x - 5.0 * Self::A) * x + 8.0 * Self::A) * x - 4.0 * Self::A;
        }
        0.0
    }
}

/// The image in grey, reduced to at most [`ANALYSIS_SIZE`] pixels on its longer side, and to no
/// fewer than `min_side` on either, sampled bilinearly as Java2D draws it.
fn analysis(image: &Bitmap, min_side: u32) -> GrayImage {
    let (width, height) = image.dimensions();
    let scale = 1.0_f64.min(f64::from(ANALYSIS_SIZE) / f64::from(width.max(height)));
    let to_width = ((f64::from(width) * scale).round() as u32).max(min_side);
    let to_height = ((f64::from(height) * scale).round() as u32).max(min_side);
    image.gray_resized(to_width, to_height)
}

/// Whether the image is light text on a dark ground, as a negative or a slide is: whether more
/// than [`MIN_INVERTED_DARK_PERCENT`]% of it is darker than its Otsu threshold. Pages
/// photographed on a dark table or lying in shadow are dark in part, up to about 70%.
pub(crate) fn inverted(image: &Bitmap) -> bool {
    let analysis = analysis(image, 1);
    let levels = analysis.as_raw();
    let threshold = otsu_threshold(&histogram(levels));
    let dark = levels
        .iter()
        .filter(|&&level| u32::from(level) < threshold)
        .count() as u64;
    dark * 100 > levels.len() as u64 * MIN_INVERTED_DARK_PERCENT
}

/// The image in grey with every level inverted, dark for light.
pub(crate) fn invert(image: &Bitmap) -> GrayImage {
    let mut image = image.gray().into_owned();
    for level in image.iter_mut() {
        *level = !*level;
    }
    image
}

/// Whether the image is speckled, as a fax or a noisy scan is: whether more than
/// [`MIN_SPECKLE_PERCENT`]% of its pixels darker than its Otsu threshold have no dark
/// neighbour. Clean scans down to 72 dpi have under 0.3% such pixels, and a fax-like scan 30% or
/// more, whose specks tesseract's Latin model reads as diacritics.
pub(crate) fn speckled(image: &Bitmap) -> bool {
    let image = image.gray();
    let (width, height) = image.dimensions();
    if width < 3 || height < 3 {
        return false;
    }
    let levels = image.as_raw();
    let threshold = otsu_threshold(&histogram(levels));
    let dark = |level: u8| u32::from(level) < threshold;
    let width = width as usize;
    let mut dark_pixels: u64 = 0;
    let mut isolated: u64 = 0;
    for y in 1..height as usize - 1 {
        let above = &levels[(y - 1) * width..][..width];
        let middle = &levels[y * width..][..width];
        let below = &levels[(y + 1) * width..][..width];
        for x in 1..width - 1 {
            if !dark(middle[x]) {
                continue;
            }
            dark_pixels += 1;
            let neighbours = above[x - 1..=x + 1]
                .iter()
                .chain(&below[x - 1..=x + 1])
                .chain([&middle[x - 1], &middle[x + 1]]);
            if !neighbours.into_iter().any(|&level| dark(level)) {
                isolated += 1;
            }
        }
    }
    isolated * 100 > dark_pixels * MIN_SPECKLE_PERCENT
}

/// The image in grey with each pixel the median of its 3×3 neighbourhood, edges repeated, which
/// removes isolated specks and keeps strokes two pixels wide or more.
pub(crate) fn despeckle(image: &Bitmap) -> GrayImage {
    let image = image.gray();
    let (width, height) = image.dimensions();
    let levels = image.as_raw();
    let row_of = |y: i64| {
        let y = y.clamp(0, i64::from(height) - 1) as usize;
        &levels[y * width as usize..][..width as usize]
    };
    let mut despeckled = GrayImage::new(width, height);
    for (y, out) in despeckled.chunks_exact_mut(width as usize).enumerate() {
        let rows = [row_of(y as i64 - 1), row_of(y as i64), row_of(y as i64 + 1)];
        for (x, pixel) in out.iter_mut().enumerate() {
            let mut window = [0u8; 9];
            for (index, row) in rows.iter().enumerate() {
                for dx in 0..3 {
                    let column = (x as i64 + dx - 1).clamp(0, i64::from(width) - 1) as usize;
                    window[index * 3 + dx as usize] = row[column];
                }
            }
            window.sort_unstable();
            *pixel = window[4];
        }
    }
    despeckled
}

/// The image in grey with every pixel lighter than [`FAINT_PERCENT`]% of the way from its ink to
/// its paper made white: faint marks, such as text showing through from the back of the page,
/// that tesseract's single threshold takes for ink between the words. Its ink is its darkest
/// half percent and its paper its 90th percentile.
pub(crate) fn whiten_faint_marks(image: &Bitmap) -> GrayImage {
    let analysis = analysis(image, 1);
    let levels = analysis.as_raw();
    let histogram = histogram(levels);
    let count = levels.len() as u64;
    let ink = percentile(&histogram, count, INK_PERCENTILE);
    let paper = percentile(&histogram, count, BACKGROUND_PERCENTILE);
    let faint = (ink as i64 + (paper as i64 - ink as i64) * i64::from(FAINT_PERCENT) / 100) as u32;
    let mut image = image.gray().into_owned();
    for level in image.iter_mut() {
        if u32::from(*level) >= faint {
            *level = WHITE;
        }
    }
    image
}

/// The count of each grey level.
pub(crate) fn histogram(levels: &[u8]) -> [u64; 256] {
    let mut histogram = [0u64; 256];
    for &level in levels {
        histogram[usize::from(level)] += 1;
    }
    histogram
}

/// Otsu's threshold of a histogram of grey levels: the levels below it are dark.
pub(crate) fn otsu_threshold(histogram: &[u64; 256]) -> u32 {
    let total: u64 = histogram.iter().sum();
    let sum_all: f64 = histogram
        .iter()
        .enumerate()
        .map(|(level, &count)| level as f64 * count as f64)
        .sum();
    let mut sum_background = 0.0;
    let mut weight_background: u64 = 0;
    let mut best_variance = -1.0;
    let mut threshold = 128;
    for (level, &count) in histogram.iter().enumerate() {
        weight_background += count;
        if weight_background == 0 {
            continue;
        }
        let weight_foreground = total - weight_background;
        if weight_foreground == 0 {
            break;
        }
        sum_background += level as f64 * count as f64;
        let mean_background = sum_background / weight_background as f64;
        let mean_foreground = (sum_all - sum_background) / weight_foreground as f64;
        let variance = weight_background as f64
            * weight_foreground as f64
            * (mean_background - mean_foreground)
            * (mean_background - mean_foreground);
        if variance > best_variance {
            best_variance = variance;
            threshold = level as u32 + 1;
        }
    }
    threshold
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use image::Luma;

    const SHADOWED_PAPER: u8 = 60;

    pub(crate) fn shadowed_page(width: u32, height: u32) -> Bitmap {
        Bitmap::Gray(GrayImage::from_fn(width, height, |x, y| {
            let inked = y % 40 < 6 && (50..width - 50).contains(&x) && x % 30 < 24;
            let paper = if x * 5 > width * 3 {
                SHADOWED_PAPER
            } else {
                WHITE
            };
            Luma([if inked { 0 } else { paper }])
        }))
    }

    #[test]
    fn normalises_the_background_of_a_page_in_shadow() {
        let page = shadowed_page(800, 600);
        assert!(uneven_lighting(&page));
        let normalised = normalise_background(&page);
        let paper_in_shadow = normalised.get_pixel(700, 20).0[0];
        let ink_in_shadow = normalised.get_pixel(700, 2).0[0];
        assert!(paper_in_shadow >= 250, "{paper_in_shadow}");
        assert_eq!(ink_in_shadow, 0);
        assert!(!uneven_lighting(&Bitmap::Gray(normalised)));
    }

    #[test]
    fn keeps_an_evenly_lit_page_as_it_is() {
        let page = rows(800, 400, &[(50, 10), (90, 10), (130, 10)]);
        assert_eq!(normalise_background(&page), *page.gray());
    }

    #[test]
    fn thresholds_as_otsu() {
        let mut histogram = [0u64; 256];
        histogram[20] = 50;
        histogram[200] = 50;
        assert_eq!(otsu_threshold(&histogram), 21);
    }

    #[test]
    fn scales_with_keys_kernel() {
        let image = GrayImage::from_pixel(4, 4, Luma([100]));
        let scaled = scale(&Bitmap::Gray(image), 2.5);
        assert_eq!(scaled.dimensions(), (10, 10));
        assert!(scaled.pixels().all(|pixel| pixel.0[0] == 100));
    }

    /// A white page with dark rows from each `(top, height)` across its width.
    fn rows(width: u32, height: u32, inked: &[(u32, u32)]) -> Bitmap {
        let mut image = GrayImage::from_pixel(width, height, Luma([WHITE]));
        for &(top, rows) in inked {
            for y in top..top + rows {
                for x in 100..width - 100 {
                    image.put_pixel(x, y, Luma([0]));
                }
            }
        }
        Bitmap::Gray(image)
    }

    #[test]
    fn enlarges_lines_of_small_text() {
        let lines: Vec<(u32, u32)> = (0..20).map(|line| (50 + line * 16, 10)).collect();
        let factor = scale_factor(&rows(800, 400, &lines), 60_000_000, true);
        assert!((factor - 3.2).abs() < 1e-9, "{factor}");
    }

    #[test]
    fn keeps_solid_text_beside_specks() {
        let inked = [
            (60, 4),
            (90, 5),
            (130, 7),
            (170, 9),
            (300, 900),
            (1250, 1000),
        ];
        assert_eq!(
            scale_factor(&rows(1600, 2400, &inked), 60_000_000, true),
            1.0
        );
    }

    #[test]
    fn finds_the_skew_of_lines() {
        let mut image = GrayImage::from_pixel(800, 600, Luma([WHITE]));
        for line in 0..12 {
            for x in 50..750u32 {
                let y = 40 + line * 45 + (f64::from(x) * 3f64.to_radians().tan()) as u32;
                for thickness in 0..4 {
                    image.put_pixel(x, y + thickness, Luma([0]));
                }
            }
        }
        let image = Bitmap::Gray(image);
        let skew = skew_degrees(&image);
        assert!((skew - 3.0).abs() < 0.2, "{skew}");
        let level = image.rotated(skew);
        assert!(skew_degrees(&level).abs() < MIN_CORRECTION_DEGREES);
    }
}
