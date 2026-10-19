//! dijo's native parsers of Microsoft Office's binary (OLE2) formats: Word, Excel, PowerPoint,
//! Visio, Publisher and Access documents, Outlook messages and PST/OST files, TNEF, RTF, and the OLE2
//! embedding they share; and of Hancom Office 5 documents, which are OLE2 compound files too.

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
    pub(crate) use dijo_html::formats::html;
    pub(crate) use dijo_mail::formats::mail;

    pub mod access;
    pub mod compound;
    pub mod dgn;
    pub mod doc;
    #[cfg(test)]
    mod encrypted;
    pub mod escher;
    pub mod hwp;
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
