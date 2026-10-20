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
mod prepare;
mod raster;
mod recognize;
mod render;
mod report;
mod script;
mod tesseract;
mod text;

use std::fmt;
use std::sync::OnceLock;

use crate::bundle;
use crate::errors::{Error, ExtractResult};
use crate::formats::embedded::OcrLanguage;
pub use bitmap::{Bitmap, Density};
pub(crate) use decode::{decode, jpeg, Decoded, Format, Undecoded};
pub use pages::{read_pages, Learning, Outputs};
pub(crate) use prepare::Orientation;
pub use recognize::{Attempt, Image, OcrSettings, Recognized, Recognizer};
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
/// bundled (`DIJO_OCR_BUNDLE`), linked into the static executable or loaded from the
/// bundle's library, or `tesseract` on `PATH` as `tesseract --version` reports it.
pub fn tesseract_version() -> ExtractResult<String> {
    linked_or_bundled_version().map_err(Error::IoError)
}

/// The version of the tesseract linked into the static executable, which native readings run.
#[cfg(feature = "static")]
fn linked_or_bundled_version() -> Result<String, String> {
    tesseract()
        .map(|tesseract| bundle::linked_version(&tesseract.version()))
        .map_err(|none| none.to_string())
}

#[cfg(not(feature = "static"))]
use bundle::tesseract_version as linked_or_bundled_version;

/// Whether tesseract runs, found once per process: the one the
/// build carries, whether or not it can be set up, so that a reading fails with why it cannot
/// rather than reading nothing, or else one on `PATH` that answers `--version`. With one, images
/// and PDF pages are read with OCR.
pub fn tesseract_runs() -> bool {
    static RUNS: OnceLock<bool> = OnceLock::new();
    *RUNS.get_or_init(|| bundle::carries_tesseract() || tesseract_version().is_ok())
}

/// Why native readings have no tesseract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoTesseract {
    /// The tesseract the build carries cannot be set up: OCR is configured, and every reading
    /// fails with this.
    Broken(OcrError),
    /// The library of the `tesseract` on the `PATH` cannot be loaded, for this reason, where
    /// Tika runs the program.
    Unloadable(String),
}

impl fmt::Display for NoTesseract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Broken(error) => write!(f, "{error}"),
            Self::Unloadable(reason) => f.write_str(reason),
        }
    }
}

/// The tesseract readings run with: the one set up, or the error every reading fails with when
/// the one the build carries cannot be set up.
pub type Reader = Result<&'static Tesseract, OcrError>;

/// The tesseract readings run with, or why only Tika reads: it runs the `tesseract` on the
/// `PATH` whose library this process cannot load.
pub fn reader() -> Result<Reader, String> {
    match tesseract() {
        Ok(tesseract) => Ok(Ok(tesseract)),
        Err(NoTesseract::Broken(error)) => Ok(Err(error)),
        Err(NoTesseract::Unloadable(reason)) => Err(reason),
    }
}

/// The tesseract native readings run, set up once per process: the one linked into the static
/// executable with the bundled language data, the bundled library with it, or the library of
/// the `tesseract` on the `PATH`, which reads the language data that program reads. Why there is
/// none otherwise, as for a bundled library that cannot be loaded.
pub fn tesseract() -> Result<&'static Tesseract, NoTesseract> {
    static TESSERACT: OnceLock<Result<Tesseract, NoTesseract>> = OnceLock::new();
    TESSERACT.get_or_init(set_up).as_ref().map_err(Clone::clone)
}

/// The tesseract the build carries could not be set up, for this reason.
fn broken(reason: impl Into<String>) -> NoTesseract {
    NoTesseract::Broken(OcrError::Unavailable {
        reason: reason.into(),
    })
}

#[cfg(feature = "static")]
fn set_up() -> Result<Tesseract, NoTesseract> {
    let tessdata = bundle::bundled_tessdata()
        .map_err(broken)?
        .ok_or_else(|| broken("The static build links tesseract but bundled no language data"))?;
    Tesseract::linked(&tessdata).map_err(broken)
}

#[cfg(not(feature = "static"))]
fn set_up() -> Result<Tesseract, NoTesseract> {
    match bundle::bundled_tesseract_library().map_err(broken)? {
        Some(library) => bundled(&library, bundle::bundled_tessdata().map_err(broken)?),
        None => on_path().map_err(NoTesseract::Unloadable),
    }
}

/// The tesseract of the library the build bundled, with the language data it bundled.
#[cfg(not(feature = "static"))]
fn bundled(
    library: &std::path::Path,
    tessdata: Option<std::path::PathBuf>,
) -> Result<Tesseract, NoTesseract> {
    let tessdata = tessdata
        .ok_or_else(|| broken("The build bundles a tesseract library but no language data"))?;
    Tesseract::loaded(library, Some(&tessdata)).map_err(broken)
}

/// The library of the `tesseract` on the `PATH`, which reports the version the program does.
#[cfg(not(feature = "static"))]
fn on_path() -> Result<Tesseract, String> {
    let program = program_on_path().ok_or("no tesseract on the PATH")?;
    let program_version = bundle::tesseract_version()?;
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
#[cfg(not(feature = "static"))]
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
#[cfg(not(feature = "static"))]
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
        let set_up = bundled(&library, Some(dir.clone()));
        std::fs::remove_dir_all(&dir).unwrap();
        let Err(NoTesseract::Broken(error @ OcrError::Unavailable { .. })) = set_up else {
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

    #[test]
    fn native_readings_run_the_tesseract_whose_version_is_reported() {
        let version = tesseract_version().unwrap();
        let linked = tesseract().unwrap().version();
        assert!(version.contains(&linked), "{version} against {linked}");
    }
}
