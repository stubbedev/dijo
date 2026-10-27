mod handlers;
mod metrics;
mod processes;

pub(crate) use processes::answer_channel;
mod tika;
mod upload;
mod workers;

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use clap::ValueEnum;
use dijo_core::Password;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{HeaderValue, CONTENT_DISPOSITION, CONTENT_TYPE, RETRY_AFTER};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Sleep;

use crate::cache::Cache;
use crate::cli::{processors_per_process, Embedded, Format, Isolation, Ocr, ServeArgs, WorkerArgs};
use crate::normalize::{Step, Steps};
use crate::render::RenderOptions;
use crate::spool::Spool;
use crate::tree::Limits;
use handlers::{error_body, ExtractRequest, Head, Task};
use metrics::Metrics;
use processes::ProcessOptions;
use upload::{Name, UploadError};
use workers::{Chunk, Job, Pool, Workers};

const PASSWORD_HEADER: &str = "x-dijo-password";
const FILENAME_HEADER: &str = "x-dijo-filename";
const RESPONSE_CHUNKS_IN_FLIGHT: usize = 8;
const IO_THREADS: usize = 2;
const RESPONSE_DEADLINE_GRACE: Duration = Duration::from_secs(5);

type ResponseBody = BoxBody<Bytes, io::Error>;

struct State {
    pool: Pool,
    metrics: Arc<Metrics>,
    spool: Spool,
    capacity: usize,
    max_input_bytes: Option<u64>,
    max_output_chars: Option<NonZeroU64>,
    limits: Limits,
    timeout: Option<Duration>,
    client_timeout: Duration,
    next_id: AtomicU64,
}

/// Counts a request as in flight until dropped, if there was room for it.
struct Admission(Arc<State>);

impl Admission {
    fn try_new(state: &Arc<State>) -> Option<Self> {
        let previous = state.metrics.in_flight.fetch_add(1, Ordering::SeqCst);
        if previous >= state.capacity {
            state.metrics.in_flight.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(Arc::clone(state)))
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.0.metrics.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn serve(args: &ServeArgs) -> bool {
    match start(args) {
        Ok(succeeded) => succeeded,
        Err(error) => {
            eprintln!("dijo: {error}");
            false
        }
    }
}

/// Runs a worker process of `serve --isolation process`.
pub fn work(args: &WorkerArgs) -> bool {
    let served = Cache::from_args(&args.cache, false).and_then(|cache| {
        processes::serve_jobs(cache.as_ref()).map_err(|e| format!("worker process: {e}"))
    });
    match served {
        Ok(()) => true,
        Err(error) => {
            eprintln!("dijo: {error}");
            false
        }
    }
}

fn start(args: &ServeArgs) -> Result<bool, String> {
    let metrics = Arc::new(Metrics::default());
    let workers = args.workers().get();
    let pool_workers = match args.isolation {
        Isolation::Thread => Workers::Threads(Cache::from_args(&args.cache, false)?.map(Arc::new)),
        Isolation::Process => Workers::Processes(ProcessOptions {
            arguments: [args.runtime.arguments(), args.cache.arguments()].concat(),
            processors: processors_per_process(args.workers(), 1),
            max_rss_bytes: args.worker_max_rss_bytes,
            grace: RESPONSE_DEADLINE_GRACE,
        }),
    };
    let pool = Pool::start(workers, args.queue, Arc::clone(&metrics), pool_workers)?;
    let spool = Spool::create(args.runtime.spool_dir.as_deref())
        .map_err(|e| format!("Failed to create the spool directory: {e}"))?;
    let state = Arc::new(State {
        pool,
        metrics,
        spool,
        capacity: workers + args.queue,
        max_input_bytes: args.max_input_bytes,
        max_output_chars: args.max_output_chars,
        limits: args.limits.limits(),
        timeout: args.timeout.map(Duration::from_secs),
        client_timeout: Duration::from_secs(args.client_timeout.get()),
        next_id: AtomicU64::new(0),
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(IO_THREADS)
        .thread_name("dijo io")
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to start the I/O runtime: {e}"))?;
    runtime.block_on(listen(
        args.listen,
        state,
        Duration::from_secs(args.shutdown_grace),
    ))
}

async fn listen(
    address: std::net::SocketAddr,
    state: Arc<State>,
    grace: Duration,
) -> Result<bool, String> {
    let listener = TcpListener::bind(address)
        .await
        .map_err(|e| format!("Failed to listen on {address}: {e}"))?;
    let local = listener
        .local_addr()
        .map_err(|e| format!("Failed to read the listening address: {e}"))?;
    eprintln!("dijo: listening on http://{local}");

    let graceful = GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown_signal());
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        eprintln!("dijo: failed to accept a connection: {error}");
                        continue;
                    }
                };
                let state = Arc::clone(&state);
                let client_timeout = state.client_timeout;
                let connection = http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(client_timeout)
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |request| handle(request, Arc::clone(&state))),
                    );
                let connection = graceful.watch(connection);
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        if !error.is_incomplete_message() {
                            eprintln!("dijo: connection error: {error}");
                        }
                    }
                });
            }
            () = &mut shutdown => break,
        }
    }
    drop(listener);
    eprintln!(
        "dijo: shutting down, waiting up to {}s for requests",
        grace.as_secs()
    );
    match tokio::time::timeout(grace, graceful.shutdown()).await {
        Ok(()) => Ok(true),
        Err(_) => {
            eprintln!("dijo: requests still running after the shutdown grace period");
            Ok(false)
        }
    }
}

async fn shutdown_signal() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("installing a SIGTERM handler");
        tokio::select! {
            _ = interrupt => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = interrupt.await;
    }
}

fn full(status: u16, content_type: &'static str, body: impl Into<Bytes>) -> Response<ResponseBody> {
    let mut response = Response::new(
        Full::new(body.into())
            .map_err(|never: Infallible| match never {})
            .boxed(),
    );
    *response.status_mut() = StatusCode::from_u16(status).expect("valid status code");
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn error(status: u16, message: &str) -> Response<ResponseBody> {
    full(status, "application/json", error_body(message))
}

async fn handle(
    request: Request<Incoming>,
    state: Arc<State>,
) -> Result<Response<ResponseBody>, Infallible> {
    let started = Instant::now();
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let (endpoint, response) = match (&method, path.as_str()) {
        (&Method::GET, "/healthz") => ("healthz", full(200, "text/plain; charset=utf-8", "ok\n")),
        (&Method::GET, "/readyz") => ("readyz", ready(&state)),
        (&Method::GET, "/metrics") => (
            "metrics",
            full(
                200,
                "text/plain; version=0.0.4; charset=utf-8",
                state.metrics.render(),
            ),
        ),
        (&Method::POST, "/extract") => ("extract", extract(request, &state).await),
        (&Method::POST, "/detect") => ("detect", detect(request, &state).await),
        _ => match tika::route(&method, &path) {
            Some(endpoint) => (endpoint.label(), tika(endpoint, request, &state).await),
            None => (
                "unknown",
                rejected(request, &state, error(404, "No such endpoint")).await,
            ),
        },
    };
    state.metrics.request(endpoint, response.status().as_u16());
    eprintln!(
        "dijo: {method} {path} {} {}ms",
        response.status().as_u16(),
        started.elapsed().as_millis()
    );
    Ok(response)
}

fn ready(state: &State) -> Response<ResponseBody> {
    if state.metrics.in_flight.load(Ordering::SeqCst) < state.capacity {
        full(200, "text/plain; charset=utf-8", "ready\n")
    } else {
        busy()
    }
}

fn busy() -> Response<ResponseBody> {
    let mut response = error(503, "All workers and queue slots are busy; retry shortly");
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

pub(crate) fn query_pairs(query: Option<&str>) -> Vec<(String, String)> {
    query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn hex_value(digit: u8) -> Option<u8> {
    char::from(digit).to_digit(16).map(|value| value as u8)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| {
                Some((
                    hex_value(*bytes.get(index + 1)?)?,
                    hex_value(*bytes.get(index + 2)?)?,
                ))
            })
            .flatten();
        match (bytes[index], escaped) {
            (_, Some((high, low))) => {
                decoded.push(high << 4 | low);
                index += 3;
            }
            (b'+', None) => {
                decoded.push(b' ');
                index += 1;
            }
            (byte, None) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn parse_flag(name: &str, value: &str) -> Result<bool, String> {
    match value {
        "" | "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(format!("{name} must be true or false, not {other:?}")),
    }
}

/// The extraction a request asks for before its options: the server's limits, plain text with
/// automatic OCR, and the passwords in the request's `password_header` headers.
fn default_extract_request(
    request: &Request<Incoming>,
    state: &State,
    password_header: &str,
) -> Result<ExtractRequest, String> {
    Ok(ExtractRequest {
        deadline: None,
        embedded: Embedded::Inline,
        limits: state.limits,
        render: RenderOptions {
            format: Format::Text,
            metadata: false,
            raw_metadata: false,
            normalization: Steps::ALL,
            max_chars: state.max_output_chars,
        },
        ocr: Ocr::Auto,
        ocr_lang: None,
        passwords: request
            .headers()
            .get_all(password_header)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(Password::new)
                    .map_err(|_| format!("{password_header} must be visible ASCII"))
            })
            .collect::<Result<_, _>>()?,
    })
}

fn parse_extract_request(
    request: &Request<Incoming>,
    state: &State,
) -> Result<ExtractRequest, String> {
    let mut extract = default_extract_request(request, state, PASSWORD_HEADER)?;
    let mut raw = false;
    let mut skipped = Vec::new();
    for (key, value) in query_pairs(request.uri().query()) {
        match key.as_str() {
            "format" => {
                extract.render.format = Format::from_str(&value, false)
                    .map_err(|_| format!("Unknown format {value:?}"))?
            }
            "metadata" => extract.render.metadata = parse_flag("metadata", &value)?,
            "raw_metadata" => extract.render.raw_metadata = parse_flag("raw_metadata", &value)?,
            "raw" => raw = parse_flag("raw", &value)?,
            "no_normalize" => {
                for step in value.split(',') {
                    skipped.push(
                        Step::from_str(step, false)
                            .map_err(|_| format!("Unknown normalisation step {step:?}"))?,
                    );
                }
            }
            "ocr" => {
                extract.ocr =
                    Ocr::from_str(&value, false).map_err(|_| format!("Unknown ocr {value:?}"))?
            }
            "ocr_lang" => extract.ocr_lang = Some(crate::settings::ocr_language(&value)?),
            "embedded" => {
                extract.embedded = Embedded::from_str(&value, false)
                    .map_err(|_| format!("Unknown embedded {value:?}"))?
            }
            other => return Err(format!("Unknown query parameter {other:?}")),
        }
    }
    if raw && !skipped.is_empty() {
        return Err("raw and no_normalize cannot be combined".to_string());
    }
    extract.render.normalization = Steps::selected(raw, &skipped);
    Ok(extract)
}

async fn extract(request: Request<Incoming>, state: &Arc<State>) -> Response<ResponseBody> {
    let task = match parse_extract_request(&request, state) {
        Ok(extract) => Task::Extract(extract),
        Err(message) => return rejected(request, state, error(400, &message)).await,
    };
    run(task, request, state, None).await
}

/// How a tika-server endpoint is answered from an upload: as `/extract` answers in the format of
/// a content handler, or by a worker's own output.
enum TikaAnswer {
    Content(tika::output::Handler),
    Output(tika::output::Output),
}

/// Answers a tika-server endpoint: a listing at once, anything else from the upload, with the
/// extraction Tika's headers ask for.
async fn tika(
    endpoint: tika::Endpoint,
    request: Request<Incoming>,
    state: &Arc<State>,
) -> Response<ResponseBody> {
    if let tika::Endpoint::Listing(listing) = &endpoint {
        let (status, content_type, body) = tika::listings::answer(listing, request.headers());
        return full(status, content_type, body);
    }
    match tika_task(endpoint, &request, state) {
        Ok((task, relabel)) => run(task, request, state, relabel).await,
        Err((status, message)) => rejected(request, state, error(status, &message)).await,
    }
}

/// The task a tika-server endpoint asks of the upload, with the media type its answer is
/// labelled with, or the status and message refusing the request.
fn tika_task(
    endpoint: tika::Endpoint,
    request: &Request<Incoming>,
    state: &State,
) -> Result<(Task, Option<&'static str>), (u16, String)> {
    if request.uri().query().is_some_and(|query| !query.is_empty()) {
        return Err((
            400,
            "tika-server's endpoints take no query parameters".to_string(),
        ));
    }
    let headers = request.headers();
    let not_acceptable = || (406, "No form of this endpoint is acceptable".to_string());
    let answer = match endpoint {
        tika::Endpoint::Listing(_) => unreachable!("listings are answered without an upload"),
        tika::Endpoint::Negotiated => {
            TikaAnswer::Content(tika::negotiated_handler(headers).ok_or_else(not_acceptable)?)
        }
        tika::Endpoint::Content(handler) => TikaAnswer::Content(handler),
        tika::Endpoint::Meta => TikaAnswer::Output(tika::output::Output::Meta(
            tika::meta_form(headers).ok_or_else(not_acceptable)?,
        )),
        tika::Endpoint::MetaField(field) => TikaAnswer::Output(tika::output::Output::MetaField {
            field,
            form: tika::field_form(headers).ok_or_else(not_acceptable)?,
        }),
        tika::Endpoint::Upload(output) => TikaAnswer::Output(output),
    };
    let mut extract = default_extract_request(request, state, tika::PASSWORD_HEADER)
        .and_then(|extract| {
            tika::check_headers(headers)?;
            Ok(ExtractRequest {
                ocr_lang: tika::ocr_language(headers)?,
                ..extract
            })
        })
        .map_err(|message| (400, message))?;
    Ok(match answer {
        TikaAnswer::Content(handler) => {
            extract.render.format = handler.format();
            (
                Task::Extract(extract),
                Some(tika::content_media_type(handler)),
            )
        }
        TikaAnswer::Output(output @ tika::output::Output::Unpack { .. }) => {
            extract.ocr = Ocr::Never;
            (Task::Tika(extract, output), None)
        }
        TikaAnswer::Output(output) => (Task::Tika(extract, output), None),
    })
}

/// Answers `request` with `response` once what is left of its body is read, which a rejection
/// answers before reading, so that closing the connection after it cannot lose the response.
async fn rejected(
    request: Request<Incoming>,
    state: &State,
    response: Response<ResponseBody>,
) -> Response<ResponseBody> {
    upload::discard(&mut request.into_body(), state.client_timeout).await;
    response
}

async fn detect(request: Request<Incoming>, state: &Arc<State>) -> Response<ResponseBody> {
    if request.uri().query().is_some_and(|query| !query.is_empty()) {
        return rejected(
            request,
            state,
            error(400, "/detect takes no query parameters"),
        )
        .await;
    }
    run(Task::Detect, request, state, None).await
}

/// Runs a task on the upload of `request`; a successful answer is labelled `relabel` rather
/// than with its format's own media type.
async fn run(
    mut task: Task,
    request: Request<Incoming>,
    state: &Arc<State>,
    relabel: Option<&'static str>,
) -> Response<ResponseBody> {
    let Some(admission) = Admission::try_new(state) else {
        return rejected(request, state, busy()).await;
    };
    let deadline = state
        .timeout
        .map(|timeout| tokio::time::Instant::now() + timeout);
    if let (Task::Extract(extract) | Task::Tika(extract, _), Some(deadline)) = (&mut task, deadline)
    {
        extract.deadline = Some(deadline.into_std());
    }
    let header = |name| {
        request
            .headers()
            .get(name)
            .and_then(|value: &HeaderValue| value.to_str().ok())
    };
    let file_name = header(FILENAME_HEADER)
        .or_else(|| header(CONTENT_DISPOSITION.as_str()).and_then(upload::disposition_file_name));
    let name = Name::from_hints(file_name, header(CONTENT_TYPE.as_str()));
    let id = state.next_id.fetch_add(1, Ordering::Relaxed);
    let received = upload::receive(
        request.into_body(),
        state.spool.dir(),
        id,
        name,
        state.max_input_bytes,
        state.client_timeout,
        &state.metrics,
    );
    let upload = match deadline {
        Some(deadline) => match tokio::time::timeout_at(deadline, received).await {
            Ok(received) => received,
            Err(_) => return error(408, "The upload did not finish in time"),
        },
        None => received.await,
    };
    let upload = match upload {
        Ok(upload) => upload,
        Err(UploadError::TooLarge) => return error(413, "The upload exceeds --max-input-bytes"),
        Err(UploadError::Stalled) => {
            return error(408, "The upload stalled for longer than --client-timeout")
        }
        Err(UploadError::Body(error)) => {
            return self::error(400, &format!("Failed to read the upload: {error}"))
        }
        Err(UploadError::Io(error)) => {
            return self::error(500, &format!("Failed to spool the upload: {error}"))
        }
    };

    let (head_sender, head) = oneshot::channel();
    let (body_sender, body) = mpsc::channel(RESPONSE_CHUNKS_IN_FLIGHT);
    let job = Job {
        task,
        upload,
        head: head_sender,
        body: body_sender,
    };
    if state.pool.submit(job).is_err() {
        return busy();
    }
    let head: Result<Head, _> = match deadline {
        Some(deadline) => {
            match tokio::time::timeout_at(deadline + RESPONSE_DEADLINE_GRACE, head).await {
                Ok(head) => head,
                Err(_) => return error(504, "Extraction did not start in time"),
            }
        }
        None => head.await,
    };
    let Ok(head) = head else {
        return error(500, "The worker stopped before answering");
    };
    let content_type = match relabel {
        Some(relabel) if head.status == 200 => relabel,
        _ => head.content_type.header(),
    };
    match head.body {
        Some(body) => full(head.status, content_type, body),
        None => {
            let mut response = Response::new(
                StreamedBody {
                    receiver: body,
                    deadline: deadline.map(|deadline| {
                        Box::pin(tokio::time::sleep_until(deadline + RESPONSE_DEADLINE_GRACE))
                    }),
                    _admission: admission,
                }
                .boxed(),
            );
            *response.status_mut() = StatusCode::from_u16(head.status).expect("valid status code");
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
            response
        }
    }
}

/// The text a worker produces. The worker itself stops at the request's deadline and closes its
/// output with a timeout error; if it has not finished a little after that, the body ends with an
/// error, which closes the connection and makes the worker stop at its next write.
struct StreamedBody {
    receiver: mpsc::Receiver<Chunk>,
    deadline: Option<Pin<Box<Sleep>>>,
    _admission: Admission,
}

impl Body for StreamedBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if let Some(deadline) = &mut self.deadline {
            if deadline.as_mut().poll(context).is_ready() {
                return Poll::Ready(Some(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the request exceeded --timeout",
                ))));
            }
        }
        self.receiver
            .poll_recv(context)
            .map(|chunk| chunk.map(|chunk| chunk.map(Frame::data)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_percent_decoded() {
        assert_eq!(
            query_pairs(Some(
                "format=json&ocr_lang=eng%2Bdeu&x=%E6%BC%A2+y&bad=%zz%"
            )),
            [
                ("format".to_string(), "json".to_string()),
                ("ocr_lang".to_string(), "eng+deu".to_string()),
                ("x".to_string(), "漢 y".to_string()),
                ("bad".to_string(), "%zz%".to_string()),
            ]
        );
    }

    #[test]
    fn arbitrary_queries_decode_without_panicking() {
        const PIECES: &[&str] = &[
            "%", "%2", "%2B", "%zz", "%E6", "%BC", "%A2", "%FF", "+", "&", "=", "==", "a",
            "format", "é", "\u{0}", "%00",
        ];
        let mut state: u64 = 0x5851_f42d_4c95_7f2d;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..5000 {
            let length = (next() % 30) as usize;
            let query: String = (0..length)
                .map(|_| PIECES[(next() % PIECES.len() as u64) as usize])
                .collect();
            for (key, value) in query_pairs(Some(&query)) {
                assert!(key.len() + value.len() <= query.len() * 3, "{query:?}");
            }
        }
    }
}
