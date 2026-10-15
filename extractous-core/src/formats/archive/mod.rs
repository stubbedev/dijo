//! Archives and compressed files as Tika 4.1's package module reads them, each entry handed to
//! the embedded document extractor: ZIP archives with `ZipParser` ([`zip`]), compressed files
//! with `CompressorParser` ([`compressor`]).

mod compressor;
mod zip;

pub(crate) use compressor::CompressorParser;
pub(crate) use zip::ZipParser;

#[cfg(test)]
mod tests;
