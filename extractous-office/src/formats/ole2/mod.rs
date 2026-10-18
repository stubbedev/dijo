//! What Tika's Office parsers read from OLE2 compound documents, which [`crate::ole2`] opens as
//! POI does: the HPSF property sets of the summary information with Tika's `SummaryExtractor`
//! mapping into metadata, and the embedded objects of its storages.

mod codepage;
mod embedded;
pub mod office;
mod property_set;
mod reader;
mod summary;

pub(crate) use codepage::{decode_text as decode_codepage, encoding as codepage_encoding};
pub(crate) use embedded::{document_type, office_document, Embedded, Handed};
pub use summary::{add_multi, Summaries};
