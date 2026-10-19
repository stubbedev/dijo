//! extractous's native parser of PDF documents, a port of PDFBox's text extraction as Tika's
//! `PDFParser` runs it, and the font parsers built on its font readers.

use extractous_base::*;
#[cfg(test)]
use extractous_core::Extractor;
use extractous_image::ocr;

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
    pub(crate) use extractous_core::testing;
    pub(crate) use extractous_image::formats::image;

    pub mod font;
    pub mod pdf;
}
