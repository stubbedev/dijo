#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

mod common;

use common::Scratch;

const MAX_RSS_BYTES: u64 = 512 << 20;

fn generate(path: &Path, size: u64, header: &str, mut record: impl FnMut(u64) -> String) -> u64 {
    let mut file = BufWriter::new(File::create(path).unwrap());
    file.write_all(header.as_bytes()).unwrap();
    let mut written = header.len() as u64;
    let mut index = 0;
    while written < size {
        let line = record(index);
        file.write_all(line.as_bytes()).unwrap();
        written += line.len() as u64;
        index += 1;
    }
    file.flush().unwrap();
    written
}

/// Extracts `path`, counting the output without keeping it, and returns the output size and the
/// peak resident memory of the extractous process.
fn extract(path: &Path) -> (u64, u64) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_extractous"))
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = io::copy(
        &mut child.stdout.take().unwrap().take(u64::MAX),
        &mut io::sink(),
    )
    .unwrap();
    assert!(child.wait().unwrap().success());
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) },
        0
    );
    (output, usage.ru_maxrss as u64 * 1024)
}

fn assert_bounded(path: &Path, input: u64) {
    let (output, peak_rss) = extract(path);
    assert!(
        output > input / 2,
        "{output} bytes of text from {input} bytes of input"
    );
    assert!(
        peak_rss < MAX_RSS_BYTES,
        "peak RSS {} MiB for {} MiB of input",
        peak_rss >> 20,
        input >> 20
    );
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn large_text_extracts_in_bounded_memory() {
    let scratch = Scratch::new("text");
    let path = scratch.0.join("large.txt");
    let input = generate(&path, 1 << 30, "", |index| {
        format!("{index} the quick brown fox jumps over the lazy dog again and again\n")
    });
    assert_bounded(&path, input);
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn large_csv_extracts_in_bounded_memory() {
    let scratch = Scratch::new("csv");
    let path = scratch.0.join("large.csv");
    let input = generate(&path, 512 << 20, "id,name,value,comment\n", |index| {
        format!(
            "{index},name {index},{}.{:02},\"a comment about row {index}\"\n",
            index * 3,
            index % 100
        )
    });
    assert_bounded(&path, input);
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn large_mbox_extracts_in_bounded_memory() {
    let scratch = Scratch::new("mbox");
    let path = scratch.0.join("large.mbox");
    let body = "The quick brown fox jumps over the lazy dog.\n".repeat(40);
    let input = generate(&path, 256 << 20, "", |index| {
        format!(
            "From sender{index}@example.com Mon Jan  1 00:00:00 2024\n\
             From: sender{index}@example.com\nTo: recipient@example.com\n\
             Subject: message {index}\nDate: Mon, 1 Jan 2024 00:00:00 +0000\n\n{body}\n"
        )
    });
    assert_bounded(&path, input);
}
