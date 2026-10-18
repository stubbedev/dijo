//! extractous's native parsers of Microsoft Office's binary (OLE2) formats: Word, Excel, PowerPoint,
//! Visio, Publisher and Access documents, Outlook messages and PST/OST files, TNEF, RTF, and the OLE2
//! embedding they share.

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
    pub(crate) use extractous_html::formats::html;
    pub(crate) use extractous_mail::formats::mail;

    pub mod access;
    pub mod compound;
    pub mod dgn;
    pub mod doc;
    #[cfg(test)]
    mod encrypted;
    pub mod escher;
    pub mod msg;
    pub mod ole2;
    pub mod poi;
    pub mod ppt;
    pub mod pst;
    pub mod publisher;
    pub mod rtf;
    pub mod tnef;
    pub mod visio;
    pub mod xls;
}
