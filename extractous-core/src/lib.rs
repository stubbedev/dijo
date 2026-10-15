//! Text and metadata extraction through Apache Tika compiled to native code with GraalVM. This
//! is the implementation behind the `extractous` command line; its API is internal and changes
//! freely.
// errors module
mod errors;
pub use errors::*;

// config module
mod config;
pub use config::*;

// embedded module for embedded document extraction
mod embedded;
pub use embedded::*;

// extractor module is the main public api interface
mod extractor;
pub use extractor::*;

mod ocr;
pub use ocr::tesseract_version;

mod licenses;
pub use licenses::{bundled_ocr_licenses, linked_native_licenses};

mod formats;
mod mime;
pub mod text;
pub mod unicode;
mod xml;

mod isolate;
pub use isolate::{
    configure_isolate, keep_current_thread_attached, wait_for_extractions, IsolateSettings,
};

// tika module, not exposed outside this crate
mod tika {
    mod jni_utils;
    pub(crate) mod native;
    mod parse;
    mod wrappers;
    pub(crate) use parse::*;
    pub(crate) use wrappers::{EmbeddedOutcome, JInputStream};
}
