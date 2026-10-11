use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use http_body_util::BodyExt;
use hyper::body::Incoming;
use tokio::io::AsyncWriteExt;

use crate::server::metrics::Metrics;

const DEFAULT_NAME: &str = "upload";
const MAX_NAME_BYTES: usize = 255;

/// A request body spooled to its own directory, which is removed when this is dropped.
pub struct Upload {
    dir: PathBuf,
    path: PathBuf,
    display: String,
}

impl Upload {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The name the document is reported under: the client's file name hint, or `upload`.
    pub fn display(&self) -> &str {
        &self.display
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.dir) {
            eprintln!(
                "extractous: Failed to remove {}: {error}",
                self.dir.display()
            );
        }
    }
}

pub enum UploadError {
    TooLarge,
    Body(hyper::Error),
    Io(io::Error),
}

impl From<io::Error> for UploadError {
    fn from(error: io::Error) -> Self {
        UploadError::Io(error)
    }
}

/// The last component of a client-supplied file name, if it is a usable file name on its own.
pub fn file_name(hint: &str) -> Option<String> {
    let name = Path::new(hint).file_name()?.to_str()?;
    (!name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.contains('\0'))
        .then(|| name.to_string())
}

pub async fn receive(
    mut body: Incoming,
    root: &Path,
    id: u64,
    name: Option<String>,
    max_bytes: Option<u64>,
    metrics: &Metrics,
) -> Result<Upload, UploadError> {
    let dir = root.join(format!("request-{id}"));
    tokio::fs::create_dir(&dir).await?;
    let display = name.unwrap_or_else(|| DEFAULT_NAME.to_string());
    let upload = Upload {
        path: dir.join(&display),
        dir,
        display,
    };
    let mut file = tokio::fs::File::create_new(&upload.path).await?;
    let mut received: u64 = 0;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(UploadError::Body)?;
        if let Some(data) = frame.data_ref() {
            received += data.len() as u64;
            if max_bytes.is_some_and(|max| received > max) {
                return Err(UploadError::TooLarge);
            }
            file.write_all(data).await?;
        }
    }
    file.flush().await?;
    metrics.bytes_in(received);
    Ok(upload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_cannot_leave_the_upload_directory() {
        assert_eq!(file_name("report.pdf"), Some("report.pdf".to_string()));
        assert_eq!(file_name("../../etc/passwd"), Some("passwd".to_string()));
        assert_eq!(file_name("/abs/dir/a.docx"), Some("a.docx".to_string()));
        assert_eq!(file_name(".."), None);
        assert_eq!(file_name(""), None);
    }
}
