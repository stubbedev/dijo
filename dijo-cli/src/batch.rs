//! Extraction across worker processes. A batch with more parallel jobs than
//! [`JOBS_PER_PROCESS`] runs in worker processes: copies of this executable, started with the same
//! arguments, that take jobs as JSON lines on standard input and answer each with one on
//! standard output. Their output goes into the spool directory of the process that started them,
//! which emits it in input order as before.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, BufWriter, Lines, Stdin, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread;

use dijo_core::Metadata;
use serde::{Deserialize, Serialize};

use crate::cli::{processors_per_process, ExtractArgs, PROCESSORS_VARIABLE};
use crate::inputs::{Input, InputError, Source};
use crate::schema::Status;
use crate::server::answer_channel;
use crate::{
    embedded_scratch, partial_output, prepare, report, run, sink, Context, Finished, Resolved,
    Sink, SinkFailure, STDOUT_CLOSED,
};

/// The extractions each worker process runs at once. Two take half the processes, and so half
/// the memory and start-up, of one each.
pub const JOBS_PER_PROCESS: usize = 2;

/// Set in a worker process's environment to the spool directory of the process that started it.
pub const WORKER_VARIABLE: &str = "DIJO_BATCH_WORKER";

/// What a worker process is told first: the passwords, which it must not read from the
/// password file itself, since that may be standard input.
#[derive(Serialize, Deserialize)]
struct Setup {
    passwords: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Job {
    index: usize,
    display: String,
    path: PathBuf,
    output_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Done {
    index: usize,
    display: String,
    output: Output,
    metadata: Option<Metadata>,
    status: Status,
    errors: Vec<String>,
}

/// Where a worker process put an input's output.
#[derive(Serialize, Deserialize)]
enum Output {
    Spooled(PathBuf),
    Written,
    Failed(String),
}

impl Done {
    fn new(index: usize, finished: Finished) -> Self {
        Done {
            index,
            display: finished.display,
            output: match finished.output {
                Ok(Some(path)) => Output::Spooled(path),
                Ok(None) => Output::Written,
                Err(SinkFailure::Failed(error)) => Output::Failed(error),
                Err(SinkFailure::StdoutClosed) => {
                    Output::Failed("standard output was closed".to_string())
                }
            },
            metadata: finished.metadata,
            status: finished.status,
            errors: finished.errors,
        }
    }
}

impl From<Done> for Finished {
    fn from(done: Done) -> Self {
        Finished {
            display: done.display,
            output: match done.output {
                Output::Spooled(path) => Ok(Some(path)),
                Output::Written => Ok(None),
                Output::Failed(error) => Err(SinkFailure::Failed(error)),
            },
            metadata: done.metadata,
            status: done.status,
            errors: done.errors,
        }
    }
}

fn failed(display: &str, message: String) -> Finished {
    Finished {
        display: display.to_string(),
        output: Ok(None),
        metadata: None,
        status: Status::Failed,
        errors: vec![message],
    }
}

/// Extracts every input through worker processes, `jobs` at once, and sends each result on as it
/// is done.
pub fn extract(
    args: &ExtractArgs,
    inputs: &[Result<Resolved, InputError>],
    spool_dir: &Path,
    passwords: &[String],
    jobs: NonZeroUsize,
    sender: &mpsc::Sender<(usize, Finished)>,
) {
    let processes = jobs.get().div_ceil(JOBS_PER_PROCESS);
    let processors = processors_per_process(jobs, JOBS_PER_PROCESS);
    let next = AtomicUsize::new(0);
    let setup = serde_json::to_string(&Setup {
        passwords: passwords.to_vec(),
    })
    .expect("the setup serialises");
    thread::scope(|scope| {
        for _ in 0..processes {
            let sender = sender.clone();
            let (next, setup) = (&next, setup.as_str());
            scope.spawn(move || {
                supervise(args, inputs, spool_dir, setup, processors, next, &sender);
            });
        }
    });
}

/// Runs inputs through one worker process at a time, two at once. When the process ends with
/// jobs in flight, each of them runs again on its own in a new process, so that only the input
/// that ends a process alone fails.
fn supervise(
    args: &ExtractArgs,
    inputs: &[Result<Resolved, InputError>],
    spool_dir: &Path,
    setup: &str,
    processors: NonZeroUsize,
    next: &AtomicUsize,
    sender: &mpsc::Sender<(usize, Finished)>,
) {
    let mut worker: Option<Worker> = None;
    let mut in_flight: Vec<usize> = Vec::with_capacity(JOBS_PER_PROCESS);
    let mut suspects: VecDeque<usize> = VecDeque::new();
    loop {
        let limit = if suspects.is_empty() {
            JOBS_PER_PROCESS
        } else {
            1
        };
        while in_flight.len() < limit {
            let index = match suspects.pop_front() {
                Some(index) => index,
                None if STDOUT_CLOSED.load(Ordering::Relaxed) => break,
                None => next.fetch_add(1, Ordering::Relaxed),
            };
            let Some(input) = inputs.get(index) else {
                break;
            };
            let resolved = match input {
                Ok(resolved) => resolved,
                Err(error) => {
                    let finished = failed(&error.display, error.message.clone());
                    if sender.send((index, finished)).is_err() {
                        return;
                    }
                    continue;
                }
            };
            let started = match worker.take() {
                Some(running) => Ok(running),
                None => Worker::start(spool_dir, setup, processors),
            };
            let sent = started.and_then(|mut running| {
                running.send(index, resolved)?;
                Ok(running)
            });
            match sent {
                Ok(running) => {
                    worker = Some(running);
                    in_flight.push(index);
                }
                Err(error) => {
                    let message = format!("Failed to start a worker process: {error}");
                    if sender
                        .send((index, failed(&resolved.input.display, message)))
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
        if in_flight.is_empty() {
            break;
        }
        let running = worker.as_mut().expect("a worker runs the jobs in flight");
        match running.receive() {
            Ok(done) if in_flight.contains(&done.index) => {
                in_flight.retain(|&index| index != done.index);
                let index = done.index;
                if sender.send((index, Finished::from(done))).is_err() {
                    return;
                }
            }
            outcome => {
                let reason = match outcome {
                    Ok(done) => format!("it answered for input {}, not in flight", done.index),
                    Err(error) => error.to_string(),
                };
                let ended = worker.take().expect("a worker runs the jobs");
                let pid = ended.child.id();
                let status = ended.stop();
                for &index in &in_flight {
                    if let Ok(resolved) = &inputs[index] {
                        remove_leftovers(args, spool_dir, index, resolved, pid);
                    }
                }
                if let [index] = in_flight[..] {
                    let display = match &inputs[index] {
                        Ok(resolved) => resolved.input.display.as_str(),
                        Err(error) => error.display.as_str(),
                    };
                    let message =
                        format!("The worker process extracting it ended ({status}): {reason}");
                    if sender.send((index, failed(display, message))).is_err() {
                        return;
                    }
                    in_flight.clear();
                } else {
                    suspects.extend(in_flight.drain(..));
                }
            }
        }
    }
    if let Some(worker) = worker {
        worker.finish();
    }
}

/// Removes what a worker process that died left of an input's output, so that it can be
/// extracted again.
fn remove_leftovers(
    args: &ExtractArgs,
    spool_dir: &Path,
    index: usize,
    resolved: &Resolved,
    pid: u32,
) {
    let output = match sink(args, spool_dir, index, &resolved.input, false) {
        Sink::Spooled(path) => path,
        Sink::OutputFile(path) => partial_output(&path, pid),
        Sink::Stdout => unreachable!("worker processes never write to standard output"),
    };
    let removed = [
        std::fs::remove_file(&output),
        std::fs::remove_dir_all(embedded_scratch(spool_dir, index)),
    ];
    for result in removed {
        if let Err(error) = ignore_missing(result) {
            report(
                &resolved.input.display,
                &format!("Failed to clean up after the worker process: {error}"),
            );
        }
    }
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

struct Worker {
    child: Child,
    jobs: BufWriter<ChildStdin>,
    answers: BufReader<ChildStdout>,
}

impl Worker {
    fn start(spool_dir: &Path, setup: &str, processors: NonZeroUsize) -> io::Result<Self> {
        let executable = std::env::current_exe()?;
        let mut child = Command::new(&executable)
            .args(crate::arguments())
            .env(WORKER_VARIABLE, spool_dir)
            .env(PROCESSORS_VARIABLE, processors.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", executable.display())))?;
        let jobs = BufWriter::new(child.stdin.take().expect("standard input is piped"));
        let answers = BufReader::new(child.stdout.take().expect("standard output is piped"));
        let mut worker = Worker {
            child,
            jobs,
            answers,
        };
        writeln!(worker.jobs, "{setup}")?;
        worker.jobs.flush()?;
        Ok(worker)
    }

    fn send(&mut self, index: usize, resolved: &Resolved) -> io::Result<()> {
        let job = Job {
            index,
            display: resolved.input.display.clone(),
            path: resolved.path.clone(),
            output_path: resolved.input.output_path.clone(),
        };
        serde_json::to_writer(&mut self.jobs, &job).map_err(io::Error::other)?;
        self.jobs.write_all(b"\n")?;
        self.jobs.flush()
    }

    fn receive(&mut self) -> io::Result<Done> {
        let mut line = String::new();
        if self.answers.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "it closed its output",
            ));
        }
        serde_json::from_str(&line).map_err(io::Error::other)
    }

    /// Kills the process, unless it already ended, and tells how it ended.
    fn stop(mut self) -> String {
        let _ = self.child.kill();
        match self.child.wait() {
            Ok(status) => status.to_string(),
            Err(error) => format!("unknown status: {error}"),
        }
    }

    /// Lets the process finish: it ends once its input closes.
    fn finish(self) {
        let Worker {
            mut child, jobs, ..
        } = self;
        drop(jobs);
        let _ = child.wait();
    }
}

/// The worker process: extracts the jobs on standard input, two at once, into the spool
/// directory of the process that started it, and answers each with a line on standard output.
pub fn serve(args: &ExtractArgs, spool_dir: &Path) -> bool {
    let answers = match answer_channel() {
        Ok(channel) => Mutex::new(BufWriter::new(channel)),
        Err(error) => {
            report("worker process", &error.to_string());
            return false;
        }
    };
    let mut lines = BufReader::new(io::stdin()).lines();
    let setup: Setup = match lines.next() {
        Some(Ok(line)) => match serde_json::from_str(&line) {
            Ok(setup) => setup,
            Err(error) => {
                report("worker process", &format!("Malformed setup: {error}"));
                return false;
            }
        },
        Some(Err(error)) => {
            report("worker process", &error.to_string());
            return false;
        }
        None => return true,
    };
    let (extractor, cache) = match prepare(args, setup.passwords) {
        Ok(prepared) => prepared,
        Err((what, error)) => {
            report(what, &error);
            return false;
        }
    };
    let context = Context {
        extractor: &extractor,
        args,
        options: crate::render_options(args),
        cache: cache.as_ref(),
        spool_dir,
        stream_to_stdout: false,
    };
    let jobs = Mutex::new(lines);
    thread::scope(|scope| {
        let handles: Vec<_> = (0..JOBS_PER_PROCESS)
            .map(|_| scope.spawn(|| serve_jobs(&context, &jobs, &answers)))
            .collect();
        handles
            .into_iter()
            .all(|handle| handle.join().unwrap_or(false))
    })
}

fn serve_jobs(
    context: &Context,
    jobs: &Mutex<Lines<BufReader<Stdin>>>,
    answers: &Mutex<BufWriter<std::fs::File>>,
) -> bool {
    loop {
        let line = jobs.lock().expect("job input lock").next();
        let job: Job = match line {
            None => return true,
            Some(Ok(line)) => match serde_json::from_str(&line) {
                Ok(job) => job,
                Err(error) => {
                    report("worker process", &format!("Malformed job: {error}"));
                    return false;
                }
            },
            Some(Err(error)) => {
                report("worker process", &error.to_string());
                return false;
            }
        };
        let resolved = Resolved {
            input: Input {
                source: Source::File(job.path.clone()),
                display: job.display,
                output_path: job.output_path,
            },
            path: job.path,
        };
        let done = Done::new(job.index, run(context, job.index, &Ok(resolved)));
        let mut answers = answers.lock().expect("answer output lock");
        let written = serde_json::to_writer(&mut *answers, &done)
            .map_err(io::Error::other)
            .and_then(|()| answers.write_all(b"\n"))
            .and_then(|()| answers.flush());
        if let Err(error) = written {
            report("worker process", &error.to_string());
            return false;
        }
    }
}
