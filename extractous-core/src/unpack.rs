//! The documents embedded in a document, unpacked whole by Tika's path, as Tika Server's
//! `/unpack/all` endpoint unpacks them.

use crate::errors::{Error, ExtractResult};
use std::collections::HashMap;

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
            let filename = safe_relative_path(&doc.resource_name)
                .unwrap_or_else(|| format!("embedded_{}", index).into());

            let file_path = Path::new(base_dir).join(&filename);
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::IoError(e.to_string()))?;
            }
            doc.save_to_file(file_path.to_str().unwrap())
                .map_err(|e| Error::IoError(e.to_string()))?;
        }

        Ok(())
    }
}

/// A resource name as a path below a directory on this platform, or `None` when nothing of it
/// is left: its components split at `/`, which archive formats separate paths with, empty, `.`
/// and `..` ones dropped so it cannot leave the directory, and each character a file name on
/// this platform cannot hold, `\\` among them on Windows, made `_`.
fn safe_relative_path(resource_name: &str) -> Option<std::path::PathBuf> {
    let invalid =
        |c: char| c == '\0' || (cfg!(windows) && (c.is_control() || "<>:\"\\|?*".contains(c)));
    let path: std::path::PathBuf = resource_name
        .split('/')
        .filter(|component| !matches!(*component, "" | "." | ".."))
        .map(|component| component.replace(invalid, "_"))
        .collect();
    (!path.as_os_str().is_empty()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_saved_documents_inside_the_directory() {
        let path =
            |name: &str| safe_relative_path(name).map(|path| path.to_string_lossy().into_owned());
        assert_eq!(path(""), None);
        assert_eq!(path("../.."), None);
        assert_eq!(
            path("/etc/passwd").as_deref(),
            Some(if cfg!(windows) {
                "etc\\passwd"
            } else {
                "etc/passwd"
            })
        );
        assert_eq!(
            path("a/../b.txt").as_deref(),
            Some(if cfg!(windows) { "a\\b.txt" } else { "a/b.txt" })
        );
        assert_eq!(
            path("back\\slash.txt").as_deref(),
            Some(if cfg!(windows) {
                "back_slash.txt"
            } else {
                "back\\slash.txt"
            })
        );
    }
}
