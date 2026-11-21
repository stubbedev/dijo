use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use tokio::io::AsyncWriteExt;

use crate::server::handlers::Files;
use crate::server::metrics::Metrics;

pub const DEFAULT_NAME: &str = "upload";
const UNTYPED: &str = "application/octet-stream";
const MAX_NAME_BYTES: usize = 255;
const MAX_DISCARDED_BYTES: u64 = 16 << 20;
/// The longest boundary RFC 2046 allows.
const MAX_BOUNDARY_BYTES: usize = 70;
const MAX_PART_HEADER_BYTES: usize = 16 << 10;
/// The largest configuration part read, which is a few hundred bytes of JSON in practice.
pub const MAX_CONFIG_BYTES: usize = 1 << 20;
const BUFFER_BYTES: usize = 64 << 10;

/// A request's own directory in the spool, removed with everything in it when this is dropped.
struct SpoolDir(PathBuf);

impl SpoolDir {
    async fn create(root: &Path, id: u64) -> io::Result<Self> {
        let dir = root.join(format!("request-{id}"));
        tokio::fs::create_dir(&dir).await?;
        Ok(SpoolDir(dir))
    }
}

impl Drop for SpoolDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("dijo: Failed to remove {}: {error}", self.0.display());
        }
    }
}

/// A document spooled to its own directory, which is removed when this is dropped.
pub struct Upload {
    dir: SpoolDir,
    path: PathBuf,
    display: String,
}

impl Upload {
    /// A directory, inside this upload's own, for documents embedded in it.
    pub fn scratch(&self) -> PathBuf {
        self.dir.0.join("embedded")
    }

    pub fn files(&self) -> Files {
        Files {
            path: self.path.clone(),
            display: self.display.clone(),
            scratch: self.scratch(),
        }
    }
}

pub enum UploadError {
    TooLarge,
    /// The client sent nothing for longer than it may.
    Stalled,
    Body(hyper::Error),
    Io(io::Error),
    /// A multipart body that does not follow RFC 7578.
    Malformed(&'static str),
    /// A multipart body without a part that is the document.
    MissingDocument,
    /// A multipart body whose configuration part exceeds [`MAX_CONFIG_BYTES`].
    ConfigTooLarge,
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

/// The file name a `Content-Disposition` header gives, such as `attachment; filename="a.pdf"`,
/// as Tika clients name an upload.
pub fn disposition_file_name(disposition: &str) -> Option<String> {
    disposition_parameter(disposition, "filename")
}

/// A parameter of a `Content-Disposition` value, a quoted string unquoted.
fn disposition_parameter(disposition: &str, wanted: &str) -> Option<String> {
    let mut rest = disposition.split_once(';')?.1;
    loop {
        let (name, after) = rest.split_once('=')?;
        let after = after.trim_start();
        let (value, next) = match after.strip_prefix('"') {
            Some(quoted) => {
                let mut value = String::new();
                let mut chars = quoted.char_indices();
                let end = loop {
                    match chars.next()? {
                        (_, '\\') => value.push(chars.next()?.1),
                        (at, '"') => break at + 1,
                        (_, other) => value.push(other),
                    }
                };
                let next = quoted[end..].split_once(';').map(|(_, next)| next);
                (value, next)
            }
            None => match after.split_once(';') {
                Some((value, next)) => (value.trim().to_string(), Some(next)),
                None => (after.trim().to_string(), None),
            },
        };
        if name.trim().eq_ignore_ascii_case(wanted) {
            return Some(value);
        }
        rest = next?;
    }
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

/// A request body read frame by frame, within the server's limits on its size and on how long
/// the client may send nothing.
struct BodyReader {
    body: Incoming,
    received: u64,
    max_bytes: Option<u64>,
    idle_timeout: Duration,
}

impl BodyReader {
    fn new(body: Incoming, max_bytes: Option<u64>, idle_timeout: Duration) -> Self {
        BodyReader {
            body,
            received: 0,
            max_bytes,
            idle_timeout,
        }
    }

    /// The next bytes of the body; None at its end.
    async fn next(&mut self) -> Result<Option<Bytes>, UploadError> {
        loop {
            let Some(frame) = tokio::time::timeout(self.idle_timeout, self.body.frame())
                .await
                .map_err(|_| UploadError::Stalled)?
            else {
                return Ok(None);
            };
            let Ok(data) = frame.map_err(UploadError::Body)?.into_data() else {
                continue;
            };
            self.received += data.len() as u64;
            if self.max_bytes.is_some_and(|max| self.received > max) {
                discard(&mut self.body, self.idle_timeout).await;
                return Err(UploadError::TooLarge);
            }
            return Ok(Some(data));
        }
    }
}

/// Where and within which limits a request's body is spooled.
pub struct Spooling<'s> {
    pub root: &'s Path,
    pub id: u64,
    pub max_bytes: Option<u64>,
    pub idle_timeout: Duration,
    pub metrics: &'s Metrics,
}

impl Spooling<'_> {
    fn reader(&self, body: Incoming) -> BodyReader {
        BodyReader::new(body, self.max_bytes, self.idle_timeout)
    }
}

pub async fn receive(
    body: Incoming,
    name: Name,
    spooling: &Spooling<'_>,
) -> Result<Upload, UploadError> {
    let dir = SpoolDir::create(spooling.root, spooling.id).await?;
    let upload = Upload {
        path: dir.0.join(&name.file),
        dir,
        display: name.display,
    };
    let mut file = tokio::fs::File::create_new(&upload.path).await?;
    let mut reader = spooling.reader(body);
    while let Some(data) = reader.next().await? {
        file.write_all(&data).await?;
    }
    file.flush().await?;
    spooling.metrics.bytes_in(reader.received);
    Ok(upload)
}

/// Which parts of a `multipart/form-data` body tika-server reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parts {
    /// Its `/form` endpoints: the first part is the document.
    Form,
    /// Its `/config` endpoints: the last part named `file`, or else the first unnamed part, is
    /// the document, and the last part named `config` its configuration.
    Config,
}

/// What a part of a multipart body is to the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Document,
    Config,
    Ignored,
}

impl Parts {
    fn role(self, head: &PartHead, first: bool, has_document: bool) -> Role {
        match self {
            Parts::Form if first => Role::Document,
            Parts::Form => Role::Ignored,
            Parts::Config => match head.name.as_deref() {
                Some("file") => Role::Document,
                Some("config") => Role::Config,
                _ if head.content_id.as_deref() == Some("config") => Role::Config,
                None if !has_document => Role::Document,
                _ => Role::Ignored,
            },
        }
    }
}

/// A multipart upload: its document, and its configuration part when the endpoint reads one.
pub struct Multipart {
    pub upload: Upload,
    pub config: Option<Vec<u8>>,
}

/// The boundary of a `multipart/form-data` media type, unquoted.
pub fn form_data_boundary(content_type: &str) -> Option<&str> {
    let mut parameters = content_type.split(';');
    let base = parameters.next()?.trim();
    if !base.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    parameters.find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(value);
        (name.trim().eq_ignore_ascii_case("boundary")
            && !value.is_empty()
            && value.len() <= MAX_BOUNDARY_BYTES)
            .then_some(value)
    })
}

/// Spools the document part of a `multipart/form-data` body and keeps its configuration part,
/// as `parts` picks them out; other parts are read and dropped.
pub async fn receive_multipart(
    body: Incoming,
    boundary: &str,
    parts: Parts,
    spooling: &Spooling<'_>,
) -> Result<Multipart, UploadError> {
    let dir = SpoolDir::create(spooling.root, spooling.id).await?;
    let mut reader = spooling.reader(body);
    let mut parser = MultipartParser::new(boundary);
    let mut document: Option<(PathBuf, String)> = None;
    let mut config = None;
    let mut part = Part::Ignored;
    let mut first = true;
    loop {
        match parser.next_event()? {
            Some(Event::Start(head)) => {
                part = match parts.role(&head, first, document.is_some()) {
                    Role::Document => {
                        if let Some((path, _)) = document.take() {
                            tokio::fs::remove_file(&path).await?;
                        }
                        let name = Name::from_hints(
                            head.file_name.as_deref(),
                            head.content_type.as_deref(),
                        );
                        let path = dir.0.join(&name.file);
                        let file = tokio::fs::File::create_new(&path).await?;
                        document = Some((path, name.display));
                        Part::Document(file)
                    }
                    Role::Config => Part::Config(Vec::new()),
                    Role::Ignored => Part::Ignored,
                };
                first = false;
            }
            Some(Event::Data(data)) => match &mut part {
                Part::Document(file) => file.write_all(&data).await?,
                Part::Config(bytes) => {
                    if bytes.len() + data.len() > MAX_CONFIG_BYTES {
                        return Err(UploadError::ConfigTooLarge);
                    }
                    bytes.extend_from_slice(&data);
                }
                Part::Ignored => {}
            },
            Some(Event::End) => match std::mem::replace(&mut part, Part::Ignored) {
                Part::Document(mut file) => file.flush().await?,
                Part::Config(bytes) => config = Some(bytes),
                Part::Ignored => {}
            },
            None if parser.is_closed() => {
                while reader.next().await?.is_some() {}
                break;
            }
            None => match reader.next().await? {
                Some(data) => parser.feed(&data),
                None => {
                    return Err(UploadError::Malformed(
                        "it ends before its closing boundary",
                    ))
                }
            },
        }
    }
    spooling.metrics.bytes_in(reader.received);
    let (path, display) = document.ok_or(UploadError::MissingDocument)?;
    Ok(Multipart {
        upload: Upload { dir, path, display },
        config,
    })
}

/// Where the bytes of the part being read go.
enum Part {
    Document(tokio::fs::File),
    Config(Vec<u8>),
    Ignored,
}

/// The headers of a part that tika-server reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PartHead {
    name: Option<String>,
    file_name: Option<String>,
    content_type: Option<String>,
    content_id: Option<String>,
}

impl PartHead {
    fn parse(block: &[u8]) -> Result<Self, UploadError> {
        let block = std::str::from_utf8(block)
            .map_err(|_| UploadError::Malformed("a part's headers are not UTF-8"))?;
        let mut head = PartHead::default();
        for line in block.split("\r\n").filter(|line| !line.is_empty()) {
            let (name, value) = line
                .split_once(':')
                .ok_or(UploadError::Malformed("a part's header has no colon"))?;
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "content-disposition" => {
                    head.name = disposition_parameter(value, "name");
                    head.file_name = disposition_parameter(value, "filename");
                }
                "content-type" => head.content_type = Some(value.to_string()),
                "content-id" => {
                    head.content_id = Some(
                        value
                            .strip_prefix('<')
                            .and_then(|value| value.strip_suffix('>'))
                            .unwrap_or(value)
                            .to_string(),
                    )
                }
                _ => {}
            }
        }
        Ok(head)
    }
}

/// What reading a multipart body comes to next.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Start(PartHead),
    Data(Bytes),
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    Preamble,
    /// Just after a boundary, before the line end or the `--` that closes the body.
    Boundary,
    Headers,
    Body,
    Closed,
}

/// Splits a `multipart/form-data` body into its parts as it arrives, keeping no more of it than
/// the longest boundary or header block it has to find.
struct MultipartParser {
    buffer: BytesMut,
    delimiter: Vec<u8>,
    position: Position,
}

impl MultipartParser {
    fn new(boundary: &str) -> Self {
        let delimiter = [b"\r\n--", boundary.as_bytes()].concat();
        let mut buffer = BytesMut::with_capacity(BUFFER_BYTES);
        buffer.extend_from_slice(b"\r\n");
        MultipartParser {
            buffer,
            delimiter,
            position: Position::Preamble,
        }
    }

    fn feed(&mut self, data: &[u8]) {
        if self.position != Position::Closed {
            self.buffer.extend_from_slice(data);
        }
    }

    fn is_closed(&self) -> bool {
        self.position == Position::Closed
    }

    /// The next event of the body fed so far; None when it needs more of it.
    fn next_event(&mut self) -> Result<Option<Event>, UploadError> {
        loop {
            match self.position {
                Position::Closed => return Ok(None),
                Position::Preamble => {
                    let Some(at) = memchr::memmem::find(&self.buffer, &self.delimiter) else {
                        let keep = self.delimiter.len() - 1;
                        if self.buffer.len() > keep {
                            let _ = self.buffer.split_to(self.buffer.len() - keep);
                        }
                        return Ok(None);
                    };
                    let _ = self.buffer.split_to(at + self.delimiter.len());
                    self.position = Position::Boundary;
                }
                Position::Boundary => {
                    if self.buffer.starts_with(b"--") {
                        self.position = Position::Closed;
                        self.buffer.clear();
                        return Ok(None);
                    }
                    let Some(end) = memchr::memmem::find(&self.buffer, b"\r\n") else {
                        if self.buffer.len() > MAX_PART_HEADER_BYTES {
                            return Err(UploadError::Malformed("a boundary line does not end"));
                        }
                        return Ok(None);
                    };
                    if !self.buffer[..end]
                        .iter()
                        .all(|byte| matches!(byte, b' ' | b'\t'))
                    {
                        return Err(UploadError::Malformed(
                            "a boundary line has more than the boundary",
                        ));
                    }
                    let _ = self.buffer.split_to(end + 2);
                    self.position = Position::Headers;
                }
                Position::Headers => {
                    let end = if self.buffer.starts_with(b"\r\n") {
                        Some(0)
                    } else {
                        memchr::memmem::find(&self.buffer, b"\r\n\r\n").map(|end| end + 2)
                    };
                    let Some(end) = end else {
                        if self.buffer.len() > MAX_PART_HEADER_BYTES {
                            return Err(UploadError::Malformed("a part's headers are too long"));
                        }
                        return Ok(None);
                    };
                    let block = self.buffer.split_to(end + 2);
                    self.position = Position::Body;
                    return PartHead::parse(&block[..end]).map(|head| Some(Event::Start(head)));
                }
                Position::Body => {
                    if let Some(at) = memchr::memmem::find(&self.buffer, &self.delimiter) {
                        if at > 0 {
                            return Ok(Some(Event::Data(self.buffer.split_to(at).freeze())));
                        }
                        let _ = self.buffer.split_to(self.delimiter.len());
                        self.position = Position::Boundary;
                        return Ok(Some(Event::End));
                    }
                    let safe = self.buffer.len().saturating_sub(self.delimiter.len() - 1);
                    if safe == 0 {
                        return Ok(None);
                    }
                    return Ok(Some(Event::Data(self.buffer.split_to(safe).freeze())));
                }
            }
        }
    }
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
    fn a_content_disposition_names_the_upload() {
        assert_eq!(
            disposition_file_name("attachment; filename=\"report.pdf\"").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(
            disposition_file_name("attachment;FILENAME=a.docx; size=3").as_deref(),
            Some("a.docx")
        );
        assert_eq!(
            disposition_file_name(r#"form-data; name="file"; filename="a;\"b\".pdf""#).as_deref(),
            Some(r#"a;"b".pdf"#)
        );
        assert_eq!(disposition_file_name("attachment"), None);
        assert_eq!(disposition_file_name("filename=a.pdf"), None);
    }

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

    fn events(boundary: &str, body: &[u8], chunk: usize) -> Result<Vec<Event>, &'static str> {
        let mut parser = MultipartParser::new(boundary);
        let mut events = Vec::new();
        let mut chunks = body.chunks(chunk);
        loop {
            match parser.next_event() {
                Err(UploadError::Malformed(reason)) => return Err(reason),
                Err(_) => return Err("unexpected error"),
                Ok(Some(Event::Data(data))) => match events.last_mut() {
                    Some(Event::Data(last)) => {
                        *last = Bytes::from([last.as_ref(), data.as_ref()].concat())
                    }
                    _ => events.push(Event::Data(data)),
                },
                Ok(Some(event)) => events.push(event),
                Ok(None) if parser.is_closed() => return Ok(events),
                Ok(None) => match chunks.next() {
                    Some(chunk) => parser.feed(chunk),
                    None => return Err("it ends before its closing boundary"),
                },
            }
        }
    }

    const BODY: &[u8] = b"preamble\r\n--XyZ\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"a.pdf\"\r\n\
Content-Type: application/pdf\r\n\r\n\
%PDF\r\n--Xy not yet\r\n\
--XyZ  \r\n\
Content-Disposition: form-data; name=\"config\"\r\n\
Content-Type: application/json\r\n\r\n\
{}\r\n--XyZ--\r\nepilogue";

    #[test]
    fn multipart_bodies_split_into_parts_however_they_arrive() {
        let expected = vec![
            Event::Start(PartHead {
                name: Some("file".to_string()),
                file_name: Some("a.pdf".to_string()),
                content_type: Some("application/pdf".to_string()),
                content_id: None,
            }),
            Event::Data(Bytes::from_static(b"%PDF\r\n--Xy not yet")),
            Event::End,
            Event::Start(PartHead {
                name: Some("config".to_string()),
                file_name: None,
                content_type: Some("application/json".to_string()),
                content_id: None,
            }),
            Event::Data(Bytes::from_static(b"{}")),
            Event::End,
        ];
        for chunk in [1, 2, 3, 7, 64, BODY.len()] {
            assert_eq!(events("XyZ", BODY, chunk), Ok(expected.clone()), "{chunk}");
        }
    }

    #[test]
    fn malformed_multipart_bodies_are_refused() {
        assert_eq!(
            events("XyZ", b"--XyZ\r\n\r\nno end", 4),
            Err("it ends before its closing boundary")
        );
        assert_eq!(
            events("XyZ", b"--XyZ junk\r\n\r\n\r\n--XyZ--", 4),
            Err("a boundary line has more than the boundary")
        );
        assert_eq!(
            events("XyZ", b"--XyZ\r\nno colon\r\n\r\n\r\n--XyZ--", 4),
            Err("a part's header has no colon")
        );
        assert_eq!(events("XyZ", b"--XyZ--", 4), Ok(Vec::new()));
    }

    #[test]
    fn config_endpoints_pick_the_document_and_config_parts_as_tika_server_does() {
        let head = |name: Option<&str>, content_id: Option<&str>| PartHead {
            name: name.map(str::to_string),
            content_id: content_id.map(str::to_string),
            ..PartHead::default()
        };
        assert_eq!(
            Parts::Config.role(&head(Some("file"), None), false, true),
            Role::Document
        );
        assert_eq!(
            Parts::Config.role(&head(None, None), true, false),
            Role::Document
        );
        assert_eq!(
            Parts::Config.role(&head(None, None), false, true),
            Role::Ignored
        );
        assert_eq!(
            Parts::Config.role(&head(Some("config"), None), true, false),
            Role::Config
        );
        assert_eq!(
            Parts::Config.role(&head(None, Some("config")), true, false),
            Role::Config
        );
        assert_eq!(
            Parts::Config.role(&head(Some("other"), None), true, false),
            Role::Ignored
        );
        assert_eq!(
            Parts::Form.role(&head(Some("config"), None), true, false),
            Role::Document
        );
        assert_eq!(
            Parts::Form.role(&head(Some("file"), None), false, true),
            Role::Ignored
        );
    }

    #[test]
    fn only_form_data_names_a_boundary() {
        assert_eq!(
            form_data_boundary("multipart/form-data; boundary=abc"),
            Some("abc")
        );
        assert_eq!(
            form_data_boundary("Multipart/Form-Data;charset=utf-8; BOUNDARY=\"a b\""),
            Some("a b")
        );
        assert_eq!(form_data_boundary("multipart/mixed; boundary=abc"), None);
        assert_eq!(form_data_boundary("multipart/form-data"), None);
        assert_eq!(form_data_boundary("multipart/form-data; boundary="), None);
    }
}
