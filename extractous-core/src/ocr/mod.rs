//! OCR: the tesseract Tika's path runs, and the one native readings run in this process, with
//! our Java `PreparingOcrParser`'s preparation of the images it reads.

mod av1;
mod avif;
mod decode;
mod flatten;
mod heif;
pub(crate) mod jbig2;
mod pages;
mod prepare;
mod raster;
mod recognize;
mod render;
mod report;
mod script;
mod tesseract;
mod text;

use std::sync::OnceLock;

use crate::errors::{Error, ExtractResult};
use crate::tika::native;
pub(crate) use decode::{decode, Decoded, Format, Undecoded};
pub(crate) use pages::{read_pages, FirstPage, Outputs};
pub(crate) use prepare::Orientation;
pub(crate) use recognize::{Attempt, Image, OcrSettings, Recognized, Recognizer};
pub(crate) use render::PdfPages;
pub(crate) use report::KEY as REPORT;
pub(crate) use tesseract::{OcrError, Tesseract};

/// What OCR read in an image file, and the script the extraction learns from it, if any.
#[derive(Debug)]
pub(crate) struct ImageRead {
    pub(crate) pages: ReadPages,
    pub(crate) learned: Option<String>,
}

/// The readings of an image file's pages.
#[derive(Debug)]
pub(crate) enum ReadPages {
    /// The reading of a file of one image.
    One(Attempt),
    /// The readings of the pages of a TIFF, each a page of the output: none for a page too large
    /// or damaged to read.
    Many(Vec<Option<Attempt>>),
}

/// Our Java `PreparingOcrParser.parse` of a decoded image file: one image read as any image is,
/// or the pages of a TIFF read side by side, the first in the script the extraction assumes
/// and the rest in the one it settles. With the automatic language, the extraction learns the
/// script of the image, or of its first page.
pub(crate) fn read_image(
    tesseract: &Tesseract,
    settings: &OcrSettings,
    decoded: Decoded<'_>,
    file_size: u64,
    orientation: Option<Orientation>,
    assumed: Option<&str>,
) -> Result<ImageRead, OcrError> {
    let recognizer = Recognizer::new(tesseract, settings);
    match decoded {
        Decoded::Single(gray) => {
            let Recognized { read, learned } =
                recognizer.recognize(&Image::of(&gray, file_size, orientation), assumed)?;
            Ok(ImageRead {
                pages: ReadPages::One(read),
                learned: learned.filter(|_| settings.auto()),
            })
        }
        Decoded::Pages(pages) => {
            let first_page = FirstPage::new(assumed.map(str::to_owned));
            let read = read_pages(
                pages.count(),
                &first_page,
                || (),
                |(), index, script| {
                    let Ok(gray) = pages.page(index) else {
                        return Ok((None, None));
                    };
                    let image = Image::of(&gray, file_size, orientation);
                    let Recognized { read, learned } = recognizer.recognize(&image, script)?;
                    Ok((Some(read), learned))
                },
                |outputs| std::iter::from_fn(|| outputs.next()).collect::<Result<_, _>>(),
            )?;
            Ok(ImageRead {
                pages: ReadPages::Many(read),
                learned: first_page.learned().filter(|_| settings.auto()),
            })
        }
    }
}

/// The version of the tesseract OCR runs, since OCR output depends on it: the one the build
/// bundled (`EXTRACTOUS_OCR_BUNDLE`), linked into the static executable or loaded from the
/// bundle's library, or `tesseract` on `PATH` as `tesseract --version` reports it.
pub fn tesseract_version() -> ExtractResult<String> {
    native::tesseract_version().map_err(Error::IoError)
}

/// Whether tesseract runs, found once per process as `TikaNativeMain` finds it once: the bundled
/// one, or one on `PATH` that answers `--version`. With one, Tika reads images and PDF pages with
/// our OCR parsers.
pub(crate) fn tesseract_runs() -> bool {
    static RUNS: OnceLock<bool> = OnceLock::new();
    *RUNS.get_or_init(|| tesseract_version().is_ok())
}

/// The tesseract native readings run, set up once per process: the one linked into the static
/// executable with the bundled language data, the bundled library with it, or the library of
/// the `tesseract` on the `PATH`, which reads the language data that program reads. Why there is
/// none otherwise, as for a bundled library built before it exported tesseract's C API.
pub(crate) fn tesseract() -> Result<&'static Tesseract, String> {
    static TESSERACT: OnceLock<Result<Tesseract, String>> = OnceLock::new();
    TESSERACT.get_or_init(set_up).as_ref().map_err(Clone::clone)
}

#[cfg(feature = "static-image")]
fn set_up() -> Result<Tesseract, String> {
    let tessdata = native::bundled_tessdata()?
        .ok_or("The static build links tesseract but bundled no language data")?;
    Tesseract::linked(&tessdata)
}

#[cfg(not(feature = "static-image"))]
fn set_up() -> Result<Tesseract, String> {
    if let Some(library) = native::bundled_tesseract_library()? {
        let tessdata = native::bundled_tessdata()?
            .ok_or("The build bundles a tesseract library but no language data")?;
        return Tesseract::loaded(&library, Some(&tessdata));
    }
    let program = program_on_path().ok_or("no tesseract on the PATH")?;
    let program_version = native::tesseract_version()?;
    let mut failures = Vec::new();
    for library in library_candidates(&program) {
        let tesseract = match Tesseract::loaded(&library, None) {
            Ok(tesseract) => tesseract,
            Err(failure) => {
                failures.push(failure);
                continue;
            }
        };
        let version = tesseract.version();
        if program_version.lines().next() == Some(&format!("tesseract {version}")) {
            return Ok(tesseract);
        }
        failures.push(format!(
            "{} is tesseract {version}, where the program reports {}",
            library.display(),
            program_version.lines().next().unwrap_or_default()
        ));
    }
    Err(format!(
        "the library of {} cannot be loaded: {}",
        program.display(),
        failures.join("; ")
    ))
}

/// The `tesseract` program the `PATH` names first, with its links resolved.
#[cfg(not(feature = "static-image"))]
fn program_on_path() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) {
        "tesseract.exe"
    } else {
        "tesseract"
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|program| program.is_file())
        .and_then(|program| program.canonicalize().ok())
}

/// Where the library of a tesseract program is: in the `lib` directory beside the program's
/// `bin` directory, or beside the program on Windows, and else where the system loader finds
/// it by name.
#[cfg(not(feature = "static-image"))]
fn library_candidates(program: &std::path::Path) -> Vec<std::path::PathBuf> {
    let (name, beside) = if cfg!(windows) {
        (
            "libtesseract-5.dll",
            program.parent().map(std::path::Path::to_path_buf),
        )
    } else {
        let name = if cfg!(target_os = "macos") {
            "libtesseract.5.dylib"
        } else {
            "libtesseract.so.5"
        };
        (
            name,
            program
                .parent()
                .and_then(std::path::Path::parent)
                .map(|prefix| prefix.join("lib")),
        )
    };
    beside
        .map(|dir| dir.join(name))
        .into_iter()
        .chain(std::iter::once(std::path::PathBuf::from(name)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tesseract_ocr_runs_reports_its_version() {
        let version = tesseract_version().unwrap();
        assert!(version.starts_with("tesseract 5."), "{version}");
    }

    #[test]
    fn native_readings_run_the_tesseract_tikas_path_runs() {
        let version = tesseract_version().unwrap();
        let linked = tesseract().unwrap().version();
        assert!(version.contains(&linked), "{version} against {linked}");
    }
}
