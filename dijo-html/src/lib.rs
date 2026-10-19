//! dijo's native parsers of HTML (Tika's `JSoupParser`) and of plain text and CSV (Tika's
//! `TXTParser` and `TextAndCSVParser`), which other formats hand their HTML and text bodies to.

use dijo_base::*;
#[cfg(test)]
use dijo_core::Extractor;
use dijo_mime::mime;

/// The tests count allocations as the command line does, so that budgets are enforced in them.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<std::alloc::System> =
    memory::CountingAllocator(std::alloc::System);

/// The parsers, at the paths they have in `dijo-core`.
pub mod formats {
    pub(crate) use dijo_base::formats::*;

    pub mod data_uri;
    pub mod html;
    #[cfg(test)]
    pub mod reference;
    pub mod text;
}
