//! Text and metadata extraction by native parsers that reproduce Apache Tika's output, and with
//! the `tika` feature, by Apache Tika compiled to native code with GraalVM for what no native
//! parser reads yet; without it, such a document fails with why, an [`Unsupported`]. This is the
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
pub use extractous_base::isolate::IsolateSettings;
pub use extractous_base::{memory, text, unicode};

#[cfg(feature = "tika")]
mod unpack;
#[cfg(feature = "tika")]
pub use unpack::*;

// extractor module is the main public api interface
mod extractor;
pub use extractor::*;

mod ocr;
pub use ocr::tesseract_version;

mod bundle;
pub use bundle::{bundled_ocr_licenses, linked_native_licenses};

mod fetch;
mod formats;
/// The tests count allocations as the command line does, so that budgets are enforced in them.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<std::alloc::System> =
    memory::CountingAllocator(std::alloc::System);
mod mime;

mod isolate;
#[cfg(feature = "tika")]
pub use isolate::keep_current_thread_attached;
pub use isolate::{configure_isolate, wait_for_extractions};

// tika module, not exposed outside this crate
#[cfg(feature = "tika")]
mod tika {
    mod jni_utils;
    pub(crate) mod native;
    mod parse;
    mod wrappers;
    pub(crate) use extractous_base::tika::EmbeddedEvents;
    #[cfg(test)]
    pub(crate) use extractous_base::tika::EmbeddedOutcome;
    pub(crate) use parse::*;
    pub(crate) use wrappers::JInputStream;
}
