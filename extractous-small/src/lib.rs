//! extractous's native parsers of the smaller formats: Java class files, executables, fonts' kin,
//! DBF, CHM, MOBI, Markdown, source code, subtitles, feeds, notebooks, iWork, OneNote, WordPerfect,
//! XML formats and the rest, and Tika's empty fallback.

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
    pub(crate) use extractous_html::formats::{data_uri, html};

    pub mod apple_single;
    pub mod chm;
    pub mod class_file;
    pub mod dbf;
    pub mod dc_xml;
    pub mod dif;
    pub mod dwg;
    pub mod empty;
    pub mod executable;
    pub mod feed;
    pub mod fiction_book;
    pub mod geogebra;
    pub mod hwpx;
    pub mod idml;
    pub mod iwork;
    pub mod iwork_package;
    pub mod markdown;
    pub mod mat;
    pub mod mif;
    pub mod mobi;
    pub mod ms_owner;
    pub mod notebook;
    pub mod onenote;
    pub mod open_pgp;
    pub mod plist;
    pub mod prt;
    pub mod sas;
    pub mod source_code;
    pub mod subtitle;
    pub mod translation;
    pub mod tsd;
    pub mod wordperfect;
    pub mod xml_2003;
}
