//! OCR: the tesseract native readings run in this process, with our Java `PreparingOcrParser`'s
//! preparation of the images it reads.

mod av1;
mod avif;
pub mod bitmap;
mod decode;
mod flatten;
mod heif;
pub mod icc;
mod java2d;
pub(crate) mod jbig2;
pub mod jpeg;
mod pages;
mod ppocr;
mod prepare;
mod raster;
mod recognize;
mod render;
mod report;
mod script;
mod slots;
mod tesseract;
mod text;

use std::sync::OnceLock;

use crate::bundle;
use crate::errors::{Error, ExtractResult};
use crate::formats::embedded::OcrLanguage;
pub use bitmap::{Bitmap, Density};
pub(crate) use decode::{decode, jpeg, Decoded, Format, Undecoded};
pub use pages::{read_pages, Learning, Outputs};
pub(crate) use prepare::Orientation;
pub use recognize::{Attempt, Image, LanguageHint, OcrSettings, Recognized, Recognizer};
pub use render::{PdfPages, Resolution};
pub use report::KEY as REPORT;
pub use tesseract::{OcrError, Tesseract};

/// What OCR read of an image file, handed on to be written as it is read.
pub(crate) enum ImageRead<'r> {
    /// The reading of a file of one image.
    One(&'r Attempt),
    /// The reading of a page of a TIFF, numbered from 1, each a page of the output: none for a
    /// page too large or damaged to read.
    Page {
        number: u32,
        attempt: Option<&'r Attempt>,
    },
}

/// Why reading an image file stopped: tesseract failed, or writing what it read did.
pub(crate) enum Stopped<W> {
    Ocr(OcrError),
    Writing(W),
}

/// Our Java `PreparingOcrParser.parse` of a decoded image file: one image read as any image is,
/// or the pages of a TIFF read side by side, each in the script and model the pages before it
/// settle, each decoded when it is read and freed once it is, and handed to `write` in page
/// order. With the automatic language, the extraction then assumes the script and model the
/// image, or its latest page read in one, was read in, which this returns.
pub(crate) fn read_image<W>(
    tesseract: &Tesseract,
    settings: &OcrSettings,
    decoded: Decoded<'_>,
    file_size: u64,
    orientation: Option<Orientation>,
    assumed: Option<&OcrLanguage>,
    mut write: impl FnMut(ImageRead<'_>) -> Result<(), W>,
) -> Result<Option<OcrLanguage>, Stopped<W>> {
    let recognizer = Recognizer::new(tesseract, settings);
    let learned = match decoded {
        Decoded::Single {
            bitmap,
            file,
            density,
        } => {
            let image = Image::of(&bitmap, file, file_size, orientation, density);
            let Recognized { read, language } = recognizer
                .recognize(&image, assumed)
                .map_err(Stopped::Ocr)?;
            drop(bitmap);
            write(ImageRead::One(&read)).map_err(Stopped::Writing)?;
            language
        }
        Decoded::Pages(pages) => {
            let learning = Learning::new(assumed.cloned());
            read_pages(
                pages.count(),
                &learning,
                || pages.reader(),
                |reader, index, assumed| {
                    let page = match reader {
                        Ok(reader) => reader.page(index),
                        Err(unreadable) => Err(unreadable.clone()),
                    };
                    let Ok((bitmap, density)) = page else {
                        return Ok((None, None));
                    };
                    let image = Image::of(&bitmap, None, file_size, orientation, density);
                    let Recognized { read, language } = recognizer.recognize(&image, assumed)?;
                    Ok((Some(read), language))
                },
                |outputs| {
                    for (index, output) in std::iter::from_fn(|| outputs.next()).enumerate() {
                        let attempt = output.map_err(Stopped::Ocr)?;
                        write(ImageRead::Page {
                            number: index as u32 + 1,
                            attempt: attempt.as_ref(),
                        })
                        .map_err(Stopped::Writing)?;
                    }
                    Ok(())
                },
            )?;
            learning.language()
        }
    };
    Ok(learned)
}

/// The version of the tesseract OCR runs, since OCR output depends on it: the one the build
/// carries (`DIJO_OCR_BUNDLE`), linked into the static executable or loaded from the bundle's
/// library.
pub fn tesseract_version() -> ExtractResult<String> {
    tesseract()
        .map(|tesseract| bundle::linked_version(&tesseract.version()))
        .map_err(|error| Error::IoError(error.to_string()))
}

/// The tesseract readings run with: the one the build carries, or why it cannot be set up,
/// which every reading fails with.
pub type Reader = Result<&'static Tesseract, OcrError>;

/// The tesseract the build carries, set up once per process: linked into the static executable
/// or loaded from the bundled library, with the bundled language data.
pub fn tesseract() -> Reader {
    static TESSERACT: OnceLock<Result<Tesseract, OcrError>> = OnceLock::new();
    TESSERACT.get_or_init(set_up).as_ref().map_err(Clone::clone)
}

/// The tesseract the build carries could not be set up, for this reason.
fn unavailable(reason: impl Into<String>) -> OcrError {
    OcrError::Unavailable {
        reason: reason.into(),
    }
}

#[cfg(feature = "static")]
fn set_up() -> Result<Tesseract, OcrError> {
    let tessdata = bundle::bundled_tessdata().map_err(unavailable)?;
    Tesseract::linked(&tessdata).map_err(unavailable)
}

#[cfg(not(feature = "static"))]
fn set_up() -> Result<Tesseract, OcrError> {
    bundled(
        &bundle::bundled_tesseract_library().map_err(unavailable)?,
        &bundle::bundled_tessdata().map_err(unavailable)?,
    )
}

/// The tesseract of the library the build bundled, with the language data it bundled.
#[cfg(not(feature = "static"))]
fn bundled(library: &std::path::Path, tessdata: &std::path::Path) -> Result<Tesseract, OcrError> {
    Tesseract::loaded(library, tessdata).map_err(unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tesseract_ocr_runs_reports_its_version() {
        let version = tesseract_version().unwrap();
        assert!(version.starts_with("tesseract 5."), "{version}");
    }

    #[cfg(not(feature = "static"))]
    #[test]
    fn a_bundled_library_that_cannot_load_breaks_every_reading_with_the_loaders_message() {
        let dir =
            std::env::temp_dir().join(format!("dijo-unloadable-tesseract-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let library = dir.join("libdijo_tesseract.so");
        std::fs::write(&library, b"not a library").unwrap();
        let loader = unsafe { libloading::Library::new(&library) }
            .err()
            .and_then(|e| std::error::Error::source(&e).map(ToString::to_string))
            .unwrap();
        let set_up = bundled(&library, &dir);
        std::fs::remove_dir_all(&dir).unwrap();
        let Err(error @ OcrError::Unavailable { .. }) = set_up else {
            panic!("a library that cannot load set up a tesseract");
        };
        let message = error.to_string();
        let attempt = format!(
            "tesseract cannot run: Loading tesseract's library {} failed: ",
            library.display()
        );
        assert!(message.starts_with(&attempt), "{message}");
        assert!(message.ends_with(&loader), "{message} against {loader}");
    }
}
