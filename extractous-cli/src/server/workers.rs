use std::io;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use bytes::Bytes;
use extractous_core::keep_current_thread_attached;
use tokio::sync::{mpsc as async_mpsc, oneshot};

use crate::cache::Cache;
use crate::server::handlers::{self, Head, Report, Respond, Task};
use crate::server::metrics::Metrics;
use crate::server::processes::{supervise, ProcessOptions, WorkerProcess};
use crate::server::upload::Upload;

/// A part of a response body, or why the body was cut short.
pub type Chunk = io::Result<Bytes>;

pub struct Job {
    pub task: Task,
    pub upload: Upload,
    pub head: oneshot::Sender<Head>,
    pub body: async_mpsc::Sender<Chunk>,
}

pub struct QueueFull;

/// Where the pool's workers run extractions.
#[derive(Clone)]
pub enum Workers {
    /// In threads of this process, attached to its isolate for good.
    Threads(Option<Arc<Cache>>),
    /// In worker processes, one per worker thread, each with its own isolate.
    Processes(ProcessOptions),
}

/// A fixed set of extraction workers fed through a bounded queue.
pub struct Pool {
    sender: mpsc::SyncSender<Job>,
}

impl Pool {
    /// Starts `count` workers. Admission already limits requests in flight to the workers plus
    /// `queue` slots, so the channel holds that many and never refuses an admitted job, even
    /// while a worker is between jobs.
    pub fn start(
        count: usize,
        queue: usize,
        metrics: Arc<Metrics>,
        workers: Workers,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(count + queue);
        let receiver = Arc::new(Mutex::new(receiver));
        let (ready_sender, ready) = mpsc::channel::<Result<(), String>>();
        for index in 0..count {
            let receiver = Arc::clone(&receiver);
            let metrics = Arc::clone(&metrics);
            let workers = workers.clone();
            let ready_sender = ready_sender.clone();
            thread::Builder::new()
                .name(format!("extractous worker {index}"))
                .spawn(move || {
                    let worker = match Worker::start(workers) {
                        Ok(worker) => worker,
                        Err(error) => {
                            let _ = ready_sender.send(Err(error));
                            return;
                        }
                    };
                    let _ = ready_sender.send(Ok(()));
                    drop(ready_sender);
                    metrics
                        .workers
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    worker.run(&receiver, &metrics);
                })
                .map_err(|e| format!("Failed to start worker {index}: {e}"))?;
        }
        drop(ready_sender);
        for _ in 0..count {
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

/// A worker ready for jobs.
enum Worker {
    Thread(Option<Arc<Cache>>),
    Process(ProcessOptions, WorkerProcess),
}

impl Worker {
    /// Readies the current thread to run jobs.
    fn start(workers: Workers) -> Result<Self, String> {
        match workers {
            Workers::Threads(cache) => keep_current_thread_attached()
                .map(|()| Worker::Thread(cache))
                .map_err(|e| e.to_string()),
            Workers::Processes(options) => WorkerProcess::start(&options)
                .map(|process| Worker::Process(options, process))
                .map_err(|e| format!("Failed to start a worker process: {e}")),
        }
    }

    fn run(self, receiver: &Mutex<mpsc::Receiver<Job>>, metrics: &Metrics) {
        match self {
            Worker::Thread(cache) => work(receiver, metrics, cache.as_deref()),
            Worker::Process(options, process) => supervise(receiver, metrics, &options, process),
        }
    }
}

fn work(receiver: &Mutex<mpsc::Receiver<Job>>, metrics: &Metrics, cache: Option<&Cache>) {
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
        let mut respond = ThreadResponder {
            head: Some(head),
            body: &body,
            metrics,
        };
        handlers::run(&task, &upload.files(), cache, &mut respond);
    }
}

/// Answers through the connection task directly, from a worker thread of this process.
pub struct ThreadResponder<'a> {
    pub head: Option<oneshot::Sender<Head>>,
    pub body: &'a async_mpsc::Sender<Chunk>,
    pub metrics: &'a Metrics,
}

impl Respond for ThreadResponder<'_> {
    fn head(&mut self, head: Head) -> io::Result<()> {
        self.head
            .take()
            .expect("a response has one head")
            .send(head)
            .map_err(|_| client_gone())
    }

    fn chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.body
            .blocking_send(Ok(Bytes::copy_from_slice(bytes)))
            .map_err(|_| client_gone())?;
        self.metrics.bytes_out(bytes.len() as u64);
        Ok(())
    }

    fn report(&mut self, report: Report) {
        self.metrics.record(report);
    }
}

pub fn client_gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the client went away")
}
