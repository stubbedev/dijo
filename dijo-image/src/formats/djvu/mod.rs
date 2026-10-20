//! DjVu documents, single-page or bundled multi-page, as our Java `DjvuParser` reads them for
//! the hidden text layer each page carries (TXTa, or TXTz compressed with BZZ): a page of the
//! output per page of the document, its text split into paragraphs where the layer ends a
//! column, region or paragraph. A page without a text layer is rendered and read by OCR, as
//! Tika hands its rendering to the OCR enricher. Tika detects DjVu but has no parser for
//! it.

mod bzz;
mod document;
mod iw44;
mod jb2;
mod render;
#[cfg(test)]
mod tests;
mod zp;

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::image::enricher::OcrReader;
use crate::formats::image::PREPARING_OCR_PARSER;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::java::{is_blank, strip, utf8};
use crate::ocr::{Bitmap, Decoded};
use document::{DjvuDocument, Form};

/// The most pixels a page, a layer or a shape may have, beyond which decoding fails rather than
/// allocate: `Jb2Decoder.MAX_PIXELS`.
const MAX_PIXELS: u64 = 1 << 28;
/// The largest text layer read.
const MAX_TEXT_BYTES: u64 = 64 << 20;
/// `PagedText.N_PAGES`.
const PAGES: &str = "xmpTPg:NPages";

/// Why a DjVu document cannot be read, as Java's exceptions say it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DjvuError {
    /// A `TikaException`: the document is not well-formed.
    Malformed(String),
    /// An `UnsupportedFormatException`: it uses a feature the reader does not.
    Unsupported(String),
}

impl DjvuError {
    /// The message of the exception, which names an `UnsupportedFormatException` as Tika's
    /// path reports it.
    fn message(&self) -> String {
        match self {
            Self::Malformed(message) => message.clone(),
            Self::Unsupported(message) => format!("UnsupportedFormatException: {message}"),
        }
    }
}

/// Our Java `DjvuParser`.
pub struct DjvuParser;

impl Parser for DjvuParser {
    fn name(&self) -> &'static str {
        "dijo::DjvuParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::DjvuParser"
    }
    fn tika_delegates(&self) -> &'static [(&'static str, &'static str)] {
        &[(PREPARING_OCR_PARSER, "dijo::PreparingOcrParser")]
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["image/vnd.djvu"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'static>> {
        let document = DjvuDocument::read(input)
            .map_err(|error| Decline::Fails(Error::ParseError(error.message())))?;
        let blank = |page| page_text(&document, page).is_ok_and(|text| is_blank(&text));
        let reader = match !configs.ocr.skips_ocr()
            && crate::ocr::tesseract_runs()
            && document.pages().iter().any(blank)
        {
            true => Some(OcrReader::of(configs)?),
            false => None,
        };
        Ok(Box::new(move |xhtml| {
            xhtml
                .metadata_mut()
                .set(PAGES, document.pages().len().to_string());
            for (index, page) in document.pages().iter().enumerate() {
                let text = page_text(&document, page)
                    .map_err(|error| Failure::Malformed(error.message()))?;
                xhtml.start_element_with("div", &[("class", "page")])?;
                if is_blank(&text) {
                    if let Some(reader) = &reader {
                        read_by_ocr(&document, page, index + 1, reader, xhtml)?;
                    }
                }
                for block in text.split(['\u{b}', '\u{1d}', '\u{1f}']) {
                    if !is_blank(block) {
                        xhtml.element("p", strip(block))?;
                    }
                }
                xhtml.end_element("div")?;
            }
            Ok(())
        }))
    }
}

/// `readByOcr`: the page, counted from 1, rendered and read as Tika reads its rendering,
/// which records the OCR parser among those the extraction ran. The OCR limits on the size of
/// an image's file are held to the rendering's grey pixels, as for a PDF page; our Java held
/// them to the PGM file it wrote, header included (#120).
fn read_by_ocr(
    document: &DjvuDocument,
    page: &Form,
    number: usize,
    reader: &OcrReader,
    xhtml: &mut crate::formats::xhtml::Xhtml,
) -> Result<(), Failure> {
    xhtml.record_parser(PREPARING_OCR_PARSER);
    let (image, density) =
        render::page(document, page).map_err(|error| Failure::Malformed(error.message()))?;
    let file_size = u64::from(image.width()) * u64::from(image.height());
    reader.read(
        xhtml,
        Decoded::Single {
            bitmap: Bitmap::Gray(image),
            file: None,
            density,
        },
        file_size,
        None,
        Some(number as u32),
    )
}

/// The text of a page's text layers, empty when it has none.
fn page_text(document: &DjvuDocument, page: &Form) -> Result<String, DjvuError> {
    let mut text = String::new();
    for chunk in document.chunks(page)? {
        let compressed = match &chunk.id {
            b"TXTa" => false,
            b"TXTz" => true,
            _ => continue,
        };
        if chunk.size > MAX_TEXT_BYTES {
            return Err(DjvuError::Malformed(format!(
                "A DjVu text chunk of {} bytes exceeds {MAX_TEXT_BYTES}",
                chunk.size
            )));
        }
        let data = document.data(&chunk)?;
        let layer = match compressed {
            true => bzz::decode(data, MAX_TEXT_BYTES as usize)?,
            false => data.to_vec(),
        };
        text.push_str(&layer_text(&layer)?);
    }
    Ok(text)
}

/// The UTF-8 string a text layer begins with, after its 24-bit length (8.3.5).
fn layer_text(layer: &[u8]) -> Result<String, DjvuError> {
    if layer.is_empty() {
        return Ok(String::new());
    }
    let [a, b, c, ..] = *layer else {
        return Err(DjvuError::Malformed(
            "A DjVu text layer ends inside its length".to_owned(),
        ));
    };
    let length = usize::from(a) << 16 | usize::from(b) << 8 | usize::from(c);
    let text = layer.get(3..3 + length).ok_or_else(|| {
        DjvuError::Malformed(format!(
            "A DjVu text layer is shorter than its length of {length}"
        ))
    })?;
    Ok(utf8(text))
}
