mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use common::{Scratch, DOCUMENTS};

const DEFAULT_MUTANTS_PER_DOCUMENT: usize = 12;
const JOBS: usize = 4;
const TIMEOUT_SECONDS: u64 = 20;
const GRACE: Duration = Duration::from_secs(30);

/// A small, seedable generator, so that a failing run can be repeated with its seed.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

/// The document damaged in one of the ways files get damaged: bytes flipped, the end cut off,
/// a stretch overwritten with noise, or a stretch repeated.
fn mutate(original: &[u8], random: &mut XorShift) -> Vec<u8> {
    let mut bytes = original.to_vec();
    if bytes.is_empty() {
        return bytes;
    }
    match random.below(4) {
        0 => {
            for _ in 0..=random.below(16) {
                let index = random.below(bytes.len());
                bytes[index] ^= 1 << random.below(8);
            }
        }
        1 => bytes.truncate(random.below(bytes.len())),
        2 => {
            let start = random.below(bytes.len());
            let end = (start + 1 + random.below(512)).min(bytes.len());
            for byte in &mut bytes[start..end] {
                *byte = random.next() as u8;
            }
        }
        _ => {
            let start = random.below(bytes.len());
            let end = (start + 1 + random.below(4096)).min(bytes.len());
            let chunk = bytes[start..end].to_vec();
            let at = random.below(bytes.len());
            bytes.splice(at..at, chunk);
        }
    }
    bytes
}

/// Copies the inputs of a failing run where they outlive the test, for reproducing it.
fn keep(inputs: &[PathBuf], seed: u64) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fuzz-failures/{seed}"));
    fs::create_dir_all(&dir).unwrap();
    for input in inputs {
        fs::copy(input, dir.join(input.file_name().unwrap())).unwrap();
    }
    dir
}

fn documents(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(documents(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// Damaged inputs may fail to extract, but extractous must answer for every one of them: exit
/// normally, within its timeout, with a JSON record each.
#[test]
#[ignore]
fn damaged_documents_never_crash_or_hang() {
    let seed = std::env::var("FUZZ_SEED").map_or_else(
        |_| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
                | 1
        },
        |seed| seed.parse().expect("FUZZ_SEED is a number"),
    );
    let mutants = std::env::var("FUZZ_MUTANTS").map_or(DEFAULT_MUTANTS_PER_DOCUMENT, |count| {
        count.parse().expect("FUZZ_MUTANTS is a number")
    });
    eprintln!("FUZZ_SEED={seed} FUZZ_MUTANTS={mutants}");
    let mut random = XorShift(seed);
    let scratch = Scratch::new("fuzz");
    for original in documents(Path::new(DOCUMENTS)) {
        let bytes = fs::read(&original).unwrap();
        let name = original.file_name().unwrap().to_string_lossy().to_string();
        let inputs: Vec<PathBuf> = (0..mutants)
            .map(|index| {
                let path = scratch.0.join(format!("{index}-{name}"));
                fs::write(&path, mutate(&bytes, &mut random)).unwrap();
                path
            })
            .collect();
        let mut child = Command::new(env!("CARGO_BIN_EXE_extractous"))
            .args(["--format", "json", "--jobs", &JOBS.to_string()])
            .args(["--timeout", &TIMEOUT_SECONDS.to_string()])
            .args(&inputs)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut output = String::new();
            std::io::Read::read_to_string(&mut stdout, &mut output).map(|_| output)
        });
        let limit = Duration::from_secs(TIMEOUT_SECONDS) * mutants.div_ceil(JOBS) as u32 + GRACE;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if started.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let output = reader.join().unwrap().unwrap();
        let records = output
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|record| record["kind"] == "document")
            .count();
        let failure = match status {
            None => Some(format!("hung past {limit:?}")),
            Some(status) if !matches!(status.code(), Some(0 | 1)) => {
                Some(format!("ended with {status}"))
            }
            Some(_) if records != mutants => {
                Some(format!("answered {records} of {mutants} inputs"))
            }
            Some(_) => None,
        };
        if let Some(failure) = failure {
            let kept = keep(&inputs, seed);
            panic!(
                "mutants of {name} {failure}; FUZZ_SEED={seed}, inputs kept in {}",
                kept.display()
            );
        }
        for input in inputs {
            fs::remove_file(input).unwrap();
        }
    }
}
