//! What Tika's Office parsers read from OLE2 compound documents, which [`crate::ole2`] opens as
//! POI does: the HPSF property sets of the summary information with Tika's `SummaryExtractor`
//! mapping into metadata.

mod codepage;
mod property_set;
mod reader;
mod summary;

pub(crate) use summary::{add_multi, parse_summaries};
