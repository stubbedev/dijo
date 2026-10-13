use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::num::NonZeroU64;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use extractous_core::Password;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc as async_mpsc, oneshot};

use crate::cache::Cache;
use crate::cli::{Embedded, Ocr};
use crate::render::RenderOptions;
use crate::schema::ErrorCategory;
use crate::server::handlers::{
    self, error_head, status_for, ExtractRequest, Files, Head, Report, Respond, Task,
};
use crate::server::metrics::Metrics;
use crate::server::workers::{client_gone, Chunk, Job};

const JOB: u8 = b'J';
const READY: u8 = b'Y';
const HEAD: u8 = b'H';
const CHUNK: u8 = b'C';
const REPORT: u8 = b'R';
const END: u8 = b'E';
const WATCH_INTERVAL: Duration = Duration::from_millis(100);

/// A job as it crosses to a worker process: the deadline as the time left, the passwords as text.
#[derive(Serialize, Deserialize)]
struct WireJob {
    task: WireTask,
    files: Files,
}

#[derive(Serialize, Deserialize)]
enum WireTask {
    Extract {
        remaining: Option<Duration>,
        embedded: Embedded,
        render: RenderOptions,
        ocr: Ocr,
        ocr_lang: Option<String>,
        passwords: Vec<String>,
    },
    Detect,
}

impl WireTask {
    fn of(task: &Task) -> Self {
        match task {
            Task::Extract(request) => WireTask::Extract {
                remaining: request
                    .deadline
                    .map(|deadline| deadline.saturating_duration_since(Instant::now())),
                embedded: request.embedded,
                render: request.render,
                ocr: request.ocr,
                ocr_lang: request.ocr_lang.clone(),
                passwords: request
                    .passwords
                    .iter()
                    .map(|password| password.expose().to_string())
                    .collect(),
            },
            Task::Detect => WireTask::Detect,
        }
    }

    fn into_task(self) -> Task {
        match self {
            WireTask::Extract {
                remaining,
                embedded,
                render,
                ocr,
                ocr_lang,
                passwords,
            } => Task::Extract(ExtractRequest {
                deadline: remaining.map(|remaining| Instant::now() + remaining),
                embedded,
                render,
                ocr,
                ocr_lang,
                passwords: passwords.into_iter().map(Password::new).collect(),
            }),
            WireTask::Detect => Task::Detect,
        }
    }
}

fn write_frame(sink: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a frame of {} bytes is too large", payload.len()),
        )
    })?;
    sink.write_all(&[tag])?;
    sink.write_all(&length.to_be_bytes())?;
    sink.write_all(payload)
}

/// The next frame, or None when the stream ended cleanly between frames.
fn read_frame(source: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut tag = [0; 1];
    if source.read(&mut tag)? == 0 {
        return Ok(None);
    }
    let mut length = [0; 4];
    source.read_exact(&mut length)?;
    let mut payload = vec![0; u32::from_be_bytes(length) as usize];
    source.read_exact(&mut payload)?;
    Ok(Some((tag[0], payload)))
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

/// The pipe a worker process answers on: its standard output, which from then on goes to
/// standard error, so nothing a parser prints can corrupt the answers.
#[cfg(unix)]
pub(crate) fn answer_channel() -> io::Result<File> {
    use std::os::fd::AsFd;
    let channel = io::stdout().as_fd().try_clone_to_owned()?;
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(File::from(channel))
}

#[cfg(windows)]
pub(crate) fn answer_channel() -> io::Result<File> {
    use std::os::windows::io::AsHandle;
    Ok(File::from(io::stdout().as_handle().try_clone_to_owned()?))
}

/// The worker process: takes jobs from standard input, one at a time, and answers each in turn,
/// until its input closes.
pub fn serve_jobs(cache: Option<&Cache>) -> io::Result<()> {
    let mut output = BufWriter::new(answer_channel()?);
    extractous_core::keep_current_thread_attached().map_err(io::Error::other)?;
    let mut input = BufReader::new(io::stdin().lock());
    write_frame(&mut output, READY, &[])?;
    output.flush()?;
    while let Some((tag, payload)) = read_frame(&mut input)? {
        if tag != JOB {
            return Err(invalid(format!("expected a job, got frame {tag}")));
        }
        let job: WireJob = serde_json::from_slice(&payload).map_err(invalid)?;
        let mut respond = ProcessResponder {
            output: &mut output,
            failed: None,
        };
        handlers::run(&job.task.into_task(), &job.files, cache, &mut respond);
        if let Some(error) = respond.failed {
            return Err(error);
        }
        write_frame(&mut output, END, &[])?;
        output.flush()?;
    }
    Ok(())
}

/// Answers the server process, from a worker process. Once the server stops listening the
/// worker cannot do anything useful, so the first failed write ends it.
struct ProcessResponder<'a, W: Write> {
    output: &'a mut W,
    failed: Option<io::Error>,
}

impl<W: Write> ProcessResponder<'_, W> {
    fn frame(&mut self, tag: u8, payload: &[u8]) -> io::Result<()> {
        if self.failed.is_some() {
            return Err(client_gone());
        }
        write_frame(self.output, tag, payload)
            .and_then(|()| self.output.flush())
            .map_err(|error| {
                self.failed = Some(error);
                client_gone()
            })
    }
}

impl<W: Write> Respond for ProcessResponder<'_, W> {
    fn head(&mut self, head: Head) -> io::Result<()> {
        let payload = serde_json::to_vec(&head).expect("a head serialises");
        self.frame(HEAD, &payload)
    }

    fn chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.frame(CHUNK, bytes)
    }

    fn report(&mut self, report: Report) {
        let payload = serde_json::to_vec(&report).expect("a report serialises");
        let _ = self.frame(REPORT, &payload);
    }
}

/// How worker processes are started and what they are held to.
#[derive(Clone)]
pub struct ProcessOptions {
    /// The options of the `worker` command.
    pub arguments: Vec<OsString>,
    pub max_rss_bytes: Option<NonZeroU64>,
    /// How long past its deadline a job may keep its worker process before it is stopped.
    pub grace: Duration,
}

/// Why the server stopped a worker process.
#[derive(Debug, Clone, Copy)]
enum Stopped {
    Deadline,
    Memory,
}

pub struct WorkerProcess {
    child: Arc<Mutex<Child>>,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl WorkerProcess {
    /// Starts a worker process and waits until it is ready for jobs.
    pub fn start(options: &ProcessOptions) -> io::Result<Self> {
        let executable = std::env::current_exe()?;
        let mut child = Command::new(&executable)
            .arg("worker")
            .args(&options.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", executable.display())))?;
        let input = BufWriter::new(child.stdin.take().expect("standard input is piped"));
        let mut output = BufReader::new(child.stdout.take().expect("standard output is piped"));
        let ready = read_frame(&mut output);
        if let Ok(Some((READY, _))) = ready {
            return Ok(WorkerProcess {
                child: Arc::new(Mutex::new(child)),
                input,
                output,
            });
        }
        let _ = child.kill();
        let status = child.wait()?;
        let reason = match ready {
            Err(error) => error.to_string(),
            Ok(_) => "it sent no ready signal".to_string(),
        };
        Err(io::Error::other(format!(
            "the worker process did not start ({status}): {reason}"
        )))
    }

    fn is_running(&self) -> bool {
        matches!(
            self.child.lock().expect("worker process lock").try_wait(),
            Ok(None)
        )
    }

    /// Kills the process, unless it already ended, and tells how it ended.
    fn stop(self) -> String {
        let mut child = self.child.lock().expect("worker process lock");
        let _ = child.kill();
        match child.wait() {
            Ok(status) => status.to_string(),
            Err(error) => format!("unknown status: {error}"),
        }
    }
}

/// Serves jobs from the queue through one worker process, starting a new one whenever the last
/// one died or was stopped.
pub fn supervise(
    receiver: &Mutex<mpsc::Receiver<Job>>,
    metrics: &Metrics,
    options: &ProcessOptions,
    first: WorkerProcess,
) {
    let mut process = Some(first);
    loop {
        let Ok(job) = receiver.lock().expect("job queue lock").recv() else {
            if let Some(process) = process {
                process.stop();
            }
            return;
        };
        let current = match next_process(process.take(), metrics, options) {
            Ok(current) => current,
            Err(error) => {
                metrics.record(Report::Failure(ErrorCategory::Internal));
                let _ = job.head.send(error_head(
                    status_for(ErrorCategory::Internal),
                    &format!("Failed to start a worker process: {error}"),
                ));
                continue;
            }
        };
        process = relay(current, job, metrics, options);
        if process.is_none() {
            metrics.workers.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// The worker process for the next job: the current one while it runs, or else a new one.
fn next_process(
    current: Option<WorkerProcess>,
    metrics: &Metrics,
    options: &ProcessOptions,
) -> io::Result<WorkerProcess> {
    if let Some(current) = current {
        if current.is_running() {
            return Ok(current);
        }
        metrics.workers.fetch_sub(1, Ordering::Relaxed);
        eprintln!(
            "extractous: a worker process ended between jobs ({})",
            current.stop()
        );
    }
    let started = WorkerProcess::start(options)?;
    metrics.workers.fetch_add(1, Ordering::Relaxed);
    Ok(started)
}

/// Hands one job to a worker process and relays its answer. Returns the process when it can take
/// the next job, or None when it died or was stopped.
fn relay(
    mut process: WorkerProcess,
    job: Job,
    metrics: &Metrics,
    options: &ProcessOptions,
) -> Option<WorkerProcess> {
    let deadline = match &job.task {
        Task::Extract(request) => request.deadline.map(|deadline| deadline + options.grace),
        Task::Detect => None,
    };
    let payload = serde_json::to_vec(&WireJob {
        task: WireTask::of(&job.task),
        files: job.upload.files(),
    })
    .expect("a job serialises");
    let Job { head, body, .. } = job;
    let mut head = Some(head);
    let sent = write_frame(&mut process.input, JOB, &payload).and_then(|()| process.input.flush());
    if let Err(error) = sent {
        let message = format!("The worker process is gone ({}): {error}", process.stop());
        fail(&mut head, &body, metrics, ErrorCategory::Internal, &message);
        return None;
    }

    let stopped = Arc::new(Mutex::new(None));
    let (finished, watching) = mpsc::channel::<()>();
    let watchdog = {
        let child = Arc::clone(&process.child);
        let stopped = Arc::clone(&stopped);
        let max_rss_bytes = options.max_rss_bytes;
        thread::spawn(move || watch(&child, &stopped, deadline, max_rss_bytes, &watching))
    };
    let mut within_limits = || {
        let mut child = process.child.lock().expect("worker process lock");
        match exceeded(&child, deadline, options.max_rss_bytes) {
            None => Ok(()),
            Some(reason) => {
                *stopped.lock().expect("watchdog lock") = Some(reason);
                let _ = child.kill();
                Err(io::Error::other("it overran its limits before answering"))
            }
        }
    };
    let answered = relay_answer(
        &mut process.output,
        &mut head,
        &body,
        metrics,
        &mut within_limits,
    );
    drop(finished);
    watchdog.join().expect("the watchdog does not panic");

    let error = match answered {
        Ok(Answered::Completely) => return Some(process),
        Ok(Answered::ToNobody) => {
            process.stop();
            return None;
        }
        Err(error) => error,
    };
    let stopped = *stopped.lock().expect("watchdog lock");
    let status = process.stop();
    let (category, message) = match stopped {
        Some(Stopped::Deadline) => (
            ErrorCategory::Timeout,
            "The extraction overran --timeout; its worker process was stopped".to_string(),
        ),
        Some(Stopped::Memory) => (
            ErrorCategory::LimitExceeded,
            "The extraction exceeded --worker-max-rss-bytes; its worker process was stopped"
                .to_string(),
        ),
        None => (
            ErrorCategory::Internal,
            format!("The worker process died ({status}): {error}"),
        ),
    };
    fail(&mut head, &body, metrics, category, &message);
    None
}

enum Answered {
    Completely,
    /// The client went away mid-answer.
    ToNobody,
}

/// Relays a worker's answer frame by frame. `within_limits` is asked before the head goes out,
/// since a status once sent cannot become the 413 or 504 of a worker over its limits.
fn relay_answer(
    output: &mut impl Read,
    head: &mut Option<oneshot::Sender<Head>>,
    body: &async_mpsc::Sender<Chunk>,
    metrics: &Metrics,
    within_limits: &mut dyn FnMut() -> io::Result<()>,
) -> io::Result<Answered> {
    loop {
        let Some((tag, payload)) = read_frame(output)? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "it ended in the middle of an answer",
            ));
        };
        match tag {
            HEAD => {
                let parsed: Head = serde_json::from_slice(&payload).map_err(invalid)?;
                within_limits()?;
                let sender = head
                    .take()
                    .ok_or_else(|| invalid("it sent a second head"))?;
                if sender.send(parsed).is_err() {
                    return Ok(Answered::ToNobody);
                }
            }
            CHUNK => {
                let length = payload.len() as u64;
                if body.blocking_send(Ok(Bytes::from(payload))).is_err() {
                    return Ok(Answered::ToNobody);
                }
                metrics.bytes_out(length);
            }
            REPORT => metrics.record(serde_json::from_slice(&payload).map_err(invalid)?),
            END => return Ok(Answered::Completely),
            other => return Err(invalid(format!("it sent an unknown frame {other}"))),
        }
    }
}

/// Fails a job: with an error head when none was sent yet, or else by cutting the body short.
fn fail(
    head: &mut Option<oneshot::Sender<Head>>,
    body: &async_mpsc::Sender<Chunk>,
    metrics: &Metrics,
    category: ErrorCategory,
    message: &str,
) {
    metrics.record(Report::Failure(category));
    match head.take() {
        Some(head) => {
            let _ = head.send(error_head(status_for(category), message));
        }
        None => {
            let _ = body.blocking_send(Err(io::Error::other(message.to_string())));
        }
    }
}

/// Stops the worker process when its job overruns the deadline or the memory limit, until the
/// job is finished.
fn watch(
    child: &Mutex<Child>,
    stopped: &Mutex<Option<Stopped>>,
    deadline: Option<Instant>,
    max_rss_bytes: Option<NonZeroU64>,
    finished: &mpsc::Receiver<()>,
) {
    while let Err(RecvTimeoutError::Timeout) = finished.recv_timeout(WATCH_INTERVAL) {
        let mut child = child.lock().expect("worker process lock");
        if let Some(reason) = exceeded(&child, deadline, max_rss_bytes) {
            *stopped.lock().expect("watchdog lock") = Some(reason);
            let _ = child.kill();
            return;
        }
    }
}

/// The limit a worker process's job has overrun, if any.
fn exceeded(
    child: &Child,
    deadline: Option<Instant>,
    max_rss_bytes: Option<NonZeroU64>,
) -> Option<Stopped> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Some(Stopped::Deadline)
    } else if max_rss_bytes
        .is_some_and(|max| resident_bytes(child.id()).is_some_and(|rss| rss > max.get()))
    {
        Some(Stopped::Memory)
    } else {
        None
    }
}

/// The resident memory of a process; `--worker-max-rss-bytes` is limited to Linux.
#[cfg(target_os = "linux")]
fn resident_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kilobytes: u64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    Some(kilobytes * 1024)
}

#[cfg(not(target_os = "linux"))]
fn resident_bytes(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_end_cleanly_between_frames() {
        let mut stream = Vec::new();
        write_frame(&mut stream, CHUNK, b"some text").unwrap();
        write_frame(&mut stream, END, &[]).unwrap();
        let mut source = stream.as_slice();
        assert_eq!(
            read_frame(&mut source).unwrap(),
            Some((CHUNK, b"some text".to_vec()))
        );
        assert_eq!(read_frame(&mut source).unwrap(), Some((END, Vec::new())));
        assert_eq!(read_frame(&mut source).unwrap(), None);
    }

    #[test]
    fn a_frame_cut_short_is_an_error() {
        let mut stream = Vec::new();
        write_frame(&mut stream, CHUNK, b"some text").unwrap();
        stream.truncate(stream.len() - 2);
        let error = read_frame(&mut stream.as_slice()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_resident_memory_of_this_process_is_known() {
        assert!(resident_bytes(std::process::id()).is_some_and(|bytes| bytes > 0));
    }
}
