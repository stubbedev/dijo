//! The OCR enricher `TikaNativeMain` hands Tika's image parsers when tesseract runs: our Java
//! `PreparingOcrParser`, under Tika's `GuardedEnricher`. It reads the image with tesseract and
//! writes the text in a div of class `ocr`, the pages of a TIFF each in a div of class `page`,
//! and adds a report of how each image was read to the metadata. With OCR skipped it is not
//! run, where Tika's path records it though it reads nothing (#114).

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::thrown::{Exception, Thrown};
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Failure, Unported};
use crate::input::Pages;
use crate::ocr::{
    self, Decoded, Format, ImageRead, OcrError, OcrSettings, Orientation, Stopped, Undecoded,
};

use super::{extractor, MIN_PIXELS, PREPARING_OCR_PARSER};

/// `TIFF.ORIENTATION`, the EXIF orientation the image parser read.
const ORIENTATION: &str = "tiff:Orientation";
/// `TikaPagedText.PAGE_NUMBER`, of an image on a page of the document embedding it.
const PAGE_NUMBER: &str = "tk:page:number";
/// `TikaCoreProperties.TIKA_META_EXCEPTION_WARNING`.
const WARNING: &str = "tk:exception:warn";
/// The exception our Java `HeifDecoder` throws when libheif cannot decode an image.
const IO_EXCEPTION: &str = "java.io.IOException";

/// What the enricher does with an image, decided before its document is written.
pub(super) enum Enricher<'a> {
    /// Tika hands the image no enricher, or OCR is skipped.
    Absent,
    /// The enricher reads the image, decoded.
    Reading {
        reader: OcrReader,
        decoded: Decoded<'a>,
    },
}

/// What `PreparingOcrParser` reads images with in this process: tesseract, under an
/// extraction's OCR settings.
pub(crate) struct OcrReader {
    tesseract: ocr::Reader,
    settings: OcrSettings,
}

impl OcrReader {
    /// The reader under the OCR settings of `configs`, or why this process cannot read.
    pub(crate) fn of(configs: &ParserConfigs) -> Result<Self, Unported> {
        let tesseract = ocr::reader().map_err(|reason| {
            Unported::new(format!(
                "OCR in this process, which needs tesseract's library: {reason}"
            ))
        })?;
        Ok(Self {
            tesseract,
            settings: OcrSettings::of(&configs.ocr),
        })
    }

    /// `PreparingOcrParser.parse` of a decoded image of `file_size` bytes, turned upright by
    /// its EXIF `orientation`, the image on page `page` of the document embedding it: read in
    /// the script the extraction assumes, the script it learns kept, and what it read written
    /// into the document.
    pub(crate) fn read(
        &self,
        xhtml: &mut Xhtml,
        decoded: Decoded<'_>,
        file_size: u64,
        orientation: Option<Orientation>,
        page: Option<u32>,
    ) -> Result<(), Failure> {
        let context = xhtml.context();
        let assumed = self
            .settings
            .auto()
            .then(|| context.assumed_ocr_script())
            .flatten();
        let tesseract = self.tesseract.clone().map_err(ocr_failure)?;
        let learned = ocr::read_image(
            tesseract,
            &self.settings,
            decoded,
            file_size,
            orientation,
            assumed.as_deref(),
            |read| write(xhtml, read, page),
        )
        .map_err(|stopped| match stopped {
            Stopped::Ocr(error) => ocr_failure(error),
            Stopped::Writing(stop) => Failure::Stopped(stop),
        })?;
        if let Some(learned) = learned {
            context.learn_ocr_script(learned);
        }
        Ok(())
    }
}

/// The failure of an extraction whose OCR failed: out of its memory budget, or with why
/// tesseract read nothing.
fn ocr_failure(error: OcrError) -> Failure {
    match error {
        OcrError::MemoryLimit { limit, .. } => Failure::Stopped(Stop::MemoryLimit(limit)),
        error => Failure::Extraction(Error::ParseError(error.to_string())),
    }
}

impl<'a> Enricher<'a> {
    /// The enricher of an image of `content_type` Tika hands one, `enriched`, unless OCR is
    /// skipped: the image decoded, or of a TIFF of several pages, mapped to the pages of
    /// `memory`, the pages found, to decode one at a time as they are read. An image the port
    /// cannot decode or read with tesseract is left to Tika's path; one libheif cannot decode
    /// fails as the `IOException` Tika's path throws out of the image parser `tika_name` (#173).
    pub(super) fn of(
        enriched: bool,
        content_type: &str,
        input: &'a [u8],
        memory: Pages<'a>,
        configs: &ParserConfigs,
        tika_name: &str,
    ) -> Result<Self, Decline<'static>> {
        if !enriched || configs.ocr.skips_ocr() {
            return Ok(Self::Absent);
        }
        let base = content_type.split(';').next().unwrap_or_default().trim();
        let format = Format::of(base).ok_or_else(|| {
            Unported::new(format!(
                "an image of {base}, which the port does not decode for OCR"
            ))
        })?;
        let timeout = OcrSettings::of(&configs.ocr).timeout();
        let decoded =
            ocr::decode(input, memory, format, timeout).map_err(|undecoded| match undecoded {
                #[cfg(not(feature = "static"))]
                Undecoded::NoDecoder => Decline::from(Unported::new(format!(
                "an image of {base}, for which no decoder is bundled or installed: Tika's path \
                 reports it unread"
            ))),
                Undecoded::Failed(reason) => Decline::from(Unported::new(format!(
                    "an image of {base} the port cannot decode for OCR: {reason}"
                ))),
                Undecoded::Rejected(message) => Decline::Fails(
                    Thrown::Io(Exception::new(IO_EXCEPTION, message)).into_error(tika_name),
                ),
            })?;
        Ok(Self::Reading {
            reader: OcrReader::of(configs)?,
            decoded,
        })
    }

    /// `GuardedEnricher.parse`: unless the image is too small for it, the enricher is recorded
    /// and reads the image of `file_size` bytes, as the image parser's metadata leaves it.
    pub(super) fn enrich(self, xhtml: &mut Xhtml, file_size: usize) -> Result<(), Failure> {
        let Self::Reading { reader, decoded } = self else {
            return Ok(());
        };
        if too_small(xhtml) {
            return Ok(());
        }
        xhtml.delegate_to(PREPARING_OCR_PARSER);
        let metadata = xhtml.metadata_mut();
        let orientation = metadata.get(ORIENTATION).and_then(Orientation::parse);
        let page = metadata
            .get(PAGE_NUMBER)
            .and_then(crate::java::parse_int)
            .and_then(|page| u32::try_from(page).ok());
        reader.read(xhtml, decoded, file_size as u64, orientation, page)
    }
}

/// Whether the image parser recorded a size under `ContentEnrichers.MIN_PIXELS`.
fn too_small(xhtml: &mut Xhtml) -> bool {
    let metadata = xhtml.metadata_mut();
    let dimension = |name: &str| metadata.get(name).and_then(crate::java::parse_int);
    dimension(extractor::IMAGE_WIDTH.name).is_some_and(|width| width < MIN_PIXELS)
        || dimension(extractor::IMAGE_LENGTH.name).is_some_and(|height| height < MIN_PIXELS)
}

/// Writes what the enricher read as `PreparingOcrParser` writes it through Tika's
/// `EmbeddedContentHandler` and `BodyContentHandler` into the image's document: each reading
/// after its report, the pages of a TIFF each in a div of class `page`, with a warning for a
/// page too large or damaged to read.
fn write(xhtml: &mut Xhtml, read: ImageRead<'_>, page: Option<u32>) -> Result<(), Stop> {
    match read {
        ImageRead::One(attempt) => emit(xhtml, attempt, page),
        ImageRead::Page { number, attempt } => {
            xhtml.start_element_with("div", &[("class", "page")])?;
            match attempt {
                Some(attempt) => emit(xhtml, attempt, Some(number))?,
                None => xhtml.metadata_mut().add(
                    WARNING,
                    format!("Page {number} of the image is too large or damaged to OCR"),
                ),
            }
            end_block(xhtml)
        }
    }
}

/// `PreparingOcrParser.emit`: the report of a reading, then its text in a div of class `ocr`.
fn emit(xhtml: &mut Xhtml, attempt: &ocr::Attempt, page: Option<u32>) -> Result<(), Stop> {
    xhtml.metadata_mut().add(ocr::REPORT, attempt.report(page));
    write_text(xhtml, attempt)
}

/// The text of a reading, if it read any, in a div of class `ocr` of `PreparingOcrParser`'s own
/// document, as Tika's `EmbeddedContentHandler` and `BodyContentHandler` pass it into the
/// document it reads an image of.
pub fn write_text(xhtml: &mut Xhtml, attempt: &ocr::Attempt) -> Result<(), Stop> {
    let Some(text) = attempt.text() else {
        return Ok(());
    };
    xhtml.start_element_with("div", &[("class", "ocr")])?;
    xhtml.characters(text)?;
    end_block(xhtml)
}

/// Ends a div of the enricher's own document: the image's document breaks the line after it,
/// and the enricher's breaks it again.
fn end_block(xhtml: &mut Xhtml) -> Result<(), Stop> {
    xhtml.end_element("div")?;
    xhtml.ignorable_whitespace("\n")
}
