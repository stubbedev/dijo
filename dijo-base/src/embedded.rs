//! Where documents embedded in an extracted one are copied, for the caller to extract.

/// Where documents embedded in an extracted one are copied, and how many bytes they may take in
/// all. With one, an extraction's text excludes its embedded documents, which are spooled for
/// the caller to extract in their own right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedSpool {
    pub dir: std::path::PathBuf,
    pub byte_budget: u64,
}

/// A document embedded in an extracted one and copied to its own file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpooledEmbedded {
    /// The name the container gives it, or `embedded-N`.
    pub name: String,
    /// The media type the container declares, if any.
    pub content_type: Option<String>,
    pub path: std::path::PathBuf,
    pub size: u64,
    /// The byte budget ran out while copying it; only its start was kept.
    pub truncated: bool,
}
