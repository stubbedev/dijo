//! The merged image of a Photoshop file and the image of a PCX file, in grey: the two formats
//! tesseract cannot read, which our Java `RecognizedImageParser` has Java decode for it.

use image::GrayImage;

use super::decode::luma;

/// Bytes read in turn, each read failing at the end.
struct Bytes<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Bytes<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(count)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| format!("it ends before byte {}", self.at.saturating_add(count)))?;
        let taken = &self.data[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16_be(&mut self) -> Result<u16, String> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32_be(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64_be(&mut self) -> Result<u64, String> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes(bytes.try_into().expect("eight bytes")))
    }

    fn u16_le(&mut self) -> Result<u16, String> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Skips a section of `length` bytes.
    fn skip(&mut self, length: u64) -> Result<(), String> {
        let length = usize::try_from(length).map_err(|_| format!("a section of {length} bytes"))?;
        self.take(length).map(|_| ())
    }
}

/// Photoshop's colour modes the merged image is decoded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorMode {
    Bitmap,
    Grayscale,
    Rgb,
    Cmyk,
}

/// The merged image of a Photoshop file (`.psd`, or `.psb`), of 1, 8 or 16 bits a channel in
/// bitmap, grey, RGB or CMYK, raw or compressed with PackBits.
pub(crate) fn photoshop(input: &[u8]) -> Result<GrayImage, String> {
    let mut bytes = Bytes::new(input);
    if bytes.take(4)? != b"8BPS" {
        return Err("it is no Photoshop file".to_owned());
    }
    let large = match bytes.u16_be()? {
        1 => false,
        2 => true,
        version => return Err(format!("Photoshop version {version}")),
    };
    bytes.take(6)?;
    let channels = usize::from(bytes.u16_be()?);
    let height = bytes.u32_be()?;
    let width = bytes.u32_be()?;
    let depth = bytes.u16_be()?;
    let mode = match bytes.u16_be()? {
        0 => ColorMode::Bitmap,
        1 => ColorMode::Grayscale,
        3 => ColorMode::Rgb,
        4 => ColorMode::Cmyk,
        mode => return Err(format!("the colour mode {mode}")),
    };
    let needed = match mode {
        ColorMode::Bitmap | ColorMode::Grayscale => 1,
        ColorMode::Rgb => 3,
        ColorMode::Cmyk => 4,
    };
    if channels < needed
        || !matches!(depth, 1 | 8 | 16)
        || (depth == 1) != (mode == ColorMode::Bitmap)
    {
        return Err(format!("{channels} channels of {depth} bits in {mode:?}"));
    }
    let color_mode_data = u64::from(bytes.u32_be()?);
    bytes.skip(color_mode_data)?;
    let resources = u64::from(bytes.u32_be()?);
    bytes.skip(resources)?;
    let layers = if large {
        bytes.u64_be()?
    } else {
        u64::from(bytes.u32_be()?)
    };
    bytes.skip(layers)?;
    let compression = bytes.u16_be()?;
    let (width, height) = (width as usize, height as usize);
    let row_bytes = (width * usize::from(depth)).div_ceil(8);
    let rows = height.checked_mul(channels).ok_or("too many rows")?;
    let planes: Vec<Vec<u8>> = match compression {
        0 => {
            let plane = row_bytes.checked_mul(height).ok_or("too large")?;
            (0..needed)
                .map(|_| bytes.take(plane).map(<[u8]>::to_vec))
                .collect::<Result<_, _>>()?
        }
        1 => {
            let mut counts = Vec::with_capacity(rows);
            for _ in 0..rows {
                counts.push(if large {
                    bytes.u32_be()? as usize
                } else {
                    usize::from(bytes.u16_be()?)
                });
            }
            let mut planes = Vec::with_capacity(needed);
            for channel in 0..needed {
                let mut plane = Vec::with_capacity(row_bytes * height);
                for row in 0..height {
                    let packed = bytes.take(counts[channel * height + row])?;
                    unpack_bits(packed, row_bytes, &mut plane)?;
                }
                planes.push(plane);
            }
            planes
        }
        other => {
            return Err(format!(
                "compression {other}, which the port does not decode"
            ))
        }
    };
    let sample = |plane: &[u8], index: usize| -> u8 {
        match depth {
            16 => plane[index * 2],
            _ => plane[index],
        }
    };
    let mut gray = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            gray.push(match mode {
                ColorMode::Bitmap => {
                    let byte = planes[0][y * row_bytes + x / 8];
                    if byte & (0x80 >> (x % 8)) != 0 {
                        0
                    } else {
                        255
                    }
                }
                ColorMode::Grayscale => sample(&planes[0], index),
                ColorMode::Rgb => luma(
                    sample(&planes[0], index),
                    sample(&planes[1], index),
                    sample(&planes[2], index),
                ),
                ColorMode::Cmyk => {
                    let black = u32::from(sample(&planes[3], index));
                    let ink = |plane: &[u8]| (u32::from(sample(plane, index)) * black / 255) as u8;
                    luma(ink(&planes[0]), ink(&planes[1]), ink(&planes[2]))
                }
            });
        }
    }
    GrayImage::from_raw(width as u32, height as u32, gray)
        .ok_or_else(|| "the image holds fewer pixels than its size".to_owned())
}

/// Appends a row compressed with PackBits, of `length` bytes, to `out`.
fn unpack_bits(packed: &[u8], length: usize, out: &mut Vec<u8>) -> Result<(), String> {
    let start = out.len();
    let mut bytes = Bytes::new(packed);
    while out.len() - start < length {
        let header = bytes.u8()? as i8;
        match header {
            -128 => {}
            0.. => out.extend_from_slice(bytes.take(header as usize + 1)?),
            _ => {
                let value = bytes.u8()?;
                out.extend(std::iter::repeat_n(
                    value,
                    (1 - isize::from(header)) as usize,
                ));
            }
        }
    }
    out.truncate(start + length);
    Ok(())
}

/// A PCX image: of 8 bits a pixel with a palette of 256 colours, of three or four planes of 8
/// bits, or of up to four planes of 1, 2 or 4 bits with the header's palette of 16 colours.
pub(crate) fn pcx(input: &[u8]) -> Result<GrayImage, String> {
    let mut bytes = Bytes::new(input);
    if bytes.u8()? != 0x0A {
        return Err("it is no PCX image".to_owned());
    }
    bytes.u8()?;
    let encoding = bytes.u8()?;
    let bits = bytes.u8()?;
    let (x_min, y_min, x_max, y_max) = (
        bytes.u16_le()?,
        bytes.u16_le()?,
        bytes.u16_le()?,
        bytes.u16_le()?,
    );
    bytes.take(4)?;
    let header_palette = bytes.take(48)?;
    bytes.u8()?;
    let planes = usize::from(bytes.u8()?);
    let line_bytes = usize::from(bytes.u16_le()?);
    if x_max < x_min || y_max < y_min {
        return Err(format!("the window {x_min},{y_min} to {x_max},{y_max}"));
    }
    let width = usize::from(x_max - x_min) + 1;
    let height = usize::from(y_max - y_min) + 1;
    if line_bytes * 8 < width * usize::from(bits) {
        return Err(format!("lines of {line_bytes} bytes for {width} pixels"));
    }
    let mut bytes = Bytes::new(input.get(128..).ok_or("it ends in its header")?);
    let scanline = line_bytes * planes;
    let total = scanline.checked_mul(height).ok_or("too large")?;
    let mut lines = Vec::with_capacity(total);
    while lines.len() < total {
        let byte = bytes.u8()?;
        if encoding == 1 && byte >= 0xC0 {
            let value = bytes.u8()?;
            lines.extend(std::iter::repeat_n(value, usize::from(byte & 0x3F)));
        } else {
            lines.push(byte);
        }
    }
    lines.truncate(total);
    let gray: Vec<u8> = match (bits, planes) {
        (8, 1) => {
            let tail = input
                .len()
                .checked_sub(769)
                .and_then(|at| input.get(at..))
                .filter(|tail| tail[0] == 0x0C)
                .ok_or("an image of 256 colours without its palette")?;
            let levels: Vec<u8> = tail[1..]
                .as_chunks::<3>()
                .0
                .iter()
                .map(|entry| luma(entry[0], entry[1], entry[2]))
                .collect();
            lines
                .chunks_exact(scanline)
                .flat_map(|line| {
                    line[..width]
                        .iter()
                        .map(|&index| levels[usize::from(index)])
                })
                .collect()
        }
        (8, 3 | 4) => lines
            .chunks_exact(scanline)
            .flat_map(|line| {
                (0..width)
                    .map(move |x| luma(line[x], line[line_bytes + x], line[2 * line_bytes + x]))
            })
            .collect(),
        (1 | 2 | 4, 1..=4) if usize::from(bits) * planes <= 4 => {
            let levels: Vec<u8> = header_palette
                .as_chunks::<3>()
                .0
                .iter()
                .map(|entry| luma(entry[0], entry[1], entry[2]))
                .collect();
            let bits = usize::from(bits);
            lines
                .chunks_exact(scanline)
                .flat_map(|line| {
                    let levels = &levels;
                    (0..width).map(move |x| {
                        let mut index = 0;
                        for plane in 0..planes {
                            let bit = x * bits;
                            let byte = line[plane * line_bytes + bit / 8];
                            let sample = (byte >> (8 - bits - bit % 8)) & ((1 << bits) - 1);
                            index |= usize::from(sample) << (plane * bits);
                        }
                        levels[index]
                    })
                })
                .collect()
        }
        _ => return Err(format!("{planes} planes of {bits} bits")),
    };
    GrayImage::from_raw(width as u32, height as u32, gray)
        .ok_or_else(|| "the image holds fewer pixels than its size".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_packbits() {
        let mut out = Vec::new();
        unpack_bits(&[0xFE, 7, 1, 1, 2, 0x80], 5, &mut out).unwrap();
        assert_eq!(out, [7, 7, 7, 1, 2]);
        assert!(unpack_bits(&[2, 1], 3, &mut Vec::new()).is_err());
    }
}
