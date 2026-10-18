//! PDFBox's `PDImage`, as an image XObject (`PDImageXObject`) or an inline image
//! (`PDInlineImage`) answers for its parameters, each under its own keys, and gives its data.

use std::borrow::Cow;

use lopdf::{Dictionary, Object, Stream};

use super::super::content::InlineImage;
use super::super::cos::{float_value, int_value, Pdf};
use super::super::Unported;
use super::colorspace::{self, ColorSpace};
use super::filters;
use super::{Thrown, Unwritten};

const IO_EXCEPTION: &str = "java.io.IOException";
/// `ImageGraphicsEngine.JPEG`, `JP2` and `JB2`: the filters whose data is copied as it is.
pub(super) const JPEG: [&[u8]; 2] = [b"DCTDecode", b"DCT"];
pub(super) const JP2: [&[u8]; 1] = [b"JPXDecode"];
pub(super) const JB2: [&[u8]; 1] = [b"JBIG2Decode"];

/// The file types `PDImage.getSuffix` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Suffix {
    Png,
    Jpg,
    Jpx,
    Tiff,
    Jb2,
}

/// An image a page draws.
pub(super) struct PdImage<'p> {
    pdf: &'p Pdf<'p>,
    source: Source<'p>,
    /// The resources current where it is drawn, which name colour spaces.
    resources: Option<&'p Dictionary>,
}

/// Where an image's parameters and data are.
enum Source<'p> {
    XObject(&'p Stream),
    /// An inline image's parameters, its data as `ID` read it and decoded by its filters.
    Inline {
        parameters: Dictionary,
        raw: Vec<u8>,
        decoded: Vec<u8>,
    },
}

impl<'p> PdImage<'p> {
    /// `new PDImageXObject(stream, resources)`.
    pub(super) fn xobject(
        pdf: &'p Pdf<'p>,
        stream: &'p Stream,
        resources: Option<&'p Dictionary>,
    ) -> Self {
        Self {
            pdf,
            source: Source::XObject(stream),
            resources,
        }
    }

    /// `new PDInlineImage(parameters, data, resources)`, which decodes the data at once, or
    /// none for an image `BeginInlineImage` skips: one without data, or whose data decodes to
    /// nothing.
    pub(super) fn inline(
        pdf: &'p Pdf<'p>,
        image: InlineImage,
        resources: Option<&'p Dictionary>,
    ) -> Result<Option<Self>, Unported> {
        let InlineImage { parameters, data } = image;
        let Some(raw) = data.filter(|data| !data.is_empty()) else {
            return Ok(None);
        };
        let filters = filter_names(pdf, get2(pdf, &parameters, b"F", b"Filter"));
        let decode_parameters = get2(pdf, &parameters, b"DP", b"DecodeParms");
        let height = get2(pdf, &parameters, b"H", b"Height")
            .and_then(int_value)
            .unwrap_or(0);
        let decoded = match filters::decoded(
            pdf,
            Cow::Borrowed(&raw),
            &filters,
            decode_parameters,
            height,
        ) {
            Ok(decoded) => decoded,
            Err(Unwritten::Unported(unported)) => return Err(unported),
            Err(failed) => {
                return Err(Unported(format!(
                    "an inline image whose data fails to decode, which ends PDFBox's reading of \
                     its page's images: {failed:?}"
                )))
            }
        };
        if decoded.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            pdf,
            source: Source::Inline {
                parameters,
                raw,
                decoded,
            },
            resources,
        }))
    }

    /// The image of the mask or soft mask stream `stream`, which PDFBox makes without
    /// resources.
    fn mask_image(&self, stream: &'p Stream) -> PdImage<'p> {
        Self::xobject(self.pdf, stream, None)
    }

    fn dict(&self) -> &Dictionary {
        match &self.source {
            Source::XObject(stream) => &stream.dict,
            Source::Inline { parameters, .. } => parameters,
        }
    }

    /// `getDictionaryObject(first, second)`: the value of a parameter under the key the image
    /// reads first, else the other.
    fn get(&self, long: &[u8], short: &[u8]) -> Option<&Object> {
        let (first, second) = match self.source {
            Source::XObject(_) => (long, short),
            Source::Inline { .. } => (short, long),
        };
        get2(self.pdf, self.dict(), first, second)
    }

    /// `getInterpolate`.
    pub(super) fn interpolates(&self) -> bool {
        matches!(self.get(b"Interpolate", b"I"), Some(Object::Boolean(true)))
    }

    /// `isStencil`.
    pub(super) fn is_stencil(&self) -> bool {
        let value = match self.source {
            Source::XObject(stream) => self.pdf.get(&stream.dict, b"ImageMask"),
            Source::Inline { .. } => self.get(b"ImageMask", b"IM"),
        };
        matches!(value, Some(Object::Boolean(true)))
    }

    /// `getBitsPerComponent`.
    pub(super) fn bits_per_component(&self) -> i32 {
        if self.is_stencil() {
            return 1;
        }
        self.get(b"BitsPerComponent", b"BPC")
            .and_then(int_value)
            .unwrap_or(-1)
    }

    pub(super) fn width(&self) -> i32 {
        self.dimension(b"Width", b"W")
    }

    pub(super) fn height(&self) -> i32 {
        self.dimension(b"Height", b"H")
    }

    fn dimension(&self, long: &[u8], short: &[u8]) -> i32 {
        let value = match self.source {
            Source::XObject(stream) => self.pdf.get(&stream.dict, long),
            Source::Inline { .. } => self.get(long, short),
        };
        value.and_then(int_value).unwrap_or(-1)
    }

    /// `getDecode`.
    pub(super) fn decode(&self) -> Option<&[Object]> {
        let value = match self.source {
            Source::XObject(stream) => self.pdf.get(&stream.dict, b"Decode"),
            Source::Inline { .. } => self.get(b"Decode", b"D"),
        };
        match value {
            Some(Object::Array(items)) => Some(items),
            _ => None,
        }
    }

    /// `getColorSpace`.
    pub(super) fn color_space(&self) -> Result<ColorSpace, Unwritten> {
        match &self.source {
            Source::XObject(stream) => {
                let value = stream
                    .dict
                    .get(b"ColorSpace")
                    .or_else(|_| stream.dict.get(b"CS"))
                    .ok();
                match value {
                    Some(value) => colorspace::create(self.pdf, value, self.resources),
                    None if self.is_stencil() => Ok(ColorSpace::Gray),
                    None if self.has_filter(&JP2) => Err(filters::missing_jpeg_2000_reader()),
                    None => Err(Unwritten::Stream(Thrown::new(
                        IO_EXCEPTION,
                        "could not determine color space".to_owned(),
                    ))),
                }
            }
            Source::Inline { .. } => match self.get(b"ColorSpace", b"CS").cloned() {
                Some(value) => self.inline_color_space(value),
                None if self.is_stencil() => Ok(ColorSpace::Gray),
                None => Err(Unwritten::Stream(Thrown::new(
                    IO_EXCEPTION,
                    "could not determine inline image color space".to_owned(),
                ))),
            },
        }
    }

    /// `PDInlineImage.createColorSpace`: abbreviated names expanded.
    fn inline_color_space(&self, value: Object) -> Result<ColorSpace, Unwritten> {
        let long = |object: &Object| match object {
            Object::Name(name) => Object::Name(
                match name.as_slice() {
                    b"RGB" => &b"DeviceRGB"[..],
                    b"CMYK" => b"DeviceCMYK",
                    b"G" => b"DeviceGray",
                    other => other,
                }
                .to_vec(),
            ),
            other => other.clone(),
        };
        match &value {
            Object::Name(_) => colorspace::create(self.pdf, &long(&value), self.resources),
            Object::Array(items) if items.len() > 1 => {
                if !matches!(&items[0], Object::Name(name) if name == b"I" || name == b"Indexed") {
                    return Err(Unported(
                        "an inline image colour space array other than a palette, which PDFBox \
                         fails on"
                            .to_owned(),
                    )
                    .into());
                }
                let mut array = items.clone();
                array[0] = Object::Name(b"Indexed".to_vec());
                array[1] = long(&items[1]);
                colorspace::create(self.pdf, &Object::Array(array), self.resources)
            }
            _ => Err(Unported(
                "an inline image colour space PDFBox fails on with its description".to_owned(),
            )
            .into()),
        }
    }

    /// The names of the image's filters.
    fn filters(&self) -> Vec<&[u8]> {
        let value = match self.source {
            Source::XObject(stream) => self.pdf.get(&stream.dict, b"Filter"),
            Source::Inline { .. } => self.get(b"Filter", b"F"),
        };
        filter_names(self.pdf, value)
    }

    fn has_filter(&self, names: &[&[u8]]) -> bool {
        self.filters().iter().any(|filter| names.contains(filter))
    }

    /// `getSuffix`: the file type PDFBox gives the image by its filters, if any.
    pub(super) fn suffix(&self) -> Option<Suffix> {
        let filters = self.filters();
        let has = |names: &[&[u8]]| filters.iter().any(|filter| names.contains(filter));
        match self.source {
            Source::XObject(_) => {
                if filters.is_empty() {
                    Some(Suffix::Png)
                } else if has(&[b"DCTDecode"]) {
                    Some(Suffix::Jpg)
                } else if has(&[b"JPXDecode"]) {
                    Some(Suffix::Jpx)
                } else if has(&[b"CCITTFaxDecode"]) {
                    Some(Suffix::Tiff)
                } else if has(&[b"FlateDecode", b"LZWDecode", b"RunLengthDecode"]) {
                    Some(Suffix::Png)
                } else if has(&[b"JBIG2Decode"]) {
                    Some(Suffix::Jb2)
                } else {
                    None
                }
            }
            Source::Inline { .. } => Some(if has(&[b"DCTDecode", b"DCT"]) {
                Suffix::Jpg
            } else if has(&[b"CCITTFaxDecode", b"CCF"]) {
                Suffix::Tiff
            } else {
                Suffix::Png
            }),
        }
    }

    /// `isEmpty`.
    pub(super) fn is_empty(&self) -> Result<bool, Unported> {
        Ok(match &self.source {
            Source::XObject(stream) => self.pdf.raw(stream)?.is_empty(),
            Source::Inline { decoded, .. } => decoded.is_empty(),
        })
    }

    /// `createInputStream(stopFilters)`: the data decoded up to the first filter of `stop`.
    pub(super) fn data_until(&self, stop: &[&[u8]]) -> Result<Vec<u8>, Unported> {
        match &self.source {
            Source::XObject(stream) => self.pdf.decode_until(stream, stop),
            Source::Inline {
                parameters, raw, ..
            } => self.pdf.decode_data(
                Cow::Borrowed(raw),
                get2(self.pdf, parameters, b"F", b"Filter"),
                get2(self.pdf, parameters, b"DP", b"DecodeParms"),
                stop,
            ),
        }
    }

    /// `createInputStream()`: the data decoded by every filter, JPEG and CCITT ones included,
    /// or why it is not: a JPEG 2000 image Java has no reader for, and filters the port does not
    /// decode.
    pub(super) fn data(&self) -> Result<Vec<u8>, Unwritten> {
        match &self.source {
            Source::Inline { decoded, .. } => Ok(decoded.clone()),
            Source::XObject(stream) => filters::decoded(
                self.pdf,
                self.pdf.raw(stream)?,
                &self.filters(),
                self.pdf.get(&stream.dict, b"DecodeParms"),
                self.height(),
            ),
        }
    }

    /// `getColorKeyMask`: the colour ranges a `Mask` array masks.
    pub(super) fn color_key(&self) -> Option<&[Object]> {
        match self.source {
            Source::XObject(stream) => match self.pdf.get(&stream.dict, b"Mask") {
                Some(Object::Array(ranges)) => Some(ranges),
                _ => None,
            },
            Source::Inline { .. } => None,
        }
    }

    /// `getMask`: the image a `Mask` stream holds.
    pub(super) fn mask(&self) -> Option<PdImage<'p>> {
        match self.source {
            Source::XObject(stream) => self
                .pdf
                .stream(&stream.dict, b"Mask")
                .map(|mask| self.mask_image(mask)),
            Source::Inline { .. } => None,
        }
    }

    /// `getSoftMask`.
    pub(super) fn soft_mask(&self) -> Option<PdImage<'p>> {
        match self.source {
            Source::XObject(stream) => self
                .pdf
                .stream(&stream.dict, b"SMask")
                .map(|mask| self.mask_image(mask)),
            Source::Inline { .. } => None,
        }
    }

    /// `ImageGraphicsEngine.hasMasks`.
    pub(super) fn has_masks(&self) -> bool {
        self.mask().is_some() || self.soft_mask().is_some()
    }

    /// The soft mask's `Matte`, if it has one.
    pub(super) fn matte(&self) -> Option<Vec<f32>> {
        match self.pdf.get(self.dict(), b"Matte") {
            Some(Object::Array(items)) => Some(
                items
                    .iter()
                    .map(|item| self.pdf.resolve(item).and_then(float_value).unwrap_or(0.0))
                    .collect(),
            ),
            _ => None,
        }
    }

    /// The XMP stream of an image XObject.
    pub(super) fn metadata(&self) -> Option<&'p Stream> {
        match self.source {
            Source::XObject(stream) => self.pdf.stream(&stream.dict, b"Metadata"),
            Source::Inline { .. } => None,
        }
    }

    /// Whether this is an inline image.
    pub(super) fn is_inline(&self) -> bool {
        matches!(self.source, Source::Inline { .. })
    }
}

/// `getDictionaryObject(first, second)`.
fn get2<'a>(pdf: &'a Pdf, dict: &'a Dictionary, first: &[u8], second: &[u8]) -> Option<&'a Object> {
    pdf.get(dict, first).or_else(|| pdf.get(dict, second))
}

/// The names a `Filter` entry gives.
fn filter_names<'a>(pdf: &'a Pdf, value: Option<&'a Object>) -> Vec<&'a [u8]> {
    match value {
        Some(Object::Name(name)) => vec![name.as_slice()],
        Some(Object::Array(items)) => items
            .iter()
            .filter_map(|item| match pdf.resolve(item) {
                Some(Object::Name(name)) => Some(name.as_slice()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}
