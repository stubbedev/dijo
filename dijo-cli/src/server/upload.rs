use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use tokio::io::AsyncWriteExt;

use crate::server::handlers::Files;
use crate::server::metrics::Metrics;

const DEFAULT_NAME: &str = "upload";
const UNTYPED: &str = "application/octet-stream";
const MAX_NAME_BYTES: usize = 255;
const MAX_DISCARDED_BYTES: u64 = 16 << 20;

/// A request body spooled to its own directory, which is removed when this is dropped.
pub struct Upload {
    dir: PathBuf,
    path: PathBuf,
    display: String,
}

impl Upload {
    /// A directory, inside this upload's own, for documents embedded in it.
    pub fn scratch(&self) -> PathBuf {
        self.dir.join("embedded")
    }

    pub fn files(&self) -> Files {
        Files {
            path: self.path.clone(),
            display: self.display.clone(),
            scratch: self.scratch(),
        }
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.dir) {
            eprintln!("dijo: Failed to remove {}: {error}", self.dir.display());
        }
    }
}

pub enum UploadError {
    TooLarge,
    /// The client sent nothing for longer than it may.
    Stalled,
    Body(hyper::Error),
    Io(io::Error),
}

impl From<io::Error> for UploadError {
    fn from(error: io::Error) -> Self {
        UploadError::Io(error)
    }
}

/// The last component of a client-supplied file name, if it is a usable file name on its own.
fn file_name(hint: &str) -> Option<String> {
    let name = Path::new(hint).file_name()?.to_str()?;
    (!name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.contains('\0'))
        .then(|| name.to_string())
}

/// What an upload is called: the name its file has, which detection takes as a hint, and the
/// name it is reported under.
#[derive(Debug, PartialEq, Eq)]
pub struct Name {
    file: String,
    display: String,
}

impl Name {
    /// The client's file name hint, when usable; else a default name, given the extension of the
    /// media type the client declared, when it has one and is more than any bytes at all.
    pub fn from_hints(file_name_hint: Option<&str>, content_type: Option<&str>) -> Self {
        if let Some(name) = file_name_hint.and_then(file_name) {
            return Name {
                file: name.clone(),
                display: name,
            };
        }
        let extension = content_type
            .filter(|declared| {
                let base = declared.split(';').next().unwrap_or_default().trim();
                !base.eq_ignore_ascii_case(UNTYPED)
            })
            .and_then(dijo_core::media_type_extension)
            .unwrap_or_default();
        Name {
            file: format!("{DEFAULT_NAME}{extension}"),
            display: DEFAULT_NAME.to_string(),
        }
    }
}

pub async fn receive(
    mut body: Incoming,
    root: &Path,
    id: u64,
    name: Name,
    max_bytes: Option<u64>,
    idle_timeout: Duration,
    metrics: &Metrics,
) -> Result<Upload, UploadError> {
    let dir = root.join(format!("request-{id}"));
    tokio::fs::create_dir(&dir).await?;
    let upload = Upload {
        path: dir.join(&name.file),
        dir,
        display: name.display,
    };
    let mut file = tokio::fs::File::create_new(&upload.path).await?;
    let mut received: u64 = 0;
    loop {
        let Some(frame) = tokio::time::timeout(idle_timeout, body.frame())
            .await
            .map_err(|_| UploadError::Stalled)?
        else {
            break;
        };
        let frame = frame.map_err(UploadError::Body)?;
        if let Some(data) = frame.data_ref() {
            received += data.len() as u64;
            if max_bytes.is_some_and(|max| received > max) {
                discard(&mut body, idle_timeout).await;
                return Err(UploadError::TooLarge);
            }
            file.write_all(data).await?;
        }
    }
    file.flush().await?;
    metrics.bytes_in(received);
    Ok(upload)
}

/// Reads and drops what is left of a rejected request's body, up to [`MAX_DISCARDED_BYTES`] and
/// for as long as the client sends something every `idle_timeout`. Closing a connection with
/// unread input makes the kernel reset it, and a client may then lose the response before
/// reading it, as macOS drops what it received once the reset arrives; draining first lets the
/// response arrive. Errors only end the draining, since the request has already failed.
pub async fn discard(body: &mut Incoming, idle_timeout: Duration) {
    let mut discarded: u64 = 0;
    while discarded < MAX_DISCARDED_BYTES {
        match tokio::time::timeout(idle_timeout, body.frame()).await {
            Ok(Some(Ok(frame))) => {
                discarded += frame.data_ref().map_or(0, |data| data.len() as u64);
            }
            Ok(Some(Err(_)) | None) | Err(_) => return,
        }
    }
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

    #[test]
    fn a_declared_media_type_names_the_upload_unless_a_file_name_does() {
        let name = |file: &str, display: &str| Name {
            file: file.to_string(),
            display: display.to_string(),
        };
        assert_eq!(
            Name::from_hints(None, Some("application/pdf")),
            name("upload.pdf", "upload")
        );
        assert_eq!(
            Name::from_hints(None, Some("text/csv; charset=utf-8")),
            name("upload.csv", "upload")
        );
        assert_eq!(
            Name::from_hints(Some("a/report.docx"), Some("application/pdf")),
            name("report.docx", "report.docx")
        );
        for unhelpful in [
            None,
            Some("application/octet-stream"),
            Some("Application/Octet-Stream; x=y"),
            Some("application/x-www-form-urlencoded"),
            Some("not a type"),
        ] {
            assert_eq!(Name::from_hints(None, unhelpful), name("upload", "upload"));
        }
        assert_eq!(Name::from_hints(Some(".."), None), name("upload", "upload"));
    }
}
