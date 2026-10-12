use std::io;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use bytes::Bytes;
use extractous_core::keep_current_thread_attached;
use tokio::sync::{mpsc as async_mpsc, oneshot};

use crate::cache::Cache;
use crate::server::handlers::{self, Head, Report, Respond, Task};
use crate::server::metrics::Metrics;
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

/// A fixed set of extraction threads, attached to the isolate for good, fed through a bounded
/// queue.
pub struct Pool {
    sender: mpsc::SyncSender<Job>,
}

impl Pool {
    /// Starts `workers` threads. Admission already limits requests in flight to the workers plus
    /// `queue` slots, so the channel holds that many and never refuses an admitted job, even
    /// while a worker is between jobs.
    pub fn start(
        workers: usize,
        queue: usize,
        metrics: Arc<Metrics>,
        cache: Option<Arc<Cache>>,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(workers + queue);
        let receiver = Arc::new(Mutex::new(receiver));
        let (ready_sender, ready) = mpsc::channel::<Result<(), String>>();
        for index in 0..workers {
            let receiver = Arc::clone(&receiver);
            let metrics = Arc::clone(&metrics);
            let cache = cache.clone();
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
                    work(&receiver, &metrics, cache.as_deref());
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
