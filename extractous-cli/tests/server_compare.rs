//! `extractous serve` against tika-server of the same Tika version on the same machine, which
//! `just server-compare` starts and names in TIKA_SERVER_ADDRESS and TIKA_SERVER_PID. Both read
//! the test documents and the quality corpus with their default OCR, after a warm-up pass, from
//! one client and from four; extractous also with English OCR, which skips the script detection
//! tika-server does not do. COMPARE_ROUNDS sets the passes measured, SERVER_COMPARISON the file
//! the table is written to.

#![cfg(target_os = "linux")]

mod common;

use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use common::server::{request, Server};
use common::{corpus, tree_rss_bytes};

const DEFAULT_ROUNDS: usize = 3;
const WORKERS: usize = 4;

/// A server under test: where it listens, how it is asked to extract, and its process.
struct Target<'a> {
    name: &'a str,
    address: &'a str,
    pid: u32,
    method: &'a str,
    path: &'a str,
    headers: &'a [(&'a str, &'a str)],
}

/// What a measured run saw.
struct Measured {
    requests: usize,
    failures: usize,
    elapsed: Duration,
    latencies: Vec<Duration>,
    peak_rss: u64,
}

impl Measured {
    fn percentile(&self, share: f64) -> Duration {
        let index = ((self.latencies.len() as f64 * share).ceil() as usize).max(1) - 1;
        self.latencies[index.min(self.latencies.len() - 1)]
    }
}

fn post(target: &Target, body: &[u8]) -> u16 {
    request(
        target.address,
        target.method,
        target.path,
        target.headers,
        body,
    )
    .status
}

/// Extracts `rounds` passes over `documents` from `clients` threads, sampling the server's
/// memory every 100 ms.
fn measure(
    target: &Target,
    documents: &[(String, Vec<u8>)],
    rounds: usize,
    clients: usize,
) -> Measured {
    let jobs: Vec<&[u8]> = (0..rounds)
        .flat_map(|_| documents.iter().map(|(_, body)| body.as_slice()))
        .collect();
    let next = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    let latencies = Mutex::new(Vec::with_capacity(jobs.len()));
    let peak = Mutex::new(0u64);
    let done = AtomicUsize::new(0);
    let started = Instant::now();
    thread::scope(|scope| {
        for _ in 0..clients {
            scope.spawn(|| {
                while let Some(body) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let sent = Instant::now();
                    if post(target, body) != 200 {
                        failures.fetch_add(1, Ordering::Relaxed);
                    }
                    latencies.lock().unwrap().push(sent.elapsed());
                }
                done.fetch_add(1, Ordering::Relaxed);
            });
        }
        scope.spawn(|| {
            while done.load(Ordering::Relaxed) < clients {
                let rss = tree_rss_bytes(target.pid);
                let mut peak = peak.lock().unwrap();
                *peak = (*peak).max(rss);
                drop(peak);
                thread::sleep(Duration::from_millis(100));
            }
        });
    });
    let elapsed = started.elapsed();
    let mut latencies = latencies.into_inner().unwrap();
    latencies.sort();
    Measured {
        requests: jobs.len(),
        failures: failures.into_inner(),
        elapsed,
        latencies,
        peak_rss: peak.into_inner().unwrap(),
    }
}

#[test]
#[ignore = "needs a running tika-server; run with `just server-compare`"]
fn extractous_serve_against_tika_server() {
    let tika_address = std::env::var("TIKA_SERVER_ADDRESS").expect("TIKA_SERVER_ADDRESS");
    let tika_pid: u32 = std::env::var("TIKA_SERVER_PID")
        .expect("TIKA_SERVER_PID")
        .parse()
        .expect("TIKA_SERVER_PID is a process id");
    let rounds = std::env::var("COMPARE_ROUNDS").map_or(DEFAULT_ROUNDS, |rounds| {
        rounds.parse().expect("COMPARE_ROUNDS is a number")
    });
    let ours = Server::start(&["--workers", &WORKERS.to_string()]);
    let targets = [
        Target {
            name: "extractous serve",
            address: &ours.address,
            pid: ours.child.id(),
            method: "POST",
            path: "/extract",
            headers: &[],
        },
        Target {
            name: "extractous serve, English OCR",
            address: &ours.address,
            pid: ours.child.id(),
            method: "POST",
            path: "/extract?ocr_lang=eng",
            headers: &[],
        },
        Target {
            name: "tika-server",
            address: &tika_address,
            pid: tika_pid,
            method: "PUT",
            path: "/tika",
            headers: &[("Accept", "text/plain")],
        },
    ];
    let documents = corpus();
    let mut table = format!(
        "{} documents ({} MiB) per pass, {rounds} passes after a warm-up pass, default OCR.\n\n\
         | Server | Clients | Requests/s | p50 | p99 | Failed | Peak RSS (MiB) |\n\
         |---|---|---|---|---|---|---|\n",
        documents.len(),
        documents.iter().map(|(_, body)| body.len()).sum::<usize>() >> 20,
    );
    for target in &targets {
        measure(target, &documents, 1, WORKERS);
        for clients in [1, WORKERS] {
            let run = measure(target, &documents, rounds, clients);
            writeln!(
                table,
                "| {} | {clients} | {:.1} | {} ms | {} ms | {} | {} |",
                target.name,
                run.requests as f64 / run.elapsed.as_secs_f64(),
                run.percentile(0.5).as_millis(),
                run.percentile(0.99).as_millis(),
                run.failures,
                run.peak_rss >> 20,
            )
            .unwrap();
        }
    }
    println!("{table}");
    if let Some(path) = std::env::var_os("SERVER_COMPARISON") {
        std::fs::write(path, &table).unwrap();
    }
}
