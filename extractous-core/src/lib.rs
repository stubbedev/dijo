//! Text and metadata extraction by native parsers that reproduce Apache Tika's output, and with
//! the `tika` feature, by Apache Tika compiled to native code with GraalVM for what no native
//! parser reads yet; without it, such a document fails with why, an [`Unsupported`]. This is the
//! implementation behind the `extractous` command line; its API is internal and changes freely.
// errors module
mod errors;
pub use errors::*;

mod compress;

// config module
mod config;
pub use config::*;

mod embedded;
pub use embedded::*;

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
mod hash;
mod input;
pub mod memory;
/// The tests count allocations as the command line does, so that budgets are enforced in them.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<std::alloc::System> =
    memory::CountingAllocator(std::alloc::System);
mod mime;
mod ole2;
pub mod text;
pub mod unicode;
mod xml;

mod isolate;
#[cfg(feature = "tika")]
pub use isolate::keep_current_thread_attached;
pub use isolate::{configure_isolate, wait_for_extractions, IsolateSettings};

mod java;

// tika module, not exposed outside this crate
#[cfg(feature = "tika")]
mod tika {
    mod events;
    mod jni_utils;
    pub(crate) mod native;
    mod parse;
    mod wrappers;
    pub(crate) use events::{decode as decode_events, Event};
    pub(crate) use parse::*;
    pub(crate) use wrappers::{EmbeddedEvents, EmbeddedOutcome, JInputStream};
}
