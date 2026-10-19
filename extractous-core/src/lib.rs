//! Text and metadata extraction by native parsers that reproduce Apache Tika's output; a document
//! no parser reads fails with why, an [`Unsupported`]. This is the
//! implementation behind the `extractous` command line; its API is internal and changes freely.
//! The native parsers live in the crates of their formats, on the common ground of
//! `extractous-base`; this crate gathers them and routes each document to its parser.

use extractous_base::*;

pub use extractous_base::config::{
    OfficeParserConfig, Password, PdfOcrStrategy, PdfParserConfig, TesseractOcrConfig,
};
pub use extractous_base::embedded::{EmbeddedSpool, SpooledEmbedded};
pub use extractous_base::errors::{Error, ExtractResult, Unsupported};
pub use extractous_base::extracted::{CharSet, Metadata};
pub use extractous_base::formats::DIFFERS_FROM_TIKA;
pub use extractous_base::process::{configure as configure_process, ProcessSettings};
pub use extractous_base::{memory, text, unicode};
use extractous_image::ocr;
use extractous_mime::mime;
use extractous_native as bundle;

// extractor module is the main public api interface
mod extractor;
pub use extractor::*;

pub use ocr::tesseract_version;

pub use bundle::{bundled_ocr_licenses, linked_native_licenses};

/// The file extension, with its dot, that Tika's default types give a media type first, such as
/// `.pdf` for `application/pdf`, whatever its parameters; None for a type they do not know, or
/// give no extension.
pub fn media_type_extension(media_type: &str) -> Option<&'static str> {
    let extension = mime::extension(&mime::MediaType::parse(media_type)?.base_type());
    (!extension.is_empty()).then_some(extension)
}

mod fetch;
mod formats;
/// Files too large to read whole, and the peak memory their extraction takes, for the tests of the
/// crates of the formats.
#[cfg(any(test, feature = "testing"))]
pub use formats::large;
/// What the tests of the crates of the formats share.
#[cfg(any(test, feature = "testing"))]
pub use formats::testing;
/// The tests count allocations as the command line does, so that budgets are enforced in them.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<std::alloc::System> =
    memory::CountingAllocator(std::alloc::System);
