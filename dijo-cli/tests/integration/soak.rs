//! Long runs of the server under load, which `just test-soak` runs nightly. SOAK_SECONDS sets
//! how long each lasts, SOAK_SEED repeats the damage of a failing run.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::common::server::Server;
#[cfg(target_os = "linux")]
use crate::common::tree_rss_bytes;
use crate::common::{corpus, mutate, XorShift};

const CLIENTS: usize = 4;
const DEFAULT_SECONDS: u64 = 120;
const ANSWERED: [u16; 6] = [200, 413, 415, 422, 500, 504];
const MEMORY_GROWTH: f64 = 1.25;
const MEMORY_SLACK_BYTES: u64 = 64 << 20;

fn duration() -> Duration {
    Duration::from_secs(
        std::env::var("SOAK_SECONDS").map_or(DEFAULT_SECONDS, |seconds| {
            seconds.parse().expect("SOAK_SECONDS is a number")
        }),
    )
}

fn seed() -> u64 {
    std::env::var("SOAK_SEED").map_or_else(
        |_| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
                | 1
        },
        |seed| seed.parse().expect("SOAK_SEED is a number"),
    )
}

/// What a load run saw: how many requests were answered, and the server's memory, sampled every
/// second.
struct Run {
    requests: usize,
    memory: Vec<u64>,
}

/// Posts documents from `CLIENTS` threads until `duration` has passed, each picked by `pick`,
/// and hands every answer to `check`.
fn load(
    server: &Server,
    duration: Duration,
    pick: &(dyn Fn(usize, usize) -> (String, Vec<u8>) + Sync),
    check: &(dyn Fn(&str, u16, &[u8]) + Sync),
) -> Run {
    let deadline = Instant::now() + duration;
    let samples = Mutex::new(Vec::new());
    let requests = AtomicUsize::new(0);
    let requests = &requests;
    thread::scope(|scope| {
        for client in 0..CLIENTS {
            scope.spawn(move || {
                let mut round = 0;
                while Instant::now() < deadline {
                    let (name, body) = pick(client, round);
                    let response = server.request("POST", "/extract?format=json", &[], &body);
                    check(&name, response.status, &response.body);
                    requests.fetch_add(1, Ordering::Relaxed);
                    round += 1;
                }
            });
        }
        scope.spawn(|| {
            while Instant::now() < deadline {
                #[cfg(target_os = "linux")]
                samples
                    .lock()
                    .unwrap()
                    .push(tree_rss_bytes(server.child.id()));
                thread::sleep(Duration::from_secs(1));
            }
        });
    });
    Run {
        requests: requests.load(Ordering::Relaxed),
        memory: samples.into_inner().unwrap(),
    }
}

/// Every document, again and again, from four clients: each answers as it did the first time,
/// and after the first half of the run the server's memory stays level.
#[test]
#[ignore = "runs for minutes; run with `just test-soak`"]
fn a_long_run_answers_alike_and_keeps_its_memory_level() {
    let server = Server::start(&["--workers", &CLIENTS.to_string(), "--timeout", "60"]);
    let originals = corpus();
    let first: Mutex<HashMap<String, u16>> = Mutex::new(HashMap::new());
    let run = load(
        &server,
        duration(),
        &|client, round| originals[(client * 7 + round) % originals.len()].clone(),
        &|name, status, _| {
            let expected = *first
                .lock()
                .unwrap()
                .entry(name.to_string())
                .or_insert(status);
            assert_eq!(
                status, expected,
                "{name} answered {status}, before {expected}"
            );
        },
    );
    let samples = &run.memory;
    eprintln!("{} requests answered", run.requests);
    if samples.len() >= 4 {
        let quarter = samples.len() / 4;
        let settled = *samples[quarter..2 * quarter].iter().max().unwrap();
        let last = *samples[3 * quarter..].iter().max().unwrap();
        eprintln!(
            "peak memory {} MiB settled, {} MiB at the end",
            settled >> 20,
            last >> 20
        );
        assert!(
            last as f64 <= settled as f64 * MEMORY_GROWTH + MEMORY_SLACK_BYTES as f64,
            "memory grew from {} MiB to {} MiB",
            settled >> 20,
            last >> 20
        );
    }
}

/// Damaged copies among the documents, from four clients, with every worker in a process of
/// its own: each request is answered, with an extraction or a JSON error, workers that crash or
/// hang are replaced, and at the end the server still extracts a clean document correctly.
#[test]
#[ignore = "runs for minutes; run with `just test-soak`"]
fn damaged_documents_under_load_are_answered_and_workers_recover() {
    let seed = seed();
    eprintln!("SOAK_SEED={seed}");
    let server = Server::start(&[
        "--isolation",
        "process",
        "--workers",
        &CLIENTS.to_string(),
        "--timeout",
        "20",
    ]);
    let originals = corpus();
    let random = Mutex::new(XorShift(seed));
    let run = load(
        &server,
        duration(),
        &|_, _| {
            let mut random = random.lock().unwrap();
            let (name, bytes) = &originals[random.below(originals.len())];
            if random.below(2) == 0 {
                (name.clone(), bytes.clone())
            } else {
                (format!("damaged {name}"), mutate(bytes, &mut random))
            }
        },
        &|name, status, body| {
            assert!(
                ANSWERED.contains(&status),
                "{name} answered {status}; SOAK_SEED={seed}"
            );
            assert!(
                serde_json::from_slice::<serde_json::Value>(body).is_ok(),
                "{name} answered {status} with no JSON; SOAK_SEED={seed}"
            );
        },
    );
    eprintln!("{} requests answered", run.requests);
    assert_eq!(server.request("GET", "/healthz", &[], b"").status, 200);
    let clean = server.post_file("/extract", &[], "simple.odt");
    assert_eq!(clean.status, 200);
    assert!(clean
        .text()
        .contains("These are a few of my favorite things"));
}
