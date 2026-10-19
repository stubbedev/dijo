mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use common::{documents, mutate, Scratch, XorShift, DOCUMENTS};

const DEFAULT_MUTANTS_PER_DOCUMENT: usize = 12;
const JOBS: usize = 4;
const TIMEOUT_SECONDS: u64 = 20;
const GRACE: Duration = Duration::from_secs(30);

/// Copies the inputs of a failing run where they outlive the test, for reproducing it.
fn keep(inputs: &[PathBuf], seed: u64) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fuzz-failures/{seed}"));
    fs::create_dir_all(&dir).unwrap();
    for input in inputs {
        fs::copy(input, dir.join(input.file_name().unwrap())).unwrap();
    }
    dir
}

/// Damaged inputs may fail to extract, but dijo must answer for every one of them: exit
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_dijo"))
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
