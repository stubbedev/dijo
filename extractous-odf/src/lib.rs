//! extractous's native parsers of OpenDocument files, packaged and flat, and of EPUB books.

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

    pub mod epub;
    #[cfg(test)]
    mod large;
    pub mod odf;
    mod zip_package;
}
