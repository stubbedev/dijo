//! An image's data decoded by its filters as PDFBox's `COSStream.createInputStream` decodes
//! it, the image filters `lopdf` does not know included: `DCTDecode` as `DCTFilter` reads the
//! JPEG through the JDK's reader, `CCITTFaxDecode` as `CCITTFaxFilter` decodes it, `JBIG2Decode` as
//! the PDFBox JBIG2 plugin reads it, and `Crypt` as `CryptFilter` passes it through. JPEG 2000 has no
//! reader in Tika's Java.

use std::borrow::Cow;

use hayro_ccitt::{DecodeSettings, Decoder, DecoderContext, EncodingMode};
use lopdf::{Dictionary, Object};

use super::super::cos::{as_dict, invalid_filter, name_string, Pdf};
use super::{Exception, Unwritten};
use crate::ocr::jpeg;

const IO_EXCEPTION: &str = "java.io.IOException";
const IIO_EXCEPTION: &str = "javax.imageio.IIOException";
const MISSING_READER: &str = "org.apache.pdfbox.filter.MissingImageReaderException";
/// `CCITTFaxFilter`'s default limit on the bytes of a decoded image.
const CCITT_MAX_BYTES: i64 = 256 * 1024 * 1024;

/// The data `data` of the image of the dictionary `dictionary` decoded by the filters
/// `filters`, each with its parameters of `parameters`, for an image of the height `height`.
pub(super) fn decoded(
    pdf: &Pdf,
    data: Cow<'_, [u8]>,
    dictionary: &Dictionary,
    filters: &[&[u8]],
    parameters: Option<&Object>,
    height: i32,
) -> Result<Vec<u8>, Unwritten> {
    if let Some(invalid) = invalid_filter(filters) {
        return Err(Unwritten::Stream(invalid));
    }
    let mut data = data.into_owned();
    for (index, filter) in filters.iter().enumerate() {
        let params = match parameters {
            Some(Object::Array(each)) => each
                .get(index)
                .and_then(|params| pdf.resolve(params))
                .and_then(as_dict),
            Some(other) => (index == 0).then(|| as_dict(other)).flatten(),
            None => None,
        };
        data = match *filter {
            b"DCTDecode" | b"DCT" => {
                jpeg::pdfbox_raster(&data)
                    .map_err(|reason| Unwritten::Stream(Exception::new(IIO_EXCEPTION, reason)))?
                    .samples
            }
            b"CCITTFaxDecode" | b"CCF" => ccitt(pdf, &data, params, height)?,
            b"JPXDecode" => return Err(missing_jpeg_2000_reader()),
            b"JBIG2Decode" => jbig2(pdf, &data, params)?,
            b"Crypt" => {
                crypt(pdf, dictionary)?;
                data
            }
            other => {
                let name = Object::Name(other.to_vec());
                let params = params.map(|params| Object::Dictionary(params.clone()));
                pdf.decode_data(
                    Cow::Owned(data),
                    dictionary,
                    Some(&name),
                    params.as_ref(),
                    &[],
                )?
            }
        };
    }
    Ok(data)
}

/// The `MissingImageReaderException` PDFBox's `JPXFilter` throws in Tika, whose Java has no
/// JPEG 2000 reader.
pub(super) fn missing_jpeg_2000_reader() -> Unwritten {
    Unwritten::MissingReader(Exception::new(
        MISSING_READER,
        "Cannot read JPEG2000 image: Java Advanced Imaging (JAI) Image I/O Tools are not \
         installed"
            .to_owned(),
    ))
}

/// `CCITTFaxFilter.decode`: the rows of 1-bit samples, white 1 unless `BlackIs1`.
fn ccitt(
    pdf: &Pdf,
    data: &[u8],
    params: Option<&Dictionary>,
    height: i32,
) -> Result<Vec<u8>, Unwritten> {
    let empty = Dictionary::new();
    let params = params.unwrap_or(&empty);
    let columns = pdf.int(params, b"Columns", 1728);
    let rows = pdf.int(params, b"Rows", 0);
    let rows = if rows > 0 && height > 0 {
        height
    } else {
        rows.max(height)
    };
    let k = pdf.int(params, b"K", 0);
    let byte_aligned = matches!(
        pdf.get(params, b"EncodedByteAlign"),
        Some(Object::Boolean(true))
    );
    if columns <= 0 || rows <= 0 {
        return Err(thrown_io(format!(
            "Invalid CCITT image dimensions: cols={columns}, rows={rows}"
        )));
    }
    let stride = (i64::from(columns) + 7) / 8;
    let size = stride * i64::from(rows);
    if size > CCITT_MAX_BYTES {
        return Err(thrown_io(format!(
            "CCITT decode buffer too large ({size} bytes) for cols={columns}, rows={rows}; max \
             allowed={CCITT_MAX_BYTES}; increase org.apache.pdfbox.filter.ccittfax.maxbytes to \
             override"
        )));
    }
    let (encoding, end_of_line) = match k {
        0 => {
            let end_of_line = match pdf.get(params, b"EndOfLine") {
                Some(value) => matches!(value, Object::Boolean(true)),
                None => {
                    if data.is_empty() {
                        return Err(thrown_io("EOF while reading CCITT header".to_owned()));
                    }
                    starts_with_end_of_line(data)
                }
            };
            (EncodingMode::Group3_1D, end_of_line)
        }
        k if k > 0 => (EncodingMode::Group3_2D { k: k as u32 }, false),
        _ => (EncodingMode::Group4, false),
    };
    let settings = DecodeSettings {
        columns: columns as u32,
        rows: rows as u32,
        end_of_block: true,
        end_of_line,
        rows_are_byte_aligned: byte_aligned && !end_of_line,
        encoding,
        invert_black: false,
    };
    let mut bitmap = Bitmap {
        rows: vec![0u8; size as usize],
        stride: stride as usize,
        row: 0,
        column: 0,
    };
    let mut context = DecoderContext::new(settings);
    hayro_ccitt::decode(data, &mut bitmap, &mut context)
        .map_err(|error| thrown_io(format!("CCITT data that does not decode: {error}")))?;
    let black_is_one = matches!(pdf.get(params, b"BlackIs1"), Some(Object::Boolean(true)));
    let mut rows = bitmap.rows;
    if !black_is_one {
        for byte in &mut rows {
            *byte = !*byte;
        }
    }
    Ok(rows)
}

/// `CCITTFaxFilter`'s sniffing of a Group 3 stream: whether an end-of-line code leads its first
/// 20 bytes, or stands in them, where TwelveMonkeys reads Group 3 with end-of-line codes rather
/// than modified Huffman.
fn starts_with_end_of_line(data: &[u8]) -> bool {
    let head = &data[..data.len().min(20)];
    let byte = |i: usize| i32::from(head.get(i).copied().unwrap_or(0) as i8);
    if byte(0) == 0 && (byte(1) >> 4 == 1 || byte(1) == 1) {
        return true;
    }
    let mut bits = (((byte(0) << 8) + (byte(1) & 0xff)) >> 4) as i16;
    for i in 12..head.len() * 8 {
        bits = ((bits as i32) << 1).wrapping_add((byte(i / 8) >> (7 - (i % 8))) & 1) as i16;
        if bits & 0xFFF == 1 {
            return true;
        }
    }
    false
}

/// The rows of a CCITT image as the decoder pushes its runs: a bit set for each black pixel.
struct Bitmap {
    rows: Vec<u8>,
    stride: usize,
    row: usize,
    column: usize,
}

impl Decoder for Bitmap {
    fn push_pixels(&mut self, white: bool, count: u32) {
        let start = self.column;
        self.column += count as usize;
        if white {
            return;
        }
        let row = self.row * self.stride;
        for column in start..self.column.min(self.stride * 8) {
            if let Some(byte) = self.rows.get_mut(row + column / 8) {
                *byte |= 0x80 >> (column % 8);
            }
        }
    }

    fn next_line(&mut self) {
        self.row += 1;
        self.column = 0;
    }
}

fn thrown_io(message: String) -> Unwritten {
    Unwritten::Stream(Exception::new(IO_EXCEPTION, message))
}

/// `CryptFilter.decode` of the data of the image of `dictionary`, which it passes through or
/// fails on ([`Pdf::unsupported_crypt_filter`]).
fn crypt(pdf: &Pdf, dictionary: &Dictionary) -> Result<(), Unwritten> {
    match pdf.unsupported_crypt_filter(dictionary) {
        None => Ok(()),
        Some(name) => Err(thrown_io(format!(
            "Unsupported crypt filter {}",
            name_string(name)
        ))),
    }
}

/// `JBIG2Filter.decode`: the image the PDFBox JBIG2 plugin reads of `data`, the data of the
/// parameters' `JBIG2Globals` stream before it, if they give one, as one stream of segments, as
/// rows of 1-bit samples, white 1, each row padded to a byte.
fn jbig2(pdf: &Pdf, data: &[u8], params: Option<&Dictionary>) -> Result<Vec<u8>, Unwritten> {
    let joined;
    let segments = match params.and_then(|params| pdf.get(params, b"JBIG2Globals")) {
        Some(Object::Stream(globals)) => {
            joined = [pdf.decode(globals)?, data.to_vec()].concat();
            &joined[..]
        }
        _ => data,
    };
    hayro_jbig2::Image::new_embedded(segments, None)
        .ok()
        .and_then(|image| jbig2_raster(&image))
        .ok_or_else(|| thrown_io("Could not read JBIG2 image".to_owned()))
}

/// The raster of `image` as Java's binary image holds it: rows of 1-bit samples, white 1, each
/// padded to a byte; none for an image it cannot decode, or more bytes than a Java array holds.
fn jbig2_raster(image: &hayro_jbig2::Image) -> Option<Vec<u8>> {
    let stride = usize::try_from(image.width().div_ceil(8)).ok()?;
    let length = stride.checked_mul(usize::try_from(image.height()).ok()?)?;
    if i32::try_from(length).is_err() {
        return None;
    }
    let mut raster = Jbig2Raster {
        bytes: Vec::with_capacity(length),
        byte: 0,
        bits: 0,
    };
    image.decode(&mut raster).ok()?;
    (raster.bytes.len() == length).then_some(raster.bytes)
}

/// The rows of a JBIG2 image as the decoder pushes its pixels: a bit set for each white one.
struct Jbig2Raster {
    bytes: Vec<u8>,
    /// The bits of the row's byte being filled, from its most significant one.
    byte: u8,
    bits: u32,
}

impl Jbig2Raster {
    fn push_byte(&mut self) {
        self.bytes.push(self.byte);
        self.byte = 0;
        self.bits = 0;
    }
}

impl hayro_jbig2::Decoder for Jbig2Raster {
    fn push_pixel(&mut self, black: bool) {
        if !black {
            self.byte |= 0x80 >> self.bits;
        }
        self.bits += 1;
        if self.bits == 8 {
            self.push_byte();
        }
    }

    fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
        let byte = if black { 0x00 } else { 0xFF };
        self.bytes
            .extend(std::iter::repeat_n(byte, chunk_count as usize));
    }

    fn next_line(&mut self) {
        if self.bits > 0 {
            self.push_byte();
        }
    }
}

#[cfg(test)]
mod tests {
    use md5::{Digest, Md5};

    use super::*;

    /// The rasters PDFBox's JBIG2 plugin reads of the fixtures: each file's name, width,
    /// height, raster length and MD5 (`Jbig2Rasters.java`).
    const JBIG2_RASTERS: &str = include_str!("../../../../tests/fixtures/pdf/jbig2-rasters.tsv");

    #[test]
    fn reads_jbig2_rasters_as_pdfbox_s_plugin_does() {
        let files = [
            "tests/fixtures/pdf/jbig2-text.jb2",
            "../quality/corpus/prose-scan.jb2",
        ];
        assert_eq!(JBIG2_RASTERS.lines().count(), files.len());
        for (line, path) in JBIG2_RASTERS.lines().zip(files) {
            let fields: Vec<&str> = line.split('\t').collect();
            assert!(path.ends_with(fields[0]), "{path}: {line}");
            let file = std::fs::read(path).unwrap();
            let image = hayro_jbig2::Image::new(&file).unwrap();
            let raster = jbig2_raster(&image).unwrap();
            let found = [
                image.width().to_string(),
                image.height().to_string(),
                raster.len().to_string(),
                Md5::digest(&raster)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            ];
            assert_eq!(found, fields[1..], "{path}");
        }
    }
}
