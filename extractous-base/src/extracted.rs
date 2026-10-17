//! What an extraction gives: the document's metadata, and the character set of its text.

use std::collections::HashMap;
use strum_macros::{Display, EnumString};

/// Metadata type alias
pub type Metadata = HashMap<String, Vec<String>>;

/// CharSet enum of all supported encodings
#[derive(Debug, Clone, Default, Copy, PartialEq, Eq, Hash, Display, EnumString)]
#[allow(non_camel_case_types)]
pub enum CharSet {
    #[default]
    #[strum(serialize = "UTF-8")]
    UTF_8,
    #[strum(serialize = "US-ASCII")]
    US_ASCII,
    #[strum(serialize = "UTF-16BE")]
    UTF_16BE,
}
