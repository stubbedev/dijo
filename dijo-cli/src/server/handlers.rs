use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::Instant;

use dijo_core::Password;
use serde::{Deserialize, Serialize};

use crate::cache::{Cache, Hit, Lookup, OutputSettings};
use crate::cli::{Embedded, Format, Ocr};
use crate::normalize::Steps;
use crate::render::{self, RenderOptions};
use crate::schema::{self, ErrorCategory, Source};
use crate::settings;
use crate::tree::{self, Limits};

const BODY_CHUNK_SIZE: usize = 64 * 1024;

pub struct ExtractRequest {
    pub deadline: Option<Instant>,
    pub embedded: Embedded,
    pub limits: Limits,
    pub render: RenderOptions,
    pub ocr: Ocr,
    pub ocr_lang: Option<String>,
    pub passwords: Vec<Password>,
}

pub enum Task {
    Extract(ExtractRequest),
    Detect,
}

/// The files of one request: the spooled upload, the name it is reported under, and a
/// directory for the documents embedded in it.
#[derive(Serialize, Deserialize)]
pub struct Files {
    pub path: PathBuf,
    pub display: String,
    pub scratch: PathBuf,
}

/// The status line and headers of a response, sent before its body. A body that is known up
/// front (an error, a detected type) comes with the head; otherwise it streams afterwards.
#[derive(Debug, Serialize, Deserialize)]
pub struct Head {
    pub status: u16,
    pub content_type: ContentType,
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentType {
    Text,
    Xhtml,
    Json,
    NdJson,
}

impl ContentType {
    pub fn for_format(format: Format) -> Self {
        match format {
            Format::Text => ContentType::Text,
            Format::Xhtml => ContentType::Xhtml,
            Format::Json => ContentType::Json,
            Format::Pages | Format::Elements => ContentType::NdJson,
        }
    }

    pub fn header(self) -> &'static str {
        match self {
            ContentType::Text => "text/plain; charset=utf-8",
            ContentType::Xhtml => "application/xhtml+xml; charset=utf-8",
            ContentType::Json => "application/json",
            ContentType::NdJson => "application/x-ndjson",
        }
    }
}

/// What a request contributes to the server's metrics.
#[derive(Debug, Serialize, Deserialize)]
pub enum Report {
    Failure(ErrorCategory),
    Extraction { mime_type: String, seconds: f64 },
    CacheHit,
    CacheMiss,
}

/// Where a request's response goes: to the connection directly, or to the server process from
/// a worker process.
pub trait Respond {
    /// Sends the head; fails like a closed pipe once the client has gone away.
    fn head(&mut self, head: Head) -> io::Result<()>;
    /// Sends the next part of a streamed body; fails like a closed pipe once the client has gone.
    fn chunk(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn report(&mut self, report: Report);
}

pub fn run(task: &Task, files: &Files, cache: Option<&Cache>, respond: &mut dyn Respond) {
    match task {
        Task::Extract(request) => extract(request, files, cache, respond),
        Task::Detect => detect(files, respond),
    }
}

pub fn status_for(category: ErrorCategory) -> u16 {
    match category {
        ErrorCategory::Unsupported => 415,
        ErrorCategory::Corrupt | ErrorCategory::Encrypted => 422,
        ErrorCategory::LimitExceeded => 413,
        ErrorCategory::Timeout => 504,
        ErrorCategory::Unreadable | ErrorCategory::DiffersFromTika | ErrorCategory::Internal => 500,
    }
}

pub fn error_body(message: &str) -> String {
    format!("{}\n", serde_json::json!({ "error": message }))
}

pub fn error_head(status: u16, message: &str) -> Head {
    Head {
        status,
        content_type: ContentType::Json,
        body: Some(error_body(message).into_bytes()),
    }
}

fn streaming(content_type: ContentType) -> Head {
    Head {
        status: 200,
        content_type,
        body: None,
    }
}

fn extract(
    request: &ExtractRequest,
    files: &Files,
    cache: Option<&Cache>,
    respond: &mut dyn Respond,
) {
    let started = Instant::now();
    let extractor = settings::extractor(
        request.render,
        request.ocr,
        request.ocr_lang.as_deref(),
        request.passwords.clone(),
        request
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now())),
    );
    let settings = OutputSettings {
        render: request.render,
        ocr: request.ocr,
        ocr_lang: request.ocr_lang.as_deref(),
        embedded: request.embedded,
    };
    let cache = cache.filter(|_| request.passwords.is_empty());
    if request.embedded == Embedded::Separate {
        extract_tree(
            request,
            &extractor,
            files,
            cache.map(|cache| (cache, settings)),
            respond,
        );
        return;
    }
    let source = render::source(&files.path, &files.display, None);
    let lookup = cache.map(|cache| cache.lookup(&files.path, &files.display, &source, settings));
    let miss = match lookup {
        None => None,
        Some(Ok(Lookup::Hit(hit))) => {
            respond.report(Report::CacheHit);
            replay(request, hit, &source, respond);
            return;
        }
        Some(Ok(Lookup::Miss(miss))) => {
            respond.report(Report::CacheMiss);
            Some(miss)
        }
        Some(Err(error)) => {
            let _ = respond.head(error_head(
                500,
                &format!("Failed to read the cache: {error}"),
            ));
            return;
        }
    };
    let opened = render::open(&extractor, &files.path);
    let mime_type = opened
        .as_ref()
        .ok()
        .and_then(|(_, metadata)| schema::mime_type(metadata))
        .unwrap_or_else(|| "unknown".to_string());

    if let Err(error) = &opened {
        let mut document = Vec::new();
        let options = RenderOptions {
            format: Format::Json,
            metadata: false,
            raw_metadata: false,
            normalization: Steps::NONE,
            max_chars: None,
        };
        if render::render_opened(Err(error.clone()), &source, options, &mut document).is_ok() {
            respond.report(Report::Failure(error.category));
            let _ = respond.head(Head {
                status: status_for(error.category),
                content_type: ContentType::Json,
                body: Some(document),
            });
        }
        return;
    }

    if respond
        .head(streaming(ContentType::for_format(request.render.format)))
        .is_err()
    {
        return;
    }
    let mut writer = BufWriter::with_capacity(BODY_CHUNK_SIZE, BodyWriter { respond });
    let render =
        |writer: &mut dyn Write| render::render_opened(opened, &source, request.render, writer);
    let rendered = match miss {
        Some(miss) => miss.store(&mut writer, &source, render),
        None => render(&mut writer),
    }
    .and_then(|outcome| writer.flush().map(|()| outcome));
    drop(writer);
    if let Ok(outcome) = rendered {
        for error in &outcome.errors {
            respond.report(Report::Failure(error.category));
        }
        respond.report(Report::Extraction {
            mime_type,
            seconds: started.elapsed().as_secs_f64(),
        });
    }
}

/// Answers with the stored output of an earlier, complete extraction.
fn replay(request: &ExtractRequest, hit: Hit, source: &Source, respond: &mut dyn Respond) {
    if respond
        .head(streaming(ContentType::for_format(request.render.format)))
        .is_err()
    {
        return;
    }
    let mut writer = BufWriter::with_capacity(BODY_CHUNK_SIZE, BodyWriter { respond });
    if let Err(error) = hit.replay(&mut writer, source).and_then(|_| writer.flush()) {
        if error.kind() != io::ErrorKind::BrokenPipe {
            eprintln!("dijo: failed to replay a cached response: {error}");
        }
    }
}

/// Extracts a document and every document embedded in it as separate records. The status of each
/// is in its own record, so the response is 200 from the start.
fn extract_tree(
    request: &ExtractRequest,
    extractor: &dijo_core::Extractor,
    files: &Files,
    cache: Option<(&Cache, OutputSettings)>,
    respond: &mut dyn Respond,
) {
    let content_type = match request.render.format {
        Format::Json => ContentType::NdJson,
        format => ContentType::for_format(format),
    };
    if let Err(error) = std::fs::create_dir(&files.scratch) {
        let _ = respond.head(error_head(
            500,
            &format!("Failed to create {}: {error}", files.scratch.display()),
        ));
        return;
    }
    if respond.head(streaming(content_type)).is_err() {
        return;
    }
    let mut writer = BufWriter::with_capacity(BODY_CHUNK_SIZE, BodyWriter { respond });
    let rendered = tree::render_tree(
        extractor,
        &files.path,
        &files.display,
        request.render,
        request.limits,
        &files.scratch,
        cache,
        &mut writer,
    )
    .and_then(|outcome| writer.flush().map(|()| outcome));
    drop(writer);
    if let Ok(outcome) = rendered {
        for error in &outcome.errors {
            respond.report(Report::Failure(error.category));
        }
    }
}

fn detect(files: &Files, respond: &mut dyn Respond) {
    let extractor = dijo_core::Extractor::new();
    let detected = files
        .path
        .to_str()
        .ok_or_else(|| "The upload path is not valid UTF-8".to_string())
        .and_then(|path| extractor.detect_file(path).map_err(|e| e.to_string()));
    let head = match detected {
        Ok(media_type) => Head {
            status: 200,
            content_type: ContentType::Text,
            body: Some(format!("{media_type}\n").into_bytes()),
        },
        Err(message) => error_head(500, &message),
    };
    let _ = respond.head(head);
}

/// Writes a streamed body through a responder.
struct BodyWriter<'r> {
    respond: &'r mut dyn Respond,
}

impl Write for BodyWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.respond.chunk(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
