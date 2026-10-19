//! dijo's native parsers of OpenDocument files, packaged and flat, and of EPUB books.

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

    pub mod epub;
    #[cfg(test)]
    mod large;
    pub mod odf;
    mod zip_package;
}
