//! What Tika's Office parsers read from OLE2 compound documents, which [`crate::ole2`] opens as
//! POI does: the HPSF property sets of the summary information with Tika's `SummaryExtractor`
//! mapping into metadata, and the documents stored in the OLE objects Office Open XML documents
//! embed.

mod codepage;
pub(crate) mod object;
mod ole10_native;
mod property_set;
mod reader;
mod summary;

pub(crate) use summary::{add_multi, parse_summaries};
