//! extractous's OCR, the native readings of tesseract that images and pages are read with, and the
//! native parsers of images, Windows metafiles and DjVu documents.

use extractous_base::*;
#[cfg(test)]
use extractous_core::Extractor;
use extractous_mime::mime;
use extractous_native as bundle;

mod jxl;
pub mod ocr;

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

    pub mod djvu;
    pub mod image;
    pub mod metafile;
}
