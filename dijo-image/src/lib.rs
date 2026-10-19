//! dijo's OCR, the native readings of tesseract that images and pages are read with, and the
//! native parsers of images, Windows metafiles and DjVu documents.

use dijo_base::*;
#[cfg(test)]
use dijo_core::Extractor;
use dijo_mime::mime;
use dijo_native as bundle;

mod jxl;
pub mod ocr;

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

    pub mod djvu;
    pub mod image;
    pub mod metafile;
}
