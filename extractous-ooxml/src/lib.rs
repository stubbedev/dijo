//! extractous's native parsers of Office Open XML packages, Word, Excel (XML and binary), PowerPoint and
//! Visio, of password protected packages, and of XPS documents.

use extractous_base::*;
#[cfg(test)]
use extractous_core::Extractor;
use extractous_mime::mime;

/// The tests count allocations as the command line does, so that budgets are enforced in them,
/// and measure the peak of what each thread holds.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<extractous_core::large::Peak<std::alloc::System>> =
    memory::CountingAllocator(extractous_core::large::Peak(std::alloc::System));

/// The parsers, at the paths they have in `extractous-core`.
pub mod formats {
    pub(crate) use extractous_base::formats::*;
    #[cfg(test)]
    pub(crate) use extractous_core::parity;
    pub(crate) use extractous_image::formats::metafile;
    pub(crate) use extractous_office::formats::ole2;

    pub mod docx;
    pub mod ooxml;
    pub mod pptx;
    pub mod protected;
    pub mod vsdx;
    pub mod xlsx;
    pub mod xps;
}
