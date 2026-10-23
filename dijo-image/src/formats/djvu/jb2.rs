//! JB2, DjVu's coding of bilevel images (DjVu v3 specification, Appendix 2), as our Java
//! `Jb2Decoder` decodes it: the mask of a page an Sjbz chunk holds, and the shapes a Djbz chunk
//! shares among the pages of a document. A JB2 stream is a sequence of records, every field of
//! them coded with the Z′ coder, that place symbols on a white image: coded directly, refined
//! from a shape in the library, or copied.

use std::rc::Rc;

use super::zp::ZpDecoder;
use super::{DjvuError, MAX_PIXELS};

const BIG_POSITIVE: i32 = 262_142;
const BIG_NEGATIVE: i32 = -262_143;

/// The record types (11.4).
const START_OF_DATA: i32 = 0;
const NEW_MARK: i32 = 1;
const NEW_MARK_LIBRARY_ONLY: i32 = 2;
const NEW_MARK_IMAGE_ONLY: i32 = 3;
const MATCHED_REFINE: i32 = 4;
const MATCHED_REFINE_LIBRARY_ONLY: i32 = 5;
const MATCHED_REFINE_IMAGE_ONLY: i32 = 6;
const MATCHED_COPY: i32 = 7;
const NON_MARK_DATA: i32 = 8;
const REQUIRED_DICT_OR_RESET: i32 = 9;
const PRESERVED_COMMENT: i32 = 10;
const END_OF_DATA: i32 = 11;

/// The bit contexts of the offset type and of the refinement flag.
const OFFSET_TYPE: usize = 0;
const REFINEMENT_FLAG: usize = 1;

/// A bilevel image, its rows top first, one byte per pixel: 1 for black, 0 for white.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Bitmap {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) pixels: Vec<u8>,
}

impl Bitmap {
    fn blank(width: usize, height: usize) -> Result<Self, DjvuError> {
        if (width as u64).saturating_mul(height as u64) > MAX_PIXELS {
            return Err(DjvuError::Malformed(format!(
                "A JB2 bitmap of {width}×{height} pixels exceeds {MAX_PIXELS}"
            )));
        }
        Ok(Self {
            width,
            height,
            pixels: vec![0; width * height],
        })
    }

    /// The bitmap without its white edge rows and columns.
    fn cropped(&self) -> Self {
        let mut left = self.width;
        let mut right = None;
        let mut top = self.height;
        let mut bottom = 0;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.pixels[y * self.width + x] != 0 {
                    left = left.min(x);
                    right = Some(right.map_or(x, |right: usize| right.max(x)));
                    top = top.min(y);
                    bottom = bottom.max(y);
                }
            }
        }
        let Some(right) = right else {
            return Self {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            };
        };
        let width = right - left + 1;
        let height = bottom - top + 1;
        let mut pixels = Vec::with_capacity(width * height);
        for y in top..=bottom {
            pixels.extend_from_slice(&self.pixels[y * self.width + left..][..width]);
        }
        Self {
            width,
            height,
            pixels,
        }
    }
}

/// Row `y` of `height` rows of `width` pixels, empty outside them.
fn row_of(pixels: &[u8], width: usize, height: usize, y: isize) -> &[u8] {
    match usize::try_from(y) {
        Ok(y) if y < height => &pixels[y * width..][..width],
        _ => &[],
    }
}

/// The pixel of a row at `x`, white outside it.
fn pixel_of(row: &[u8], x: isize) -> usize {
    usize::try_from(x)
        .ok()
        .and_then(|x| row.get(x))
        .map_or(0, |&pixel| usize::from(pixel))
}

/// The pixels of a row at `x - 1`, `x` and `x + 1`, the first in the highest bit.
fn three_of(row: &[u8], x: isize) -> usize {
    pixel_of(row, x - 1) << 2 | pixel_of(row, x) << 1 | pixel_of(row, x + 1)
}

/// Decides a bitmap's pixels, row by row, with `decide` from their contexts in the ten-pixel
/// template of Figure 2: the pixels of the template in each of its rows held in a register
/// shifted along a column at a time.
fn direct_pixels(mark: &mut Bitmap, mut decide: impl FnMut(usize) -> u8) {
    let width = mark.width;
    for y in 0..mark.height {
        let (above, row) = mark.pixels.split_at_mut(y * width);
        let above = &*above;
        let two_up = row_of(above, width, y, y as isize - 2);
        let one_up = row_of(above, width, y, y as isize - 1);
        let mut two_up_bits = three_of(two_up, 0);
        let mut one_up_bits = three_of(one_up, 1);
        let mut left_bits = 0;
        for (x, pixel) in row[..width].iter_mut().enumerate() {
            *pixel = decide(two_up_bits << 7 | one_up_bits << 2 | left_bits);
            let x = x as isize;
            two_up_bits = ((two_up_bits << 1) & 0b111) | pixel_of(two_up, x + 2);
            one_up_bits = ((one_up_bits << 1) & 0b1_1111) | pixel_of(one_up, x + 3);
            left_bits = ((left_bits << 1) & 0b11) | usize::from(*pixel);
        }
    }
}

/// Decides a bitmap's pixels, row by row, with `decide` from their contexts in the
/// eleven-pixel template of Figure 3 over the shape it refines, offset from it by `dx` and `dy`:
/// the pixels of the template in each of its rows held in a register shifted along a column at
/// a time.
fn refined_pixels(
    mark: &mut Bitmap,
    reference: &Bitmap,
    (dx, dy): (isize, isize),
    mut decide: impl FnMut(usize) -> u8,
) {
    let width = mark.width;
    let (pixels, reference_width, reference_height) =
        (&reference.pixels[..], reference.width, reference.height);
    for y in 0..mark.height {
        let (above, row) = mark.pixels.split_at_mut(y * width);
        let up = row_of(above, width, y, y as isize - 1);
        let ry = y as isize + dy;
        let reference_up = row_of(pixels, reference_width, reference_height, ry - 1);
        let middle = row_of(pixels, reference_width, reference_height, ry);
        let below = row_of(pixels, reference_width, reference_height, ry + 1);
        let mut up_bits = three_of(up, 0);
        let mut middle_bits = three_of(middle, dx);
        let mut below_bits = three_of(below, dx);
        let mut left = 0;
        for (x, pixel) in row[..width].iter_mut().enumerate() {
            let (x, rx) = (x as isize, x as isize + dx);
            *pixel = decide(
                up_bits << 8
                    | left << 7
                    | pixel_of(reference_up, rx) << 6
                    | middle_bits << 3
                    | below_bits,
            );
            up_bits = ((up_bits << 1) & 0b111) | pixel_of(up, x + 2);
            middle_bits = ((middle_bits << 1) & 0b111) | pixel_of(middle, rx + 2);
            below_bits = ((below_bits << 1) & 0b111) | pixel_of(below, rx + 2);
            left = usize::from(*pixel);
        }
    }
}

/// A symbol placed on the image, its top left corner at column `x` and row `y`.
#[derive(Debug, Clone)]
pub(super) struct Blit {
    pub(super) mark: Rc<Bitmap>,
    pub(super) x: isize,
    pub(super) y: isize,
}

/// An image and the symbols placed on it, in the order they were placed.
#[derive(Debug)]
pub(super) struct Mask {
    pub(super) image: Bitmap,
    pub(super) blits: Vec<Blit>,
}

/// One of the integer contexts of the multivalue extension (11.2.2): a binary decision tree
/// whose nodes, each a Z′ context, are allocated as they are first reached.
#[derive(Debug)]
struct NumberContext {
    states: Vec<u8>,
    left: Vec<usize>,
    right: Vec<usize>,
}

impl NumberContext {
    fn new() -> Self {
        Self {
            states: vec![0],
            left: vec![0],
            right: vec![0],
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn child(&mut self, node: usize, decision: bool) -> usize {
        let existing = if decision {
            self.right[node]
        } else {
            self.left[node]
        };
        if existing != 0 {
            return existing;
        }
        let created = self.states.len();
        self.states.push(0);
        self.left.push(0);
        self.right.push(0);
        if decision {
            self.right[node] = created;
        } else {
            self.left[node] = created;
        }
        created
    }
}

/// The number contexts of a stream, by what they code.
#[derive(Debug)]
enum Number {
    RecordType,
    ImageSize,
    MatchIndex,
    SymbolWidth,
    SymbolHeight,
    WidthDifference,
    HeightDifference,
    AbsoluteColumn,
    AbsoluteRow,
    SameLineColumn,
    SameLineRow,
    NewLineColumn,
    NewLineRow,
    CommentLength,
    CommentOctet,
    DictionarySize,
}

const NUMBERS: usize = 16;

/// Where a symbol goes, as the column of its left edge and the row of its bottom edge, counted
/// from 1 at the left and at the bottom.
struct Location {
    left: i32,
    bottom: i32,
}

struct Decoder<'a> {
    zp: ZpDecoder<'a>,
    bits: [u8; 2],
    direct: Vec<u8>,
    refinement: Vec<u8>,
    numbers: [NumberContext; NUMBERS],
    library: Vec<Rc<Bitmap>>,
    blits: Vec<Blit>,
    dictionary: Option<&'a [Rc<Bitmap>]>,
    image_wanted: bool,
    image: Option<Bitmap>,
    last_row_left: i32,
    last_row_bottom: i32,
    last_right: i32,
    bottoms: [i32; 3],
    bottom_index: usize,
    baseline: i32,
}

/// The image an Sjbz chunk codes, with the shapes of the dictionary it requires, if any, in
/// `dictionary`, and the symbols it places.
pub(super) fn decode_image(
    chunk: &[u8],
    dictionary: Option<&[Rc<Bitmap>]>,
) -> Result<Mask, DjvuError> {
    let mut decoder = Decoder::new(chunk, dictionary, true);
    decoder.records()?;
    let image = decoder.image.take().ok_or_else(|| {
        DjvuError::Malformed("A JB2 image ends before its start of data".to_owned())
    })?;
    Ok(Mask {
        image,
        blits: decoder.blits,
    })
}

/// The shapes a Djbz chunk defines, after those of the dictionary it requires, if any.
pub(super) fn decode_dictionary(
    chunk: &[u8],
    dictionary: Option<&[Rc<Bitmap>]>,
) -> Result<Vec<Rc<Bitmap>>, DjvuError> {
    let mut decoder = Decoder::new(chunk, dictionary, false);
    decoder.records()?;
    Ok(decoder.library)
}

fn median(a: i32, b: i32, c: i32) -> i32 {
    a.min(b).max(a.max(b).min(c))
}

impl<'a> Decoder<'a> {
    fn new(chunk: &'a [u8], dictionary: Option<&'a [Rc<Bitmap>]>, image_wanted: bool) -> Self {
        Self {
            zp: ZpDecoder::new(chunk),
            bits: [0; 2],
            direct: vec![0; 1024],
            refinement: vec![0; 2048],
            numbers: std::array::from_fn(|_| NumberContext::new()),
            library: Vec::new(),
            blits: Vec::new(),
            dictionary,
            image_wanted,
            image: None,
            last_row_left: 0,
            last_row_bottom: 0,
            last_right: 0,
            bottoms: [0; 3],
            bottom_index: 0,
            baseline: 0,
        }
    }

    fn records(&mut self) -> Result<(), DjvuError> {
        let mut started = false;
        loop {
            let kind = self.number(Number::RecordType, START_OF_DATA, END_OF_DATA);
            if !started
                && !matches!(
                    kind,
                    START_OF_DATA | REQUIRED_DICT_OR_RESET | PRESERVED_COMMENT
                )
            {
                return Err(DjvuError::Malformed(format!(
                    "JB2 record {kind} comes before the start of data"
                )));
            }
            match kind {
                START_OF_DATA => {
                    if started {
                        return Err(DjvuError::Malformed("A JB2 stream starts twice".to_owned()));
                    }
                    self.start()?;
                    started = true;
                }
                NEW_MARK => {
                    let mark = self.symbol_size()?;
                    let mark = self.direct(mark);
                    let location = self.relative_location(&mark);
                    let mark = Rc::new(mark);
                    self.place(&mark, location)?;
                    self.library.push(Rc::new(mark.cropped()));
                }
                NEW_MARK_LIBRARY_ONLY => {
                    let mark = self.symbol_size()?;
                    let mark = self.direct(mark);
                    self.library.push(Rc::new(mark.cropped()));
                }
                NEW_MARK_IMAGE_ONLY => {
                    let mark = self.symbol_size()?;
                    let mark = self.direct(mark);
                    let location = self.relative_location(&mark);
                    self.place(&Rc::new(mark), location)?;
                }
                MATCHED_REFINE => {
                    let reference = self.matched()?;
                    let mark = self.refined(&reference)?;
                    let location = self.relative_location(&mark);
                    let mark = Rc::new(mark);
                    self.place(&mark, location)?;
                    self.library.push(Rc::new(mark.cropped()));
                }
                MATCHED_REFINE_LIBRARY_ONLY => {
                    let reference = self.matched()?;
                    let mark = self.refined(&reference)?;
                    self.library.push(Rc::new(mark.cropped()));
                }
                MATCHED_REFINE_IMAGE_ONLY => {
                    let reference = self.matched()?;
                    let mark = self.refined(&reference)?;
                    let location = self.relative_location(&mark);
                    self.place(&Rc::new(mark), location)?;
                }
                MATCHED_COPY => {
                    let mark = self.matched()?;
                    let location = self.relative_location(&mark);
                    self.place(&mark, location)?;
                }
                NON_MARK_DATA => {
                    let mark = self.symbol_size()?;
                    let mark = self.direct(mark);
                    let location = self.absolute_location(&mark)?;
                    self.place(&Rc::new(mark), location)?;
                }
                REQUIRED_DICT_OR_RESET => {
                    if started {
                        for context in &mut self.numbers {
                            context.reset();
                        }
                    } else {
                        let shapes = self.number(Number::DictionarySize, 0, BIG_POSITIVE);
                        self.inherit(shapes)?;
                    }
                }
                PRESERVED_COMMENT => {
                    let length = self.number(Number::CommentLength, 0, BIG_POSITIVE);
                    for _ in 0..length {
                        self.number(Number::CommentOctet, 0, 255);
                    }
                }
                END_OF_DATA => return Ok(()),
                other => unreachable!("record type {other} is out of the range decoded"),
            }
        }
    }

    fn start(&mut self) -> Result<(), DjvuError> {
        let width = self.number(Number::ImageSize, 0, BIG_POSITIVE);
        let height = self.number(Number::ImageSize, 0, BIG_POSITIVE);
        if self.zp.decode(&mut self.bits, REFINEMENT_FLAG) != 0 {
            return Err(DjvuError::Unsupported(
                "JB2 image refinement data is not supported".to_owned(),
            ));
        }
        if self.image_wanted {
            self.image = Some(Bitmap::blank(width as usize, height as usize)?);
        }
        self.last_row_left = 0;
        self.last_row_bottom = height;
        self.last_right = 0;
        self.start_line(height);
        Ok(())
    }

    fn inherit(&mut self, shapes: i32) -> Result<(), DjvuError> {
        if !self.library.is_empty() {
            return Err(DjvuError::Malformed(
                "A JB2 stream requires a dictionary after defining shapes".to_owned(),
            ));
        }
        match self.dictionary {
            Some(dictionary) if dictionary.len() == shapes as usize => {
                self.library.extend_from_slice(dictionary);
                Ok(())
            }
            Some(dictionary) => Err(DjvuError::Malformed(format!(
                "A JB2 stream requires a dictionary of {shapes} shapes, but its dictionary has {}",
                dictionary.len()
            ))),
            None => Err(DjvuError::Malformed(format!(
                "A JB2 stream requires a dictionary of {shapes} shapes, but has none"
            ))),
        }
    }

    fn symbol_size(&mut self) -> Result<Bitmap, DjvuError> {
        let width = self.number(Number::SymbolWidth, 0, BIG_POSITIVE);
        let height = self.number(Number::SymbolHeight, 0, BIG_POSITIVE);
        Bitmap::blank(width as usize, height as usize)
    }

    fn matched(&mut self) -> Result<Rc<Bitmap>, DjvuError> {
        if self.library.is_empty() {
            return Err(DjvuError::Malformed(
                "A JB2 record matches a shape of an empty library".to_owned(),
            ));
        }
        let index = self.number(Number::MatchIndex, 0, self.library.len() as i32 - 1);
        Ok(Rc::clone(&self.library[index as usize]))
    }

    /// Codes a bitmap's pixels with the ten-pixel template of Figure 2 (11.5.9).
    fn direct(&mut self, mut mark: Bitmap) -> Bitmap {
        direct_pixels(&mut mark, |context| {
            self.zp.decode(&mut self.direct, context) as u8
        });
        mark
    }

    /// Codes a bitmap's pixels with the eleven-pixel template of Figure 3 (11.5.10), four of its
    /// own and seven of the shape it refines, the two aligned on their centres: the left of two
    /// central columns and the lower of two central rows.
    fn refined(&mut self, reference: &Bitmap) -> Result<Bitmap, DjvuError> {
        let width = reference.width as i32
            + self.number(Number::WidthDifference, BIG_NEGATIVE, BIG_POSITIVE);
        let height = reference.height as i32
            + self.number(Number::HeightDifference, BIG_NEGATIVE, BIG_POSITIVE);
        if width < 0 || height < 0 {
            return Err(DjvuError::Malformed(
                "A refined JB2 shape has a negative size".to_owned(),
            ));
        }
        let mut mark = Bitmap::blank(width as usize, height as usize)?;
        let dx = (reference.width as isize - 1) / 2 - (width as isize - 1) / 2;
        let dy = reference.height as isize / 2 - height as isize / 2;
        refined_pixels(&mut mark, reference, (dx, dy), |context| {
            self.zp.decode(&mut self.refinement, context) as u8
        });
        Ok(mark)
    }

    /// The location of a symbol relative to the first symbol of the current line, starting a
    /// new line, or to the previous symbol on it, against the median bottom of the line's last
    /// three symbols (11.5.8).
    fn relative_location(&mut self, mark: &Bitmap) -> Location {
        let (width, height) = (mark.width as i32, mark.height as i32);
        if self.zp.decode(&mut self.bits, OFFSET_TYPE) == 1 {
            let left =
                self.last_row_left + self.number(Number::NewLineColumn, BIG_NEGATIVE, BIG_POSITIVE);
            let top =
                self.last_row_bottom + self.number(Number::NewLineRow, BIG_NEGATIVE, BIG_POSITIVE);
            let bottom = top - height + 1;
            self.last_row_left = left;
            self.last_row_bottom = bottom;
            self.last_right = left + width - 1;
            self.start_line(bottom);
            return Location { left, bottom };
        }
        let left =
            self.last_right + self.number(Number::SameLineColumn, BIG_NEGATIVE, BIG_POSITIVE);
        let bottom = self.baseline + self.number(Number::SameLineRow, BIG_NEGATIVE, BIG_POSITIVE);
        self.last_right = left + width - 1;
        self.bottom_index = (self.bottom_index + 1) % self.bottoms.len();
        self.bottoms[self.bottom_index] = bottom;
        self.baseline = median(self.bottoms[0], self.bottoms[1], self.bottoms[2]);
        Location { left, bottom }
    }

    fn start_line(&mut self, bottom: i32) {
        self.bottoms = [bottom; 3];
        self.bottom_index = 0;
        self.baseline = bottom;
    }

    /// The location of non-symbol data, from its top left corner (11.5.7).
    fn absolute_location(&mut self, mark: &Bitmap) -> Result<Location, DjvuError> {
        let (image_width, image_height) = match &self.image {
            Some(image) => (image.width as i32, image.height as i32),
            None => {
                return Err(DjvuError::Malformed(
                    "A JB2 dictionary places a symbol on an image".to_owned(),
                ))
            }
        };
        let left = self.number(Number::AbsoluteColumn, 1, image_width);
        let top = self.number(Number::AbsoluteRow, 1, image_height);
        Ok(Location {
            left,
            bottom: top - mark.height as i32 + 1,
        })
    }

    fn place(&mut self, mark: &Rc<Bitmap>, location: Location) -> Result<(), DjvuError> {
        let image = match (&mut self.image, self.image_wanted) {
            (Some(image), true) => image,
            _ => {
                return Err(DjvuError::Malformed(
                    "A JB2 dictionary places a symbol on an image".to_owned(),
                ))
            }
        };
        let x0 = location.left as isize - 1;
        let y0 = image.height as isize - (location.bottom as isize + mark.height as isize - 1);
        self.blits.push(Blit {
            mark: Rc::clone(mark),
            x: x0,
            y: y0,
        });
        let (width, height) = (image.width as isize, image.height as isize);
        for y in (-y0).max(0)..mark.height as isize {
            if y0 + y >= height {
                break;
            }
            for x in (-x0).max(0)..mark.width as isize {
                if x0 + x >= width {
                    break;
                }
                if mark.pixels[y as usize * mark.width + x as usize] != 0 {
                    image.pixels[((y0 + y) * width + x0 + x) as usize] = 1;
                }
            }
        }
        Ok(())
    }

    /// An integer from `low` to `high` (11.2.5): its sign, then the doubling range it falls
    /// in, then its bits within that range, each decision skipped when the bounds settle it.
    fn number(&mut self, which: Number, mut low: i32, mut high: i32) -> i32 {
        let context = &mut self.numbers[which as usize];
        let mut negative = false;
        let mut cutoff = 0i32;
        let mut node = 0;
        let mut phase = 1;
        let mut range = -1i32;
        while range != 1 {
            let decision = if low < cutoff && high >= cutoff {
                self.zp.decode(&mut context.states, node) == 1
            } else {
                low >= cutoff
            };
            node = context.child(node, decision);
            match phase {
                1 => {
                    negative = !decision;
                    if negative {
                        let lowered = -high - 1;
                        high = -low - 1;
                        low = lowered;
                    }
                    phase = 2;
                    cutoff = 1;
                }
                2 => {
                    if decision {
                        cutoff = 2 * cutoff + 1;
                    } else {
                        phase = 3;
                        range = (cutoff + 1) / 2;
                        cutoff = if range == 1 { 0 } else { cutoff - range / 2 };
                    }
                }
                _ => {
                    range /= 2;
                    if range != 1 {
                        cutoff += if decision { range / 2 } else { -(range / 2) };
                    } else if !decision {
                        cutoff -= 1;
                    }
                }
            }
        }
        if negative {
            -cutoff - 1
        } else {
            cutoff
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(bitmap: &Bitmap, x: isize, y: isize) -> usize {
        if x < 0 || y < 0 || x as usize >= bitmap.width || y as usize >= bitmap.height {
            0
        } else {
            usize::from(bitmap.pixels[y as usize * bitmap.width + x as usize])
        }
    }

    fn direct_pixel_by_pixel(mark: &mut Bitmap, mut decide: impl FnMut(usize) -> u8) {
        for y in 0..mark.height as isize {
            for x in 0..mark.width as isize {
                let context = at(mark, x - 1, y - 2) << 9
                    | at(mark, x, y - 2) << 8
                    | at(mark, x + 1, y - 2) << 7
                    | at(mark, x - 2, y - 1) << 6
                    | at(mark, x - 1, y - 1) << 5
                    | at(mark, x, y - 1) << 4
                    | at(mark, x + 1, y - 1) << 3
                    | at(mark, x + 2, y - 1) << 2
                    | at(mark, x - 2, y) << 1
                    | at(mark, x - 1, y);
                mark.pixels[y as usize * mark.width + x as usize] = decide(context);
            }
        }
    }

    fn refined_pixel_by_pixel(
        mark: &mut Bitmap,
        reference: &Bitmap,
        (dx, dy): (isize, isize),
        mut decide: impl FnMut(usize) -> u8,
    ) {
        for y in 0..mark.height as isize {
            let ry = y + dy;
            for x in 0..mark.width as isize {
                let rx = x + dx;
                let context = at(mark, x - 1, y - 1) << 10
                    | at(mark, x, y - 1) << 9
                    | at(mark, x + 1, y - 1) << 8
                    | at(mark, x - 1, y) << 7
                    | at(reference, rx, ry - 1) << 6
                    | at(reference, rx - 1, ry) << 5
                    | at(reference, rx, ry) << 4
                    | at(reference, rx + 1, ry) << 3
                    | at(reference, rx - 1, ry + 1) << 2
                    | at(reference, rx, ry + 1) << 1
                    | at(reference, rx + 1, ry + 1);
                mark.pixels[y as usize * mark.width + x as usize] = decide(context);
            }
        }
    }

    fn deciding(contexts: &mut Vec<usize>, black: u32) -> impl FnMut(usize) -> u8 + '_ {
        let mut state = 0x9e37_79b9_u32;
        move |context| {
            contexts.push(context);
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            u8::from((state ^ context as u32) % 8 < black)
        }
    }

    const SIZES: [(usize, usize); 9] = [
        (0, 0),
        (0, 4),
        (4, 0),
        (1, 1),
        (1, 6),
        (6, 1),
        (2, 2),
        (5, 3),
        (37, 23),
    ];

    #[test]
    fn decides_direct_pixels_in_the_contexts_of_the_template() {
        for (width, height) in SIZES {
            for black in [0, 1, 4, 7, 8] {
                let (mut ours, mut theirs) = (Vec::new(), Vec::new());
                let mut mark = Bitmap::blank(width, height).unwrap();
                direct_pixels(&mut mark, deciding(&mut ours, black));
                let mut expected = Bitmap::blank(width, height).unwrap();
                direct_pixel_by_pixel(&mut expected, deciding(&mut theirs, black));
                assert_eq!(ours, theirs, "{width}x{height} {black}");
                assert_eq!(mark, expected, "{width}x{height} {black}");
            }
        }
    }

    #[test]
    fn decides_refined_pixels_in_the_contexts_of_the_template() {
        let mut references = Vec::new();
        for (width, height) in SIZES {
            let mut reference = Bitmap::blank(width, height).unwrap();
            direct_pixels(&mut reference, deciding(&mut Vec::new(), 3));
            references.push(reference);
        }
        for reference in &references {
            for (width, height) in SIZES {
                let dx = (reference.width as isize - 1) / 2 - (width as isize - 1) / 2;
                let dy = reference.height as isize / 2 - height as isize / 2;
                for offset in [(dx, dy), (-3, 2), (4, -5), (40, 30)] {
                    let (mut ours, mut theirs) = (Vec::new(), Vec::new());
                    let mut mark = Bitmap::blank(width, height).unwrap();
                    refined_pixels(&mut mark, reference, offset, deciding(&mut ours, 4));
                    let mut expected = Bitmap::blank(width, height).unwrap();
                    refined_pixel_by_pixel(
                        &mut expected,
                        reference,
                        offset,
                        deciding(&mut theirs, 4),
                    );
                    let size = (reference.width, reference.height, width, height);
                    assert_eq!(ours, theirs, "{size:?} {offset:?}");
                    assert_eq!(mark, expected, "{size:?} {offset:?}");
                }
            }
        }
    }
}
