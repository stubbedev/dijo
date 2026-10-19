//! dijo's native parsers of email (RFC 822, mbox, emlx), archives and compressed files, web
//! archives (WARC, WACZ), CMS signed data and SQLite databases.

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
    pub(crate) use dijo_html::formats::{html, text};

    pub mod archive;
    pub mod cms;
    pub mod mail;
    pub mod sqlite;
    pub mod warc;
}
