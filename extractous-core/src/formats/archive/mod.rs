//! Archives and compressed files as Tika 4.1's package module reads them, each entry handed to
//! the embedded document extractor: ZIP archives with `ZipParser` ([`zip`]), compressed files
//! with `CompressorParser` ([`compressor`]), TAR, ar and cpio archives with `PackageParser`
//! ([`package`]).

mod compressor;
mod package;
mod zip;

pub(crate) use compressor::CompressorParser;
pub(crate) use package::PackageParser;
pub(crate) use zip::ZipParser;

use crate::formats::xhtml::{Stop, Xhtml};

/// `ZipParser.writeEntryXhtml` and the div of `handleEntryMetadata`: an empty `div` of class
/// `embedded` identified by the entry's name, if it has one.
fn write_entry_div(xhtml: &mut Xhtml, name: &str) -> Result<(), Stop> {
    if name.is_empty() {
        return Ok(());
    }
    xhtml.start_element_with("div", &[("class", "embedded"), ("id", name)])?;
    xhtml.end_element("div")
}

#[cfg(test)]
mod tests;
