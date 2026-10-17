//! Tika's `EmptyParser`, which `CompositeParser` falls back to for a document of a type none of
//! its parsers takes, by the type or any type it specializes: an empty document, with no
//! metadata of its own. It takes no type itself; [`super::parser_for`] falls back to it where the
//! walk through a document's types finds no parser, native or Tika's, so the native path writes
//! the same empty document without starting the JVM.

use crate::config::ParserConfigs;
use crate::formats::metadata::TikaMetadata;
use crate::formats::{Declined, Parser, Prepared};

/// Tika's `EmptyParser`.
pub(crate) struct EmptyParser;

impl Parser for EmptyParser {
    fn name(&self) -> &'static str {
        "extractous::EmptyParser"
    }

    #[cfg(test)]
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.EmptyParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn prepare<'a>(
        &self,
        _input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Declined> {
        Ok(Box::new(|_| Ok(())))
    }
}

#[cfg(test)]
mod tests;
