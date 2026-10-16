//! The types Tika's path parses with `EmptyParser`: those no parser of the native image's
//! composite parser takes, by the type or any type it specializes, so the composite falls back
//! to `EmptyParser`, which writes an empty document. The native path writes the same empty
//! document without starting the JVM. [`types`] lists them, as `just empty-parser-types`
//! derives them from Tika's own registry.

mod types;

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
        &types::EMPTY
    }

    /// A type with parameters may have a parser in Tika's path the type without them has not,
    /// as `image/vnd.dgn; version=8` has, so only the types listed are taken.
    fn takes_base_types(&self) -> bool {
        false
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
