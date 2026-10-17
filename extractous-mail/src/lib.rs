//! extractous's native parsers of email (RFC 822, mbox, emlx), archives and compressed files, web
//! archives (WARC, WACZ), CMS signed data and SQLite databases.

use extractous_base::*;
#[cfg(test)]
use extractous_core::Extractor;
use extractous_mime::mime;

/// The tests count allocations as the command line does, so that budgets are enforced in them.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: memory::CountingAllocator<std::alloc::System> =
    memory::CountingAllocator(std::alloc::System);

/// The parsers, at the paths they have in `extractous-core`.
pub mod formats {
    pub(crate) use extractous_base::formats::*;
    #[cfg(test)]
    pub(crate) use extractous_core::parity;
    pub(crate) use extractous_html::formats::{html, text};
    #[cfg(all(test, feature = "tika"))]
    pub(crate) use extractous_small::formats::dc_xml;

    pub mod archive;
    pub mod cms;
    pub mod mail;
    #[cfg(not(feature = "static-image"))]
    pub mod sqlite;
    pub mod warc;
}
