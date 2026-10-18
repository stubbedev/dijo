//! An image's data decoded by its filters as PDFBox's `COSStream.createInputStream` decodes
//! it, the image filters `lopdf` does not know included: `DCTDecode` as `DCTFilter` reads the
//! JPEG through the JDK's reader, and `CCITTFaxDecode` as `CCITTFaxFilter` decodes it.
//! JPEG 2000 has no reader in Tika's Java, and JBIG2 and `Crypt` the port does not decode.

use std::borrow::Cow;

use hayro_ccitt::{DecodeSettings, Decoder, DecoderContext, EncodingMode};
use lopdf::{Dictionary, Object};

use super::super::cos::{as_dict, Pdf};
use super::super::Unported;
use super::{Thrown, Unwritten};
use crate::ocr::jpeg;

const IO_EXCEPTION: &str = "java.io.IOException";
const IIO_EXCEPTION: &str = "javax.imageio.IIOException";
const MISSING_READER: &str = "org.apache.pdfbox.filter.MissingImageReaderException";
/// `CCITTFaxFilter`'s default limit on the bytes of a decoded image.
const CCITT_MAX_BYTES: i64 = 256 * 1024 * 1024;

/// The data `data` decoded by the filters `filters`, each with its parameters of `parameters`,
/// for an image of the height `height`.
pub(super) fn decoded(
    pdf: &Pdf,
    data: Cow<'_, [u8]>,
    filters: &[&[u8]],
    parameters: Option<&Object>,
    height: i32,
) -> Result<Vec<u8>, Unwritten> {
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
                    .map_err(|reason| Unwritten::Stream(Thrown::new(IIO_EXCEPTION, reason)))?
                    .samples
            }
            b"CCITTFaxDecode" | b"CCF" => ccitt(pdf, &data, params, height)?,
            b"JPXDecode" => return Err(missing_jpeg_2000_reader()),
            b"JBIG2Decode" | b"Crypt" => {
                return Err(Unported(format!(
                    "an image of the filter {}, which the port does not decode",
                    String::from_utf8_lossy(filter)
                ))
                .into())
            }
            other => {
                let name = Object::Name(other.to_vec());
                let params = params.map(|params| Object::Dictionary(params.clone()));
                pdf.decode_data(Cow::Owned(data), Some(&name), params.as_ref(), &[])?
            }
        };
    }
    Ok(data)
}

/// The `MissingImageReaderException` PDFBox's `JPXFilter` throws in Tika, whose Java has no
/// JPEG 2000 reader.
pub(super) fn missing_jpeg_2000_reader() -> Unwritten {
    Unwritten::MissingReader(Thrown::new(
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
    Unwritten::Stream(Thrown::new(IO_EXCEPTION, message))
}
