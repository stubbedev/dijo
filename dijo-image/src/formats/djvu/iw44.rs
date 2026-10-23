//! IW44, DjVu's wavelet coding of continuous-tone images (DjVu v3 specification, Appendix 1),
//! as our Java `Iw44Decoder` decodes it: the BG44 background and FG44 foreground of a page. The
//! chunks of one layer refine the same wavelet coefficients slice by slice; the image is the
//! inverse transform of the coefficients they leave. Only luminance is reconstructed, the grey
//! the page is read in; chrominance is decoded, since its decisions share the stream, but not
//! transformed.

use image::GrayImage;

use super::zp::ZpDecoder;
use super::{DjvuError, MAX_PIXELS};

const BANDS: usize = 10;
const BLOCK: usize = 32;
const BAND_FIRST_BUCKET: [usize; BANDS] = [0, 1, 2, 3, 4, 8, 12, 16, 32, 48];
const BAND_BUCKETS: [usize; BANDS] = [1, 1, 1, 1, 4, 4, 4, 16, 16, 16];
const INITIAL_STEPS: [i32; 16] = [
    0x004000, 0x008000, 0x008000, 0x010000, 0x010000, 0x010000, 0x020000, 0x020000, 0x020000,
    0x040000, 0x040000, 0x040000, 0x080000, 0x040000, 0x040000, 0x080000,
];
const MAX_SLICE_STEP: i32 = 0x8000;

const ZERO: u8 = 1;
const ACTIVE: u8 = 2;
const NEW: u8 = 4;
const UNKNOWN: u8 = 8;

/// The context of a decision whether a block has new coefficients.
const ROOT: usize = 0;
/// The context of a low mantissa bit.
const MANTISSA: usize = 97;

/// The row and column of each of a block's 1024 coefficients, in their coding order.
const ZIGZAG: [(u8, u8); 1024] = {
    const fn bit(index: usize, position: usize) -> u8 {
        ((index >> position) & 1) as u8
    }
    let mut zigzag = [(0, 0); 1024];
    let mut i = 0;
    while i < 1024 {
        let row = bit(i, 1) << 4 | bit(i, 3) << 3 | bit(i, 5) << 2 | bit(i, 7) << 1 | bit(i, 9);
        let column = bit(i, 0) << 4 | bit(i, 2) << 3 | bit(i, 4) << 2 | bit(i, 6) << 1 | bit(i, 8);
        zigzag[i] = (row, column);
        i += 1;
    }
    zigzag
};

/// The coefficients of one colour component and the state of their decoding.
struct Component {
    blocks_wide: usize,
    coefficients: Vec<Option<Box<[i16; 1024]>>>,
    low_steps: [i32; 16],
    high_steps: [i32; BANDS],
    contexts: [u8; 1 + 80 + 16 + 1],
    coefficient_states: [u8; 256],
    bucket_states: [u8; 16],
    band: usize,
    exhausted: bool,
}

impl Component {
    fn new(width: usize, height: usize) -> Self {
        let blocks_wide = width.div_ceil(BLOCK);
        let blocks = blocks_wide * height.div_ceil(BLOCK);
        let mut low_steps = [0; 16];
        for (i, step) in low_steps.iter_mut().enumerate() {
            *step = if i < 4 {
                INITIAL_STEPS[i]
            } else {
                INITIAL_STEPS[4 + (i - 4) / 4]
            };
        }
        let mut high_steps = [0; BANDS];
        for (band, step) in high_steps.iter_mut().enumerate().skip(1) {
            *step = INITIAL_STEPS[6 + band];
        }
        Self {
            blocks_wide,
            coefficients: (0..blocks).map(|_| None).collect(),
            low_steps,
            high_steps,
            contexts: [0; 1 + 80 + 16 + 1],
            coefficient_states: [0; 256],
            bucket_states: [0; 16],
            band: 0,
            exhausted: false,
        }
    }

    fn bucket_context(band: usize, index: usize) -> usize {
        1 + band * 8 + index
    }

    fn activation_context(index: usize) -> usize {
        81 + index
    }

    /// Decodes one slice of this component: one band of every block (10.4, 10.5).
    fn slice(&mut self, zp: &mut ZpDecoder) {
        if self.exhausted {
            return;
        }
        if !self.null_slice() {
            for block in 0..self.coefficients.len() {
                self.block_band(zp, block);
            }
        }
        self.high_steps[self.band] >>= 1;
        if self.band == 0 {
            for step in &mut self.low_steps {
                *step >>= 1;
            }
        }
        self.band += 1;
        if self.band == BANDS {
            self.band = 0;
            if self.high_steps[BANDS - 1] == 0 {
                self.exhausted = true;
            }
        }
    }

    /// Whether no coefficient of the band can change at its present steps (10.5.1).
    fn null_slice(&mut self) -> bool {
        if self.band != 0 {
            let step = self.high_steps[self.band];
            return step <= 0 || step >= MAX_SLICE_STEP;
        }
        let mut empty = true;
        for i in 0..16 {
            let live = self.low_steps[i] > 0 && self.low_steps[i] < MAX_SLICE_STEP;
            self.coefficient_states[i] = if live { UNKNOWN } else { ZERO };
            empty &= !live;
        }
        empty
    }

    fn step(&self, bucket: usize, i: usize) -> i32 {
        if self.band == 0 {
            self.low_steps[bucket * 16 + i]
        } else {
            self.high_steps[self.band]
        }
    }

    fn block_band(&mut self, zp: &mut ZpDecoder, block: usize) {
        let band = self.band;
        let first = BAND_FIRST_BUCKET[band];
        let buckets = BAND_BUCKETS[band];
        let mut block_state = 0;
        for bucket in 0..buckets {
            let mut bucket_state = 0;
            for i in 0..16 {
                let index = bucket * 16 + i;
                let state = if band == 0 && self.coefficient_states[index] == ZERO {
                    ZERO
                } else if self.coefficients[block]
                    .as_ref()
                    .is_some_and(|data| data[(first + bucket) * 16 + i] != 0)
                {
                    ACTIVE
                } else {
                    UNKNOWN
                };
                self.coefficient_states[index] = state;
                bucket_state |= state;
            }
            self.bucket_states[bucket] = bucket_state;
            block_state |= bucket_state;
        }
        if buckets < 16
            || block_state & ACTIVE != 0
            || (block_state & UNKNOWN != 0 && zp.decode(&mut self.contexts, ROOT) == 1)
        {
            block_state |= NEW;
        }
        if block_state & NEW != 0 {
            for bucket in 0..buckets {
                if self.bucket_states[bucket] & UNKNOWN == 0 {
                    continue;
                }
                let mut context = 0;
                if band > 0 {
                    if let Some(data) = &self.coefficients[block] {
                        let parent = (first + bucket) << 2;
                        for k in 0..4 {
                            if context >= 3 {
                                break;
                            }
                            if data[parent + k] != 0 {
                                context += 1;
                            }
                        }
                    }
                }
                if block_state & ACTIVE != 0 {
                    context |= 4;
                }
                if zp.decode(&mut self.contexts, Self::bucket_context(band, context)) == 1 {
                    self.bucket_states[bucket] |= NEW;
                }
            }
            for bucket in 0..buckets {
                if self.bucket_states[bucket] & NEW == 0 {
                    continue;
                }
                let mut potential = (0..16)
                    .filter(|&i| self.coefficient_states[bucket * 16 + i] & UNKNOWN != 0)
                    .count();
                if self.coefficients[block].is_none() {
                    self.coefficients[block] = Some(Box::new([0; 1024]));
                }
                for i in 0..16 {
                    let index = bucket * 16 + i;
                    if self.coefficient_states[index] & UNKNOWN == 0 {
                        continue;
                    }
                    let mut context = potential.min(7);
                    if self.bucket_states[bucket] & ACTIVE != 0 {
                        context |= 8;
                    }
                    if zp.decode(&mut self.contexts, Self::activation_context(context)) == 1 {
                        self.coefficient_states[index] |= NEW;
                        let step = self.step(bucket, i);
                        let half = step >> 1;
                        let value = step + half - (half >> 2);
                        let signed = if zp.decode_iw44_pass_through() == 1 {
                            -value
                        } else {
                            value
                        };
                        if let Some(data) = &mut self.coefficients[block] {
                            data[(first + bucket) * 16 + i] = signed as i16;
                        }
                        potential = 0;
                    } else {
                        potential = potential.saturating_sub(1);
                    }
                }
            }
        }
        if block_state & ACTIVE != 0 {
            for bucket in 0..buckets {
                if self.bucket_states[bucket] & ACTIVE == 0 {
                    continue;
                }
                for i in 0..16 {
                    if self.coefficient_states[bucket * 16 + i] & ACTIVE == 0 {
                        continue;
                    }
                    let position = (first + bucket) * 16 + i;
                    let step = self.step(bucket, i);
                    let Some(data) = &mut self.coefficients[block] else {
                        continue;
                    };
                    let current = i32::from(data[position]);
                    let mut magnitude = current.abs();
                    let increase = if magnitude <= 3 * step {
                        magnitude += step >> 2;
                        zp.decode(&mut self.contexts, MANTISSA) == 1
                    } else {
                        zp.decode_iw44_pass_through() == 1
                    };
                    magnitude += if increase {
                        step >> 1
                    } else {
                        -step + (step >> 1)
                    };
                    data[position] = if current > 0 { magnitude } else { -magnitude } as i16;
                }
            }
        }
    }

    /// The component's values, rows bottom first, as signed fixed point with six fractional
    /// bits: the coefficients put in place (10.6.3) and inversely transformed (10.6.4).
    fn reconstruct(&self, width: usize, height: usize) -> Vec<i32> {
        let mut values = vec![0i32; width * height];
        for (block, data) in self.coefficients.iter().enumerate() {
            let Some(data) = data else {
                continue;
            };
            let x0 = (block % self.blocks_wide) * BLOCK;
            let y0 = (block / self.blocks_wide) * BLOCK;
            for (&coefficient, &(row, column)) in data.iter().zip(&ZIGZAG) {
                let (x, y) = (x0 + usize::from(column), y0 + usize::from(row));
                if x < width && y < height {
                    values[y * width + x] = i32::from(coefficient);
                }
            }
        }
        let mut scale = 16;
        while scale >= 1 {
            if width > 0 {
                let columns = (0..width).step_by(scale);
                inverse(&mut values, columns, width * scale, (height - 1) / scale);
            }
            for y in (0..height).step_by(scale) {
                inverse(
                    &mut values,
                    std::iter::once(y * width),
                    scale,
                    (width - 1) / scale,
                );
            }
            scale >>= 1;
        }
        values
    }
}

/// The inverse of the interpolating (4, 4) wavelet along rows or columns of the values at a
/// scale, all of them a step at a time: `last + 1` values from each of `starts`, `stride`
/// apart. Lifting restores the even ones, prediction the odd ones from them (10.6.4).
fn inverse(
    values: &mut [i32],
    starts: impl Iterator<Item = usize> + Clone,
    stride: usize,
    last: usize,
) {
    let at = |values: &[i32], start: usize, k: isize| -> i32 {
        if k < 0 || k as usize > last {
            0
        } else {
            values[start + k as usize * stride]
        }
    };
    for k in (0..=last).step_by(2) {
        for start in starts.clone() {
            let k = k as isize;
            let near = at(values, start, k - 1) + at(values, start, k + 1);
            let far = at(values, start, k - 3) + at(values, start, k + 3);
            values[start + k as usize * stride] -= (9 * near - far + 16) >> 5;
        }
    }
    for k in (1..=last).step_by(2) {
        for start in starts.clone() {
            let previous = values[start + (k - 1) * stride];
            let prediction = if k >= 3 && k + 3 <= last {
                let near = previous + values[start + (k + 1) * stride];
                let far = values[start + (k - 3) * stride] + values[start + (k + 3) * stride];
                (9 * near - far + 8) >> 4
            } else if k < last {
                (previous + values[start + (k + 1) * stride] + 1) >> 1
            } else {
                previous
            };
            values[start + k * stride] += prediction;
        }
    }
}

/// The decoder of a layer, fed its chunks in order.
#[derive(Default)]
struct Layer {
    luminance: Option<Component>,
    chrominance: Vec<Component>,
    width: usize,
    height: usize,
    grey: bool,
    chrominance_delay: u8,
    next_serial: usize,
}

/// The grey image a layer's chunks code, in their order.
pub(super) fn decode(chunks: &[&[u8]]) -> Result<GrayImage, DjvuError> {
    if chunks.is_empty() {
        return Err(DjvuError::Malformed(
            "An IW44 layer has no chunks".to_owned(),
        ));
    }
    let mut layer = Layer::default();
    for chunk in chunks {
        layer.chunk(chunk)?;
    }
    layer.image()
}

fn ends_inside_header() -> DjvuError {
    DjvuError::Malformed("An IW44 chunk ends inside its header".to_owned())
}

impl Layer {
    fn chunk(&mut self, chunk: &[u8]) -> Result<(), DjvuError> {
        let [serial, slices, ..] = *chunk else {
            return Err(ends_inside_header());
        };
        let serial = usize::from(serial);
        if serial != self.next_serial {
            return Err(DjvuError::Malformed(format!(
                "IW44 chunk {serial} follows chunk {}",
                self.next_serial as isize - 1
            )));
        }
        self.next_serial += 1;
        let mut offset = 2;
        if serial == 0 {
            if chunk.len() < 8 {
                return Err(ends_inside_header());
            }
            let major = chunk[2] & 0x7f;
            let minor = chunk[3];
            if major != 1 || minor > 2 {
                return Err(DjvuError::Malformed(format!(
                    "IW44 version {major}.{minor} is not supported"
                )));
            }
            self.grey = chunk[2] & 0x80 != 0;
            self.width = usize::from(u16::from_be_bytes([chunk[4], chunk[5]]));
            self.height = usize::from(u16::from_be_bytes([chunk[6], chunk[7]]));
            offset = 8;
            if minor >= 2 {
                let delay = *chunk.get(8).ok_or_else(ends_inside_header)?;
                self.chrominance_delay = delay & 0x7f;
                offset = 9;
            }
            if (self.width * self.height) as u64 > MAX_PIXELS {
                return Err(DjvuError::Malformed(format!(
                    "An IW44 image of {}×{} pixels exceeds {MAX_PIXELS}",
                    self.width, self.height
                )));
            }
            self.luminance = Some(Component::new(self.width, self.height));
            self.chrominance = match self.grey {
                true => Vec::new(),
                false => (0..2)
                    .map(|_| Component::new(self.width, self.height))
                    .collect(),
            };
        }
        let luminance = self.luminance.as_mut().ok_or_else(|| {
            DjvuError::Malformed("An IW44 layer does not start with chunk 0".to_owned())
        })?;
        let mut zp = ZpDecoder::new(&chunk[offset..]);
        for _ in 0..slices {
            luminance.slice(&mut zp);
            if self.chrominance_delay > 0 {
                self.chrominance_delay -= 1;
            } else {
                for component in &mut self.chrominance {
                    component.slice(&mut zp);
                }
            }
        }
        Ok(())
    }

    fn image(&self) -> Result<GrayImage, DjvuError> {
        let luminance = self
            .luminance
            .as_ref()
            .ok_or_else(|| DjvuError::Malformed("An IW44 layer has no chunk 0".to_owned()))?;
        let (width, height) = (self.width, self.height);
        let values = luminance.reconstruct(width, height);
        let mut pixels = vec![0u8; width * height];
        for y in 0..height {
            for x in 0..width {
                let value = ((values[y * width + x] + 32) >> 6).clamp(-128, 127);
                pixels[(height - 1 - y) * width + x] = if self.grey {
                    (127 - value) as u8
                } else {
                    (value + 128) as u8
                };
            }
        }
        Ok(GrayImage::from_raw(width as u32, height as u32, pixels)
            .expect("the pixels fill the image"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zigzag(index: usize) -> (usize, usize) {
        let bit = |position: usize| (index >> position) & 1;
        let row = bit(1) << 4 | bit(3) << 3 | bit(5) << 2 | bit(7) << 1 | bit(9);
        let column = bit(0) << 4 | bit(2) << 3 | bit(4) << 2 | bit(6) << 1 | bit(8);
        (row, column)
    }

    fn inverse_one(values: &mut [i32], start: usize, stride: usize, last: usize) {
        let at = |values: &[i32], k: isize| -> i32 {
            if k < 0 || k as usize > last {
                0
            } else {
                values[start + k as usize * stride]
            }
        };
        for k in (0..=last).step_by(2) {
            let k = k as isize;
            let near = at(values, k - 1) + at(values, k + 1);
            let far = at(values, k - 3) + at(values, k + 3);
            values[start + k as usize * stride] -= (9 * near - far + 16) >> 5;
        }
        for k in (1..=last).step_by(2) {
            let previous = values[start + (k - 1) * stride];
            let prediction = if k >= 3 && k + 3 <= last {
                let near = previous + values[start + (k + 1) * stride];
                let far = values[start + (k - 3) * stride] + values[start + (k + 3) * stride];
                (9 * near - far + 8) >> 4
            } else if k < last {
                (previous + values[start + (k + 1) * stride] + 1) >> 1
            } else {
                previous
            };
            values[start + k * stride] += prediction;
        }
    }

    fn reconstruct_column_by_column(
        component: &Component,
        width: usize,
        height: usize,
    ) -> Vec<i32> {
        let mut values = vec![0i32; width * height];
        for (block, data) in component.coefficients.iter().enumerate() {
            let Some(data) = data else {
                continue;
            };
            let x0 = (block % component.blocks_wide) * BLOCK;
            let y0 = (block / component.blocks_wide) * BLOCK;
            for (i, &coefficient) in data.iter().enumerate() {
                let (row, column) = zigzag(i);
                let (x, y) = (x0 + column, y0 + row);
                if x < width && y < height {
                    values[y * width + x] = i32::from(coefficient);
                }
            }
        }
        let mut scale = 16;
        while scale >= 1 {
            for x in (0..width).step_by(scale) {
                inverse_one(&mut values, x, width * scale, (height - 1) / scale);
            }
            for y in (0..height).step_by(scale) {
                inverse_one(&mut values, y * width, scale, (width - 1) / scale);
            }
            scale >>= 1;
        }
        values
    }

    #[test]
    fn places_coefficients_by_the_zigzag_table() {
        for (i, &(row, column)) in ZIGZAG.iter().enumerate() {
            assert_eq!((usize::from(row), usize::from(column)), zigzag(i), "{i}");
        }
    }

    #[test]
    fn reconstructs_row_by_row_as_column_by_column() {
        let sizes = [
            (0, 0),
            (1, 1),
            (1, 40),
            (40, 1),
            (2, 3),
            (31, 33),
            (64, 32),
            (75, 97),
        ];
        let mut seed = 0x2545_f491_u32;
        for (width, height) in sizes {
            let mut component = Component::new(width, height);
            for (block, data) in component.coefficients.iter_mut().enumerate() {
                if block % 3 == 2 {
                    continue;
                }
                let mut coefficients = Box::new([0i16; 1024]);
                for coefficient in coefficients.iter_mut() {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    *coefficient = (seed % 4096) as i16 - 2048;
                }
                *data = Some(coefficients);
            }
            assert_eq!(
                component.reconstruct(width, height),
                reconstruct_column_by_column(&component, width, height),
                "{width}x{height}"
            );
        }
    }
}
