use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::common::DOCUMENTS;

const QUALITY_CORPUS: &str = "../quality/corpus";
const DEFAULT_ROUNDS: usize = 10;
const COLD_STARTS: usize = 5;
const PARALLEL_JOBS: usize = 4;

/// The documents of one format, born-digital, so the numbers measure text extraction rather
/// than tesseract.
const GROUPS: &[(&str, &[&str])] = &[
    (
        "PDF",
        &[
            "Q:prose.pdf",
            "Q:report.pdf",
            "Q:table.pdf",
            "Q:german.pdf",
            "Q:chinese.pdf",
            "D:2022_Q3_AAPL.pdf",
        ],
    ),
    (
        "DOCX",
        &[
            "Q:prose.docx",
            "Q:report.docx",
            "Q:table.docx",
            "D:bug_16.docx",
        ],
    ),
    ("DOC", &["Q:prose.doc", "D:simple.doc"]),
    (
        "XLSX",
        &[
            "Q:table.xlsx",
            "D:vodafone.xlsx",
            "D:many-shared-strings.xlsx",
        ],
    ),
    ("PPTX", &["D:simple.pptx", "D:science-exploration-1p.pptx"]),
    ("ODT", &["Q:prose.odt", "D:simple.odt"]),
    ("RTF", &["Q:prose.rtf"]),
    ("HTML", &["Q:prose.html", "Q:table.html", "D:sample.html"]),
    ("EPUB", &["Q:prose.epub", "D:winter-sports.epub"]),
    ("Email", &["D:sample.eml"]),
    (
        "Text",
        &[
            "D:mostly-ascii-utf8.txt",
            "Q:japanese-shift_jis.txt",
            "Q:russian-koi8-r.txt",
        ],
    ),
];

fn resolve(entry: &str) -> PathBuf {
    match entry.split_once(':') {
        Some(("Q", name)) => Path::new(QUALITY_CORPUS).join(name),
        Some(("D", name)) => Path::new(DOCUMENTS).join(name),
        _ => panic!("unknown corpus entry {entry}"),
    }
}

struct Run {
    elapsed: Duration,
    peak_rss_bytes: Option<u64>,
}

/// Extracts `inputs` in one process, discarding the text, and measures it.
fn run(arguments: &[&str], inputs: &[PathBuf]) -> Run {
    let started = Instant::now();
    let child = Command::new(env!("CARGO_BIN_EXE_dijo"))
        .args(arguments)
        .args(inputs)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (code, peak_rss_bytes) = wait(child);
    assert_eq!(code, Some(0), "extracting {inputs:?} failed");
    Run {
        elapsed: started.elapsed(),
        peak_rss_bytes,
    }
}

#[cfg(target_os = "linux")]
fn wait(child: std::process::Child) -> (Option<i32>, Option<u64>) {
    let mut status = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let pid = child.id() as libc::pid_t;
    assert_eq!(unsafe { libc::wait4(pid, &mut status, 0, &mut usage) }, pid);
    let code = libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status));
    (code, Some(usage.ru_maxrss as u64 * 1024))
}

#[cfg(not(target_os = "linux"))]
fn wait(mut child: std::process::Child) -> (Option<i32>, Option<u64>) {
    (child.wait().unwrap().code(), None)
}

fn mebibytes(bytes: u64) -> f64 {
    bytes as f64 / f64::from(1 << 20)
}

fn rss(run: &Run) -> String {
    run.peak_rss_bytes.map_or_else(
        || "–".to_string(),
        |bytes| format!("{:.0}", mebibytes(bytes)),
    )
}

/// Measures extraction throughput per format and writes a Markdown table to standard error and
/// to BENCH_OUTPUT, if set. The numbers depend on the machine, so they are reported, not
/// checked; compare runs on the same machine.
#[test]
#[ignore]
fn throughput() {
    let rounds = std::env::var("BENCH_ROUNDS").map_or(DEFAULT_ROUNDS, |rounds| {
        rounds.parse().expect("BENCH_ROUNDS is a number")
    });
    let mut table = String::from(
        "| Format | Documents | MiB | Seconds | Documents/s | MiB/s | Peak RSS (MiB) |\n\
         |---|---|---|---|---|---|---|\n",
    );
    let mut everything = Vec::new();
    for (format, entries) in GROUPS {
        let files: Vec<PathBuf> = entries.iter().map(|entry| resolve(entry)).collect();
        let bytes: u64 = files
            .iter()
            .map(|file| fs::metadata(file).unwrap().len())
            .sum::<u64>()
            * rounds as u64;
        let inputs: Vec<PathBuf> = (0..rounds).flat_map(|_| files.iter().cloned()).collect();
        let measured = run(&["--ocr", "never", "--jobs", "1"], &inputs);
        let seconds = measured.elapsed.as_secs_f64();
        writeln!(
            table,
            "| {format} | {} | {:.1} | {seconds:.2} | {:.1} | {:.2} | {} |",
            inputs.len(),
            mebibytes(bytes),
            inputs.len() as f64 / seconds,
            mebibytes(bytes) / seconds,
            rss(&measured)
        )
        .unwrap();
        everything.extend(inputs);
    }

    let parallel = run(
        &["--ocr", "never", "--jobs", &PARALLEL_JOBS.to_string()],
        &everything,
    );
    let mut cold: Vec<Duration> = (0..COLD_STARTS)
        .map(|_| run(&[], &[Path::new(DOCUMENTS).join("mostly-ascii-utf8.txt")]).elapsed)
        .collect();
    cold.sort();
    writeln!(
        table,
        "\nAll {} documents with {PARALLEL_JOBS} jobs: {:.2} s, {:.1} documents/s, peak RSS {} MiB. \
         Cold start to a small text file's output: {:.0} ms (median of {COLD_STARTS}).",
        everything.len(),
        parallel.elapsed.as_secs_f64(),
        everything.len() as f64 / parallel.elapsed.as_secs_f64(),
        rss(&parallel),
        cold[COLD_STARTS / 2].as_secs_f64() * 1000.0
    )
    .unwrap();
    eprintln!("{table}");
    if let Ok(path) = std::env::var("BENCH_OUTPUT") {
        fs::write(path, table).unwrap();
    }
}
