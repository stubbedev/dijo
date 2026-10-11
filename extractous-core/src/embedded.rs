use crate::errors::{Error, ExtractResult};
use std::collections::HashMap;

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

/// Represents an embedded document extracted from a container file
#[derive(Debug, Clone)]
pub struct EmbeddedDocument {
    /// The name/path of the embedded resource
    pub resource_name: String,
    /// MIME type of the embedded content
    pub content_type: String,
    /// The actual content bytes
    pub content: Vec<u8>,
    /// Optional relationship ID (for formats like OOXML)
    pub embedded_relationship_id: Option<String>,
}

/// Result of embedded document extraction
#[derive(Debug)]
pub struct EmbeddedExtractResult {
    /// List of extracted embedded documents
    pub documents: Vec<EmbeddedDocument>,
    /// Metadata from the parent document
    pub metadata: HashMap<String, Vec<String>>,
}

impl EmbeddedDocument {
    /// Save the embedded document to a file
    pub fn save_to_file(&self, path: &str) -> std::io::Result<()> {
        use std::fs;
        use std::path::Path;

        // Create parent directories if needed
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, &self.content)
    }

    /// Get the size of the embedded content in bytes
    pub fn size(&self) -> usize {
        self.content.len()
    }

    /// Check if this is likely an image based on content type
    pub fn is_image(&self) -> bool {
        self.content_type.starts_with("image/")
    }

    /// Check if this is likely a document based on content type
    pub fn is_document(&self) -> bool {
        matches!(
            self.content_type.as_str(),
            "application/pdf"
                | "application/msword"
                | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                | "application/vnd.ms-excel"
                | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                | "application/vnd.ms-powerpoint"
                | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        ) || self.content_type.starts_with("text/")
    }
}

impl EmbeddedExtractResult {
    /// Get only image documents
    pub fn images(&self) -> Vec<&EmbeddedDocument> {
        self.documents.iter().filter(|doc| doc.is_image()).collect()
    }

    /// Get only non-image documents
    pub fn non_images(&self) -> Vec<&EmbeddedDocument> {
        self.documents
            .iter()
            .filter(|doc| !doc.is_image())
            .collect()
    }

    /// Get total size of all embedded documents
    pub fn total_size(&self) -> usize {
        self.documents.iter().map(|doc| doc.size()).sum()
    }

    /// Save all embedded documents to a directory
    pub fn save_all_to_directory(&self, base_dir: &str) -> ExtractResult<()> {
        use std::fs;
        use std::path::Path;

        // Create base directory
        fs::create_dir_all(base_dir).map_err(|e| Error::IoError(e.to_string()))?;

        for (index, doc) in self.documents.iter().enumerate() {
            let filename = if doc.resource_name.is_empty() {
                format!("embedded_{}", index)
            } else {
                doc.resource_name.clone()
            };

            let file_path = Path::new(base_dir).join(&filename);
            doc.save_to_file(file_path.to_str().unwrap())
                .map_err(|e| Error::IoError(e.to_string()))?;
        }

        Ok(())
    }
}
