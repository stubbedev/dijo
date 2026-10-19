//! dijo's native parsers of Office Open XML packages, Word, Excel (XML and binary), PowerPoint and
//! Visio, of password protected packages, and of XPS documents.

use dijo_base::*;
#[cfg(test)]
use dijo_core::Extractor;
use dijo_mime::mime;

/// The tests count allocations as the command line does, so that budgets are enforced in them,
/// and measure the peak of what each thread holds.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<dijo_core::large::Peak<std::alloc::System>> =
    memory::CountingAllocator(dijo_core::large::Peak(std::alloc::System));

/// The parsers, at the paths they have in `dijo-core`.
pub mod formats {
    pub(crate) use dijo_base::formats::*;
    #[cfg(test)]
    pub(crate) use dijo_core::testing;
    pub(crate) use dijo_image::formats::metafile;
    pub(crate) use dijo_office::formats::ole2;

    pub mod docx;
    pub mod ooxml;
    pub mod pptx;
    pub mod protected;
    pub mod vsdx;
    pub mod xlsx;
    pub mod xps;
}
