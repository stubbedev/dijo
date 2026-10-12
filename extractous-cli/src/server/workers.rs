use std::io::{self, BufWriter, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

use bytes::Bytes;
use extractous_core::{keep_current_thread_attached, Password};
use tokio::sync::{mpsc as async_mpsc, oneshot};

use crate::cli::{Embedded, Format, Ocr};
use crate::normalize::Steps;
use crate::render::{self, RenderOptions};
use crate::schema::{self, ErrorCategory};
use crate::server::metrics::Metrics;
use crate::server::upload::Upload;
use crate::settings;
use crate::tree::{self, Limits};

const BODY_CHUNK_SIZE: usize = 64 * 1024;

pub struct ExtractRequest {
    pub deadline: Option<Instant>,
    pub embedded: Embedded,
    pub render: RenderOptions,
    pub ocr: Ocr,
    pub ocr_lang: Option<String>,
    pub passwords: Vec<Password>,
}

pub enum Task {
    Extract(ExtractRequest),
    Detect,
}

/// The status line and headers of a response, sent before its body. A body that is known up
/// front (an error, a detected type) comes with the head; otherwise it streams afterwards.
pub struct Head {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Option<Bytes>,
}

pub struct Job {
    pub task: Task,
    pub upload: Upload,
    pub head: oneshot::Sender<Head>,
    pub body: async_mpsc::Sender<Bytes>,
}

pub struct QueueFull;

/// A fixed set of extraction threads, attached to the isolate for good, fed through a bounded
/// queue.
pub struct Pool {
    sender: mpsc::SyncSender<Job>,
}

impl Pool {
    /// Starts `workers` threads. Admission already limits requests in flight to the workers plus
    /// `queue` slots, so the channel holds that many and never refuses an admitted job, even
    /// while a worker is between jobs.
    pub fn start(workers: usize, queue: usize, metrics: Arc<Metrics>) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(workers + queue);
        let receiver = Arc::new(Mutex::new(receiver));
        let (ready_sender, ready) = mpsc::channel::<Result<(), String>>();
        for index in 0..workers {
            let receiver = Arc::clone(&receiver);
            let metrics = Arc::clone(&metrics);
            let ready_sender = ready_sender.clone();
            thread::Builder::new()
                .name(format!("extractous worker {index}"))
                .spawn(move || {
                    if let Err(error) = keep_current_thread_attached() {
                        let _ = ready_sender.send(Err(error.to_string()));
                        return;
                    }
                    let _ = ready_sender.send(Ok(()));
                    drop(ready_sender);
                    metrics
                        .workers
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    work(&receiver, &metrics);
                })
                .map_err(|e| format!("Failed to start worker {index}: {e}"))?;
        }
        drop(ready_sender);
        for _ in 0..workers {
            ready
                .recv()
                .map_err(|_| "A worker exited during startup".to_string())??;
        }
        Ok(Self { sender })
    }

    /// Queues a job; one that finds the queue full is dropped, removing its upload.
    pub fn submit(&self, job: Job) -> Result<(), QueueFull> {
        self.sender.try_send(job).map_err(|_| QueueFull)
    }
}

fn work(receiver: &Mutex<mpsc::Receiver<Job>>, metrics: &Metrics) {
    loop {
        let job = receiver.lock().expect("job queue lock").recv();
        let Ok(Job {
            task,
            upload,
            head,
            body,
        }) = job
        else {
            return;
        };
        match task {
            Task::Extract(request) => extract(&request, &upload, head, body, metrics),
            Task::Detect => detect(&upload, head),
        }
    }
}

pub fn status_for(category: ErrorCategory) -> u16 {
    match category {
        ErrorCategory::Unsupported => 415,
        ErrorCategory::Corrupt | ErrorCategory::Encrypted => 422,
        ErrorCategory::LimitExceeded => 413,
        ErrorCategory::Timeout => 504,
        ErrorCategory::Unreadable | ErrorCategory::Internal => 500,
    }
}

pub fn content_type(format: Format) -> &'static str {
    match format {
        Format::Text => "text/plain; charset=utf-8",
        Format::Xhtml => "application/xhtml+xml; charset=utf-8",
        Format::Json => "application/json",
        Format::Pages | Format::Elements => "application/x-ndjson",
    }
}

fn extract(
    request: &ExtractRequest,
    upload: &Upload,
    head: oneshot::Sender<Head>,
    body: async_mpsc::Sender<Bytes>,
    metrics: &Metrics,
) {
    let started = Instant::now();
    let extractor = settings::extractor(
        request.render.format,
        request.ocr,
        request.ocr_lang.as_deref(),
        request.passwords.clone(),
        request
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now())),
    );
    let path = upload.path();
    let display = upload.display();
    if request.embedded == Embedded::Separate {
        extract_tree(request, &extractor, upload, head, body, metrics);
        return;
    }
    let opened = render::open(&extractor, path);
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
        };
        let source = render::source(path, display, None);
        if render::render_opened(Err(error.clone()), &source, options, &mut document).is_ok() {
            metrics.failure(error.category.name());
            let _ = head.send(Head {
                status: status_for(error.category),
                content_type: content_type(Format::Json),
                body: Some(Bytes::from(document)),
            });
        }
        return;
    }

    let started_response = head.send(Head {
        status: 200,
        content_type: content_type(request.render.format),
        body: None,
    });
    if started_response.is_err() {
        return;
    }
    let mut writer = BufWriter::with_capacity(
        BODY_CHUNK_SIZE,
        ChannelWriter {
            sender: body,
            metrics,
        },
    );
    let source = render::source(path, display, None);
    let rendered = render::render_opened(opened, &source, request.render, &mut writer)
        .and_then(|outcome| writer.flush().map(|()| outcome));
    if let Ok(outcome) = rendered {
        for error in &outcome.errors {
            metrics.failure(error.category.name());
        }
        metrics.extraction(&mime_type, started.elapsed());
    }
}

/// Extracts a document and every document embedded in it as separate records. The status of each
/// is in its own record, so the response is 200 from the start.
fn extract_tree(
    request: &ExtractRequest,
    extractor: &extractous_core::Extractor,
    upload: &Upload,
    head: oneshot::Sender<Head>,
    body: async_mpsc::Sender<Bytes>,
    metrics: &Metrics,
) {
    let content_type = match request.render.format {
        Format::Json => content_type(Format::Pages),
        format => content_type(format),
    };
    let scratch = upload.scratch();
    if let Err(error) = std::fs::create_dir(&scratch) {
        let _ = head.send(Head {
            status: 500,
            content_type: "application/json",
            body: Some(Bytes::from(error_body(&format!(
                "Failed to create {}: {error}",
                scratch.display()
            )))),
        });
        return;
    }
    let started = head.send(Head {
        status: 200,
        content_type,
        body: None,
    });
    if started.is_err() {
        return;
    }
    let mut writer = BufWriter::with_capacity(
        BODY_CHUNK_SIZE,
        ChannelWriter {
            sender: body,
            metrics,
        },
    );
    let rendered = tree::render_tree(
        extractor,
        upload.path(),
        upload.display(),
        request.render,
        Limits::DEFAULT,
        &scratch,
        &mut writer,
    )
    .and_then(|outcome| writer.flush().map(|()| outcome));
    if let Ok(outcome) = rendered {
        for error in &outcome.errors {
            metrics.failure(error.category.name());
        }
    }
}

fn detect(upload: &Upload, head: oneshot::Sender<Head>) {
    let extractor = extractous_core::Extractor::new();
    let detected = upload
        .path()
        .to_str()
        .ok_or_else(|| "The upload path is not valid UTF-8".to_string())
        .and_then(|path| extractor.detect_file(path).map_err(|e| e.to_string()));
    let response = match detected {
        Ok(media_type) => Head {
            status: 200,
            content_type: "text/plain; charset=utf-8",
            body: Some(Bytes::from(format!("{media_type}\n"))),
        },
        Err(message) => Head {
            status: 500,
            content_type: "application/json",
            body: Some(Bytes::from(error_body(&message))),
        },
    };
    let _ = head.send(response);
}

pub fn error_body(message: &str) -> String {
    format!("{}\n", serde_json::json!({ "error": message }))
}

/// Hands the response body to the connection task, failing like a closed pipe once the client
/// has gone away so the extraction stops.
struct ChannelWriter<'m> {
    sender: async_mpsc::Sender<Bytes>,
    metrics: &'m Metrics,
}

impl Write for ChannelWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.sender
            .blocking_send(Bytes::copy_from_slice(buf))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the client went away"))?;
        self.metrics.bytes_out(buf.len() as u64);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
